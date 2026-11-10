//! Form mode: making a form by placing fields on the pages.
//!
//! Pick a kind of field in the panel, then drag on a page where it goes
//! (or click, for the usual size). Clicking a field selects it: drag it to
//! move it, drag a corner to resize it, and change its name and settings in
//! the panel; Delete removes it. Each change is an edit like any other
//! (see `k_pdf::edit::fields`), undone with Ctrl+Z until the file is saved.
//! Leaving form mode, the new fields can be filled in at once.

use eframe::egui::{self, Color32, CursorIcon, Id, Pos2, Rect, Sense, Stroke, StrokeKind, Vec2};
use k_pdf::edit::fields::{FieldSpec, NewField};
use k_pdf::{FieldKind, FormField, ObjRef};

/// What clicking or dragging on a page makes.
#[derive(Clone, Copy, PartialEq)]
pub enum Tool {
    /// Selecting, moving and resizing fields only.
    Select,
    Text,
    Paragraph,
    CheckBox,
    Dropdown,
}

impl Tool {
    const ALL: [Tool; 5] = [Tool::Select, Tool::Text, Tool::Paragraph, Tool::CheckBox, Tool::Dropdown];

    fn label(self) -> &'static str {
        match self {
            Tool::Select => "Select",
            Tool::Text => "Text box",
            Tool::Paragraph => "Paragraph",
            Tool::CheckBox => "Check box",
            Tool::Dropdown => "Drop-down",
        }
    }

    fn tip(self) -> &'static str {
        match self {
            Tool::Select => "Select fields to move, resize or change them",
            Tool::Text => "A box for one line of text: a name, a date, an amount",
            Tool::Paragraph => "A box for several lines of text, wrapped",
            Tool::CheckBox => "A box to tick",
            Tool::Dropdown => "A list to choose one answer from",
        }
    }

    /// The field it makes: kind, and whether it has several lines.
    fn makes(self) -> Option<(NewField, bool)> {
        match self {
            Tool::Select => None,
            Tool::Text => Some((NewField::Text, false)),
            Tool::Paragraph => Some((NewField::Text, true)),
            Tool::CheckBox => Some((NewField::CheckBox, false)),
            Tool::Dropdown => Some((NewField::Dropdown, false)),
        }
    }

    /// The size a click makes (points).
    fn usual_size(self) -> Vec2 {
        match self {
            Tool::CheckBox => Vec2::splat(14.0),
            Tool::Paragraph => Vec2::new(260.0, 72.0),
            Tool::Dropdown => Vec2::new(150.0, 22.0),
            _ => Vec2::new(180.0, 22.0),
        }
    }
}

/// What the user asked for.
pub enum Action {
    /// Make a field on `page` (of the open file) at `rect` (display points).
    Add { page: usize, rect: [f32; 4], kind: NewField, multiline: bool },
    /// Put a field's widget at `rect` on its page.
    Move { page: usize, widget: ObjRef, rect: [f32; 4] },
    /// Change a field's settings.
    Update { page: usize, id: ObjRef, spec: FieldSpec },
    Delete { page: usize, id: ObjRef },
    /// Leave form mode.
    Done,
}

/// A drag in progress, by the position of the page it started on.
enum Drag {
    New { position: usize, page: usize, start: Pos2 },
    Move { position: usize, widget: ObjRef, start: Pos2, rect: Rect },
    /// Moving corner `corner` (0 top left, then clockwise) of `rect`.
    Resize { position: usize, widget: ObjRef, corner: usize, rect: Rect },
}

pub struct Designer {
    pub tool: Tool,
    /// The selected field, and the widget of it that was clicked.
    selected: Option<(ObjRef, ObjRef)>,
    drag: Option<Drag>,
    /// The selected field's settings as being edited, and the field they
    /// were read from.
    spec: FieldSpec,
    options: String,
    spec_of: Option<ObjRef>,
    /// Shown in the panel: why the last change couldn't be made.
    pub message: Option<String>,
}

