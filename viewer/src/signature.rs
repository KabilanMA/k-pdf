//! Signing: making a signature, and placing it.
//!
//! 1. The Sign button opens a dialog with three ways to make one (with a
//!    signature made earlier in this session, that is offered first, so a
//!    document can be signed in several places):
//!    - **Draw**: on a white sheet, with the mouse, a touchpad or a pen
//!      (`k_pdf::scan::drawn_signature` smooths the strokes);
//!    - **Type**: a name, written in one of the handwriting fonts installed
//!      (`k_pdf::scan::typed_signature`), each shown with the name;
//!    - **Photo or scan**: an image, or a page of a PDF; a box dragged
//!      around the signature is lifted off it (`k_pdf::scan::lift_signature`:
//!      the paper becomes transparent, specks go, the ink gets one color),
//!      with a preview on a checkerboard.
//! 2. The signature then sits on the page, where it can be dragged
//!    (also onto another page) and resized by its corners; "Place" draws
//!    it into the page (`k_pdf::edit::place_image`), which Ctrl+Z undoes.

use std::path::{Path, PathBuf};
use std::sync::mpsc;

use eframe::egui::{self, Color32, CursorIcon, Id, Pos2, Rect, Sense, Stroke, StrokeKind, TextureHandle, Vec2};
use k_pdf::scan::{Image, Ink, Signature};

/// What the user asked for.
pub enum Action {
    /// Pick a photo or scan to take a signature from.
    PickFile,
    /// Draw the signature on `page` (of the open file) over `rect`
    /// (display points).
    Place { page: usize, rect: [f32; 4], signature: Signature },
}

/// A signature, with its texture: white, with the signature's opacity, so
/// it can be tinted for dark pages.
#[derive(Clone)]
struct Lifted {
    signature: Signature,
    texture: TextureHandle,
}

impl Lifted {
    fn new(ctx: &egui::Context, signature: Signature) -> Lifted {
        let white: Vec<u8> = signature.rgba.chunks_exact(4).flat_map(|p| [255, 255, 255, p[3]]).collect();
        let size = [signature.width as usize, signature.height as usize];
        let texture = ctx.load_texture("signature", egui::ColorImage::from_rgba_unmultiplied(size, &white), egui::TextureOptions::LINEAR);
        Lifted { signature, texture }
    }

    fn color(&self) -> Color32 {
        let p = &self.signature.rgba;
        Color32::from_rgb(p[0], p[1], p[2])
    }

    fn aspect(&self) -> f32 {
        self.signature.width as f32 / self.signature.height.max(1) as f32
    }

    /// Draws it over `rect`, on a page (dark pages recolor it).
    fn paint(&self, painter: &egui::Painter, rect: Rect) {
        let uv = Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0));
        painter.image(self.texture.id(), rect, uv, crate::theme::on_page(self.color()));
    }
}

/// The picture a signature is taken from.
struct Source {
    path: PathBuf,
    image: Image,
    texture: TextureHandle,
    /// For a PDF: the page shown and how many there are.
    page: Option<(usize, usize)>,
}

/// A picture that was read, with the page shown and the page count for
/// a PDF; or why it couldn't be.
type Read = Result<(Image, Option<(usize, usize)>), String>;

/// A picture being read, on a thread of its own.
struct Loading {
    name: String,
    reply: mpsc::Receiver<Read>,
    path: PathBuf,
}

/// The ways a signature can be made.
#[derive(Clone, Copy, PartialEq, Eq, Default)]
enum Mode {
    #[default]
    Draw,
    Type,
    Photo,
}

/// How thick the pen draws, in sheet units (the sheet is
/// [`SHEET_WIDTH`] wide).
#[derive(Clone, Copy, PartialEq, Eq, Default)]
enum Pen {
    Fine,
    #[default]
    Medium,
    Bold,
}

impl Pen {
    const ALL: [Pen; 3] = [Pen::Fine, Pen::Medium, Pen::Bold];

    fn width(self) -> f32 {
        match self {
            Pen::Fine => 3.5,
            Pen::Medium => 5.5,
            Pen::Bold => 8.5,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Pen::Fine => "Fine",
            Pen::Medium => "Medium",
            Pen::Bold => "Bold",
        }
    }
}

/// A handwriting font typed signatures can use.
struct Font {
    name: String,
    data: Vec<u8>,
    index: u32,
}

/// The handwriting fonts, found on a thread the first time the dialog
/// opens (so the dialog doesn't wait for the font list).
#[derive(Default)]
enum Fonts {
    #[default]
    Unknown,
    Finding(mpsc::Receiver<Vec<Font>>),
    Found(std::sync::Arc<Vec<Font>>),
}

