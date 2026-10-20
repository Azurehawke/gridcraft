//! What a tablet needs around the desktop UI.
//!
//! - **System bars.** NativeActivity draws under the status bar and the taskbar. Android reports
//!   the part of the window they leave free (the content rect); the app is laid out inside it,
//!   and egui is told the same insets so dialogs and menus stay clear too. The bars take the tab
//!   strip's colour, with icons that show on it.
//! - **Touch** (`touch`): the S Pen and a mouse are the pointer; fingers tap, scroll and pinch.
//! - **Keyboard.** With a keyboard cover or Bluetooth keyboard attached, text fields don't ask
//!   for the on-screen keyboard.
//! - **Clipboard.** What GridCraft copies goes on the system clipboard, and Ctrl+V pastes what
//!   other apps copied (`clipboard`).
//! - **Workbook tabs.** Each open workbook has a tab along the top (the desktop shows one at a
//!   time): + makes a new one, × closes one, Ctrl+Tab moves between them.
//! - **Files** (`files`): the UI's file choices are answered by GridCraft's own browser.
//! - **Workbooks from other apps** open as they arrive (`incoming`).

use std::path::{Path, PathBuf};

use egui::Event;
use gridcraft_ui_egui::SheetApp;
use gridcraft_ui_egui::theme::Tokens;
use serde_json::json;
use winit::platform::android::activity::AndroidApp;

use crate::files::FileBrowser;
use crate::incoming::Incoming;
use crate::touch::TouchRouter;

/// Height of the tab strip, in points.
const TAB_STRIP: f32 = 36.0;
/// Widest a tab gets, and narrowest before tabs stop shrinking.
const TAB_MAX: f32 = 230.0;
const TAB_MIN: f32 = 110.0;

pub struct TabletApp {
    app: SheetApp,
    /// File choices the UI asked for, answered one at a time by `browser`.
    requests: crate::FileRequests,
    browser: Option<FileBrowser>,
    /// The folder the browser showed last (where it opens next time).
    last_dir: Option<PathBuf>,
    android: AndroidApp,
    touches: TouchRouter,
    /// The insets last applied, in points.
    insets: egui::epaint::MarginF32,
    /// The app's part of the screen last frame (clear of the system bars).
    inner: egui::Rect,
    /// Look for a workbook sent by another app (`incoming`): at start-up and when the app comes
    /// back to the front.
    check_incoming: bool,
    focused: bool,
    /// New builds from the GitHub repository the APK came from (`updater`).
    updater: crate::updater::Updater,
    /// The system bars' colour last sent to Android.
    bar_color: Option<egui::Color32>,
}

impl TabletApp {
    pub fn new(app: SheetApp, requests: crate::FileRequests, android: AndroidApp) -> Self {
        Self {
            app,
            requests,
            browser: None,
            last_dir: None,
            android,
            touches: TouchRouter::default(),
            insets: egui::epaint::MarginF32::default(),
            inner: egui::Rect::NOTHING,
            check_incoming: true,
            focused: true,
            updater: crate::updater::Updater::default(),
            bar_color: None,
        }
    }

    fn say(&mut self, text: String) {
        self.app.toast = Some((text, gridcraft_ui_egui::now_ms()));
    }

    /// Bring workbook `i` to the front (a cell being edited is committed first; one that can't be
    /// keeps its workbook in front).
    fn switch_to(&mut self, i: usize) {
        if i == self.app.session.active_index() || i >= self.app.session.documents().len() {
            return;
        }
        if !self.app.commit_edit(0, 0, false, false) {
            return;
        }
        self.app.store_view_state();
        self.app.session.set_active(i);
        self.app.after_engine();
    }

    /// Close workbook `i`: its Close command asks to save unsaved changes first.
    fn close_workbook(&mut self, i: usize) {
        self.switch_to(i);
        if self.app.session.active_index() == i {
            self.app.run_or_alert("file.close", json!({}));
        }
    }

