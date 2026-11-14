//! Plain-text extraction from page content streams (ISO 32000 §9).
//!
//! This interprets just enough of the content stream to know *which*
//! characters are drawn and roughly *where*: text operators, the current
//! transformation matrix, and Form XObjects. Positions are used to insert
//! spaces and line breaks; no layout analysis (columns, tables) is done.
//!
//! [`TextExtractor::extract_chars`] also reports where each character is on
//! the page, which is what a viewer needs for text selection.

use crate::hash::HashMap;
use std::rc::Rc;

use crate::document::{Document, Page};
use crate::error::Result;
use crate::font::Font;
use crate::object::{Dictionary, ObjRef, Object};
use crate::parser::{Lexer, Parser, Token};

type Matrix = [f64; 6];
const IDENTITY: Matrix = [1.0, 0.0, 0.0, 1.0, 0.0, 0.0];
/// Form XObjects may nest; bound the recursion.
const MAX_FORM_DEPTH: usize = 12;

fn mul(m: &Matrix, n: &Matrix) -> Matrix {
    [
        m[0] * n[0] + m[1] * n[2],
        m[0] * n[1] + m[1] * n[3],
        m[2] * n[0] + m[3] * n[2],
        m[2] * n[1] + m[3] * n[3],
        m[4] * n[0] + m[5] * n[2] + n[4],
        m[4] * n[1] + m[5] * n[3] + n[5],
    ]
}

fn translate(tx: f64, ty: f64) -> Matrix {
    [1.0, 0.0, 0.0, 1.0, tx, ty]
}

/// One piece of extracted text: normally a single character (a ligature
/// such as "fi" may give several), or a space / line break inserted between
/// runs of text.
#[derive(Debug, Clone, PartialEq)]
pub struct TextChar {
    pub text: String,
    /// Bounds `[left, top, right, bottom]` in display points (origin at the
    /// top-left of the page as shown, y down; see
    /// [`Page::display_matrix`]). `None` for inserted spaces and line breaks.
    pub rect: Option<[f32; 4]>,
}

/// Joins characters back into plain text.
pub fn chars_to_string(chars: &[TextChar]) -> String {
    chars.iter().map(|c| c.text.as_str()).collect()
}

/// Extracts text from pages, caching fonts across pages so shared fonts
/// (and their ToUnicode maps) are parsed only once.
pub struct TextExtractor<'d> {
    doc: &'d Document,
    fonts: HashMap<ObjRef, Rc<Font>>,
}

impl<'d> TextExtractor<'d> {
    pub fn new(doc: &'d Document) -> Self {
        TextExtractor { doc, fonts: HashMap::default() }
    }

    pub fn extract_page(&mut self, page: &Page) -> Result<String> {
        Ok(chars_to_string(&self.extract_chars(page)?))
    }

    /// The page's text as positioned characters, in reading order.
    pub fn extract_chars(&mut self, page: &Page) -> Result<Vec<TextChar>> {
        let content = self.doc.page_content(page)?;
        let resources = self.doc.get_dict(&page.dict, b"Resources").unwrap_or_default();
        let mut out = TextSink { display: page.display_matrix(self.doc), ..TextSink::default() };
        self.run(&content, &resources, IDENTITY, &mut out, 0)?;
        Ok(out.finish())
    }

