//! Fonts for edited text.
//!
//! New text uses, in order of preference:
//! 1. the font the page already uses, when it is the requested family and
//!    style and has every character needed (the page already shows those
//!    characters in it, so they are in the embedded subset);
//! 2. a system font of the requested family and style, embedded with just
//!    the glyphs used (needs the `render` feature);
//! 3. one of the standard 14 fonts (Times, Helvetica, Courier), which every
//!    PDF reader has, for Latin text.

use std::collections::HashMap;
#[cfg(feature = "render")]
use std::collections::BTreeMap;

use crate::encoding::{char_to_glyph_name, BaseEncoding};
use crate::object::{Dictionary, Object};

/// What a page font looks like, worked out from its name and descriptor.
#[derive(Debug, Clone, PartialEq)]
pub struct FontLook {
    /// Family name, e.g. "Times" for `ABCDEF+Times-BoldItalic`.
    pub family: String,
    pub bold: bool,
    pub italic: bool,
    pub serif: bool,
    pub monospace: bool,
}

/// Derives the family and style from a PDF font's `/BaseFont` and
/// descriptor flags.
pub fn look_of(base_font: &str, flags: i64) -> FontLook {
    let name = match base_font.split_once('+') {
        Some((tag, rest)) if tag.len() == 6 => rest,
        _ => base_font,
    };
    let lower = name.to_ascii_lowercase();
    let has = |words: &[&str]| words.iter().any(|w| lower.contains(w));
    // Linux Libertine/Biolinum name styles by suffix: LinLibertineTB, …TI, …TBI.
    let libertine = lower.starts_with("lin") && (lower.contains("libertine") || lower.contains("biolinum"));
    let suffix = lower.trim_start_matches("linlibertine").trim_start_matches("linbiolinum");
    let bold = has(&["bold", "black", "heavy", "semibold", "demi"])
        || flags & 0x40000 != 0
        || (libertine && (suffix.contains('b') || suffix.ends_with('z')));
    let italic = has(&["italic", "oblique", "-it", "slanted"])
        || flags & 0x40 != 0
        || (libertine && (suffix.ends_with('i') || suffix.ends_with('z')));
    let monospace = has(&["courier", "mono", "consol", "typewriter"]) || flags & 1 != 0;
    let serif = has(&["times", "roman", "serif", "georgia", "garamond", "minion", "palatino", "cmr", "century", "libertin"])
        || (flags & 2 != 0 && !has(&["sans"]));
    // "Arial,Bold" / "Times-Roman" / "LinLibertineT-Bold" → the part before
    // the style; split CamelCase PostScript names ("TimesNewRomanPSMT").
    let base = name.split([',', '-']).next().unwrap_or(name);
    // "LinLibertineTB" → "LinLibertine".
    let base = if libertine { base.trim_end_matches(['T', 'O', 'B', 'I', 'Z']) } else { base };
    let base = base.trim_end_matches("PSMT").trim_end_matches("MT").trim_end_matches("PS");
    let mut family = String::new();
    let chars: Vec<char> = base.chars().collect();
    for (i, &c) in chars.iter().enumerate() {
        if i > 0 && c.is_uppercase() && chars[i - 1].is_lowercase() {
            family.push(' ');
        }
        family.push(c);
    }
    let family = match family.as_str() {
        "" => "Helvetica".to_string(),
        "Arial" | "Helvetica" | "Times" | "Courier" => family,
        _ => family,
    };
    FontLook { family, bold, italic, serif, monospace }
}

/// The standard 14 font for a look: Times, Helvetica or Courier.
pub(crate) fn standard_font(serif: bool, mono: bool, bold: bool, italic: bool) -> &'static str {
    match (mono, serif, bold, italic) {
        (true, _, false, false) => "Courier",
        (true, _, true, false) => "Courier-Bold",
        (true, _, false, true) => "Courier-Oblique",
        (true, _, true, true) => "Courier-BoldOblique",
        (false, true, false, false) => "Times-Roman",
        (false, true, true, false) => "Times-Bold",
        (false, true, false, true) => "Times-Italic",
        (false, true, true, true) => "Times-BoldItalic",
        (false, false, false, false) => "Helvetica",
        (false, false, true, false) => "Helvetica-Bold",
        (false, false, false, true) => "Helvetica-Oblique",
        (false, false, true, true) => "Helvetica-BoldOblique",
    }
}

