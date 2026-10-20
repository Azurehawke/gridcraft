//! GridCraft on Android, aimed at tablets with a keyboard, a stylus and a mouse.
//!
//! Android starts the app through `GridCraftActivity`, a `NativeActivity` (packaging/android/java:
//! it keeps the latest intent and does the updater's HTTP and install), which loads this library
//! and calls [`android_main`]. The UI is the desktop one (`gridcraft-ui-egui`); what differs:
//! - Files: Open, Save As, the exports and Insert Picture use GridCraft's own file browser over
//!   shared storage (`files`), which needs "All files access" granted once in Settings
//!   (`file_access`). The UI asks through `Services::ask_file` and gets the answer a frame later.
//! - Settings live in the app's data folder.
//! - Touch, S Pen, mouse, system bars, workbook tabs and the keyboard cover (`tablet`); the system
//!   clipboard through JNI (`clipboard`); workbooks from other apps ("Open with", `incoming`); new
//!   builds from the GitHub repository the APK was built from (`updater`).
//! - No printing to a printer, no `--control` channel.
//!
//! The approach (NativeActivity, the patched winit, JNI for what Rust can't reach) follows the
//! PdfCraft and WordCraft Android ports. Elsewhere the crate is empty, so `cargo build
//! --workspace` on a desktop still works.
#![cfg(target_os = "android")]
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

mod clipboard;
mod file_access;
mod files;
mod incoming;
mod tablet;
mod touch;
mod updater;

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::OnceLock;

use gridcraft_engine::Session;
use gridcraft_ui_egui::{FileRequest, Services, SheetApp};
use winit::platform::android::activity::AndroidApp;

/// Shared storage as apps see it (the device's "Internal storage").
pub(crate) const SHARED: &str = "/storage/emulated/0";

/// The app's internal data folder (settings, logs, received workbooks).
static APP_DIR: OnceLock<PathBuf> = OnceLock::new();

pub(crate) fn app_dir() -> Option<PathBuf> {
    APP_DIR.get().cloned()
}

/// The settings folder (`<app data>/gridcraft`), as `~/.config/gridcraft` on Linux.
fn config_dir() -> Option<PathBuf> {
    app_dir().map(|d| d.join("gridcraft"))
}

/// The interface settings (`ui.json`) and application options (`prefs.json`), as on the desktop.
/// First run: the device's language and its light or dark appearance.
fn load_prefs(app: &mut SheetApp) {
    app.ui.language = gridcraft_ui_egui::i18n::Language::system();
    app.ui.system_theme = true;
    let Some(dir) = config_dir() else { return };
    if let Ok(b) = std::fs::read(dir.join("ui.json"))
        && let Ok(ui) = serde_json::from_slice::<gridcraft_ui_egui::UiState>(&b)
    {
        let saved = serde_json::from_slice(&b).ok().and_then(|v| gridcraft_ui_egui::i18n::saved_language(&v));
        let language = saved.unwrap_or(app.ui.language);
        app.ui = ui;
        app.ui.language = language;
    }
    if let Ok(b) = std::fs::read(dir.join("prefs.json"))
        && let Ok(p) = serde_json::from_slice(&b)
    {
        app.session.prefs = p;
    }
}

/// Write the settings: on leaving the foreground (Android may then stop the app without warning)
/// and on exit.
pub(crate) fn save_prefs(app: &SheetApp) {
    let Some(dir) = config_dir() else { return };
    if let Err(e) = std::fs::create_dir_all(&dir) {
        log::warn!("settings: cannot create {}: {e}", dir.display());
        return;
    }
    if let Ok(b) = serde_json::to_vec_pretty(&app.ui)
        && let Err(e) = write_atomically(&dir.join("ui.json"), &b)
    {
        log::warn!("settings not saved: {e}");
    }
    if let Ok(b) = serde_json::to_vec_pretty(&app.session.prefs)
        && let Err(e) = write_atomically(&dir.join("prefs.json"), &b)
    {
        log::warn!("options not saved: {e}");
    }
}

/// Write `bytes` to a sibling temporary file and rename it over `path`, so Android stopping the
/// app mid-write leaves the previous settings in place.
fn write_atomically(path: &std::path::Path, bytes: &[u8]) -> std::io::Result<()> {
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "settings.json".into());
    let tmp = path.with_file_name(format!(".{name}.{}.tmp", std::process::id()));
    let written = std::fs::write(&tmp, bytes).and_then(|()| std::fs::rename(&tmp, path));
    if written.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    written
}

