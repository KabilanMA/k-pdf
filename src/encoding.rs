//! Character encodings: PDF text strings, simple-font base encodings and
//! glyph names (ISO 32000 §7.9.2, Annex D).

/// The built-in single-byte encodings a simple font can be based on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BaseEncoding {
    Standard,
    WinAnsi,
    MacRoman,
    PdfDoc,
}

impl BaseEncoding {
    pub fn from_name(name: &[u8]) -> Option<Self> {
        match name {
            b"StandardEncoding" => Some(Self::Standard),
            b"WinAnsiEncoding" => Some(Self::WinAnsi),
            b"MacRomanEncoding" => Some(Self::MacRoman),
            b"PDFDocEncoding" => Some(Self::PdfDoc),
            _ => None,
        }
    }

    /// Maps a byte to Unicode; `None` for unmapped codes.
    pub fn decode(self, b: u8) -> Option<char> {
        let c = match (self, b) {
            // StandardEncoding uses curly quotes in the ASCII range.
            (Self::Standard, b'\'') => '\u{2019}',
            (Self::Standard, b'`') => '\u{2018}',
            (Self::PdfDoc, 0x18..=0x1F) => PDF_DOC_LOW[(b - 0x18) as usize],
            (_, 0x00..=0x7F) => b as char,
            (Self::Standard, _) => STANDARD_HIGH[(b - 0x80) as usize],
            (Self::WinAnsi, _) => WIN_ANSI_HIGH[(b - 0x80) as usize],
            (Self::MacRoman, _) => MAC_ROMAN_HIGH[(b - 0x80) as usize],
            (Self::PdfDoc, _) => PDF_DOC_HIGH[(b - 0x80) as usize],
        };
        (c != '\u{FFFD}').then_some(c)
    }
}

/// Decodes a PDF "text string" (used in /Info, outlines, annotations…):
/// UTF-16BE or UTF-8 when prefixed with a BOM, PDFDocEncoding otherwise.
pub fn decode_text_string(bytes: &[u8]) -> String {
    if let Some(rest) = bytes.strip_prefix(&[0xFE, 0xFF]) {
        let units: Vec<u16> = rest.chunks_exact(2).map(|c| u16::from_be_bytes([c[0], c[1]])).collect();
        return String::from_utf16_lossy(&units);
    }
    if let Some(rest) = bytes.strip_prefix(&[0xFF, 0xFE]) {
        // Not allowed by the spec, but some writers emit little-endian.
        let units: Vec<u16> = rest.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
        return String::from_utf16_lossy(&units);
    }
    if let Some(rest) = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]) {
        return String::from_utf8_lossy(rest).into_owned(); // PDF 2.0
    }
    bytes.iter().map(|&b| BaseEncoding::PdfDoc.decode(b).unwrap_or('\u{FFFD}')).collect()
}

/// Maps an Adobe glyph name (e.g. `eacute`, `uni00E9`, `f_i`) to text.
pub fn glyph_name_to_unicode(name: &str) -> Option<String> {
    // Drop variant suffixes: `a.sc`, `one.oldstyle`.
    let base = name.split('.').next().unwrap_or(name);
    if base.is_empty() {
        return None;
    }
    // Ligatures written as components: `f_f_i`.
    if base.contains('_') {
        let parts: Option<String> = base.split('_').map(glyph_name_to_unicode).collect();
        return parts;
    }
    if base.len() == 1 && base.as_bytes()[0].is_ascii_alphabetic() {
        return Some(base.to_string());
    }
    if let Ok(i) = GLYPH_NAMES.binary_search_by(|(n, _)| (*n).cmp(base)) {
        return Some(GLYPH_NAMES[i].1.to_string());
    }
    // `uniXXXX[XXXX…]`: one or more UTF-16 code units, 4 hex digits each.
    if let Some(hex) = base.strip_prefix("uni")
        && hex.len() >= 4 && hex.len() % 4 == 0 {
            let units: Option<Vec<u16>> =
                (0..hex.len()).step_by(4).map(|i| u16::from_str_radix(&hex[i..i + 4], 16).ok()).collect();
            return units.map(|u| String::from_utf16_lossy(&u));
        }
    // `uXXXX` to `uXXXXXX`: a single code point.
    if let Some(hex) = base.strip_prefix('u')
        && (4..=6).contains(&hex.len()) {
            return u32::from_str_radix(hex, 16).ok().and_then(char::from_u32).map(String::from);
        }
    None
}

