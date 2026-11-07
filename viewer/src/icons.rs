//! Toolbar icons drawn as vector lines (crisp at any scale, no image or
//! font files), and the toolbar's buttons.

use std::f32::consts::PI;

use eframe::egui::{self, Color32, CornerRadius, Pos2, Rect, Sense, Shape, Stroke, Vec2};

use crate::theme;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Icon {
    Open,
    Save,
    Undo,
    Redo,
    Edit,
    Crop,
    Redact,
    Sidebar,
    Left,
    Right,
    Plus,
    Minus,
    More,
    Document,
    Moon,
    Pages,
    Sections,
    Comment,
    Highlight,
    RotateLeft,
    RotateRight,
    Present,
    Trash,
}

/// Draws `icon` in `rect` (laid out on a 16 × 16 grid).
pub fn paint(painter: &egui::Painter, rect: Rect, icon: Icon, color: Color32) {
    let s = rect.width().min(rect.height()) / 16.0;
    let o = rect.center() - Vec2::splat(8.0 * s);
    let p = |x: f32, y: f32| Pos2::new(o.x + x * s, o.y + y * s);
    let stroke = Stroke::new((1.5 * s).max(1.0), color);
    let line = |pts: &[(f32, f32)]| painter.add(Shape::line(pts.iter().map(|&(x, y)| p(x, y)).collect(), stroke));
    let closed = |pts: &[(f32, f32)]| painter.add(Shape::closed_line(pts.iter().map(|&(x, y)| p(x, y)).collect(), stroke));
    // Points along a circle arc (angles in degrees, y down).
    let arc = |cx: f32, cy: f32, r: f32, from: f32, to: f32| -> Vec<(f32, f32)> {
        (0..=12)
            .map(|i| {
                let a = (from + (to - from) * i as f32 / 12.0) * PI / 180.0;
                (cx + r * a.cos(), cy + r * a.sin())
            })
            .collect()
    };
    match icon {
        Icon::Open => {
            closed(&[(1.5, 3.5), (6.0, 3.5), (7.5, 5.0), (14.5, 5.0), (14.5, 13.0), (1.5, 13.0)]);
            line(&[(1.5, 7.5), (14.5, 7.5)]);
        }
        Icon::Save => {
            closed(&[(2.0, 2.0), (11.5, 2.0), (14.0, 4.5), (14.0, 14.0), (2.0, 14.0)]);
            closed(&[(5.0, 2.0), (5.0, 5.5), (10.5, 5.5), (10.5, 2.0)]);
            closed(&[(4.5, 14.0), (4.5, 9.5), (11.5, 9.5), (11.5, 14.0)]);
        }
        Icon::Undo | Icon::Redo => {
            let flip = |pts: Vec<(f32, f32)>| -> Vec<(f32, f32)> {
                if icon == Icon::Redo { pts.into_iter().map(|(x, y)| (16.0 - x, y)).collect() } else { pts }
            };
            line(&flip(vec![(5.5, 2.5), (2.0, 6.0), (5.5, 9.5)]));
            let mut path = vec![(2.0, 6.0), (9.5, 6.0)];
            path.extend(arc(9.5, 9.5, 3.5, -90.0, 90.0));
            path.push((5.0, 13.0));
            line(&flip(path));
        }
        Icon::Edit => {
            closed(&[(2.5, 13.5), (3.0, 10.5), (10.5, 3.0), (13.0, 5.5), (5.5, 13.0)]);
            line(&[(9.0, 4.5), (11.5, 7.0)]);
        }
        Icon::Crop => {
            line(&[(4.5, 1.5), (4.5, 11.5), (14.5, 11.5)]);
            line(&[(1.5, 4.5), (11.5, 4.5), (11.5, 14.5)]);
        }
        Icon::Redact => {
            line(&[(2.0, 3.5), (14.0, 3.5)]);
            painter.rect_filled(Rect::from_min_max(p(2.0, 6.5), p(12.0, 9.5)), CornerRadius::same(1), color);
            line(&[(2.0, 12.5), (10.0, 12.5)]);
        }
        Icon::Sidebar => {
            painter.rect_stroke(
                Rect::from_min_max(p(1.5, 2.5), p(14.5, 13.5)),
                CornerRadius::same((2.0 * s) as u8),
                stroke,
                egui::StrokeKind::Middle,
            );
            line(&[(6.0, 2.5), (6.0, 13.5)]);
        }
        Icon::Left => {
            line(&[(10.0, 3.5), (5.5, 8.0), (10.0, 12.5)]);
        }
        Icon::Right => {
            line(&[(6.0, 3.5), (10.5, 8.0), (6.0, 12.5)]);
        }
        Icon::Plus => {
            line(&[(3.0, 8.0), (13.0, 8.0)]);
            line(&[(8.0, 3.0), (8.0, 13.0)]);
        }
        Icon::Minus => {
            line(&[(3.0, 8.0), (13.0, 8.0)]);
        }
        Icon::More => {
            for x in [3.5, 8.0, 12.5] {
                painter.circle_filled(p(x, 8.0), 1.3 * s, color);
            }
        }
        Icon::Moon => {
            // A crescent: the outer circle's arc between the points where it
            // meets a smaller circle, then back along that one.
            let mut pts = arc(8.0, 8.0, 6.0, 10.3, 259.7);
            pts.extend(arc(11.0, 5.0, 5.0, 215.5, 54.5));
            closed(&pts);
        }
        Icon::Pages => {
            // Two page thumbnails, one above the other.
            for y in [1.5, 9.0] {
                painter.rect_stroke(
                    Rect::from_min_max(p(4.0, y), p(12.0, y + 5.5)),
                    CornerRadius::same(s as u8),
                    stroke,
                    egui::StrokeKind::Middle,
                );
            }
        }
        Icon::Sections => {
            // A table of contents: headings with indented subheadings.
            for (x, y) in [(2.0, 3.0), (5.0, 6.5), (5.0, 10.0), (2.0, 13.5)] {
                painter.circle_filled(p(x, y), 0.9 * s, color);
                line(&[(x + 2.5, y), (14.0, y)]);
            }
        }
        Icon::Comment => {
            // A speech bubble.
            let mut pts = vec![(2.0, 3.5), (14.0, 3.5), (14.0, 11.0), (7.5, 11.0), (4.5, 14.0), (4.5, 11.0), (2.0, 11.0)];
            pts.dedup();
            closed(&pts);
            line(&[(5.0, 6.5), (11.0, 6.5)]);
            line(&[(5.0, 8.5), (9.0, 8.5)]);
        }
        Icon::Highlight => {
            // A marker over a line of text.
            closed(&[(6.0, 10.5), (12.0, 2.5), (14.5, 4.5), (8.5, 12.5)]);
            line(&[(6.0, 10.5), (4.5, 12.5), (6.5, 13.5), (8.5, 12.5)]);
            line(&[(1.5, 15.0), (14.5, 15.0)]);
        }
        Icon::RotateLeft | Icon::RotateRight => {
            // A circular arrow, open at the top, its head at the top end.
            let flip = |pts: Vec<(f32, f32)>| -> Vec<(f32, f32)> {
                if icon == Icon::RotateLeft { pts.into_iter().map(|(x, y)| (16.0 - x, y)).collect() } else { pts }
            };
            line(&flip(arc(8.0, 8.5, 5.5, -60.0, 210.0)));
            line(&flip(vec![(8.6, 1.6), (11.0, 3.7), (8.4, 5.6)]));
        }
        Icon::Present => {
            // A screen on a stand, with a play triangle.
            closed(&[(1.5, 2.5), (14.5, 2.5), (14.5, 11.5), (1.5, 11.5)]);
            line(&[(8.0, 11.5), (8.0, 14.5)]);
            line(&[(5.0, 14.5), (11.0, 14.5)]);
            painter.add(Shape::convex_polygon(vec![p(6.5, 4.8), p(10.5, 7.0), p(6.5, 9.2)], color, Stroke::NONE));
        }
        Icon::Trash => {
            // A bin with a lid.
            line(&[(2.5, 4.0), (13.5, 4.0)]);
            line(&[(6.0, 4.0), (6.5, 2.0), (9.5, 2.0), (10.0, 4.0)]);
            closed(&[(3.8, 4.0), (12.2, 4.0), (11.3, 14.0), (4.7, 14.0)]);
            line(&[(6.7, 6.8), (6.9, 11.5)]);
            line(&[(9.3, 6.8), (9.1, 11.5)]);
        }
        Icon::Document => {
            closed(&[(3.0, 1.0), (10.0, 1.0), (13.5, 4.5), (13.5, 15.0), (3.0, 15.0)]);
            line(&[(10.0, 1.0), (10.0, 4.5), (13.5, 4.5)]);
            for y in [7.5, 10.0, 12.5] {
                line(&[(5.5, y), (11.0, y)]);
            }
        }
    }
}

