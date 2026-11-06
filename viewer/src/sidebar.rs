//! The page sidebar: a scrollable column of page thumbnails. The current
//! page is outlined and kept in view; clicking a thumbnail goes to it, and
//! dragging one moves the page. Hovering the gap between two thumbnails (or
//! above the first, or below the last) shows a "+" button for inserting the
//! pages of another PDF there.
//!
//! Pages are identified two ways: a *page* is its index in the file (what
//! thumbnails and renders are keyed by), a *position* is where it currently
//! sits in the (possibly rearranged) order.
//!
//! Thumbnails are rendered in the background only for the part of the list
//! on screen, after the main view's pages, and the least recently shown
//! ones are dropped beyond a fixed count.

use std::collections::HashMap;

use eframe::egui::{
    self, Align, Color32, CursorIcon, FontId, Id, LayerId, Order, Pos2, Rect, Sense, Stroke, StrokeKind,
    TextureHandle, Vec2,
};

use crate::worker::Pool;

/// Space left and right of the thumbnails.
const PAD: f32 = 12.0;
/// Vertical space between one page's number and the next thumbnail (room
/// for the insert button).
const GAP: f32 = 22.0;
/// Height of the page number under each thumbnail.
const LABEL_HEIGHT: f32 = 18.0;
/// Thumbnails kept in memory (about 100 KB each at the default width).
const MAX_THUMBS: usize = 400;
/// A thumbnail this much narrower or wider than wanted is re-rendered.
const RESIZE_TOLERANCE: f32 = 0.15;
/// Half the height of the area around a gap that shows the insert button.
const GAP_ZONE: f32 = 8.0;
/// While dragging, the list scrolls when the pointer is this close to its
/// top or bottom edge.
const AUTOSCROLL_ZONE: f32 = 40.0;

pub const DEFAULT_WIDTH: f32 = 170.0;

/// What the user did in the sidebar.
pub enum Action {
    /// Clicked the thumbnail at this position.
    GoTo(usize),
    /// Dragged the page at position `from` so it now sits at position `to`.
    Move { from: usize, to: usize },
    /// Clicked the insert button in the gap before this position (the
    /// number of pages for the gap after the last one).
    Insert(usize),
}

struct Thumb {
    texture: TextureHandle,
    last_used: u64,
}

#[derive(Default)]
pub struct Sidebar {
    thumbs: HashMap<usize, Thumb>,
    /// Last thumbnail request sent, to avoid resending it every frame.
    last_want: Vec<(usize, f32)>,
    /// The current position as of the last frame; when it changes the list
    /// scrolls to keep it visible.
    shown_position: Option<usize>,
    /// Position of the thumbnail being dragged.
    dragging: Option<usize>,
    frame: u64,
}

impl Sidebar {
    /// Forgets everything (a new document was opened).
    pub fn clear(&mut self) {
        *self = Sidebar::default();
    }

    /// Whether requested thumbnails are still being rendered.
    pub fn pending(&self) -> bool {
        self.last_want.iter().any(|(p, _)| !self.thumbs.contains_key(p))
    }

    /// Drops a page's thumbnail so it is rendered again (after an edit).
    pub fn invalidate(&mut self, page: usize) {
        self.thumbs.remove(&page);
        self.last_want.clear();
    }

    pub fn insert(&mut self, ctx: &egui::Context, page: usize, image: egui::ColorImage) {
        let texture = ctx.load_texture(format!("thumb-{page}"), image, egui::TextureOptions::LINEAR);
        self.thumbs.insert(page, Thumb { texture, last_used: self.frame });
    }

