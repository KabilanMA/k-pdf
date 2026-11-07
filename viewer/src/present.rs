//! Presenting: the pages as full-screen slides, with smooth transitions,
//! and a presenter view in a second window.
//!
//! F5 starts from the first page, Shift+F5 from the current one (also in
//! the toolbar and the ⋯ menu). The slide fills the screen on black; →,
//! Space, Page Down, Enter or a click go forward, ←, Backspace, Page Up or
//! a right click go back, Home / End to the first / last slide, a number
//! then Enter to that slide, B / W to blank the screen black / white, P to
//! show or hide the presenter view, Esc to end. Slides change with a short
//! fade (or a push, or nothing, chosen in the presenter view).
//!
//! The presenter view (a separate window, for the presenter's screen)
//! shows the current and next slide, the comments on the current page as
//! speaker notes, the slide number and a timer that can be paused.
//!
//! With more than one screen, the presenter view fills the main screen
//! (the primary one, or the one the viewer was on) and the slides another;
//! the presenter view can move the slides to another screen or swap them.
//! With one screen, the slides fill it and the presenter view is a window.

use std::time::{Duration, Instant};

use eframe::egui::{self, Align2, Color32, CursorIcon, FontId, Key, Pos2, Rect, RichText, Sense, TextureId, Vec2};

/// How long a transition takes.
const TRANSITION: Duration = Duration::from_millis(450);
/// The hint at the start stays this long.
const HINT: Duration = Duration::from_secs(4);
/// The pointer hides after this long without moving.
const POINTER_IDLE: Duration = Duration::from_secs(2);

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Transition {
    Fade,
    Push,
    None,
}

impl Transition {
    fn name(self) -> &'static str {
        match self {
            Transition::Fade => "Fade",
            Transition::Push => "Push",
            Transition::None => "None",
        }
    }
}

/// What the presenter asked for.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Nav {
    Next,
    Prev,
    First,
    Last,
    /// A slide, by position.
    GoTo(usize),
    /// Blank the screen in this color, or show the slide again.
    Blank(Color32),
    TogglePresenter,
    /// Show the slides on this screen (the presenter view goes to another).
    SlidesOn(usize),
    /// Swap the slides' and the presenter view's screens.
    SwapScreens,
    Exit,
}

/// A screen (monitor), in the order the windowing system lists them.
#[derive(Clone, Debug, PartialEq)]
pub struct Screen {
    /// Its number in the windowing system's list (what windows are moved
    /// with).
    pub index: usize,
    pub name: String,
    pub size: (u32, u32),
    /// The main screen: the primary one, or the one the viewer was on.
    pub main: bool,
}

/// The slide being left, during a transition.
struct Outgoing {
    position: usize,
    /// When the transition started: once the new slide is ready.
    start: Option<Instant>,
    forward: bool,
}

/// A presentation in progress.
pub struct Show {
    /// The slide shown (a position).
    pub position: usize,
    from: Option<Outgoing>,
    pub transition: Transition,
    pub presenter: bool,
    pub blank: Option<Color32>,
    /// The timer: when it started, and how long it has been paused.
    started: Instant,
    paused_at: Option<Instant>,
    paused: Duration,
    /// Digits typed to go to a slide.
    typed: String,
    pointer_moved: Instant,
    shown_at: Instant,
    last_wheel: Instant,
    /// The view to go back to: zoom and fit.
    pub saved_zoom: f32,
    /// The screens, once read (empty: not known, or just one).
    pub screens: Vec<Screen>,
    pub screens_read: bool,
    /// Where the slides and the presenter view go, with several screens.
    pub slides_screen: Option<usize>,
    pub presenter_screen: Option<usize>,
    /// The slides' screen changed: the window must be moved.
    pub move_slides: bool,
}

impl Show {
    pub fn new(position: usize, presenter: bool, saved_zoom: f32) -> Show {
        let now = Instant::now();
        Show {
            position,
            from: None,
            transition: Transition::Fade,
            presenter,
            blank: None,
            started: now,
            paused_at: None,
            paused: Duration::ZERO,
            typed: String::new(),
            pointer_moved: now,
            shown_at: now,
            last_wheel: now,
            saved_zoom,
            screens: Vec::new(),
            screens_read: false,
            slides_screen: None,
            presenter_screen: None,
            move_slides: false,
        }
    }

