//! Page content as a list of operations with their byte positions, the
//! glyphs drawn by each text operation, and rewriting content with some of
//! those glyphs removed.

use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use crate::document::Document;
use crate::font::Font;
use crate::object::{Dictionary, Object};
use crate::parser::{Parser, Token};

pub(crate) type Matrix = [f64; 6];
pub(crate) const IDENTITY: Matrix = [1.0, 0.0, 0.0, 1.0, 0.0, 0.0];

pub(crate) fn mul(a: &Matrix, b: &Matrix) -> Matrix {
    [
        a[0] * b[0] + a[1] * b[2],
        a[0] * b[1] + a[1] * b[3],
        a[2] * b[0] + a[3] * b[2],
        a[2] * b[1] + a[3] * b[3],
        a[4] * b[0] + a[5] * b[2] + b[4],
        a[4] * b[1] + a[5] * b[3] + b[5],
    ]
}

fn translate(x: f64, y: f64) -> Matrix {
    [1.0, 0.0, 0.0, 1.0, x, y]
}

/// One content stream operation: operands and operator, with the byte
/// range they span (inline images span `BI … EI`).
pub(crate) struct Op {
    pub start: usize,
    pub end: usize,
    pub operator: Vec<u8>,
    pub operands: Vec<Object>,
}

pub(crate) fn parse_ops(data: &[u8]) -> Vec<Op> {
    let mut ops = Vec::new();
    let mut parser = Parser::new(data, 0);
    let mut operands = Vec::new();
    let mut start = None;
    loop {
        parser.lex.skip_whitespace();
        let here = parser.lex.pos();
        let Some(tok) = parser.lex.next_token() else { break };
        let first = *start.get_or_insert(here);
        match tok {
            Token::Keyword(op) => {
                let operator = op.to_vec();
                if operator == b"BI" {
                    crate::text::skip_inline_image(&mut parser.lex);
                }
                ops.push(Op { start: first, end: parser.lex.pos(), operator, operands: std::mem::take(&mut operands) });
                start = None;
            }
            other => match parser.object_from_token(other, 0) {
                Ok(o) => operands.push(o),
                // Unreadable operands are dropped from the operation, but
                // their bytes stay part of it, so rewritten content keeps
                // them as they were.
                Err(_) => operands.clear(),
            },
        }
    }
    ops
}

/// A glyph drawn directly by the page's content (not inside a form
/// XObject), with where it came from and how it looks.
#[derive(Debug, Clone)]
pub struct Glyph {
    /// The text it stands for (may be empty for unmapped codes).
    pub text: String,
    /// Bounds `[left, top, right, bottom]` in display points.
    pub rect: [f32; 4],
    /// Baseline start and end in display points.
    pub origin: [f32; 2],
    pub end_x: f32,
    /// Font size as shown, in display points.
    pub size: f32,
    /// Fill color (RGB, 0–1).
    pub color: [f32; 3],
    /// The font's resource name on the page.
    pub font: Vec<u8>,
    /// Drawn upright (not rotated or skewed) and visible.
    pub plain: bool,
    // Where it is in the content:
    pub(crate) op: usize,
    pub(crate) item: usize,
    pub(crate) byte: usize,
    pub(crate) len: usize,
    pub(crate) code: u32,
    /// Horizontal displacement in unscaled text space (w0·Tfs + Tc + Tw).
    pub(crate) advance: f64,
    pub(crate) font_size: f64,
    /// Glyph width per unit of font size (w0, from the font).
    pub(crate) width: f32,
}

#[derive(Clone)]
struct State {
    ctm: Matrix,
    font: Option<(Vec<u8>, Rc<Font>)>,
    font_size: f64,
    char_spacing: f64,
    word_spacing: f64,
    h_scale: f64,
    leading: f64,
    rise: f64,
    render_mode: i64,
    color: [f32; 3],
}

