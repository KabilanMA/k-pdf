//! Comments on pages: highlights over text and notes anywhere, each with
//! an optional comment (stored in the file as standard PDF annotations;
//! see `k_pdf::edit::annotate`).
//!
//! Selecting text shows a small bar above it: a color highlights the
//! text, "Comment" highlights it and opens the comment box. Clicking a
//! highlight or note opens its comment box, to read, change or delete it.
//! The Comment tool in the toolbar places a note where the page is
//! clicked next. Comments are read from the editor's copy of the document
//! (which has the unsaved changes), page by page, as needed.

use std::collections::HashMap;
use std::sync::Arc;

use eframe::egui::{self, Align2, Color32, CornerRadius, Id, Order, Pos2, Rect, Sense, Stroke, Vec2};
use k_pdf::edit::annotate::{self, Comment, CommentKind};
use k_pdf::{Changes, ObjRef, TextChar};

use crate::editor::Editor;
use crate::icons::{self, Icon};
use crate::theme;

/// Highlight colors offered, with their names.
pub const COLORS: [([u8; 3], &str); 4] = [
    (annotate::HIGHLIGHT_YELLOW, "yellow"),
    ([140, 228, 120], "green"),
    ([125, 190, 255], "blue"),
    ([255, 145, 195], "pink"),
];

/// Width of the comment box.
const POPUP_WIDTH: f32 = 280.0;

/// The name comments are signed with: the user's login name.
pub fn author() -> String {
    std::env::var("USER").or_else(|_| std::env::var("USERNAME")).unwrap_or_default()
}

pub fn color32([r, g, b]: [u8; 3]) -> Color32 {
    Color32::from_rgb(r, g, b)
}

pub fn kind_name(kind: &CommentKind) -> String {
    match kind {
        CommentKind::Highlight => "Highlight".into(),
        CommentKind::Note => "Note".into(),
        CommentKind::Other(k) => match k.as_str() {
            "StrikeOut" => "Strike-out".into(),
            "FreeText" => "Text box".into(),
            "Square" => "Rectangle".into(),
            "Circle" => "Ellipse".into(),
            "PolyLine" => "Lines".into(),
            "Ink" => "Drawing".into(),
            "FileAttachment" => "Attachment".into(),
            other => other.to_string(),
        },
    }
}

/// `D:20261106140312Z` → `2026-11-06 14:03`.
pub fn short_date(raw: &str) -> String {
    let full = crate::worker::pretty_date(raw);
    // Drop the seconds and time zone.
    match full.char_indices().filter(|&(_, c)| c == ':').nth(1) {
        Some((i, _)) => full[..i].to_string(),
        None => full,
    }
}

