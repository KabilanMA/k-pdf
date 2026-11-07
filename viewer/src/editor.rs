//! Editing page text in place (Edit mode).
//!
//! Hovering text outlines its paragraph; clicking it opens a rich-text
//! editor right where the text was, in the text's own font, size and color
//! (shown with the matching system font). A toolbar above it changes the
//! font, size, bold/italic/underline, color, alignment and line spacing of
//! the selection (or of what is typed next). Clicking an empty spot adds a
//! new text box. Clicking outside the editor applies the change; Esc
//! cancels it.
//!
//! Edits are applied with `k_pdf::edit::edit_text` to a copy of the
//! document kept here; the render threads get the same changes (see
//! `Pool::set_overlay`) and re-render the page with the real fonts.

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::Arc;

use eframe::egui::{
    self, text::LayoutJob, Align, Color32, CursorIcon, FontFamily, FontId, Id, Pos2, Rect, Sense, Stroke, StrokeKind,
    TextFormat, Vec2,
};
use k_pdf::edit::{self as pdfedit, BlockEdit, PageText, TextStyle};
use k_pdf::Changes;

/// What happened in the editor this frame.
pub enum Event {
    /// An edit was applied to this page (index into the open file's pages);
    /// the previous changes are given for undo.
    Applied { page: usize, before: Changes },
    Failed(String),
}

#[derive(Default)]
pub struct Editor {
    /// The open file, with the edits so far applied as an overlay.
    doc: Option<k_pdf::Document>,
    changes: Changes,
    /// Editable text per page (index into the open file's pages).
    texts: HashMap<usize, Arc<PageText>>,
    session: Option<Session>,
    /// What was just applied, shown until the page has been re-rendered.
    preview: Option<Session>,
    fonts: ScreenFonts,
    families: Option<Vec<String>>,
}

/// A block being edited.
#[derive(Clone)]
struct Session {
    page: usize,
    remove: Vec<usize>,
    /// The original glyphs' area, hidden while editing.
    cover: Option<[f32; 4]>,
    text: String,
    /// Style of each character of `text`.
    styles: Vec<TextStyle>,
    original: (String, Vec<TextStyle>),
    /// Style for newly typed text.
    typing: TextStyle,
    left: f32,
    width: f32,
    first_baseline: f32,
    line_spacing: f32,
    indent: f32,
    align: pdfedit::Align,
    wrap: bool,
    /// The last selection (character range), kept while the toolbar has
    /// the focus.
    selection: Option<(usize, usize)>,
    focus: bool,
}

impl Editor {
    /// Starts over with a newly opened file.
    pub fn open(&mut self, path: &Path) {
        let fonts = std::mem::take(&mut self.fonts);
        let families = self.families.take();
        *self = Editor { doc: k_pdf::Document::open(path).ok(), fonts, families, ..Editor::default() };
    }

    pub fn changes(&self) -> &Changes {
        &self.changes
    }

    pub fn is_modified(&self) -> bool {
        !self.changes.replace.is_empty() || !self.changes.added.is_empty()
    }

    pub fn is_editing(&self) -> bool {
        self.session.is_some()
    }

    /// Goes back to earlier changes (undo).
    pub fn restore(&mut self, changes: Changes) {
        self.changes = changes;
        if let Some(doc) = &self.doc {
            doc.set_overlay(&self.changes);
        }
        self.texts.clear();
        self.session = None;
        self.preview = None;
    }

    /// Redacts areas (page index into the open file's pages, area in
    /// display points). Returns the changes from before, for undo, and what
    /// was removed.
    pub fn redact(&mut self, marks: &[(usize, [f32; 4])]) -> Result<(Changes, pdfedit::Redacted), String> {
        let doc = self.doc.as_ref().ok_or("the document isn't open")?;
        self.session = None;
        let before = self.changes.clone();
        let mut by_page: std::collections::BTreeMap<usize, Vec<[f32; 4]>> = Default::default();
        for &(page, rect) in marks {
            by_page.entry(page).or_default().push(rect);
        }
        let mut total = pdfedit::Redacted::default();
        for (page, areas) in by_page {
            match pdfedit::redact_page(doc, page, &areas, &mut self.changes) {
                Ok(done) => total += done,
                Err(e) => {
                    // All or nothing: a partly applied redaction would be
                    // easy to miss.
                    self.changes = before;
                    doc.set_overlay(&self.changes);
                    return Err(format!("page {}: {e}", page + 1));
                }
            }
        }
        self.texts.clear();
        self.preview = None;
        Ok((before, total))
    }

