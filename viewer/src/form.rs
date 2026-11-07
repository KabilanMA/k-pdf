//! Filling in forms: editable fields drawn over the page.
//!
//! The page bitmaps are rendered without the contents of fillable fields
//! (see `Renderer::set_hide_form_fields`); this module draws them instead,
//! as live widgets: text boxes, check boxes, radio buttons and drop-down
//! lists. Values are kept per field, so a field shown in several places
//! stays in sync, and saved with `k_pdf::fill_form`.

use std::collections::HashMap;

use eframe::egui::{self, Color32, CursorIcon, FontId, Id, Rect, Sense, Stroke, StrokeKind, UiBuilder, Vec2};
use k_pdf::{FieldKind, FieldValue, FormField, ObjRef, Widget};

/// The tint marking fields that can be filled in (as other PDF viewers do).
const FILLABLE_TINT: Color32 = Color32::from_rgba_premultiplied(40, 52, 100, 40);

#[derive(Default)]
pub struct Form {
    fields: Vec<FormField>,
    /// Current value of each field.
    values: Vec<FieldValue>,
    /// Values as last saved.
    saved: Vec<FieldValue>,
    /// Page → (field, widget) indices of the widgets on it.
    by_page: HashMap<usize, Vec<(usize, usize)>>,
}

impl Form {
    pub fn new(fields: Vec<FormField>) -> Self {
        let mut by_page: HashMap<usize, Vec<(usize, usize)>> = HashMap::new();
        for (fi, f) in fields.iter().enumerate() {
            if matches!(f.kind, FieldKind::PushButton | FieldKind::Signature) {
                continue;
            }
            for (wi, w) in f.widgets.iter().enumerate() {
                by_page.entry(w.page).or_default().push((fi, wi));
            }
        }
        let values: Vec<FieldValue> = fields.iter().map(|f| f.value.clone()).collect();
        Form { saved: values.clone(), values, fields, by_page }
    }

    pub fn is_modified(&self) -> bool {
        self.values != self.saved
    }

    /// The fields whose values changed, for saving.
    pub fn edits(&self) -> Vec<(ObjRef, FieldValue)> {
        self.fields
            .iter()
            .zip(self.values.iter().zip(&self.saved))
            .filter(|(_, (v, s))| v != s)
            .map(|(f, (v, _))| (f.id, v.clone()))
            .collect()
    }

    /// Takes new widget positions (after pages were cropped), keeping the
    /// values entered.
    pub fn update_layout(&mut self, fields: &[FormField]) {
        for f in &mut self.fields {
            if let Some(n) = fields.iter().find(|n| n.id == f.id) {
                f.widgets = n.widgets.clone();
            }
        }
        let mut by_page: HashMap<usize, Vec<(usize, usize)>> = HashMap::new();
        for (fi, f) in self.fields.iter().enumerate() {
            if matches!(f.kind, FieldKind::PushButton | FieldKind::Signature) {
                continue;
            }
            for (wi, w) in f.widgets.iter().enumerate() {
                by_page.entry(w.page).or_default().push((fi, wi));
            }
        }
        self.by_page = by_page;
    }

    pub fn mark_saved(&mut self) {
        self.saved = self.values.clone();
    }

