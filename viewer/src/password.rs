//! Password-protected PDFs: the passwords given this session, and the
//! dialog asking for one.
//!
//! The workers, the editor and saving each open the file themselves, so
//! the password that opened a file is kept (in memory, for the session)
//! and every place that opens it uses [`open`]. A file whose user password
//! is empty opens without asking.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use eframe::egui::{self, Id, Pos2, Rect, Sense, Stroke, StrokeKind, Vec2};

use crate::icons::{self, Icon};

/// Passwords that opened files, by path.
static PASSWORDS: Mutex<Vec<(PathBuf, String)>> = Mutex::new(Vec::new());

/// The password that opened `path` (empty if none was needed).
pub fn password_for(path: &Path) -> String {
    let list = PASSWORDS.lock().unwrap_or_else(|e| e.into_inner());
    list.iter().find(|(p, _)| p == path).map(|(_, pw)| pw.clone()).unwrap_or_default()
}

fn remember(path: &Path, password: &str) {
    let mut list = PASSWORDS.lock().unwrap_or_else(|e| e.into_inner());
    list.retain(|(p, _)| p != path);
    list.push((path.to_path_buf(), password.to_string()));
}

fn forget(path: &Path) {
    PASSWORDS.lock().unwrap_or_else(|e| e.into_inner()).retain(|(p, _)| p != path);
}

/// Opens a PDF with the password given for it, if any.
pub fn open(path: &Path) -> k_pdf::Result<k_pdf::Document> {
    k_pdf::Document::open_with_password(path, &password_for(path))
}

/// Opens a PDF held in memory (a copy of `path`'s document, as when
/// compressing it), with `path`'s password.
pub fn from_bytes(data: Vec<u8>, path: &Path) -> k_pdf::Result<k_pdf::Document> {
    k_pdf::Document::from_bytes_with_password(data, &password_for(path))
}

/// What the password is for.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Purpose {
    /// Opening the file.
    Open,
    /// Inserting its pages at a position.
    Insert(usize),
}

/// What the user answered.
pub enum Answer {
    /// Try the file again (the password is remembered for it).
    Retry(PathBuf, Purpose),
    Cancel(Purpose),
}

/// The dialog asking for a file's password.
pub struct Prompt {
    path: PathBuf,
    purpose: Purpose,
    password: String,
    show: bool,
    /// The last password tried didn't open it.
    wrong: bool,
    /// Waiting to hear whether the password opens it.
    checking: bool,
    focus: bool,
    /// When a wrong password was reported (the field shakes).
    shake: Option<f64>,
}

impl Prompt {
    pub fn new(path: PathBuf, purpose: Purpose) -> Prompt {
        Prompt { path, purpose, password: String::new(), show: false, wrong: false, checking: false, focus: true, shake: None }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The file still wants a password: the one tried was wrong.
    pub fn rejected(&mut self, ctx: &egui::Context) {
        forget(&self.path);
        self.wrong = self.checking || self.wrong;
        self.checking = false;
        self.focus = true;
        if self.wrong {
            self.shake = Some(ctx.input(|i| i.time));
        }
    }

    pub fn show(&mut self, ctx: &egui::Context) -> Option<Answer> {
        let mut answer = None;
        let name = self.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let modal = egui::Modal::new(Id::new("password")).show(ctx, |ui| {
            ui.set_width(380.0);
            let p = crate::theme::palette(ui);
            ui.horizontal(|ui| {
                let (rect, _) = ui.allocate_exact_size(Vec2::splat(30.0), Sense::hover());
                ui.painter().circle_filled(rect.center(), 15.0, p.accent.gamma_multiply(0.18));
                icons::paint(ui.painter(), Rect::from_center_size(rect.center(), Vec2::splat(18.0)), Icon::Lock, p.accent);
                ui.add_space(4.0);
                ui.heading("Password required");
            });
            ui.add_space(6.0);
            let what = match self.purpose {
                Purpose::Open => "open it",
                Purpose::Insert(_) => "insert its pages",
            };
            ui.label(format!("“{name}” is protected. Enter its password to {what}."));
            ui.add_space(10.0);

            // The field (shaking for a moment after a wrong password), with
            // a button to show what was typed.
            let now = ui.input(|i| i.time);
            let offset = match self.shake {
                Some(start) if now - start < 0.45 => {
                    ui.ctx().request_repaint();
                    let t = (now - start) as f32;
                    (t * 55.0).sin() * 7.0 * (1.0 - t / 0.45)
                }
                _ => 0.0,
            };
            let mut submit = false;
            ui.horizontal(|ui| {
                ui.add_space(8.0 + offset);
                let field = egui::TextEdit::singleline(&mut self.password)
                    .password(!self.show)
                    .hint_text("Password")
                    .desired_width(ui.available_width() - 48.0 - offset)
                    .margin(egui::Margin::symmetric(8, 6));
                let response = ui.add_enabled(!self.checking, field);
                if self.wrong {
                    ui.painter().rect_stroke(response.rect, 4.0, Stroke::new(1.5, p.danger), StrokeKind::Outside);
                }
                if std::mem::take(&mut self.focus) {
                    response.request_focus();
                }
                if response.changed() {
                    self.wrong = false;
                }
                submit = response.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                let eye = if self.show { Icon::EyeOff } else { Icon::Eye };
                let tip = if self.show { "Hide the password" } else { "Show the password" };
                if icons::button(ui, eye, None, self.show, true).on_hover_text(tip).clicked() {
                    self.show = !self.show;
                    self.focus = true;
                }
            });
            ui.add_space(4.0);
            // Room for the message, so the dialog doesn't jump.
            let (rect, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 18.0), Sense::hover());
            if self.wrong {
                ui.painter().text(
                    Pos2::new(rect.left() + 8.0, rect.center().y),
                    egui::Align2::LEFT_CENTER,
                    "That password didn't work. Check it and try again.",
                    egui::FontId::proportional(13.0),
                    p.danger,
                );
            } else if self.checking {
                ui.painter().text(
                    Pos2::new(rect.left() + 8.0, rect.center().y),
                    egui::Align2::LEFT_CENTER,
                    "Unlocking…",
                    egui::FontId::proportional(13.0),
                    p.weak,
                );
            }
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                let ready = !self.password.is_empty() && !self.checking;
                let open = ui.add_enabled(ready, egui::Button::new(egui::RichText::new(if self.purpose == Purpose::Open { "Open" } else { "Insert" }).color(p.on_accent)).fill(p.accent));
                if open.clicked() || (submit && ready) {
                    self.checking = true;
                    remember(&self.path, &self.password);
                    answer = Some(Answer::Retry(self.path.clone(), self.purpose));
                }
                if ui.button("Cancel").clicked() {
                    answer = Some(Answer::Cancel(self.purpose));
                }
            });
        });
        if modal.should_close() && answer.is_none() {
            answer = Some(Answer::Cancel(self.purpose));
        }
        if matches!(answer, Some(Answer::Cancel(_))) {
            forget(&self.path);
        }
        answer
    }
}