/// A font that new text is set in.
pub(crate) enum Face {
    /// A font already on the page: char → (code bytes, width per unit of
    /// font size).
    Page { glyphs: HashMap<char, (Vec<u8>, f32)> },
    /// A standard 14 font with WinAnsi encoding.
    Standard { name: &'static str },
    /// A system font, embedded on save. Glyph ids are the codes.
    #[cfg(feature = "render")]
    System(Box<SystemFace>),
}

impl Face {
    /// Bytes to show `c`, or `None` if the font can't show it.
    pub fn encode(&mut self, c: char) -> Option<Vec<u8>> {
        match self {
            Face::Page { glyphs, .. } => glyphs.get(&c).map(|(b, _)| b.clone()),
            Face::Standard { .. } => win_ansi(c).map(|b| vec![b]),
            #[cfg(feature = "render")]
            Face::System(f) => f.encode(c),
        }
    }

    /// Advance width of `c` per unit of font size.
    pub fn width(&self, c: char) -> f32 {
        match self {
            Face::Page { glyphs, .. } => glyphs.get(&c).map_or(0.5, |(_, w)| *w),
            Face::Standard { name } => {
                let code = win_ansi(c).unwrap_or(b'?');
                let glyph = char_to_glyph_name(c);
                crate::std14::lookup(name)
                    .and_then(|s| s.width(glyph.as_deref(), code))
                    .map_or(0.5, |w| (w / 1000.0) as f32)
            }
            #[cfg(feature = "render")]
            Face::System(f) => f.width(c),
        }
    }

    /// Whether every character of `text` can be shown.
    #[cfg_attr(not(feature = "render"), allow(dead_code))]
    pub fn covers(&mut self, text: &str) -> bool {
        text.chars().filter(|c| !c.is_control()).all(|c| self.encode(c).is_some())
    }