/// Interprets content (a page's own, or a form XObject's) and lists the
/// glyphs it draws. `display` maps its user space to display points.
pub(crate) fn glyphs(doc: &Document, display: &Matrix, ops: &[Op], resources: &Dictionary) -> Vec<Glyph> {
    let display = *display;
    let mut fonts: HashMap<Vec<u8>, Option<Rc<Font>>> = HashMap::new();
    let font_dict = doc.get_dict(resources, b"Font").unwrap_or_default();
    let mut load_font = |name: &[u8]| -> Option<Rc<Font>> {
        fonts
            .entry(name.to_vec())
            .or_insert_with(|| {
                let d = doc.resolve(font_dict.get(name)?).ok()?.as_dict()?.clone();
                Some(Rc::new(Font::load(doc, &d)))
            })
            .clone()
    };

    let mut out = Vec::new();
    let mut gs = State {
        ctm: IDENTITY,
        font: None,
        font_size: 0.0,
        char_spacing: 0.0,
        word_spacing: 0.0,
        h_scale: 1.0,
        leading: 0.0,
        rise: 0.0,
        render_mode: 0,
        color: [0.0; 3],
    };
    let mut stack: Vec<State> = Vec::new();
    let (mut tm, mut tlm) = (IDENTITY, IDENTITY);

    for (index, op) in ops.iter().enumerate() {
        let o = &op.operands;
        let num = |i: usize| o.get(i).and_then(Object::as_f64).unwrap_or(0.0);
        let nums = || -> Vec<f32> { o.iter().filter_map(Object::as_f64).map(|v| v as f32).collect() };
        match op.operator.as_slice() {
            b"q" => stack.push(gs.clone()),
            b"Q" => {
                if let Some(s) = stack.pop() {
                    gs = s;
                }
            }
            b"cm" if o.len() >= 6 => gs.ctm = mul(&[num(0), num(1), num(2), num(3), num(4), num(5)], &gs.ctm),
            b"BT" => {
                tm = IDENTITY;
                tlm = IDENTITY;
            }
            b"Tf" => {
                gs.font_size = num(1);
                gs.font = o.first().and_then(Object::as_name).and_then(|n| Some((n.to_vec(), load_font(n)?)));
            }
            b"Tc" => gs.char_spacing = num(0),
            b"Tw" => gs.word_spacing = num(0),
            b"Tz" => gs.h_scale = num(0) / 100.0,
            b"TL" => gs.leading = num(0),
            b"Ts" => gs.rise = num(0),
            b"Tr" => gs.render_mode = o.first().and_then(Object::as_i64).unwrap_or(0),
            b"Td" => {
                tlm = mul(&translate(num(0), num(1)), &tlm);
                tm = tlm;
            }
            b"TD" => {
                gs.leading = -num(1);
                tlm = mul(&translate(num(0), num(1)), &tlm);
                tm = tlm;
            }
            b"Tm" if o.len() >= 6 => {
                tlm = [num(0), num(1), num(2), num(3), num(4), num(5)];
                tm = tlm;
            }
            b"T*" => {
                tlm = mul(&translate(0.0, -gs.leading), &tlm);
                tm = tlm;
            }
            // Fill colors (device color spaces; others count as black).
            b"g" => gs.color = [num(0) as f32; 3],
            b"rg" => gs.color = [num(0) as f32, num(1) as f32, num(2) as f32],
            b"k" => gs.color = cmyk(num(0), num(1), num(2), num(3)),
            b"sc" | b"scn" => {
                gs.color = match nums()[..] {
                    [g] => [g; 3],
                    [r, g, b] => [r, g, b],
                    [c, m, y, k] => cmyk(c as f64, m as f64, y as f64, k as f64),
                    _ => gs.color,
                }
            }
            b"cs" => gs.color = [0.0; 3],
            b"Tj" | b"'" | b"\"" | b"TJ" => {
                if op.operator != b"Tj" && op.operator != b"TJ" {
                    if op.operator == b"\"" {
                        gs.word_spacing = num(0);
                        gs.char_spacing = num(1);
                    }
                    tlm = mul(&translate(0.0, -gs.leading), &tlm);
                    tm = tlm;
                }
                let items: Vec<Object> = match (op.operator.as_slice(), o.last()) {
                    (b"TJ", Some(Object::Array(a))) => a.clone(),
                    (_, Some(s @ Object::String(_))) => vec![s.clone()],
                    _ => continue,
                };
                for (item, obj) in items.iter().enumerate() {
                    match obj {
                        Object::String(bytes) => show(bytes, index, item, &gs, &mut tm, &display, &mut out),
                        other => {
                            let adj = other.as_f64().unwrap_or(0.0);
                            tm = mul(&translate(-adj / 1000.0 * gs.font_size * gs.h_scale, 0.0), &tm);
                        }
                    }
                }
            }
            _ => {}
        }
    }
    out
}

fn cmyk(c: f64, m: f64, y: f64, k: f64) -> [f32; 3] {
    [(1.0 - c) * (1.0 - k), (1.0 - m) * (1.0 - k), (1.0 - y) * (1.0 - k)].map(|v| v as f32)
}