    /// The open file, with the changes so far.
    pub fn document(&self) -> Option<&k_pdf::Document> {
        self.doc.as_ref()
    }

    /// Makes a change to the comments (see `k_pdf::edit::annotate`) with
    /// `f`. Returns the changes from before, for undo, and what `f`
    /// returned; on failure nothing is changed.
    pub fn annotate<T>(
        &mut self,
        f: impl FnOnce(&k_pdf::Document, &mut Changes) -> k_pdf::Result<T>,
    ) -> Result<(Changes, T), String> {
        let doc = self.doc.as_ref().ok_or("the document isn't open")?;
        let before = self.changes.clone();
        match f(doc, &mut self.changes) {
            Ok(v) => Ok((before, v)),
            Err(e) => {
                self.changes = before;
                doc.set_overlay(&self.changes);
                Err(e.to_string())
            }
        }
    }

    /// Every place `words` appear in the open file: (page, area in display
    /// points).
    pub fn find_text(&self, words: &str) -> Vec<(usize, [f32; 4])> {
        let Some(doc) = &self.doc else { return Vec::new() };
        let count = doc.page_count().unwrap_or(0);
        (0..count)
            .flat_map(|p| pdfedit::find_text(doc, p, words).unwrap_or_default().into_iter().map(move |r| (p, r)))
            .collect()
    }

    /// Crops pages (indices into the open file's pages) to `rect`
    /// (display points), or uncrops them (`None`). Returns the changes from
    /// before, for undo. With several pages, pages the area doesn't fit
    /// are left alone.
    pub fn crop(&mut self, pages: &[usize], rect: Option<[f32; 4]>) -> Result<Changes, String> {
        let doc = self.doc.as_ref().ok_or("the document isn't open")?;
        self.session = None;
        let before = self.changes.clone();
        let mut cropped = 0;
        let mut last_error = None;
        for &p in pages {
            match pdfedit::crop_page(doc, p, rect, &mut self.changes) {
                Ok(()) => cropped += 1,
                Err(e) => last_error = Some(e.to_string()),
            }
        }
        if cropped == 0 {
            self.changes = before;
            doc.set_overlay(&self.changes);
            return Err(last_error.unwrap_or_else(|| "nothing to crop".into()));
        }
        self.texts.clear();
        self.preview = None;
        Ok(before)
    }

    /// Displayed size of every page of the open file, with the edits.
    pub fn page_sizes(&self) -> Option<Vec<egui::Vec2>> {
        let doc = self.doc.as_ref()?;
        let pages = doc.pages().ok()?;
        Some(
            pages
                .iter()
                .map(|p| {
                    let (w, h) = p.display_size(doc);
                    egui::vec2(w as f32, h as f32)
                })
                .collect(),
        )
    }

    /// The form fields of the open file, with the edits (their positions
    /// change when pages are cropped).
    pub fn form_fields(&self) -> Vec<k_pdf::FormField> {
        self.doc.as_ref().and_then(|d| k_pdf::form_fields(d).ok()).unwrap_or_default()
    }

    /// The page with this index was re-rendered: stop showing the preview.
    pub fn page_rendered(&mut self, page: usize) {
        if self.preview.as_ref().is_some_and(|p| p.page == page) {
            self.preview = None;
        }
    }

    fn text_of(&mut self, page: usize) -> Option<Arc<PageText>> {
        if !self.texts.contains_key(&page) {
            let doc = self.doc.as_ref()?;
            let text = pdfedit::page_text(doc, page).ok()?;
            self.texts.insert(page, Arc::new(text));
        }
        self.texts.get(&page).cloned()
    }

