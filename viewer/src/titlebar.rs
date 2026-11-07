//! A custom title bar: the toolbar doubles as the window's title bar, with
//! minimize / maximize / close buttons, dragging, double-click to maximize,
//! and resizing from the window edges.
//!
//! On Linux and Windows the window is created without system decorations
//! so it looks and works the same everywhere (on GNOME/Wayland there would
//! otherwise be none). On macOS the native title bar is kept (its "traffic
//! light" buttons are what Mac users expect), and these functions do nothing.

/// Whether the system draws the window's title bar (macOS).
pub const NATIVE: bool = cfg!(target_os = "macos");

use eframe::egui::{
    self, Color32, CursorIcon, Id, Pos2, Rect, ResizeDirection, Sense, Stroke, StrokeKind, Vec2, ViewportCommand,
};

/// Width of the invisible resize border around the window.
const RESIZE_BORDER: f32 = 6.0;
const BUTTON_SIZE: Vec2 = Vec2::new(34.0, 30.0);
/// Width of the minimize / maximize / close buttons together.
pub const BUTTONS_WIDTH: f32 = if NATIVE { 0.0 } else { 3.0 * BUTTON_SIZE.x };

fn is_maximized(ctx: &egui::Context) -> bool {
    ctx.input(|i| i.viewport().maximized.unwrap_or(false))
}

/// Makes `rect` (the title bar) move the window when dragged and toggle
/// maximization on double-click. Call before adding the bar's widgets so
/// they stay clickable.
pub fn drag_area(ui: &mut egui::Ui, rect: Rect) {
    if NATIVE {
        return;
    }
    let response = ui.interact(rect, Id::new("titlebar-drag"), Sense::click_and_drag());
    if response.double_clicked() {
        let maximized = is_maximized(ui.ctx());
        ui.ctx().send_viewport_cmd(ViewportCommand::Maximized(!maximized));
    } else if response.drag_started_by(egui::PointerButton::Primary) {
        ui.ctx().send_viewport_cmd(ViewportCommand::StartDrag);
    }
}

/// Minimize, maximize/restore and close buttons, right-aligned in `ui`.
pub fn window_buttons(ui: &mut egui::Ui) {
    if NATIVE {
        return;
    }
    let ctx = ui.ctx().clone();
    ui.spacing_mut().item_spacing.x = 0.0;
    // Laid out right to left: close first.
    if icon_button(ui, Icon::Close, "Close").clicked() {
        ctx.send_viewport_cmd(ViewportCommand::Close);
    }
    let maximized = is_maximized(&ctx);
    let (icon, tip) = if maximized { (Icon::Restore, "Restore") } else { (Icon::Maximize, "Maximize") };
    if icon_button(ui, icon, tip).clicked() {
        ctx.send_viewport_cmd(ViewportCommand::Maximized(!maximized));
    }
    if icon_button(ui, Icon::Minimize, "Minimize").clicked() {
        ctx.send_viewport_cmd(ViewportCommand::Minimized(true));
    }
}