    /// Draws the sidebar. `sizes` are the displayed page sizes in points,
    /// indexed by page; `order` lists the pages by position.
    pub fn show(
        &mut self,
        ui: &mut egui::Ui,
        sizes: &[Vec2],
        order: &[usize],
        current: usize,
        pool: &Pool,
    ) -> Option<Action> {
        self.frame += 1;
        let ppp = ui.ctx().pixels_per_point();
        let width = ui.available_width();
        let thumb_w = (width - 2.0 * PAD).max(40.0);

        // Vertical layout by position: thumbnail heights follow the page shapes.
        let heights: Vec<f32> =
            order.iter().map(|&p| (thumb_w * sizes[p].y / sizes[p].x.max(1.0)).min(thumb_w * 3.0)).collect();
        let mut tops = Vec::with_capacity(order.len());
        let mut y = GAP;
        for h in &heights {
            tops.push(y);
            y += h + LABEL_HEIGHT + GAP;
        }
        let total = y;

        let mut action = None;
        let mut on_screen: Vec<usize> = Vec::new();
        let follow = self.shown_position != Some(current) && self.dragging.is_none();
        self.shown_position = Some(current);

        let (pointer, primary_down, escape) =
            ui.input(|i| (i.pointer.latest_pos(), i.pointer.primary_down(), i.key_pressed(egui::Key::Escape)));
        if escape {
            self.dragging = None;
        }

        let palette = crate::theme::palette(ui);
        let accent = palette.accent;
        let hover_fill = palette.hover;
        let text_color = palette.text;
        let shadow = egui::epaint::Shadow { offset: [0, 1], blur: 6, spread: 0, color: palette.shadow };
        egui::ScrollArea::vertical().id_salt("thumbnails").auto_shrink(false).show_viewport(ui, |ui, viewport| {
            ui.set_min_size(Vec2::new(width, total));
            if order.is_empty() {
                return;
            }
            let origin = ui.max_rect().min;
            let painter = ui.painter().clone();
            let screen_view = viewport.translate(origin.to_vec2());
            // The gap before position i is at the top of its item; the one
            // after the last page at the bottom of the list.
            let gap_y = |i: usize| origin.y + if i < order.len() { tops[i] } else { total } - GAP / 2.0;
            let hovered_gap = match pointer {
                Some(p) if self.dragging.is_none() && ui.rect_contains_pointer(screen_view) => {
                    let below = tops.partition_point(|&t| origin.y + t - GAP / 2.0 <= p.y);
                    [below.saturating_sub(1), below]
                        .into_iter()
                        .filter(|&i| i <= order.len())
                        .find(|&i| (gap_y(i) - p.y).abs() <= GAP_ZONE)
                }
                _ => None,
            };
            let page_rect = |pos: usize| {
                Rect::from_min_size(
                    origin + Vec2::new((width - thumb_w) / 2.0, tops[pos]),
                    Vec2::new(thumb_w, heights[pos]),
                )
            };

            for (pos, &page) in order.iter().enumerate() {
                let item = Rect::from_min_size(
                    origin + Vec2::new(0.0, tops[pos] - GAP / 2.0),
                    Vec2::new(width, heights[pos] + LABEL_HEIGHT + GAP),
                );
                let rect = page_rect(pos);
                if pos == current && follow && !screen_view.contains_rect(rect) {
                    ui.scroll_to_rect(item, Some(Align::Center));
                }
                // Only the thumbnails on screen are drawn and interactive.
                if !item.intersects(screen_view) {
                    continue;
                }
                on_screen.push(pos);

                let response = ui.interact(item, ui.id().with(("thumb", page)), Sense::click_and_drag());
                if self.dragging.is_none() && hovered_gap.is_none() {
                    if response.hovered() {
                        painter.rect_filled(item.shrink(2.0), 8.0, hover_fill);
                        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
                    }
                    if response.clicked() {
                        action = Some(Action::GoTo(pos));
                    }
                    if response.drag_started() {
                        self.dragging = Some(pos);
                    }
                }

                // The page being dragged stays in place, dimmed.
                let tint = if self.dragging == Some(pos) { Color32::from_white_alpha(70) } else { Color32::WHITE };
                painter.add(shadow.as_shape(rect, 1));
                painter.rect_filled(rect, 0.0, tint);
                if let Some(t) = self.thumbs.get_mut(&page) {
                    t.last_used = self.frame;
                    painter.image(t.texture.id(), rect, full_uv(), tint);
                }
                if pos == current {
                    painter.rect_stroke(rect.expand(2.0), 4.0, Stroke::new(2.0, accent), StrokeKind::Outside);
                }
                // Page number; the current page's on an accent pill.
                let number = painter.layout_no_wrap((pos + 1).to_string(), FontId::proportional(11.5), text_color);
                let at = Pos2::new(rect.center().x, rect.bottom() + 5.0 + number.size().y / 2.0);
                if pos == current {
                    let pill = Rect::from_center_size(at, number.size() + Vec2::new(14.0, 3.0));
                    painter.rect_filled(pill, 8.0, accent);
                    painter.galley_with_override_text_color(at - number.size() / 2.0, number, palette.on_accent);
                } else {
                    painter.galley_with_override_text_color(at - number.size() / 2.0, number, palette.weak);
                }
            }

            // The insert button in the hovered gap (on top of the thumbnails).
            if let Some(i) = hovered_gap {
                let y = gap_y(i);
                let zone = Rect::from_center_size(Pos2::new(origin.x + width / 2.0, y), Vec2::new(width, 2.0 * GAP_ZONE));
                let response = ui
                    .interact(zone, ui.id().with(("insert", i)), Sense::click())
                    .on_hover_text("Insert pages from another PDF here");
                let x = origin.x + (width - thumb_w) / 2.0;
                painter.line_segment([Pos2::new(x, y), Pos2::new(x + thumb_w, y)], Stroke::new(2.0, accent));
                let center = Pos2::new(origin.x + width / 2.0, y);
                painter.circle_filled(center, 9.0, accent);
                let plus = Stroke::new(2.0, Color32::WHITE);
                painter.line_segment([center - Vec2::new(4.5, 0.0), center + Vec2::new(4.5, 0.0)], plus);
                painter.line_segment([center - Vec2::new(0.0, 4.5), center + Vec2::new(0.0, 4.5)], plus);
                ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
                if response.clicked() {
                    action = Some(Action::Insert(i));
                }
            }

            // Dragging: show where the page would go, and drop it there.
            let (Some(from), Some(p)) = (self.dragging, pointer) else { return };
            // Insertion point: before the first thumbnail whose middle is
            // below the pointer.
            let insert_at = (0..order.len()).find(|&pos| page_rect(pos).center().y > p.y).unwrap_or(order.len());
            if !primary_down {
                self.dragging = None;
                let to = if insert_at > from { insert_at - 1 } else { insert_at };
                if to != from {
                    action = Some(Action::Move { from, to });
                }
                return;
            }
            ui.ctx().set_cursor_icon(CursorIcon::Grabbing);
            let line_y = match insert_at {
                0 => page_rect(0).top() - GAP / 2.0,
                n => page_rect(n - 1).bottom() + LABEL_HEIGHT + GAP / 2.0,
            };
            let x = origin.x + (width - thumb_w) / 2.0;
            painter.line_segment(
                [Pos2::new(x - 6.0, line_y), Pos2::new(x + thumb_w + 6.0, line_y)],
                Stroke::new(3.0, accent),
            );
            // A translucent copy of the thumbnail follows the pointer.
            let ghost = Rect::from_center_size(p, page_rect(from).size() * 0.75);
            let layer = ui.ctx().layer_painter(LayerId::new(Order::Tooltip, Id::new("thumb-drag")));
            let alpha = Color32::from_white_alpha(190);
            layer.rect_filled(ghost.translate(Vec2::new(2.0, 3.0)), 2.0, Color32::from_black_alpha(80));
            layer.rect_filled(ghost, 0.0, alpha);
            if let Some(t) = self.thumbs.get(&order[from]) {
                layer.image(t.texture.id(), ghost, full_uv(), alpha);
            }
            // Scroll when the pointer nears the top or bottom edge.
            let speed = if p.y < screen_view.top() + AUTOSCROLL_ZONE {
                (screen_view.top() + AUTOSCROLL_ZONE - p.y).min(AUTOSCROLL_ZONE) / 2.0
            } else if p.y > screen_view.bottom() - AUTOSCROLL_ZONE {
                -(p.y - (screen_view.bottom() - AUTOSCROLL_ZONE)).min(AUTOSCROLL_ZONE) / 2.0
            } else {
                0.0
            };
            if speed != 0.0 {
                ui.scroll_with_delta(Vec2::new(0.0, speed));
            }
            ui.ctx().request_repaint();
        });

        self.request(sizes, order, &on_screen, thumb_w * ppp, pool);
        action
    }

