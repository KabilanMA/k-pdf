//! Text selection on a page: hit-testing, word/line ranges and caret
//! movement over the positioned characters from `TextExtractor`.
//!
//! Positions are *carets*: caret `i` sits before character `i`, so a
//! selection is the character range `min(anchor, focus)..max(anchor, focus)`.

use std::ops::Range;

use eframe::egui::{Pos2, Rect};
use k_pdf::TextChar;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Selection {
    pub page: usize,
    /// Where the selection started (stays fixed while extending).
    pub anchor: usize,
    /// The moving end.
    pub focus: usize,
}

impl Selection {
    pub fn range(&self) -> Range<usize> {
        self.anchor.min(self.focus)..self.anchor.max(self.focus)
    }

    pub fn is_empty(&self) -> bool {
        self.anchor == self.focus
    }
}

fn rect_of(c: &TextChar) -> Option<Rect> {
    c.rect.map(|[l, t, r, b]| Rect::from_min_max(Pos2::new(l, t), Pos2::new(r, b)))
}

/// Caret closest to point `p` (display points), or `None` if the page has
/// no positioned text.
pub fn caret_at(chars: &[TextChar], p: Pos2) -> Option<usize> {
    let mut best: Option<(f32, usize, Rect)> = None;
    for (i, c) in chars.iter().enumerate() {
        let Some(r) = rect_of(c) else { continue };
        // Distance to the box; vertical distance counts more, so a point
        // between two lines picks the line it is closest to.
        let dx = (r.left() - p.x).max(p.x - r.right()).max(0.0);
        let dy = (r.top() - p.y).max(p.y - r.bottom()).max(0.0);
        let d = dx + 4.0 * dy;
        if best.is_none_or(|(bd, _, _)| d < bd) {
            best = Some((d, i, r));
        }
    }
    let (_, i, r) = best?;
    Some(if p.x > r.center().x { i + 1 } else { i })
}

/// Whether `p` is over a character (for the text cursor).
pub fn is_over_text(chars: &[TextChar], p: Pos2) -> bool {
    chars.iter().filter_map(rect_of).any(|r| r.expand(1.0).contains(p))
}

fn is_word_char(c: &TextChar) -> bool {
    c.text.chars().any(|ch| ch.is_alphanumeric() || ch == '_' || ch == '\'' || ch == '’')
}

/// The word around character `i` (or just that character if it isn't
/// part of a word).
pub fn word_at(chars: &[TextChar], i: usize) -> Range<usize> {
    if i >= chars.len() {
        return chars.len()..chars.len();
    }
    if !is_word_char(&chars[i]) {
        return i..i + 1;
    }
    let mut start = i;
    while start > 0 && is_word_char(&chars[start - 1]) {
        start -= 1;
    }
    let mut end = i + 1;
    while end < chars.len() && is_word_char(&chars[end]) {
        end += 1;
    }
    start..end
}

/// The line containing character `i`, without its line break.
pub fn line_at(chars: &[TextChar], i: usize) -> Range<usize> {
    let i = i.min(chars.len().saturating_sub(1));
    let start = chars[..i].iter().rposition(|c| c.text == "\n").map_or(0, |p| p + 1);
    let end = chars[i..].iter().position(|c| c.text == "\n").map_or(chars.len(), |p| i + p);
    start..end
}

#[derive(Clone, Copy, Debug)]
pub enum Move {
    Left,
    Right,
    Up,
    Down,
    LineStart,
    LineEnd,
}

/// Moves a caret, as Shift+arrow keys do in a text editor.
pub fn move_caret(chars: &[TextChar], caret: usize, m: Move) -> usize {
    let n = chars.len();
    match m {
        Move::Left => caret.saturating_sub(1),
        Move::Right => (caret + 1).min(n),
        Move::LineStart => line_at(chars, caret).start,
        Move::LineEnd => line_at(chars, caret).end,
        Move::Up | Move::Down => {
            if n == 0 {
                return 0;
            }
            let line = line_at(chars, caret.min(n - 1));
            let x = caret_x(chars, caret);
            let target = match m {
                Move::Up if line.start > 0 => line_at(chars, line.start - 1),
                Move::Down if line.end + 1 < n => line_at(chars, line.end + 1),
                Move::Up => return 0,
                _ => return n,
            };
            // The caret in the target line closest to the same x.
            let Some(x) = x else { return target.start };
            (target.start..=target.end)
                .min_by(|&a, &b| {
                    let da = caret_x(chars, a).map_or(f32::MAX, |v| (v - x).abs());
                    let db = caret_x(chars, b).map_or(f32::MAX, |v| (v - x).abs());
                    da.total_cmp(&db)
                })
                .unwrap_or(target.start)
        }
    }
}