    /// Takes the screens: with several, the presenter view goes on the
    /// main one and the slides on another. Returns whether there are
    /// several.
    pub fn set_screens(&mut self, screens: Vec<Screen>) -> bool {
        self.screens_read = true;
        self.screens = screens;
        if self.screens.len() < 2 {
            return false;
        }
        let main = self.screens.iter().position(|s| s.main).unwrap_or(0);
        let other = (0..self.screens.len()).find(|&i| i != main).unwrap_or(0);
        self.place(other);
        self.presenter = true;
        true
    }

    /// Slides on screen `slides`, the presenter view on the main screen
    /// (or, if the slides go there, on another).
    pub fn place(&mut self, slides: usize) {
        let main = self.screens.iter().position(|s| s.main).unwrap_or(0);
        let presenter = if slides != main { main } else { (0..self.screens.len()).find(|&i| i != slides).unwrap_or(0) };
        self.slides_screen = Some(slides);
        self.presenter_screen = Some(presenter);
        self.move_slides = true;
    }

    pub fn swap_screens(&mut self) {
        if let (Some(s), Some(p)) = (self.slides_screen, self.presenter_screen) {
            self.slides_screen = Some(p);
            self.presenter_screen = Some(s);
            self.move_slides = true;
        }
    }

    /// Moves to slide `position` (within `count`), with a transition.
    pub fn go(&mut self, position: usize, count: usize) {
        let position = position.min(count.saturating_sub(1));
        if position == self.position {
            return;
        }
        self.blank = None;
        if self.transition == Transition::None {
            self.from = None;
        } else {
            self.from = Some(Outgoing { position: self.position, start: None, forward: position > self.position });
        }
        self.position = position;
    }

    pub fn elapsed(&self) -> Duration {
        let end = self.paused_at.unwrap_or_else(Instant::now);
        end.duration_since(self.started).saturating_sub(self.paused)
    }

    fn toggle_pause(&mut self) {
        match self.paused_at.take() {
            Some(at) => self.paused += at.elapsed(),
            None => self.paused_at = Some(Instant::now()),
        }
    }

    fn reset_timer(&mut self) {
        self.started = Instant::now();
        self.paused = Duration::ZERO;
        if self.paused_at.is_some() {
            self.paused_at = Some(self.started);
        }
    }

    /// Keys, in either window.
    pub fn keys(&mut self, input: &egui::InputState) -> Vec<Nav> {
        let mut out = Vec::new();
        for e in &input.events {
            match e {
                egui::Event::Key { key, pressed: true, modifiers, .. } if !modifiers.command => match key {
                    Key::ArrowRight | Key::ArrowDown | Key::Space | Key::PageDown | Key::N => out.push(Nav::Next),
                    Key::Enter => match self.typed.parse::<usize>() {
                        Ok(n) if n > 0 => out.push(Nav::GoTo(n - 1)),
                        _ => out.push(Nav::Next),
                    },
                    Key::ArrowLeft | Key::ArrowUp | Key::Backspace | Key::PageUp => out.push(Nav::Prev),
                    Key::Home => out.push(Nav::First),
                    Key::End => out.push(Nav::Last),
                    Key::B | Key::Period => out.push(Nav::Blank(Color32::BLACK)),
                    Key::W | Key::Comma => out.push(Nav::Blank(Color32::WHITE)),
                    Key::P => out.push(Nav::TogglePresenter),
                    Key::Escape => out.push(Nav::Exit),
                    _ => {}
                },
                egui::Event::Text(t) if t.chars().all(|c| c.is_ascii_digit()) => {
                    self.typed.push_str(t);
                    continue;
                }
                _ => {}
            }
            if matches!(e, egui::Event::Key { pressed: true, .. }) {
                self.typed.clear();
            }
        }
        out
    }
}

/// Where a slide of `size` (points) goes on `screen`: as large as fits,
/// centered.
fn fit(screen: Rect, size: Vec2) -> Rect {
    let scale = (screen.width() / size.x.max(1.0)).min(screen.height() / size.y.max(1.0));
    Rect::from_center_size(screen.center(), size * scale)
}