/// Reverse of [`glyph_name_to_unicode`] for single characters.
#[cfg_attr(not(feature = "render"), allow(dead_code))]
pub fn char_to_glyph_name(c: char) -> Option<String> {
    if c.is_ascii_alphabetic() {
        return Some(c.to_string());
    }
    if let Some((name, _)) = GLYPH_NAMES.iter().find(|(_, g)| *g == c) {
        return Some((*name).to_string());
    }
    Some(format!("uni{:04X}", c as u32)).filter(|_| (c as u32) <= 0xFFFF)
}

/// PDFDocEncoding 0x18–0x1F (spacing diacritics).
const PDF_DOC_LOW: [char; 8] =
    ['\u{02D8}', '\u{02C7}', '\u{02C6}', '\u{02D9}', '\u{02DD}', '\u{02DB}', '\u{02DA}', '\u{02DC}'];

// ---------------------------------------------------------------------------
// Tables below were generated from Python's codec and Unicode name data.
// ---------------------------------------------------------------------------

/// WinAnsiEncoding (Windows-1252), bytes 0x80–0xFF.
pub(crate) const WIN_ANSI_HIGH: [char; 128] = [
    '\u{20AC}', '\u{FFFD}', '\u{201A}', '\u{0192}', '\u{201E}', '\u{2026}', '\u{2020}', '\u{2021}',
    '\u{02C6}', '\u{2030}', '\u{0160}', '\u{2039}', '\u{0152}', '\u{FFFD}', '\u{017D}', '\u{FFFD}',
    '\u{FFFD}', '\u{2018}', '\u{2019}', '\u{201C}', '\u{201D}', '\u{2022}', '\u{2013}', '\u{2014}',
    '\u{02DC}', '\u{2122}', '\u{0161}', '\u{203A}', '\u{0153}', '\u{FFFD}', '\u{017E}', '\u{0178}',
    '\u{00A0}', '\u{00A1}', '\u{00A2}', '\u{00A3}', '\u{00A4}', '\u{00A5}', '\u{00A6}', '\u{00A7}',
    '\u{00A8}', '\u{00A9}', '\u{00AA}', '\u{00AB}', '\u{00AC}', '\u{00AD}', '\u{00AE}', '\u{00AF}',
    '\u{00B0}', '\u{00B1}', '\u{00B2}', '\u{00B3}', '\u{00B4}', '\u{00B5}', '\u{00B6}', '\u{00B7}',
    '\u{00B8}', '\u{00B9}', '\u{00BA}', '\u{00BB}', '\u{00BC}', '\u{00BD}', '\u{00BE}', '\u{00BF}',
    '\u{00C0}', '\u{00C1}', '\u{00C2}', '\u{00C3}', '\u{00C4}', '\u{00C5}', '\u{00C6}', '\u{00C7}',
    '\u{00C8}', '\u{00C9}', '\u{00CA}', '\u{00CB}', '\u{00CC}', '\u{00CD}', '\u{00CE}', '\u{00CF}',
    '\u{00D0}', '\u{00D1}', '\u{00D2}', '\u{00D3}', '\u{00D4}', '\u{00D5}', '\u{00D6}', '\u{00D7}',
    '\u{00D8}', '\u{00D9}', '\u{00DA}', '\u{00DB}', '\u{00DC}', '\u{00DD}', '\u{00DE}', '\u{00DF}',
    '\u{00E0}', '\u{00E1}', '\u{00E2}', '\u{00E3}', '\u{00E4}', '\u{00E5}', '\u{00E6}', '\u{00E7}',
    '\u{00E8}', '\u{00E9}', '\u{00EA}', '\u{00EB}', '\u{00EC}', '\u{00ED}', '\u{00EE}', '\u{00EF}',
    '\u{00F0}', '\u{00F1}', '\u{00F2}', '\u{00F3}', '\u{00F4}', '\u{00F5}', '\u{00F6}', '\u{00F7}',
    '\u{00F8}', '\u{00F9}', '\u{00FA}', '\u{00FB}', '\u{00FC}', '\u{00FD}', '\u{00FE}', '\u{00FF}',
];