/// A frameless toolbar button: an icon, and a label when `label` is given.
/// Selected buttons (active modes) are tinted with the accent color.
pub fn button(ui: &mut egui::Ui, icon: Icon, label: Option<&str>, selected: bool, enabled: bool) -> egui::Response {
    let p = theme::palette(ui);
    let font = egui::TextStyle::Button.resolve(ui.style());
    let galley = label.map(|l| ui.painter().layout_no_wrap(l.to_string(), font, p.text));
    let height = 30.0;
    let width = height + galley.as_ref().map_or(0.0, |g| g.size().x + 6.0);
    let sense = if enabled { Sense::click() } else { Sense::hover() };
    let (rect, response) = ui.allocate_exact_size(Vec2::new(width, height), sense);
    if ui.is_rect_visible(rect) {
        let painter = ui.painter();
        let bg = if selected {
            p.accent_soft
        } else if enabled && response.is_pointer_button_down_on() {
            p.pressed
        } else if enabled && response.hovered() {
            p.hover
        } else {
            Color32::TRANSPARENT
        };
        painter.rect_filled(rect, CornerRadius::same(7), bg);
        let color = match (enabled, selected) {
            (false, _) => p.weak.gamma_multiply(0.5),
            (true, true) => p.accent,
            (true, false) => p.text,
        };
        let icon_rect = Rect::from_center_size(Pos2::new(rect.left() + height / 2.0, rect.center().y), Vec2::splat(16.0));
        paint(painter, icon_rect, icon, color);
        if let Some(g) = galley {
            let pos = Pos2::new(rect.left() + height - 4.0, rect.center().y - g.size().y / 2.0);
            painter.galley_with_override_text_color(pos, g, color);
        }
    }
    if enabled { response.on_hover_cursor(egui::CursorIcon::PointingHand) } else { response }
}

/// A group of related buttons on a shared rounded background.
pub fn group<R>(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
    let p = theme::palette(ui);
    egui::Frame::new()
        .fill(p.hover)
        .corner_radius(CornerRadius::same(9))
        .inner_margin(egui::Margin::same(2))
        .show(ui, |ui| {
            // Its own row, so the background hugs the buttons.
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 2.0;
                add(ui)
            })
            .inner
        })
        .inner
}