    /// Draws edit mode over a page (`page` indexes the open file's pages)
    /// shown at `rect` with `zoom`. Returns what happened.
    pub fn show_page(&mut self, ui: &mut egui::Ui, page: usize, rect: Rect, zoom: f32) -> Option<Event> {
        self.fonts.prepare(ui.ctx());
        let to_screen = |x: f32, y: f32| rect.min + Vec2::new(x, y) * zoom;
        let to_page = |p: Pos2| ((p - rect.min) / zoom).to_pos2();

        if let Some(preview) = self.preview.clone()
            && preview.page == page
        {
            draw_cover(ui, &preview, &to_screen, zoom);
            let job = self.fonts.layout_job(&preview.text, &preview.styles, &preview, zoom);
            let galley = ui.fonts_mut(|f| f.layout_job(job));
            let top = to_screen(preview.left, preview.first_baseline) - Vec2::new(0.0, ascent(&preview) * zoom);
            ui.painter().galley(top, galley, Color32::BLACK);
        }

        let editing_here = self.session.as_ref().is_some_and(|s| s.page == page);
        if editing_here {
            return self.show_session(ui, &to_screen, zoom);
        }
        if self.session.is_some() {
            return None; // editing on another page
        }

        // Not editing: outline the paragraph under the pointer, click to edit.
        let text = self.text_of(page)?;
        let response = ui.interact(rect, ui.id().with(("edit-page", page)), Sense::click());
        let pointer = response.hover_pos();
        let hovered = pointer.and_then(|p| {
            let q = to_page(p);
            text.block_at(q.x, q.y)
        });
        if let Some(b) = hovered {
            let block = &text.blocks[b];
            let [l, t, r, bt] = block.rect;
            let outline = Rect::from_min_max(to_screen(l, t), to_screen(r, bt)).expand(2.0);
            let color = if block.editable { Color32::from_rgb(40, 120, 230) } else { Color32::GRAY };
            ui.painter().rect_stroke(outline, 2.0, Stroke::new(1.5, color), StrokeKind::Outside);
            ui.ctx().set_cursor_icon(if block.editable { CursorIcon::Text } else { CursorIcon::NotAllowed });
            response.clone().on_hover_text(if block.editable {
                "Click to edit this text"
            } else {
                "This text can't be edited (it is rotated, hidden or part of a graphic)"
            });
        } else if pointer.is_some() {
            ui.ctx().set_cursor_icon(CursorIcon::Text);
            response.clone().on_hover_text("Click to add text here");
        }
        if response.clicked()
            && let Some(p) = response.interact_pointer_pos()
        {
            match hovered {
                Some(b) if text.blocks[b].editable => self.start(page, &text, b),
                Some(_) => {}
                None => self.start_new(page, &text, to_page(p)),
            }
        }
        None
    }

    /// Opens the editor on block `b`.
    fn start(&mut self, page: usize, text: &PageText, b: usize) {
        let block = &text.blocks[b];
        let chars = text.block_text(block);
        let styles: Vec<TextStyle> = chars.iter().map(|(_, s)| s.clone()).collect();
        let string: String = chars.iter().map(|(c, _)| *c).collect();
        let left = block.lines.iter().map(|l| l.left).fold(f32::MAX, f32::min);
        let typing = styles.last().cloned().unwrap_or_default();
        self.session = Some(Session {
            page,
            remove: block.glyphs.clone(),
            cover: Some(block.rect),
            original: (string.clone(), styles.clone()),
            text: string,
            styles,
            typing,
            left,
            width: (block.rect[2] - left).max(10.0),
            first_baseline: block.lines[0].baseline,
            line_spacing: if block.line_spacing.is_finite() && block.line_spacing > 0.5 { block.line_spacing } else { 1.2 },
            indent: block.indent,
            align: block.align,
            wrap: block.lines.len() > 1,
            selection: None,
            focus: true,
        });
    }

    /// Opens an empty text box at `at` (display points), styled like the
    /// nearest text on the page.
    fn start_new(&mut self, page: usize, text: &PageText, at: Pos2) {
        let nearest = text.glyphs.iter().filter(|g| g.plain).min_by(|a, b| {
            let d = |g: &pdfedit::Glyph| (g.origin[0] - at.x).hypot(g.origin[1] - at.y);
            d(a).total_cmp(&d(b))
        });
        let style = nearest.map(|g| TextStyle { underline: false, ..text.style_of(g) }).unwrap_or_default();
        let baseline = at.y + style.size * 0.35;
        self.session = Some(Session {
            page,
            remove: Vec::new(),
            cover: None,
            text: String::new(),
            styles: Vec::new(),
            original: (String::new(), Vec::new()),
            typing: style,
            left: at.x,
            width: 250.0,
            first_baseline: baseline,
            line_spacing: 1.2,
            indent: 0.0,
            align: pdfedit::Align::Left,
            wrap: true,
            selection: None,
            focus: true,
        });
    }

