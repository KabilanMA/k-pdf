//! Crop mode: choosing the part of a page to keep.
//!
//! Dragging on a page draws the crop area (the rest of the page is
//! dimmed); its corners and edges can then be dragged to adjust it, and
//! dragging inside moves it. A panel offers cropping this page, or every
//! page to the same area, and removing crops.

use eframe::egui::{self, Color32, CursorIcon, Id, Pos2, Rect, Sense, Stroke, StrokeKind, Vec2};

/// What the user asked for in the crop panel.
pub enum Action {
    /// Crop one page (index into the open file's pages) to `rect`
    /// (display points).
    Page { page: usize, rect: [f32; 4] },
    /// Crop every page to the same area.
    All { rect: [f32; 4] },
    /// Crop some pages (positions in the view, 0-based) to the same area.
    Pages { positions: Vec<usize>, rect: [f32; 4] },
    /// Show the whole page again.
    ResetPage { page: usize },
    ResetAll,
    /// Leave crop mode.
    Done,
}

#[derive(Default)]
pub struct Crop {
    session: Option<Session>,
    /// The "pages" field: which pages to crop, e.g. "1, 3-5".
    pages: String,
    /// Why the pages field can't be used, if it can't.
    pages_error: Option<String>,
}

struct Session {
    /// Index into the open file's pages, and its position in the view.
    page: usize,
    position: usize,
    /// The area to keep, in display points of the page.
    rect: Rect,
    drag: Option<Drag>,
}

#[derive(Clone, Copy)]
struct Drag {
    /// Which edges move: left, top, right, bottom (all four: moving).
    edges: [bool; 4],
    new: bool,
    start: Pos2,
    start_rect: Rect,
}

/// Grab distance for edges and corners, in screen points.
const GRAB: f32 = 8.0;
/// Smallest crop, in points.
const MIN_SIZE: f32 = 10.0;

impl Crop {
    pub fn clear(&mut self) {
        self.session = None;
    }

