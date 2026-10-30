//! MCP over HTTP: the protocol's "Streamable HTTP" transport (2025-03-26 and later), for clients
//! on other machines or in apps that can't start a local process (a phone, a tablet, an
//! Obsidian plugin, a web client).
//!
//! One endpoint, `/mcp` (`/` works too). Each `POST` carries one JSON-RPC message and gets its
//! reply as `application/json`; a notification gets `202 Accepted`. There is no server-initiated
//! stream, so `GET` is `405`, as the spec allows. `GET /health` answers `ok` for monitors.
//!
//! Every client shares the one [`Server`] (and so one workbook session). Connections are served
//! one at a time, each closed after its reply, with read/write timeouts so a stalled client can't
//! hold the server.
//!
//! Safety:
//! - Serving on anything but loopback needs a bearer token ([`HttpOptions::token`]); without
//!   one, [`serve_http`] refuses to start, because anyone who can reach the port could edit
//!   workbooks.
//! - A browser `Origin` is checked against local origins (plus [`HttpOptions::allowed_origins`])
//!   when no token is set, so a web page can't drive a loopback server (DNS rebinding). With a
//!   token, any origin may call, but only with the token.
//! - Request sizes are capped and every input path returns an HTTP error rather than panicking.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream, ToSocketAddrs};
use std::time::Duration;

use crate::Server;

/// The MCP endpoint path.
pub const MCP_PATH: &str = "/mcp";
/// Largest request head (request line and headers).
const MAX_HEAD: usize = 16 * 1024;
const MAX_HEADERS: usize = 100;
/// Largest body: the server's own 16 MB message limit, plus room for whitespace.
const MAX_BODY: usize = 16 * 1024 * 1024 + 4096;
const IO_TIMEOUT: Duration = Duration::from_secs(30);

/// How the HTTP transport is exposed.
#[derive(Clone, Debug, Default)]
pub struct HttpOptions {
    /// Bearer token every request must carry (`Authorization: Bearer <token>`). Required to serve
    /// on a non-loopback address.
    pub token: Option<String>,
    /// Browser origins allowed besides local ones (`http://localhost…`, `app://obsidian.md`,
    /// `capacitor://localhost`) when no token is set, e.g. `https://dashboards.example`.
    pub allowed_origins: Vec<String>,
}

/// Binds `addr` (`"127.0.0.1:8790"`, `"0.0.0.0:8790"`) and serves until the process ends.
pub fn serve_http(server: &mut Server, addr: &str, opts: &HttpOptions) -> std::io::Result<()> {
    let listener = TcpListener::bind(addr)?;
    let local = listener.local_addr()?;
    check_exposure(&local, opts)?;
    log::info!("MCP over HTTP on http://{local}{MCP_PATH}");
    serve_listener(server, &listener, opts, None)
}

/// Refuses a non-loopback address without a token.
pub fn check_exposure(addr: &SocketAddr, opts: &HttpOptions) -> std::io::Result<()> {
    if !addr.ip().is_loopback() && opts.token.as_deref().is_none_or(str::is_empty) {
        return Err(std::io::Error::other(format!(
            "refusing to serve MCP on {addr} without a token: anyone who can reach it could edit your workbooks. \
             Set a token (--token or GRIDCRAFT_MCP_TOKEN), or listen on 127.0.0.1 only."
        )));
    }
    Ok(())
}

/// `"8790"` → `"127.0.0.1:8790"`; `"host:port"` is kept. Checks the address resolves.
pub fn http_addr(s: &str) -> std::io::Result<String> {
    let addr = if s.parse::<u16>().is_ok() { format!("127.0.0.1:{s}") } else { s.to_string() };
    addr.to_socket_addrs()?.next().ok_or_else(|| std::io::Error::other(format!("{s} isn't an address")))?;
    Ok(addr)
}

/// Serves connections from `listener` one at a time; stops after `limit` connections when given
/// (tests), otherwise runs until the listener fails.
pub fn serve_listener(server: &mut Server, listener: &TcpListener, opts: &HttpOptions, limit: Option<usize>) -> std::io::Result<()> {
    let mut served = 0usize;
    for conn in listener.incoming() {
        match conn {
            Ok(stream) => {
                if let Err(e) = handle_connection(server, stream, opts) {
                    log::debug!("MCP HTTP connection: {e}");
                }
            }
            Err(e) => log::warn!("MCP HTTP accept: {e}"),
        }
        served = served.saturating_add(1);
        if limit.is_some_and(|n| served >= n) {
            break;
        }
    }
    Ok(())
}