fn smooth(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// The full-screen slide. `texture` gives a slide's rendered page, `sizes`
/// the page sizes by position.
pub fn audience(ui: &mut egui::Ui, show: &mut Show, sizes: &[Vec2], texture: &dyn Fn(usize) -> Option<TextureId>) -> Vec<Nav> {
    let mut navs = Vec::new();
    let screen = ui.max_rect();
    let response = ui.interact(screen, ui.id().with("present"), Sense::click());
    let painter = ui.painter();
    painter.rect_filled(screen, 0.0, Color32::BLACK);
    let uv = Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0));
    let ctx = ui.ctx().clone();

    if let Some(color) = show.blank {
        painter.rect_filled(screen, 0.0, color);
    } else {
        let current = texture(show.position);
        let rect = fit(screen, sizes[show.position]);
        let mut drawn = false;
        if let Some(from) = &mut show.from {
            let old = texture(from.position);
            let old_rect = fit(screen, sizes[from.position]);
            if current.is_some() && from.start.is_none() {
                from.start = Some(Instant::now());
            }
            let t = from.start.map_or(0.0, |s| s.elapsed().as_secs_f32() / TRANSITION.as_secs_f32());
            if t < 1.0 {
                let e = smooth(t);
                drawn = true;
                match show.transition {
                    Transition::Push => {
                        let dir = if from.forward { -1.0 } else { 1.0 };
                        let shift = Vec2::new(dir * screen.width() * e, 0.0);
                        if let Some(old) = old {
                            painter.image(old, old_rect.translate(shift), uv, Color32::WHITE);
                        }
                        if let Some(new) = current {
                            let enter = Vec2::new(-dir * screen.width() * (1.0 - e), 0.0);
                            painter.image(new, rect.translate(enter), uv, Color32::WHITE);
                        }
                    }
                    _ => {
                        if let Some(old) = old {
                            painter.image(old, old_rect, uv, Color32::WHITE.gamma_multiply(1.0 - e));
                        }
                        if let Some(new) = current {
                            painter.image(new, rect, uv, Color32::WHITE.gamma_multiply(e));
                        }
                    }
                }
                ctx.request_repaint();
            }
        }
        if !drawn {
            show.from = None;
            if let Some(new) = current {
                painter.image(new, rect, uv, Color32::WHITE);
            }
        }
    }

    // The pointer: hidden while still; clicks and the wheel move.
    if ctx.input(|i| i.pointer.delta() != Vec2::ZERO) {
        show.pointer_moved = Instant::now();
    }
    let moving = show.pointer_moved.elapsed() < POINTER_IDLE;
    if !moving {
        ctx.set_cursor_icon(CursorIcon::None);
    } else {
        ctx.request_repaint_after(POINTER_IDLE);
    }
    if response.clicked() {
        navs.push(Nav::Next);
    }
    if response.secondary_clicked() {
        navs.push(Nav::Prev);
    }
    let wheel: f32 = ctx.input(|i| {
        i.raw.events.iter().map(|e| if let egui::Event::MouseWheel { delta, .. } = e { delta.y } else { 0.0 }).sum()
    });
    if wheel.abs() > 0.1 && show.last_wheel.elapsed() > Duration::from_millis(250) {
        show.last_wheel = Instant::now();
        navs.push(if wheel < 0.0 { Nav::Next } else { Nav::Prev });
    }

    // The slide number while the pointer moves, and the hint at first.
    let faint = Color32::from_white_alpha(150);
    if moving && show.blank.is_none() {
        let text = format!("{} / {}", show.position + 1, sizes.len());
        painter.text(screen.right_bottom() - Vec2::new(16.0, 12.0), Align2::RIGHT_BOTTOM, text, FontId::proportional(14.0), faint);
    }
    let since = show.shown_at.elapsed();
    if since < HINT && show.blank.is_none() {
        let alpha = 1.0 - (since.as_secs_f32() - (HINT.as_secs_f32() - 1.0)).max(0.0);
        let text = "→ / click: next   ← : back   P: presenter view   B: black screen   Esc: end";
        let galley = painter.layout_no_wrap(text.into(), FontId::proportional(15.0), Color32::WHITE.gamma_multiply(alpha));
        let at = Pos2::new(screen.center().x, screen.bottom() - 40.0);
        let bg = Rect::from_center_size(at, galley.size() + Vec2::new(28.0, 14.0));
        painter.rect_filled(bg, 10.0, Color32::from_black_alpha((170.0 * alpha) as u8));
        painter.galley(at - galley.size() / 2.0, galley, Color32::WHITE);
        ctx.request_repaint();
    }
    navs
}

