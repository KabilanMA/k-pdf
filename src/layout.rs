//! Reading-order analysis for positioned text.
//!
//! Content streams draw text in whatever order the producer chose. A line of
//! a two-column paper may come out together with the margin line number next
//! to it ("… including compu- 2"), so selecting a few lines would also pick
//! up the line numbers. [`reading_order`] splits each extracted line at large
//! horizontal gaps, groups the pieces into blocks (columns, paragraphs,
//! margin notes) and orders the blocks the way a person reads them.

use crate::text::TextChar;

/// A gap wider than this many times the text height splits a line. Word
/// spaces in justified text stay well below one em.
const SPLIT_GAP: f32 = 1.2;
/// Pages with more blocks than this (large tables, scattered labels) are
/// left in content-stream order.
const MAX_BLOCKS: usize = 2000;

#[derive(Clone, Copy, Debug)]
struct BBox {
    left: f32,
    top: f32,
    right: f32,
    bottom: f32,
}

impl BBox {
    fn new([left, top, right, bottom]: [f32; 4]) -> Self {
        BBox { left, top, right, bottom }
    }
    fn height(&self) -> f32 {
        self.bottom - self.top
    }
    fn union(&mut self, o: BBox) {
        self.left = self.left.min(o.left);
        self.top = self.top.min(o.top);
        self.right = self.right.max(o.right);
        self.bottom = self.bottom.max(o.bottom);
    }
    fn x_overlap(&self, o: &BBox) -> f32 {
        self.right.min(o.right) - self.left.max(o.left)
    }
    fn y_overlap(&self, o: &BBox) -> f32 {
        self.bottom.min(o.bottom) - self.top.max(o.top)
    }
}

/// A run of characters on one line with no large gap inside.
struct Segment {
    chars: Vec<TextChar>,
    bbox: BBox,
}

struct Block {
    segments: Vec<Segment>,
    bbox: BBox,
}

impl Block {
    /// Margin decoration such as review line numbers or page numbers:
    /// every line is just a number.
    fn is_numbering(&self) -> bool {
        self.segments.iter().all(|s| {
            let text: String = s.chars.iter().map(|c| c.text.as_str()).collect();
            let text = text.trim();
            !text.is_empty() && text.len() <= 4 && text.chars().all(|c| c.is_ascii_digit())
        })
    }
}

/// Reorders characters into reading order: blocks of text one after the
/// other, each line ending in a `"\n"`. Characters keep their rectangles;
/// only the order and the inserted line breaks change.
pub fn reading_order(chars: &[TextChar]) -> Vec<TextChar> {
    let segments = segments(chars);
    let blocks = blocks(segments);
    if blocks.len() > MAX_BLOCKS {
        return chars.to_vec();
    }
    let mut out = Vec::with_capacity(chars.len() + blocks.len());
    for i in order(&blocks) {
        for seg in &blocks[i].segments {
            out.extend(seg.chars.iter().cloned());
            out.push(TextChar { text: "\n".into(), rect: None });
        }
    }
    out.pop();
    out
}

/// Splits the extracted lines into segments at line breaks, wide gaps and
/// jumps back to the left.
fn segments(chars: &[TextChar]) -> Vec<Segment> {
    let mut out = Vec::new();
    let mut current: Vec<TextChar> = Vec::new();
    let mut bbox: Option<BBox> = None;
    let mut prev: Option<BBox> = None;

    let mut finish = |current: &mut Vec<TextChar>, bbox: &mut Option<BBox>| {
        // Inserted spaces at the ends belong to neither neighbour.
        while current.last().is_some_and(|c| c.rect.is_none()) {
            current.pop();
        }
        if let Some(b) = bbox.take() {
            out.push(Segment { chars: std::mem::take(current), bbox: b });
        }
        current.clear();
    };

    for c in chars {
        let Some(r) = c.rect.map(BBox::new) else {
            if c.text == "\n" {
                finish(&mut current, &mut bbox);
                prev = None;
            } else if bbox.is_some() {
                current.push(c.clone());
            }
            continue;
        };
        if let Some(p) = prev {
            let h = p.height().max(r.height()).max(1.0);
            let gap = r.left - p.right;
            if gap > SPLIT_GAP * h || r.left < p.left - 0.5 * h {
                finish(&mut current, &mut bbox);
            }
        }
        current.push(c.clone());
        match &mut bbox {
            Some(b) => b.union(r),
            None => bbox = Some(r),
        }
        prev = Some(r);
    }
    finish(&mut current, &mut bbox);
    out
}