/// Reads one request from `stream`, answers it and closes the connection.
pub fn handle_connection(server: &mut Server, mut stream: TcpStream, opts: &HttpOptions) -> std::io::Result<()> {
    stream.set_read_timeout(Some(IO_TIMEOUT))?;
    stream.set_write_timeout(Some(IO_TIMEOUT))?;
    let reply = {
        let mut reader = BufReader::new(stream.try_clone()?);
        match read_request(&mut reader) {
            Ok(req) => respond(server, &req, opts),
            Err(fail) => Reply::text(fail.status, fail.message),
        }
    };
    stream.write_all(&reply.to_bytes())?;
    stream.flush()?;
    let _ = stream.shutdown(std::net::Shutdown::Both);
    Ok(())
}

struct Request {
    method: String,
    path: String,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl Request {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(k, _)| k == name).map(|(_, v)| v.as_str())
    }
}

struct Failure {
    status: u16,
    message: &'static str,
}

fn fail(status: u16, message: &'static str) -> Failure {
    Failure { status, message }
}

fn read_request(reader: &mut impl BufRead) -> Result<Request, Failure> {
    let mut head_bytes = 0usize;
    let mut line = Vec::new();
    let mut read_line = |reader: &mut dyn BufRead, line: &mut Vec<u8>| -> Result<String, Failure> {
        line.clear();
        let n = reader
            .take((MAX_HEAD - head_bytes.min(MAX_HEAD)) as u64 + 1)
            .read_until(b'\n', line)
            .map_err(|_| fail(408, "request timed out or failed"))?;
        head_bytes = head_bytes.saturating_add(n);
        if head_bytes > MAX_HEAD {
            return Err(fail(431, "request headers too large"));
        }
        if n == 0 {
            return Err(fail(400, "empty request"));
        }
        let text = std::str::from_utf8(line).map_err(|_| fail(400, "request head is not UTF-8"))?;
        Ok(text.trim_end_matches(['\r', '\n']).to_string())
    };

    let start = read_line(reader, &mut line)?;
    let mut parts = start.split(' ');
    let (Some(method), Some(target), Some(version), None) = (parts.next(), parts.next(), parts.next(), parts.next()) else {
        return Err(fail(400, "malformed request line"));
    };
    if !version.starts_with("HTTP/1.") {
        return Err(fail(505, "HTTP/1.x only"));
    }
    let path = target.split('?').next().unwrap_or_default().to_string();

    let mut headers = Vec::new();
    loop {
        let h = read_line(reader, &mut line)?;
        if h.is_empty() {
            break;
        }
        if headers.len() >= MAX_HEADERS {
            return Err(fail(431, "too many headers"));
        }
        let Some((name, value)) = h.split_once(':') else { return Err(fail(400, "malformed header")) };
        headers.push((name.trim().to_ascii_lowercase(), value.trim().to_string()));
    }
    let req = Request { method: method.to_string(), path, headers, body: Vec::new() };

    if req.header("transfer-encoding").is_some() {
        return Err(fail(411, "send the body with Content-Length (chunked bodies aren't supported)"));
    }
    let len = match req.header("content-length") {
        Some(v) => v.parse::<usize>().map_err(|_| fail(400, "bad Content-Length"))?,
        None => 0,
    };
    if len > MAX_BODY {
        return Err(fail(413, "request body too large (16 MB max)"));
    }
    let mut body = vec![0u8; len];
    reader.read_exact(&mut body).map_err(|_| fail(400, "request body shorter than Content-Length"))?;
    Ok(Request { body, ..req })
}

/// A browser origin that's allowed to call: any with a token; otherwise local ones and the
/// configured list. Requests without an `Origin` (non-browser clients) are always allowed.
fn origin_allowed(origin: Option<&str>, opts: &HttpOptions) -> bool {
    let Some(origin) = origin else { return true };
    if opts.token.as_deref().is_some_and(|t| !t.is_empty()) {
        return true;
    }
    if opts.allowed_origins.iter().any(|o| o.eq_ignore_ascii_case(origin)) {
        return true;
    }
    let o = origin.to_ascii_lowercase();
    // Obsidian's desktop and mobile apps; "null" (sandboxed pages, files) is never local.
    if matches!(o.as_str(), "app://obsidian.md" | "capacitor://localhost") {
        return true;
    }
    let host = o.split_once("://").map(|(_, rest)| rest).unwrap_or("");
    let host = host.rsplit_once(':').filter(|(_, port)| port.chars().all(|c| c.is_ascii_digit())).map(|(h, _)| h).unwrap_or(host);
    matches!(host, "localhost" | "127.0.0.1" | "[::1]")
}