    /// The font dictionary to add to the page, if it isn't there already.
    /// `add_object` stores an object (streams must be indirect) and returns
    /// a reference to it.
    pub fn font_object(&self, add_object: &mut dyn FnMut(Object) -> Object) -> Option<Object> {
        match self {
            Face::Page { .. } => None,
            Face::Standard { name } => {
                let mut d = Dictionary::new();
                d.set(b"Type".to_vec(), Object::Name(b"Font".to_vec()));
                d.set(b"Subtype".to_vec(), Object::Name(b"Type1".to_vec()));
                d.set(b"BaseFont".to_vec(), Object::Name(name.as_bytes().to_vec()));
                d.set(b"Encoding".to_vec(), Object::Name(b"WinAnsiEncoding".to_vec()));
                Some(add_object(Object::Dictionary(d)))
            }
            #[cfg(feature = "render")]
            Face::System(f) => f.font_object(add_object),
        }
    }
}

fn win_ansi(c: char) -> Option<u8> {
    if c.is_ascii() && !c.is_ascii_control() {
        return Some(c as u8);
    }
    (0x80..=0xFFu8).find(|&b| BaseEncoding::WinAnsi.decode(b) == Some(c))
}

/// A system font to embed: the font file, and the glyphs used so far.
#[cfg(feature = "render")]
pub(crate) struct SystemFace {
    data: std::sync::Arc<dyn AsRef<[u8]> + Send + Sync>,
    index: u32,
    post_script_name: String,
    look: FontLook,
    /// CFF outlines (OpenType): embedded whole as FontFile3/OpenType.
    cff: bool,
    units_per_em: f32,
    /// Glyph id → (advance in font units, the character it shows).
    used: BTreeMap<u16, (u16, char)>,
    widths: HashMap<char, f32>,
}

#[cfg(feature = "render")]
impl SystemFace {
    /// Finds a system font of `family` (or, failing that, one that looks
    /// alike) in the requested style. Only TrueType outlines can be
    /// embedded this way.
    pub fn find(look: &FontLook) -> Option<SystemFace> {
        let db = system_db();
        let weight = if look.bold { fontdb::Weight::BOLD } else { fontdb::Weight::NORMAL };
        let style = if look.italic { fontdb::Style::Italic } else { fontdb::Style::Normal };
        let fallback = if look.monospace {
            fontdb::Family::Monospace
        } else if look.serif {
            fontdb::Family::Serif
        } else {
            fontdb::Family::SansSerif
        };
        // The family as installed: names in PDFs are often abbreviated
        // PostScript names ("LinBiolinumTB" for "Linux Biolinum O").
        let installed = closest_family(db, &look.family);
        let alias = alias_of(&look.family);
        let mut candidates = vec![fontdb::Family::Name(&look.family)];
        if let Some(name) = &installed {
            candidates.push(fontdb::Family::Name(name));
        }
        for a in alias {
            candidates.push(fontdb::Family::Name(a));
        }
        candidates.push(fallback);
        for fam in candidates {
            let query = fontdb::Query { families: &[fam], weight, style, stretch: fontdb::Stretch::Normal };
            let Some(id) = db.query(&query) else { continue };
            let face_info = db.face(id)?;
            // `query` falls back to any family; only accept the right one
            // for the named family, any for the generic fallback.
            if let fontdb::Family::Name(want) = fam
                && !face_info.families.iter().any(|(f, _)| f.eq_ignore_ascii_case(want))
            {
                continue;
            }
            let (data, index) = load_face(db, id)?;
            let face = ttf_parser::Face::parse((*data).as_ref(), index).ok()?;
            let cff = face.tables().glyf.is_none();
            // CFF-based fonts are embedded whole, and only from single-font
            // files (not collections).
            if cff && (face.tables().cff.is_none() || index != 0) {
                continue;
            }
            return Some(SystemFace {
                cff,
                units_per_em: face.units_per_em() as f32,
                post_script_name: face_info.post_script_name.clone(),
                look: FontLook { family: face_info.families.first().map_or(look.family.clone(), |f| f.0.clone()), ..look.clone() },
                data,
                index,
                used: BTreeMap::new(),
                widths: HashMap::new(),
            });
        }
        None
    }

    fn face(&self) -> Option<ttf_parser::Face<'_>> {
        ttf_parser::Face::parse((*self.data).as_ref(), self.index).ok()
    }

    fn encode(&mut self, c: char) -> Option<Vec<u8>> {
        let face = self.face()?;
        let gid = face.glyph_index(c)?;
        let adv = face.glyph_hor_advance(gid).unwrap_or(0);
        self.widths.insert(c, adv as f32 / self.units_per_em);
        self.used.entry(gid.0).or_insert((adv, c));
        Some(gid.0.to_be_bytes().to_vec())
    }

    fn width(&self, c: char) -> f32 {
        if let Some(w) = self.widths.get(&c) {
            return *w;
        }
        let Some(face) = self.face() else { return 0.5 };
        face.glyph_index(c)
            .and_then(|g| face.glyph_hor_advance(g))
            .map_or(0.5, |a| a as f32 / self.units_per_em)
    }