fn show(bytes: &[u8], op: usize, item: usize, gs: &State, tm: &mut Matrix, display: &Matrix, out: &mut Vec<Glyph>) {
    let Some((name, font)) = &gs.font else { return };
    let mut byte = 0;
    for (code, len) in font.codes(bytes) {
        let trm = mul(tm, &gs.ctm);
        let mut advance = font.width(code) * gs.font_size + gs.char_spacing;
        if code == 32 && len == 1 {
            advance += gs.word_spacing;
        }
        *tm = mul(&translate(advance * gs.h_scale, 0.0), tm);
        let page_m = mul(&trm, display);
        let point = |x: f64, y: f64| (x * page_m[0] + y * page_m[2] + page_m[4], x * page_m[1] + y * page_m[3] + page_m[5]);
        let w = advance * gs.h_scale;
        let (bottom, top) = (gs.rise - 0.22 * gs.font_size, gs.rise + 0.78 * gs.font_size);
        let corners = [point(0.0, bottom), point(w, bottom), point(0.0, top), point(w, top)];
        let (l, r) = corners.iter().fold((f64::MAX, f64::MIN), |(a, b), p| (a.min(p.0), b.max(p.0)));
        let (t, bt) = corners.iter().fold((f64::MAX, f64::MIN), |(a, b), p| (a.min(p.1), b.max(p.1)));
        let origin = point(0.0, gs.rise);
        let end = point(w, gs.rise);
        // Upright: x grows to the right, y down, no skew (display space).
        let plain = page_m[0] > 0.0 && page_m[3] < 0.0 && page_m[1].abs() < 1e-3 * page_m[0].abs()
            && page_m[2].abs() < 1e-3 * page_m[3].abs()
            && gs.render_mode != 3
            && gs.render_mode != 7;
        let size = (gs.font_size * page_m[2].hypot(page_m[3])).abs() as f32;
        if [l, r, t, bt].iter().all(|v| v.is_finite()) {
            out.push(Glyph {
                text: font.to_text(code).unwrap_or("").to_string(),
                rect: [l as f32, t as f32, r as f32, bt as f32],
                origin: [origin.0 as f32, origin.1 as f32],
                end_x: end.0 as f32,
                size,
                color: gs.color,
                font: name.clone(),
                plain,
                op,
                item,
                byte,
                len,
                code,
                advance,
                font_size: gs.font_size,
                width: font.width(code) as f32,
            });
        }
        byte += len;
    }
}

/// The content with the glyphs at `remove` (indices into `glyphs`) taken
/// out. Text after a removed glyph in the same operation keeps its place:
/// the removed glyph is replaced by an equal amount of space. Operations in
/// `replace` (by index) are written as the bytes given instead.
pub(crate) fn remove_glyphs(
    data: &[u8],
    ops: &[Op],
    glyphs: &[Glyph],
    remove: &HashSet<usize>,
    replace: &HashMap<usize, Vec<u8>>,
) -> Vec<u8> {
    let mut by_op: HashMap<usize, Vec<&Glyph>> = HashMap::new();
    for g in glyphs {
        by_op.entry(g.op).or_default().push(g);
    }
    let removed_ops: HashSet<usize> = remove.iter().filter_map(|&i| glyphs.get(i)).map(|g| g.op).collect();
    let removed: HashSet<(usize, usize, usize)> =
        remove.iter().filter_map(|&i| glyphs.get(i)).map(|g| (g.op, g.item, g.byte)).collect();

    let mut out = Vec::with_capacity(data.len());
    for (index, op) in ops.iter().enumerate() {
        if let Some(bytes) = replace.get(&index) {
            out.extend_from_slice(bytes);
            out.push(b'\n');
            continue;
        }
        if !removed_ops.contains(&index) {
            out.extend_from_slice(&data[op.start..op.end]);
            out.push(b'\n');
            continue;
        }
        let o = &op.operands;
        let items: Vec<Object> = match (op.operator.as_slice(), o.last()) {
            (b"TJ", Some(Object::Array(a))) => a.clone(),
            (_, Some(s @ Object::String(_))) => vec![s.clone()],
            _ => Vec::new(),
        };
        let op_glyphs = by_op.get(&index).map(Vec::as_slice).unwrap_or(&[]);
        let mut array: Vec<Object> = Vec::new();
        let push_space = |array: &mut Vec<Object>, amount: f64| {
            if let Some(Object::Real(prev)) = array.last_mut() {
                *prev += amount;
            } else {
                array.push(Object::Real(amount));
            }
        };
        for (item, obj) in items.iter().enumerate() {
            match obj {
                Object::String(bytes) => {
                    let mut kept: Vec<u8> = Vec::new();
                    for g in op_glyphs.iter().filter(|g| g.item == item) {
                        if removed.contains(&(index, item, g.byte)) && g.font_size != 0.0 {
                            if !kept.is_empty() {
                                array.push(Object::String(std::mem::take(&mut kept)));
                            }
                            push_space(&mut array, -g.advance * 1000.0 / g.font_size);
                        } else {
                            kept.extend_from_slice(&bytes[g.byte..(g.byte + g.len).min(bytes.len())]);
                        }
                    }
                    if !kept.is_empty() {
                        array.push(Object::String(kept));
                    }
                }
                Object::Integer(n) => push_space(&mut array, *n as f64),
                Object::Real(n) => push_space(&mut array, *n),
                _ => {}
            }
        }
        let prefix = match op.operator.as_slice() {
            b"'" => "T* ".to_string(),
            b"\"" => format!("{} Tw {} Tc T* ", o.first().unwrap_or(&Object::Integer(0)), o.get(1).unwrap_or(&Object::Integer(0))),
            _ => String::new(),
        };
        out.extend_from_slice(format!("{prefix}{} TJ\n", Object::Array(array)).as_bytes());
    }
    out
}