    fn run(&mut self, content: &[u8], resources: &Dictionary, ctm: Matrix, out: &mut TextSink, depth: usize) -> Result<()> {
        let mut gs = GraphicsState { ctm, ..GraphicsState::default() };
        let mut stack: Vec<GraphicsState> = Vec::new();
        let mut tm = IDENTITY;
        let mut tlm = IDENTITY;
        let mut operands: Vec<Object> = Vec::new();
        let mut parser = Parser::new(content, 0);

        while let Some(tok) = parser.lex.next_token() {
            let op = match tok {
                Token::Keyword(op) => op,
                other => {
                    // Malformed operands are dropped rather than aborting the page.
                    match parser.object_from_token(other, 0) {
                        Ok(o) => operands.push(o),
                        Err(_) => operands.clear(),
                    }
                    continue;
                }
            };
            let num = |i: usize| operands.get(i).and_then(Object::as_f64).unwrap_or(0.0);
            match op {
                b"q" => stack.push(gs.clone()),
                b"Q" => {
                    if let Some(saved) = stack.pop() {
                        gs = saved;
                    }
                }
                b"cm" if operands.len() >= 6 => {
                    let m = [num(0), num(1), num(2), num(3), num(4), num(5)];
                    gs.ctm = mul(&m, &gs.ctm);
                }
                b"BT" => {
                    tm = IDENTITY;
                    tlm = IDENTITY;
                }
                b"Tf" => {
                    gs.font_size = num(1);
                    gs.font = operands.first().and_then(Object::as_name).and_then(|n| self.font(resources, n));
                }
                b"Tc" => gs.char_spacing = num(0),
                b"Tw" => gs.word_spacing = num(0),
                b"Tz" => gs.h_scale = num(0) / 100.0,
                b"TL" => gs.leading = num(0),
                b"Ts" => gs.rise = num(0),
                b"Td" => {
                    tlm = mul(&translate(num(0), num(1)), &tlm);
                    tm = tlm;
                }
                b"TD" => {
                    gs.leading = -num(1);
                    tlm = mul(&translate(num(0), num(1)), &tlm);
                    tm = tlm;
                }
                b"Tm" if operands.len() >= 6 => {
                    tlm = [num(0), num(1), num(2), num(3), num(4), num(5)];
                    tm = tlm;
                }
                b"T*" => {
                    tlm = mul(&translate(0.0, -gs.leading), &tlm);
                    tm = tlm;
                }
                b"Tj" | b"'" | b"\"" => {
                    if op != b"Tj" {
                        if op == b"\"" {
                            gs.word_spacing = num(0);
                            gs.char_spacing = num(1);
                        }
                        tlm = mul(&translate(0.0, -gs.leading), &tlm);
                        tm = tlm;
                    }
                    if let Some(Object::String(s)) = operands.last() {
                        show_string(s, &gs, &mut tm, out);
                    }
                }
                b"TJ" => {
                    if let Some(Object::Array(items)) = operands.first() {
                        for item in items {
                            match item {
                                Object::String(s) => show_string(s, &gs, &mut tm, out),
                                other => {
                                    let adj = other.as_f64().unwrap_or(0.0);
                                    let tx = -adj / 1000.0 * gs.font_size * gs.h_scale;
                                    tm = mul(&translate(tx, 0.0), &tm);
                                }
                            }
                        }
                    }
                }
                b"Do" if depth < MAX_FORM_DEPTH => {
                    if let Some(name) = operands.first().and_then(Object::as_name) {
                        self.run_form(resources, name, &gs.ctm, out, depth)?;
                    }
                }
                b"BI" => skip_inline_image(&mut parser.lex),
                _ => {} // graphics operators are irrelevant for text
            }
            operands.clear();
        }
        Ok(())
    }

    fn run_form(&mut self, resources: &Dictionary, name: &[u8], ctm: &Matrix, out: &mut TextSink, depth: usize) -> Result<()> {
        let doc = self.doc;
        let Some(xobjects) = doc.get_dict(resources, b"XObject") else { return Ok(()) };
        let Ok(Object::Stream(form)) = doc.get_resolved(&xobjects, name) else { return Ok(()) };
        if form.dict.get(b"Subtype").and_then(Object::as_name) != Some(b"Form") {
            return Ok(()); // images carry no text
        }
        let matrix = match doc.get_resolved(&form.dict, b"Matrix")?.as_array() {
            Some(a) if a.len() == 6 => {
                let v: Vec<f64> = a.iter().map(|o| o.as_f64().unwrap_or(0.0)).collect();
                [v[0], v[1], v[2], v[3], v[4], v[5]]
            }
            _ => IDENTITY,
        };
        // Forms without their own resources use the parent's.
        let form_resources = doc.get_dict(&form.dict, b"Resources").unwrap_or_else(|| resources.clone());
        let Ok(content) = doc.decode_stream(&form) else { return Ok(()) };
        self.run(&content, &form_resources, mul(&matrix, ctm), out, depth + 1)
    }

