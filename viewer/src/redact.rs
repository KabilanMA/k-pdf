//! Redact mode: marking what to remove for good.
//!
//! Dragging on a page marks an area (shown in red); clicking a mark
//! removes it. The panel can also mark every occurrence of some words.
//! "Apply" removes the text, image pixels, shapes and annotations under
//! the marks and draws black boxes in their place (see
//! `k_pdf::edit::redact_page`); like other edits it can be undone until
//! the file is saved.

use eframe::egui::{self, Color32, CursorIcon, Id, Pos2, Rect, Sense, Stroke, StrokeKind};

/// What the user asked for in the redact panel.
pub enum Action {
    /// Redact the marked areas.
    Apply,
    /// Mark every occurrence of these words.
    Find(String),
    /// Leave redact mode.
    Done,
}

/// An area to redact, in display points of a page (index into the open
/// file's pages).
#[derive(Clone, Copy, PartialEq)]
pub struct Mark {
    pub page: usize,
    pub rect: Rect,
}

#[derive(Default)]
pub struct Redact {
    pub marks: Vec<Mark>,
    /// A mark being drawn: page position in the view, page, start point.
    drag: Option<(usize, usize, Pos2)>,
    find: String,
    /// Shown in the panel: what the last action did.
    pub message: Option<String>,
}

/// Marks smaller than this (points) are taken as clicks.
const MIN_SIZE: f32 = 2.0;
const MARK_FILL: Color32 = Color32::from_rgba_premultiplied(60, 8, 8, 60);
const MARK_STROKE: Color32 = Color32::from_rgb(220, 40, 40);

impl Redact {
    /// Adds marks, leaving out ones already there. Returns how many were new.
    pub fn add(&mut self, marks: impl IntoIterator<Item = Mark>) -> usize {
        let before = self.marks.len();
        for m in marks {
            if !self.marks.contains(&m) {
                self.marks.push(m);
            }
        }
        self.marks.len() - before
    }

    /// Draws the marks on a page and handles marking on it. `page` indexes
    /// the open file's pages, `position` is its place in the view.
    pub fn show_page(&mut self, ui: &mut egui::Ui, page: usize, position: usize, page_rect: Rect, zoom: f32) {
        let to_screen = |r: Rect| Rect::from_min_max(page_rect.min + r.min.to_vec2() * zoom, page_rect.min + r.max.to_vec2() * zoom);
        let to_page = |p: Pos2| ((p - page_rect.min) / zoom).to_pos2();
        let bounds = Rect::from_min_size(Pos2::ZERO, page_rect.size() / zoom);
        let response = ui.interact(page_rect, ui.id().with(("redact-page", position)), Sense::click_and_drag());

        let mark_at = |p: Pos2| self.marks.iter().rposition(|m| m.page == page && to_screen(m.rect).contains(p));
        let hovered = response.hover_pos().and_then(mark_at);
        if response.hovered() {
            ui.ctx().set_cursor_icon(if hovered.is_some() { CursorIcon::PointingHand } else { CursorIcon::Crosshair });
        }
        if response.clicked()
            && let Some(i) = response.interact_pointer_pos().and_then(mark_at)
        {
            self.marks.remove(i);
        }

        // Drags are recognised after a few points of movement: start from
        // where the button went down.
        let press = ui.input(|i| i.pointer.press_origin());
        if response.drag_started()
            && let Some(p) = press.or(response.interact_pointer_pos())
        {
            self.drag = Some((position, page, to_page(p).clamp(bounds.min, bounds.max)));
        }
        let mut current = None;
        if let Some((pos, _, start)) = self.drag
            && pos == position
            && let Some(p) = response.interact_pointer_pos()
        {
            current = Some(Rect::from_two_pos(start, to_page(p).clamp(bounds.min, bounds.max)));
        }
        if response.drag_stopped() && self.drag.is_some_and(|(pos, ..)| pos == position) {
            self.drag = None;
            if let Some(rect) = current.take().filter(|r| r.width() >= MIN_SIZE && r.height() >= MIN_SIZE) {
                self.marks.push(Mark { page, rect });
                self.message = None;
            }
        }

        let painter = ui.painter();
        for (i, m) in self.marks.iter().enumerate().filter(|(_, m)| m.page == page) {
            let r = to_screen(m.rect);
            painter.rect_filled(r, 0.0, MARK_FILL);
            let width = if hovered == Some(i) { 2.5 } else { 1.5 };
            painter.rect_stroke(r, 0.0, Stroke::new(width, MARK_STROKE), StrokeKind::Outside);
        }
        if let Some(r) = current {
            painter.rect_filled(to_screen(r), 0.0, MARK_FILL);
            painter.rect_stroke(to_screen(r), 0.0, Stroke::new(1.5, MARK_STROKE), StrokeKind::Outside);
        }
        if hovered.is_some() && self.drag.is_none() {
            response.on_hover_text("Click to unmark");
        }
    }

    /// The redact panel, shown at the top of the page view.
    pub fn panel(&mut self, ctx: &egui::Context, view: Rect) -> Option<Action> {
        let mut action = None;
        egui::Area::new(Id::new("redact-panel"))
            .order(egui::Order::Foreground)
            .fixed_pos(Pos2::new(view.center().x, view.top() + 8.0))
            .pivot(egui::Align2::CENTER_TOP)
            .show(ctx, |ui| {
                crate::theme::floating_frame(ui).show(ui, |ui| {
                    // Wraps onto more rows in narrow windows.
                    ui.set_max_width((view.width() - 24.0).max(200.0));
                    ui.horizontal_wrapped(|ui| {
                        ui.label("Drag over what to hide, or find:");
                        let field = ui
                            .add(egui::TextEdit::singleline(&mut self.find).hint_text("words to redact").desired_width(140.0))
                            .on_hover_text("Marks every place these words appear (ignoring case)");
                        let enter = field.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                        if (ui.add_enabled(!self.find.trim().is_empty(), egui::Button::new("Mark all")).clicked() || enter)
                            && !self.find.trim().is_empty()
                        {
                            action = Some(Action::Find(self.find.trim().to_string()));
                        }
                        ui.separator();
                        let n = self.marks.len();
                        ui.label(match n {
                            0 => "Nothing marked".to_string(),
                            1 => "1 area marked".to_string(),
                            n => format!("{n} areas marked"),
                        });
                        if crate::theme::danger_button(ui, "Redact", n > 0)
                            .on_hover_text(
                                "Remove the text, images, shapes and links under the marks for good and black them out \
                                 (Ctrl+Z undoes it until you save)",
                            )
                            .clicked()
                        {
                            action = Some(Action::Apply);
                        }
                        if ui.add_enabled(n > 0, egui::Button::new("Clear marks")).clicked() {
                            self.marks.clear();
                            self.message = None;
                        }
                        if ui.button("Done").clicked() {
                            action = Some(Action::Done);
                        }
                        if let Some(m) = &self.message {
                            ui.end_row();
                            ui.weak(m.as_str());
                        }
                    });
                });
            });
        action
    }
}
