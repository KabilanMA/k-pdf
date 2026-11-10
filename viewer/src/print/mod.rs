//! Printing: the Print dialog (Ctrl+P) and sending the job.
//!
//! The dialog offers the printer (with the paper sizes, two-sided printing
//! and color it supports), copies and collating, which pages (all, the
//! current one, or a list like "1-3, 5"; odd or even only; in reverse),
//! two-sided printing, orientation, pages per sheet, scaling, paper size,
//! black and white, and whether comments are printed, with a preview of
//! each sheet. Unsaved changes are printed.
//!
//! The pages to print are first written to a temporary PDF (with the
//! edits, form values, and without comments if they are left out), then:
//! - on Linux and macOS it goes to CUPS with `lp` (see `cups.rs`), which
//!   also does the pages per sheet, scaling and paper handling;
//! - on Windows each sheet is rendered and drawn on the printer through
//!   GDI (see `windows.rs`).

#[cfg(unix)]
mod cups;
#[cfg(windows)]
mod windows;

use std::path::Path;
use std::sync::mpsc::Receiver;

use eframe::egui::{self, Color32, Pos2, Rect, RichText, Sense, Stroke, StrokeKind, TextureId, Vec2};

/// A printer, with what it can do.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Printer {
    /// Its name for the system (what jobs are sent to).
    pub name: String,
    /// Its name for people.
    pub label: String,
    pub is_default: bool,
    pub papers: Vec<Paper>,
    /// The paper used when none is chosen (an id in `papers`).
    pub default_paper: Option<String>,
    pub duplex: bool,
    pub color: bool,
    /// How to ask for black and white, when the printer has its own way
    /// (CUPS: an `-o key=value` option).
    pub gray_option: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Paper {
    /// The printer's name for it.
    pub id: String,
    pub label: String,
    /// Width × height in millimeters, when known.
    pub size_mm: Option<(f32, f32)>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Range {
    All,
    Current,
    /// The pages typed in the pages field.
    Listed,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Subset {
    All,
    Odd,
    Even,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Sides {
    One,
    /// Flipped on the long edge (like a book).
    LongEdge,
    /// Flipped on the short edge (like a notepad).
    ShortEdge,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Orientation {
    /// As the pages are.
    Auto,
    Portrait,
    Landscape,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Scaling {
    /// Each page as large as fits the paper.
    Fit,
    /// At its real size (cut off if larger than the paper).
    Actual,
    /// Real size, made smaller only if it doesn't fit.
    Shrink,
}

/// Everything chosen in the dialog.
#[derive(Clone, Debug)]
pub struct Settings {
    pub printer: Option<String>,
    pub copies: u32,
    pub collate: bool,
    pub range: Range,
    pub pages_text: String,
    pub subset: Subset,
    pub reverse: bool,
    pub sides: Sides,
    pub orientation: Orientation,
    pub per_sheet: u32,
    pub scaling: Scaling,
    /// A paper id of the printer; `None`: the printer's default.
    pub paper: Option<String>,
    pub gray: bool,
    pub comments: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            printer: None,
            copies: 1,
            collate: true,
            range: Range::All,
            pages_text: String::new(),
            subset: Subset::All,
            reverse: false,
            sides: Sides::One,
            orientation: Orientation::Auto,
            per_sheet: 1,
            scaling: Scaling::Fit,
            paper: None,
            gray: false,
            comments: true,
        }
    }
}

/// Pages per sheet offered.
pub const PER_SHEET: [u32; 6] = [1, 2, 4, 6, 9, 16];

/// A print job, ready to send.
#[derive(Clone, Debug)]
pub struct Job {
    pub printer: Printer,
    /// Positions (in the view) of the pages to print, in printing order.
    pub positions: Vec<usize>,
    pub settings: Settings,
    pub title: String,
}

/// The printers on this computer.
pub fn list_printers() -> Result<Vec<Printer>, String> {
    #[cfg(unix)]
    return cups::printers();
    #[cfg(windows)]
    return windows::printers();
    #[cfg(not(any(unix, windows)))]
    Err("printing isn't available on this system".into())
}

/// Prints `file` (the pages of the job, already in order) as `job` says.
/// Returns what to tell the user.
pub fn send(file: &Path, job: &Job) -> Result<String, String> {
    #[cfg(unix)]
    return cups::print(file, job);
    #[cfg(windows)]
    return windows::print(file, job);
    #[cfg(not(any(unix, windows)))]
    {
        let _ = (file, job);
        Err("printing isn't available on this system".into())
    }
}

/// The positions to print, in order: the range, then odd or even page
/// numbers only, then reversed if asked.
pub fn selected_positions(s: &Settings, count: usize, current: usize) -> Result<Vec<usize>, String> {
    let mut positions: Vec<usize> = match s.range {
        Range::All => (0..count).collect(),
        Range::Current => vec![current.min(count.saturating_sub(1))],
        Range::Listed => crate::crop::parse_pages(&s.pages_text, count)?,
    };
    // Odd and even by page number (1-based, as shown).
    match s.subset {
        Subset::All => {}
        Subset::Odd => positions.retain(|p| p % 2 == 0),
        Subset::Even => positions.retain(|p| p % 2 == 1),
    }
    if positions.is_empty() {
        return Err("no pages are left to print with these choices".into());
    }
    if s.reverse {
        positions.reverse();
    }
    Ok(positions)
}

// ----------------------------------------------------------------------
// Sheet layout (the preview, and printing on Windows)
// ----------------------------------------------------------------------

/// Columns × rows of pages on a portrait sheet... turned for landscape.
fn grid(per_sheet: u32, landscape: bool) -> (u32, u32) {
    let (cols, rows) = match per_sheet {
        2 => (1, 2),
        4 => (2, 2),
        6 => (2, 3),
        9 => (3, 3),
        16 => (4, 4),
        _ => (1, 1),
    };
    if landscape { (rows, cols) } else { (cols, rows) }
}

/// Whether the sheet is used sideways. Automatic: so the pages fill it
/// best (2 or 6 portrait pages per sheet go on a landscape sheet).
pub fn sheet_landscape(s: &Settings, first_page: Vec2) -> bool {
    match s.orientation {
        Orientation::Portrait => false,
        Orientation::Landscape => true,
        Orientation::Auto => {
            let page_landscape = first_page.x > first_page.y;
            page_landscape != matches!(s.per_sheet, 2 | 6)
        }
    }
}

/// The cells of a sheet, left to right, top to bottom.
pub fn cells(sheet: Rect, per_sheet: u32, landscape: bool) -> Vec<Rect> {
    let (cols, rows) = grid(per_sheet, landscape);
    let (w, h) = (sheet.width() / cols as f32, sheet.height() / rows as f32);
    let mut out = Vec::new();
    for r in 0..rows {
        for c in 0..cols {
            out.push(Rect::from_min_size(sheet.min + Vec2::new(c as f32 * w, r as f32 * h), Vec2::new(w, h)));
        }
    }
    out
}

/// Where a page (`size` in points) goes in a cell. `per_point` is the
/// cell's units per point at real size.
pub fn place(cell: Rect, size: Vec2, scaling: Scaling, per_sheet: u32, per_point: f32) -> Rect {
    let fit = (cell.width() / size.x.max(1.0)).min(cell.height() / size.y.max(1.0));
    let scale = match scaling {
        // Several pages per sheet are always fitted to their cell.
        _ if per_sheet > 1 => fit,
        Scaling::Fit => fit,
        Scaling::Actual => per_point,
        Scaling::Shrink => fit.min(per_point),
    };
    Rect::from_center_size(cell.center(), size * scale)
}

/// Width × height in millimeters of a paper, from its name: common
/// names ("A4", "Letter"…) or names with the size in them
/// ("iso_a4_210x297mm", "na_letter_8.5x11in").
pub fn paper_mm(id: &str) -> Option<(f32, f32)> {
    let lower = id.to_ascii_lowercase();
    // A size written in the name.
    let tail = lower.rsplit('_').next().unwrap_or(&lower);
    for (unit, per) in [("mm", 1.0f32), ("in", 25.4)] {
        if let Some(dims) = tail.strip_suffix(unit)
            && let Some((w, h)) = dims.split_once('x')
            && let (Ok(w), Ok(h)) = (w.parse::<f32>(), h.parse::<f32>())
        {
            return Some((w * per, h * per));
        }
    }
    let base = lower.split(['.', '_']).find(|p| !p.is_empty() && *p != "na" && *p != "iso").unwrap_or(&lower);
    Some(match base {
        "letter" | "lettersmall" => (215.9, 279.4),
        "legal" => (215.9, 355.6),
        "executive" => (184.2, 266.7),
        "tabloid" | "ledger" | "11x17" => (279.4, 431.8),
        "statement" => (139.7, 215.9),
        "a3" => (297.0, 420.0),
        "a4" | "a4small" => (210.0, 297.0),
        "a5" => (148.0, 210.0),
        "a6" => (105.0, 148.0),
        "b4" => (250.0, 353.0),
        "b5" => (176.0, 250.0),
        "env10" | "com10" => (104.8, 241.3),
        "envdl" | "dl" => (110.0, 220.0),
        "envc5" | "c5" => (162.0, 229.0),
        "4x6" => (101.6, 152.4),
        _ => return None,
    })
}

/// A paper name for people.
pub fn paper_label(id: &str) -> String {
    let lower = id.to_ascii_lowercase();
    let known = [
        ("letter", "Letter"),
        ("legal", "Legal"),
        ("executive", "Executive"),
        ("tabloid", "Tabloid"),
        ("ledger", "Ledger"),
        ("statement", "Statement"),
        ("a3", "A3"),
        ("a4", "A4"),
        ("a5", "A5"),
        ("a6", "A6"),
        ("b4", "B4"),
        ("b5", "B5"),
        ("env10", "Envelope #10"),
        ("envdl", "Envelope DL"),
        ("envc5", "Envelope C5"),
    ];
    let base = lower.split(['.', '_']).find(|p| !p.is_empty() && *p != "na" && *p != "iso").unwrap_or(&lower);
    let name = known.iter().find(|(k, _)| *k == base).map(|(_, v)| v.to_string()).unwrap_or_else(|| id.replace('_', " "));
    match paper_mm(id) {
        Some((w, h)) if !known.iter().any(|(k, _)| *k == base) => format!("{name} ({w:.0} × {h:.0} mm)"),
        _ => name,
    }
}

// ----------------------------------------------------------------------
// The dialog
// ----------------------------------------------------------------------

pub enum State {
    Closed,
    Options,
    /// Being sent to this printer.
    Sending(String),
    /// Sent: what happened.
    Done(Result<String, String>),
}

/// What the dialog needs to know about the document.
pub struct DocView<'a> {
    pub title: &'a str,
    /// Displayed size (points) of each page, by position.
    pub sizes: &'a [Vec2],
    /// The page being read (position).
    pub current: usize,
    /// The rendered page at a position, if there is one.
    pub texture: &'a dyn Fn(usize) -> Option<TextureId>,
}

/// What the user asked for.
pub enum Action {
    Print(Job),
}

pub struct Print {
    pub state: State,
    settings: Settings,
    printers: Option<Result<Vec<Printer>, String>>,
    loading: Option<Receiver<Result<Vec<Printer>, String>>>,
    /// The sheet shown in the preview.
    sheet: usize,
    pages_error: Option<String>,
}

impl Default for Print {
    fn default() -> Self {
        Print {
            state: State::Closed,
            settings: Settings::default(),
            printers: None,
            loading: None,
            sheet: 0,
            pages_error: None,
        }
    }
}

impl Print {
    pub fn open(&mut self, ctx: &egui::Context) {
        if matches!(self.state, State::Sending(_)) {
            return;
        }
        self.state = State::Options;
        self.sheet = 0;
        self.pages_error = None;
        if self.loading.is_none() {
            self.refresh(ctx);
        }
    }

    /// Looks for printers again (in the background).
    fn refresh(&mut self, ctx: &egui::Context) {
        let (tx, rx) = std::sync::mpsc::channel();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let _ = tx.send(list_printers());
            ctx.request_repaint();
        });
        self.loading = Some(rx);
    }

    fn poll_printers(&mut self) {
        let Some(rx) = &self.loading else { return };
        let Ok(result) = rx.try_recv() else { return };
        self.loading = None;
        if let Ok(list) = &result {
            // Keep the chosen printer if it is still there, else the default.
            let keep = self.settings.printer.as_ref().is_some_and(|n| list.iter().any(|p| &p.name == n));
            if !keep {
                self.settings.printer =
                    list.iter().find(|p| p.is_default).or(list.first()).map(|p| p.name.clone());
                self.settings.paper = None;
            }
        }
        self.printers = Some(result);
    }

    fn printer(&self) -> Option<&Printer> {
        let list = self.printers.as_ref()?.as_ref().ok()?;
        let name = self.settings.printer.as_ref()?;
        list.iter().find(|p| &p.name == name)
    }

    /// The paper the sheets are (the chosen one, or the printer's), in mm.
    fn paper_size(&self) -> (f32, f32) {
        let id = self.settings.paper.clone().or_else(|| self.printer().and_then(|p| p.default_paper.clone()));
        let from_printer = id.as_ref().and_then(|id| {
            self.printer().and_then(|p| p.papers.iter().find(|x| &x.id == id)).and_then(|x| x.size_mm)
        });
        from_printer.or_else(|| id.as_deref().and_then(paper_mm)).unwrap_or(default_paper_mm())
    }

    pub fn show(&mut self, ctx: &egui::Context, doc: &DocView) -> Option<Action> {
        if matches!(self.state, State::Closed) {
            return None;
        }
        self.poll_printers();
        let mut action = None;
        let mut close = false;
        // In a narrow window the preview is left out.
        let room = ctx.content_rect().width() - 60.0;
        let with_preview = room >= 760.0;
        let modal = egui::Modal::new(egui::Id::new("print")).show(ctx, |ui| {
            ui.set_width(if with_preview { 760.0 } else { room.clamp(300.0, 430.0) });
            ui.heading("Print");
            ui.add_space(8.0);
            match &self.state {
                State::Closed => {}
                State::Sending(to) => {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label(format!("Sending to {to}…"));
                    });
                }
                State::Done(result) => {
                    match result {
                        Ok(message) => ui.label(message.as_str()),
                        Err(e) => ui.colored_label(crate::theme::palette(ui).danger, format!("Could not print: {e}")),
                    };
                    ui.add_space(12.0);
                    if crate::theme::primary_button(ui, "Close").clicked() {
                        close = true;
                    }
                }
                State::Options => {
                    let count = doc.sizes.len();
                    let selection = selected_positions(&self.settings, count, doc.current);
                    ui.horizontal_top(|ui| {
                        ui.vertical(|ui| {
                            ui.set_width(if with_preview { 400.0 } else { ui.available_width() });
                            self.options(ui, doc, count);
                        });
                        if with_preview {
                            ui.add_space(16.0);
                            ui.vertical(|ui| {
                                ui.set_width(330.0);
                                self.preview(ui, doc, selection.as_deref().unwrap_or(&[]));
                            });
                        }
                    });
                    ui.add_space(10.0);
                    ui.separator();
                    ui.add_space(4.0);
                    let printer = self.printer().cloned();
                    ui.horizontal(|ui| {
                        match &selection {
                            Ok(positions) => {
                                ui.label(self.summary(positions.len()));
                            }
                            Err(e) => {
                                ui.colored_label(crate::theme::palette(ui).danger, e.as_str());
                            }
                        }
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            let ready = printer.is_some() && selection.is_ok();
                            if ui.add_enabled(ready, primary("Print")).clicked()
                                && let (Some(printer), Ok(positions)) = (printer, selection.clone())
                            {
                                action = Some(Action::Print(Job {
                                    printer,
                                    positions,
                                    settings: self.settings.clone(),
                                    title: doc.title.to_string(),
                                }));
                            }
                            if ui.button("Cancel").clicked() {
                                close = true;
                            }
                        });
                    });
                }
            }
        });
        let sending = matches!(self.state, State::Sending(_));
        if (close || modal.should_close()) && !sending {
            self.state = State::Closed;
        }
        if let Some(Action::Print(job)) = &action {
            self.state = State::Sending(job.printer.label.clone());
        }
        action
    }

    /// "12 pages on 3 sheets, 2 copies".
    fn summary(&self, pages: usize) -> String {
        let s = &self.settings;
        let sides = pages.div_ceil(s.per_sheet as usize);
        let sheets = if s.sides == Sides::One { sides } else { sides.div_ceil(2) };
        let plural = |n: usize, one: &str, many: &str| if n == 1 { format!("1 {one}") } else { format!("{n} {many}") };
        let mut out = format!("{} on {}", plural(pages, "page", "pages"), plural(sheets, "sheet", "sheets"));
        if s.copies > 1 {
            out += &format!(" × {} copies", s.copies);
        }
        out
    }

    fn options(&mut self, ui: &mut egui::Ui, doc: &DocView, count: usize) {
        let s = &mut self.settings;
        let printers = self.printers.as_ref();
        let loading = self.loading.is_some();
        let mut refresh = false;
        egui::Grid::new("print-options").num_columns(2).spacing([12.0, 10.0]).show(ui, |ui| {
            // Printer.
            ui.label("Printer");
            ui.horizontal(|ui| {
                match printers {
                    _ if loading => {
                        ui.spinner();
                        ui.label("Looking for printers…");
                    }
                    Some(Ok(list)) if !list.is_empty() => {
                        let shown = list.iter().find(|p| Some(&p.name) == s.printer.as_ref());
                        let text = shown.map(printer_text).unwrap_or_default();
                        egui::ComboBox::from_id_salt("printer").width(250.0).selected_text(text).show_ui(ui, |ui| {
                            for p in list {
                                if ui.selectable_label(s.printer.as_ref() == Some(&p.name), printer_text(p)).clicked() {
                                    s.printer = Some(p.name.clone());
                                    s.paper = None;
                                }
                            }
                        });
                    }
                    Some(Ok(_)) => {
                        ui.label("No printers found");
                    }
                    Some(Err(e)) => {
                        ui.colored_label(Color32::from_rgb(220, 80, 80), e.as_str());
                    }
                    None => {}
                }
                if !loading && ui.small_button("⟳").on_hover_text("Look for printers again").clicked() {
                    refresh = true;
                }
            });
            ui.end_row();

            let printer = printers.and_then(|p| p.as_ref().ok()).and_then(|list| {
                list.iter().find(|p| Some(&p.name) == s.printer.as_ref())
            });

            // Copies.
            ui.label("Copies");
            ui.horizontal(|ui| {
                ui.add(egui::DragValue::new(&mut s.copies).range(1..=999).speed(0.1));
                ui.add_enabled(s.copies > 1, egui::Checkbox::new(&mut s.collate, "Collate"))
                    .on_hover_text("Print whole copies one after another (1 2 3, 1 2 3) rather than each page's copies together (1 1, 2 2, 3 3)");
            });
            ui.end_row();

            // Pages.
            ui.label("Pages");
            ui.vertical(|ui| {
                ui.horizontal(|ui| {
                    ui.radio_value(&mut s.range, Range::All, format!("All ({count})"));
                    ui.radio_value(&mut s.range, Range::Current, format!("Current (page {})", doc.current + 1));
                });
                ui.horizontal(|ui| {
                    ui.radio_value(&mut s.range, Range::Listed, "Pages");
                    let field = ui.add(egui::TextEdit::singleline(&mut s.pages_text).hint_text("e.g. 1-3, 5").desired_width(120.0));
                    if field.gained_focus() || field.changed() {
                        s.range = Range::Listed;
                    }
                });
                ui.horizontal(|ui| {
                    egui::ComboBox::from_id_salt("subset")
                        .selected_text(match s.subset {
                            Subset::All => "All of them",
                            Subset::Odd => "Odd pages only",
                            Subset::Even => "Even pages only",
                        })
                        .show_ui(ui, |ui| {
                            ui.selectable_value(&mut s.subset, Subset::All, "All of them");
                            ui.selectable_value(&mut s.subset, Subset::Odd, "Odd pages only");
                            ui.selectable_value(&mut s.subset, Subset::Even, "Even pages only");
                        });
                    ui.checkbox(&mut s.reverse, "Reverse order");
                });
            });
            ui.end_row();

            // Two-sided.
            ui.label("Two-sided");
            let duplex = printer.is_some_and(|p| p.duplex);
            if !duplex {
                s.sides = Sides::One;
            }
            ui.add_enabled_ui(duplex, |ui| {
                egui::ComboBox::from_id_salt("sides")
                    .width(200.0)
                    .selected_text(sides_text(s.sides))
                    .show_ui(ui, |ui| {
                        for v in [Sides::One, Sides::LongEdge, Sides::ShortEdge] {
                            ui.selectable_value(&mut s.sides, v, sides_text(v));
                        }
                    })
                    .response
                    .on_disabled_hover_text("This printer can't print on both sides");
            });
            ui.end_row();

            // Orientation.
            ui.label("Orientation");
            egui::ComboBox::from_id_salt("orientation")
                .width(200.0)
                .selected_text(orientation_text(s.orientation))
                .show_ui(ui, |ui| {
                    for v in [Orientation::Auto, Orientation::Portrait, Orientation::Landscape] {
                        ui.selectable_value(&mut s.orientation, v, orientation_text(v));
                    }
                });
            ui.end_row();

            // Pages per sheet.
            ui.label("Pages per sheet");
            egui::ComboBox::from_id_salt("per-sheet").width(200.0).selected_text(s.per_sheet.to_string()).show_ui(ui, |ui| {
                for n in PER_SHEET {
                    ui.selectable_value(&mut s.per_sheet, n, n.to_string());
                }
            });
            ui.end_row();

            // Scale.
            ui.label("Scale");
            ui.add_enabled_ui(s.per_sheet == 1, |ui| {
                egui::ComboBox::from_id_salt("scaling")
                    .width(200.0)
                    .selected_text(scaling_text(s.scaling))
                    .show_ui(ui, |ui| {
                        for v in [Scaling::Fit, Scaling::Actual, Scaling::Shrink] {
                            ui.selectable_value(&mut s.scaling, v, scaling_text(v));
                        }
                    })
                    .response
                    .on_disabled_hover_text("With several pages per sheet, each page is fitted to its place");
            });
            ui.end_row();

            // Paper.
            ui.label("Paper");
            let default_label = printer
                .and_then(|p| p.default_paper.as_ref().map(|d| (p, d)))
                .map(|(p, d)| {
                    let label = p.papers.iter().find(|x| &x.id == d).map_or_else(|| paper_label(d), |x| x.label.clone());
                    format!("Printer default ({label})")
                })
                .unwrap_or_else(|| "Printer default".into());
            let chosen = s.paper.as_ref().and_then(|id| {
                printer.and_then(|p| p.papers.iter().find(|x| &x.id == id)).map(|x| x.label.clone())
            });
            egui::ComboBox::from_id_salt("paper")
                .width(250.0)
                .selected_text(chosen.unwrap_or_else(|| default_label.clone()))
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut s.paper, None, default_label);
                    for paper in printer.map(|p| p.papers.as_slice()).unwrap_or_default() {
                        ui.selectable_value(&mut s.paper, Some(paper.id.clone()), &paper.label);
                    }
                });
            ui.end_row();

            // Color.
            ui.label("Color");
            let color = printer.is_none_or(|p| p.color);
            if !color {
                s.gray = true;
            }
            ui.add_enabled_ui(color, |ui| {
                ui.horizontal(|ui| {
                    ui.radio_value(&mut s.gray, false, "Color");
                    ui.radio_value(&mut s.gray, true, "Black & white");
                })
                .response
                .on_disabled_hover_text("This printer prints in black and white only");
            });
            ui.end_row();

            // Comments.
            ui.label("Comments");
            ui.checkbox(&mut s.comments, "Print highlights and notes")
                .on_hover_text("Form field values are always printed; links never show");
            ui.end_row();
        });
        if refresh {
            self.refresh(ui.ctx());
        }
    }

    /// The preview: one sheet as it will come out, with arrows to go
    /// through the sheets.
    fn preview(&mut self, ui: &mut egui::Ui, doc: &DocView, positions: &[usize]) {
        let p = crate::theme::palette(ui);
        let s = &self.settings;
        let per_sheet = s.per_sheet as usize;
        let sheets = positions.len().div_ceil(per_sheet).max(1);
        self.sheet = self.sheet.min(sheets - 1);
        let first = positions.first().map(|&i| doc.sizes[i]).unwrap_or(Vec2::new(612.0, 792.0));
        let landscape = sheet_landscape(s, first);
        let (mut w, mut h) = self.paper_size();
        if landscape {
            std::mem::swap(&mut w, &mut h);
        }
        // The sheet, as large as fits the preview area.
        let area = Vec2::new(330.0, 360.0);
        let (rect, _) = ui.allocate_exact_size(area, Sense::hover());
        let scale = ((area.x - 20.0) / w).min((area.y - 20.0) / h);
        let sheet = Rect::from_center_size(rect.center(), Vec2::new(w, h) * scale);
        let painter = ui.painter_at(rect);
        painter.add(egui::epaint::Shadow { offset: [0, 3], blur: 12, spread: 0, color: p.shadow }.as_shape(sheet, 1));
        painter.rect_filled(sheet, 0.0, Color32::WHITE);
        // A small margin, as printers can't print to the edge.
        let printable = sheet.shrink(4.0 * scale);
        let per_point = scale * 25.4 / 72.0;
        let start = self.sheet * per_sheet;
        for (cell, &pos) in cells(printable, s.per_sheet, landscape).into_iter().zip(positions.iter().skip(start)) {
            let size = doc.sizes[pos];
            let page = place(cell.shrink(if per_sheet > 1 { 2.0 } else { 0.0 }), size, s.scaling, s.per_sheet, per_point);
            let shown = page.intersect(printable);
            let painter = ui.painter_at(shown);
            match (doc.texture)(pos) {
                Some(texture) => {
                    let uv = Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0));
                    painter.image(texture, page, uv, if s.gray { Color32::from_gray(235) } else { Color32::WHITE });
                }
                None => {
                    painter.rect_filled(page, 0.0, Color32::from_gray(248));
                    painter.text(page.center(), egui::Align2::CENTER_CENTER, (pos + 1).to_string(), egui::FontId::proportional(14.0), Color32::from_gray(150));
                }
            }
            if per_sheet > 1 {
                painter.rect_stroke(page, 0.0, Stroke::new(0.5, Color32::from_gray(200)), StrokeKind::Inside);
            }
        }
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            if ui.add_enabled(self.sheet > 0, egui::Button::new("◀")).clicked() {
                self.sheet -= 1;
            }
            ui.label(format!("Sheet {} of {sheets}", self.sheet + 1));
            if ui.add_enabled(self.sheet + 1 < sheets, egui::Button::new("▶")).clicked() {
                self.sheet += 1;
            }
            let mut notes = Vec::new();
            if s.gray {
                notes.push("black & white");
            }
            if s.sides != Sides::One {
                notes.push("two-sided");
            }
            if !notes.is_empty() {
                ui.label(RichText::new(notes.join(", ")).color(p.weak).size(12.0));
            }
        });
    }
}