/// Horizontal position of a caret (left edge of the next character, or the
/// right edge of the previous one).
fn caret_x(chars: &[TextChar], caret: usize) -> Option<f32> {
    chars.get(caret).and_then(|c| c.rect).map(|r| r[0]).or_else(|| {
        let prev = caret.checked_sub(1)?;
        chars.get(prev).and_then(|c| c.rect).map(|r| r[2])
    })
}

/// The selected text, with surrounding whitespace trimmed.
pub fn selected_text(chars: &[TextChar], range: Range<usize>) -> String {
    let end = range.end.min(chars.len());
    let start = range.start.min(end);
    k_pdf::chars_to_string(&chars[start..end]).trim().to_string()
}

/// Highlight rectangles for a range: neighbouring characters on the same
/// line are merged so the highlight has no seams between letters.
pub fn highlight_rects(chars: &[TextChar], range: Range<usize>) -> Vec<Rect> {
    let mut out: Vec<Rect> = Vec::new();
    let mut current: Option<Rect> = None;
    for c in chars.get(range).unwrap_or_default() {
        if c.text == "\n" {
            out.extend(current.take());
            continue;
        }
        let Some(r) = rect_of(c) else { continue };
        current = Some(match current {
            // Same line: the boxes overlap vertically by more than half.
            Some(cur) if overlap(cur, r) > 0.5 * r.height().min(cur.height()) => cur.union(r),
            Some(cur) => {
                out.push(cur);
                r
            }
            None => r,
        });
    }
    out.extend(current);
    out
}

fn overlap(a: Rect, b: Rect) -> f32 {
    (a.bottom().min(b.bottom()) - a.top().max(b.top())).max(0.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// "ab cd\nef": 10pt-wide letters, lines at y 0–10 and 20–30.
    fn sample() -> Vec<TextChar> {
        let ch = |t: &str, x: f32, y: f32| TextChar { text: t.into(), rect: Some([x, y, x + 10.0, y + 10.0]) };
        let sep = |t: &str| TextChar { text: t.into(), rect: None };
        vec![
            ch("a", 0.0, 0.0),
            ch("b", 10.0, 0.0),
            sep(" "),
            ch("c", 30.0, 0.0),
            ch("d", 40.0, 0.0),
            sep("\n"),
            ch("e", 0.0, 20.0),
            ch("f", 10.0, 20.0),
        ]
    }

    #[test]
    fn hit_testing() {
        let c = sample();
        assert_eq!(caret_at(&c, Pos2::new(2.0, 5.0)), Some(0)); // left half of "a"
        assert_eq!(caret_at(&c, Pos2::new(8.0, 5.0)), Some(1)); // right half of "a"
        assert_eq!(caret_at(&c, Pos2::new(100.0, 5.0)), Some(5)); // past the line end
        assert_eq!(caret_at(&c, Pos2::new(12.0, 27.0)), Some(7)); // second line
        assert_eq!(caret_at(&[], Pos2::ZERO), None);
        assert!(is_over_text(&c, Pos2::new(35.0, 5.0)));
        assert!(!is_over_text(&c, Pos2::new(35.0, 15.0)));
    }

    #[test]
    fn words_lines_and_text() {
        let c = sample();
        assert_eq!(word_at(&c, 4), 3..5);
        assert_eq!(word_at(&c, 2), 2..3); // the space itself
        assert_eq!(line_at(&c, 1), 0..5);
        assert_eq!(line_at(&c, 7), 6..8);
        assert_eq!(selected_text(&c, 0..8), "ab cd\nef");
        assert_eq!(selected_text(&c, 1..4), "b c");
    }

    #[test]
    fn caret_movement() {
        let c = sample();
        assert_eq!(move_caret(&c, 0, Move::Left), 0);
        assert_eq!(move_caret(&c, 0, Move::Right), 1);
        assert_eq!(move_caret(&c, 1, Move::Down), 7); // x = 10 → before "f"
        assert_eq!(move_caret(&c, 7, Move::Up), 1);
        assert_eq!(move_caret(&c, 7, Move::Down), 8); // last line → end
        assert_eq!(move_caret(&c, 3, Move::LineStart), 0);
        assert_eq!(move_caret(&c, 3, Move::LineEnd), 5);
    }

    #[test]
    fn highlights_merge_per_line() {
        let c = sample();
        let r = highlight_rects(&c, 0..8);
        assert_eq!(r.len(), 2);
        assert_eq!((r[0].left(), r[0].right()), (0.0, 50.0));
        assert_eq!((r[1].top(), r[1].right()), (20.0, 20.0));
    }
}