/// The dialog for choosing the signature.
#[derive(Default)]
struct Chooser {
    mode: Mode,
    /// Showing the signature made earlier (to use again), not the ways
    /// to make one.
    reuse: bool,
    /// The color of drawn and typed signatures.
    pen_ink: Ink,
    // Drawing: the strokes, in sheet units (x from 0 to SHEET_WIDTH).
    strokes: Vec<Vec<Pos2>>,
    pen: Pen,
    // Typing: the name, the font chosen, and the name in each font (for
    // the name and color in `typed_for`).
    name: String,
    font: usize,
    typed: Vec<Option<Lifted>>,
    typed_for: Option<(String, Ink, usize)>,
    focus_name: bool,
    // A photo or scan.
    source: Option<Source>,
    loading: Option<Loading>,
    /// The box around the signature, in the picture's pixels.
    area: Option<Rect>,
    /// A box being drawn (its first corner) or moved (the offset from the
    /// pointer to its top left), in pixels.
    drag: Option<(bool, Pos2)>,
    ink: Ink,
    preview: Option<Lifted>,
    /// "Choose a photo or scan…" was clicked.
    pick: bool,
    message: Option<String>,
}

impl Chooser {
    fn new(mode: Mode) -> Chooser {
        Chooser { mode, pen_ink: Ink::Black, focus_name: mode == Mode::Type, ..Chooser::default() }
    }
}

/// The signature on a page, not yet placed.
struct Placing {
    lifted: Lifted,
    /// The open file's page it is on (`None` until first shown).
    page: Option<usize>,
    /// Where, in display points of the page.
    rect: Rect,
    grab: Option<Grab>,
}

#[derive(Clone, Copy)]
enum Grab {
    /// Moving: the pointer's offset from the top left corner.
    Move(Vec2),
    /// Resizing by a corner: the opposite corner stays.
    Corner(Pos2),
}

#[derive(Default)]
enum Stage {
    #[default]
    Off,
    Choosing(Box<Chooser>),
    Placing(Box<Placing>),
}

#[derive(Default)]
pub struct Signing {
    stage: Stage,
    /// The last signature made, to sign again with.
    last: Option<Lifted>,
    /// How it was made (the dialog opens there next time).
    mode: Mode,
    fonts: Fonts,
}

/// Pictures are shown in the dialog at most this many pixels on a side.
const SHOWN_SIDE: u32 = 2048;
/// Boxes smaller than this (pixels of the picture) are taken as clicks.
const MIN_BOX: f32 = 8.0;
/// The size of a corner handle (screen points).
const HANDLE: f32 = 9.0;
/// The drawing sheet's width, in the units strokes are kept in.
const SHEET_WIDTH: f32 = 1000.0;
/// Typed signatures are made this many pixels to the em (about 1000
/// pixels for a name: sharp when printed a few inches wide).
const TYPED_EM: f32 = 160.0;

impl Signing {
    pub fn is_active(&self) -> bool {
        !matches!(self.stage, Stage::Off)
    }

    pub fn is_placing(&self) -> bool {
        matches!(self.stage, Stage::Placing(_))
    }

    /// The Sign button: opens the dialog, on the last signature if there
    /// is one, else on the way the last one was made.
    pub fn begin(&mut self) {
        let mut chooser = Chooser::new(self.mode);
        chooser.reuse = self.last.is_some();
        chooser.focus_name &= !chooser.reuse;
        self.stage = Stage::Choosing(Box::new(chooser));
        if matches!(self.fonts, Fonts::Unknown) {
            self.fonts = Fonts::Finding(find_fonts());
        }
    }

    pub fn cancel(&mut self) {
        self.stage = Stage::Off;
    }

    /// After the signature was placed.
    pub fn placed(&mut self) {
        self.stage = Stage::Off;
    }

    /// Reads `path` (an image, or a PDF's first page) to take a signature
    /// from.
    pub fn open_file(&mut self, ctx: &egui::Context, path: &Path) {
        if !matches!(self.stage, Stage::Choosing(_)) {
            self.begin();
        }
        let Stage::Choosing(c) = &mut self.stage else { return };
        c.mode = Mode::Photo;
        c.reuse = false;
        c.source = None;
        c.area = None;
        c.preview = None;
        c.message = None;
        c.loading = Some(load(ctx, path, 0));
    }