/// MacRomanEncoding, bytes 0x80–0xFF.
pub(crate) const MAC_ROMAN_HIGH: [char; 128] = [
    '\u{00C4}', '\u{00C5}', '\u{00C7}', '\u{00C9}', '\u{00D1}', '\u{00D6}', '\u{00DC}', '\u{00E1}',
    '\u{00E0}', '\u{00E2}', '\u{00E4}', '\u{00E3}', '\u{00E5}', '\u{00E7}', '\u{00E9}', '\u{00E8}',
    '\u{00EA}', '\u{00EB}', '\u{00ED}', '\u{00EC}', '\u{00EE}', '\u{00EF}', '\u{00F1}', '\u{00F3}',
    '\u{00F2}', '\u{00F4}', '\u{00F6}', '\u{00F5}', '\u{00FA}', '\u{00F9}', '\u{00FB}', '\u{00FC}',
    '\u{2020}', '\u{00B0}', '\u{00A2}', '\u{00A3}', '\u{00A7}', '\u{2022}', '\u{00B6}', '\u{00DF}',
    '\u{00AE}', '\u{00A9}', '\u{2122}', '\u{00B4}', '\u{00A8}', '\u{2260}', '\u{00C6}', '\u{00D8}',
    '\u{221E}', '\u{00B1}', '\u{2264}', '\u{2265}', '\u{00A5}', '\u{00B5}', '\u{2202}', '\u{2211}',
    '\u{220F}', '\u{03C0}', '\u{222B}', '\u{00AA}', '\u{00BA}', '\u{03A9}', '\u{00E6}', '\u{00F8}',
    '\u{00BF}', '\u{00A1}', '\u{00AC}', '\u{221A}', '\u{0192}', '\u{2248}', '\u{2206}', '\u{00AB}',
    '\u{00BB}', '\u{2026}', '\u{00A0}', '\u{00C0}', '\u{00C3}', '\u{00D5}', '\u{0152}', '\u{0153}',
    '\u{2013}', '\u{2014}', '\u{201C}', '\u{201D}', '\u{2018}', '\u{2019}', '\u{00F7}', '\u{25CA}',
    '\u{00FF}', '\u{0178}', '\u{2044}', '\u{20AC}', '\u{2039}', '\u{203A}', '\u{FB01}', '\u{FB02}',
    '\u{2021}', '\u{00B7}', '\u{201A}', '\u{201E}', '\u{2030}', '\u{00C2}', '\u{00CA}', '\u{00C1}',
    '\u{00CB}', '\u{00C8}', '\u{00CD}', '\u{00CE}', '\u{00CF}', '\u{00CC}', '\u{00D3}', '\u{00D4}',
    '\u{F8FF}', '\u{00D2}', '\u{00DA}', '\u{00DB}', '\u{00D9}', '\u{0131}', '\u{02C6}', '\u{02DC}',
    '\u{00AF}', '\u{02D8}', '\u{02D9}', '\u{02DA}', '\u{00B8}', '\u{02DD}', '\u{02DB}', '\u{02C7}',
];