    /// Draws crop mode over a page and handles dragging on it. `page`
    /// indexes the open file's pages, `position` is its place in the view.
    pub fn show_page(&mut self, ui: &mut egui::Ui, page: usize, position: usize, page_rect: Rect, zoom: f32) {
        let to_screen = |p: Pos2| page_rect.min + p.to_vec2() * zoom;
        let to_page = |p: Pos2| ((p - page_rect.min) / zoom).to_pos2();
        let bounds = Rect::from_min_size(Pos2::ZERO, page_rect.size() / zoom);
        let response = ui.interact(page_rect, ui.id().with(("crop-page", position)), Sense::click_and_drag());
        let here = self.session.as_ref().is_some_and(|s| s.position == position);

        // Which part of the crop area the pointer is on.
        let grab = |rect: Rect, p: Pos2| -> Option<[bool; 4]> {
            let r = Rect::from_min_max(to_screen(rect.min), to_screen(rect.max));
            if !r.expand(GRAB).contains(p) {
                return None;
            }
            let edges = [
                (p.x - r.left()).abs() < GRAB,
                (p.y - r.top()).abs() < GRAB,
                (p.x - r.right()).abs() < GRAB,
                (p.y - r.bottom()).abs() < GRAB,
            ];
            if edges.iter().any(|&e| e) {
                Some(edges)
            } else if r.contains(p) {
                Some([true; 4])
            } else {
                None
            }
        };

        if let Some(p) = response.hover_pos() {
            let icon = match self.session.as_ref().filter(|_| here).and_then(|s| grab(s.rect, p)) {
                Some([true, true, true, true]) => CursorIcon::Grab,
                Some([l, t, r, b]) if (l && t) || (r && b) => CursorIcon::ResizeNwSe,
                Some([l, t, r, b]) if (r && t) || (l && b) => CursorIcon::ResizeNeSw,
                Some([l, _, r, _]) if l || r => CursorIcon::ResizeHorizontal,
                Some(_) => CursorIcon::ResizeVertical,
                None => CursorIcon::Crosshair,
            };
            ui.ctx().set_cursor_icon(icon);
        }

        // Drags are recognised after a few points of movement: start from
        // where the button went down.
        let press = ui.input(|i| i.pointer.press_origin());
        if response.drag_started()
            && let Some(p) = press.or(response.interact_pointer_pos())
        {
            let existing = self.session.as_ref().filter(|_| here).and_then(|s| grab(s.rect, p).map(|e| (e, s.rect)));
            let (edges, rect, new) = match existing {
                Some((edges, rect)) => (edges, rect, false),
                None => {
                    let start = to_page(p).clamp(bounds.min, bounds.max);
                    ([false, false, true, true], Rect::from_min_max(start, start), true)
                }
            };
            self.session = Some(Session {
                page,
                position,
                rect,
                drag: Some(Drag { edges, new, start: to_page(p), start_rect: rect }),
            });
        }
        if let Some(s) = self.session.as_mut().filter(|s| s.position == position) {
            if let (Some(drag), Some(p)) = (s.drag, response.interact_pointer_pos()) {
                let p = to_page(p);
                let d = p - drag.start;
                let r = drag.start_rect;
                s.rect = if drag.new {
                    Rect::from_two_pos(r.min, p.clamp(bounds.min, bounds.max))
                } else if drag.edges == [true; 4] {
                    // Move, keeping inside the page.
                    let mut moved = r.translate(d);
                    moved = moved.translate(Vec2::new(
                        (bounds.left() - moved.left()).max(0.0) + (bounds.right() - moved.right()).min(0.0),
                        (bounds.top() - moved.top()).max(0.0) + (bounds.bottom() - moved.bottom()).min(0.0),
                    ));
                    moved
                } else {
                    let [l, t, rt, b] = drag.edges;
                    let mut n = r;
                    if l {
                        n.min.x = (r.min.x + d.x).clamp(0.0, r.max.x - MIN_SIZE);
                    }
                    if t {
                        n.min.y = (r.min.y + d.y).clamp(0.0, r.max.y - MIN_SIZE);
                    }
                    if rt {
                        n.max.x = (r.max.x + d.x).clamp(r.min.x + MIN_SIZE, bounds.right());
                    }
                    if b {
                        n.max.y = (r.max.y + d.y).clamp(r.min.y + MIN_SIZE, bounds.bottom());
                    }
                    n
                };
            }
            if response.drag_stopped() {
                s.drag = None;
                // A click without dragging leaves nothing to crop.
                if s.rect.width() < MIN_SIZE || s.rect.height() < MIN_SIZE {
                    self.session = None;
                    return;
                }
            }

            // Dim what will be cut off, outline the rest, show the handles.
            let keep = Rect::from_min_max(to_screen(s.rect.min), to_screen(s.rect.max));
            let shade = Color32::from_black_alpha(140);
            let painter = ui.painter();
            for r in [
                Rect::from_min_max(page_rect.min, Pos2::new(page_rect.right(), keep.top())),
                Rect::from_min_max(Pos2::new(page_rect.left(), keep.bottom()), page_rect.max),
                Rect::from_min_max(Pos2::new(page_rect.left(), keep.top()), Pos2::new(keep.left(), keep.bottom())),
                Rect::from_min_max(Pos2::new(keep.right(), keep.top()), Pos2::new(page_rect.right(), keep.bottom())),
            ] {
                if r.is_positive() {
                    painter.rect_filled(r, 0.0, shade);
                }
            }
            let accent = Color32::from_rgb(40, 120, 230);
            painter.rect_stroke(keep, 0.0, Stroke::new(1.5, accent), StrokeKind::Outside);
            for c in [keep.left_top(), keep.right_top(), keep.left_bottom(), keep.right_bottom(),
                keep.center_top(), keep.center_bottom(), keep.left_center(), keep.right_center()]
            {
                painter.rect_filled(Rect::from_center_size(c, Vec2::splat(8.0)), 1.0, Color32::WHITE);
                painter.rect_stroke(Rect::from_center_size(c, Vec2::splat(8.0)), 1.0, Stroke::new(1.0, accent), StrokeKind::Inside);
            }
        }
    }