/// What the presenter view shows besides the slides.
pub struct Notes {
    pub title: String,
    /// The comments on the current page.
    pub notes: Vec<String>,
}

/// The presenter view, in its own window: on its screen when there are
/// several, else a normal window. Returns what was asked for.
pub fn presenter(
    ctx: &egui::Context,
    show: &mut Show,
    sizes: &[Vec2],
    texture: &dyn Fn(usize) -> Option<TextureId>,
    notes: &Notes,
) -> Vec<Nav> {
    let mut navs = Vec::new();
    let mut builder = egui::ViewportBuilder::default()
        .with_title(format!("Presenter view — {}", notes.title))
        .with_inner_size([1100.0, 700.0])
        .with_min_inner_size([640.0, 420.0])
        .with_decorations(crate::titlebar::NATIVE);
    if let Some(screen) = show.presenter_screen.and_then(|i| show.screens.get(i)) {
        builder = builder.with_monitor(screen.index);
    }
    ctx.show_viewport_immediate(egui::ViewportId::from_hash_of("presenter-view"), builder, |ui, _| {
        let ctx = ui.ctx().clone();
        // Closing the presenter view ends the show (P only hides it).
        if ctx.input(|i| i.viewport().close_requested()) {
            navs.push(Nav::Exit);
        }
        crate::titlebar::resize_edges(&ctx);
        if !ctx.egui_wants_keyboard_input() {
            navs.extend(ctx.input(|i| show.keys(i)));
        }
        presenter_ui(ui, show, sizes, texture, notes, &mut navs);
    });
    navs
}

/// A card: a raised, rounded area with a shadow and a small title.
fn card(ui: &egui::Ui, rect: Rect, title: &str) -> Rect {
    let p = crate::theme::palette(ui);
    let painter = ui.painter();
    painter.add(egui::epaint::Shadow { offset: [0, 4], blur: 16, spread: 0, color: p.shadow }.as_shape(rect, 12));
    painter.rect(rect, 12.0, p.raised, egui::Stroke::new(1.0, p.border), egui::StrokeKind::Inside);
    painter.text(rect.min + Vec2::new(14.0, 11.0), Align2::LEFT_TOP, title, FontId::proportional(12.5), p.weak);
    Rect::from_min_max(rect.min + Vec2::new(14.0, 34.0), rect.max - Vec2::new(14.0, 14.0))
}

/// A slide (with a shadow) in `area`.
fn slide(ui: &egui::Ui, area: Rect, size: Vec2, texture: Option<TextureId>) -> Rect {
    let p = crate::theme::palette(ui);
    let rect = fit(area, size);
    let painter = ui.painter();
    painter.add(egui::epaint::Shadow { offset: [0, 3], blur: 12, spread: 0, color: p.shadow }.as_shape(rect, 2));
    painter.rect_filled(rect, 2.0, Color32::WHITE);
    if let Some(t) = texture {
        painter.image(t, rect, Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)), Color32::WHITE);
    }
    rect
}

