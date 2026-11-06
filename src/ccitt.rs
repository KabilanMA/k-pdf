//! CCITTFaxDecode (ISO 32000 §7.4.6): ITU-T T.4 (Group 3, 1-D and 2-D) and
//! T.6 (Group 4) bilevel image decompression. Used by scanned documents and
//! by bitmap (Type 3) fonts.

use std::collections::HashMap;
use std::sync::OnceLock;

/// Decoding parameters, from the stream's `/DecodeParms`.
#[derive(Debug, Clone)]
pub(crate) struct Params {
    /// < 0: Group 4; 0: Group 3 1-D; > 0: Group 3 mixed 1-D/2-D.
    pub k: i64,
    pub columns: usize,
    /// 0 when unknown: decode until the data ends.
    pub rows: usize,
    pub black_is_1: bool,
    pub byte_align: bool,
}

impl Default for Params {
    fn default() -> Self {
        Params { k: 0, columns: 1728, rows: 0, black_is_1: false, byte_align: false }
    }
}

/// Decodes to packed 1-bit rows (MSB first). As the PDF spec requires,
/// black pixels are 0 bits unless `black_is_1` is set.
pub(crate) fn decode(data: &[u8], p: &Params) -> Vec<u8> {
    let columns = p.columns.clamp(1, 1 << 16);
    let row_bytes = columns.div_ceil(8);
    let mut out = Vec::new();
    let mut bits = BitReader { data, pos: 0 };
    // Changing elements of the reference (previous) line; initially an
    // imaginary all-white line.
    let mut reference: Vec<usize> = Vec::new();
    let mut row = 0;

    while p.rows == 0 || row < p.rows {
        if p.byte_align {
            bits.align();
        }
        let two_d = if p.k < 0 {
            true
        } else {
            // Group 3: optional EOL before each line, then (K > 0) a tag bit.
            if bits.skip_eol().is_none() && bits.at_end() {
                break;
            }
            if p.k > 0 {
                match bits.bit() {
                    Some(tag) => tag == 0,
                    None => break,
                }
            } else {
                false
            }
        };
        let line = if two_d { decode_2d(&mut bits, &reference, columns) } else { decode_1d(&mut bits, columns) };
        let Some(line) = line else { break };
        push_row(&mut out, &line, columns, row_bytes, p.black_is_1);
        reference = line;
        row += 1;
        if bits.at_end() {
            break;
        }
    }
    out
}

/// Writes one row given its changing elements (white first).
fn push_row(out: &mut Vec<u8>, changes: &[usize], columns: usize, row_bytes: usize, black_is_1: bool) {
    let start = out.len();
    // Start with the white value everywhere, then paint black runs.
    out.resize(start + row_bytes, if black_is_1 { 0x00 } else { 0xFF });
    let row = &mut out[start..];
    for pair in changes.chunks(2) {
        let (from, to) = (pair[0].min(columns), pair.get(1).copied().unwrap_or(columns).min(columns));
        for x in from..to {
            let mask = 0x80 >> (x % 8);
            if black_is_1 {
                row[x / 8] |= mask;
            } else {
                row[x / 8] &= !mask;
            }
        }
    }
}

/// One-dimensional (Modified Huffman) line.
fn decode_1d(bits: &mut BitReader, columns: usize) -> Option<Vec<usize>> {
    let mut changes = Vec::new();
    let mut pos = 0;
    let mut black = false;
    while pos < columns {
        pos = (pos + bits.run(black)?).min(columns);
        changes.push(pos);
        black = !black;
    }
    Some(changes)
}

