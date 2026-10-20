//! GridCraft's own file browser, for Open, Save As, the exports, Save a Copy and Insert Picture:
//! Android has no file dialog an app can ask for a path from. The UI asks through
//! `Services::ask_file`; the host shows this browser and hands the answer back with
//! `SheetApp::finish_file_request`. Adapted from WordCraft's built-in browser.
//!
//! Shortcuts to the device's storage places, a folder path with Up, the folder's subfolders and
//! the files of the chosen type (rows tall enough for a finger), a file name field and the types.
//! Listing shared storage needs All files access; until it is given the browser says so, with a
//! button that opens the setting, and lists the folder again once access arrives.

use std::path::{Path, PathBuf};

use egui::{Align2, Color32, Rect, Sense, Stroke, Ui, pos2, vec2};
use gridcraft_ui_egui::FileRequest;
use gridcraft_ui_egui::theme::Tokens;

/// At most this many entries are listed; a bigger folder shows the first ones and says so.
const MAX_LISTED: usize = 5000;
/// At most this many folder entries are read, so a huge folder can't stall the window.
const MAX_SCANNED: usize = 100_000;
/// Row height: big enough to tap.
const ROW: f32 = 34.0;

/// One subfolder or file in the listing.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Entry {
    pub(crate) name: String,
    pub(crate) path: PathBuf,
    pub(crate) dir: bool,
}

/// A folder's subfolders (first) and files, by name.
#[derive(Debug, Default)]
pub(crate) struct Listing {
    pub(crate) entries: Vec<Entry>,
    /// The folder holds more than was listed.
    pub(crate) truncated: bool,
}

/// Whether `path` has one of `exts` (case-insensitive); `*` takes any file.
pub(crate) fn matches(path: &Path, exts: &[String]) -> bool {
    exts.iter().any(|e| e == "*") || path.extension().and_then(|e| e.to_str()).is_some_and(|e| exts.iter().any(|x| x.eq_ignore_ascii_case(e)))
}

/// The subfolders and the files with one of `exts` in `dir`, at most `cap` of them, folders first
/// and each by name. Hidden entries (a leading dot) are left out; unreadable entries are skipped.
pub(crate) fn list_dir(dir: &Path, exts: &[String], cap: usize) -> std::io::Result<Listing> {
    let mut listing = Listing::default();
    for (i, item) in std::fs::read_dir(dir)?.enumerate() {
        if i >= MAX_SCANNED {
            listing.truncated = true;
            break;
        }
        let Ok(item) = item else { continue };
        let name = item.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') {
            continue;
        }
        let path = item.path();
        // A symbolic link to a folder is a folder.
        let is_dir = match item.file_type() {
            Ok(t) if t.is_symlink() => std::fs::metadata(&path).is_ok_and(|m| m.is_dir()),
            Ok(t) => t.is_dir(),
            Err(_) => continue,
        };
        if !is_dir && !matches(&path, exts) {
            continue;
        }
        if listing.entries.len() >= cap {
            listing.truncated = true;
            break;
        }
        listing.entries.push(Entry { name, path, dir: is_dir });
    }
    listing.entries.sort_by_cached_key(|e| (!e.dir, e.name.to_lowercase()));
    Ok(listing)
}

/// What the user typed in the file name field, as a path: relative to `dir` or absolute. `None`
/// when nothing was typed.
pub(crate) fn resolve(dir: &Path, typed: &str) -> Option<PathBuf> {
    let typed = typed.trim();
    if typed.is_empty() {
        return None;
    }
    if typed == "~" {
        return Some(PathBuf::from(crate::SHARED));
    }
    if let Some(rest) = typed.strip_prefix("~/") {
        return Some(PathBuf::from(crate::SHARED).join(rest));
    }
    Some(dir.join(typed))
}

