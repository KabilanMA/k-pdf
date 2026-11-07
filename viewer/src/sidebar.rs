//! The sidebar shows the pages, the sections (the document's outline) or
//! the comments. Long documents with an outline open on the sections,
//! others on the pages; buttons at the top switch.
//!
//! Pages: a scrollable column of page thumbnails. The current
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
//!
//! Sections: headings with their subheadings indented under them, each
//! with its page number; the section being read is highlighted and kept in
//! view. Sections with subsections fold open and closed.
//!
//! Comments: every highlight and note, page by page, with the highlighted
//! text and the comment; clicking one goes to it and opens it.

use std::collections::HashMap;

use eframe::egui::{
    self, Align, Color32, CursorIcon, FontId, Id, LayerId, Order, Pos2, Rect, Sense, Stroke, StrokeKind,
    TextureHandle, Vec2,
};

use k_pdf::{LinkTarget, ObjRef, OutlineItem};

use crate::icons::{self, Icon};
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
/// The sidebar's width when it shows the sections.
pub const SECTIONS_WIDTH: f32 = 250.0;

/// Documents with at least this many pages (and an outline) open on the
/// sections.
const LONG_DOCUMENT: usize = 20;
/// Height of the bar with the pages/sections buttons.
const HEADER_HEIGHT: f32 = 38.0;
/// Height of a row in the sections list.
const ROW_HEIGHT: f32 = 28.0;
/// Indent per outline level.
const INDENT: f32 = 14.0;

/// What the sidebar shows.
#[derive(Clone, Copy, PartialEq, Eq, Default, Debug)]
pub enum View {
    #[default]
    Pages,
    Sections,
    Comments,
}

/// The comments to list, and the one that is open.
pub struct CommentList<'a> {
    pub rows: &'a [CommentRow],
    pub open: Option<ObjRef>,
}

/// A comment as listed.
pub struct CommentRow {
    pub id: ObjRef,
    /// Page of the open file, and its position.
    pub page: usize,
    pub position: usize,
    pub color: Color32,
    pub kind: String,
    /// The highlighted text (highlights only, once known).
    pub excerpt: Option<String>,
    pub text: String,
    /// Author and date.
    pub by: String,
}

/// What the user did in the sidebar.
pub enum Action {
    /// Clicked the thumbnail at this position.
    GoTo(usize),
    /// Dragged the page at position `from` so it now sits at position `to`.
    Move { from: usize, to: usize },
    /// Clicked the insert button in the gap before this position (the
    /// number of pages for the gap after the last one).
    Insert(usize),
    /// Clicked a section; its page is a page of the open file (not a
    /// position).
    Section(LinkTarget),
    /// Clicked a comment (on this page of the open file).
    Comment { id: ObjRef, page: usize },
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
    view: View,
    outline: Vec<OutlineItem>,
    /// Whether each section's subsections are shown.
    expanded: Vec<bool>,
    /// The highlighted section as of the last frame; when it changes the
    /// list scrolls to keep it visible.
    shown_section: Option<usize>,
    /// The section last clicked, and the reading spot it took the view to:
    /// it stays highlighted while the view is there (a short section right
    /// after it would otherwise take over).
    clicked: Option<(usize, Option<(usize, f32)>)>,
    /// The open comment as of the last frame; when it changes the list
    /// scrolls to it.
    shown_comment: Option<ObjRef>,
}

impl Sidebar {
    /// Forgets everything (a new document was opened).
    pub fn clear(&mut self) {
        *self = Sidebar::default();
    }

    /// Sets the document's outline and picks the view: `view` if given
    /// (the file was reopened after saving), otherwise the sections for
    /// long documents that have them and the pages for the rest.
    pub fn set_outline(&mut self, outline: Vec<OutlineItem>, page_count: usize, view: Option<View>) {
        // Headings start open on their subheadings; deeper levels as the
        // file asks.
        self.expanded = outline.iter().map(|item| item.level == 0 || item.open).collect();
        self.view = match view {
            Some(View::Sections) if !outline.is_empty() => View::Sections,
            Some(View::Comments) => View::Comments,
            Some(_) => View::Pages,
            None if !outline.is_empty() && page_count >= LONG_DOCUMENT => View::Sections,
            None => View::Pages,
        };
        self.outline = outline;
    }

    pub fn view(&self) -> View {
        self.view
    }

    /// Goes to the next view: pages, sections (if the document has
    /// them), comments.
    pub fn toggle_view(&mut self, pool: &Pool) {
        let view = match self.view {
            View::Pages if !self.outline.is_empty() => View::Sections,
            View::Pages | View::Sections => View::Comments,
            View::Comments => View::Pages,
        };
        self.set_view(view, pool);
    }