/// The presenter view's contents.
pub fn presenter_ui(
    ui: &mut egui::Ui,
    show: &mut Show,
    sizes: &[Vec2],
    texture: &dyn Fn(usize) -> Option<TextureId>,
    notes: &Notes,
    navs: &mut Vec<Nav>,
) {
    let ctx = ui.ctx().clone();
    let p = crate::theme::palette(ui);
    let count = sizes.len();

    // Title bar: name, slide number, window buttons.
    egui::Panel::top("presenter-title")
        .exact_size(46.0)
        .frame(egui::Frame::new().fill(p.panel))
        .show(ui, |ui| {
            crate::titlebar::drag_area(ui, ui.max_rect());
            ui.horizontal_centered(|ui| {
                ui.add_space(12.0);
                crate::icons::paint(
                    ui.painter(),
                    Rect::from_center_size(ui.cursor().min + Vec2::new(8.0, 23.0), Vec2::splat(16.0)),
                    crate::icons::Icon::Present,
                    p.accent,
                );
                ui.add_space(24.0);
                ui.label(RichText::new("Presenter view").strong());
                ui.label(RichText::new(&notes.title).color(p.weak));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    crate::titlebar::window_buttons(ui);
                    ui.add_space(12.0);
                    // The slide number on a small pill.
                    let chip = format!("Slide {} of {count}", show.position + 1);
                    let galley = ui.painter().layout_no_wrap(chip, FontId::proportional(13.0), p.accent);
                    let (rect, _) = ui.allocate_exact_size(galley.size() + Vec2::new(20.0, 8.0), Sense::hover());
                    ui.painter().rect_filled(rect, rect.height() / 2.0, p.accent_soft);
                    ui.painter().galley(rect.center() - galley.size() / 2.0, galley, p.accent);
                });
            });
        });

    // Controls.
    egui::Panel::bottom("presenter-controls")
        .exact_size(58.0)
        .frame(egui::Frame::new().fill(p.panel).inner_margin(egui::Margin::symmetric(14, 0)))
        .show(ui, |ui| {
            ui.horizontal_centered(|ui| {
                ui.spacing_mut().item_spacing.x = 8.0;
                if crate::icons::button(ui, crate::icons::Icon::Left, Some("Previous"), false, show.position > 0)
                    .on_hover_text("← or Page Up")
                    .clicked()
                {
                    navs.push(Nav::Prev);
                }
                if crate::icons::button(ui, crate::icons::Icon::Right, Some("Next"), false, show.position + 1 < count)
                    .on_hover_text("→, Space or Page Down")
                    .clicked()
                {
                    navs.push(Nav::Next);
                }
                ui.add_space(6.0);
                ui.label(RichText::new("Transition").color(p.weak));
                egui::ComboBox::from_id_salt("transition").width(90.0).selected_text(show.transition.name()).show_ui(ui, |ui| {
                    for t in [Transition::Fade, Transition::Push, Transition::None] {
                        ui.selectable_value(&mut show.transition, t, t.name());
                    }
                });
                let blank = show.blank.is_some();
                if ui.selectable_label(blank, "Black screen").on_hover_text("B").clicked() {
                    navs.push(Nav::Blank(Color32::BLACK));
                }
                if show.screens.len() >= 2 {
                    ui.add_space(6.0);
                    ui.label(RichText::new("Slides on").color(p.weak));
                    let current = show.slides_screen.unwrap_or(0);
                    let label = |i: usize| screen_label(&show.screens, i);
                    egui::ComboBox::from_id_salt("slides-screen").width(170.0).selected_text(label(current)).show_ui(ui, |ui| {
                        for i in 0..show.screens.len() {
                            if ui.selectable_label(i == current, label(i)).clicked() {
                                navs.push(Nav::SlidesOn(i));
                            }
                        }
                    });
                    if ui.button("Swap screens").on_hover_text("Show the slides on the presenter's screen and this view on the other").clicked() {
                        navs.push(Nav::SwapScreens);
                    }
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if crate::theme::danger_button(ui, "End show", true).on_hover_text("Esc").clicked() {
                        navs.push(Nav::Exit);
                    }
                });
            });
        });

    egui::CentralPanel::default().frame(egui::Frame::new().fill(p.canvas).inner_margin(16.0)).show(ui, |ui| {
        let area = ui.available_rect_before_wrap();
        let gap = 16.0;
        let left = Rect::from_min_max(area.min, Pos2::new(area.left() + (area.width() - gap) * 0.63, area.bottom()));
        let right = Rect::from_min_max(Pos2::new(left.right() + gap, area.top()), area.max);

        // The current slide.
        let inner = card(ui, left, "Current");
        let current = slide(ui, inner, sizes[show.position], texture(show.position));
        if let Some(color) = show.blank {
            ui.painter().rect_filled(current, 2.0, color.gamma_multiply(0.9));
            let text = if color == Color32::BLACK { "The screen is black (B)" } else { "The screen is white (W)" };
            ui.painter().text(current.center(), Align2::CENTER_CENTER, text, FontId::proportional(16.0), Color32::from_gray(140));
        }

        // The timer.
        let timer = Rect::from_min_size(right.min, Vec2::new(right.width(), 96.0));
        let inner = card(ui, timer, "Time");
        let secs = show.elapsed().as_secs();
        let time = format!("{:02}:{:02}:{:02}", secs / 3600, secs / 60 % 60, secs % 60);
        let paused = show.paused_at.is_some();
        let mut row = ui.new_child(egui::UiBuilder::new().max_rect(inner).layout(egui::Layout::left_to_right(egui::Align::Center)));
        row.label(RichText::new(time).size(30.0).monospace().color(if paused { p.weak } else { p.text }));
        row.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui.button("Reset").clicked() {
                show.reset_timer();
            }
            if ui.button(if paused { "Resume" } else { "Pause" }).clicked() {
                show.toggle_pause();
            }
        });
        ctx.request_repaint_after(Duration::from_millis(500));

        // The next slide.
        let next_box = Rect::from_min_size(Pos2::new(right.left(), timer.bottom() + gap), Vec2::new(right.width(), (right.height() - 96.0 - 2.0 * gap) * 0.5));
        let inner = card(ui, next_box, "Next");
        if show.position + 1 < count {
            slide(ui, inner, sizes[show.position + 1], texture(show.position + 1));
        } else {
            ui.painter().text(inner.center(), Align2::CENTER_CENTER, "End of the presentation", FontId::proportional(14.0), p.weak);
        }

        // Notes: the comments on this page.
        let notes_box = Rect::from_min_max(Pos2::new(right.left(), next_box.bottom() + gap), right.max);
        let inner = card(ui, notes_box, "Notes");
        let mut notes_ui = ui.new_child(egui::UiBuilder::new().max_rect(inner).layout(egui::Layout::top_down(egui::Align::Min)));
        egui::ScrollArea::vertical().id_salt("notes").auto_shrink(false).show(&mut notes_ui, |ui| {
            if notes.notes.is_empty() {
                ui.label(RichText::new("No notes on this page. Comments on a page show here as notes.").color(p.weak));
            }
            for note in &notes.notes {
                ui.label(RichText::new(note).size(16.0));
                ui.add_space(8.0);
            }
        });
    });
}