/// What OK (or Enter, or a double tap) did.
#[derive(Debug, PartialEq)]
pub(crate) enum Step {
    /// The browser stays open (it moved to a folder, or has something to say).
    Stay,
    /// The file to open or write.
    Pick(PathBuf),
}

/// A message under the listing.
#[derive(Clone, Debug, PartialEq)]
enum Note {
    None,
    NotFound(String),
    Replace(PathBuf),
}

/// The open browser.
pub(crate) struct FileBrowser {
    pub(crate) req: FileRequest,
    pub(crate) dir: PathBuf,
    /// The folder field (edited by the user, applied on Enter).
    location: String,
    /// The file name field.
    pub(crate) name: String,
    /// The file types, as (label, extensions).
    types: Vec<(String, Vec<String>)>,
    filter: usize,
    listing: Result<Listing, String>,
    /// The highlighted entry.
    selected: Option<PathBuf>,
    note: Note,
    focus_name: bool,
    /// Whether shared storage was accessible last frame; `None` before the first check.
    granted: Option<bool>,
    /// Why the storage setting couldn't be opened, if it couldn't.
    access_error: Option<String>,
}

impl FileBrowser {
    pub(crate) fn new(req: FileRequest, start: PathBuf) -> Self {
        let types = req.file_types().into_iter().map(|(l, e)| (l.to_string(), e.into_iter().map(str::to_string).collect())).collect();
        let name = req.suggested().to_string();
        let mut b = FileBrowser {
            req,
            dir: start.clone(),
            location: String::new(),
            name,
            types,
            filter: 0,
            listing: Ok(Listing::default()),
            selected: None,
            note: Note::None,
            // On a tablet the on-screen keyboard would cover the listing: only a save focuses the
            // name, and only with a keyboard attached does that matter little.
            focus_name: false,
            granted: None,
            access_error: None,
        };
        b.focus_name = b.req.is_save();
        b.navigate(start);
        b
    }

    fn exts(&self) -> &[String] {
        self.types.get(self.filter).map_or(&[], |(_, e)| e.as_slice())
    }

    /// Show the folder `dir` (an unreadable folder shows why, and Up still works).
    pub(crate) fn navigate(&mut self, dir: PathBuf) {
        self.listing = list_dir(&dir, self.exts(), MAX_LISTED).map_err(|e| e.to_string());
        self.location = dir.to_string_lossy().into_owned();
        self.dir = dir;
        self.selected = None;
        self.note = Note::None;
    }

    fn up(&mut self) {
        if let Some(parent) = self.dir.parent().map(Path::to_path_buf) {
            self.navigate(parent);
        }
    }

    /// OK: a highlighted folder, or a typed folder, is opened; otherwise the typed name is the
    /// pick. A save adds the type's extension to a name without one and asks once more before
    /// replacing a file; Open wants a file that exists.
    pub(crate) fn accept(&mut self) -> Step {
        if let Some(dir) = self.selected.take().filter(|p| p.is_dir()) {
            self.navigate(dir);
            return Step::Stay;
        }
        let Some(mut path) = resolve(&self.dir, &self.name) else { return Step::Stay };
        if path.is_dir() {
            self.name.clear();
            self.navigate(path);
            return Step::Stay;
        }
        if !self.req.is_save() {
            if path.is_file() {
                return Step::Pick(path);
            }
            self.note = Note::NotFound(self.name.trim().to_string());
            return Step::Stay;
        }
        if path.extension().is_none()
            && let Some(ext) = self.exts().first().filter(|e| e.as_str() != "*")
        {
            path.set_extension(ext);
        }
        if path.exists() && self.note != Note::Replace(path.clone()) {
            self.note = Note::Replace(path);
            return Step::Stay;
        }
        Step::Pick(path)
    }

    /// A tap on an entry: highlight it, and a file's name goes into the file name field.
    fn select(&mut self, e: &Entry) {
        self.selected = Some(e.path.clone());
        if !e.dir {
            self.name = e.name.clone();
        }
        self.note = Note::None;
    }