/// The text under a highlight's areas (display points).
pub fn excerpt(chars: &[TextChar], areas: &[[f32; 4]]) -> String {
    let inside = |c: &TextChar| {
        c.rect.is_some_and(|[l, t, r, b]| {
            let (x, y) = ((l + r) / 2.0, (t + b) / 2.0);
            areas.iter().any(|a| x >= a[0] - 1.0 && x <= a[2] + 1.0 && y >= a[1] - 1.0 && y <= a[3] + 1.0)
        })
    };
    let mut out = String::new();
    for c in chars {
        if inside(c) {
            out.push_str(&c.text);
        } else if !out.ends_with(' ') {
            out.push(' ');
        }
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The comment box that is open.
pub struct Open {
    pub id: ObjRef,
    /// Page of the open file.
    pub page: usize,
    pub text: String,
    original: String,
    /// The changes from before it was opened (or created), for undo.
    pub before: Changes,
    /// Created along with opening it (a new note, or "Comment" on a
    /// selection): undo removes it too.
    pub created: bool,
    /// Focus the text box on the next frame.
    focus: bool,
}

impl Open {
    pub fn changed(&self) -> bool {
        self.text != self.original
    }
}

pub struct Comments {
    /// By page of the open file.
    by_page: HashMap<usize, Arc<Vec<Comment>>>,
    all: Option<Arc<Vec<Comment>>>,
    /// Highlighted text, by comment.
    excerpts: HashMap<ObjRef, String>,
    pub open: Option<Open>,
    /// The Comment tool is on: a click on a page places a note.
    pub placing: bool,
    /// The last highlight color used.
    pub color: [u8; 3],
}

impl Default for Comments {
    fn default() -> Self {
        Comments {
            by_page: HashMap::new(),
            all: None,
            excerpts: HashMap::new(),
            open: None,
            placing: false,
            color: annotate::HIGHLIGHT_YELLOW,
        }
    }
}

impl Comments {
    /// Starts over (another file was opened), keeping the color.
    pub fn reset(&mut self) {
        *self = Comments { color: self.color, ..Comments::default() };
    }

    /// Forgets what was read (the comments changed).
    pub fn invalidate(&mut self) {
        self.by_page.clear();
        self.all = None;
        self.excerpts.clear();
    }

    pub fn on_page(&mut self, editor: &Editor, page: usize) -> Arc<Vec<Comment>> {
        self.by_page
            .entry(page)
            .or_insert_with(|| {
                Arc::new(editor.document().and_then(|d| annotate::page_comments(d, page).ok()).unwrap_or_default())
            })
            .clone()
    }

    /// Every comment of the open file.
    pub fn all(&mut self, editor: &Editor) -> Arc<Vec<Comment>> {
        self.all
            .get_or_insert_with(|| {
                Arc::new(editor.document().and_then(|d| annotate::comments(d).ok()).unwrap_or_default())
            })
            .clone()
    }

    pub fn find(&mut self, editor: &Editor, page: usize, id: ObjRef) -> Option<Comment> {
        self.on_page(editor, page).iter().find(|c| c.id == id).cloned()
    }

    /// The comment at `p` (display points) on `page`: the top one.
    pub fn at(&mut self, editor: &Editor, page: usize, p: Pos2) -> Option<Comment> {
        self.on_page(editor, page)
            .iter()
            .rev()
            .find(|c| c.areas.iter().any(|a| p.x >= a[0] - 1.0 && p.x <= a[2] + 1.0 && p.y >= a[1] - 1.0 && p.y <= a[3] + 1.0))
            .cloned()
    }

    /// A comment's text changed (while typing): the lists show it.
    pub fn update_text(&mut self, id: ObjRef, text: &str) {
        let lists = self.by_page.values_mut().chain(self.all.iter_mut());
        for list in lists {
            if list.iter().any(|c| c.id == id) {
                let list = Arc::make_mut(list);
                for c in list.iter_mut().filter(|c| c.id == id) {
                    c.text = text.to_string();
                }
            }
        }
    }

    /// The highlighted text of a highlight, once its page's text is known.
    pub fn excerpt(&mut self, c: &Comment, chars: Option<&[TextChar]>) -> Option<String> {
        if c.kind != CommentKind::Highlight {
            return None;
        }
        if let Some(e) = self.excerpts.get(&c.id) {
            return Some(e.clone());
        }
        let e = excerpt(chars?, &c.areas);
        self.excerpts.insert(c.id, e.clone());
        Some(e)
    }

    pub fn start(&mut self, c: &Comment, before: Changes, created: bool, focus: bool) {
        self.open = Some(Open {
            id: c.id,
            page: c.page,
            text: c.text.clone(),
            original: c.text.clone(),
            before,
            created,
            focus,
        });
    }
}

/// What the user did in the selection bar.
pub enum BarAction {
    Highlight([u8; 3]),
    Comment,
}

/// The bar shown over selected text. `anchor` is the selection's first
/// line on screen, `view` the visible part of the pages.
pub fn selection_bar(ctx: &egui::Context, anchor: Rect, view: Rect, current: [u8; 3]) -> Option<BarAction> {
    let above = anchor.top() - 8.0;
    let (pos, pivot) = if above - 44.0 > view.top() {
        (Pos2::new(anchor.center().x, above), Align2::CENTER_BOTTOM)
    } else {
        (Pos2::new(anchor.center().x, anchor.bottom() + 8.0), Align2::CENTER_TOP)
    };
    let pos = Pos2::new(pos.x.clamp(view.left() + 120.0, (view.right() - 120.0).max(view.left() + 120.0)), pos.y);
    let mut action = None;
    egui::Area::new(Id::new("selection-bar")).order(Order::Foreground).fixed_pos(pos).pivot(pivot).show(ctx, |ui| {
        theme::floating_frame(ui).inner_margin(egui::Margin::symmetric(8, 4)).show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 4.0;
                let p = theme::palette(ui);
                icons::paint(
                    ui.painter(),
                    Rect::from_center_size(ui.cursor().min + Vec2::new(9.0, 15.0), Vec2::splat(16.0)),
                    Icon::Highlight,
                    p.weak,
                );
                ui.add_space(22.0);
                for (color, name) in COLORS {
                    let (rect, response) = ui.allocate_exact_size(Vec2::splat(26.0), Sense::click());
                    let response = response.on_hover_text(format!("Highlight in {name} (Ctrl+Shift+H: last color)"));
                    let painter = ui.painter();
                    if response.hovered() {
                        painter.rect_filled(rect, CornerRadius::same(7), p.hover);
                    }
                    painter.circle_filled(rect.center(), 8.0, color32(color));
                    painter.circle_stroke(rect.center(), 8.0, Stroke::new(1.0, Color32::from_black_alpha(40)));
                    if color == current {
                        painter.circle_stroke(rect.center(), 11.0, Stroke::new(1.5, p.accent));
                    }
                    if response.clicked() {
                        action = Some(BarAction::Highlight(color));
                    }
                    response.on_hover_cursor(egui::CursorIcon::PointingHand);
                }
                ui.add_space(2.0);
                let (r, _) = ui.allocate_exact_size(Vec2::new(1.0, 20.0), Sense::hover());
                ui.painter().vline(r.center().x, r.y_range(), Stroke::new(1.0, p.border));
                ui.add_space(2.0);
                if icons::button(ui, Icon::Comment, Some("Comment"), false, true)
                    .on_hover_text("Highlight the text and add a comment")
                    .clicked()
                {
                    action = Some(BarAction::Comment);
                }
            });
        });
    });
    action
}