    /// Applies the edit being made (if anything changed).
    pub fn commit(&mut self) -> Option<Event> {
        let s = self.session.take()?;
        if s.text == s.original.0 && s.styles == s.original.1 {
            return None;
        }
        let doc = self.doc.as_ref()?;
        // Consecutive characters with the same style form one run.
        let mut runs: Vec<(String, TextStyle)> = Vec::new();
        for (c, style) in s.text.chars().zip(&s.styles) {
            match runs.last_mut() {
                Some((t, st)) if st == style => t.push(c),
                _ => runs.push((c.to_string(), style.clone())),
            }
        }
        let edit = BlockEdit {
            page: s.page,
            remove: s.remove.clone(),
            runs,
            left: s.left,
            width: s.width,
            first_baseline: s.first_baseline,
            line_spacing: s.line_spacing,
            align: s.align,
            indent: s.indent,
            wrap: s.wrap,
        };
        let before = self.changes.clone();
        match pdfedit::edit_text(doc, &edit, &mut self.changes) {
            Ok(()) => {
                self.texts.remove(&s.page);
                let page = s.page;
                self.preview = Some(s);
                Some(Event::Applied { page, before })
            }
            Err(e) => {
                self.changes = before;
                doc.set_overlay(&self.changes);
                Some(Event::Failed(e.to_string()))
            }
        }
    }

    fn show_session(
        &mut self,
        ui: &mut egui::Ui,
        to_screen: &dyn Fn(f32, f32) -> Pos2,
        zoom: f32,
    ) -> Option<Event> {
        let ctx = ui.ctx().clone();
        let id = Id::new("pdf-text-editor");
        let families = self.families.get_or_insert_with(k_pdf::edit::fonts::system_font_families).clone();
        let page_families: Vec<String> = self
            .session
            .as_ref()
            .and_then(|s| self.texts.get(&s.page))
            .map(|t| {
                let mut f: Vec<String> = t.fonts.values().map(|l| l.family.clone()).collect();
                f.sort();
                f.dedup();
                f
            })
            .unwrap_or_default();
        let s = self.session.as_mut()?;
        draw_cover(ui, s, to_screen, zoom);

        // The editor box: as wide as the text area, as tall as the text.
        let top_left = to_screen(s.left, s.first_baseline) - Vec2::new(0.0, ascent(s) * zoom);
        let wrap_width = if s.wrap { s.width * zoom } else { f32::INFINITY };
        let before_text = s.text.clone();
        let before_styles = s.styles.clone();
        let fonts = &mut self.fonts;
        let session_view = s.clone();
        let mut layouter = |ui: &egui::Ui, buf: &dyn egui::TextBuffer, _wrap: f32| {
            let styles = restyle(&before_text, &before_styles, buf.as_str(), &session_view.typing);
            let mut job = fonts.layout_job(buf.as_str(), &styles, &session_view, zoom);
            job.wrap.max_width = wrap_width;
            ui.fonts_mut(|f| f.layout_job(job))
        };
        let width = if s.wrap { s.width * zoom } else { (ui.max_rect().right() - top_left.x).max(50.0) };
        // Room to grow downwards as the text gets longer.
        let area = Rect::from_min_size(top_left, Vec2::new(width, 4000.0));
        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(area));
        *child.visuals_mut() = egui::Visuals::light();
        let output = egui::TextEdit::multiline(&mut s.text)
            .id(id)
            .frame(egui::Frame::NONE)
            .margin(egui::Margin::ZERO)
            .desired_width(width)
            .desired_rows(1)
            .layouter(&mut layouter)
            .show(&mut child);
        let editor_rect = output.response.rect;
        if s.focus {
            output.response.request_focus();
            s.focus = false;
        }
        if output.response.changed() {
            s.styles = restyle(&before_text, &before_styles, &s.text, &s.typing);
        }
        if let Some(range) = output.cursor_range {
            let (a, b) = (range.primary.index.0, range.secondary.index.0);
            s.selection = Some((a.min(b), a.max(b)));
        }
        ui.painter().rect_stroke(editor_rect.expand(3.0), 2.0, Stroke::new(1.5, Color32::from_rgb(40, 120, 230)), StrokeKind::Outside);