impl Default for Designer {
    fn default() -> Self {
        Designer { tool: Tool::Text, selected: None, drag: None, spec: FieldSpec::default(), options: String::new(), spec_of: None, message: None }
    }
}

/// Fields smaller than this (points) are taken as clicks.
const MIN_SIZE: f32 = 4.0;
/// How near a corner (screen points) grabs it.
const HANDLE: f32 = 7.0;

fn field_color() -> Color32 {
    crate::theme::on_page(Color32::from_rgb(37, 99, 235))
}

fn rect_of(r: [f32; 4]) -> Rect {
    Rect::from_min_max(Pos2::new(r[0], r[1]), Pos2::new(r[2], r[3]))
}

fn array_of(r: Rect) -> [f32; 4] {
    [r.left(), r.top(), r.right(), r.bottom()]
}

fn corners(r: Rect) -> [Pos2; 4] {
    [r.left_top(), r.right_top(), r.right_bottom(), r.left_bottom()]
}

impl Designer {
    /// Selects field `id` (at its widget `widget`), as after adding it.
    pub fn select(&mut self, id: ObjRef, widget: ObjRef) {
        self.selected = Some((id, widget));
        self.spec_of = None;
    }

    /// Forgets the selection and any drag (another file was opened, or
    /// the field is gone).
    pub fn clear(&mut self) {
        self.selected = None;
        self.drag = None;
        self.spec_of = None;
        self.message = None;
    }