    /// Draws the fields of `page` (an index into the open file's pages),
    /// shown at `page_rect` on screen with `zoom` screen points per PDF point.
    pub fn show_page(&mut self, ui: &mut egui::Ui, page: usize, page_rect: Rect, zoom: f32) {
        let Some(widgets) = self.by_page.get(&page) else { return };
        // Fields sit on a white page: use light-theme colors for them.
        let mut ui = ui.new_child(UiBuilder::new().max_rect(page_rect));
        ui.set_clip_rect(page_rect.intersect(ui.clip_rect()));
        *ui.visuals_mut() = egui::Visuals::light();
        for &(fi, wi) in widgets {
            let field = &self.fields[fi];
            let w = &field.widgets[wi];
            let [l, t, r, b] = w.rect;
            let rect = Rect::from_min_max(page_rect.min + Vec2::new(l, t) * zoom, page_rect.min + Vec2::new(r, b) * zoom);
            if !ui.is_rect_visible(rect) {
                continue;
            }
            let id = Id::new(("form-field", field.id.num, wi));
            if w.background.is_none() && !field.read_only {
                ui.painter().rect_filled(rect, 0.0, FILLABLE_TINT);
            }
            let changed = match &field.kind {
                FieldKind::Text { multiline, password, max_len, .. } => {
                    let FieldValue::Text(text) = &mut self.values[fi] else { continue };
                    text_field(&mut ui, id, rect, zoom, field, text, *multiline, *password, *max_len);
                    false
                }
                FieldKind::CheckBox | FieldKind::Radio => button(&mut ui, id, rect, field, w, &mut self.values[fi]),
                FieldKind::ComboBox { options, .. } | FieldKind::ListBox { options, .. } => {
                    let FieldValue::Choice(selected) = &mut self.values[fi] else { continue };
                    choice(&mut ui, id, rect, zoom, field, options, selected)
                }
                FieldKind::PushButton | FieldKind::Signature => false,
            };
            if changed {
                ui.ctx().request_repaint();
            }
        }
    }
}

/// A field's color as shown on the page (dark pages included).
fn color(c: [f32; 3]) -> Color32 {
    let [r, g, b] = c.map(|v| (v.clamp(0.0, 1.0) * 255.0).round() as u8);
    crate::theme::on_page(Color32::from_rgb(r, g, b))
}

/// Text size on screen: the field's own size, or one that fits its height.
fn text_size(field: &FormField, rect: Rect, zoom: f32, multiline: bool) -> f32 {
    if field.font_size > 0.0 {
        field.font_size * zoom
    } else if multiline {
        10.0 * zoom
    } else {
        (rect.height() * 0.7).min(12.0 * zoom).max(4.0)
    }
}

#[allow(clippy::too_many_arguments)]
fn text_field(
    ui: &mut egui::Ui,
    id: Id,
    rect: Rect,
    zoom: f32,
    field: &FormField,
    text: &mut String,
    multiline: bool,
    password: bool,
    max_len: Option<usize>,
) {
    let size = text_size(field, rect, zoom, multiline);
    let align = match field.alignment {
        1 => egui::Align::Center,
        2 => egui::Align::Max,
        _ => egui::Align::Min,
    };
    let pad = (2.0 * zoom).round().clamp(0.0, 100.0) as i8;
    let edit = if multiline { egui::TextEdit::multiline(text) } else { egui::TextEdit::singleline(text) };
    let mut edit = edit
        .id(id)
        .font(FontId::proportional(size))
        .text_color(color(field.text_color))
        .frame(egui::Frame::NONE)
        .margin(egui::Margin::symmetric(pad, 0))
        .min_size(rect.size())
        .horizontal_align(align)
        .password(password)
        .interactive(!field.read_only)
        .desired_width(rect.width());
    if !multiline {
        edit = edit.vertical_align(egui::Align::Center);
    }
    if let Some(n) = max_len {
        edit = edit.char_limit(n);
    }
    let mut child = ui.new_child(UiBuilder::new().max_rect(rect));
    child.set_clip_rect(rect.intersect(ui.clip_rect()));
    let response = child.put(rect, edit);
    if let Some(tip) = &field.tooltip {
        response.on_hover_text(tip);
    }
}