/// Two-dimensional (READ) line, coded relative to `reference`.
fn decode_2d(bits: &mut BitReader, reference: &[usize], columns: usize) -> Option<Vec<usize>> {
    let mut changes: Vec<usize> = Vec::new();
    let mut a0: isize = -1;
    let mut black = false;
    while a0 < columns as isize {
        let (b1, b2) = find_b1_b2(reference, a0, black, columns);
        match bits.mode()? {
            Mode::Pass => a0 = b2 as isize,
            Mode::Horizontal => {
                let start = a0.max(0) as usize;
                let a1 = (start + bits.run(black)?).min(columns);
                let a2 = (a1 + bits.run(!black)?).min(columns);
                changes.push(a1);
                changes.push(a2);
                a0 = a2 as isize;
            }
            Mode::Vertical(d) => {
                let a1 = (b1 as isize + d).clamp(0, columns as isize) as usize;
                // Keep changing elements in order even for bad data.
                let a1 = a1.max(changes.last().copied().unwrap_or(0));
                changes.push(a1);
                a0 = a1 as isize;
                black = !black;
            }
            Mode::Eol => return if changes.is_empty() { None } else { Some(changes) },
        }
    }
    Some(changes)
}

/// b1: first changing element on the reference line right of a0 with the
/// opposite color of a0; b2: the next one.
fn find_b1_b2(reference: &[usize], a0: isize, black: bool, columns: usize) -> (usize, usize) {
    // Even indices are white→black changes; for a white a0, b1 must be one.
    let mut i = if black { 1 } else { 0 };
    while i < reference.len() && (reference[i] as isize) <= a0 {
        i += 2;
    }
    let b1 = reference.get(i).copied().unwrap_or(columns);
    let b2 = reference.get(i + 1).copied().unwrap_or(columns);
    (b1, b2)
}

enum Mode {
    Pass,
    Horizontal,
    Vertical(isize),
    Eol,
}

struct BitReader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl BitReader<'_> {
    fn at_end(&self) -> bool {
        self.pos >= self.data.len() * 8
    }

    fn bit(&mut self) -> Option<u8> {
        let byte = *self.data.get(self.pos / 8)?;
        let b = (byte >> (7 - self.pos % 8)) & 1;
        self.pos += 1;
        Some(b)
    }

    fn align(&mut self) {
        self.pos = self.pos.div_ceil(8) * 8;
    }

    /// Skips fill bits and an EOL (`0000 0000 0001`) if one is next.
    fn skip_eol(&mut self) -> Option<()> {
        let mut p = self.pos;
        let mut zeros = 0;
        loop {
            let byte = *self.data.get(p / 8)?;
            let b = (byte >> (7 - p % 8)) & 1;
            p += 1;
            if b == 1 {
                if zeros >= 11 {
                    self.pos = p;
                    return Some(());
                }
                return None;
            }
            zeros += 1;
        }
    }

    fn mode(&mut self) -> Option<Mode> {
        let mut code = 0u32;
        for len in 1..=12 {
            code = code << 1 | self.bit()? as u32;
            let mode = match (len, code) {
                (1, 0b1) => Mode::Vertical(0),
                (3, 0b011) => Mode::Vertical(1),
                (3, 0b010) => Mode::Vertical(-1),
                (3, 0b001) => Mode::Horizontal,
                (4, 0b0001) => Mode::Pass,
                (6, 0b000011) => Mode::Vertical(2),
                (6, 0b000010) => Mode::Vertical(-2),
                (7, 0b0000011) => Mode::Vertical(3),
                (7, 0b0000010) => Mode::Vertical(-3),
                (12, 0b000000000001) => Mode::Eol,
                _ => continue,
            };
            return Some(mode);
        }
        None // extensions / corrupt data
    }

    /// A complete run length: make-up codes followed by a terminating code.
    fn run(&mut self, black: bool) -> Option<usize> {
        let table = if black { &tables().black } else { &tables().white };
        let mut total = 0;
        loop {
            let mut code = 0u32;
            let mut found = None;
            for len in 1..=13u8 {
                code = code << 1 | self.bit()? as u32;
                if let Some(&run) = table.get(&(len, code)) {
                    found = Some(run);
                    break;
                }
            }
            let run = found? as usize;
            total += run;
            if run < 64 {
                return Some(total);
            }
            // Stop runaway make-up sequences on corrupt data.
            if total > 1 << 20 {
                return None;
            }
        }
    }
}