    pub fn set_view(&mut self, view: View, pool: &Pool) {
        if view == self.view {
            return;
        }
        self.view = view;
        // Thumbnails aren't needed while the sections are shown (and the
        // ones still wanted are asked for again when the pages come back).
        self.last_want.clear();
        pool.want_thumbs(&[]);
        self.shown_position = None;
        self.shown_section = None;
        self.clicked = None;
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
    /// indexed by page; `order` lists the pages by position. `reading` is
    /// the spot being read: a position, and display points from the top
    /// of that page.
    pub fn show(
        &mut self,
        ui: &mut egui::Ui,
        sizes: &[Vec2],
        order: &[usize],
        reading: (usize, f32),
        comments: CommentList,
        pool: &Pool,
    ) -> Option<Action> {
        self.header(ui, pool);
        match self.view {
            View::Pages => self.show_pages(ui, sizes, order, reading.0, pool),
            View::Sections => self.show_sections(ui, order, reading),
            View::Comments => self.show_comments(ui, comments.rows, comments.open),
        }
    }

    /// The comments list.
    fn show_comments(&mut self, ui: &mut egui::Ui, rows: &[CommentRow], open: Option<ObjRef>) -> Option<Action> {
        let palette = crate::theme::palette(ui);
        let follow = open.is_some() && open != self.shown_comment;
        self.shown_comment = open;
        let mut action = None;
        egui::ScrollArea::vertical().id_salt("comments").auto_shrink(false).show(ui, |ui| {
            ui.add_space(6.0);
            if rows.is_empty() {
                ui.add_space(16.0);
                ui.vertical_centered(|ui| {
                    icons::paint(
                        ui.painter(),
                        Rect::from_center_size(ui.cursor().min + Vec2::new(ui.available_width() / 2.0, 14.0), Vec2::splat(28.0)),
                        Icon::Comment,
                        palette.weak,
                    );
                    ui.add_space(36.0);
                    ui.label(egui::RichText::new("No comments yet").color(palette.text));
                    ui.add_space(4.0);
                    ui.label(
                        egui::RichText::new(
                            "Select text to highlight it or comment on it, or use the Comment tool to add a note anywhere.",
                        )
                        .size(12.0)
                        .color(palette.weak),
                    );
                });
                return;
            }
            let mut last_position = None;
            for row in rows {
                if last_position != Some(row.position) {
                    last_position = Some(row.position);
                    ui.add_space(4.0);
                    ui.horizontal(|ui| {
                        ui.add_space(PAD - 2.0);
                        ui.label(egui::RichText::new(format!("Page {}", row.position + 1)).size(12.0).color(palette.weak));
                    });
                }
                let selected = open == Some(row.id);
                let frame = egui::Frame::new()
                    .fill(if selected { palette.accent_soft } else { Color32::TRANSPARENT })
                    .corner_radius(8.0)
                    .inner_margin(egui::Margin { left: 14, right: 8, top: 6, bottom: 6 })
                    .outer_margin(egui::Margin::symmetric(6, 1));
                let inner = frame.show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.horizontal(|ui| {
                        ui.label(egui::RichText::new(&row.kind).size(12.0).strong().color(palette.text));
                        ui.label(egui::RichText::new(&row.by).size(11.5).color(palette.weak));
                    });
                    if let Some(e) = row.excerpt.as_ref().filter(|e| !e.is_empty()) {
                        let mut job = egui::text::LayoutJob::single_section(
                            format!("“{e}”"),
                            egui::TextFormat { italics: true, ..egui::TextFormat::simple(FontId::proportional(12.5), palette.weak) },
                        );
                        job.wrap = egui::text::TextWrapping { max_rows: 2, ..egui::text::TextWrapping::wrap_at_width(ui.available_width()) };
                        ui.label(job);
                    }
                    if !row.text.trim().is_empty() {
                        let mut job = egui::text::LayoutJob::single_section(
                            row.text.trim().to_string(),
                            egui::TextFormat::simple(FontId::proportional(13.0), palette.text),
                        );
                        job.wrap = egui::text::TextWrapping { max_rows: 4, ..egui::text::TextWrapping::wrap_at_width(ui.available_width()) };
                        ui.label(job);
                    }
                });
                let rect = inner.response.rect;
                // The highlight's color as a bar on the left.
                let bar = Rect::from_min_max(rect.min + Vec2::new(5.0, 6.0), Pos2::new(rect.left() + 8.0, rect.bottom() - 6.0));
                ui.painter().rect_filled(bar, 2.0, row.color);
                let response = ui.interact(rect, ui.id().with(("comment", row.id.num)), Sense::click());
                if response.hovered() && !selected {
                    ui.painter().rect_filled(rect, 8.0, palette.hover);
                }
                if response.on_hover_cursor(CursorIcon::PointingHand).clicked() {
                    action = Some(Action::Comment { id: row.id, page: row.page });
                }
                if selected && follow {
                    ui.scroll_to_rect(rect, Some(Align::Center));
                }
            }
            ui.add_space(8.0);
        });
        action
    }

    /// The bar at the top: what is shown, and buttons to switch.
    fn header(&mut self, ui: &mut egui::Ui, pool: &Pool) {
        let has_sections = !self.outline.is_empty();
        let mut view = self.view;
        let (rect, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), HEADER_HEIGHT), Sense::hover());
        let palette = crate::theme::palette(ui);
        ui.painter().hline(rect.x_range(), rect.bottom() - 0.5, Stroke::new(1.0, palette.border));
        let inner = rect.shrink2(Vec2::new(PAD - 2.0, 4.0));
        ui.scope_builder(egui::UiBuilder::new().max_rect(inner).layout(egui::Layout::right_to_left(Align::Center)), |ui| {
            icons::group(ui, |ui| {
                if icons::button(ui, Icon::Comment, None, view == View::Comments, true)
                    .on_hover_text("Comments (Ctrl+Shift+B)")
                    .clicked()
                {
                    view = View::Comments;
                }
                let sections = icons::button(ui, Icon::Sections, None, view == View::Sections, has_sections);
                let sections = if has_sections {
                    sections.on_hover_text("Sections (Ctrl+Shift+B)")
                } else {
                    sections.on_hover_text("This document has no sections")
                };
                if sections.clicked() {
                    view = View::Sections;
                }
                if icons::button(ui, Icon::Pages, None, view == View::Pages, true)
                    .on_hover_text("Pages (Ctrl+Shift+B)")
                    .clicked()
                {
                    view = View::Pages;
                }
            });
            let title = match view {
                View::Pages => "Pages",
                View::Sections => "Sections",
                View::Comments => "Comments",
            };
            // The title, if it fits.
            let galley = ui.painter().layout_no_wrap(title.to_string(), FontId::proportional(13.0), palette.weak);
            if galley.size().x + 8.0 <= ui.available_width() {
                ui.with_layout(egui::Layout::left_to_right(Align::Center), |ui| {
                    ui.add_space(4.0);
                    ui.label(egui::RichText::new(title).size(13.0).color(palette.weak));
                });
            }
        });
        self.set_view(view, pool);
    }

    /// The sections list.
    fn show_sections(&mut self, ui: &mut egui::Ui, order: &[usize], reading: (usize, f32)) -> Option<Action> {
        let items = &self.outline;
        // Page → position (pages may have been moved).
        let mut position = vec![usize::MAX; order.len()];
        for (pos, &page) in order.iter().enumerate() {
            if let Some(slot) = position.get_mut(page) {
                *slot = pos;
            }
        }
        let place = |item: &OutlineItem| match &item.target {
            Some(LinkTarget::Page { page, top }) => {
                position.get(*page).filter(|&&p| p != usize::MAX).map(|&p| (p, top.unwrap_or(0.0)))
            }
            _ => None,
        };

        // Rows shown: everything not inside a folded section.
        let mut rows = Vec::new();
        let mut hidden_below: Option<usize> = None;
        for (i, item) in items.iter().enumerate() {
            if let Some(level) = hidden_below {
                if item.level > level {
                    continue;
                }
                hidden_below = None;
            }
            rows.push(i);
            if !self.expanded[i] {
                hidden_below = Some(item.level);
            }
        }
        let has_children = |i: usize| items.get(i + 1).is_some_and(|next| next.level > items[i].level);

        // The section being read: the last one starting at or before the
        // reading spot (a little slack so a heading at the top of the view
        // counts); if it is folded away, the section it is in.
        let (at_pos, at_y) = reading;
        let mut current = None;
        let mut best = (0usize, f32::MIN);
        for (i, item) in items.iter().enumerate() {
            if let Some((p, y)) = place(item)
                && (p < at_pos || p == at_pos && y <= at_y + 1.0)
                && (p, y) >= best
            {
                best = (p, y);
                current = Some(i);
            }
        }
        match &mut self.clicked {
            // The first frame after the click: the view is where it went.
            Some((i, spot @ None)) => {
                *spot = Some(reading);
                current = Some(*i);
            }
            Some((i, Some((p, y)))) if *p == reading.0 && (*y - reading.1).abs() < 1.0 => current = Some(*i),
            Some(_) => self.clicked = None,
            None => {}
        }
        let current_row = current.map(|mut c| {
            while rows.binary_search(&c).is_err() {
                // Up to the parent: the closest earlier entry one level up.
                let level = items[c].level;
                match (0..c).rev().find(|&j| items[j].level < level) {
                    Some(parent) => c = parent,
                    None => break,
                }
            }
            c
        });
        let follow = current_row != self.shown_section;
        self.shown_section = current_row;

        let palette = crate::theme::palette(ui);
        let width = ui.available_width();
        let total = rows.len() as f32 * ROW_HEIGHT + 2.0 * 6.0;
        let mut action = None;
        let mut toggle = None;
        let mut clicked = None;
        egui::ScrollArea::vertical().id_salt("sections").auto_shrink(false).show_viewport(ui, |ui, viewport| {
            ui.set_min_size(Vec2::new(width, total));
            let origin = ui.max_rect().min + Vec2::new(0.0, 6.0);
            let screen_view = viewport.translate(ui.max_rect().min.to_vec2());
            let painter = ui.painter().clone();
            for (row, &i) in rows.iter().enumerate() {
                let item = &items[i];
                let rect = Rect::from_min_size(
                    origin + Vec2::new(6.0, row as f32 * ROW_HEIGHT),
                    Vec2::new(width - 12.0, ROW_HEIGHT),
                );
                let selected = current_row == Some(i);
                if selected && follow && !screen_view.contains_rect(rect) {
                    ui.scroll_to_rect(rect, Some(Align::Center));
                }
                if !rect.intersects(screen_view) {
                    continue;
                }
                let response = ui.interact(rect, ui.id().with(("section", i)), Sense::click());
                let indent = rect.left() + 4.0 + item.level as f32 * INDENT;
                let arrow = Rect::from_min_size(Pos2::new(indent, rect.top()), Vec2::new(16.0, ROW_HEIGHT));
                let on_arrow = has_children(i) && response.hover_pos().is_some_and(|p| arrow.contains(p));
                if selected {
                    painter.rect_filled(rect, 7.0, palette.accent_soft);
                } else if response.hovered() {
                    painter.rect_filled(rect, 7.0, palette.hover);
                }
                if response.hovered() {
                    ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
                }
                let color = if selected { palette.accent } else { palette.text };
                if has_children(i) {
                    let c = arrow.center();
                    let points = if self.expanded[i] {
                        vec![c + Vec2::new(-3.5, -1.5), c + Vec2::new(0.0, 2.0), c + Vec2::new(3.5, -1.5)]
                    } else {
                        vec![c + Vec2::new(-1.5, -3.5), c + Vec2::new(2.0, 0.0), c + Vec2::new(-1.5, 3.5)]
                    };
                    let arrow_color = if on_arrow { palette.text } else { palette.weak };
                    painter.add(egui::Shape::line(points, Stroke::new(1.5, arrow_color)));
                }
                // Page number on the right, the title in what's left.
                let number = place(item).map(|(p, _)| {
                    painter.layout_no_wrap((p + 1).to_string(), FontId::proportional(11.5), palette.weak)
                });
                let number_w = number.as_ref().map_or(0.0, |n| n.size().x + 8.0);
                let text_left = indent + 18.0;
                let title = if item.title.is_empty() { "Untitled" } else { &item.title };
                let size = if item.level == 0 { 13.5 } else { 13.0 };
                let mut job = egui::text::LayoutJob::single_section(
                    title.to_string(),
                    egui::TextFormat::simple(FontId::proportional(size), color),
                );
                job.wrap = egui::text::TextWrapping::truncate_at_width((rect.right() - 6.0 - number_w - text_left).max(10.0));
                let galley = painter.layout_job(job);
                let elided = galley.elided;
                painter.galley(Pos2::new(text_left, rect.center().y - galley.size().y / 2.0), galley, color);
                if let Some(n) = number {
                    let at = Pos2::new(rect.right() - 6.0 - n.size().x, rect.center().y - n.size().y / 2.0);
                    painter.galley_with_override_text_color(at, n, if selected { palette.accent } else { palette.weak });
                }
                let response = if elided { response.on_hover_text(title) } else { response };
                if response.clicked() {
                    if on_arrow || item.target.is_none() {
                        toggle = Some(i);
                    } else if let Some(target) = &item.target {
                        action = Some(Action::Section(target.clone()));
                        clicked = Some(i);
                    }
                }
                if response.double_clicked() && has_children(i) && !on_arrow {
                    toggle = Some(i);
                }
            }
        });
        if let Some(i) = toggle
            && has_children(i)
        {
            self.expanded[i] = !self.expanded[i];
        }
        if let Some(i) = clicked {
            self.clicked = Some((i, None));
        }
        action
    }

    /// The page thumbnails.
    fn show_pages(
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
                painter.rect_filled(rect, 0.0, crate::theme::paper(crate::theme::dark_pages()).gamma_multiply(tint.r() as f32 / 255.0));
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