    fn font(&mut self, resources: &Dictionary, name: &[u8]) -> Option<Rc<Font>> {
        let fonts = self.doc.get_dict(resources, b"Font")?;
        let entry = fonts.get(name)?;
        if let Some(r) = entry.as_reference()
            && let Some(f) = self.fonts.get(&r) {
                return Some(f.clone());
            }
        let dict = self.doc.resolve(entry).ok()?.as_dict()?.clone();
        let font = Rc::new(Font::load(self.doc, &dict));
        if let Some(r) = entry.as_reference() {
            self.fonts.insert(r, font.clone());
        }
        Some(font)
    }
}

#[derive(Clone)]
struct GraphicsState {
    ctm: Matrix,
    font: Option<Rc<Font>>,
    font_size: f64,
    char_spacing: f64,
    word_spacing: f64,
    h_scale: f64,
    leading: f64,
    rise: f64,
}

impl Default for GraphicsState {
    fn default() -> Self {
        GraphicsState {
            ctm: IDENTITY,
            font: None,
            font_size: 0.0,
            char_spacing: 0.0,
            word_spacing: 0.0,
            h_scale: 1.0,
            leading: 0.0,
            rise: 0.0,
        }
    }
}

/// Decodes one string operand, writes its text, and advances the text matrix.
fn show_string(bytes: &[u8], gs: &GraphicsState, tm: &mut Matrix, out: &mut TextSink) {
    let Some(font) = &gs.font else { return };
    for (code, len) in font.codes(bytes) {
        let start = mul(tm, &gs.ctm);
        let mut tx = font.width(code) * gs.font_size + gs.char_spacing;
        // Word spacing applies to single-byte code 32 only (§9.3.3).
        if code == 32 && len == 1 {
            tx += gs.word_spacing;
        }
        *tm = mul(&translate(tx * gs.h_scale, 0.0), tm);
        if let Some(text) = font.to_text(code) {
            let end = mul(tm, &gs.ctm);
            let size = (gs.font_size * start[2].hypot(start[3])).abs();
            // Glyph box in text space: the advance horizontally, and a
            // typical descender/ascender (−0.2…0.8 em) vertically.
            let (bottom, top) = (gs.rise - 0.2 * gs.font_size, gs.rise + 0.8 * gs.font_size);
            let corners = [(0.0, bottom), (tx * gs.h_scale, bottom), (0.0, top), (tx * gs.h_scale, top)]
                .map(|(x, y)| (x * start[0] + y * start[2] + start[4], x * start[1] + y * start[3] + start[5]));
            let rect = out.display_rect(&corners);
            out.glyph(text, (start[4], start[5]), (end[4], end[5]), size, rect);
        }
    }
}

/// Skips `BI … ID <binary> EI`; the lexer is positioned after `BI`.
pub(crate) fn skip_inline_image(lex: &mut Lexer) {
    // Skip the image dictionary up to `ID`.
    loop {
        match lex.next_token() {
            None => return,
            Some(Token::Keyword(b"ID")) => break,
            Some(_) => {}
        }
    }
    let data = lex.data();
    let mut p = lex.pos() + 1; // single whitespace after ID
    // `EI` must be surrounded by whitespace (or end of data).
    while p + 1 < data.len() {
        if data[p] == b'E'
            && data[p + 1] == b'I'
            && crate::object::is_whitespace(data[p - 1])
            && data.get(p + 2).is_none_or(|&c| crate::object::is_whitespace(c))
        {
            lex.set_pos(p + 2);
            return;
        }
        p += 1;
    }
    lex.set_pos(data.len());
}

/// How many recent glyphs to remember for overstrike detection.
const RECENT_GLYPHS: usize = 8;

/// Accumulates text and decides where spaces and line breaks go.
/// Spacing decisions use page space; rectangles are reported in display space.
#[derive(Default)]
struct TextSink {
    chars: Vec<TextChar>,
    /// Page space → display space.
    display: Matrix,
    /// End position of the previous glyph in page space.
    last_end: Option<(f64, f64)>,
    last_size: f64,
    /// Recently drawn glyphs: (text, x, y), to drop overstrikes.
    recent: Vec<(String, f64, f64)>,
}