    /// A Type0 font (Identity-H, codes are glyph ids) with the used glyphs
    /// embedded.
    fn font_object(&self, add: &mut dyn FnMut(Object) -> Object) -> Option<Object> {
        let face = self.face()?;
        let gids: Vec<u16> = self.used.keys().copied().collect();
        let program = if self.cff {
            (*self.data).as_ref().to_vec()
        } else {
            super::subset::subset_truetype((*self.data).as_ref(), self.index, &gids)?
        };
        // A subset tag from the glyphs used, as PDF producers do.
        let hash = gids.iter().fold(0u32, |h, &g| h.wrapping_mul(31).wrapping_add(g as u32 + 7));
        let tag: String = (0..6).map(|i| (b'A' + ((hash >> (i * 5)) % 26) as u8) as char).collect();
        let name = format!("{tag}+{}", self.post_script_name.replace(' ', ""));
        let scale = 1000.0 / self.units_per_em;

        let n = |v: f32| Object::Integer(v.round() as i64);
        let mut file_dict = Dictionary::new();
        if self.cff {
            file_dict.set(b"Subtype".to_vec(), Object::Name(b"OpenType".to_vec()));
        } else {
            file_dict.set(b"Length1".to_vec(), Object::Integer(program.len() as i64));
        }
        let mut enc = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
        std::io::Write::write_all(&mut enc, &program).ok()?;
        file_dict.set(b"Filter".to_vec(), Object::Name(b"FlateDecode".to_vec()));
        let file = add(Object::Stream(crate::object::Stream { dict: file_dict, raw: enc.finish().ok()?.into() }));

        let bbox = face.global_bounding_box();
        let mut flags = 32; // nonsymbolic
        if self.look.monospace {
            flags |= 1;
        }
        if self.look.serif {
            flags |= 2;
        }
        if self.look.italic {
            flags |= 64;
        }
        let mut desc = Dictionary::new();
        desc.set(b"Type".to_vec(), Object::Name(b"FontDescriptor".to_vec()));
        desc.set(b"FontName".to_vec(), Object::Name(name.clone().into_bytes()));
        desc.set(b"Flags".to_vec(), Object::Integer(flags));
        desc.set(
            b"FontBBox".to_vec(),
            Object::Array(vec![
                n(bbox.x_min as f32 * scale),
                n(bbox.y_min as f32 * scale),
                n(bbox.x_max as f32 * scale),
                n(bbox.y_max as f32 * scale),
            ]),
        );
        desc.set(b"ItalicAngle".to_vec(), Object::Real(face.italic_angle() as f64));
        desc.set(b"Ascent".to_vec(), n(face.ascender() as f32 * scale));
        desc.set(b"Descent".to_vec(), n(face.descender() as f32 * scale));
        desc.set(b"CapHeight".to_vec(), n(face.capital_height().unwrap_or(face.ascender()) as f32 * scale));
        desc.set(b"StemV".to_vec(), Object::Integer(if self.look.bold { 140 } else { 80 }));
        desc.set(if self.cff { b"FontFile3".to_vec() } else { b"FontFile2".to_vec() }, file);
        let desc = add(Object::Dictionary(desc));

        let mut widths = Vec::new();
        for (&gid, &(adv, _)) in &self.used {
            widths.push(Object::Integer(gid as i64));
            widths.push(Object::Array(vec![n(adv as f32 * scale)]));
        }
        let mut sysinfo = Dictionary::new();
        sysinfo.set(b"Registry".to_vec(), Object::String(b"Adobe".to_vec()));
        sysinfo.set(b"Ordering".to_vec(), Object::String(b"Identity".to_vec()));
        sysinfo.set(b"Supplement".to_vec(), Object::Integer(0));
        let mut cid = Dictionary::new();
        cid.set(b"Type".to_vec(), Object::Name(b"Font".to_vec()));
        cid.set(b"Subtype".to_vec(), Object::Name(if self.cff { b"CIDFontType0".to_vec() } else { b"CIDFontType2".to_vec() }));
        cid.set(b"BaseFont".to_vec(), Object::Name(name.clone().into_bytes()));
        cid.set(b"CIDSystemInfo".to_vec(), Object::Dictionary(sysinfo));
        cid.set(b"FontDescriptor".to_vec(), desc);
        cid.set(b"W".to_vec(), Object::Array(widths));
        if !self.cff {
            cid.set(b"CIDToGIDMap".to_vec(), Object::Name(b"Identity".to_vec()));
        }
        let cid = add(Object::Dictionary(cid));

        // ToUnicode, so the new text can be selected and copied.
        let mut cmap = String::from(
            "/CIDInit /ProcSet findresource begin 12 dict begin begincmap\n\
             /CIDSystemInfo << /Registry (Adobe) /Ordering (UCS) /Supplement 0 >> def\n\
             /CMapName /Adobe-Identity-UCS def /CMapType 2 def\n\
             1 begincodespacerange <0000> <FFFF> endcodespacerange\n",
        );
        let entries: Vec<(u16, char)> = self.used.iter().map(|(&g, &(_, c))| (g, c)).collect();
        for chunk in entries.chunks(100) {
            cmap += &format!("{} beginbfchar\n", chunk.len());
            for (g, c) in chunk {
                let utf16: String = c.encode_utf16(&mut [0; 2]).iter().map(|u| format!("{u:04X}")).collect();
                cmap += &format!("<{g:04X}> <{utf16}>\n");
            }
            cmap += "endbfchar\n";
        }
        cmap += "endcmap CMapName currentdict /CMap defineresource pop end end\n";
        let to_unicode = add(Object::Stream(crate::object::Stream { dict: Dictionary::new(), raw: cmap.into_bytes().into() }));

        let mut font = Dictionary::new();
        font.set(b"Type".to_vec(), Object::Name(b"Font".to_vec()));
        font.set(b"Subtype".to_vec(), Object::Name(b"Type0".to_vec()));
        font.set(b"BaseFont".to_vec(), Object::Name(name.into_bytes()));
        font.set(b"Encoding".to_vec(), Object::Name(b"Identity-H".to_vec()));
        font.set(b"DescendantFonts".to_vec(), Object::Array(vec![cid]));
        font.set(b"ToUnicode".to_vec(), to_unicode);
        Some(add(Object::Dictionary(font)))
    }
}

