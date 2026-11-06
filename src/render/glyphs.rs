//! Glyph outlines for PDF fonts.
//!
//! Embedded font programs are used when present: TrueType and OpenType
//! (`FontFile2`, `FontFile3/OpenType`), bare CFF (`FontFile3/Type1C`,
//! `CIDFontType0C`) and classic Type 1 (`FontFile`). Fonts that are not
//! embedded — notably the standard 14 (Helvetica, Times…) — are drawn with a
//! similar system font, stretched to the widths the PDF specifies so the
//! layout stays correct.

use std::cell::RefCell;
use std::collections::HashMap;
use std::ops::Deref;
use std::rc::Rc;

use tiny_skia::{Path, PathBuilder, Transform};
use ttf_parser::{Face, GlyphId, PlatformId};

use super::type1::Type1Font;
use crate::document::Document;
use crate::encoding::glyph_name_to_unicode;
use crate::font::Font;
use crate::object::{Dictionary, Object, Stream};

/// Font file bytes: owned (decoded from the PDF) or memory-mapped (system).
pub(crate) enum FontData {
    Owned(Vec<u8>),
    Mapped(memmap2::Mmap),
}

impl Deref for FontData {
    type Target = [u8];
    fn deref(&self) -> &[u8] {
        match self {
            FontData::Owned(v) => v,
            FontData::Mapped(m) => m,
        }
    }
}

enum Program {
    /// TrueType / OpenType file (embedded or system substitute).
    Sfnt { data: Rc<FontData>, index: u32 },
    /// A bare CFF font.
    Cff(Vec<u8>),
    Type1(Type1Font),
    Type3 { procs: Dictionary, resources: Option<Dictionary>, matrix: [f64; 6] },
    None,
}

/// A Type 3 glyph: a content stream to run, in glyph space.
pub(crate) struct Type3Glyph {
    pub stream: Stream,
    pub resources: Option<Dictionary>,
    pub matrix: [f64; 6],
}

pub(crate) struct RenderFont {
    pub info: Font,
    program: Program,
    /// The program is a stand-in system font, not the real one.
    substitute: bool,
    /// CID → glyph id, for CIDFontType2 `CIDToGIDMap` streams and
    /// CID-keyed CFF fonts.
    cid_to_gid: Option<HashMap<u32, u16>>,
    glyphs: RefCell<HashMap<u32, Option<Rc<Path>>>>,
    advances: RefCell<HashMap<u32, f64>>,
}

impl RenderFont {
    pub fn load(doc: &Document, dict: &Dictionary, system: &mut SystemFonts) -> RenderFont {
        let info = Font::load(doc, dict);
        let mut font = RenderFont {
            info,
            program: Program::None,
            substitute: false,
            cid_to_gid: None,
            glyphs: RefCell::new(HashMap::new()),
            advances: RefCell::new(HashMap::new()),
        };

        if font.info.subtype == b"Type3" {
            let procs = doc.get_dict(dict, b"CharProcs").unwrap_or_default();
            let resources = doc.get_dict(dict, b"Resources");
            let m: Vec<f64> = doc
                .get_resolved(dict, b"FontMatrix")
                .ok()
                .and_then(|o| o.as_array().map(|a| a.iter().filter_map(Object::as_f64).collect()))
                .unwrap_or_default();
            let matrix = if m.len() == 6 { [m[0], m[1], m[2], m[3], m[4], m[5]] } else { [0.001, 0.0, 0.0, 0.001, 0.0, 0.0] };
            font.program = Program::Type3 { procs, resources, matrix };
            return font;
        }

        font.program = embedded_program(doc, &font.info).unwrap_or(Program::None);
        if matches!(font.program, Program::None) {
            let base = dict.get(b"BaseFont").and_then(Object::as_name).map(|n| String::from_utf8_lossy(n).into_owned());
            let flags = font
                .info
                .descriptor
                .as_ref()
                .and_then(|d| doc.get_resolved(d, b"Flags").ok())
                .and_then(|o| o.as_i64())
                .unwrap_or(0);
            if let Some((data, index)) = system.find(base.as_deref().unwrap_or(""), flags, font.info.composite) {
                font.program = Program::Sfnt { data, index };
                font.substitute = true;
            }
        }
        font.cid_to_gid = font.load_cid_map(doc);
        font
    }