    /// Picks up a picture that has been read.
    fn poll(&mut self, ctx: &egui::Context) {
        let Stage::Choosing(c) = &mut self.stage else { return };
        let Some(loading) = &c.loading else { return };
        let reply = match loading.reply.try_recv() {
            Ok(reply) => reply,
            Err(mpsc::TryRecvError::Empty) => return,
            Err(mpsc::TryRecvError::Disconnected) => Err("reading it failed".into()),
        };
        let loading = c.loading.take().expect("loading");
        match reply {
            Ok((image, page)) => {
                let (shown, _) = image.shrink(SHOWN_SIDE);
                let rgb = shown.to_rgb();
                let texture = ctx.load_texture(
                    "signature-source",
                    egui::ColorImage::from_rgb([rgb.width as usize, rgb.height as usize], &rgb.data),
                    egui::TextureOptions::LINEAR,
                );
                // A picture of just the signature needs no box: the whole
                // of it is taken to begin with.
                c.area = Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(image.width as f32, image.height as f32)));
                c.source = Some(Source { path: loading.path, image, texture, page });
                c.message = None;
                c.update_preview(ctx);
            }
            Err(e) => c.message = Some(format!("Could not open {}: {e}", loading.name)),
        }
    }

    /// The dialog for choosing the signature, while it is open.
    pub fn dialog(&mut self, ctx: &egui::Context, view: Rect) -> Option<Action> {
        self.poll(ctx);
        if let Fonts::Finding(rx) = &self.fonts {
            match rx.try_recv() {
                Ok(fonts) => self.fonts = Fonts::Found(std::sync::Arc::new(fonts)),
                Err(mpsc::TryRecvError::Disconnected) => self.fonts = Fonts::Found(Default::default()),
                Err(mpsc::TryRecvError::Empty) => {}
            }
        }
        let fonts = match &self.fonts {
            Fonts::Found(f) => Some(f.clone()),
            _ => None,
        };
        let Stage::Choosing(c) = &mut self.stage else { return None };
        if c.loading.is_some() || (c.mode == Mode::Type && fonts.is_none()) {
            ctx.request_repaint_after(std::time::Duration::from_millis(50));
        }
        let mut action = None;
        let mut close = false;
        let mut use_it: Option<Lifted> = None;
        let last = self.last.clone();
        let modal = egui::Modal::new(Id::new("signature")).show(ctx, |ui| {
            let width = (view.width() - 80.0).clamp(320.0, 820.0);
            ui.set_width(width);
            ui.heading("Add a signature");
            ui.add_space(4.0);
            if c.reuse
                && let Some(last) = &last
            {
                ui.label("Your signature:");
                ui.add_space(4.0);
                preview(ui, Some(last), Vec2::new(width.min(420.0), 96.0));
                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    if crate::theme::primary_button(ui, "Use this signature").clicked() {
                        use_it = Some(last.clone());
                    }
                    if ui.button("New signature").on_hover_text("Draw it, type it or take it from a photo or scan").clicked() {
                        c.reuse = false;
                        c.focus_name = c.mode == Mode::Type;
                    }
                    if ui.button("Cancel").clicked() {
                        close = true;
                    }
                });
                return;
            }

            ui.horizontal(|ui| {
                for (mode, label, tip) in [
                    (Mode::Draw, "Draw", "Sign with the mouse, a touchpad or a pen"),
                    (Mode::Type, "Type", "Type your name in a handwriting font"),
                    (Mode::Photo, "Photo or scan", "Take your signature from a photo or scan of it on paper"),
                ] {
                    if ui.selectable_label(c.mode == mode, label).on_hover_text(tip).clicked() && c.mode != mode {
                        c.mode = mode;
                        c.message = None;
                        c.focus_name = mode == Mode::Type;
                    }
                }
            });
            ui.separator();
            let ready = match c.mode {
                Mode::Draw => c.draw(ui, Vec2::new(width, (width * 0.34).clamp(140.0, 280.0).min(view.height() - 300.0).max(120.0))),
                Mode::Type => c.type_name(ctx, ui, width, fonts.as_deref()),
                Mode::Photo => c.photo(ctx, ui, view, width),
            };
            if let Some(m) = &c.message {
                ui.add_space(4.0);
                ui.colored_label(crate::theme::palette(ui).danger, m.as_str());
            }
            ui.add_space(10.0);
            let mut chosen = false;
            ui.horizontal(|ui| {
                let p = crate::theme::palette(ui);
                let button = ui.add_enabled(ready, egui::Button::new(egui::RichText::new("Use signature").color(p.on_accent)).fill(p.accent));
                if button.on_hover_text("Put it on the page, then drag it where it goes").clicked() {
                    chosen = true;
                }
                if c.mode == Mode::Photo
                    && c.source.is_some()
                    && ui.button("Other file…").on_hover_text("Take the signature from another photo or scan").clicked()
                {
                    action = Some(Action::PickFile);
                }
                if ui.button("Cancel").clicked() {
                    close = true;
                }
            });
            // Enter in the name field uses it.
            chosen |= ready && c.mode == Mode::Type && ui.input(|i| i.key_pressed(egui::Key::Enter));
            if chosen {
                use_it = c.make(ctx, fonts.as_deref());
                if use_it.is_none() {
                    c.message = Some("Could not make the signature: there is nothing to put on the page.".into());
                }
            }
        });
        if std::mem::take(&mut c.pick) {
            action = Some(Action::PickFile);
        }
        if modal.should_close() || close {
            self.stage = Stage::Off;
        }
        if let Some(lifted) = use_it {
            // The dialog's button would keep the keyboard (Enter places).
            ctx.memory_mut(|m| {
                if let Some(id) = m.focused() {
                    m.surrender_focus(id);
                }
            });
            if let Stage::Choosing(c) = &self.stage
                && !c.reuse
            {
                self.mode = c.mode;
            }
            self.last = Some(lifted.clone());
            self.stage = Stage::Placing(Box::new(Placing { lifted, page: None, rect: Rect::NOTHING, grab: None }));
        }
        action
    }

    /// Shows the signature being placed and handles dragging it, over the
    /// visible pages of the open file (`(page, its rectangle on screen)`).
    /// `current` is the page it goes on to begin with.
    pub fn show_pages(&mut self, ui: &mut egui::Ui, pages: &[(usize, Rect)], zoom: f32, current: Option<usize>) {
        let Stage::Placing(p) = &mut self.stage else { return };
        if pages.is_empty() {
            return;
        }
        // To begin with: in the middle of what shows of the current page,
        // about two inches wide.
        if p.page.is_none() {
            let (page, rect) = current.and_then(|c| pages.iter().find(|(page, _)| *page == c)).unwrap_or(&pages[0]);
            let size = rect.size() / zoom;
            let mut w = 150.0f32.min(size.x * 0.4);
            if w / p.lifted.aspect() > 60.0 {
                w = 60.0 * p.lifted.aspect();
            }
            let shown = rect.intersect(ui.clip_rect());
            let middle = if shown.is_positive() { shown.center() } else { rect.center() };
            let middle = ((middle - rect.min) / zoom).to_pos2();
            p.page = Some(*page);
            p.rect = Rect::from_center_size(middle, Vec2::new(w, w / p.lifted.aspect()));
        }
        let response = ui.interact(ui.clip_rect(), ui.id().with("place-signature"), Sense::drag());
        let pointer = response.interact_pointer_pos().or(response.hover_pos());
        let Some(&(_, page_rect)) = pages.iter().find(|(page, _)| Some(*page) == p.page) else { return };
        let to_screen = |r: Rect, page_rect: Rect| Rect::from_min_max(page_rect.min + r.min.to_vec2() * zoom, page_rect.min + r.max.to_vec2() * zoom);
        let screen = to_screen(p.rect, page_rect);
        let corners = [screen.left_top(), screen.right_top(), screen.right_bottom(), screen.left_bottom()];
        let corner_at = |pos: Pos2| corners.iter().position(|c| (*c - pos).length() <= HANDLE);

        // Hovering: the cursor says what dragging would do.
        if p.grab.is_none()
            && let Some(pos) = response.hover_pos()
        {
            match corner_at(pos) {
                Some(0 | 2) => ui.ctx().set_cursor_icon(CursorIcon::ResizeNwSe),
                Some(_) => ui.ctx().set_cursor_icon(CursorIcon::ResizeNeSw),
                None if screen.contains(pos) => ui.ctx().set_cursor_icon(CursorIcon::Grab),
                None => {}
            }
        }
        let press = ui.input(|i| i.pointer.press_origin());
        if response.drag_started()
            && let Some(start) = press.or(pointer)
        {
            p.grab = match corner_at(start) {
                Some(i) => {
                    let opposite = [p.rect.right_bottom(), p.rect.left_bottom(), p.rect.left_top(), p.rect.right_top()][i];
                    Some(Grab::Corner(opposite))
                }
                None if screen.contains(start) => Some(Grab::Move(((start - page_rect.min) / zoom) - p.rect.min.to_vec2())),
                None => None,
            };
        }
        if let (Some(grab), Some(pos)) = (p.grab, pointer)
            && response.dragged()
        {
            match grab {
                Grab::Move(offset) => {
                    ui.ctx().set_cursor_icon(CursorIcon::Grabbing);
                    // Onto whichever page the pointer is over.
                    let (page, rect) = pages.iter().find(|(_, r)| r.contains(pos)).copied().unwrap_or((p.page.unwrap_or(0), page_rect));
                    let bounds = rect.size() / zoom;
                    let size = p.rect.size();
                    let min = ((pos - rect.min) / zoom - offset).to_pos2();
                    let min = Pos2::new(min.x.clamp(0.0, (bounds.x - size.x).max(0.0)), min.y.clamp(0.0, (bounds.y - size.y).max(0.0)));
                    p.page = Some(page);
                    p.rect = Rect::from_min_size(min, size);
                }
                Grab::Corner(anchor) => {
                    let at = ((pos - page_rect.min) / zoom).to_pos2();
                    let bounds = page_rect.size() / zoom;
                    let aspect = p.lifted.aspect();
                    // The width the pointer asks for, keeping the shape,
                    // and within the page.
                    let d = at - anchor;
                    let room_x = if d.x < 0.0 { anchor.x } else { bounds.x - anchor.x };
                    let room_y = if d.y < 0.0 { anchor.y } else { bounds.y - anchor.y };
                    let w = d.x.abs().max(d.y.abs() * aspect).min(room_x).min(room_y * aspect).max(12.0);
                    let other = anchor + Vec2::new(w.copysign(d.x), (w / aspect).copysign(d.y));
                    p.rect = Rect::from_two_pos(anchor, other);
                }
            }
        }
        if response.drag_stopped() {
            p.grab = None;
        }

        // Drawn on the page it is on, with a frame and corner handles.
        let Some(&(_, page_rect)) = pages.iter().find(|(page, _)| Some(*page) == p.page) else { return };
        let screen = to_screen(p.rect, page_rect);
        let painter = ui.painter();
        p.lifted.paint(painter, screen);
        let accent = crate::theme::palette(ui).accent;
        painter.rect_stroke(screen.expand(2.0), 2.0, Stroke::new(1.5, accent), StrokeKind::Outside);
        for c in [screen.left_top(), screen.right_top(), screen.right_bottom(), screen.left_bottom()] {
            let handle = Rect::from_center_size(c, Vec2::splat(HANDLE - 1.0));
            painter.rect_filled(handle, 2.0, Color32::WHITE);
            painter.rect_stroke(handle, 2.0, Stroke::new(1.5, accent), StrokeKind::Inside);
        }
    }

    /// The bar shown while placing, at the top of the page view.
    pub fn panel(&mut self, ctx: &egui::Context, view: Rect) -> Option<Action> {
        let Stage::Placing(_) = &self.stage else { return None };
        let mut action = None;
        let mut cancel = false;
        egui::Area::new(Id::new("signature-panel"))
            .order(egui::Order::Foreground)
            .fixed_pos(Pos2::new(view.center().x, view.top() + 8.0))
            .pivot(egui::Align2::CENTER_TOP)
            .show(ctx, |ui| {
                crate::theme::floating_frame(ui).show(ui, |ui| {
                    ui.set_max_width((view.width() - 24.0).max(200.0));
                    ui.horizontal_wrapped(|ui| {
                        ui.label("Drag the signature where it goes; drag a corner to resize it.");
                        if crate::theme::primary_button(ui, "Place").on_hover_text("Put it on the page (Enter)").clicked() {
                            action = self.place();
                        }
                        if ui.button("Cancel").on_hover_text("Esc").clicked() {
                            cancel = true;
                        }
                    });
                });
            });
        if cancel {
            self.stage = Stage::Off;
        }
        action
    }

    /// Places the signature where it is (Enter, or the Place button).
    pub fn place(&self) -> Option<Action> {
        let Stage::Placing(p) = &self.stage else { return None };
        let r = p.rect;
        Some(Action::Place { page: p.page?, rect: [r.left(), r.top(), r.right(), r.bottom()], signature: p.lifted.signature.clone() })
    }
}