/// Installed stand-ins for well-known families that PDFs name differently.
#[cfg(feature = "render")]
fn alias_of(family: &str) -> &'static [&'static str] {
    let n: String = family.chars().filter(|c| c.is_ascii_alphanumeric()).collect::<String>().to_ascii_lowercase();
    let table: [(&str, &[&str]); 12] = [
        ("libertine", &["Libertinus Serif", "Linux Libertine O"]),
        ("biolinum", &["Libertinus Sans", "Linux Biolinum O"]),
        ("nimbusrom", &["Nimbus Roman", "Liberation Serif"]),
        ("nimbussan", &["Nimbus Sans", "Liberation Sans"]),
        ("nimbusmon", &["Nimbus Mono PS", "Liberation Mono"]),
        ("timesnewroman", &["Liberation Serif", "Tinos", "Nimbus Roman"]),
        ("times", &["Liberation Serif", "Nimbus Roman"]),
        ("arial", &["Liberation Sans", "Arimo", "Nimbus Sans"]),
        ("helvetica", &["Liberation Sans", "Nimbus Sans"]),
        ("couriernew", &["Liberation Mono", "Cousine"]),
        ("courier", &["Liberation Mono", "Nimbus Mono PS"]),
        ("cmr", &["Latin Modern Roman", "CMU Serif"]),
    ];
    table.iter().find(|(k, _)| n.contains(k)).map_or(&[], |(_, v)| v)
}

/// The installed family whose name best matches `family` (ignoring case,
/// spaces and punctuation; the longer the shared start, the better).
#[cfg(feature = "render")]
fn closest_family(db: &fontdb::Database, family: &str) -> Option<String> {
    let norm = |s: &str| s.chars().filter(|c| c.is_ascii_alphanumeric()).collect::<String>().to_ascii_lowercase();
    let want = norm(family);
    if want.len() < 4 {
        return None;
    }
    let mut best: Option<(usize, String)> = None;
    for face in db.faces() {
        for name in face.families.iter().map(|(n, _)| n.as_str()).chain([face.post_script_name.as_str()]) {
            let have = norm(name);
            let shared = want.chars().zip(have.chars()).take_while(|(a, b)| a == b).count();
            // Mostly the same name ("linbiolinum" in "linbiolinumtb" and
            // "linbiolinumo"), or one contains the other.
            let good = shared >= 6 && shared * 10 >= want.len().min(have.len()) * 7 || (have.len() >= 5 && want.contains(&have));
            if good && best.as_ref().is_none_or(|(s, _)| shared > *s) {
                let fam = face.families.first().map_or(name.to_string(), |f| f.0.clone());
                best = Some((shared, fam));
            }
        }
    }
    best.map(|(_, f)| f)
}