    /// Open the workbook another app sent, if there is one. One that only exists in the other
    /// app (an attachment, a cloud file) is first kept as a file of GridCraft's own, so Save
    /// writes somewhere real.
    fn open_incoming(&mut self) {
        let path = match crate::incoming::take() {
            None => return,
            Some(Incoming::Path(path)) => path,
            Some(Incoming::Bytes { name, bytes }) => match keep_copy(&name, &bytes) {
                Ok(path) => {
                    let msg = format!("Saved a copy of {name} in {}", path.parent().map_or_else(String::new, |p| p.display().to_string()));
                    self.say(msg);
                    path.to_string_lossy().into_owned()
                }
                Err(e) => {
                    self.say(format!("Couldn't open {name}: {e}"));
                    return;
                }
            },
            Some(Incoming::Failed(e)) => {
                self.say(e);
                return;
            }
        };
        log::info!("opening {path}");
        // Already open: bring its tab to the front.
        if let Some(i) = self.app.session.documents().iter().position(|d| d.path.as_deref() == Some(path.as_str())) {
            self.switch_to(i);
            return;
        }
        self.app.open_path(&path);
    }

    /// Where the browser starts: where it was last, else the front workbook's folder, else
    /// Documents, else the app's folder.
    fn browser_start(&self) -> PathBuf {
        let docs = PathBuf::from(crate::SHARED).join("Documents");
        self.last_dir
            .clone()
            .filter(|d| d.is_dir())
            .or_else(|| {
                self.app
                    .session
                    .active()
                    .and_then(|d| d.path.as_deref())
                    .and_then(|p| Path::new(p).parent())
                    .map(Path::to_path_buf)
                    .filter(|d| d.is_dir())
            })
            .or_else(|| Some(docs).filter(|d| d.is_dir() && crate::file_access::granted()))
            .or_else(crate::app_dir)
            .unwrap_or_else(|| PathBuf::from(crate::SHARED))
    }

    /// Show the next file choice the UI asked for, and finish it once answered.
    fn files(&mut self, ctx: &egui::Context) {
        if self.browser.is_none() {
            let next = {
                let mut q = self.requests.borrow_mut();
                (!q.is_empty()).then(|| q.remove(0))
            };
            if let Some(req) = next {
                self.browser = Some(FileBrowser::new(req, self.browser_start()));
            }
        }
        let Some(b) = self.browser.as_mut() else { return };
        let area = if self.inner.is_positive() { self.inner } else { ctx.content_rect() };
        if let Some(answer) = b.show(ctx, area) {
            self.last_dir = Some(b.dir.clone());
            let req = b.req.clone();
            self.browser = None;
            self.app.finish_file_request(req, answer.map(|p| p.to_string_lossy().into_owned()));
        }
    }