impl Chooser {
    /// The drawing sheet, with the pen and ink choices. Returns whether
    /// there is a signature to use.
    fn draw(&mut self, ui: &mut egui::Ui, size: Vec2) -> bool {
        ui.label("Sign on the sheet with the mouse, a touchpad or a pen.");
        ui.add_space(6.0);
        let (response, painter) = ui.allocate_painter(size, Sense::click_and_drag());
        let sheet = response.rect;
        let k = sheet.width() / SHEET_WIDTH;
        let to_sheet = |p: Pos2| ((p.clamp(sheet.min, sheet.max) - sheet.min) / k).to_pos2();
        let to_screen = |p: Pos2| sheet.min + p.to_vec2() * k;
        let ink = ink_color(self.pen_ink);

        // Paper, a line to sign on, and a hint while it is empty.
        painter.rect_filled(sheet, 6.0, Color32::WHITE);
        painter.rect_stroke(sheet, 6.0, Stroke::new(1.0, crate::theme::palette(ui).border), StrokeKind::Inside);
        let line_y = sheet.top() + sheet.height() * 0.72;
        let gray = Color32::from_gray(190);
        painter.line_segment([Pos2::new(sheet.left() + sheet.width() * 0.06, line_y), Pos2::new(sheet.right() - sheet.width() * 0.06, line_y)], Stroke::new(1.0, gray));
        painter.text(Pos2::new(sheet.left() + sheet.width() * 0.06, line_y - 4.0), egui::Align2::LEFT_BOTTOM, "×", egui::FontId::proportional(18.0), gray);
        if self.strokes.is_empty() {
            painter.text(sheet.center() - Vec2::new(0.0, sheet.height() * 0.12), egui::Align2::CENTER_CENTER, "Sign here", egui::FontId::proportional(22.0), Color32::from_gray(170));
        }

        if response.hovered() {
            ui.ctx().set_cursor_icon(CursorIcon::Crosshair);
        }
        if response.drag_started()
            && let Some(start) = ui.input(|i| i.pointer.press_origin()).or(response.interact_pointer_pos())
        {
            self.strokes.push(vec![to_sheet(start)]);
        }
        if response.dragged()
            && let (Some(pos), Some(stroke)) = (response.interact_pointer_pos(), self.strokes.last_mut())
        {
            let p = to_sheet(pos);
            if stroke.last().is_none_or(|l| (*l - p).length() >= 1.5) {
                stroke.push(p);
            }
        }
        if response.clicked()
            && let Some(pos) = response.interact_pointer_pos()
        {
            // A dot (over an i, or a full stop).
            self.strokes.push(vec![to_sheet(pos)]);
        }

        let width = self.pen.width() * k;
        for stroke in &self.strokes {
            let points: Vec<Pos2> = smooth(stroke).into_iter().map(to_screen).collect();
            if points.len() == 1 {
                painter.circle_filled(points[0], width / 2.0, ink);
                continue;
            }
            // Each step on its own, with a dot at each point: round joins
            // (a joined line would have spikes at sharp turns).
            for pair in points.windows(2) {
                painter.line_segment([pair[0], pair[1]], Stroke::new(width, ink));
            }
            for p in &points {
                painter.circle_filled(*p, width / 2.0, ink);
            }
        }

        ui.add_space(6.0);
        ui.horizontal(|ui| {
            ui.label("Pen:");
            for pen in Pen::ALL {
                ui.selectable_value(&mut self.pen, pen, pen.label());
            }
            ui.add_space(12.0);
            self.pen_choice(ui);
            ui.add_space(12.0);
            if ui.add_enabled(!self.strokes.is_empty(), egui::Button::new("Undo stroke")).clicked() {
                self.strokes.pop();
            }
            if ui.add_enabled(!self.strokes.is_empty(), egui::Button::new("Clear")).clicked() {
                self.strokes.clear();
            }
        });
        !self.strokes.is_empty()
    }