    fn load_cid_map(&self, doc: &Document) -> Option<HashMap<u32, u16>> {
        if !self.info.composite || self.substitute {
            return None;
        }
        match &self.program {
            Program::Sfnt { .. } => {
                let desc = self.info.descendant.as_ref()?;
                let Ok(Object::Stream(s)) = doc.get_resolved(desc, b"CIDToGIDMap") else { return None };
                let data = doc.decode_stream(&s).ok()?;
                Some(data.chunks_exact(2).enumerate().map(|(cid, g)| (cid as u32, u16::from_be_bytes([g[0], g[1]]))).collect())
            }
            Program::Cff(data) => {
                let table = ttf_parser::cff::Table::parse(data)?;
                let map: HashMap<u32, u16> = (0..table.number_of_glyphs())
                    .filter_map(|g| table.glyph_cid(GlyphId(g)).map(|cid| (cid as u32, g)))
                    .collect();
                (!map.is_empty()).then_some(map)
            }
            _ => None,
        }
    }

    pub fn type3_glyph(&self, doc: &Document, code: u32) -> Option<Type3Glyph> {
        let Program::Type3 { procs, resources, matrix } = &self.program else { return None };
        let name = self.info.glyph_name(code)?;
        let Ok(Object::Stream(stream)) = doc.get_resolved(procs, name.as_bytes()) else { return None };
        Some(Type3Glyph { stream, resources: resources.clone(), matrix: *matrix })
    }

    pub fn is_type3(&self) -> bool {
        matches!(self.program, Program::Type3 { .. })
    }

    /// Glyph outline in text space for a font size of 1 (y up).
    pub fn glyph(&self, code: u32) -> Option<Rc<Path>> {
        if let Some(cached) = self.glyphs.borrow().get(&code) {
            return cached.clone();
        }
        let path = self.build_glyph(code).map(Rc::new);
        self.glyphs.borrow_mut().insert(code, path.clone());
        path
    }

    /// Horizontal advance in text space for a font size of 1.
    pub fn advance(&self, code: u32) -> f64 {
        if self.info.declared_width(code).is_some() || self.is_type3() {
            return self.info.width(code);
        }
        if let Some(&a) = self.advances.borrow().get(&code) {
            return a;
        }
        let a = self.program_advance(code).unwrap_or_else(|| self.info.width(code));
        self.advances.borrow_mut().insert(code, a);
        a
    }

    fn program_advance(&self, code: u32) -> Option<f64> {
        match &self.program {
            Program::Sfnt { data, index } => {
                let face = Face::parse(data, *index).ok()?;
                let gid = self.sfnt_gid(&face, code)?;
                Some(face.glyph_hor_advance(gid)? as f64 / face.units_per_em() as f64)
            }
            Program::Cff(data) => {
                let table = ttf_parser::cff::Table::parse(data)?;
                let gid = self.cff_gid(&table, code)?;
                Some(table.glyph_width(gid)? as f64 * table.matrix().sx as f64)
            }
            Program::Type1(t1) => {
                let name = self.type1_name(t1, code)?;
                Some(t1.advance(&name)? * t1.matrix[0])
            }
            _ => None,
        }
    }

