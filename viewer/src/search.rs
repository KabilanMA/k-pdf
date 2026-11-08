//! Finding words in the document (Ctrl+F).
//!
//! The search bar floats at the top right of the pages. Typing searches
//! every page in the background (`worker::Pool::search`): each page's text
//! is matched as it comes, and pages with images also have the text in
//! their images read (OCR, one page at a time) and matched approximately,
//! since reading now and then gets a letter wrong. Matches are counted and
//! highlighted on the pages; Enter and the arrows go from one to the next
//! in page order, the current one shown in a stronger color.

use std::collections::{HashMap, HashSet};

use eframe::egui::{self, Color32, Key, Pos2, Rect, RichText, Vec2};
use k_pdf::search::{Match, Query};

use crate::icons::{self, Icon};
use crate::theme;

/// Matches, and the current one.
pub const MATCH_COLOR: Color32 = Color32::from_rgba_premultiplied(120, 96, 0, 110);
pub const CURRENT_COLOR: Color32 = Color32::from_rgba_premultiplied(190, 90, 0, 170);

#[derive(Default)]
pub struct Search {
    pub open: bool,
    pub text: String,
    pub match_case: bool,
    pub whole_words: bool,
    /// Also read the text in images (on by default).
    pub skip_images: bool,
    /// Put the cursor in the search field next frame.
    focus: bool,
    /// The search running now (replies of older ones are dropped).
    pub id: u64,
    pub query: Option<Query>,
    /// Matches in each page's own text, and in its images, by page key.
    pub in_text: HashMap<usize, Vec<Match>>,
    pub in_images: HashMap<usize, Vec<Match>>,
    /// Pages searched so far, of how many.
    pub searched: usize,
    /// … and which.
    pub searched_keys: HashSet<usize>,
    pub total: usize,
    /// Pages whose images are being read for this search.
    pub reading: HashSet<usize>,
    /// The current match: page key and its index among the page's matches
    /// (see [`Search::on_page`]).
    pub current: Option<(usize, usize)>,
    /// Until a match is current: the position to look for the first one
    /// from (the page being read when the search started).
    pub seek_from: Option<usize>,
    /// Reading images failed (shown once).
    pub ocr_error: Option<String>,
    /// The document changed: search again.
    pub dirty: bool,
}

/// What the bar asks of the viewer.
pub enum Action {
    /// The phrase or an option changed: search again.
    Changed,
    Next,
    Previous,
    Close,
}

impl Search {
    pub fn show(&mut self) {
        self.open = true;
        self.focus = true;
    }

    /// Forgets the results (the bar keeps its text).
    pub fn clear_results(&mut self) {
        self.in_text.clear();
        self.in_images.clear();
        self.reading.clear();
        self.current = None;
        self.searched = 0;
        self.searched_keys.clear();
        self.total = 0;
        self.ocr_error = None;
    }

    /// A page's matches, top to bottom, and whether each was found in an
    /// image.
    pub fn on_page(&self, key: usize) -> Vec<(&Match, bool)> {
        let mut all: Vec<(&Match, bool)> = self.in_text.get(&key).into_iter().flatten().map(|m| (m, false)).collect();
        all.extend(self.in_images.get(&key).into_iter().flatten().map(|m| (m, true)));
        let top = |m: &Match| m.rects.first().map_or([0.0; 2], |r| [r[1], r[0]]);
        all.sort_by(|a, b| {
            let (a, b) = (top(a.0), top(b.0));
            // Same line: left to right.
            if (a[0] - b[0]).abs() < 3.0 { a[1].total_cmp(&b[1]) } else { a[0].total_cmp(&b[0]) }
        });
        all
    }

    fn count_on(&self, key: usize) -> usize {
        self.in_text.get(&key).map_or(0, Vec::len) + self.in_images.get(&key).map_or(0, Vec::len)
    }

    /// Every match in on-screen order: (page key, index on the page).
    pub fn ordered(&self, order: &[usize]) -> Vec<(usize, usize)> {
        order.iter().flat_map(|&key| (0..self.count_on(key)).map(move |i| (key, i))).collect()
    }

    pub fn count(&self, order: &[usize]) -> usize {
        order.iter().map(|&k| self.count_on(k)).sum()
    }

    /// The match after (or before) the current one, going round; without a
    /// current one, the first from `from` (a position) on.
    pub fn step(&self, order: &[usize], forward: bool, from: usize) -> Option<(usize, usize)> {
        let all = self.ordered(order);
        if all.is_empty() {
            return None;
        }
        let i = match self.current.and_then(|c| all.iter().position(|&m| m == c)) {
            Some(i) if forward => (i + 1) % all.len(),
            Some(i) => (i + all.len() - 1) % all.len(),
            None => {
                let position = |key: usize| order.iter().position(|&k| k == key).unwrap_or(0);
                let after = all.iter().position(|&(k, _)| position(k) >= from);
                match (after, forward) {
                    (Some(i), true) => i,
                    (Some(0) | None, false) => all.len() - 1,
                    (Some(i), false) => i - 1,
                    (None, true) => 0,
                }
            }
        };
        Some(all[i])
    }