        // The toolbar, above the box (below it if there is no room above).
        let view = ui.clip_rect();
        let above = editor_rect.top() - 44.0;
        let y = if above >= view.top() + 2.0 { above } else { (editor_rect.bottom() + 8.0).min(view.bottom() - 40.0) };
        let x = (editor_rect.left() - 3.0).clamp(view.left() + 2.0, (view.right() - 640.0).max(view.left() + 2.0));
        let toolbar_pos = Pos2::new(x, y);
        let toolbar = egui::Area::new(Id::new("pdf-text-toolbar"))
            .order(egui::Order::Foreground)
            .fixed_pos(toolbar_pos)
            .show(&ctx, |ui| {
                egui::Frame::popup(ui.style()).show(ui, |ui| {
                    ui.horizontal(|ui| toolbar_ui(ui, s, &page_families, &families, id));
                })
            });
        let toolbar_rect = toolbar.response.rect;

        // Esc cancels; clicking elsewhere applies.
        if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            self.session = None;
            return None;
        }
        let pressed_outside = ctx.input(|i| i.pointer.primary_pressed())
            && ctx.input(|i| i.pointer.interact_pos()).is_some_and(|p| {
                !editor_rect.expand(6.0).contains(p) && !toolbar_rect.contains(p) && !ctx.any_popup_open()
            });
        if pressed_outside {
            return self.commit();
        }
        None
    }
}

/// Height of the first line above its baseline (display points).
fn ascent(s: &Session) -> f32 {
    let size = s.styles.iter().map(|st| st.size).fold(s.typing.size, f32::max);
    size * 0.95
}

/// Hides the original text with the page background while it is edited.
fn draw_cover(ui: &egui::Ui, s: &Session, to_screen: &dyn Fn(f32, f32) -> Pos2, zoom: f32) {
    if let Some([l, t, r, b]) = s.cover {
        let rect = Rect::from_min_max(to_screen(l, t), to_screen(r, b)).expand(1.0 * zoom);
        ui.painter().rect_filled(rect, 0.0, crate::theme::paper(crate::theme::dark_pages()));
    }
}

/// Styles for `new` text, given the styles of the `old` text: unchanged
/// parts keep theirs, inserted characters take the typing style.
fn restyle(old: &str, old_styles: &[TextStyle], new: &str, typing: &TextStyle) -> Vec<TextStyle> {
    let o: Vec<char> = old.chars().collect();
    let n: Vec<char> = new.chars().collect();
    let prefix = o.iter().zip(&n).take_while(|(a, b)| a == b).count();
    let max_suffix = o.len().min(n.len()) - prefix;
    let suffix = o.iter().rev().zip(n.iter().rev()).take(max_suffix).take_while(|(a, b)| a == b).count();
    let mut out: Vec<TextStyle> = old_styles.iter().take(prefix).cloned().collect();
    out.extend(std::iter::repeat_n(typing.clone(), n.len() - prefix - suffix));
    out.extend(old_styles.iter().skip(o.len() - suffix).cloned());
    out.resize(n.len(), typing.clone());
    out
}

/// A text color as shown on the page (dark pages included).
fn color32(c: [f32; 3]) -> Color32 {
    let [r, g, b] = c.map(|v| (v.clamp(0.0, 1.0) * 255.0).round() as u8);
    crate::theme::on_page(Color32::from_rgb(r, g, b))
}