    /// The ink choice for drawn and typed signatures.
    fn pen_choice(&mut self, ui: &mut egui::Ui) {
        ui.label("Ink:");
        for ink in [Ink::Black, Ink::Blue] {
            ui.selectable_value(&mut self.pen_ink, ink, ink.label());
        }
    }

    /// The name field, and the name in each handwriting font to choose
    /// from. Returns whether there is a signature to use.
    fn type_name(&mut self, ctx: &egui::Context, ui: &mut egui::Ui, width: f32, fonts: Option<&Vec<Font>>) -> bool {
        ui.horizontal(|ui| {
            ui.label("Your name:");
            let field = ui.add(egui::TextEdit::singleline(&mut self.name).hint_text("Type your name").desired_width((width - 260.0).max(160.0)));
            if std::mem::take(&mut self.focus_name) {
                field.request_focus();
            }
            ui.add_space(12.0);
            self.pen_choice(ui);
        });
        ui.add_space(8.0);
        let Some(fonts) = fonts else {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label("Finding handwriting fonts…");
            });
            return false;
        };
        if fonts.is_empty() {
            ui.label("No fonts were found to write the name in.");
            return false;
        }
        self.font = self.font.min(fonts.len() - 1);

        // The name in each font, made again when it changes.
        let shown = if self.name.trim().is_empty() { "Your Name".to_string() } else { self.name.trim().to_string() };
        let key = (shown.clone(), self.pen_ink, fonts.len());
        if self.typed_for.as_ref() != Some(&key) {
            let em = 40.0 * ctx.pixels_per_point();
            self.typed = fonts
                .iter()
                .map(|f| k_pdf::scan::typed_signature(&shown, &f.data, f.index, em, self.pen_ink).map(|s| Lifted::new(ctx, s)))
                .collect();
            self.typed_for = Some(key);
        }

        // Cards in two columns: the name on white, the font's name under it.
        let gap = 8.0;
        let card = Vec2::new((width - gap) / 2.0, 66.0);
        let accent = crate::theme::palette(ui).accent;
        let border = crate::theme::palette(ui).border;
        egui::ScrollArea::vertical().max_height(4.0 * (card.y + gap)).auto_shrink([false, true]).show(ui, |ui| {
            for row in (0..fonts.len()).step_by(2) {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = gap;
                    for (i, font) in fonts.iter().enumerate().skip(row).take(2) {
                        let (rect, response) = ui.allocate_exact_size(card, Sense::click());
                        let chosen = i == self.font;
                        let painter = ui.painter();
                        painter.rect_filled(rect, 6.0, Color32::WHITE);
                        let outline = if chosen { Stroke::new(2.0, accent) } else { Stroke::new(1.0, if response.hovered() { accent } else { border }) };
                        painter.rect_stroke(rect, 6.0, outline, StrokeKind::Inside);
                        let inner = Rect::from_min_max(rect.min + Vec2::new(10.0, 6.0), Pos2::new(rect.max.x - 10.0, rect.max.y - 18.0));
                        if let Some(Some(l)) = self.typed.get(i) {
                            let size = Vec2::new(l.signature.width as f32, l.signature.height as f32) / ctx.pixels_per_point();
                            let size = size * (inner.width() / size.x).min(inner.height() / size.y).min(1.0);
                            let uv = Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0));
                            painter.image(l.texture.id(), Rect::from_center_size(inner.center(), size), uv, l.color());
                        }
                        painter.text(Pos2::new(rect.left() + 10.0, rect.bottom() - 4.0), egui::Align2::LEFT_BOTTOM, &font.name, egui::FontId::proportional(11.0), Color32::from_gray(120));
                        if response.on_hover_cursor(CursorIcon::PointingHand).clicked() {
                            self.font = i;
                        }
                    }
                });
                ui.add_space(gap - ui.spacing().item_spacing.y);
            }
        });
        !self.name.trim().is_empty() && self.typed.get(self.font).is_some_and(Option::is_some)
    }

    /// Taking the signature from a photo or scan. Returns whether there is
    /// a signature to use.
    fn photo(&mut self, ctx: &egui::Context, ui: &mut egui::Ui, view: Rect, width: f32) -> bool {
        if let Some(loading) = &self.loading {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(format!("Opening {}…", loading.name));
            });
            return false;
        }
        if self.source.is_none() {
            ui.label("Sign a sheet of white paper, then take a photo of it or scan it. Choose the picture (or a PDF of the scan) here.");
            ui.add_space(8.0);
            if crate::theme::primary_button(ui, "Choose a photo or scan…").clicked() {
                self.pick = true;
            }
            return false;
        }
        ui.label("Drag a box around the signature. The paper around it becomes see-through.");
        ui.add_space(6.0);
        let height = (view.height() - 380.0).clamp(160.0, 520.0);
        ui.vertical_centered(|ui| self.picture(ctx, ui, Vec2::new(width, height)));
        let mut turn = None;
        if let Some(Source { page: Some((page, count)), .. }) = &self.source
            && *count > 1
        {
            ui.horizontal(|ui| {
                if ui.add_enabled(*page > 0, egui::Button::new("◀")).on_hover_text("Previous page").clicked() {
                    turn = Some(page - 1);
                }
                ui.label(format!("Page {} of {count}", page + 1));
                if ui.add_enabled(page + 1 < *count, egui::Button::new("▶")).on_hover_text("Next page").clicked() {
                    turn = Some(page + 1);
                }
            });
        }
        if let Some(page) = turn
            && let Some(source) = &self.source
        {
            self.loading = Some(load(ctx, &source.path, page));
        }
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            ui.label("Ink:");
            let before = self.ink;
            for ink in Ink::ALL {
                ui.selectable_value(&mut self.ink, ink, ink.label());
            }
            if self.ink != before {
                self.update_preview(ctx);
            }
        });
        ui.add_space(4.0);
        ui.vertical_centered(|ui| preview(ui, self.preview.as_ref(), Vec2::new(width.min(420.0), 96.0)));
        self.preview.is_some() && self.loading.is_none()
    }

    /// The signature made the chosen way, to put on the page.
    fn make(&self, ctx: &egui::Context, fonts: Option<&Vec<Font>>) -> Option<Lifted> {
        let signature = match self.mode {
            Mode::Draw => {
                let strokes: Vec<Vec<[f32; 2]>> = self.strokes.iter().map(|s| s.iter().map(|p| [p.x, p.y]).collect()).collect();
                k_pdf::scan::drawn_signature(&strokes, self.pen.width(), self.pen_ink)?
            }
            Mode::Type => {
                let font = fonts?.get(self.font)?;
                k_pdf::scan::typed_signature(self.name.trim(), &font.data, font.index, TYPED_EM, self.pen_ink)?
            }
            Mode::Photo => return self.preview.clone(),
        };
        Some(Lifted::new(ctx, signature))
    }

    /// The picture, fitted into `room`, with the box drawn on it.
    fn picture(&mut self, ctx: &egui::Context, ui: &mut egui::Ui, room: Vec2) {
        let Some(source) = &self.source else { return };
        let size = Vec2::new(source.image.width as f32, source.image.height as f32);
        let k = (room.x / size.x).min(room.y / size.y);
        let (response, painter) = ui.allocate_painter(size * k, Sense::drag());
        let shown = response.rect;
        let to_screen = |p: Pos2| shown.min + p.to_vec2() * k;
        let to_image = |p: Pos2| (((p - shown.min) / k).to_pos2()).clamp(Pos2::ZERO, size.to_pos2());
        painter.image(source.texture.id(), shown, Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)), Color32::WHITE);

        let inside = |p: Pos2| self.area.is_some_and(|a| a.contains(to_image(p)));
        if let Some(pos) = response.hover_pos() {
            ctx.set_cursor_icon(if inside(pos) && !self.whole() { CursorIcon::Grab } else { CursorIcon::Crosshair });
        }
        let press = ui.input(|i| i.pointer.press_origin());
        if response.drag_started()
            && let Some(start) = press.or(response.interact_pointer_pos())
        {
            // Inside a box (not the whole picture), it moves; elsewhere a
            // new box is drawn.
            self.drag = Some(match self.area {
                Some(a) if inside(start) && !self.whole() => (false, (to_image(start) - a.min).to_pos2()),
                _ => (true, to_image(start)),
            });
        }
        if let (Some((new, at)), Some(pos)) = (self.drag, response.interact_pointer_pos())
            && response.dragged()
        {
            let pos = to_image(pos);
            if new {
                self.area = Some(Rect::from_two_pos(at, pos));
            } else if let Some(a) = self.area {
                let min = pos - at.to_vec2();
                let min = Pos2::new(min.x.clamp(0.0, size.x - a.width()), min.y.clamp(0.0, size.y - a.height()));
                self.area = Some(Rect::from_min_size(min, a.size()));
            }
        }
        if response.drag_stopped() {
            self.drag = None;
            if self.area.is_some_and(|a| a.width() < MIN_BOX || a.height() < MIN_BOX) {
                self.area = Some(Rect::from_min_size(Pos2::ZERO, size));
            }
            self.update_preview(ctx);
        }

        // Outside the box dimmed, the box outlined.
        if let Some(a) = self.area.filter(|_| !self.whole() || self.drag.is_some()) {
            let a = Rect::from_min_max(to_screen(a.min), to_screen(a.max));
            let dim = Color32::from_black_alpha(110);
            for r in [
                Rect::from_min_max(shown.min, Pos2::new(shown.max.x, a.min.y)),
                Rect::from_min_max(Pos2::new(shown.min.x, a.max.y), shown.max),
                Rect::from_min_max(Pos2::new(shown.min.x, a.min.y), Pos2::new(a.min.x, a.max.y)),
                Rect::from_min_max(Pos2::new(a.max.x, a.min.y), Pos2::new(shown.max.x, a.max.y)),
            ] {
                painter.rect_filled(r, 0.0, dim);
            }
            painter.rect_stroke(a, 0.0, Stroke::new(2.0, crate::theme::palette(ui).accent), StrokeKind::Outside);
        }
    }

    /// Whether the box is the whole picture (as it starts).
    fn whole(&self) -> bool {
        match (&self.source, self.area) {
            (Some(s), Some(a)) => a.min == Pos2::ZERO && a.max == Pos2::new(s.image.width as f32, s.image.height as f32),
            _ => false,
        }
    }

    fn update_preview(&mut self, ctx: &egui::Context) {
        let (Some(source), Some(a)) = (&self.source, self.area) else { return };
        let area = [a.min.x.floor() as u32, a.min.y.floor() as u32, a.max.x.ceil() as u32, a.max.y.ceil() as u32];
        match k_pdf::scan::lift_signature(&source.image, area, self.ink) {
            Some(signature) => {
                self.preview = Some(Lifted::new(ctx, signature));
                self.message = None;
            }
            None => {
                self.preview = None;
                self.message = Some("No signature in the box: it looks like blank paper. Drag a box around the signature.".into());
            }
        }
    }
}

