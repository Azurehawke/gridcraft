# GridCraft MCP server

`gridcraft-cli mcp` exposes GridCraft to AI agents over the [Model Context Protocol](https://modelcontextprotocol.io):
newline-delimited JSON-RPC 2.0 on stdin/stdout (protocol `2025-06-18`; `2025-03-26` and `2024-11-05` clients are
answered in their own version). Logs go to stderr.

Two backends:

| Mode | Command | What it drives |
|---|---|---|
| Headless (default) | `gridcraft-cli mcp [--in book.xlsx \| --sample budget]` | An in-process engine session — no window. |
| Remote | `gridcraft-cli mcp --connect 7979` | A running app started with `gridcraft --control 7979` (TCP JSON lines on `127.0.0.1`). |

## Claude Code

```sh
claude mcp add gridcraft -- gridcraft-cli mcp
# or drive the desktop app you are looking at:
claude mcp add gridcraft-app -- gridcraft-cli mcp --connect 7979
```

JSON config (`.mcp.json`, Claude Desktop, other clients):

```json
{
  "mcpServers": {
    "gridcraft": {
      "command": "gridcraft-cli",
      "args": ["mcp"]
    }
  }
}
```

Use `"args": ["mcp", "--connect", "7979"]` for the desktop app, or `["mcp", "--in", "/path/book.xlsx"]` to start
with a file open.

## Over HTTP

`--http` serves the same server over MCP's Streamable HTTP transport instead of stdio, for clients on other
machines or in apps that can't start a local program (a phone or tablet, an Obsidian plugin, a web client):

```sh
# This machine only (loopback); no token needed.
gridcraft-cli mcp --http 8790 --in ~/vault/Budget.xlsx

# Reachable from your network: a bearer token is required (kept out of the process list via the environment).
GRIDCRAFT_MCP_TOKEN="$(openssl rand -hex 24)" gridcraft-cli mcp --http 0.0.0.0:8790
```

| Option | Meaning |
|---|---|
| `--http PORT` or `--http HOST:PORT` | Listen on HTTP; a bare port means `127.0.0.1:PORT`. The endpoint is `http://HOST:PORT/mcp`. |
| `--token TOKEN` / `GRIDCRAFT_MCP_TOKEN` | Every request must send `Authorization: Bearer TOKEN`. **Required** on any address other than loopback: without one the server refuses to start, since anyone who can reach the port could edit workbooks. |
| `--allow-origin O1,O2` | Extra browser origins allowed when there's no token (local origins, `app://obsidian.md` and `capacitor://localhost` always are). |

Behaviour:

- `POST /mcp` (or `/`) carries one JSON-RPC message; the reply comes back as `application/json`, and a notification
  gets `202 Accepted`. There is no server-initiated stream, so `GET /mcp` is `405`, as the spec allows. No
  `Mcp-Session-Id` is issued.
- `GET /health` answers `ok`, for uptime monitors.
- Every client shares one server, so one session and its open workbook. Requests are served one at a time; each
  connection closes after its reply, and stalled connections time out after 30 s.
- Browser `Origin`s are checked (DNS-rebinding protection): without a token only local origins and `--allow-origin`
  ones may call; with a token any origin may, but only with the token. CORS preflights are answered.
- Requests are capped (16 KB of headers, 16 MB of body); malformed ones get an HTTP error, never a crash.
- It's plain HTTP. Off a trusted network, put it behind TLS (a reverse proxy such as Caddy, or Tailscale).

Client config (clients that speak Streamable HTTP):

```json
{
  "mcpServers": {
    "gridcraft": {
      "type": "http",
      "url": "http://homeserver:8790/mcp",
      "headers": { "Authorization": "Bearer YOUR_TOKEN" }
    }
  }
}
```

## Tools

| Tool | Arguments | Does |
|---|---|---|
| `execute_command` | `command`, `params?` | Runs **any** engine command (`cell.set`, `home.bold`, `data.removeDuplicates`, …). |
| `list_commands` | `search?` | Command ids, labels, ribbon paths, shortcuts and parameter docs. |
| `inspect_workbook` | — | Sheets, used ranges, tables, charts, names, selection, history. |
| `read_range` | `range?`, `sheet?`, `formulas?`, `formatted?` | Rows of values (default: the used range). |
| `write_range` | `range`, `values` (2-D) | `range.setValues`; strings starting with `=` are formulas. |
| `set_cell` / `get_cell` | `cell`, `input` / `cell` | Enter one cell / read value, formula, display text, style. |
| `evaluate_formula` | `formula`, `cell?` | Evaluates without changing the sheet. A formula that couldn't be entered in a cell (it doesn't parse, or a function gets the wrong number of arguments) is an error saying why. |
| `select` | `range`, `active?` | Sets the selection. |
| `format_range` | `range`, `bold?`, `italic?`, `underline?`, `fontName?`, `fontSize?`, `fontColor?`, `fillColor?`, `numberFormat?`, `horizontalAlign?`, `wrapText?`, `borders?` | Composes the `home.*` formatting commands. |
| `insert_chart` | `range`, `type`, `title?`, `subtype?`, `at?` | `insert.chart`. |
| `create_table` | `range`, `style?`, `header?`, `name?` | `insert.table`. |
| `sort_range` | `keys`, `range?`, `header?` | `data.sort`; keys are `"B"`, `"B desc"` or `{column, order}`. |
| `filter` | `column`, `range?`, `values?`, `custom?`, `top?`, `blanks?`, `clear?` | `data.filterBy` (turns AutoFilter on when needed). |
| `open_workbook` / `save_workbook` / `new_workbook` | `path` / `path?` / `sample?` | Files (`.xlsx`, `.csv`, `.tsv`, `.json`, `.html`). |
| `undo` / `redo` | `steps?` | History. |
| `screenshot` | `path` | PNG of the window (Remote only). |
| `list_functions` | `search?`, `category?` | Worksheet functions with signatures. |

Results are one text block holding pretty JSON; failures set `isError: true` with a readable message. Unknown tools,
missing required arguments and wrong argument types are JSON-RPC `-32602` errors; malformed JSON is `-32700`; unknown
methods `-32601`.

## Resources

- `gridcraft://workbook` — `document.inspect` of the active workbook.
- `gridcraft://commands` — every command with its parameter docs.
- `gridcraft://functions` — every worksheet function.

`prompts/list` returns an empty list.

## Example session

```text
→ {"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18"}}
→ {"jsonrpc":"2.0","method":"notifications/initialized"}
→ {"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"write_range","arguments":{"range":"A1","values":[["Item","Cost"],["Rent",1200],["Food",400],["Total","=SUM(B2:B3)"]]}}}
→ {"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"read_range","arguments":{"range":"B4"}}}
← {"jsonrpc":"2.0","id":3,"result":{"content":[{"type":"text","text":"{\n  \"range\": \"B4\", ... \"values\": [[1600.0]]\n}"}],"isError":false}}
```

## Embedding

`gridcraft-mcp` is a library: `Server::new(Box::new(Headless::new()))` or
`Server::new(Box::new(Remote::connect(&control_addr("7979"))?))`, then `Server::serve(stdin, stdout)` or
`Server::handle_line(line) -> Option<String>`, or `serve_http(&mut server, "127.0.0.1:8790", &HttpOptions::default())`
for HTTP. Implement the `Backend` trait (`call(method, params)`) to drive another
host.