/// The formatting toolbar.
fn toolbar_ui(ui: &mut egui::Ui, s: &mut Session, page_families: &[String], families: &[String], editor: Id) {
    // The style shown: the selection's first character, or the typing style.
    let range = s.selection.filter(|(a, b)| a < b);
    let current = match range {
        Some((a, _)) => s.styles.get(a).cloned().unwrap_or_else(|| s.typing.clone()),
        None => s.typing.clone(),
    };
    let mut style = current.clone();

    egui::ComboBox::from_id_salt("pdf-font-family")
        .selected_text(&style.family)
        .width(150.0)
        .height(400.0)
        .show_ui(ui, |ui| {
            if !page_families.is_empty() {
                ui.label(egui::RichText::new("In this document").weak());
                for f in page_families {
                    ui.selectable_value(&mut style.family, f.clone(), f);
                }
                ui.separator();
                ui.label(egui::RichText::new("Installed").weak());
            }
            for f in families {
                ui.selectable_value(&mut style.family, f.clone(), f);
            }
        });
    ui.add(egui::DragValue::new(&mut style.size).range(2.0..=200.0).speed(0.2).suffix(" pt"))
        .on_hover_text("Font size");
    ui.toggle_value(&mut style.bold, egui::RichText::new("B").strong()).on_hover_text("Bold");
    ui.toggle_value(&mut style.italic, egui::RichText::new("I").italics()).on_hover_text("Italic");
    ui.toggle_value(&mut style.underline, egui::RichText::new("U").underline()).on_hover_text("Underline");
    let mut rgb = style.color;
    if ui.color_edit_button_rgb(&mut rgb).on_hover_text("Text color").changed() {
        style.color = rgb;
    }
    ui.separator();
    for (align, label, tip) in [
        (pdfedit::Align::Left, "⬅", "Align left"),
        (pdfedit::Align::Center, "↔", "Center"),
        (pdfedit::Align::Right, "➡", "Align right"),
        (pdfedit::Align::Justify, "☰", "Justify"),
    ] {
        if ui.selectable_label(s.align == align, label).on_hover_text(tip).clicked() {
            s.align = align;
            s.focus = true;
        }
    }
    ui.add(egui::DragValue::new(&mut s.line_spacing).range(0.8..=3.0).speed(0.01).max_decimals(2))
        .on_hover_text("Line spacing (× font size)");
    if ui.checkbox(&mut s.wrap, "Wrap").on_hover_text("Wrap lines at the box width").changed() {
        s.focus = true;
    }
    if s.wrap {
        ui.add(egui::DragValue::new(&mut s.width).range(20.0..=2000.0).speed(1.0).suffix(" pt"))
            .on_hover_text("Box width");
    }

    // Apply style changes to the selection, or to what is typed next.
    if style != current {
        match range {
            Some((a, b)) => {
                for st in s.styles.iter_mut().take(b).skip(a) {
                    apply_change(st, &current, &style);
                }
            }
            None => s.typing = style,
        }
        s.focus = true;
        ui.ctx().memory_mut(|m| m.request_focus(editor));
    }
}

/// Copies only the attributes that changed from `old` to `new` into `st`
/// (so making a mixed selection bold keeps each part's size).
fn apply_change(st: &mut TextStyle, old: &TextStyle, new: &TextStyle) {
    if new.family != old.family {
        st.family = new.family.clone();
    }
    if new.size != old.size {
        st.size = new.size;
    }
    if new.bold != old.bold {
        st.bold = new.bold;
    }
    if new.italic != old.italic {
        st.italic = new.italic;
    }
    if new.underline != old.underline {
        st.underline = new.underline;
    }
    if new.color != old.color {
        st.color = new.color;
    }
}

/// System fonts loaded into egui to show text in its own typeface.
#[derive(Default)]
struct ScreenFonts {
    /// Font families egui can use now, by (family, bold, italic).
    ready: HashMap<(String, bool, bool), Option<String>>,
    /// Requested; usable from the next frame.
    pending: HashSet<(String, bool, bool)>,
    definitions: Option<egui::FontDefinitions>,
    dirty: bool,
    /// The frame `prepare` last ran in (it must run once per frame).
    frame: Option<u64>,
}

impl ScreenFonts {
    /// Installs fonts requested last frame.
    fn prepare(&mut self, ctx: &egui::Context) {
        let frame = ctx.cumulative_pass_nr();
        if self.frame == Some(frame) {
            return;
        }
        self.frame = Some(frame);
        if self.dirty
            && let Some(defs) = &self.definitions
        {
            ctx.set_fonts(defs.clone());
            self.dirty = false;
            return; // they work from the next frame
        }
        for key in self.pending.drain() {
            let name = format!("pdf-{}-{}{}", key.0, key.1 as u8, key.2 as u8);
            let usable = self.definitions.as_ref().is_some_and(|d| d.families.contains_key(&FontFamily::Name(name.clone().into())));
            self.ready.insert(key, usable.then_some(name));
        }
    }