fn primary(text: &str) -> egui::Button<'static> {
    egui::Button::new(RichText::new(text.to_string()).color(Color32::WHITE)).fill(Color32::from_rgb(45, 108, 240))
}

fn printer_text(p: &Printer) -> String {
    if p.is_default { format!("{} (default)", p.label) } else { p.label.clone() }
}

fn sides_text(s: Sides) -> &'static str {
    match s {
        Sides::One => "Off",
        Sides::LongEdge => "Flip on long edge",
        Sides::ShortEdge => "Flip on short edge",
    }
}

fn orientation_text(o: Orientation) -> &'static str {
    match o {
        Orientation::Auto => "Automatic",
        Orientation::Portrait => "Portrait",
        Orientation::Landscape => "Landscape",
    }
}

fn scaling_text(s: Scaling) -> &'static str {
    match s {
        Scaling::Fit => "Fit to paper",
        Scaling::Actual => "Actual size",
        Scaling::Shrink => "Shrink large pages",
    }
}

/// Letter in the US and Canada (by the locale), A4 elsewhere.
pub fn default_paper_mm() -> (f32, f32) {
    let locale = ["LC_ALL", "LC_PAPER", "LANG"].iter().find_map(|v| std::env::var(v).ok().filter(|s| !s.is_empty()));
    match locale {
        Some(l) if l.contains("_US") || l.contains("_CA") => (215.9, 279.4),
        _ => (210.0, 297.0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn page_selection() {
        let mut s = Settings::default();
        assert_eq!(selected_positions(&s, 5, 2).unwrap(), [0, 1, 2, 3, 4]);
        s.subset = Subset::Even;
        s.reverse = true;
        assert_eq!(selected_positions(&s, 5, 2).unwrap(), [3, 1]);
        s.range = Range::Listed;
        s.pages_text = "2-4".into();
        s.subset = Subset::Odd;
        s.reverse = false;
        assert_eq!(selected_positions(&s, 5, 2).unwrap(), [2]);
        s.range = Range::Current;
        s.subset = Subset::Even;
        assert!(selected_positions(&s, 5, 2).is_err());
    }

    #[test]
    fn papers_and_layout() {
        assert_eq!(paper_mm("A4"), Some((210.0, 297.0)));
        assert_eq!(paper_mm("na_letter_8.5x11in"), Some((215.9, 279.4)));
        assert_eq!(paper_mm("iso_a5_148x210mm"), Some((148.0, 210.0)));
        assert_eq!(paper_label("na_letter_8.5x11in"), "Letter");
        let s = Settings { per_sheet: 2, ..Settings::default() };
        assert!(sheet_landscape(&s, Vec2::new(612.0, 792.0)));
        let sheet = Rect::from_min_size(Pos2::ZERO, Vec2::new(300.0, 200.0));
        let c = cells(sheet, 2, true);
        assert_eq!(c.len(), 2);
        assert_eq!(c[1].min, Pos2::new(150.0, 0.0));
        assert_eq!(cells(sheet, 6, true).len(), 6);
    }
}