struct Tables {
    white: HashMap<(u8, u32), u16>,
    black: HashMap<(u8, u32), u16>,
}

fn tables() -> &'static Tables {
    static TABLES: OnceLock<Tables> = OnceLock::new();
    TABLES.get_or_init(|| {
        let build = |codes: &[(&str, u16)]| -> HashMap<(u8, u32), u16> {
            codes
                .iter()
                .chain(EXTENDED_MAKEUP)
                .map(|(bits, run)| ((bits.len() as u8, u32::from_str_radix(bits, 2).unwrap()), *run))
                .collect()
        };
        Tables { white: build(WHITE), black: build(BLACK) }
    })
}

/// ITU-T T.4 white run-length codes (terminating 0–63, make-up 64–1728).
const WHITE: &[(&str, u16)] = &[
    ("00110101", 0), ("000111", 1), ("0111", 2), ("1000", 3), ("1011", 4), ("1100", 5), ("1110", 6), ("1111", 7),
    ("10011", 8), ("10100", 9), ("00111", 10), ("01000", 11), ("001000", 12), ("000011", 13), ("110100", 14),
    ("110101", 15), ("101010", 16), ("101011", 17), ("0100111", 18), ("0001100", 19), ("0001000", 20),
    ("0010111", 21), ("0000011", 22), ("0000100", 23), ("0101000", 24), ("0101011", 25), ("0010011", 26),
    ("0100100", 27), ("0011000", 28), ("00000010", 29), ("00000011", 30), ("00011010", 31), ("00011011", 32),
    ("00010010", 33), ("00010011", 34), ("00010100", 35), ("00010101", 36), ("00010110", 37), ("00010111", 38),
    ("00101000", 39), ("00101001", 40), ("00101010", 41), ("00101011", 42), ("00101100", 43), ("00101101", 44),
    ("00000100", 45), ("00000101", 46), ("00001010", 47), ("00001011", 48), ("01010010", 49), ("01010011", 50),
    ("01010100", 51), ("01010101", 52), ("00100100", 53), ("00100101", 54), ("01011000", 55), ("01011001", 56),
    ("01011010", 57), ("01011011", 58), ("01001010", 59), ("01001011", 60), ("00110010", 61), ("00110011", 62),
    ("00110100", 63),
    ("11011", 64), ("10010", 128), ("010111", 192), ("0110111", 256), ("00110110", 320), ("00110111", 384),
    ("01100100", 448), ("01100101", 512), ("01101000", 576), ("01100111", 640), ("011001100", 704),
    ("011001101", 768), ("011010010", 832), ("011010011", 896), ("011010100", 960), ("011010101", 1024),
    ("011010110", 1088), ("011010111", 1152), ("011011000", 1216), ("011011001", 1280), ("011011010", 1344),
    ("011011011", 1408), ("010011000", 1472), ("010011001", 1536), ("010011010", 1600), ("011000", 1664),
    ("010011011", 1728),
];