    /// The tab strip along the top: one tab per workbook (name, a dot for unsaved changes, and a
    /// close button, always shown since there is no hover on a touch screen), then New.
    fn tab_strip(&mut self, ui: &mut egui::Ui, rect: egui::Rect) {
        let t = Tokens::get(ui.ctx());
        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, 0.0, t.window);
        painter.hline(rect.x_range(), rect.max.y - 0.5, egui::Stroke::new(1.0, t.separator));
        let plus_w = 40.0;
        let docs: Vec<(String, bool)> = self.app.session.documents().iter().map(|d| (d.display_title(), d.is_dirty())).collect();
        let active = self.app.session.active_index();
        let n = docs.len().max(1) as f32;
        let w = ((rect.width() - plus_w - 8.0) / n).clamp(TAB_MIN, TAB_MAX);
        let font = egui::FontId::proportional(13.0);
        let mut x = rect.min.x + 4.0;
        let mut switch = None;
        let mut close = None;
        for (i, (title, dirty)) in docs.iter().enumerate() {
            if x + TAB_MIN > rect.max.x - plus_w {
                // No room: the rest are reached with Ctrl+Tab or by closing tabs.
                break;
            }
            let tab = egui::Rect::from_min_size(egui::pos2(x, rect.min.y + 4.0), egui::vec2(w - 4.0, rect.height() - 4.0));
            let on = i == active;
            let resp = ui.interact(tab, ui.id().with(("book_tab", i)), egui::Sense::click());
            let fill = if on {
                t.ribbon
            } else if resp.hovered() {
                t.hover
            } else {
                t.window
            };
            painter.rect_filled(tab, egui::CornerRadius { nw: 6, ne: 6, sw: 0, se: 0 }, fill);
            let x_btn = egui::Rect::from_center_size(egui::pos2(tab.max.x - 16.0, tab.center().y), egui::vec2(28.0, 28.0));
            let x_resp = ui.interact(x_btn, ui.id().with(("book_tab_close", i)), egui::Sense::click());
            if x_resp.hovered() {
                painter.rect_filled(x_btn.shrink(3.0), 4.0, t.hover);
            }
            let c = x_btn.center();
            let stroke = egui::Stroke::new(1.4, if on { t.text } else { t.text_dim });
            painter.line_segment([c + egui::vec2(-4.5, -4.5), c + egui::vec2(4.5, 4.5)], stroke);
            painter.line_segment([c + egui::vec2(4.5, -4.5), c + egui::vec2(-4.5, 4.5)], stroke);
            let mut name = Path::new(title).file_stem().map_or_else(|| title.clone(), |s| s.to_string_lossy().into_owned());
            if *dirty {
                name.push_str(" •");
            }
            let text_rect = egui::Rect::from_min_max(egui::pos2(tab.min.x + 10.0, tab.min.y), egui::pos2(x_btn.min.x - 2.0, tab.max.y));
            painter.with_clip_rect(text_rect).text(
                egui::pos2(text_rect.min.x, text_rect.center().y),
                egui::Align2::LEFT_CENTER,
                name,
                font.clone(),
                if on { t.text } else { t.text_dim },
            );
            if x_resp.clicked() {
                close = Some(i);
            } else if resp.clicked() {
                switch = Some(i);
            }
            x += w;
        }
        let plus = egui::Rect::from_center_size(egui::pos2(x + plus_w / 2.0, rect.center().y + 2.0), egui::vec2(32.0, 28.0));
        let plus_resp = ui.interact(plus, ui.id().with("book_tab_new"), egui::Sense::click()).on_hover_text("New workbook");
        if plus_resp.hovered() {
            painter.rect_filled(plus, 4.0, t.hover);
        }
        let c = plus.center();
        let stroke = egui::Stroke::new(1.6, t.text);
        painter.line_segment([c + egui::vec2(-6.0, 0.0), c + egui::vec2(6.0, 0.0)], stroke);
        painter.line_segment([c + egui::vec2(0.0, -6.0), c + egui::vec2(0.0, 6.0)], stroke);
        if let Some(i) = close {
            self.close_workbook(i);
        } else if let Some(i) = switch {
            self.switch_to(i);
        } else if plus_resp.clicked() && self.app.commit_edit(0, 0, false, false) {
            self.app.run_or_alert("file.new", json!({}));
        }
    }

    /// Colour the status bar and taskbar like the tab strip under them, with dark icons on a
    /// light colour and light ones on a dark colour.
    fn colour_system_bars(&mut self, color: egui::Color32) {
        if self.bar_color == Some(color) {
            return;
        }
        self.bar_color = Some(color);
        let [r, g, b, _] = color.to_array();
        let argb = i32::from_ne_bytes(u32::from_be_bytes([0xff, r, g, b]).to_ne_bytes());
        let luminance = 0.2126 * f32::from(r) + 0.7152 * f32::from(g) + 0.0722 * f32::from(b);
        let light = luminance > 140.0;
        let _ = crate::file_access::with_activity("colouring the system bars", |env, activity| {
            env.call_method(activity, "gcSetSystemBars", "(IZ)V", &[jni::objects::JValue::Int(argb), jni::objects::JValue::Bool(u8::from(light))])?;
            Ok(())
        });
    }

    /// With a keyboard cover or Bluetooth keyboard attached, typing arrives as key presses, so a
    /// text field doesn't ask for the on-screen keyboard (Samsung shows its keyboard toolbar for
    /// that request, which takes room and comes and goes as fields gain and lose focus).
    fn keep_soft_keyboard_away(&self, ctx: &egui::Context) {
        if matches!(self.android.config().keys_hidden(), ndk::configuration::KeysHidden::No) {
            ctx.output_mut(|o| o.ime = None);
        }
    }

    /// The window's edges covered by system bars, in points, from Android's content rect.
    fn system_bar_insets(&self, ctx: &egui::Context, raw: &egui::RawInput) -> Option<egui::epaint::MarginF32> {
        let ppp = ctx.pixels_per_point();
        let screen = raw.screen_rect?;
        if !(ppp.is_finite() && ppp > 0.0) {
            return None;
        }
        let r = self.android.content_rect();
        if r.right <= r.left || r.bottom <= r.top {
            return None;
        }
        let (w, h) = (screen.width() * ppp, screen.height() * ppp);
        let inset = |px: f32| (px / ppp).clamp(0.0, 400.0);
        Some(egui::epaint::MarginF32 {
            left: inset(r.left as f32),
            top: inset(r.top as f32),
            right: inset(w - r.right as f32),
            bottom: inset(h - r.bottom as f32),
        })
    }

    /// Ctrl+V pastes what other apps copied: egui-winit's own clipboard only knows GridCraft's
    /// copies (and sends no paste at all when it holds nothing).
    fn system_paste(raw: &mut egui::RawInput) {
        let ctrl_v = raw.events.iter().any(|e| {
            matches!(e, Event::Key { key: egui::Key::V, pressed: true, modifiers, .. } if modifiers.command && !modifiers.alt && !modifiers.shift)
        });
        let has_paste = raw.events.iter().any(|e| matches!(e, Event::Paste(_)));
        if !(ctrl_v || has_paste) {
            return;
        }
        let Some(text) = crate::clipboard::text() else { return };
        if has_paste {
            for e in &mut raw.events {
                if let Event::Paste(p) = e {
                    *p = text.clone();
                }
            }
        } else {
            raw.events.push(Event::Paste(text));
        }
    }
}