    fn title(&self) -> &'static str {
        match self.req {
            FileRequest::Open => "Open",
            FileRequest::Picture => "Insert Picture",
            FileRequest::SaveAs { .. } => "Save As",
            FileRequest::Export { .. } => "Export",
            FileRequest::SaveCopy { .. } => "Save a Copy",
        }
    }

    fn ok_label(&self) -> &'static str {
        match self.req {
            FileRequest::Open => "Open",
            FileRequest::Picture => "Insert",
            FileRequest::Export { .. } => "Export",
            FileRequest::SaveAs { .. } | FileRequest::SaveCopy { .. } => "Save",
        }
    }

    /// Draw the browser in `area` (the app's part of the screen). `Some` once it closes:
    /// `Some(None)` cancelled, `Some(Some(path))` the answer.
    pub(crate) fn show(&mut self, ctx: &egui::Context, area: Rect) -> Option<Option<PathBuf>> {
        let t = Tokens::get(ctx);
        // Dim what's behind, and keep taps from reaching it.
        egui::Area::new(egui::Id::new("gridcraft_android_files_shade")).order(egui::Order::Middle).fixed_pos(area.min).show(ctx, |ui| {
            let (r, _) = ui.allocate_exact_size(area.size(), Sense::click_and_drag());
            ui.painter().rect_filled(r, 0.0, Color32::from_black_alpha(90));
        });
        let width = (area.width() - 32.0).clamp(300.0, 680.0);
        let list_h = (area.height() - 330.0).clamp(140.0, 520.0);
        let mut done = None;
        let mut open = true;
        egui::Window::new(self.title())
            .id(egui::Id::new("gridcraft_android_files"))
            .order(egui::Order::Foreground)
            .collapsible(false)
            .resizable(false)
            .open(&mut open)
            .pivot(Align2::CENTER_CENTER)
            .fixed_pos(area.center())
            .show(ctx, |ui| {
                ui.set_width(width);
                done = self.body(ui, &t, list_h);
            });
        if !open {
            done = Some(None);
        }
        done
    }

    fn body(&mut self, ui: &mut Ui, t: &Tokens, list_h: f32) -> Option<Option<PathBuf>> {
        self.storage_bar(ui, t);
        let mut location_done = false;
        ui.horizontal(|ui| {
            if ui.add_enabled(self.dir.parent().is_some(), egui::Button::new("⬆ Up").min_size(vec2(64.0, 30.0))).clicked() {
                self.up();
            }
            let r = ui.add(egui::TextEdit::singleline(&mut self.location).desired_width(f32::INFINITY).min_size(vec2(0.0, 30.0)));
            if r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                location_done = true;
                let typed = self.location.clone();
                if let Some(dir) = resolve(&self.dir, &typed) {
                    self.navigate(dir);
                }
            }
        });
        ui.add_space(4.0);
        let mut pick = false;
        let mut go: Option<PathBuf> = None;
        let mut clicked: Option<Entry> = None;
        egui::Frame::NONE.fill(t.input_bg).stroke(Stroke::new(1.0, t.input_border)).inner_margin(2.0).show(ui, |ui| {
            let entries: &[Entry] = self.listing.as_ref().map(|l| l.entries.as_slice()).unwrap_or(&[]);
            egui::ScrollArea::vertical().max_height(list_h).min_scrolled_height(list_h).auto_shrink([false, false]).show_rows(
                ui,
                ROW,
                entries.len(),
                |ui, range| {
                    for e in entries.get(range).unwrap_or(&[]) {
                        let (r, resp) = ui.allocate_exact_size(vec2(ui.available_width(), ROW), Sense::click());
                        if self.selected.as_ref() == Some(&e.path) {
                            ui.painter().rect_filled(r, 3.0, t.accent_soft);
                        } else if resp.hovered() {
                            ui.painter().rect_filled(r, 3.0, t.hover);
                        }
                        paint_glyph(ui.painter(), Rect::from_center_size(pos2(r.min.x + 18.0, r.center().y), vec2(18.0, 18.0)), e.dir, t);
                        ui.painter().with_clip_rect(r).text(
                            pos2(r.min.x + 38.0, r.center().y),
                            Align2::LEFT_CENTER,
                            &e.name,
                            egui::FontId::proportional(15.0),
                            t.text,
                        );
                        if resp.double_clicked() {
                            if e.dir {
                                go = Some(e.path.clone());
                            } else {
                                clicked = Some(e.clone());
                                pick = true;
                            }
                        } else if resp.clicked() {
                            // One tap opens a folder: there is no hover to show it's a target.
                            if e.dir {
                                go = Some(e.path.clone());
                            } else {
                                clicked = Some(e.clone());
                            }
                        }
                    }
                },
            );
        });
        if let Some(e) = clicked {
            self.select(&e);
        }
        if let Some(dir) = go {
            self.navigate(dir);
        }
        let message = match (&self.listing, &self.note) {
            (_, Note::NotFound(name)) => Some((format!("{name} wasn't found."), t.danger)),
            (_, Note::Replace(p)) => {
                let name = p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                Some((format!("{name} already exists. Choose {} again to replace it.", self.ok_label()), t.danger))
            }
            (Err(e), _) => Some((format!("This folder can't be read: {e}"), t.danger)),
            (Ok(l), _) if l.truncated => Some((format!("Showing the first {} items.", l.entries.len()), t.text_dim)),
            (Ok(l), _) if l.entries.is_empty() => Some(("Nothing of this type here.".to_string(), t.text_dim)),
            _ => None,
        };
        if let Some((text, color)) = message {
            ui.label(egui::RichText::new(text).color(color));
        }
        ui.add_space(6.0);
        let mut refilter = false;
        ui.horizontal(|ui| {
            let label = ui.label("File name:");
            let width = (ui.available_width() - 190.0).max(120.0);
            let r = ui.add(egui::TextEdit::singleline(&mut self.name).desired_width(width).min_size(vec2(0.0, 30.0))).labelled_by(label.id);
            if self.focus_name {
                r.request_focus();
                self.focus_name = false;
            }
            if r.changed() {
                self.selected = None;
                self.note = Note::None;
            }
            let before = self.filter;
            let current = self.types.get(self.filter).map(|(l, _)| l.clone()).unwrap_or_default();
            egui::ComboBox::from_id_salt("gridcraft_android_files_type").selected_text(current).width(170.0).show_ui(ui, |ui| {
                for (i, (l, _)) in self.types.iter().enumerate() {
                    ui.selectable_value(&mut self.filter, i, l.as_str());
                }
            });
            refilter = self.filter != before;
        });
        if refilter {
            self.navigate(self.dir.clone());
        }
        ui.add_space(8.0);
        let mut cancel = ui.input(|i| i.key_pressed(egui::Key::Escape));
        let enter = !location_done && ui.input(|i| i.key_pressed(egui::Key::Enter));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            cancel |= ui.add(egui::Button::new("Cancel").min_size(vec2(90.0, 32.0))).clicked();
            let ok = egui::Button::new(egui::RichText::new(self.ok_label()).color(Color32::WHITE)).fill(t.accent).min_size(vec2(90.0, 32.0));
            pick |= ui.add(ok).clicked() || enter;
        });
        if cancel {
            return Some(None);
        }
        if pick && let Step::Pick(path) = self.accept() {
            return Some(Some(path));
        }
        None
    }

    /// Above the listing: a request for All files access while it isn't given, and shortcuts to
    /// the storage places.
    fn storage_bar(&mut self, ui: &mut Ui, t: &Tokens) {
        let granted = crate::file_access::granted();
        if self.granted == Some(false) && granted {
            // Access was just turned on (in Settings): what couldn't be read before can be now.
            self.access_error = None;
            self.navigate(self.dir.clone());
        }
        self.granted = Some(granted);
        if !granted {
            egui::Frame::NONE.fill(t.notice_info_bg).stroke(Stroke::new(1.0, t.notice_info_border)).inner_margin(8.0).show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.label(
                    egui::RichText::new(
                        "To open and save workbooks in your folders, allow GridCraft to manage all files. Without it, GridCraft can only use its own app folder.",
                    )
                    .color(t.text),
                );
                ui.add_space(4.0);
                if ui.add(egui::Button::new(egui::RichText::new("Allow access…").color(Color32::WHITE)).fill(t.accent)).clicked() {
                    self.access_error = crate::file_access::request().err();
                }
                if let Some(e) = &self.access_error {
                    ui.label(
                        egui::RichText::new(format!(
                            "Settings didn't open ({e}). Turn it on in Settings › Apps › Special access › All files access › GridCraft."
                        ))
                        .color(t.danger),
                    );
                }
            });
            ui.add_space(6.0);
        }
        let places = crate::places();
        if !places.is_empty() {
            ui.horizontal_wrapped(|ui| {
                for (label, dir) in places {
                    let here = self.dir == dir;
                    if ui.add(egui::Button::selectable(here, label.as_str()).min_size(vec2(0.0, 30.0))).clicked() {
                        self.navigate(dir);
                    }
                }
            });
            ui.add_space(4.0);
        }
    }
}