/// "HDMI-1 · 1920 × 1080 (main)".
fn screen_label(screens: &[Screen], i: usize) -> String {
    let Some(s) = screens.get(i) else { return format!("Screen {}", i + 1) };
    let mut label = format!("{} · {} × {}", s.name, s.size.0, s.size.1);
    if s.main {
        label += " (main)";
    }
    label
}

#[cfg(test)]
mod tests {
    use super::*;

    fn screens(n: usize, main: usize) -> Vec<Screen> {
        (0..n).map(|i| Screen { index: i, name: format!("S{i}"), size: (1920, 1080), main: i == main }).collect()
    }

    #[test]
    fn screens_are_placed() {
        let mut show = Show::new(0, false, 1.0);
        assert!(!show.set_screens(screens(1, 0)));
        assert_eq!(show.slides_screen, None);

        let mut show = Show::new(0, false, 1.0);
        assert!(show.set_screens(screens(2, 1)));
        // The presenter view on the main screen, slides on the other.
        assert_eq!((show.slides_screen, show.presenter_screen), (Some(0), Some(1)));
        assert!(show.presenter && show.move_slides);
        show.swap_screens();
        assert_eq!((show.slides_screen, show.presenter_screen), (Some(1), Some(0)));
        // Slides on the main screen: the presenter view moves away.
        let mut show = Show::new(0, false, 1.0);
        show.set_screens(screens(3, 0));
        show.place(0);
        assert_eq!((show.slides_screen, show.presenter_screen), (Some(0), Some(1)));
        show.place(2);
        assert_eq!((show.slides_screen, show.presenter_screen), (Some(2), Some(0)));
    }
}