/// Groups segments into blocks: a segment continues a block when it sits
/// just below the block's last line and overlaps it horizontally.
fn blocks(segments: Vec<Segment>) -> Vec<Block> {
    let mut blocks: Vec<Block> = Vec::new();
    for seg in segments {
        let s = seg.bbox;
        let target = blocks.iter().rposition(|b| {
            let last = b.segments.last().map_or(b.bbox, |l| l.bbox);
            let h = last.height().max(s.height()).max(1.0);
            let below = s.top >= last.top + 0.5 * last.height().min(s.height());
            let close = s.top - last.bottom <= h;
            below && close && s.x_overlap(&last) > 0.0
        });
        match target {
            Some(i) => {
                blocks[i].bbox.union(s);
                blocks[i].segments.push(seg);
            }
            None => blocks.push(Block { bbox: s, segments: vec![seg] }),
        }
    }
    blocks
}

/// Reading order of the blocks. A block comes after any block above it in
/// the same column and after any block to its left on the same rows; ties
/// keep the content-stream order. Line and page numbers go last.
fn order(blocks: &[Block]) -> Vec<usize> {
    let n = blocks.len();
    let numbering: Vec<bool> = blocks.iter().map(Block::is_numbering).collect();
    let tol = 2.0;
    // before[b] = blocks that must come before b.
    let mut before: Vec<Vec<usize>> = vec![Vec::new(); n];
    for (a, ba) in blocks.iter().enumerate() {
        if numbering[a] {
            continue;
        }
        for (b, bb) in blocks.iter().enumerate() {
            if a == b || numbering[b] {
                continue;
            }
            let (a_box, b_box) = (ba.bbox, bb.bbox);
            let above = a_box.x_overlap(&b_box) > 0.0 && a_box.bottom <= b_box.top + tol;
            let left_of = a_box.right <= b_box.left + tol && a_box.y_overlap(&b_box) > 0.0;
            if above || left_of {
                before[b].push(a);
            }
        }
    }

    let mut done = vec![false; n];
    let mut out = Vec::with_capacity(n);
    let text_blocks: Vec<usize> = (0..n).filter(|&i| !numbering[i]).collect();
    while out.len() < text_blocks.len() {
        let ready = text_blocks.iter().copied().find(|&b| !done[b] && before[b].iter().all(|&a| done[a]));
        // A cycle (odd layouts): fall back to content-stream order.
        let next = ready.unwrap_or_else(|| text_blocks.iter().copied().find(|&b| !done[b]).unwrap());
        done[next] = true;
        out.push(next);
    }
    out.extend((0..n).filter(|&i| numbering[i]));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Lays out `text` from `x` at height `y`, one 5-point-wide character
    /// each, spaces as inserted (rectless) characters.
    fn line(out: &mut Vec<TextChar>, text: &str, x: f32, y: f32) {
        let mut x = x;
        for ch in text.chars() {
            if ch == ' ' {
                out.push(TextChar { text: " ".into(), rect: None });
                x += 3.0;
            } else {
                out.push(TextChar { text: ch.into(), rect: Some([x, y, x + 5.0, y + 10.0]) });
                x += 5.0;
            }
        }
    }

    fn text(chars: &[TextChar]) -> String {
        crate::text::chars_to_string(chars)
    }

    fn newline(out: &mut Vec<TextChar>) {
        out.push(TextChar { text: "\n".into(), rect: None });
    }

    #[test]
    fn margin_line_numbers_are_moved_out_of_the_text() {
        let mut c = Vec::new();
        // Text at x = 50, its line number drawn afterwards in the left margin.
        for (i, words) in ["first line", "second line", "third line"].iter().enumerate() {
            let y = 100.0 + 12.0 * i as f32;
            line(&mut c, words, 50.0, y);
            c.push(TextChar { text: " ".into(), rect: None });
            line(&mut c, &(i + 1).to_string(), 20.0, y);
            newline(&mut c);
        }
        assert_eq!(text(&reading_order(&c)), "first line\nsecond line\nthird line\n1\n2\n3");
    }

    #[test]
    fn columns_drawn_line_by_line_are_separated() {
        let mut c = Vec::new();
        for (i, (l, r)) in [("left one", "right one"), ("left two", "right two")].iter().enumerate() {
            let y = 100.0 + 12.0 * i as f32;
            line(&mut c, l, 50.0, y);
            c.push(TextChar { text: " ".into(), rect: None });
            line(&mut c, r, 300.0, y);
            newline(&mut c);
        }
        assert_eq!(text(&reading_order(&c)), "left one\nleft two\nright one\nright two");
    }

    #[test]
    fn ordinary_text_is_unchanged() {
        let mut c = Vec::new();
        line(&mut c, "Title", 80.0, 50.0);
        newline(&mut c);
        line(&mut c, "a paragraph with words", 50.0, 80.0);
        newline(&mut c);
        line(&mut c, "that goes on", 50.0, 92.0);
        assert_eq!(text(&reading_order(&c)), text(&c));
    }
}