    /// Asks for the thumbnails on screen (and one more on each side) that
    /// are missing or rendered at a clearly different size.
    fn request(&mut self, sizes: &[Vec2], order: &[usize], on_screen: &[usize], width_px: f32, pool: &Pool) {
        let (Some(&first), Some(&last)) = (on_screen.first(), on_screen.last()) else { return };
        let positions = first.saturating_sub(1)..(last + 2).min(order.len());
        let want: Vec<(usize, f32)> = positions
            .map(|pos| order[pos])
            .filter(|p| match self.thumbs.get(p) {
                None => true,
                Some(t) => {
                    let w = t.texture.size()[0] as f32;
                    // Blank placeholders (failed pages) are 1 px wide; keep them.
                    w > 1.0 && (w / width_px - 1.0).abs() > RESIZE_TOLERANCE
                }
            })
            .map(|p| (p, width_px / sizes[p].x.max(1.0)))
            .collect();
        if want != self.last_want {
            pool.want_thumbs(&want);
            self.last_want = want;
        }

        if self.thumbs.len() > MAX_THUMBS {
            let mut by_age: Vec<(u64, usize)> = self.thumbs.iter().map(|(&p, t)| (t.last_used, p)).collect();
            by_age.sort_unstable();
            for (_, p) in by_age.into_iter().take(self.thumbs.len() - MAX_THUMBS) {
                self.thumbs.remove(&p);
            }
        }
    }
}

fn full_uv() -> Rect {
    Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0))
}

/// Moves the item at `from` to index `to` (both within `order`).
pub fn move_item(order: &mut Vec<usize>, from: usize, to: usize) {
    let item = order.remove(from);
    order.insert(to.min(order.len()), item);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn moving_pages() {
        let mut order = vec![0, 1, 2, 3];
        move_item(&mut order, 0, 2);
        assert_eq!(order, [1, 2, 0, 3]);
        move_item(&mut order, 3, 0);
        assert_eq!(order, [3, 1, 2, 0]);
        move_item(&mut order, 1, 9);
        assert_eq!(order, [3, 2, 0, 1]);
    }
}