    fn build_glyph(&self, code: u32) -> Option<Path> {
        match &self.program {
            Program::Sfnt { data, index } => {
                let face = Face::parse(data, *index).ok()?;
                let gid = self.sfnt_gid(&face, code)?;
                let mut b = Builder(PathBuilder::new());
                face.outline_glyph(gid, &mut b)?;
                let path = b.0.finish()?;
                let upem = face.units_per_em() as f32;
                let mut sx = 1.0 / upem;
                // A substitute font is stretched to the PDF's glyph width.
                if self.substitute
                    && let (Some(pdf_w), Some(adv)) = (self.info.declared_width(code), face.glyph_hor_advance(gid))
                    && adv > 0
                    && pdf_w > 0.0
                {
                    sx = (pdf_w * 0.001) as f32 / adv as f32;
                }
                path.transform(Transform::from_scale(sx, 1.0 / upem))
            }
            Program::Cff(data) => {
                let table = ttf_parser::cff::Table::parse(data)?;
                let gid = self.cff_gid(&table, code)?;
                let mut b = Builder(PathBuilder::new());
                table.outline(gid, &mut b).ok()?;
                let m = table.matrix();
                b.0.finish()?.transform(Transform::from_row(m.sx, m.ky, m.kx, m.sy, m.tx, m.ty))
            }
            Program::Type1(t1) => {
                let name = self.type1_name(t1, code)?;
                let m = t1.matrix.map(|v| v as f32);
                t1.outline(&name)?.transform(Transform::from_row(m[0], m[1], m[2], m[3], m[4], m[5]))
            }
            Program::Type3 { .. } | Program::None => None,
        }
    }

    /// The character a code stands for, used to look glyphs up by Unicode.
    fn unicode(&self, code: u32) -> Option<char> {
        if let Some(name) = self.info.differences.get(code as usize).and_then(|n| n.as_deref())
            && let Some(text) = glyph_name_to_unicode(name)
        {
            return text.chars().next();
        }
        self.info.to_text(code).and_then(|t| t.chars().next())
    }

    fn sfnt_gid(&self, face: &Face, code: u32) -> Option<GlyphId> {
        let n = face.number_of_glyphs() as u32;
        let valid = |g: GlyphId| (g.0 != 0).then_some(g);

        if self.info.composite {
            if self.substitute {
                return self.unicode(code).and_then(|c| face.glyph_index(c));
            }
            // Identity encoding: code = CID.
            let gid = match &self.cid_to_gid {
                Some(map) => *map.get(&code)? as u32,
                None => code,
            };
            return (gid < n).then_some(GlyphId(gid as u16));
        }

        if self.substitute {
            if let Some(g) = self.unicode(code).and_then(|c| face.glyph_index(c)) {
                return Some(g);
            }
            return self.info.glyph_name(code).and_then(|name| face.glyph_index_by_name(&name));
        }

        // Embedded simple TrueType: ISO 32000 §9.6.6.4.
        let subtables: Vec<_> = face.tables().cmap.map(|c| c.subtables.into_iter().collect()).unwrap_or_default();
        let find = |platform: PlatformId, encoding: Option<u16>| {
            subtables.iter().find(|s| s.platform_id == platform && encoding.is_none_or(|e| s.encoding_id == e)).copied()
        };
        let win_unicode = find(PlatformId::Windows, Some(1)).or_else(|| find(PlatformId::Unicode, None));
        let win_symbol = find(PlatformId::Windows, Some(0));
        let mac_roman = find(PlatformId::Macintosh, Some(0));

        let has_encoding = self.info.base_encoding.is_some() || self.info.differences.iter().any(Option::is_some);
        if !self.info.symbolic || has_encoding {
            if let (Some(t), Some(c)) = (win_unicode, self.unicode(code))
                && let Some(g) = t.glyph_index(c as u32).and_then(valid)
            {
                return Some(g);
            }
            if let Some(g) = self.info.glyph_name(code).and_then(|name| face.glyph_index_by_name(&name)).and_then(valid) {
                return Some(g);
            }
        }
        if let Some(t) = win_symbol {
            for base in [0xF000, 0xF100, 0xF200, 0] {
                if let Some(g) = t.glyph_index(base + code).and_then(valid) {
                    return Some(g);
                }
            }
        }
        if let Some(g) = mac_roman.and_then(|t| t.glyph_index(code)).and_then(valid) {
            return Some(g);
        }
        if let Some(g) = win_unicode.and_then(|t| t.glyph_index(code)).and_then(valid) {
            return Some(g);
        }
        // Subset fonts without a usable cmap are often indexed by code.
        (code < n).then_some(GlyphId(code as u16))
    }