    /// The selected field, if it still exists.
    fn selected_field<'a>(&self, fields: &'a [FormField]) -> Option<&'a FormField> {
        let (id, _) = self.selected?;
        fields.iter().find(|f| f.id == id)
    }

    /// Draws the fields of `page` (of the open file; `position` is its
    /// place in the view) and handles placing, selecting, moving and
    /// resizing them.
    pub fn show_page(&mut self, ui: &mut egui::Ui, page: usize, position: usize, page_rect: Rect, zoom: f32, fields: &[FormField]) -> Option<Action> {
        let to_screen = |r: Rect| Rect::from_min_max(page_rect.min + r.min.to_vec2() * zoom, page_rect.min + r.max.to_vec2() * zoom);
        let bounds = Rect::from_min_size(Pos2::ZERO, page_rect.size() / zoom);
        let to_page = |p: Pos2| ((p - page_rect.min) / zoom).to_pos2().clamp(bounds.min, bounds.max);
        let response = ui.interact(page_rect, ui.id().with(("form-design", position)), Sense::click_and_drag());

        // The widgets on this page: field, widget, rectangle.
        let widgets: Vec<(&FormField, ObjRef, Rect)> = fields
            .iter()
            .flat_map(|f| f.widgets.iter().filter(|w| w.page == page).map(move |w| (f, w.id, rect_of(w.rect))))
            .collect();
        let hit = |p: Pos2| widgets.iter().rev().find(|(_, _, r)| to_screen(*r).expand(2.0).contains(p)).map(|&(f, w, r)| (f.id, w, r));
        let selected = self.selected.and_then(|(_, w)| widgets.iter().find(|x| x.1 == w)).map(|&(_, w, r)| (w, r));
        let corner_at = |p: Pos2| {
            let (w, r) = selected?;
            corners(to_screen(r)).iter().position(|c| c.distance(p) <= HANDLE).map(|i| (w, r, i))
        };

        let mut action = None;
        if let Some(p) = response.hover_pos()
            && self.drag.is_none()
        {
            let icon = match (corner_at(p), hit(p)) {
                (Some((.., i)), _) if i % 2 == 0 => CursorIcon::ResizeNwSe,
                (Some(_), _) => CursorIcon::ResizeNeSw,
                (None, Some(_)) => CursorIcon::Grab,
                (None, None) if self.tool != Tool::Select => CursorIcon::Crosshair,
                _ => CursorIcon::Default,
            };
            ui.ctx().set_cursor_icon(icon);
        }

        if response.clicked()
            && let Some(p) = response.interact_pointer_pos()
        {
            if let Some((id, w, _)) = hit(p) {
                self.select(id, w);
            } else if let Some((kind, multiline)) = self.tool.makes() {
                // A field of the usual size, its top left where clicked
                // (kept on the page).
                let size = self.tool.usual_size().min(bounds.size());
                let min = to_page(p).min(bounds.max - size);
                action = Some(Action::Add { page, rect: array_of(Rect::from_min_size(min, size)), kind, multiline });
            } else {
                self.selected = None;
            }
        }

        // Drags are recognised after a few points of movement: start from
        // where the button went down.
        let press = ui.input(|i| i.pointer.press_origin());
        if response.drag_started()
            && let Some(p) = press.or(response.interact_pointer_pos())
        {
            self.drag = if let Some((widget, rect, corner)) = corner_at(p) {
                Some(Drag::Resize { position, widget, corner, rect })
            } else if let Some((id, widget, rect)) = hit(p) {
                self.select(id, widget);
                Some(Drag::Move { position, widget, start: to_page(p), rect })
            } else if self.tool.makes().is_some() {
                Some(Drag::New { position, page, start: to_page(p) })
            } else {
                None
            };
        }
        // Where the dragged field is now, in page points.
        let mut current: Option<(Option<ObjRef>, Rect)> = None;
        if let Some(p) = response.interact_pointer_pos().map(to_page) {
            current = match self.drag {
                Some(Drag::New { position: at, start, .. }) if at == position => Some((None, Rect::from_two_pos(start, p))),
                Some(Drag::Move { position: at, widget, start, rect }) if at == position => {
                    // Kept on the page.
                    let delta = (p - start).clamp(bounds.min - rect.min, bounds.max - rect.max);
                    Some((Some(widget), rect.translate(delta)))
                }
                Some(Drag::Resize { position: at, widget, corner, rect }) if at == position => {
                    let opposite = corners(rect)[(corner + 2) % 4];
                    Some((Some(widget), Rect::from_two_pos(opposite, p)))
                }
                _ => None,
            };
        }
        if response.drag_stopped() && self.drag.as_ref().is_some_and(|d| drag_position(d) == position) {
            let drag = self.drag.take();
            if let (Some(drag), Some((_, rect))) = (drag, current) {
                action = match drag {
                    Drag::New { page, start, .. } => self.tool.makes().map(|(kind, multiline)| {
                        let rect = if rect.width() < MIN_SIZE || rect.height() < MIN_SIZE {
                            let size = self.tool.usual_size().min(bounds.size());
                            Rect::from_min_size(start.min(bounds.max - size), size)
                        } else {
                            rect
                        };
                        Action::Add { page, rect: array_of(rect), kind, multiline }
                    }),
                    Drag::Move { widget, rect: before, .. } if (rect.min - before.min).length() > 0.25 => {
                        Some(Action::Move { page, widget, rect: array_of(rect) })
                    }
                    Drag::Resize { widget, rect: before, .. } if rect != before && rect.width() >= MIN_SIZE && rect.height() >= MIN_SIZE => {
                        Some(Action::Move { page, widget, rect: array_of(rect) })
                    }
                    _ => None,
                };
            }
        }

        // The fields, the selected one with its corners, and the one being
        // made or dragged where it is now.
        let color = field_color();
        let painter = ui.painter_at(page_rect);
        let font = egui::FontId::proportional((10.0 * zoom.sqrt()).clamp(9.0, 13.0));
        for &(field, widget, rect) in &widgets {
            let rect = match current {
                Some((Some(w), now)) if w == widget => now,
                _ => rect,
            };
            let r = to_screen(rect);
            let is_selected = self.selected.is_some_and(|(id, _)| id == field.id);
            painter.rect_filled(r, 1.0, color.gamma_multiply(if is_selected { 0.18 } else { 0.08 }));
            painter.rect_stroke(r, 1.0, Stroke::new(if is_selected { 2.0 } else { 1.0 }, color), StrokeKind::Outside);
            // The name, inside when it fits, else above.
            let galley = painter.layout_no_wrap(field.name.clone(), font.clone(), color);
            if galley.size().x + 6.0 < r.width() && galley.size().y + 2.0 < r.height() {
                painter.galley(r.left_top() + Vec2::new(3.0, 1.0), galley, color);
            } else if !matches!(field.kind, FieldKind::CheckBox | FieldKind::Radio) || is_selected {
                painter.galley(r.left_top() - Vec2::new(0.0, galley.size().y + 1.0), galley, color);
            }
            if is_selected && self.selected.is_some_and(|(_, w)| w == widget) {
                for c in corners(r) {
                    let handle = Rect::from_center_size(c, Vec2::splat(7.0));
                    painter.rect_filled(handle, 1.0, Color32::WHITE);
                    painter.rect_stroke(handle, 1.0, Stroke::new(1.5, color), StrokeKind::Inside);
                }
            }
        }
        if let Some((None, rect)) = current {
            let r = to_screen(rect);
            painter.rect_filled(r, 1.0, color.gamma_multiply(0.15));
            painter.rect_stroke(r, 1.0, Stroke::new(1.5, color), StrokeKind::Outside);
        }
        if self.drag.is_none() && response.hover_pos().is_some_and(|p| hit(p).is_none() && corner_at(p).is_none()) && self.tool.makes().is_some() {
            response.on_hover_text_at_pointer(format!("Drag to place a {}, or click for the usual size", self.tool.label().to_lowercase()));
        }
        action
    }

    /// Delete removes the selected field, Esc unselects it (when no text
    /// box has the keyboard).
    pub fn keys(&mut self, ctx: &egui::Context, fields: &[FormField]) -> Option<Action> {
        if ctx.memory(|m| m.focused().is_some()) {
            return None;
        }
        let field = self.selected_field(fields)?;
        let (delete, escape) = ctx.input_mut(|i| {
            (
                i.consume_key(egui::Modifiers::NONE, egui::Key::Delete) || i.consume_key(egui::Modifiers::NONE, egui::Key::Backspace),
                i.consume_key(egui::Modifiers::NONE, egui::Key::Escape),
            )
        });
        if escape {
            self.selected = None;
            return None;
        }
        delete.then(|| Action::Delete { page: field.widgets.first().map_or(0, |w| w.page), id: field.id })
    }

    /// The form panel, at the top of the page view: the kinds of field,
    /// and the selected field's settings.
    pub fn panel(&mut self, ctx: &egui::Context, view: Rect, fields: &[FormField]) -> Option<Action> {
        let mut action = None;
        let field = self.selected_field(fields).cloned();
        if field.is_none() {
            self.selected = None;
        }
        // Settings read again when another field is selected (or the
        // selected one was changed, by undo for instance).
        if let Some(f) = &field
            && (self.spec_of != Some(f.id) || (!ctx.memory(|m| m.focused().is_some()) && FieldSpec::of(f) != self.edited()))
        {
            self.spec = FieldSpec::of(f);
            self.options = self.spec.options.join(", ");
            self.spec_of = Some(f.id);
        }
        egui::Area::new(Id::new("form-panel"))
            .order(egui::Order::Foreground)
            .fixed_pos(Pos2::new(view.center().x, view.top() + 8.0))
            .pivot(egui::Align2::CENTER_TOP)
            .show(ctx, |ui| {
                crate::theme::floating_frame(ui).show(ui, |ui| {
                    // One width, so the buttons stay put as a field is
                    // selected and its settings appear.
                    ui.set_width((view.width() - 40.0).clamp(200.0, 640.0));
                    ui.horizontal_wrapped(|ui| {
                        ui.label("Add:");
                        for tool in Tool::ALL {
                            if ui.selectable_label(self.tool == tool, tool.label()).on_hover_text(tool.tip()).clicked() {
                                self.tool = tool;
                            }
                        }
                        ui.separator();
                        let count = fields.len();
                        ui.weak(match count {
                            0 => "No fields yet".to_string(),
                            1 => "1 field".to_string(),
                            n => format!("{n} fields"),
                        });
                        if crate::theme::primary_button(ui, "Done").on_hover_text("Stop making the form, and try filling it in").clicked() {
                            action = Some(Action::Done);
                        }
                    });
                    if let Some(f) = &field {
                        ui.separator();
                        if let Some(a) = self.settings(ui, f) {
                            action = Some(a);
                        }
                    }
                    if let Some(m) = &self.message {
                        ui.colored_label(crate::theme::palette(ui).danger, m.as_str());
                    }
                });
            });
        action
    }

    /// The settings as edited (the choices read from their text).
    fn edited(&self) -> FieldSpec {
        let options = self.options.split(',').map(|o| o.trim().to_string()).filter(|o| !o.is_empty()).collect();
        FieldSpec { options, ..self.spec.clone() }
    }

    /// The selected field's settings. Text is taken when its box loses the
    /// keyboard (or on Enter), other settings at once.
    fn settings(&mut self, ui: &mut egui::Ui, field: &FormField) -> Option<Action> {
        let page = field.widgets.first().map_or(0, |w| w.page);
        let mut commit = false;
        let editable = !matches!(field.kind, FieldKind::PushButton | FieldKind::Signature);
        ui.horizontal_wrapped(|ui| {
            ui.label("Name");
            let name = ui.add(egui::TextEdit::singleline(&mut self.spec.name).desired_width(130.0)).on_hover_text("What the answer is called when the form is read");
            commit |= name.lost_focus();
            ui.label("Tip");
            let tip = ui
                .add(egui::TextEdit::singleline(&mut self.spec.tooltip).hint_text("shown on hover").desired_width(130.0))
                .on_hover_text("A short description, shown when the pointer is over the field");
            commit |= tip.lost_focus();
            match &field.kind {
                FieldKind::Text { .. } | FieldKind::ComboBox { .. } | FieldKind::ListBox { .. } => {
                    let size_label = |s: f32| if s <= 0.0 { "Fit".to_string() } else { format!("{s:.0} pt") };
                    egui::ComboBox::from_id_salt("form-text-size")
                        .selected_text(size_label(self.spec.font_size))
                        .width(64.0)
                        .show_ui(ui, |ui| {
                            for s in [0.0, 8.0, 9.0, 10.0, 11.0, 12.0, 14.0, 16.0, 18.0, 24.0] {
                                commit |= ui.selectable_value(&mut self.spec.font_size, s, size_label(s)).changed();
                            }
                        })
                        .response
                        .on_hover_text("Text size (Fit: as large as fits)");
                }
                _ => {}
            }
            if let FieldKind::Text { .. } = field.kind {
                commit |= ui.checkbox(&mut self.spec.multiline, "Several lines").changed();
            }
            commit |= ui.checkbox(&mut self.spec.required, "Required").on_hover_text("Must be filled in before the form is sent").changed();
            if let FieldKind::ComboBox { .. } | FieldKind::ListBox { .. } = field.kind {
                ui.end_row();
                ui.label("Choices");
                let choices = ui
                    .add(egui::TextEdit::singleline(&mut self.options).hint_text("Red, Green, Blue").desired_width(300.0))
                    .on_hover_text("The answers to choose from, separated by commas");
                commit |= choices.lost_focus();
            }
            if crate::theme::danger_button(ui, "Delete", true).on_hover_text("Delete this field (Delete key)").clicked() {
                self.message = None;
                commit = false;
                self.selected = None;
                return Some(Action::Delete { page, id: field.id });
            }
            None
        })
        .inner
        .or_else(|| {
            let spec = self.edited();
            (commit && editable && spec != FieldSpec::of(field)).then_some(Action::Update { page, id: field.id, spec })
        })
    }
}

fn drag_position(d: &Drag) -> usize {
    match d {
        Drag::New { position, .. } | Drag::Move { position, .. } | Drag::Resize { position, .. } => *position,
    }
}
