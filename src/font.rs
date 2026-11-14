//! Font information shared by text extraction and rendering: how string
//! bytes split into character codes, what text each code means, and glyph
//! advance widths (ISO 32000 §9.5–9.10).

use crate::hash::HashMap;

use crate::document::Document;
use crate::encoding::{glyph_name_to_unicode, BaseEncoding};
use crate::object::{Dictionary, Object};
use crate::parser::{Lexer, Token};

/// Upper bound on entries generated from one CMap range.
const MAX_RANGE: u32 = 0x10000;

/// The subset of a font needed to turn codes into text and advance widths.
/// Some fields are only used by the renderer.
#[cfg_attr(not(feature = "render"), allow(dead_code))]
pub(crate) struct Font {
    /// `/Subtype`: Type1, TrueType, Type3, Type0, MMType1.
    pub subtype: Vec<u8>,
    /// Type0 fonts: the descendant CIDFont dictionary.
    pub descendant: Option<Dictionary>,
    /// The font descriptor (of the descendant, for Type0 fonts).
    pub descriptor: Option<Dictionary>,
    /// Simple fonts: glyph names given by `/Differences`, per code.
    pub differences: Vec<Option<String>>,
    /// Simple fonts: base encoding named in `/Encoding`, if any.
    pub base_encoding: Option<BaseEncoding>,
    /// FontDescriptor flag 3: the font uses its own, non-Latin character set.
    pub symbolic: bool,
    pub composite: bool,
    /// Code space ranges `(byte length, low, high)` for composite fonts.
    codespace: Vec<(u8, u32, u32)>,
    to_unicode: HashMap<u32, String>,
    /// Simple fonts: text for each byte from /Encoding.
    encoding: Vec<Option<String>>,
    widths: HashMap<u32, f64>,
    default_width: f64,
    /// Glyph space → text space (0.001, or FontMatrix[0] for Type3).
    width_scale: f64,
}

#[cfg_attr(not(feature = "render"), allow(dead_code))]
impl Font {
    pub(crate) fn load(doc: &Document, dict: &Dictionary) -> Font {
        let subtype = dict.get(b"Subtype").and_then(Object::as_name).unwrap_or(b"");
        let composite = subtype == b"Type0";
        let descendant = if composite {
            match doc.get_resolved(dict, b"DescendantFonts") {
                Ok(Object::Array(a)) => a.first().and_then(|d| doc.resolve(d).ok()).and_then(|o| o.as_dict().cloned()),
                _ => None,
            }
        } else {
            None
        };
        let descriptor = doc.get_dict(descendant.as_ref().unwrap_or(dict), b"FontDescriptor");
        let flags = descriptor
            .as_ref()
            .and_then(|d| doc.get_resolved(d, b"Flags").ok())
            .and_then(|o| o.as_i64())
            .unwrap_or(0);
        let mut font = Font {
            subtype: subtype.to_vec(),
            descendant,
            descriptor,
            differences: Vec::new(),
            base_encoding: None,
            symbolic: flags & 4 != 0,
            composite,
            codespace: Vec::new(),
            to_unicode: HashMap::default(),
            encoding: Vec::new(),
            widths: HashMap::default(),
            default_width: 0.0,
            width_scale: 0.001,
        };

        if let Ok(Object::Stream(s)) = doc.get_resolved(dict, b"ToUnicode")
            && let Ok(data) = doc.decode_stream(&s) {
                parse_cmap(&data, &mut font.to_unicode, &mut font.codespace);
            }

        if composite {
            // An embedded encoding CMap defines the real code space.
            if let Ok(Object::Stream(s)) = doc.get_resolved(dict, b"Encoding")
                && let Ok(data) = doc.decode_stream(&s) {
                    let mut cs = Vec::new();
                    parse_cmap(&data, &mut HashMap::default(), &mut cs);
                    if !cs.is_empty() {
                        font.codespace = cs;
                    }
                }
            font.load_cid_widths(doc);
        } else {
            font.load_simple_encoding(doc, dict, subtype);
            font.load_simple_widths(doc, dict, subtype);
        }
        font
    }

    fn load_simple_encoding(&mut self, doc: &Document, dict: &Dictionary, subtype: &[u8]) {
        let default_base = if subtype == b"Type1" { BaseEncoding::Standard } else { BaseEncoding::WinAnsi };
        let enc = doc.get_resolved(dict, b"Encoding").unwrap_or_default();
        let (base, differences) = match &enc {
            Object::Name(n) => (BaseEncoding::from_name(n), None),
            Object::Dictionary(d) => {
                let base = d.get(b"BaseEncoding").and_then(Object::as_name).and_then(BaseEncoding::from_name);
                (base, doc.get_resolved(d, b"Differences").ok())
            }
            _ => (None, None),
        };
        self.base_encoding = base;
        let base = base.unwrap_or(default_base);
        self.differences = vec![None; 256];
        self.encoding = (0..=255u8).map(|b| base.decode(b).map(String::from)).collect();

        // /Differences [code /name /name … code /name …]
        if let Some(Object::Array(diffs)) = differences {
            let mut code = 0usize;
            for item in &diffs {
                match item {
                    Object::Integer(c) => code = (*c).clamp(0, 256) as usize,
                    Object::Name(name) => {
                        if code < 256 {
                            let name = String::from_utf8_lossy(name).into_owned();
                            let text = glyph_name_to_unicode(&name);
                            self.differences[code] = Some(name);
                            // Unknown glyph names keep the base encoding's char.
                            if text.is_some() {
                                self.encoding[code] = text;
                            }
                        }
                        code += 1;
                    }
                    _ => {}
                }
            }
        }
    }