    fn cff_gid(&self, table: &ttf_parser::cff::Table, code: u32) -> Option<GlyphId> {
        if self.info.composite {
            let gid = match &self.cid_to_gid {
                Some(map) => *map.get(&code)?,
                None => code as u16,
            };
            return Some(GlyphId(gid));
        }
        if let Some(Some(name)) = self.info.differences.get(code as usize)
            && let Some(g) = table.glyph_index_by_name(name)
        {
            return Some(g);
        }
        if self.info.base_encoding.is_some()
            && !self.info.symbolic
            && let Some(g) = self.info.glyph_name(code).and_then(|n| table.glyph_index_by_name(&n))
        {
            return Some(g);
        }
        if let Some(g) = table.glyph_index(code as u8).filter(|g| g.0 != 0) {
            return Some(g);
        }
        self.info.glyph_name(code).and_then(|n| table.glyph_index_by_name(&n))
    }

    fn type1_name(&self, t1: &Type1Font, code: u32) -> Option<String> {
        let c = code as usize;
        if let Some(Some(name)) = self.info.differences.get(c) {
            return Some(name.clone());
        }
        if self.info.base_encoding.is_some() && !self.info.symbolic {
            return self.info.glyph_name(code);
        }
        t1.encoding.get(c).cloned().flatten().or_else(|| self.info.glyph_name(code))
    }
}

/// Reads the embedded font program, if any and if it parses.
fn embedded_program(doc: &Document, info: &Font) -> Option<Program> {
    let desc = info.descriptor.as_ref()?;
    for key in [&b"FontFile2"[..], b"FontFile3", b"FontFile"] {
        let Ok(Object::Stream(s)) = doc.get_resolved(desc, key) else { continue };
        let Ok(data) = doc.decode_stream(&s) else { continue };
        let program = match key {
            b"FontFile" => Type1Font::parse(&data).map(Program::Type1),
            _ => {
                let subtype = s.dict.get(b"Subtype").and_then(Object::as_name).unwrap_or(b"");
                let is_sfnt = data.starts_with(b"OTTO")
                    || data.starts_with(&[0, 1, 0, 0])
                    || data.starts_with(b"true")
                    || data.starts_with(b"ttcf");
                if is_sfnt || subtype == b"OpenType" {
                    Face::parse(&data, 0).ok().map(|_| ())?;
                    Some(Program::Sfnt { data: Rc::new(FontData::Owned(data)), index: 0 })
                } else if ttf_parser::cff::Table::parse(&data).is_some() {
                    Some(Program::Cff(data))
                } else if data.starts_with(b"%!") {
                    // Occasionally a Type 1 font is stored as FontFile3.
                    Type1Font::parse(&data).map(Program::Type1)
                } else {
                    None
                }
            }
        };
        if program.is_some() {
            return program;
        }
    }
    None
}

/// Adapts ttf-parser's outline callbacks to a tiny-skia path.
struct Builder(PathBuilder);

impl ttf_parser::OutlineBuilder for Builder {
    fn move_to(&mut self, x: f32, y: f32) {
        self.0.move_to(x, y);
    }
    fn line_to(&mut self, x: f32, y: f32) {
        self.0.line_to(x, y);
    }
    fn quad_to(&mut self, x1: f32, y1: f32, x: f32, y: f32) {
        self.0.quad_to(x1, y1, x, y);
    }
    fn curve_to(&mut self, x1: f32, y1: f32, x2: f32, y2: f32, x: f32, y: f32) {
        self.0.cubic_to(x1, y1, x2, y2, x, y);
    }
    fn close(&mut self) {
        self.0.close();
    }
}

