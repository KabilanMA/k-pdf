//! Finding words in a page's text.
//!
//! The page's characters (from [`TextExtractor`](crate::TextExtractor), or
//! read from its images by `ocr`) are matched against a phrase. Spaces and
//! line breaks all count as one space, so a phrase is found when it wraps
//! onto the next line; case is ignored unless asked for. Each match comes
//! back as the rectangles it covers on the page (one per line it's on).

use crate::text::TextChar;

/// What to look for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Query {
    /// The phrase, normalized (see [`normalize`]).
    needle: Vec<char>,
    match_case: bool,
    whole_words: bool,
}

/// A place the phrase was found: `[left, top, right, bottom]` rectangles in
/// display points, one per line, and the character range in the page text.
#[derive(Clone, Debug, PartialEq)]
pub struct Match {
    pub rects: Vec<[f32; 4]>,
    pub chars: std::ops::Range<usize>,
}

impl Query {
    /// `None` when the phrase is only spaces.
    pub fn new(phrase: &str, match_case: bool, whole_words: bool) -> Option<Query> {
        let mut needle = Vec::new();
        for c in phrase.chars() {
            push_normalized(&mut needle, c, match_case);
        }
        while needle.first() == Some(&' ') {
            needle.remove(0);
        }
        while needle.last() == Some(&' ') {
            needle.pop();
        }
        (!needle.is_empty()).then_some(Query { needle, match_case, whole_words })
    }

    /// The page text, normalized, with the index of the character each
    /// normalized character came from.
    fn normalized(&self, chars: &[TextChar]) -> (Vec<char>, Vec<usize>) {
        let mut text: Vec<char> = Vec::new();
        let mut from: Vec<usize> = Vec::new();
        for (i, c) in chars.iter().enumerate() {
            for ch in c.text.chars() {
                let before = text.len();
                push_normalized(&mut text, ch, self.match_case);
                from.extend(std::iter::repeat_n(i, text.len() - before));
            }
        }
        (text, from)
    }

    /// Every place the phrase appears in `chars`, in order.
    pub fn find(&self, chars: &[TextChar]) -> Vec<Match> {
        let (text, from) = self.normalized(chars);
        let n = self.needle.len();
        let mut out = Vec::new();
        let mut i = 0;
        while i + n <= text.len() {
            if text[i..i + n] == self.needle[..]
                && (!self.whole_words || (is_boundary(&text, i) && is_boundary(&text, i + n)))
            {
                let range = from[i]..from[i + n - 1] + 1;
                out.push(Match { rects: line_rects(&chars[range.clone()]), chars: range });
                i += n;
            } else {
                i += 1;
            }
        }
        out
    }
}

impl Query {
    /// Like [`find`](Self::find), but allowing a letter in six to be
    /// wrong, missing or extra: for text read from images, where OCR
    /// misreads a letter now and then ("San Djego"). Phrases shorter than
    /// six letters must match exactly.
    pub fn find_approximately(&self, chars: &[TextChar]) -> Vec<Match> {
        let m = self.needle.len();
        let allowed = (m / 6) as u32;
        if allowed == 0 {
            return self.find(chars);
        }
        let (text, from) = self.normalized(chars);
        // Sellers' algorithm: edit distance of the phrase against text
        // ending at each position (a match may start anywhere), keeping
        // where the best alignment starts.
        let mut prev: Vec<(u32, usize)> = (0..=m).map(|i| (i as u32, 0)).collect();
        let mut cur = prev.clone();
        let mut ends: Vec<(u32, usize, usize)> = Vec::new(); // (cost, start, end)
        for j in 1..=text.len() {
            cur[0] = (0, j);
            for i in 1..=m {
                let sub = (prev[i - 1].0 + (self.needle[i - 1] != text[j - 1]) as u32, prev[i - 1].1);
                let skip = (prev[i].0 + 1, prev[i].1);
                let extra = (cur[i - 1].0 + 1, cur[i - 1].1);
                cur[i] = [sub, skip, extra].into_iter().min_by_key(|c| c.0).unwrap();
            }
            if cur[m].0 <= allowed {
                ends.push((cur[m].0, cur[m].1, j));
            }
            std::mem::swap(&mut prev, &mut cur);
        }
        // Of overlapping candidates, the closest one; then the next one
        // that starts after it.
        let mut out = Vec::new();
        let mut last_end = 0;
        let mut i = 0;
        while i < ends.len() {
            let mut best = ends[i];
            let mut k = i + 1;
            while k < ends.len() && ends[k].1 < best.2 {
                if ends[k].0 < best.0 {
                    best = ends[k];
                }
                k += 1;
            }
            i = k;
            let (_, mut start, mut end) = best;
            while start < end && text[start] == ' ' {
                start += 1;
            }
            while end > start && text[end - 1] == ' ' {
                end -= 1;
            }
            if start < last_end || end <= start {
                continue;
            }
            if self.whole_words && !(is_boundary(&text, start) && is_boundary(&text, end)) {
                continue;
            }
            last_end = end;
            let range = from[start]..from[end - 1] + 1;
            out.push(Match { rects: line_rects(&chars[range.clone()]), chars: range });
        }
        out
    }
}