    fn load_simple_widths(&mut self, doc: &Document, dict: &Dictionary, subtype: &[u8]) {
        if subtype == b"Type3"
            && let Some(m) = doc.get_resolved(dict, b"FontMatrix").ok().and_then(|o| o.as_array().map(<[_]>::to_vec)) {
                self.width_scale = m.first().and_then(Object::as_f64).unwrap_or(0.001);
            }
        let first = doc.get_resolved(dict, b"FirstChar").ok().and_then(|o| o.as_i64()).unwrap_or(0);
        let descriptor = doc.get_dict(dict, b"FontDescriptor");
        let missing = descriptor
            .as_ref()
            .and_then(|d| doc.get_resolved(d, b"MissingWidth").ok())
            .and_then(|o| o.as_f64())
            .unwrap_or(0.0);
        match doc.get_resolved(dict, b"Widths") {
            Ok(Object::Array(ws)) => {
                self.default_width = missing;
                for (i, w) in ws.iter().enumerate() {
                    if let Some(w) = doc.resolve(w).ok().and_then(|o| o.as_f64()) {
                        self.widths.insert((first + i as i64) as u32, w);
                    }
                }
            }
            // Standard 14 fonts may omit /Widths: use their known metrics.
            _ => {
                let base = dict.get(b"BaseFont").and_then(Object::as_name).map(String::from_utf8_lossy);
                match base.as_deref().and_then(crate::std14::lookup) {
                    Some(std) => {
                        self.default_width = std.default_width();
                        for code in 0..=255u8 {
                            if let Some(w) = std.width(self.glyph_name(code as u32).as_deref(), code) {
                                self.widths.insert(code as u32, w);
                            }
                        }
                    }
                    None => self.default_width = 500.0, // an average glyph
                }
            }
        }
    }

    fn load_cid_widths(&mut self, doc: &Document) {
        let Some(cid_font) = self.descendant.clone() else {
            self.default_width = 1000.0;
            return;
        };
        self.default_width = doc.get_resolved(&cid_font, b"DW").ok().and_then(|o| o.as_f64()).unwrap_or(1000.0);
        // /W [c [w1 w2 …]  cfirst clast w  …]
        let Ok(Object::Array(w)) = doc.get_resolved(&cid_font, b"W") else { return };
        let mut i = 0;
        while i < w.len() {
            let Some(start) = w[i].as_i64() else { break };
            match w.get(i + 1) {
                Some(Object::Array(list)) => {
                    for (k, width) in list.iter().enumerate() {
                        if let Some(width) = width.as_f64() {
                            self.widths.insert((start + k as i64) as u32, width);
                        }
                    }
                    i += 2;
                }
                Some(end) => {
                    let (Some(end), Some(width)) = (end.as_i64(), w.get(i + 2).and_then(Object::as_f64)) else { break };
                    for c in start..=end.min(start + MAX_RANGE as i64) {
                        self.widths.insert(c as u32, width);
                    }
                    i += 3;
                }
                None => break,
            }
        }
    }

    /// Splits a string into (code, byte length) pairs.
    pub(crate) fn codes<'a>(&'a self, bytes: &'a [u8]) -> impl Iterator<Item = (u32, usize)> + 'a {
        let mut i = 0;
        std::iter::from_fn(move || {
            if i >= bytes.len() {
                return None;
            }
            let len = if !self.composite {
                1
            } else {
                self.code_length(&bytes[i..])
            };
            let len = len.min(bytes.len() - i);
            let code = bytes[i..i + len].iter().fold(0u32, |acc, &b| acc << 8 | b as u32);
            i += len;
            Some((code, len))
        })
    }

    fn code_length(&self, bytes: &[u8]) -> usize {
        if self.codespace.is_empty() {
            return 2; // Identity-H and most CJK CMaps
        }
        for n in 1..=4usize {
            if n > bytes.len() {
                break;
            }
            let code = bytes[..n].iter().fold(0u32, |acc, &b| acc << 8 | b as u32);
            if self.codespace.iter().any(|&(len, lo, hi)| len as usize == n && (lo..=hi).contains(&code)) {
                return n;
            }
        }
        // No range matched: use the shortest declared length.
        self.codespace.iter().map(|&(len, _, _)| len as usize).min().unwrap_or(1)
    }