/// A check box or radio button; returns whether it was clicked.
fn button(ui: &mut egui::Ui, id: Id, rect: Rect, field: &FormField, w: &Widget, value: &mut FieldValue) -> bool {
    let FieldValue::Button(state) = value else { return false };
    let on = state.is_some() && *state == w.on_state;
    let mut response = ui.interact(rect, id, if field.read_only { Sense::hover() } else { Sense::click() });
    if let Some(tip) = &field.tooltip {
        response = response.on_hover_text(tip);
    }
    if !field.read_only && response.hovered() {
        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
        ui.painter().rect_filled(rect, 0.0, Color32::from_black_alpha(20));
    }
    let clicked = response.clicked();
    if clicked {
        *state = match (&field.kind, on) {
            // A selected radio button stays selected; picking another
            // button of the group switches.
            (FieldKind::CheckBox, true) => None,
            _ => w.on_state.clone(),
        };
    }
    if state.is_some() && *state == w.on_state {
        draw_mark(ui.painter(), rect, w.check_style, matches!(field.kind, FieldKind::Radio), color(field.text_color));
    }
    clicked
}

/// Draws the "on" mark in the style the PDF asks for (a ZapfDingbats
/// character; without one, radio buttons get a dot and check boxes a check).
fn draw_mark(painter: &egui::Painter, rect: Rect, style: Option<char>, radio: bool, color: Color32) {
    let s = rect.width().min(rect.height()) * 0.375;
    let c = rect.center();
    let stroke = Stroke::new((s * 0.25).max(1.0), color);
    match style.unwrap_or(if radio { 'l' } else { '4' }) {
        '8' => {
            painter.line_segment([c + Vec2::new(-s, -s) * 0.8, c + Vec2::new(s, s) * 0.8], stroke);
            painter.line_segment([c + Vec2::new(s, -s) * 0.8, c + Vec2::new(-s, s) * 0.8], stroke);
        }
        'l' => {
            painter.circle_filled(c, s * 0.6, color);
        }
        'n' => {
            painter.rect_filled(Rect::from_center_size(c, Vec2::splat(s * 1.2)), 0.0, color);
        }
        'u' => {
            let d = s * 0.7;
            let pts = vec![c + Vec2::new(0.0, -d), c + Vec2::new(d, 0.0), c + Vec2::new(0.0, d), c + Vec2::new(-d, 0.0)];
            painter.add(egui::Shape::convex_polygon(pts, color, Stroke::NONE));
        }
        _ => {
            let pts = vec![c + Vec2::new(-0.75, 0.05) * s, c + Vec2::new(-0.25, 0.6) * s, c + Vec2::new(0.8, -0.65) * s];
            painter.add(egui::Shape::line(pts, stroke));
        }
    }
}

/// A drop-down list (also used for list boxes); returns whether the
/// selection changed.
fn choice(
    ui: &mut egui::Ui,
    id: Id,
    rect: Rect,
    zoom: f32,
    field: &FormField,
    options: &[k_pdf::ChoiceOption],
    selected: &mut Vec<String>,
) -> bool {
    let label = selected
        .first()
        .map(|v| options.iter().find(|o| &o.value == v).map_or(v.clone(), |o| o.label.clone()))
        .unwrap_or_default();
    let size = text_size(field, rect, zoom, false);
    let mut changed = false;
    let mut child = ui.new_child(UiBuilder::new().max_rect(rect));
    child.style_mut().override_font_id = Some(FontId::proportional(size));
    child.spacing_mut().interact_size.y = rect.height();
    child.add_enabled_ui(!field.read_only, |ui| {
        let response = egui::ComboBox::from_id_salt(id)
            .width(rect.width())
            .selected_text(egui::RichText::new(label).color(color(field.text_color)))
            .show_ui(ui, |ui| {
                for o in options {
                    let is_selected = selected.contains(&o.value);
                    if ui.selectable_label(is_selected, &o.label).clicked() && !is_selected {
                        *selected = vec![o.value.clone()];
                        changed = true;
                    }
                }
            });
        if let Some(tip) = &field.tooltip {
            response.response.on_hover_text(tip);
        }
    });
    // Keep the widget's border visible over the combo box frame.
    ui.painter().rect_stroke(rect, 0.0, Stroke::new(1.0, Color32::from_gray(160)), StrokeKind::Inside);
    changed
}