/// Just a close button, for secondary windows.
pub fn close_button(ui: &mut egui::Ui) {
    if NATIVE {
        return;
    }
    if icon_button(ui, Icon::Close, "Close").clicked() {
        ui.ctx().send_viewport_cmd(ViewportCommand::Close);
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Icon {
    Minimize,
    Maximize,
    Restore,
    Close,
}

/// Draws a title-bar button with a crisp vector icon.
fn icon_button(ui: &mut egui::Ui, icon: Icon, tooltip: &str) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(BUTTON_SIZE, Sense::click());
    let visuals = ui.visuals();
    let hovered = response.hovered();
    let pressed = response.is_pointer_button_down_on();
    // Round buttons, as on GNOME: a faint circle, stronger on hover; close
    // turns red.
    let p = crate::theme::of(visuals);
    let bg = match (icon, hovered, pressed) {
        (Icon::Close, true, _) => Color32::from_rgb(224, 60, 60),
        (_, true, true) => p.pressed.gamma_multiply(1.6),
        (_, true, false) => p.pressed,
        _ => p.hover,
    };
    let painter = ui.painter();
    painter.circle_filled(rect.center(), 12.0, bg);
    let fg = if icon == Icon::Close && hovered { Color32::WHITE } else { visuals.text_color() };
    let stroke = Stroke::new(1.3, fg);
    let c = rect.center();
    let s = 3.8; // half icon size
    match icon {
        Icon::Minimize => {
            painter.line_segment([Pos2::new(c.x - s, c.y), Pos2::new(c.x + s, c.y)], stroke);
        }
        Icon::Maximize => {
            let r = Rect::from_center_size(c, Vec2::splat(2.0 * s));
            painter.rect_stroke(r, 0.0, stroke, StrokeKind::Inside);
        }
        Icon::Restore => {
            let back = Rect::from_center_size(c + Vec2::new(1.5, -1.5), Vec2::splat(1.6 * s));
            let front = Rect::from_center_size(c + Vec2::new(-1.5, 1.5), Vec2::splat(1.6 * s));
            painter.rect_stroke(back, 0.0, stroke, StrokeKind::Inside);
            painter.rect_filled(front, 0.0, visuals.panel_fill);
            painter.rect_stroke(front, 0.0, stroke, StrokeKind::Inside);
        }
        Icon::Close => {
            painter.line_segment([c + Vec2::new(-s, -s), c + Vec2::new(s, s)], stroke);
            painter.line_segment([c + Vec2::new(s, -s), c + Vec2::new(-s, s)], stroke);
        }
    }
    response.on_hover_text(tooltip)
}

/// Lets the user resize the undecorated window by dragging its edges, and
/// draws a thin border so the window stands out from what's behind it.
/// Call at the start of each frame.
pub fn resize_edges(ctx: &egui::Context) {
    if NATIVE {
        return;
    }
    let rect = ctx.content_rect();
    let border_color = ctx.global_style().visuals.widgets.noninteractive.bg_stroke.color;
    ctx.layer_painter(egui::LayerId::new(egui::Order::Foreground, Id::new("window-border")))
        .rect_stroke(rect, 0.0, Stroke::new(1.0, border_color), StrokeKind::Inside);

    if is_maximized(ctx) {
        return;
    }
    let Some(pos) = ctx.input(|i| i.pointer.hover_pos()) else { return };
    let left = pos.x - rect.left() < RESIZE_BORDER;
    let right = rect.right() - pos.x < RESIZE_BORDER;
    let top = pos.y - rect.top() < RESIZE_BORDER;
    let bottom = rect.bottom() - pos.y < RESIZE_BORDER;
    let (dir, cursor) = match (left, right, top, bottom) {
        (true, _, true, _) => (ResizeDirection::NorthWest, CursorIcon::ResizeNwSe),
        (_, true, _, true) => (ResizeDirection::SouthEast, CursorIcon::ResizeNwSe),
        (_, true, true, _) => (ResizeDirection::NorthEast, CursorIcon::ResizeNeSw),
        (true, _, _, true) => (ResizeDirection::SouthWest, CursorIcon::ResizeNeSw),
        (true, ..) => (ResizeDirection::West, CursorIcon::ResizeHorizontal),
        (_, true, ..) => (ResizeDirection::East, CursorIcon::ResizeHorizontal),
        (_, _, true, _) => (ResizeDirection::North, CursorIcon::ResizeVertical),
        (_, _, _, true) => (ResizeDirection::South, CursorIcon::ResizeVertical),
        _ => return,
    };
    ctx.set_cursor_icon(cursor);
    if ctx.input(|i| i.pointer.primary_pressed()) {
        ctx.send_viewport_cmd(ViewportCommand::BeginResize(dir));
    }
}