impl eframe::App for TabletApp {
    fn raw_input_hook(&mut self, ctx: &egui::Context, raw: &mut egui::RawInput) {
        if let Some(insets) = self.system_bar_insets(ctx, raw) {
            if insets != self.insets {
                log::info!("system bar insets (points): {insets:?}");
            }
            self.insets = insets;
            raw.safe_area_insets = Some(egui::SafeAreaInsets(insets));
        }
        if raw.focused && !self.focused {
            // Back in front, perhaps with another workbook to open.
            self.check_incoming = true;
        }
        if !raw.focused && self.focused {
            // Android may stop a backgrounded app without warning: keep the settings now.
            crate::save_prefs(&self.app);
        }
        self.focused = raw.focused;
        self.touches.route(&mut raw.events, raw.time.unwrap_or(0.0));
        if self.touches.waiting() {
            // A lift held back for the next frame, or a finger that may become a long press.
            ctx.request_repaint_after(std::time::Duration::from_millis(50));
        }
        Self::system_paste(raw);
        // Ctrl+Tab and Ctrl+Shift+Tab move between workbooks.
        let mut step: i64 = 0;
        raw.events.retain(|e| match e {
            Event::Key { key: egui::Key::Tab, pressed, modifiers, .. } if modifiers.ctrl => {
                if *pressed {
                    step += if modifiers.shift { -1 } else { 1 };
                }
                false
            }
            _ => true,
        });
        let n = self.app.session.documents().len() as i64;
        if step != 0 && n > 1 {
            let next = (self.app.session.active_index() as i64 + step).rem_euclid(n);
            self.switch_to(usize::try_from(next).unwrap_or(0));
        }
        self.app.raw_input_hook(raw);
    }

    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if std::mem::take(&mut self.check_incoming) {
            self.open_incoming();
        }
        // At start-up, then at most hourly (`updater`).
        self.updater.maybe_check(ctx.input(|i| i.time));
        self.app.logic(ctx);
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let full = ui.max_rect();
        let m = self.insets;
        let mut inner = egui::Rect::from_min_max(full.min + egui::vec2(m.left, m.top), full.max - egui::vec2(m.right, m.bottom));
        if inner.width() < 200.0 || inner.height() < 200.0 {
            inner = full;
        }
        self.inner = inner;
        // Behind the status bar and taskbar: the tab strip's colour, so the bars read as part of
        // the app, and Android draws their icons dark or light to match.
        let bar = Tokens::get(ui.ctx()).window;
        ui.painter().rect_filled(full, 0.0, bar);
        self.colour_system_bars(bar);
        let strip = egui::Rect::from_min_size(inner.min, egui::vec2(inner.width(), TAB_STRIP));
        let body = egui::Rect::from_min_max(egui::pos2(inner.min.x, strip.max.y), inner.max);
        self.tab_strip(ui, strip);
        let app = &mut self.app;
        ui.scope_builder(egui::UiBuilder::new().max_rect(body), |ui| {
            ui.set_clip_rect(body);
            app.ui(ui);
        });
        self.files(ui.ctx());
        self.updater.show(ui.ctx(), inner);
        // What GridCraft copied as plain text this frame (a cell editor, a dialog field) goes on
        // the system clipboard too; cell copies went there as HTML already.
        let copied = ui.ctx().output(|o| {
            o.commands.iter().rev().find_map(|c| match c {
                egui::OutputCommand::CopyText(t) => Some(t.clone()),
                _ => None,
            })
        });
        if let Some(text) = copied {
            crate::clipboard::set_text(&text);
        }
        self.keep_soft_keyboard_away(ui.ctx());
    }

    fn on_exit(&mut self) {
        crate::save_prefs(&self.app);
    }
}