/// Adobe StandardEncoding, bytes 0x80–0xFF (U+FFFD where undefined).
pub(crate) const STANDARD_HIGH: [char; 128] = [
    '\u{FFFD}', '\u{FFFD}', '\u{FFFD}', '\u{FFFD}', '\u{FFFD}', '\u{FFFD}', '\u{FFFD}', '\u{FFFD}',
    '\u{FFFD}', '\u{FFFD}', '\u{FFFD}', '\u{FFFD}', '\u{FFFD}', '\u{FFFD}', '\u{FFFD}', '\u{FFFD}',
    '\u{FFFD}', '\u{FFFD}', '\u{FFFD}', '\u{FFFD}', '\u{FFFD}', '\u{FFFD}', '\u{FFFD}', '\u{FFFD}',
    '\u{FFFD}', '\u{FFFD}', '\u{FFFD}', '\u{FFFD}', '\u{FFFD}', '\u{FFFD}', '\u{FFFD}', '\u{FFFD}',
    '\u{FFFD}', '\u{00A1}', '\u{00A2}', '\u{00A3}', '\u{2044}', '\u{00A5}', '\u{0192}', '\u{00A7}',
    '\u{00A4}', '\u{0027}', '\u{201C}', '\u{00AB}', '\u{2039}', '\u{203A}', '\u{FB01}', '\u{FB02}',
    '\u{FFFD}', '\u{2013}', '\u{2020}', '\u{2021}', '\u{00B7}', '\u{FFFD}', '\u{00B6}', '\u{2022}',
    '\u{201A}', '\u{201E}', '\u{201D}', '\u{00BB}', '\u{2026}', '\u{2030}', '\u{FFFD}', '\u{00BF}',
    '\u{FFFD}', '\u{0060}', '\u{00B4}', '\u{02C6}', '\u{02DC}', '\u{00AF}', '\u{02D8}', '\u{02D9}',
    '\u{00A8}', '\u{FFFD}', '\u{02DA}', '\u{00B8}', '\u{FFFD}', '\u{02DD}', '\u{02DB}', '\u{02C7}',
    '\u{2014}', '\u{FFFD}', '\u{FFFD}', '\u{FFFD}', '\u{FFFD}', '\u{FFFD}', '\u{FFFD}', '\u{FFFD}',
    '\u{FFFD}', '\u{FFFD}', '\u{FFFD}', '\u{FFFD}', '\u{FFFD}', '\u{FFFD}', '\u{FFFD}', '\u{FFFD}',
    '\u{FFFD}', '\u{00C6}', '\u{FFFD}', '\u{00AA}', '\u{FFFD}', '\u{FFFD}', '\u{FFFD}', '\u{FFFD}',
    '\u{0141}', '\u{00D8}', '\u{0152}', '\u{00BA}', '\u{FFFD}', '\u{FFFD}', '\u{FFFD}', '\u{FFFD}',
    '\u{FFFD}', '\u{00E6}', '\u{FFFD}', '\u{FFFD}', '\u{FFFD}', '\u{0131}', '\u{FFFD}', '\u{FFFD}',
    '\u{0142}', '\u{00F8}', '\u{0153}', '\u{00DF}', '\u{FFFD}', '\u{FFFD}', '\u{FFFD}', '\u{FFFD}',
];

/// PDFDocEncoding, bytes 0x80–0xFF (ISO 32000 Annex D).
pub(crate) const PDF_DOC_HIGH: [char; 128] = [
    '\u{2022}', '\u{2020}', '\u{2021}', '\u{2026}', '\u{2014}', '\u{2013}', '\u{0192}', '\u{2044}',
    '\u{2039}', '\u{203A}', '\u{2212}', '\u{2030}', '\u{201E}', '\u{201C}', '\u{201D}', '\u{2018}',
    '\u{2019}', '\u{201A}', '\u{2122}', '\u{FB01}', '\u{FB02}', '\u{0141}', '\u{0152}', '\u{0160}',
    '\u{0178}', '\u{017D}', '\u{0131}', '\u{0142}', '\u{0153}', '\u{0161}', '\u{017E}', '\u{FFFD}',
    '\u{20AC}', '\u{00A1}', '\u{00A2}', '\u{00A3}', '\u{00A4}', '\u{00A5}', '\u{00A6}', '\u{00A7}',
    '\u{00A8}', '\u{00A9}', '\u{00AA}', '\u{00AB}', '\u{00AC}', '\u{FFFD}', '\u{00AE}', '\u{00AF}',
    '\u{00B0}', '\u{00B1}', '\u{00B2}', '\u{00B3}', '\u{00B4}', '\u{00B5}', '\u{00B6}', '\u{00B7}',
    '\u{00B8}', '\u{00B9}', '\u{00BA}', '\u{00BB}', '\u{00BC}', '\u{00BD}', '\u{00BE}', '\u{00BF}',
    '\u{00C0}', '\u{00C1}', '\u{00C2}', '\u{00C3}', '\u{00C4}', '\u{00C5}', '\u{00C6}', '\u{00C7}',
    '\u{00C8}', '\u{00C9}', '\u{00CA}', '\u{00CB}', '\u{00CC}', '\u{00CD}', '\u{00CE}', '\u{00CF}',
    '\u{00D0}', '\u{00D1}', '\u{00D2}', '\u{00D3}', '\u{00D4}', '\u{00D5}', '\u{00D6}', '\u{00D7}',
    '\u{00D8}', '\u{00D9}', '\u{00DA}', '\u{00DB}', '\u{00DC}', '\u{00DD}', '\u{00DE}', '\u{00DF}',
    '\u{00E0}', '\u{00E1}', '\u{00E2}', '\u{00E3}', '\u{00E4}', '\u{00E5}', '\u{00E6}', '\u{00E7}',
    '\u{00E8}', '\u{00E9}', '\u{00EA}', '\u{00EB}', '\u{00EC}', '\u{00ED}', '\u{00EE}', '\u{00EF}',
    '\u{00F0}', '\u{00F1}', '\u{00F2}', '\u{00F3}', '\u{00F4}', '\u{00F5}', '\u{00F6}', '\u{00F7}',
    '\u{00F8}', '\u{00F9}', '\u{00FA}', '\u{00FB}', '\u{00FC}', '\u{00FD}', '\u{00FE}', '\u{00FF}',
];