/// ITU-T T.4 black run-length codes (terminating 0–63, make-up 64–1728).
const BLACK: &[(&str, u16)] = &[
    ("0000110111", 0), ("010", 1), ("11", 2), ("10", 3), ("011", 4), ("0011", 5), ("0010", 6), ("00011", 7),
    ("000101", 8), ("000100", 9), ("0000100", 10), ("0000101", 11), ("0000111", 12), ("00000100", 13),
    ("00000111", 14), ("000011000", 15), ("0000010111", 16), ("0000011000", 17), ("0000001000", 18),
    ("00001100111", 19), ("00001101000", 20), ("00001101100", 21), ("00000110111", 22), ("00000101000", 23),
    ("00000010111", 24), ("00000011000", 25), ("000011001010", 26), ("000011001011", 27), ("000011001100", 28),
    ("000011001101", 29), ("000001101000", 30), ("000001101001", 31), ("000001101010", 32), ("000001101011", 33),
    ("000011010010", 34), ("000011010011", 35), ("000011010100", 36), ("000011010101", 37), ("000011010110", 38),
    ("000011010111", 39), ("000001101100", 40), ("000001101101", 41), ("000011011010", 42), ("000011011011", 43),
    ("000001010100", 44), ("000001010101", 45), ("000001010110", 46), ("000001010111", 47), ("000001100100", 48),
    ("000001100101", 49), ("000001010010", 50), ("000001010011", 51), ("000000100100", 52), ("000000110111", 53),
    ("000000111000", 54), ("000000100111", 55), ("000000101000", 56), ("000001011000", 57), ("000001011001", 58),
    ("000000101011", 59), ("000000101100", 60), ("000001011010", 61), ("000001100110", 62), ("000001100111", 63),
    ("0000001111", 64), ("000011001000", 128), ("000011001001", 192), ("000001011011", 256),
    ("000000110011", 320), ("000000110100", 384), ("000000110101", 448), ("0000001101100", 512),
    ("0000001101101", 576), ("0000001001010", 640), ("0000001001011", 704), ("0000001001100", 768),
    ("0000001001101", 832), ("0000001110010", 896), ("0000001110011", 960), ("0000001110100", 1024),
    ("0000001110101", 1088), ("0000001110110", 1152), ("0000001110111", 1216), ("0000001010010", 1280),
    ("0000001010011", 1344), ("0000001010100", 1408), ("0000001010101", 1472), ("0000001011010", 1536),
    ("0000001011011", 1600), ("0000001100100", 1664), ("0000001100101", 1728),
];

/// Make-up codes shared by both colors (1792–2560).
const EXTENDED_MAKEUP: &[(&str, u16)] = &[
    ("00000001000", 1792), ("00000001100", 1856), ("00000001101", 1920), ("000000010010", 1984),
    ("000000010011", 2048), ("000000010100", 2112), ("000000010101", 2176), ("000000010110", 2240),
    ("000000010111", 2304), ("000000011100", 2368), ("000000011101", 2432), ("000000011110", 2496),
    ("000000011111", 2560),
];

#[cfg(test)]
mod tests {
    use super::*;

    /// Every code table must be prefix-free, or decoding is ambiguous.
    #[test]
    fn code_tables_are_prefix_free() {
        for table in [WHITE, BLACK] {
            let all: Vec<&str> = table.iter().chain(EXTENDED_MAKEUP).map(|(c, _)| *c).collect();
            for (i, a) in all.iter().enumerate() {
                for (j, b) in all.iter().enumerate() {
                    assert!(i == j || !b.starts_with(a), "{a} is a prefix of {b}");
                }
            }
            // Terminating codes 0..63 and make-up codes are all present.
            assert_eq!(table.len(), 64 + 27);
        }
    }

    fn bits(s: &str) -> Vec<u8> {
        let s: String = s.chars().filter(|c| !c.is_whitespace()).collect();
        let mut out = vec![0u8; s.len().div_ceil(8)];
        for (i, c) in s.chars().enumerate() {
            if c == '1' {
                out[i / 8] |= 0x80 >> (i % 8);
            }
        }
        out
    }

    #[test]
    fn group3_1d_line() {
        // 8 columns: 2 white, 3 black, 3 white.
        let data = bits("0111 10 1000");
        let p = Params { k: 0, columns: 8, rows: 1, ..Params::default() };
        assert_eq!(decode(&data, &p), vec![0b1100_0111]);
    }

    #[test]
    fn group4_vertical_and_horizontal() {
        // Line 1 (vs. white reference): H, white 2, black 3, then V0 for b1=b2=8.
        // Line 2: V0 ×3 (each change lines up with the line above).
        let data = bits("001 0111 10 1  1 1 1");
        let p = Params { k: -1, columns: 8, rows: 2, ..Params::default() };
        assert_eq!(decode(&data, &p), vec![0b1100_0111, 0b1100_0111]);
    }
}