/// The file browser's shortcuts: the device's storage and its usual folders, SD cards and USB
/// drives that are plugged in, and GridCraft's own folder.
pub(crate) fn places() -> Vec<(String, PathBuf)> {
    let mut out = vec![
        ("Internal storage".to_string(), PathBuf::from(SHARED)),
        ("Documents".to_string(), PathBuf::from(SHARED).join("Documents")),
        ("Download".to_string(), PathBuf::from(SHARED).join("Download")),
    ];
    if let Ok(entries) = std::fs::read_dir("/storage") {
        let mut volumes: Vec<PathBuf> = entries
            .flatten()
            .filter(|e| {
                let name = e.file_name();
                let name = name.to_string_lossy();
                name != "emulated" && name != "self" && e.path().is_dir()
            })
            .map(|e| e.path())
            .collect();
        volumes.sort();
        for v in volumes {
            let name = v.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            out.push((format!("SD card or USB drive ({name})"), v));
        }
    }
    if let Some(dir) = app_dir() {
        out.push(("App folder".to_string(), dir));
    }
    out
}

/// File choices the UI asked for, for the host's file browser to answer (`tablet`).
pub(crate) type FileRequests = Rc<RefCell<Vec<FileRequest>>>;

/// What the host gives the app on Android.
fn services(requests: &FileRequests) -> Services {
    let requests = Rc::clone(requests);
    Services {
        ask_file: Some(Box::new(move |req| requests.borrow_mut().push(req))),
        copy_html: Some(Box::new(|html, text| if clipboard::set_html(html, text) { Ok(()) } else { Err("the clipboard is unavailable".into()) })),
        paste_html: Some(Box::new(clipboard::html)),
        paste_text: Some(Box::new(clipboard::text)),
        open_url: Some(Box::new(|url| {
            // Only http, https and mailto addresses leave GridCraft, as on the desktop.
            match gridcraft_engine::links::classify(url) {
                gridcraft_engine::links::LinkKind::External(target) => file_access::open_url(&target),
                _ => log::warn!("refused to open {url:?}: only http, https and mailto links are opened"),
            }
        })),
        ..Default::default()
    }
}

/// Entry point, called by android-activity on its own thread once the activity is created.
#[allow(unsafe_code)] // Exported by name: android-activity looks this symbol up.
#[unsafe(no_mangle)]
fn android_main(android: AndroidApp) {
    android_logger::init_once(android_logger::Config::default().with_max_level(log::LevelFilter::Info).with_tag("GridCraft"));
    std::panic::set_hook(Box::new(|info| {
        let trace = std::backtrace::Backtrace::force_capture();
        log::error!("internal error: {info}\n{trace}");
    }));

    if let Some(dir) = android.internal_data_path() {
        let _ = APP_DIR.set(dir.clone());
        let tmp = dir.join("tmp");
        let _ = std::fs::create_dir_all(&tmp);
        // Android sets none of these. The file browser's Home and `~` are shared storage;
        // temporary files go where the app may write. Nothing else runs yet: this is the app's
        // only thread besides Java's.
        #[allow(unsafe_code)]
        // SAFETY: called before eframe or the app start any thread that could read the environment.
        unsafe {
            std::env::set_var("XDG_CONFIG_HOME", &dir);
            std::env::set_var("HOME", SHARED);
            std::env::set_var("TMPDIR", &tmp);
        }
        log::info!("GridCraft {} data folder {}", env!("CARGO_PKG_VERSION"), dir.display());
    }
    file_access::set_app(android.clone());
    log::info!("All files access: {}", if file_access::granted() { "granted" } else { "not granted" });
    let shell_android = android.clone();

    let options = eframe::NativeOptions { android_app: Some(android), renderer: eframe::Renderer::Wgpu, ..Default::default() };
    let result = eframe::run_native(
        "GridCraft",
        options,
        Box::new(|cc| {
            let requests = FileRequests::default();
            let mut session = Session::new();
            session.new_workbook();
            let mut app = SheetApp::new(session, services(&requests));
            load_prefs(&mut app);
            SheetApp::setup_context_for_language(&cc.egui_ctx, app.ui.dark, app.ui.language);
            if let Some(state) = &cc.wgpu_render_state {
                let info = state.adapter.get_info();
                log::info!("renderer: wgpu on {} ({:?}, {:?})", info.name, info.device_type, info.backend);
            }
            app.after_engine();
            Ok(Box::new(tablet::TabletApp::new(app, requests, shell_android)))
        }),
    );
    if let Err(e) = result {
        log::error!("GridCraft stopped: {e}");
    }
}