/// Lower case (unless `match_case`), every kind of space as one space, and
/// typographic quotes, dashes and ligatures as their plain forms.
fn push_normalized(out: &mut Vec<char>, c: char, match_case: bool) {
    if c.is_whitespace() || c == '\u{00AD}' {
        // (A soft hyphen only shows at a line break.)
        if c != '\u{00AD}' && out.last() != Some(&' ') {
            out.push(' ');
        }
        return;
    }
    let plain: &[char] = match c {
        '\u{2018}' | '\u{2019}' | '\u{201B}' | '\u{2032}' => &['\''],
        '\u{201C}' | '\u{201D}' | '\u{201F}' | '\u{2033}' => &['"'],
        '\u{2010}' | '\u{2011}' | '\u{2012}' | '\u{2013}' | '\u{2014}' | '\u{2212}' => &['-'],
        '\u{FB00}' => &['f', 'f'],
        '\u{FB01}' => &['f', 'i'],
        '\u{FB02}' => &['f', 'l'],
        '\u{FB03}' => &['f', 'f', 'i'],
        '\u{FB04}' => &['f', 'f', 'l'],
        _ => &[],
    };
    if !plain.is_empty() {
        out.extend_from_slice(plain);
    } else if match_case {
        out.push(c);
    } else {
        out.extend(c.to_lowercase());
    }
}

/// Whether a word can start or end at `i` in `text`.
fn is_boundary(text: &[char], i: usize) -> bool {
    let word = |c: &char| c.is_alphanumeric() || *c == '_';
    i == 0 || i == text.len() || !(text.get(i - 1).is_some_and(word) && text.get(i).is_some_and(word))
}

/// The characters' rectangles joined into one per line.
fn line_rects(chars: &[TextChar]) -> Vec<[f32; 4]> {
    let mut out: Vec<[f32; 4]> = Vec::new();
    for r in chars.iter().filter_map(|c| c.rect) {
        if let Some(last) = out.last_mut() {
            let overlap = last[3].min(r[3]) - last[1].max(r[1]);
            let height = (last[3] - last[1]).min(r[3] - r[1]).max(0.1);
            if overlap > 0.5 * height && r[0] >= last[0] - height {
                *last = [last[0].min(r[0]), last[1].min(r[1]), last[2].max(r[2]), last[3].max(r[3])];
                continue;
            }
        }
        out.push(r);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Characters 10 points wide, lines 20 points apart ('\n' starts a line).
    fn page(text: &str) -> Vec<TextChar> {
        let (mut x, mut y) = (0.0, 0.0);
        text.chars()
            .map(|c| {
                if c == '\n' {
                    (x, y) = (0.0, y + 20.0);
                    return TextChar { text: "\n".into(), rect: None };
                }
                x += 10.0;
                TextChar { text: c.to_string(), rect: Some([x - 10.0, y, x, y + 12.0]) }
            })
            .collect()
    }

    #[test]
    fn finds_phrases_ignoring_case_and_line_breaks() {
        let chars = page("The quick brown\nfox. THE END");
        let q = Query::new("the", false, false).unwrap();
        let found = q.find(&chars);
        assert_eq!(found.len(), 2);
        assert_eq!(found[0].rects, vec![[0.0, 0.0, 30.0, 12.0]]);
        // Across the line break: one rectangle per line.
        let q = Query::new("brown  fox", false, false).unwrap();
        let found = q.find(&chars);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].rects.len(), 2);
        assert!(Query::new("the", true, false).unwrap().find(&chars).is_empty());
        assert_eq!(Query::new("THE", true, false).unwrap().find(&chars).len(), 1);
    }

    #[test]
    fn approximate_matches_for_text_read_from_images() {
        let chars = page("Taxi from San Djego Airport; Sun Dieso; San Diego");
        let q = Query::new("San Diego", false, false).unwrap();
        assert_eq!(q.find(&chars).len(), 1);
        // One letter in six may be off: "San Djego" (one), not "Sun Dieso" (two).
        let found = q.find_approximately(&chars);
        assert_eq!(found.len(), 2, "{found:?}");
        assert_eq!(found[0].chars, 10..19);
        // Short words must match exactly.
        assert_eq!(Query::new("Taxi", false, false).unwrap().find_approximately(&page("Tax1 Taxi")).len(), 1);
    }

    #[test]
    fn whole_words_and_typography() {
        let chars = page("theory of the \u{FB01}eld \u{201C}it\u{2019}s\u{201D}");
        assert_eq!(Query::new("the", false, false).unwrap().find(&chars).len(), 2);
        assert_eq!(Query::new("the", false, true).unwrap().find(&chars).len(), 1);
        assert_eq!(Query::new("field", false, true).unwrap().find(&chars).len(), 1);
        assert_eq!(Query::new("\"it's\"", false, false).unwrap().find(&chars).len(), 1);
        assert!(Query::new("   ", false, false).is_none());
    }
}