/// The lifted signature on a checkerboard (showing what is transparent).
fn preview(ui: &mut egui::Ui, lifted: Option<&Lifted>, room: Vec2) {
    let (rect, painter) = ui.allocate_painter(room, Sense::hover());
    let rect = rect.rect;
    let cell = 8.0;
    painter.rect_filled(rect, 4.0, Color32::from_gray(250));
    let (cols, rows) = ((rect.width() / cell).ceil() as usize, (rect.height() / cell).ceil() as usize);
    for y in 0..rows {
        for x in (y % 2..cols).step_by(2) {
            let min = rect.min + Vec2::new(x as f32, y as f32) * cell;
            painter.rect_filled(Rect::from_min_max(min, (min + Vec2::splat(cell)).min(rect.max)), 0.0, Color32::from_gray(225));
        }
    }
    if let Some(l) = lifted {
        let inner = rect.shrink(8.0);
        let k = (inner.width() / l.signature.width as f32).min(inner.height() / l.signature.height as f32);
        let size = Vec2::new(l.signature.width as f32, l.signature.height as f32) * k;
        let uv = Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0));
        painter.image(l.texture.id(), Rect::from_center_size(inner.center(), size), uv, l.color());
    }
}

/// The ink's color on screen (the drawing sheet is white in both
/// themes, like paper).
fn ink_color(ink: Ink) -> Color32 {
    let [r, g, b] = ink.pen();
    Color32::from_rgb(r, g, b)
}