/// Keep the bytes of a workbook that only exists in another app as a file GridCraft can open
/// and save: in Documents/GridCraft when shared storage is accessible, else in the app's own
/// folder. An existing file of that name is never replaced ("Budget (2).xlsx").
fn keep_copy(name: &str, bytes: &[u8]) -> Result<PathBuf, String> {
    let shared = PathBuf::from(crate::SHARED).join("Documents/GridCraft");
    let dir = if crate::file_access::granted() { shared } else { crate::app_dir().map(|d| d.join("Received")).ok_or("no app folder")? };
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let path = free_name(&dir, &safe_file_name(name));
    std::fs::write(&path, bytes).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(path)
}

/// `name` without folders or characters a file name can't hold; never empty.
fn safe_file_name(name: &str) -> String {
    let base = name.rsplit(['/', '\\']).next().unwrap_or(name);
    let cleaned: String =
        base.chars().map(|c| if c.is_control() || matches!(c, ':' | '*' | '?' | '"' | '<' | '>' | '|') { '_' } else { c }).collect();
    let cleaned = cleaned.trim().trim_start_matches('.').to_string();
    if cleaned.is_empty() { "Book.xlsx".to_string() } else { cleaned }
}

/// `dir/name`, or `dir/stem (2).ext` and so on when that file already exists.
fn free_name(dir: &Path, name: &str) -> PathBuf {
    let first = dir.join(name);
    if !first.exists() {
        return first;
    }
    let p = Path::new(name);
    let stem = p.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| name.to_string());
    let ext = p.extension().map(|e| format!(".{}", e.to_string_lossy())).unwrap_or_default();
    for n in 2..10_000 {
        let candidate = dir.join(format!("{stem} ({n}){ext}"));
        if !candidate.exists() {
            return candidate;
        }
    }
    first
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn copies_of_incoming_workbooks_get_free_names() {
        assert_eq!(safe_file_name("../a/b:c.xlsx"), "b_c.xlsx");
        assert_eq!(safe_file_name(""), "Book.xlsx");
        let dir = std::env::temp_dir().join(format!("gridcraft-android-free-name-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("R.xlsx"), b"x").unwrap();
        assert_eq!(free_name(&dir, "R.xlsx"), dir.join("R (2).xlsx"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