#[cfg(feature = "render")]
fn system_db() -> &'static fontdb::Database {
    static DB: std::sync::OnceLock<fontdb::Database> = std::sync::OnceLock::new();
    DB.get_or_init(|| {
        let mut db = fontdb::Database::new();
        db.load_system_fonts();
        db
    })
}

#[cfg(feature = "render")]
type FaceData = std::sync::Arc<dyn AsRef<[u8]> + Send + Sync>;

#[cfg(feature = "render")]
fn load_face(db: &fontdb::Database, id: fontdb::ID) -> Option<(FaceData, u32)> {
    let face = db.face(id)?;
    let data: FaceData = match &face.source {
        fontdb::Source::Binary(b) => b.clone(),
        fontdb::Source::File(path) => std::sync::Arc::new(std::fs::read(path).ok()?),
        fontdb::Source::SharedFile(_, b) => b.clone(),
    };
    Some((data, face.index))
}

/// Font families installed on the system, sorted (for font menus).
#[cfg(feature = "render")]
pub fn system_font_families() -> Vec<String> {
    let mut names: Vec<String> =
        system_db().faces().filter_map(|f| f.families.first().map(|(n, _)| n.clone())).collect();
    names.sort_by_key(|n| n.to_lowercase());
    names.dedup();
    names
}

/// The font file that best shows `family` in a style on screen (the same
/// family new text would be embedded in, or a look-alike): (file data,
/// face index in the file).
#[cfg(feature = "render")]
pub fn screen_font(family: &str, bold: bool, italic: bool) -> Option<(Vec<u8>, u32)> {
    let db = system_db();
    let look = look_of(family, 0);
    let installed = closest_family(db, family);
    let mut names: Vec<&str> = vec![family];
    if let Some(n) = &installed {
        names.push(n);
    }
    names.extend(alias_of(family));
    let generic = if look.monospace {
        fontdb::Family::Monospace
    } else if look.serif {
        fontdb::Family::Serif
    } else {
        fontdb::Family::SansSerif
    };
    let weight = if bold { fontdb::Weight::BOLD } else { fontdb::Weight::NORMAL };
    let style = if italic { fontdb::Style::Italic } else { fontdb::Style::Normal };
    for fam in names.into_iter().map(fontdb::Family::Name).chain([generic]) {
        let query = fontdb::Query { families: &[fam], weight, style, stretch: fontdb::Stretch::Normal };
        let Some(id) = db.query(&query) else { continue };
        if let fontdb::Family::Name(want) = fam
            && !db.face(id)?.families.iter().any(|(f, _)| f.eq_ignore_ascii_case(want))
        {
            continue;
        }
        let (data, index) = load_face(db, id)?;
        return Some(((*data).as_ref().to_vec(), index));
    }
    None
}