/// Compares in time that doesn't depend on where the strings differ.
fn same_secret(a: &str, b: &str) -> bool {
    a.len() == b.len() && a.bytes().zip(b.bytes()).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

fn authorized(req: &Request, opts: &HttpOptions) -> bool {
    let Some(token) = opts.token.as_deref().filter(|t| !t.is_empty()) else { return true };
    let Some(auth) = req.header("authorization") else { return false };
    let Some((scheme, given)) = auth.split_once(' ') else { return false };
    scheme.eq_ignore_ascii_case("bearer") && same_secret(given.trim(), token)
}

fn respond(server: &mut Server, req: &Request, opts: &HttpOptions) -> Reply {
    let origin = req.header("origin");
    if !origin_allowed(origin, opts) {
        return Reply::text(403, "origin not allowed");
    }
    let cors = origin.map(str::to_string);
    let with_cors = |r: Reply| r.cors(cors.clone());

    if req.path == "/health" && req.method == "GET" {
        return with_cors(Reply::text(200, "ok"));
    }
    if req.path != MCP_PATH && req.path != "/" {
        return with_cors(Reply::text(404, "not found: the MCP endpoint is /mcp"));
    }
    if req.method == "OPTIONS" {
        return with_cors(
            Reply::empty(204)
                .header("Access-Control-Allow-Methods", "POST, GET, OPTIONS")
                .header("Access-Control-Allow-Headers", "Content-Type, Authorization, Accept, Mcp-Session-Id, Mcp-Protocol-Version, Last-Event-ID"),
        );
    }
    if !authorized(req, opts) {
        return with_cors(Reply::text(401, "missing or wrong bearer token").header("WWW-Authenticate", "Bearer"));
    }
    match req.method.as_str() {
        "POST" => {
            if let Some(ct) = req.header("content-type")
                && !ct.to_ascii_lowercase().starts_with("application/json")
            {
                return with_cors(Reply::text(415, "send application/json"));
            }
            let Ok(body) = std::str::from_utf8(&req.body) else { return with_cors(Reply::text(400, "body is not UTF-8")) };
            match server.handle_line(body) {
                Some(json) => with_cors(Reply::json(json)),
                // A notification or a response: nothing to send back.
                None => with_cors(Reply::empty(202)),
            }
        }
        // No server-initiated stream (allowed by the spec); DELETE ends no session (none are kept).
        _ => with_cors(Reply::text(405, "use POST").header("Allow", "POST, OPTIONS")),
    }
}

struct Reply {
    status: u16,
    content_type: Option<&'static str>,
    body: Vec<u8>,
    headers: Vec<(&'static str, String)>,
}

impl Reply {
    fn empty(status: u16) -> Self {
        Reply { status, content_type: None, body: Vec::new(), headers: Vec::new() }
    }
    fn text(status: u16, text: &str) -> Self {
        Reply { status, content_type: Some("text/plain; charset=utf-8"), body: text.as_bytes().to_vec(), headers: Vec::new() }
    }
    fn json(json: String) -> Self {
        Reply { status: 200, content_type: Some("application/json"), body: json.into_bytes(), headers: Vec::new() }
    }
    fn header(mut self, name: &'static str, value: &str) -> Self {
        self.headers.push((name, value.to_string()));
        self
    }
    fn cors(self, origin: Option<String>) -> Self {
        match origin {
            Some(o) => {
                self.header("Access-Control-Allow-Origin", &o).header("Vary", "Origin").header("Access-Control-Expose-Headers", "Mcp-Session-Id")
            }
            None => self,
        }
    }
    fn to_bytes(&self) -> Vec<u8> {
        let reason = match self.status {
            200 => "OK",
            202 => "Accepted",
            204 => "No Content",
            400 => "Bad Request",
            401 => "Unauthorized",
            403 => "Forbidden",
            404 => "Not Found",
            405 => "Method Not Allowed",
            408 => "Request Timeout",
            411 => "Length Required",
            413 => "Content Too Large",
            415 => "Unsupported Media Type",
            431 => "Request Header Fields Too Large",
            505 => "HTTP Version Not Supported",
            _ => "Error",
        };
        let mut head = format!("HTTP/1.1 {} {reason}\r\n", self.status);
        if let Some(ct) = self.content_type {
            head.push_str(&format!("Content-Type: {ct}\r\n"));
        }
        head.push_str(&format!("Content-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n", self.body.len()));
        for (k, v) in &self.headers {
            // Header values come from our own constants or a request's Origin; strip line breaks.
            let v: String = v.chars().filter(|c| *c != '\r' && *c != '\n').collect();
            head.push_str(&format!("{k}: {v}\r\n"));
        }
        head.push_str("\r\n");
        let mut out = head.into_bytes();
        out.extend_from_slice(&self.body);
        out
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use std::io::{Read, Write};
    use std::net::{TcpListener, TcpStream};

    use serde_json::{Value, json};

    use super::*;
    use crate::Headless;

    /// Serves one raw request on a fresh loopback port and returns (status, headers, body).
    fn exchange(server: &mut Server, opts: &HttpOptions, raw: Vec<u8>) -> (u16, String, String) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let client = std::thread::spawn(move || {
            let mut s = TcpStream::connect(addr).unwrap();
            s.write_all(&raw).unwrap();
            // Done sending: a server waiting for more body sees the end at once.
            let _ = s.shutdown(std::net::Shutdown::Write);
            let mut out = Vec::new();
            s.read_to_end(&mut out).unwrap();
            String::from_utf8_lossy(&out).to_string()
        });
        serve_listener(server, &listener, opts, Some(1)).unwrap();
        let text = client.join().unwrap();
        let (head, body) = text.split_once("\r\n\r\n").unwrap_or((&text, ""));
        let status = head.split(' ').nth(1).and_then(|s| s.parse().ok()).unwrap_or(0);
        (status, head.to_string(), body.to_string())
    }

    fn post(path: &str, body: &str, extra: &[&str]) -> Vec<u8> {
        let mut req = format!("POST {path} HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {}\r\n", body.len());
        for h in extra {
            req.push_str(h);
            req.push_str("\r\n");
        }
        req.push_str("\r\n");
        req.push_str(body);
        req.into_bytes()
    }

    fn server() -> Server {
        Server::new(Box::new(Headless::new()))
    }

    #[test]
    fn json_rpc_over_post() {
        let mut s = server();
        let opts = HttpOptions::default();
        let init = json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"protocolVersion": "2025-06-18"}}).to_string();
        let (status, head, body) = exchange(&mut s, &opts, post("/mcp", &init, &[]));
        assert_eq!(status, 200, "{head}");
        assert!(head.contains("Content-Type: application/json"));
        let v: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(v["id"], 1);
        assert!(v["result"]["serverInfo"].is_object(), "{v}");

        let note = json!({"jsonrpc": "2.0", "method": "notifications/initialized"}).to_string();
        assert_eq!(exchange(&mut s, &opts, post("/mcp", &note, &[])).0, 202);

        // The same session across requests: write, then read back.
        let write = json!({"jsonrpc": "2.0", "id": 2, "method": "tools/call", "params": {"name": "write_range", "arguments": {"range": "A1", "values": [[2, "=A1*21"]]}}}).to_string();
        assert_eq!(exchange(&mut s, &opts, post("/", &write, &[])).0, 200);
        let read =
            json!({"jsonrpc": "2.0", "id": 3, "method": "tools/call", "params": {"name": "read_range", "arguments": {"range": "B1"}}}).to_string();
        let (_, _, body) = exchange(&mut s, &opts, post("/mcp", &read, &[]));
        assert!(body.contains("42"), "{body}");
    }

    #[test]
    fn token_is_required_when_set() {
        let mut s = server();
        let opts = HttpOptions { token: Some("s3cret".into()), ..Default::default() };
        let msg = json!({"jsonrpc": "2.0", "id": 1, "method": "ping"}).to_string();
        let (status, head, _) = exchange(&mut s, &opts, post("/mcp", &msg, &[]));
        assert_eq!(status, 401);
        assert!(head.contains("WWW-Authenticate: Bearer"));
        assert_eq!(exchange(&mut s, &opts, post("/mcp", &msg, &["Authorization: Bearer wrong!"])).0, 401);
        assert_eq!(exchange(&mut s, &opts, post("/mcp", &msg, &["Authorization: bearer s3cret"])).0, 200);
        // With a token, a web page may call (it still needs the token).
        let (status, head, _) = exchange(&mut s, &opts, post("/mcp", &msg, &["Authorization: Bearer s3cret", "Origin: https://example.com"]));
        assert_eq!(status, 200);
        assert!(head.contains("Access-Control-Allow-Origin: https://example.com"));
    }

    #[test]
    fn origins_without_a_token() {
        let mut s = server();
        let opts = HttpOptions { allowed_origins: vec!["https://dash.example".into()], ..Default::default() };
        let msg = json!({"jsonrpc": "2.0", "id": 1, "method": "ping"}).to_string();
        for (origin, ok) in [
            ("https://evil.example", false),
            ("http://localhost:3000", true),
            ("http://127.0.0.1", true),
            ("app://obsidian.md", true),
            ("capacitor://localhost", true),
            ("https://dash.example", true),
            ("null", false),
            ("http://localhost.evil.example", false),
        ] {
            let header = format!("Origin: {origin}");
            let status = exchange(&mut s, &opts, post("/mcp", &msg, &[&header])).0;
            assert_eq!(status == 200, ok, "{origin}: {status}");
        }
    }

    #[test]
    fn methods_paths_preflight_and_health() {
        let mut s = server();
        let opts = HttpOptions::default();
        assert_eq!(exchange(&mut s, &opts, b"GET /mcp HTTP/1.1\r\nHost: x\r\n\r\n".to_vec()).0, 405);
        assert_eq!(exchange(&mut s, &opts, b"DELETE /mcp HTTP/1.1\r\n\r\n".to_vec()).0, 405);
        assert_eq!(exchange(&mut s, &opts, b"GET /nope HTTP/1.1\r\n\r\n".to_vec()).0, 404);
        let (status, _, body) = exchange(&mut s, &opts, b"GET /health HTTP/1.1\r\n\r\n".to_vec());
        assert_eq!((status, body.as_str()), (200, "ok"));
        let (status, head, _) = exchange(&mut s, &opts, b"OPTIONS /mcp HTTP/1.1\r\nOrigin: app://obsidian.md\r\n\r\n".to_vec());
        assert_eq!(status, 204);
        assert!(head.contains("Access-Control-Allow-Origin: app://obsidian.md") && head.contains("Authorization"), "{head}");
    }

    #[test]
    fn hostile_requests_get_errors_not_panics() {
        let mut s = server();
        let opts = HttpOptions::default();
        let cases: Vec<(Vec<u8>, u16)> = vec![
            (b"\r\n".to_vec(), 400),
            (b"NONSENSE\r\n\r\n".to_vec(), 400),
            (b"POST /mcp HTTP/2\r\n\r\n".to_vec(), 505),
            (b"POST /mcp HTTP/1.1\r\nContent-Length: abc\r\n\r\n".to_vec(), 400),
            (b"POST /mcp HTTP/1.1\r\nContent-Length: 99999999999\r\n\r\n".to_vec(), 413),
            (b"POST /mcp HTTP/1.1\r\nTransfer-Encoding: chunked\r\n\r\n0\r\n\r\n".to_vec(), 411),
            (b"POST /mcp HTTP/1.1\r\nContent-Length: 10\r\n\r\nshort".to_vec(), 400),
            (b"POST /mcp HTTP/1.1\r\nContent-Type: text/plain\r\nContent-Length: 2\r\n\r\n{}".to_vec(), 415),
            (b"POST /mcp HTTP/1.1\r\nBad header line\r\n\r\n".to_vec(), 400),
            ([b"GET /mcp HTTP/1.1\r\nX: ".to_vec(), vec![b'a'; MAX_HEAD + 10], b"\r\n\r\n".to_vec()].concat(), 431),
        ];
        for (raw, want) in cases {
            let shown = String::from_utf8_lossy(&raw[..raw.len().min(60)]).to_string();
            assert_eq!(exchange(&mut s, &opts, raw).0, want, "{shown}");
        }
        // Not JSON: a JSON-RPC parse error in a 200 reply, as over stdio.
        let (status, _, body) = exchange(&mut s, &opts, post("/mcp", "{not json", &[]));
        assert_eq!(status, 200);
        assert!(body.contains("-32700"), "{body}");
    }

    #[test]
    fn public_addresses_need_a_token() {
        let public: SocketAddr = "0.0.0.0:8790".parse().unwrap();
        let local: SocketAddr = "127.0.0.1:8790".parse().unwrap();
        assert!(check_exposure(&public, &HttpOptions::default()).is_err());
        assert!(check_exposure(&public, &HttpOptions { token: Some(String::new()), ..Default::default() }).is_err());
        assert!(check_exposure(&public, &HttpOptions { token: Some("t".into()), ..Default::default() }).is_ok());
        assert!(check_exposure(&local, &HttpOptions::default()).is_ok());
        assert_eq!(http_addr("8790").unwrap(), "127.0.0.1:8790");
        assert_eq!(http_addr("0.0.0.0:9000").unwrap(), "0.0.0.0:9000");
        assert!(http_addr("not an address").is_err());
    }
}