    /// The crop panel, shown at the top of the page view.
    /// `count` is the number of pages in the view.
    pub fn panel(&mut self, ctx: &egui::Context, view: Rect, count: usize) -> Option<Action> {
        let mut action = None;
        let mut cancel = false;
        egui::Area::new(Id::new("crop-panel"))
            .order(egui::Order::Foreground)
            .fixed_pos(Pos2::new(view.center().x, view.top() + 8.0))
            .pivot(egui::Align2::CENTER_TOP)
            .show(ctx, |ui| {
                crate::theme::floating_frame(ui).show(ui, |ui| {
                    // Wraps onto more rows in narrow windows.
                    ui.set_max_width((view.width() - 24.0).max(200.0));
                    ui.horizontal_wrapped(|ui| match &self.session {
                        None => {
                            ui.label("Drag on a page to choose the area to keep.");
                            ui.separator();
                            if ui.button("Remove all crops").on_hover_text("Show every page in full again").clicked() {
                                action = Some(Action::ResetAll);
                            }
                            if ui.button("Done").clicked() {
                                action = Some(Action::Done);
                            }
                        }
                        Some(s) => {
                            let mm = |v: f32| v * 25.4 / 72.0;
                            ui.label(format!(
                                "Page {}: keep {:.0} × {:.0} mm",
                                s.position + 1,
                                mm(s.rect.width()),
                                mm(s.rect.height())
                            ));
                            ui.separator();
                            let rect = [s.rect.left(), s.rect.top(), s.rect.right(), s.rect.bottom()];
                            if crate::theme::primary_button(ui, "Crop this page").clicked() {
                                action = Some(Action::Page { page: s.page, rect });
                            }
                            if ui
                                .button("Crop all pages")
                                .on_hover_text("Crop every page to the same area (measured from each page's top-left corner)")
                                .clicked()
                            {
                                action = Some(Action::All { rect });
                            }
                            ui.separator();
                            ui.label("Pages:");
                            let field = ui.add(
                                egui::TextEdit::singleline(&mut self.pages)
                                    .hint_text("e.g. 1, 3-5")
                                    .desired_width(90.0),
                            );
                            let field = match &self.pages_error {
                                Some(e) => field.on_hover_text(e.as_str()),
                                None => field.on_hover_text("Page numbers and ranges to crop to this area"),
                            };
                            let enter = field.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                            if ui.button("Crop these pages").clicked() || enter {
                                match parse_pages(&self.pages, count) {
                                    Ok(positions) => {
                                        self.pages_error = None;
                                        action = Some(Action::Pages { positions, rect });
                                    }
                                    Err(e) => self.pages_error = Some(e),
                                }
                            }
                            if let Some(e) = &self.pages_error {
                                ui.colored_label(Color32::from_rgb(220, 80, 80), e);
                            }
                            ui.separator();
                            if ui.button("Remove crop").on_hover_text("Show this page in full again").clicked() {
                                action = Some(Action::ResetPage { page: s.page });
                            }
                            if ui.button("Cancel").on_hover_text("Choose another area").clicked() {
                                cancel = true;
                            }
                        }
                    });
                });
            });
        if action.is_some() || cancel {
            self.session = None;
        }
        action
    }
}

/// Parses page numbers and ranges ("1, 3-5, 8"; 1-based, as shown) into
/// 0-based positions, in order and without repeats.
pub fn parse_pages(spec: &str, count: usize) -> Result<Vec<usize>, String> {
    let mut out: Vec<usize> = Vec::new();
    let page = |s: &str| -> Result<usize, String> {
        match s.trim().parse::<usize>() {
            Ok(n) if (1..=count).contains(&n) => Ok(n - 1),
            Ok(n) => Err(format!("there is no page {n} (the document has {count})")),
            Err(_) => Err(format!("\"{}\" isn't a page number", s.trim())),
        }
    };
    for part in spec.split([',', ';', ' ']).map(str::trim).filter(|p| !p.is_empty()) {
        let (a, b) = match part.split_once(['-', '–']) {
            Some((a, b)) => (page(a)?, page(b)?),
            None => {
                let p = page(part)?;
                (p, p)
            }
        };
        for p in a.min(b)..=a.max(b) {
            if !out.contains(&p) {
                out.push(p);
            }
        }
    }
    if out.is_empty() {
        return Err("enter page numbers, e.g. 1, 3-5".into());
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::parse_pages;

    #[test]
    fn page_lists() {
        assert_eq!(parse_pages("1, 3-5, 8", 10), Ok(vec![0, 2, 3, 4, 7]));
        assert_eq!(parse_pages("5-3 4", 10), Ok(vec![2, 3, 4]));
        assert_eq!(parse_pages(" 2 ", 2), Ok(vec![1]));
        assert!(parse_pages("11", 10).is_err());
        assert!(parse_pages("a-3", 10).is_err());
        assert!(parse_pages("", 10).is_err());
    }
}