/// Adobe glyph names → Unicode for the glyphs common in Latin text.
/// Sorted by name for binary search. (Generated from Unicode character names.)
pub(crate) const GLYPH_NAMES: &[(&str, char)] = &[
    ("AE", '\u{00C6}'),
    ("Aacute", '\u{00C1}'),
    ("Abreve", '\u{0102}'),
    ("Acircumflex", '\u{00C2}'),
    ("Adieresis", '\u{00C4}'),
    ("Agrave", '\u{00C0}'),
    ("Amacron", '\u{0100}'),
    ("Aogonek", '\u{0104}'),
    ("Aring", '\u{00C5}'),
    ("Atilde", '\u{00C3}'),
    ("Cacute", '\u{0106}'),
    ("Ccaron", '\u{010C}'),
    ("Ccedilla", '\u{00C7}'),
    ("Ccircumflex", '\u{0108}'),
    ("Cdotaccent", '\u{010A}'),
    ("Dcaron", '\u{010E}'),
    ("Delta", '\u{2206}'),
    ("Dslash", '\u{0110}'),
    ("Eacute", '\u{00C9}'),
    ("Ebreve", '\u{0114}'),
    ("Ecaron", '\u{011A}'),
    ("Ecircumflex", '\u{00CA}'),
    ("Edieresis", '\u{00CB}'),
    ("Edotaccent", '\u{0116}'),
    ("Egrave", '\u{00C8}'),
    ("Emacron", '\u{0112}'),
    ("Eogonek", '\u{0118}'),
    ("Eth", '\u{00D0}'),
    ("Euro", '\u{20AC}'),
    ("Gbreve", '\u{011E}'),
    ("Gcedilla", '\u{0122}'),
    ("Gcircumflex", '\u{011C}'),
    ("Gdotaccent", '\u{0120}'),
    ("Hcircumflex", '\u{0124}'),
    ("Hslash", '\u{0126}'),
    ("Iacute", '\u{00CD}'),
    ("Ibreve", '\u{012C}'),
    ("Icircumflex", '\u{00CE}'),
    ("Idieresis", '\u{00CF}'),
    ("Idotaccent", '\u{0130}'),
    ("Igrave", '\u{00CC}'),
    ("Imacron", '\u{012A}'),
    ("Iogonek", '\u{012E}'),
    ("Itilde", '\u{0128}'),
    ("Jcircumflex", '\u{0134}'),
    ("Kcedilla", '\u{0136}'),
    ("Lacute", '\u{0139}'),
    ("Lcaron", '\u{013D}'),
    ("Lcedilla", '\u{013B}'),
    ("Lslash", '\u{0141}'),
    ("Nacute", '\u{0143}'),
    ("Ncaron", '\u{0147}'),
    ("Ncedilla", '\u{0145}'),
    ("Ntilde", '\u{00D1}'),
    ("OE", '\u{0152}'),
    ("Oacute", '\u{00D3}'),
    ("Obreve", '\u{014E}'),
    ("Ocircumflex", '\u{00D4}'),
    ("Odieresis", '\u{00D6}'),
    ("Ograve", '\u{00D2}'),
    ("Ohungarumlaut", '\u{0150}'),
    ("Omacron", '\u{014C}'),
    ("Omega", '\u{03A9}'),
    ("Oslash", '\u{00D8}'),
    ("Otilde", '\u{00D5}'),
    ("Racute", '\u{0154}'),
    ("Rcaron", '\u{0158}'),
    ("Rcedilla", '\u{0156}'),
    ("Sacute", '\u{015A}'),
    ("Scaron", '\u{0160}'),
    ("Scedilla", '\u{015E}'),
    ("Scircumflex", '\u{015C}'),
    ("Tcaron", '\u{0164}'),
    ("Tcedilla", '\u{0162}'),
    ("Thorn", '\u{00DE}'),
    ("Tslash", '\u{0166}'),
    ("Uacute", '\u{00DA}'),
    ("Ubreve", '\u{016C}'),
    ("Ucircumflex", '\u{00DB}'),
    ("Udieresis", '\u{00DC}'),
    ("Ugrave", '\u{00D9}'),
    ("Uhungarumlaut", '\u{0170}'),
    ("Umacron", '\u{016A}'),
    ("Uogonek", '\u{0172}'),
    ("Uring", '\u{016E}'),
    ("Utilde", '\u{0168}'),
    ("Wcircumflex", '\u{0174}'),
    ("Yacute", '\u{00DD}'),
    ("Ycircumflex", '\u{0176}'),
    ("Ydieresis", '\u{0178}'),
    ("Zacute", '\u{0179}'),
    ("Zcaron", '\u{017D}'),
    ("Zdotaccent", '\u{017B}'),
    ("aacute", '\u{00E1}'),
    ("abreve", '\u{0103}'),
    ("acircumflex", '\u{00E2}'),
    ("acute", '\u{00B4}'),
    ("adieresis", '\u{00E4}'),
    ("ae", '\u{00E6}'),
    ("agrave", '\u{00E0}'),
    ("alpha", '\u{03B1}'),
    ("amacron", '\u{0101}'),
    ("ampersand", '\u{0026}'),
    ("aogonek", '\u{0105}'),
    ("approxequal", '\u{2248}'),
    ("aring", '\u{00E5}'),
    ("arrowboth", '\u{2194}'),
    ("arrowdown", '\u{2193}'),
    ("arrowleft", '\u{2190}'),
    ("arrowright", '\u{2192}'),
    ("arrowup", '\u{2191}'),
    ("asciicircum", '\u{005E}'),
    ("asciitilde", '\u{007E}'),
    ("asterisk", '\u{002A}'),
    ("at", '\u{0040}'),
    ("atilde", '\u{00E3}'),
    ("backslash", '\u{005C}'),
    ("bar", '\u{007C}'),
    ("beta", '\u{03B2}'),
    ("braceleft", '\u{007B}'),
    ("braceright", '\u{007D}'),
    ("bracketleft", '\u{005B}'),
    ("bracketright", '\u{005D}'),
    ("breve", '\u{02D8}'),
    ("brokenbar", '\u{00A6}'),
    ("bullet", '\u{2022}'),
    ("cacute", '\u{0107}'),
    ("caron", '\u{02C7}'),
    ("ccaron", '\u{010D}'),
    ("ccedilla", '\u{00E7}'),
    ("ccircumflex", '\u{0109}'),
    ("cdotaccent", '\u{010B}'),
    ("cedilla", '\u{00B8}'),
    ("cent", '\u{00A2}'),
    ("circumflex", '\u{02C6}'),
    ("colon", '\u{003A}'),
    ("comma", '\u{002C}'),
    ("copyright", '\u{00A9}'),
    ("currency", '\u{00A4}'),
    ("dagger", '\u{2020}'),
    ("daggerdbl", '\u{2021}'),
    ("dcaron", '\u{010F}'),
    ("degree", '\u{00B0}'),
    ("delta", '\u{03B4}'),
    ("dieresis", '\u{00A8}'),
    ("divide", '\u{00F7}'),
    ("dollar", '\u{0024}'),
    ("dotaccent", '\u{02D9}'),
    ("dotlessi", '\u{0131}'),
    ("dotlessj", '\u{0237}'),
    ("dslash", '\u{0111}'),
    ("eacute", '\u{00E9}'),
    ("ebreve", '\u{0115}'),
    ("ecaron", '\u{011B}'),
    ("ecircumflex", '\u{00EA}'),
    ("edieresis", '\u{00EB}'),
    ("edotaccent", '\u{0117}'),
    ("egrave", '\u{00E8}'),
    ("eight", '\u{0038}'),
    ("ellipsis", '\u{2026}'),
    ("emacron", '\u{0113}'),
    ("emdash", '\u{2014}'),
    ("endash", '\u{2013}'),
    ("eogonek", '\u{0119}'),
    ("epsilon", '\u{03B5}'),
    ("equal", '\u{003D}'),
    ("eth", '\u{00F0}'),
    ("exclam", '\u{0021}'),
    ("exclamdown", '\u{00A1}'),
    ("ff", '\u{FB00}'),
    ("ffi", '\u{FB03}'),
    ("ffl", '\u{FB04}'),
    ("fi", '\u{FB01}'),
    ("five", '\u{0035}'),
    ("fl", '\u{FB02}'),
    ("florin", '\u{0192}'),
    ("four", '\u{0034}'),
    ("fraction", '\u{2044}'),
    ("gamma", '\u{03B3}'),
    ("gbreve", '\u{011F}'),
    ("gcedilla", '\u{0123}'),
    ("gcircumflex", '\u{011D}'),
    ("gdotaccent", '\u{0121}'),
    ("germandbls", '\u{00DF}'),
    ("grave", '\u{0060}'),
    ("greater", '\u{003E}'),
    ("greaterequal", '\u{2265}'),
    ("guillemotleft", '\u{00AB}'),
    ("guillemotright", '\u{00BB}'),
    ("guilsinglleft", '\u{2039}'),
    ("guilsinglright", '\u{203A}'),
    ("hcircumflex", '\u{0125}'),
    ("hslash", '\u{0127}'),
    ("hungarumlaut", '\u{02DD}'),
    ("hyphen", '\u{002D}'),
    ("iacute", '\u{00ED}'),
    ("ibreve", '\u{012D}'),
    ("icircumflex", '\u{00EE}'),
    ("idieresis", '\u{00EF}'),
    ("igrave", '\u{00EC}'),
    ("imacron", '\u{012B}'),
    ("infinity", '\u{221E}'),
    ("integral", '\u{222B}'),
    ("iogonek", '\u{012F}'),
    ("itilde", '\u{0129}'),
    ("jcircumflex", '\u{0135}'),
    ("kcedilla", '\u{0137}'),
    ("lacute", '\u{013A}'),
    ("lambda", '\u{03BB}'),
    ("lcaron", '\u{013E}'),
    ("lcedilla", '\u{013C}'),
    ("less", '\u{003C}'),
    ("lessequal", '\u{2264}'),
    ("logicalnot", '\u{00AC}'),
    ("lozenge", '\u{25CA}'),
    ("lslash", '\u{0142}'),
    ("macron", '\u{00AF}'),
    ("minus", '\u{2212}'),
    ("minute", '\u{2032}'),
    ("mu", '\u{00B5}'),
    ("multiply", '\u{00D7}'),
    ("nacute", '\u{0144}'),
    ("nbspace", '\u{00A0}'),
    ("ncaron", '\u{0148}'),
    ("ncedilla", '\u{0146}'),
    ("nine", '\u{0039}'),
    ("notequal", '\u{2260}'),
    ("ntilde", '\u{00F1}'),
    ("numbersign", '\u{0023}'),
    ("oacute", '\u{00F3}'),
    ("obreve", '\u{014F}'),
    ("ocircumflex", '\u{00F4}'),
    ("odieresis", '\u{00F6}'),
    ("oe", '\u{0153}'),
    ("ogonek", '\u{02DB}'),
    ("ograve", '\u{00F2}'),
    ("ohungarumlaut", '\u{0151}'),
    ("omacron", '\u{014D}'),
    ("omega", '\u{03C9}'),
    ("one", '\u{0031}'),
    ("onehalf", '\u{00BD}'),
    ("onequarter", '\u{00BC}'),
    ("onesuperior", '\u{00B9}'),
    ("ordfeminine", '\u{00AA}'),
    ("ordmasculine", '\u{00BA}'),
    ("oslash", '\u{00F8}'),
    ("otilde", '\u{00F5}'),
    ("paragraph", '\u{00B6}'),
    ("parenleft", '\u{0028}'),
    ("parenright", '\u{0029}'),
    ("partialdiff", '\u{2202}'),
    ("percent", '\u{0025}'),
    ("period", '\u{002E}'),
    ("periodcentered", '\u{00B7}'),
    ("perthousand", '\u{2030}'),
    ("phi", '\u{03C6}'),
    ("pi", '\u{03C0}'),
    ("plus", '\u{002B}'),
    ("plusminus", '\u{00B1}'),
    ("product", '\u{220F}'),
    ("question", '\u{003F}'),
    ("questiondown", '\u{00BF}'),
    ("quotedbl", '\u{0022}'),
    ("quotedblbase", '\u{201E}'),
    ("quotedblleft", '\u{201C}'),
    ("quotedblright", '\u{201D}'),
    ("quoteleft", '\u{2018}'),
    ("quotereversed", '\u{201B}'),
    ("quoteright", '\u{2019}'),
    ("quotesinglbase", '\u{201A}'),
    ("quotesingle", '\u{0027}'),
    ("racute", '\u{0155}'),
    ("radical", '\u{221A}'),
    ("rcaron", '\u{0159}'),
    ("rcedilla", '\u{0157}'),
    ("registered", '\u{00AE}'),
    ("ring", '\u{02DA}'),
    ("sacute", '\u{015B}'),
    ("scaron", '\u{0161}'),
    ("scedilla", '\u{015F}'),
    ("scircumflex", '\u{015D}'),
    ("second", '\u{2033}'),
    ("section", '\u{00A7}'),
    ("semicolon", '\u{003B}'),
    ("seven", '\u{0037}'),
    ("sfthyphen", '\u{00AD}'),
    ("sigma", '\u{03C3}'),
    ("six", '\u{0036}'),
    ("slash", '\u{002F}'),
    ("softhyphen", '\u{00AD}'),
    ("space", '\u{0020}'),
    ("sterling", '\u{00A3}'),
    ("summation", '\u{2211}'),
    ("tau", '\u{03C4}'),
    ("tcaron", '\u{0165}'),
    ("tcedilla", '\u{0163}'),
    ("theta", '\u{03B8}'),
    ("thorn", '\u{00FE}'),
    ("three", '\u{0033}'),
    ("threequarters", '\u{00BE}'),
    ("threesuperior", '\u{00B3}'),
    ("tilde", '\u{02DC}'),
    ("trademark", '\u{2122}'),
    ("tslash", '\u{0167}'),
    ("two", '\u{0032}'),
    ("twosuperior", '\u{00B2}'),
    ("uacute", '\u{00FA}'),
    ("ubreve", '\u{016D}'),
    ("ucircumflex", '\u{00FB}'),
    ("udieresis", '\u{00FC}'),
    ("ugrave", '\u{00F9}'),
    ("uhungarumlaut", '\u{0171}'),
    ("umacron", '\u{016B}'),
    ("underscore", '\u{005F}'),
    ("uogonek", '\u{0173}'),
    ("uring", '\u{016F}'),
    ("utilde", '\u{0169}'),
    ("wcircumflex", '\u{0175}'),
    ("yacute", '\u{00FD}'),
    ("ycircumflex", '\u{0177}'),
    ("ydieresis", '\u{00FF}'),
    ("yen", '\u{00A5}'),
    ("zacute", '\u{017A}'),
    ("zcaron", '\u{017E}'),
    ("zdotaccent", '\u{017C}'),
    ("zero", '\u{0030}'),
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_strings() {
        assert_eq!(decode_text_string(b"Hello"), "Hello");
        assert_eq!(decode_text_string(&[0xFE, 0xFF, 0x00, 0x48, 0x00, 0xE9]), "Hé");
        assert_eq!(decode_text_string(&[0x84, 0x93]), "—ﬁ");
    }

    #[test]
    fn glyph_names() {
        assert_eq!(glyph_name_to_unicode("eacute").as_deref(), Some("é"));
        assert_eq!(glyph_name_to_unicode("A").as_deref(), Some("A"));
        assert_eq!(glyph_name_to_unicode("uni00410042").as_deref(), Some("AB"));
        assert_eq!(glyph_name_to_unicode("u1F600").as_deref(), Some("😀"));
        assert_eq!(glyph_name_to_unicode("f_i").as_deref(), Some("fi"));
        assert_eq!(glyph_name_to_unicode("a.sc").as_deref(), Some("a"));
        assert_eq!(glyph_name_to_unicode("zcaron").as_deref(), Some("ž"));
        assert_eq!(glyph_name_to_unicode("g123"), None);
    }

    #[test]
    fn glyph_table_is_sorted() {
        assert!(GLYPH_NAMES.windows(2).all(|w| w[0].0 < w[1].0));
    }

    #[test]
    fn base_encodings() {
        assert_eq!(BaseEncoding::WinAnsi.decode(0x93), Some('“'));
        assert_eq!(BaseEncoding::MacRoman.decode(0x8E), Some('é'));
        assert_eq!(BaseEncoding::Standard.decode(0xAE), Some('ﬁ'));
        assert_eq!(BaseEncoding::Standard.decode(0x80), None);
    }
}