/// System fonts used in place of fonts that aren't embedded. The font
/// database is only built the first time a substitute is needed.
#[derive(Default)]
pub(crate) struct SystemFonts {
    db: Option<fontdb::Database>,
    loaded: HashMap<fontdb::ID, Rc<FontData>>,
}

impl SystemFonts {
    /// Finds a stand-in for the PDF font `base_font` (e.g. `Helvetica-Bold`).
    pub fn find(&mut self, base_font: &str, flags: i64, cjk: bool) -> Option<(Rc<FontData>, u32)> {
        let db = self.db.get_or_insert_with(|| {
            let mut db = fontdb::Database::new();
            db.load_system_fonts();
            db
        });

        // Drop a subset tag like `ABCDEF+`.
        let name = match base_font.split_once('+') {
            Some((tag, rest)) if tag.len() == 6 => rest,
            _ => base_font,
        };
        let lower = name.to_ascii_lowercase();
        let has = |words: &[&str]| words.iter().any(|w| lower.contains(w));
        let bold = has(&["bold", "black", "heavy", "semibold", "demi"]) || flags & 0x40000 != 0;
        let italic = has(&["italic", "oblique"]) || flags & 0x40 != 0;
        let fixed = has(&["courier", "mono", "consol", "typewriter"]) || flags & 1 != 0;
        let serif = has(&["times", "roman", "serif", "georgia", "garamond", "minion", "book", "palatino", "cmr", "century"])
            || (flags & 2 != 0 && !has(&["sans"]));

        // The family named in the PDF, e.g. "Arial" from "Arial,Bold".
        let family: String = name.split([',', '-']).next().unwrap_or("").to_string();
        let candidates: &[&str] = if cjk {
            &["Noto Sans CJK JP", "Noto Sans CJK SC", "Noto Serif CJK JP", "Source Han Sans", "WenQuanYi Micro Hei", "Droid Sans Fallback"]
        } else if has(&["symbol"]) {
            &["Standard Symbols PS", "Symbol", "OpenSymbol", "DejaVu Sans"]
        } else if has(&["dingbat"]) {
            &["D050000L", "Dingbats", "DejaVu Sans"]
        } else if fixed {
            &["Liberation Mono", "Cousine", "Nimbus Mono PS", "Courier New", "DejaVu Sans Mono"]
        } else if serif {
            &["Liberation Serif", "Tinos", "Nimbus Roman", "Times New Roman", "DejaVu Serif"]
        } else {
            &["Liberation Sans", "Arimo", "Nimbus Sans", "Arial", "Helvetica", "DejaVu Sans"]
        };
        let generic = if fixed {
            fontdb::Family::Monospace
        } else if serif {
            fontdb::Family::Serif
        } else {
            fontdb::Family::SansSerif
        };
        let mut families = vec![fontdb::Family::Name(&family)];
        families.extend(candidates.iter().map(|c| fontdb::Family::Name(c)));
        families.push(generic);

        let query = fontdb::Query {
            families: &families,
            weight: if bold { fontdb::Weight::BOLD } else { fontdb::Weight::NORMAL },
            stretch: fontdb::Stretch::Normal,
            style: if italic { fontdb::Style::Italic } else { fontdb::Style::Normal },
        };
        let id = db.query(&query)?;
        let face = db.face(id)?;
        let index = face.index;
        if let Some(data) = self.loaded.get(&id) {
            return Some((data.clone(), index));
        }
        let data = match &face.source {
            fontdb::Source::File(path) | fontdb::Source::SharedFile(path, _) => {
                let file = std::fs::File::open(path).ok()?;
                // SAFETY: read-only map of a system font file.
                FontData::Mapped(unsafe { memmap2::Mmap::map(&file).ok()? })
            }
            fontdb::Source::Binary(bytes) => FontData::Owned((**bytes).as_ref().to_vec()),
        };
        let data = Rc::new(data);
        self.loaded.insert(id, data.clone());
        Some((data, index))
    }
}
