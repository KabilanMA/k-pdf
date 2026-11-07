//! The Compress dialog: saving a smaller copy without losing anything.
//!
//! The copy includes unsaved changes. Lossless compression (duplicates
//! merged, streams recompressed, JPEGs optimized) is always used; Brotli,
//! which older readers can't open, is an option that is off by default
//! and shows a warning as soon as it is chosen. The work runs in the
//! background (see `main.rs`); the dialog then shows the result.

use eframe::egui::{self, Color32, RichText};
use k_pdf::compress::{CompressOptions, CompressReport};

/// Readers known to open Brotli-compressed PDFs, and some that can't.
pub const BROTLI_WARNING: &str = "Brotli makes the file a PDF 2.0 file using a 2026 extension. \
Only recent readers can open it, such as current Chrome and Firefox (PDFium, pdf.js) and MuPDF. \
Many others will refuse it or show blank pages: older versions of Acrobat and Foxit, \
Linux viewers built on Poppler (Evince, Okular), many phone apps and printers. \
It is also no longer valid PDF/A. Keep the original if you share the file.";

pub enum State {
    Closed,
    /// Choosing the options.
    Options,
    /// Running: the file being written.
    Working(std::path::PathBuf),
    Done { target: std::path::PathBuf, before: u64, report: CompressReport, brotli: bool },
}

pub struct Compress {
    pub state: State,
    jpeg: bool,
    brotli: bool,
}

impl Default for Compress {
    fn default() -> Self {
        Compress { state: State::Closed, jpeg: true, brotli: false }
    }
}

/// What the user asked for in the dialog.
pub enum Action {
    /// Pick where to save, then compress.
    Start(CompressOptions),
    /// Open the compressed file.
    Open(std::path::PathBuf),
}

impl Compress {
    /// Whether Brotli is ticked.
    pub fn brotli_chosen(&self) -> bool {
        self.brotli
    }

    pub fn open(&mut self) {
        if !matches!(self.state, State::Working(_)) {
            self.state = State::Options;
        }
    }

    pub fn show(&mut self, ctx: &egui::Context) -> Option<Action> {
        if matches!(self.state, State::Closed) {
            return None;
        }
        let mut action = None;
        let mut close = false;
        let modal = egui::Modal::new(egui::Id::new("compress")).show(ctx, |ui| {
            ui.set_width(440.0);
            ui.heading("Compress");
            ui.add_space(6.0);
            match &self.state {
                State::Closed => {}
                State::Options => {
                    ui.label(
                        "Saves a smaller copy that looks and works exactly the same: nothing is \
                         lost, not even image quality. Unsaved changes are included.",
                    );
                    ui.add_space(8.0);
                    ui.label(RichText::new("Always done").strong());
                    ui.label("• identical parts (fonts, images, form appearances) stored once");
                    ui.label("• everything recompressed as tightly as possible");
                    ui.add_space(6.0);
                    ui.checkbox(&mut self.jpeg, "Optimize photos (JPEG)")
                        .on_hover_text("Re-encodes the same image data more efficiently: identical pixels, often 10–15% smaller");
                    ui.checkbox(&mut self.brotli, "Brotli compression (not compatible with many readers)")
                        .on_hover_text("Smaller still, using PDF 2.0's new Brotli extension");
                    if self.brotli {
                        ui.add_space(4.0);
                        egui::Frame::new()
                            .fill(Color32::from_rgba_unmultiplied(230, 140, 0, 40))
                            .stroke(egui::Stroke::new(1.0, Color32::from_rgb(230, 140, 0)))
                            .corner_radius(4.0)
                            .inner_margin(8.0)
                            .show(ui, |ui| {
                                ui.label(RichText::new("⚠ Many PDF readers can't open these files").strong().color(Color32::from_rgb(230, 140, 0)));
                                ui.label(BROTLI_WARNING);
                            });
                    }
                    ui.add_space(12.0);
                    ui.horizontal(|ui| {
                        let label = if self.brotli { "Compress with Brotli and save as…" } else { "Compress and save as…" };
                        if crate::theme::primary_button(ui, label).clicked() {
                            action = Some(Action::Start(CompressOptions { jpeg: self.jpeg, brotli: self.brotli }));
                        }
                        if ui.button("Cancel").clicked() {
                            close = true;
                        }
                    });
                }
                State::Working(target) => {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label(format!("Compressing into {}…", file_name(target)));
                    });
                    ui.weak("Large files with many images can take a while.");
                }
                State::Done { target, before, report, brotli } => {
                    let pct = 100.0 * (1.0 - report.size as f64 / (*before).max(1) as f64);
                    let line = if pct >= 0.05 {
                        format!("{} to {} ({pct:.1}% smaller)", size(*before), size(report.size))
                    } else {
                        format!("{} to {} (it was already as small as it gets)", size(*before), size(report.size))
                    };
                    ui.label(format!("Saved {}", file_name(target)));
                    ui.label(RichText::new(line).strong());
                    let mut parts = Vec::new();
                    if report.duplicates > 0 {
                        parts.push(format!("{} duplicates merged", report.duplicates));
                    }
                    if report.jpegs > 0 {
                        parts.push(format!("{} photos optimized", report.jpegs));
                    }
                    parts.push(format!("{} parts recompressed", report.streams));
                    ui.weak(parts.join(", "));
                    if *brotli {
                        ui.add_space(4.0);
                        if report.brotli_streams > 0 {
                            ui.colored_label(
                                Color32::from_rgb(230, 140, 0),
                                "⚠ Uses Brotli: only recent PDF readers can open this copy.",
                            );
                        } else {
                            ui.weak("Brotli didn't make anything smaller, so it wasn't used: every reader can open this copy.");
                        }
                    }
                    ui.add_space(12.0);
                    ui.horizontal(|ui| {
                        if crate::theme::primary_button(ui, "Open it").clicked() {
                            action = Some(Action::Open(target.clone()));
                        }
                        if ui.button("Close").clicked() {
                            close = true;
                        }
                    });
                }
            }
        });
        let working = matches!(self.state, State::Working(_));
        if (close || modal.should_close()) && !working {
            self.state = State::Closed;
        }
        if action.is_some() {
            self.state = State::Closed;
        }
        action
    }
}

fn file_name(path: &std::path::Path) -> String {
    path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
}

pub fn size(bytes: u64) -> String {
    match bytes {
        b if b >= 1 << 20 => format!("{:.2} MB", b as f64 / (1 << 20) as f64),
        b if b >= 1 << 10 => format!("{:.0} KB", b as f64 / 1024.0),
        b => format!("{b} bytes"),
    }
}