    pub(crate) fn to_text(&self, code: u32) -> Option<&str> {
        if let Some(s) = self.to_unicode.get(&code) {
            return Some(s);
        }
        if !self.composite {
            return self.encoding.get(code as usize).and_then(|o| o.as_deref());
        }
        None
    }

    /// Glyph name for a simple-font code: from `/Differences`, else derived
    /// from the encoding's character (e.g. 'é' → `eacute`).
    pub(crate) fn glyph_name(&self, code: u32) -> Option<String> {
        if let Some(Some(name)) = self.differences.get(code as usize) {
            return Some(name.clone());
        }
        let text = self.encoding.get(code as usize)?.as_deref()?;
        let mut chars = text.chars();
        let (c, None) = (chars.next()?, chars.next()) else { return None };
        crate::encoding::char_to_glyph_name(c)
    }

    /// Advance width in text space units (before multiplying by font size).
    /// Width from the PDF's /Widths or /W, if listed (glyph units).
    pub(crate) fn declared_width(&self, code: u32) -> Option<f64> {
        self.widths.get(&code).copied()
    }

    pub(crate) fn width(&self, code: u32) -> f64 {
        self.widths.get(&code).copied().unwrap_or(self.default_width) * self.width_scale
    }
}

/// Parses the parts of a CMap we need: code space ranges and bfchar/bfrange
/// mappings to Unicode (UTF-16BE). CID mappings are ignored.
fn parse_cmap(data: &[u8], map: &mut HashMap<u32, String>, codespace: &mut Vec<(u8, u32, u32)>) {
    let mut lex = Lexer::new(data, 0);
    let code_of = |s: &[u8]| s.iter().take(4).fold(0u32, |acc, &b| acc << 8 | b as u32);

    while let Some(tok) = lex.next_token() {
        match tok {
            Token::Keyword(b"begincodespacerange") => {
                while let (Some(Token::String(lo)), Some(Token::String(hi))) = (lex.next_token(), lex.next_token()) {
                    let len = lo.len().clamp(1, 4) as u8;
                    codespace.push((len, code_of(&lo), code_of(&hi)));
                }
            }
            Token::Keyword(b"beginbfchar") => {
                while let (Some(Token::String(src)), Some(dst)) = (lex.next_token(), lex.next_token()) {
                    if let Some(text) = cmap_destination(&dst) {
                        map.insert(code_of(&src), text);
                    }
                }
            }
            Token::Keyword(b"beginbfrange") => loop {
                let (Some(Token::String(lo)), Some(Token::String(hi))) = (lex.next_token(), lex.next_token()) else {
                    break;
                };
                let (lo, hi) = (code_of(&lo), code_of(&hi));
                if hi < lo || hi - lo > MAX_RANGE {
                    lex.next_token(); // skip destination
                    continue;
                }
                match lex.next_token() {
                    Some(Token::String(dst)) => {
                        // Increment the last UTF-16 unit for each code.
                        let mut units = utf16_units(&dst);
                        if units.is_empty() {
                            continue;
                        }
                        let last = units.len() - 1;
                        let base = units[last];
                        for (k, code) in (lo..=hi).enumerate() {
                            units[last] = base.wrapping_add(k as u16);
                            map.insert(code, String::from_utf16_lossy(&units));
                        }
                    }
                    Some(Token::ArrayStart) => {
                        let mut code = lo;
                        while let Some(t) = lex.next_token() {
                            if t == Token::ArrayEnd {
                                break;
                            }
                            if let Some(text) = cmap_destination(&t)
                                && code <= hi {
                                    map.insert(code, text);
                                }
                            code = code.saturating_add(1);
                        }
                    }
                    _ => break,
                }
            },
            _ => {}
        }
    }
}

fn utf16_units(bytes: &[u8]) -> Vec<u16> {
    if bytes.len() == 1 {
        return vec![bytes[0] as u16];
    }
    bytes.chunks_exact(2).map(|c| u16::from_be_bytes([c[0], c[1]])).collect()
}

fn cmap_destination(tok: &Token) -> Option<String> {
    match tok {
        Token::String(s) => Some(String::from_utf16_lossy(&utf16_units(s))),
        Token::Name(n) => std::str::from_utf8(n).ok().and_then(glyph_name_to_unicode),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cmap_parsing() {
        let cmap = b"1 begincodespacerange <0000> <FFFF> endcodespacerange
            2 beginbfchar <0003> <0020> <0011> <00660069> endbfchar
            2 beginbfrange <0024> <0026> <0041> <0030> <0031> [<0061> <0062>] endbfrange";
        let mut map = HashMap::default();
        let mut cs = Vec::new();
        parse_cmap(cmap, &mut map, &mut cs);
        assert_eq!(cs, vec![(2, 0, 0xFFFF)]);
        assert_eq!(map[&0x03], " ");
        assert_eq!(map[&0x11], "fi");
        assert_eq!(map[&0x24], "A");
        assert_eq!(map[&0x26], "C");
        assert_eq!(map[&0x30], "a");
        assert_eq!(map[&0x31], "b");
    }
}