    /// The egui font family for a style (the default font until loaded).
    fn family(&mut self, style: &TextStyle) -> FontFamily {
        let key = (style.family.clone(), style.bold, style.italic);
        if let Some(found) = self.ready.get(&key) {
            return found.as_ref().map_or(FontFamily::Proportional, |n| FontFamily::Name(n.clone().into()));
        }
        if !self.pending.contains(&key) {
            let defs = self.definitions.get_or_insert_with(crate::theme::base_fonts);
            if let Some((data, index)) = k_pdf::edit::fonts::screen_font(&style.family, style.bold, style.italic) {
                let name = format!("pdf-{}-{}{}", key.0, key.1 as u8, key.2 as u8);
                let mut font = egui::FontData::from_owned(data);
                font.index = index;
                defs.font_data.insert(name.clone(), Arc::new(font));
                // Characters the font lacks come from the default fonts.
                let mut list = vec![name.clone()];
                list.extend(defs.families.get(&FontFamily::Proportional).cloned().unwrap_or_default());
                defs.families.insert(FontFamily::Name(name.into()), list);
                self.dirty = true;
            }
            self.pending.insert(key);
        }
        FontFamily::Proportional
    }

    /// A layout of `text` in the session's styles, at `zoom`.
    fn layout_job(&mut self, text: &str, styles: &[TextStyle], s: &Session, zoom: f32) -> LayoutJob {
        let mut job = LayoutJob {
            halign: match s.align {
                pdfedit::Align::Left | pdfedit::Align::Justify => Align::Min,
                pdfedit::Align::Center => Align::Center,
                pdfedit::Align::Right => Align::Max,
            },
            // egui justifies by spacing out letters; the PDF spreads words.
            // While typing, plain left alignment reads better.
            justify: false,
            ..Default::default()
        };
        job.wrap.max_width = if s.wrap { s.width * zoom } else { f32::INFINITY };
        let mut chars = text.char_indices().peekable();
        let mut i = 0;
        while let Some((start, _)) = chars.next() {
            let style = styles.get(i).unwrap_or(&s.typing).clone();
            let mut end = text.len();
            let mut j = i + 1;
            while let Some(&(pos, _)) = chars.peek() {
                if styles.get(j).unwrap_or(&s.typing) != &style {
                    end = pos;
                    break;
                }
                chars.next();
                j += 1;
            }
            let format = TextFormat {
                font_id: FontId::new(style.size * zoom, self.family(&style)),
                color: color32(style.color),
                underline: if style.underline { Stroke::new((style.size * zoom * 0.06).max(1.0), color32(style.color)) } else { Stroke::NONE },
                line_height: Some(style.size * s.line_spacing * zoom),
                ..Default::default()
            };
            job.append(&text[start..end], 0.0, format);
            i = j;
        }
        if text.is_empty() {
            // An empty box still has the typing style's height.
            let format = TextFormat {
                font_id: FontId::new(s.typing.size * zoom, self.family(&s.typing)),
                line_height: Some(s.typing.size * s.line_spacing * zoom),
                ..Default::default()
            };
            job.append("", 0.0, format);
        }
        job
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn styles_follow_edits() {
        let a = TextStyle { size: 10.0, ..TextStyle::default() };
        let b = TextStyle { size: 20.0, ..TextStyle::default() };
        let t = TextStyle { bold: true, ..TextStyle::default() };
        let old = vec![a.clone(), a.clone(), b.clone()];
        // "abc" → "aXbc": the inserted X takes the typing style.
        assert_eq!(restyle("abc", &old, "aXbc", &t), vec![a.clone(), t.clone(), a.clone(), b.clone()]);
        // Deleting keeps the rest.
        assert_eq!(restyle("abc", &old, "ac", &t), vec![a.clone(), b.clone()]);
        assert_eq!(restyle("abc", &old, "", &t), Vec::<TextStyle>::new());
    }
}