/// A stroke as it is drawn: quadratic curves through the middles of its
/// steps, as `k_pdf::scan::drawn_signature` draws it, as short lines.
fn smooth(points: &[Pos2]) -> Vec<Pos2> {
    if points.len() < 3 {
        return points.to_vec();
    }
    let mut out = vec![points[0]];
    let mut from = points[0];
    for pair in points[1..].windows(2) {
        let (c, to) = (pair[0], pair[0].lerp(pair[1], 0.5));
        for i in 1..=4 {
            let t = i as f32 / 4.0;
            out.push(from.lerp(c, t).lerp(c.lerp(to, t), t));
        }
        from = to;
    }
    out.push(*points.last().expect("points"));
    out
}

/// Finds the handwriting fonts and reads their files, on a thread.
fn find_fonts() -> mpsc::Receiver<Vec<Font>> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let fonts = k_pdf::edit::fonts::handwriting_fonts()
            .into_iter()
            .filter_map(|name| {
                let (data, index) = k_pdf::edit::fonts::family_font(&name, true)?;
                Some(Font { name, data, index })
            })
            .collect();
        tx.send(fonts).ok();
    });
    rx
}

/// Reads a picture to take a signature from, on a thread of its own: an
/// image, or page `page` of a PDF (as for scanning).
fn load(ctx: &egui::Context, path: &Path, page: usize) -> Loading {
    let (tx, rx) = mpsc::channel();
    let (ctx, owned) = (ctx.clone(), path.to_path_buf());
    std::thread::spawn(move || {
        let read = || -> Read {
            let data = std::fs::read(&owned).map_err(|e| e.to_string())?;
            if data.starts_with(b"%PDF") || owned.extension().is_some_and(|e| e.eq_ignore_ascii_case("pdf")) {
                let doc = k_pdf::Document::from_bytes(data).map_err(|e| e.to_string())?;
                let pages = doc.pages().map_err(|e| e.to_string())?;
                let p = pages.get(page).ok_or("it has no pages")?;
                let mut renderer = k_pdf::Renderer::new(&doc);
                let image = k_pdf::scan::pdf_page_image(&doc, &mut renderer, p, k_pdf::scan::PDF_PAGE_PIXELS).map_err(|e| e.to_string())?;
                Ok((image, Some((page, pages.len()))))
            } else {
                Ok((Image::decode(&data).map_err(|e| e.to_string())?, None))
            }
        };
        tx.send(read()).ok();
        ctx.request_repaint();
    });
    Loading { name: path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(), reply: rx, path: path.to_path_buf() }
}