impl TextSink {
    /// Axis-aligned display-space bounds of page-space points.
    fn display_rect(&self, points: &[(f64, f64)]) -> Option<[f32; 4]> {
        let m = &self.display;
        let mut r = [f64::MAX, f64::MAX, f64::MIN, f64::MIN];
        for &(x, y) in points {
            let (dx, dy) = (x * m[0] + y * m[2] + m[4], x * m[1] + y * m[3] + m[5]);
            r = [r[0].min(dx), r[1].min(dy), r[2].max(dx), r[3].max(dy)];
        }
        r.iter().all(|v| v.is_finite()).then(|| r.map(|v| v as f32))
    }

    /// Adds a glyph drawn from `start` to `end` (page space).
    fn glyph(&mut self, text: &str, start: (f64, f64), end: (f64, f64), size: f64, rect: Option<[f32; 4]>) {
        let size = if size > 0.0 { size } else { 10.0 };
        let advance = (end.0 - start.0).hypot(end.1 - start.1);

        // "Fake bold" and shadow effects draw the same glyph several times
        // with a tiny offset. Keep only the first copy.
        let tol = if advance > 0.0 { 0.5 * advance } else { 0.1 * size };
        let overstrike = self.recent.iter().any(|(t, x, y)| {
            t == text && (start.0 - x).abs() < tol && (start.1 - y).abs() < 0.1 * size
        });
        if overstrike {
            return;
        }
        if self.recent.len() == RECENT_GLYPHS {
            self.recent.remove(0);
        }
        self.recent.push((text.to_string(), start.0, start.1));

        if let Some((lx, ly)) = self.last_end {
            let line_tol = 0.5 * size.max(self.last_size);
            if (start.1 - ly).abs() > line_tol {
                self.newline();
            } else if (start.0 - lx) > 0.15 * size || (lx - start.0) > 2.0 * size {
                // A gap, or a jump backwards on the same baseline (e.g. a
                // second column): separate with a space.
                self.space();
            }
        }
        self.last_size = size;
        self.push(text, rect);
        self.last_end = Some(end);
    }

    fn push(&mut self, s: &str, rect: Option<[f32; 4]>) {
        // Drop control characters that ToUnicode maps sometimes produce.
        let clean: String = s.chars().filter(|&c| !c.is_control() || c == '\t').collect();
        if !clean.is_empty() {
            self.chars.push(TextChar { text: clean, rect });
        }
        if s.contains(['\n', '\r']) {
            self.newline();
        }
    }

    fn last_is(&self, ws: &[char]) -> bool {
        self.chars.last().is_none_or(|c| c.text.ends_with(ws))
    }

    fn separator(&mut self, text: &str) {
        self.chars.push(TextChar { text: text.to_string(), rect: None });
    }

    fn space(&mut self) {
        if !self.last_is(&[' ', '\n']) {
            self.separator(" ");
        }
    }

    fn newline(&mut self) {
        while self.chars.last().is_some_and(|c| c.text == " ") {
            self.chars.pop();
        }
        if !self.last_is(&['\n']) {
            self.separator("\n");
        }
    }

    fn finish(mut self) -> Vec<TextChar> {
        self.newline();
        self.chars
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sink_spacing() {
        let mut s = TextSink::default();
        for (i, c) in "Hi".chars().enumerate() {
            let x = i as f64 * 5.0;
            s.glyph(&c.to_string(), (x, 100.0), (x + 5.0, 100.0), 10.0, None);
        }
        s.glyph("W", (13.0, 100.0), (18.0, 100.0), 10.0, None); // gap > 0.15em → space
        s.glyph("N", (0.0, 88.0), (5.0, 88.0), 10.0, None); // next line
        assert_eq!(chars_to_string(&s.finish()), "Hi W\nN\n");
    }

    #[test]
    fn sink_drops_fake_bold_but_keeps_double_letters() {
        let mut s = TextSink::default();
        for dx in [0.0, 0.2, 0.4] {
            s.glyph("报", (dx, 0.0), (dx + 10.0, 0.0), 10.0, None);
        }
        s.glyph("l", (10.0, 0.0), (13.0, 0.0), 10.0, None);
        s.glyph("l", (13.0, 0.0), (16.0, 0.0), 10.0, None);
        assert_eq!(chars_to_string(&s.finish()), "报ll\n");
    }
}