/// What the user did in the comment box.
#[derive(PartialEq)]
pub enum PopupAction {
    None,
    /// The text changed (it is in `open.text`).
    Edited,
    Close,
    Delete,
}

/// The comment box, next to `anchor` (the comment on screen) within
/// `view`; `None` when the comment is scrolled out of view (the box
/// waits, hidden).
pub fn popup(ctx: &egui::Context, open: &mut Open, comment: &Comment, anchor: Option<Rect>, view: Rect) -> PopupAction {
    let Some(anchor) = anchor else { return PopupAction::None };
    // To the right of the comment if there is room, else to its left; at
    // its top, kept on screen.
    let x = if anchor.right() + 12.0 + POPUP_WIDTH + 28.0 < view.right() {
        anchor.right() + 12.0
    } else {
        (anchor.left() - 12.0 - POPUP_WIDTH - 28.0).max(view.left() + 8.0)
    };
    let y = anchor.top().clamp(view.top() + 8.0, (view.bottom() - 220.0).max(view.top() + 8.0));
    let mut action = PopupAction::None;
    let area = egui::Area::new(Id::new("comment-popup")).order(Order::Foreground).fixed_pos(Pos2::new(x, y)).show(ctx, |ui| {
        theme::floating_frame(ui).inner_margin(egui::Margin::same(12)).show(ui, |ui| {
            ui.set_width(POPUP_WIDTH);
            let p = theme::palette(ui);
            ui.horizontal(|ui| {
                let (r, _) = ui.allocate_exact_size(Vec2::splat(12.0), Sense::hover());
                ui.painter().circle_filled(r.center(), 5.5, color32(comment.color));
                ui.label(egui::RichText::new(kind_name(&comment.kind)).strong());
                let mut by = comment.author.clone();
                if let Some(d) = &comment.modified {
                    if !by.is_empty() {
                        by += " · ";
                    }
                    by += &short_date(d);
                }
                ui.label(egui::RichText::new(by).size(12.0).color(p.weak));
            });
            ui.add_space(4.0);
            let edit = egui::TextEdit::multiline(&mut open.text)
                .hint_text("Add a comment…")
                .desired_rows(3)
                .desired_width(f32::INFINITY);
            let response = ui.add(edit);
            if open.focus {
                response.request_focus();
                open.focus = false;
            }
            if response.changed() {
                action = PopupAction::Edited;
            }
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                if theme::danger_button(ui, "Delete", true).clicked() {
                    action = PopupAction::Delete;
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if theme::primary_button(ui, "Done").on_hover_text("Ctrl+Enter").clicked() {
                        action = PopupAction::Close;
                    }
                });
            });
        });
    });
    let rect = area.response.rect;
    let (outside, escape, done) = ctx.input(|i| {
        let pressed = i.pointer.any_pressed();
        let at = i.pointer.interact_pos();
        (
            pressed && at.is_some_and(|p| !rect.contains(p) && !anchor.contains(p)),
            i.key_pressed(egui::Key::Escape),
            i.modifiers.command && i.key_pressed(egui::Key::Enter),
        )
    });
    if action == PopupAction::None && (outside || escape || done) {
        action = PopupAction::Close;
    }
    action
}