/// A small folder or page shape before an entry's name.
fn paint_glyph(p: &egui::Painter, r: Rect, dir: bool, t: &Tokens) {
    if dir {
        let tab = Rect::from_min_size(r.min + vec2(0.0, 2.0), vec2(r.width() * 0.45, 4.0));
        let body = Rect::from_min_max(r.min + vec2(0.0, 5.0), r.max - vec2(0.0, 2.0));
        p.rect_filled(tab, 1.0, t.accent);
        p.rect_filled(body, 2.0, t.accent);
    } else {
        let page = Rect::from_center_size(r.center(), vec2(r.width() * 0.7, r.height()));
        p.rect_stroke(page, 1.5, Stroke::new(1.3, t.text_dim), egui::StrokeKind::Inside);
        for k in 1..4 {
            let y = page.min.y + page.height() * k as f32 / 4.5 + 2.0;
            p.hline(page.min.x + 3.0..=page.max.x - 3.0, y, Stroke::new(1.0, t.text_dim));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("gridcraft-android-files-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn listing_filters_sorts_and_caps() {
        let dir = scratch("list");
        for f in ["b.xlsx", "A.XLSX", "notes.txt", ".hidden.xlsx", "c.csv"] {
            std::fs::write(dir.join(f), b"x").unwrap();
        }
        std::fs::create_dir(dir.join("zeta")).unwrap();
        let exts: Vec<String> = ["xlsx", "csv"].iter().map(|s| s.to_string()).collect();
        let l = list_dir(&dir, &exts, 100).unwrap();
        let names: Vec<&str> = l.entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, ["zeta", "A.XLSX", "b.xlsx", "c.csv"]);
        assert!(list_dir(&dir.join("missing"), &exts, 10).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn save_adds_the_extension_and_asks_before_replacing() {
        let dir = scratch("save");
        std::fs::write(dir.join("old.xlsx"), b"x").unwrap();
        let mut b = FileBrowser::new(FileRequest::SaveAs { suggested: "Book1.xlsx".into() }, dir.clone());
        b.name = "budget".into();
        assert_eq!(b.accept(), Step::Pick(dir.join("budget.xlsx")));
        b.name = "old".into();
        assert_eq!(b.accept(), Step::Stay, "asks before replacing");
        assert_eq!(b.accept(), Step::Pick(dir.join("old.xlsx")));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