    /// The search bar, at the top right of `view`.
    pub fn bar(&mut self, ctx: &egui::Context, view: Rect, order: &[usize], ocr: bool) -> Option<Action> {
        if !self.open || !view.is_positive() {
            return None;
        }
        let mut action = None;
        egui::Area::new(egui::Id::new("search-bar"))
            .order(egui::Order::Foreground)
            .fixed_pos(Pos2::new(view.right() - 14.0, view.top() + 10.0))
            .pivot(egui::Align2::RIGHT_TOP)
            .show(ctx, |ui| {
                theme::floating_frame(ui).show(ui, |ui| {
                    let p = theme::palette(ui);
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 4.0;
                        let width = (view.width() - 280.0).clamp(90.0, 230.0);
                        let field = ui.add(
                            egui::TextEdit::singleline(&mut self.text)
                                .hint_text("Find in document")
                                .desired_width(width)
                                .margin(egui::Margin::symmetric(6, 4)),
                        );
                        if std::mem::take(&mut self.focus) {
                            field.request_focus();
                            // Select what's there, to type over it.
                            if let Some(mut state) = egui::TextEdit::load_state(ui.ctx(), field.id) {
                                let all = egui::text::CCursorRange::two(
                                    egui::text::CCursor::new(0),
                                    egui::text::CCursor::new(self.text.chars().count()),
                                );
                                state.cursor.set_char_range(Some(all));
                                state.store(ui.ctx(), field.id);
                            }
                        }
                        if field.changed() {
                            action = Some(Action::Changed);
                        }
                        // Enter: next (Shift: previous), staying in the field.
                        if field.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
                            action = Some(if ui.input(|i| i.modifiers.shift) { Action::Previous } else { Action::Next });
                            self.focus = true;
                        }
                        if field.has_focus() && ui.input(|i| i.key_pressed(Key::Escape)) {
                            action = Some(Action::Close);
                        }

                        let total = self.count(order);
                        let done = self.searched >= self.total && self.reading.is_empty();
                        let label = if self.query.is_none() {
                            String::new()
                        } else if total == 0 {
                            if done { "No matches".into() } else { "…".into() }
                        } else {
                            let at = self.current.and_then(|c| self.ordered(order).iter().position(|&m| m == c));
                            let plus = if done { "" } else { "+" };
                            match at {
                                Some(i) => format!("{} of {total}{plus}", i + 1),
                                None => format!("{total}{plus}"),
                            }
                        };
                        ui.add_sized(
                            Vec2::new(64.0, 24.0),
                            egui::Label::new(RichText::new(label).size(12.0).color(if total == 0 && done { p.danger } else { p.weak })),
                        );
                        let any = total > 0;
                        if icons::button(ui, Icon::Up, None, false, any).on_hover_text("Previous (Shift+Enter, Shift+F3)").clicked() {
                            action = Some(Action::Previous);
                        }
                        if icons::button(ui, Icon::Down, None, false, any).on_hover_text("Next (Enter, F3)").clicked() {
                            action = Some(Action::Next);
                        }
                        let toggle = |ui: &mut egui::Ui, on: &mut bool, text: &str, tip: &str| {
                            let r = ui.add(egui::Button::selectable(*on, RichText::new(text).size(12.0))).on_hover_text(tip);
                            if r.clicked() {
                                *on = !*on;
                                true
                            } else {
                                false
                            }
                        };
                        if toggle(ui, &mut self.match_case, "Aa", "Match case") {
                            action = Some(Action::Changed);
                        }
                        if toggle(ui, &mut self.whole_words, "ab|", "Whole words only") {
                            action = Some(Action::Changed);
                        }
                        let mut images = !self.skip_images;
                        let tip = if ocr {
                            "Also find words in images: scanned pages, photos and pictures (read with text recognition, which can get a letter wrong now and then)"
                        } else {
                            "Finding words in images needs the text-recognition models, which aren't installed"
                        };
                        if ui.add_enabled_ui(ocr, |ui| toggle(ui, &mut images, "Images", tip)).inner {
                            self.skip_images = !images;
                            action = Some(Action::Changed);
                        }
                        if icons::button(ui, Icon::Close, None, false, true).on_hover_text("Close (Esc)").clicked() {
                            action = Some(Action::Close);
                        }
                    });
                    // What's still going on.
                    let status = if self.query.is_none() {
                        None
                    } else if self.searched < self.total {
                        Some(format!("Searching… page {} of {}", self.searched, self.total))
                    } else if !self.reading.is_empty() {
                        let n = self.reading.len();
                        Some(format!("Reading text in images: {n} page{} to go", if n == 1 { "" } else { "s" }))
                    } else {
                        self.ocr_error.as_ref().map(|e| format!("Could not read text in images: {e}"))
                    };
                    if let Some(status) = status {
                        ui.add(egui::Label::new(RichText::new(status).size(11.5).color(p.weak)).wrap());
                    }
                });
            });
        action
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn m(top: f32) -> Match {
        Match { rects: vec![[0.0, top, 10.0, top + 10.0]], chars: 0..1 }
    }

    #[test]
    fn steps_through_matches_in_page_order() {
        let mut s = Search::default();
        s.in_text.insert(5, vec![m(100.0)]);
        s.in_images.insert(5, vec![m(50.0)]);
        s.in_text.insert(2, vec![m(10.0)]);
        // Pages shown in the order 5, 7, 2.
        let order = [5, 7, 2];
        assert_eq!(s.ordered(&order), vec![(5, 0), (5, 1), (2, 0)]);
        // The image match is higher on the page, so it comes first.
        assert!(s.on_page(5)[0].1);
        // From position 1 on, the first match is on page 2 (position 2).
        assert_eq!(s.step(&order, true, 1), Some((2, 0)));
        assert_eq!(s.step(&order, false, 1), Some((5, 1)));
        s.current = Some((2, 0));
        assert_eq!(s.step(&order, true, 0), Some((5, 0)));
        assert_eq!(s.count(&order), 3);
    }
}