/// Handwriting fonts installed on the system, best first, for typed
/// signatures: script and hand-lettered families that come with Windows,
/// macOS and common Linux font packages, then any other family whose name
/// says it is a script. With few of those, italic serif and sans fonts
/// fill in (an italic reads more like writing than an upright face).
#[cfg(feature = "render")]
pub fn handwriting_fonts() -> Vec<String> {
    const KNOWN: &[&str] = &[
        // Windows.
        "Segoe Script", "Lucida Handwriting", "Edwardian Script ITC", "Brush Script MT", "Freestyle Script",
        "Vladimir Script", "Kunstler Script", "Ink Free", "Segoe Print", "Mistral",
        // macOS.
        "Snell Roundhand", "Apple Chancery", "Savoye LET", "Bradley Hand", "Zapfino", "Noteworthy",
        // Linux packages and Google fonts.
        "Z003", "URW Chancery L", "TeX Gyre Chorus", "Great Vibes", "Pinyon Script", "Alex Brush", "Allura",
        "Parisienne", "Sacramento", "Dancing Script", "Satisfy", "Kaushan Script", "Caveat", "Homemade Apple",
        "QTMerryScript", "QTHandwriting", "QTChanceryType", "Purisa", "Comic Neue",
    ];
    const MOST: usize = 8;
    let db = system_db();
    let families: Vec<&str> = {
        let mut f: Vec<&str> = db.faces().filter_map(|f| f.families.first().map(|(n, _)| n.as_str())).collect();
        f.sort_unstable();
        f.dedup();
        f
    };
    let mut out: Vec<String> = Vec::new();
    let push = |name: &str, out: &mut Vec<String>| {
        if out.len() < MOST && !out.iter().any(|o| o.eq_ignore_ascii_case(name)) {
            out.push(name.to_string());
        }
    };
    for want in KNOWN {
        if let Some(have) = families.iter().find(|f| f.eq_ignore_ascii_case(want)) {
            push(have, &mut out);
        }
    }
    for f in &families {
        let l = f.to_ascii_lowercase();
        let scripty = ["script", "handwriting", "chancery", "calligraph"].iter().any(|w| l.contains(w));
        let not = ["math", "noto", "symbol", "mono", "fetamont"].iter().any(|w| l.contains(w));
        if scripty && !not {
            push(f, &mut out);
        }
    }
    // TeX Gyre Chorus and URW Chancery L are copies of Z003.
    if out.iter().any(|f| f == "Z003") {
        out.retain(|f| f != "TeX Gyre Chorus" && f != "URW Chancery L");
    }
    if out.len() < 3 {
        for generic in [fontdb::Family::Serif, fontdb::Family::SansSerif] {
            let query = fontdb::Query { families: &[generic], weight: fontdb::Weight::NORMAL, style: fontdb::Style::Italic, stretch: fontdb::Stretch::Normal };
            if let Some(name) = db.query(&query).and_then(|id| db.face(id)).and_then(|f| f.families.first().map(|(n, _)| n.clone())) {
                push(&name, &mut out);
            }
        }
    }
    out
}

/// The file of an installed family (`(file data, face index)`), its
/// italic when it has one and `italic` is asked for. Unlike
/// [`screen_font`], nothing else stands in for a missing family.
#[cfg(feature = "render")]
pub fn family_font(family: &str, italic: bool) -> Option<(Vec<u8>, u32)> {
    let db = system_db();
    let style = if italic { fontdb::Style::Italic } else { fontdb::Style::Normal };
    let query = fontdb::Query { families: &[fontdb::Family::Name(family)], weight: fontdb::Weight::NORMAL, style, stretch: fontdb::Stretch::Normal };
    let id = db.query(&query)?;
    if !db.face(id)?.families.iter().any(|(f, _)| f.eq_ignore_ascii_case(family)) {
        return None;
    }
    let (data, index) = load_face(db, id)?;
    Some(((*data).as_ref().to_vec(), index))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn looks_from_font_names() {
        let l = look_of("ABCDEF+TimesNewRomanPS-BoldItalicMT", 0);
        assert_eq!((l.family.as_str(), l.bold, l.italic, l.serif), ("Times New Roman", true, true, true));
        let l = look_of("Arial,Bold", 0);
        assert_eq!((l.family.as_str(), l.bold, l.italic, l.serif), ("Arial", true, false, false));
        let l = look_of("Courier", 0);
        assert!(l.monospace);
        assert_eq!(standard_font(true, false, false, false), "Times-Roman");
    }
}
