//! Stream filters (ISO 32000 §7.4).
//!
//! Supported for decoding: FlateDecode, LZWDecode, ASCIIHexDecode,
//! ASCII85Decode, RunLengthDecode (plus PNG/TIFF predictors),
//! CCITTFaxDecode, and BrotliDecode (the PDF Association's PDF 2.0
//! extension, 2026). The image codecs DCT, JPX and JBIG2 are left encoded: their
//! output *is* an image file format, and decoding stops at such a filter.

use std::io::Read;

use crate::error::{Error, Result};
use crate::object::{Dictionary, Object};

/// Filters whose output is an image format we deliberately do not decode.
const IMAGE_FILTERS: &[&[u8]] = &[b"DCTDecode", b"DCT", b"JPXDecode", b"JBIG2Decode"];

/// Result of decoding a stream.
pub struct Decoded {
    pub data: Vec<u8>,
    /// If decoding stopped at an image filter, its name (e.g. `DCTDecode`).
    pub remaining_filter: Option<Vec<u8>>,
}

/// Applies every filter listed in `dict` to `raw`.
///
/// `dict` must already have `/Filter` and `/DecodeParms` resolved to direct
/// objects (the document layer takes care of that).
pub fn decode(raw: &[u8], dict: &Dictionary) -> Result<Decoded> {
    decode_with(raw, dict, false)
}

/// Like [`decode`], but damaged data is an error instead of being decoded
/// as far as possible: for rewriting streams, where a partly decoded one
/// would lose data.
pub fn decode_strict(raw: &[u8], dict: &Dictionary) -> Result<Decoded> {
    decode_with(raw, dict, true)
}

fn decode_with(raw: &[u8], dict: &Dictionary, strict: bool) -> Result<Decoded> {
    let filters: Vec<&[u8]> = match dict.get(b"Filter").or_else(|| dict.get(b"F")) {
        None => Vec::new(),
        Some(Object::Name(n)) => vec![n.as_slice()],
        Some(Object::Array(a)) => a.iter().filter_map(Object::as_name).collect(),
        Some(o) => return Err(Error::Filter(format!("invalid /Filter of type {}", o.type_name()))),
    };
    let parms = dict.get(b"DecodeParms").or_else(|| dict.get(b"DP"));
    let parm_at = |i: usize| -> Option<&Dictionary> {
        match parms? {
            Object::Array(a) => a.get(i).and_then(Object::as_dict),
            o => o.as_dict().filter(|_| i == 0),
        }
    };

    let mut data: Option<Vec<u8>> = None; // avoid copying when there are no filters
    for (i, &name) in filters.iter().enumerate() {
        if IMAGE_FILTERS.contains(&name) {
            return Ok(Decoded {
                data: data.unwrap_or_else(|| raw.to_vec()),
                remaining_filter: Some(name.to_vec()),
            });
        }
        let input = data.as_deref().unwrap_or(raw);
        let parm = parm_at(i);
        let out = match name {
            b"FlateDecode" | b"Fl" => {
                let data = if strict { flate_strict(input)? } else { flate(input)? };
                if strict {
                    check_predictor_rows(&data, parm)?;
                }
                apply_predictor(data, parm)?
            }
            b"BrotliDecode" => brotli(input, strict)?,
            b"LZWDecode" | b"LZW" => {
                let early = parm.and_then(|p| p.get(b"EarlyChange")).and_then(Object::as_i64) != Some(0);
                apply_predictor(lzw(input, early), parm)?
            }
            b"ASCIIHexDecode" | b"AHx" => ascii_hex(input),
            b"ASCII85Decode" | b"A85" => ascii85(input)?,
            b"RunLengthDecode" | b"RL" => run_length(input),
            b"CCITTFaxDecode" | b"CCF" if strict => {
                return Err(Error::Filter("CCITT data is left as it is".into()));
            }
            b"CCITTFaxDecode" | b"CCF" => crate::ccitt::decode(input, &ccitt_params(parm)),
            b"Crypt" => input.to_vec(), // only the Identity crypt filter is meaningful unencrypted
            other => {
                return Err(Error::Filter(format!("unsupported filter /{}", String::from_utf8_lossy(other))));
            }
        };
        data = Some(out);
    }
    Ok(Decoded { data: data.unwrap_or_else(|| raw.to_vec()), remaining_filter: None })
}

fn ccitt_params(parm: Option<&Dictionary>) -> crate::ccitt::Params {
    let mut p = crate::ccitt::Params::default();
    let Some(d) = parm else { return p };
    let int = |k: &[u8]| d.get(k).and_then(Object::as_i64);
    let flag = |k: &[u8]| d.get(k).and_then(Object::as_bool).unwrap_or(false);
    p.k = int(b"K").unwrap_or(0);
    p.columns = int(b"Columns").unwrap_or(1728).max(1) as usize;
    p.rows = int(b"Rows").unwrap_or(0).max(0) as usize;
    p.black_is_1 = flag(b"BlackIs1");
    p.byte_align = flag(b"EncodedByteAlign");
    p
}

/// zlib/deflate decoding. Many broken files have truncated or corrupt
/// streams; like most viewers we keep whatever decoded before the error.
fn flate(input: &[u8]) -> Result<Vec<u8>> {
    let mut out = Vec::with_capacity(input.len().saturating_mul(3));
    let zlib_result = flate2::read::ZlibDecoder::new(input).read_to_end(&mut out);
    if zlib_result.is_ok() || !out.is_empty() {
        return Ok(out);
    }
    // Some writers omit the 2-byte zlib header: try raw deflate.
    out.clear();
    let raw_result = flate2::read::DeflateDecoder::new(input).read_to_end(&mut out);
    if raw_result.is_ok() || !out.is_empty() {
        return Ok(out);
    }
    Err(Error::Filter(format!("FlateDecode failed: {}", zlib_result.unwrap_err())))
}

/// Flate data that must decode completely and without errors.
fn flate_strict(input: &[u8]) -> Result<Vec<u8>> {
    let mut out = Vec::with_capacity(input.len().saturating_mul(3));
    flate2::read::ZlibDecoder::new(input)
        .read_to_end(&mut out)
        .map_err(|e| Error::Filter(format!("FlateDecode failed: {e}")))?;
    Ok(out)
}

/// In strict mode, PNG-predicted data must be whole rows with valid row
/// filter types.
fn check_predictor_rows(data: &[u8], parm: Option<&Dictionary>) -> Result<()> {
    let Some(parm) = parm else { return Ok(()) };
    let get = |k: &[u8], default: i64| parm.get(k).and_then(Object::as_i64).unwrap_or(default);
    if get(b"Predictor", 1) < 10 {
        return Ok(());
    }
    let row_len = (get(b"Colors", 1).clamp(1, 32) * get(b"BitsPerComponent", 8).clamp(1, 16) * get(b"Columns", 1).max(1)) as usize;
    let row = row_len.div_ceil(8) + 1;
    if !data.len().is_multiple_of(row) || data.chunks(row).any(|r| r[0] > 4) {
        return Err(Error::Filter("predicted data doesn't fill whole rows".into()));
    }
    Ok(())
}

/// Brotli (RFC 7932), with no extra headers.
fn brotli(input: &[u8], strict: bool) -> Result<Vec<u8>> {
    let mut out = Vec::with_capacity(input.len().saturating_mul(4));
    let result = brotli_decompressor::Decompressor::new(input, 64 * 1024).read_to_end(&mut out);
    match result {
        Ok(_) => Ok(out),
        Err(_) if !strict && !out.is_empty() => Ok(out),
        Err(e) => Err(Error::Filter(format!("BrotliDecode failed: {e}"))),
    }
}

/// LZW decoding as used by PDF (and TIFF): MSB-first codes, 9–12 bits,
/// 256 = clear table, 257 = end of data.
pub(crate) fn lzw(input: &[u8], early_change: bool) -> Vec<u8> {
    const CLEAR: usize = 256;
    const EOD: usize = 257;
    // Each table entry is (prefix code, last byte); strings are
    // rebuilt by walking prefixes, so the table stays small (4096 entries).
    let mut table: Vec<(u16, u8)> = Vec::with_capacity(4096);
    let reset = |t: &mut Vec<(u16, u8)>| {
        t.clear();
        for b in 0..=255u16 {
            t.push((u16::MAX, b as u8));
        }
        t.push((u16::MAX, 0)); // 256 clear
        t.push((u16::MAX, 0)); // 257 eod
    };
    reset(&mut table);

    let mut out = Vec::with_capacity(input.len() * 2);
    let mut code_len = 9u32;
    let mut bit_buf: u32 = 0;
    let mut bit_count = 0u32;
    let mut prev: Option<usize> = None;
    let mut bytes = input.iter();
    let mut scratch: Vec<u8> = Vec::new();

    let write_entry = |code: usize, table: &[(u16, u8)], out: &mut Vec<u8>, scratch: &mut Vec<u8>| -> u8 {
        scratch.clear();
        let mut c = code;
        loop {
            let (prefix, byte) = table[c];
            scratch.push(byte);
            if prefix == u16::MAX {
                break;
            }
            c = prefix as usize;
        }
        out.extend(scratch.iter().rev());
        *scratch.last().unwrap() // first byte of the string
    };

    loop {
        while bit_count < code_len {
            match bytes.next() {
                Some(&b) => {
                    bit_buf = (bit_buf << 8) | b as u32;
                    bit_count += 8;
                }
                None => return out,
            }
        }
        let code = ((bit_buf >> (bit_count - code_len)) & ((1 << code_len) - 1)) as usize;
        bit_count -= code_len;

        match code {
            CLEAR => {
                reset(&mut table);
                code_len = 9;
                prev = None;
                continue;
            }
            EOD => return out,
            _ => {}
        }

        match prev {
            None => {
                if code >= table.len() {
                    return out; // corrupt
                }
                write_entry(code, &table, &mut out, &mut scratch);
            }
            Some(p) => {
                let first = if code < table.len() {
                    write_entry(code, &table, &mut out, &mut scratch)
                } else if code == table.len() {
                    // KwKwK case: string is prev + first byte of prev.
                    let f = write_entry(p, &table, &mut out, &mut scratch);
                    out.push(f);
                    f
                } else {
                    return out; // corrupt
                };
                if table.len() < 4096 {
                    table.push((p as u16, first));
                }
            }
        }
        prev = Some(code);

        let threshold = table.len() + usize::from(early_change);
        code_len = match threshold {
            0..=511 => 9,
            512..=1023 => 10,
            1024..=2047 => 11,
            _ => 12,
        };
    }
}

fn ascii_hex(input: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(input.len() / 2);
    let mut pending: Option<u8> = None;
    for &c in input {
        if c == b'>' {
            break;
        }
        if let Some(v) = crate::parser::hex_value(c) {
            match pending.take() {
                Some(hi) => out.push(hi << 4 | v),
                None => pending = Some(v),
            }
        }
    }
    if let Some(hi) = pending {
        out.push(hi << 4);
    }
    out
}

fn ascii85(input: &[u8]) -> Result<Vec<u8>> {
    let mut out = Vec::with_capacity(input.len() * 4 / 5);
    let mut group = [0u8; 5];
    let mut n = 0;
    let mut data = input;
    if data.starts_with(b"<~") {
        data = &data[2..];
    }
    for &c in data {
        match c {
            b'~' => break,
            b'z' if n == 0 => out.extend_from_slice(&[0, 0, 0, 0]),
            b'!'..=b'u' => {
                group[n] = c - b'!';
                n += 1;
                if n == 5 {
                    let v = group.iter().fold(0u64, |acc, &d| acc * 85 + d as u64);
                    if v > u32::MAX as u64 {
                        return Err(Error::Filter("ASCII85Decode: group out of range".into()));
                    }
                    out.extend_from_slice(&(v as u32).to_be_bytes());
                    n = 0;
                }
            }
            c if crate::object::is_whitespace(c) => {}
            _ => return Err(Error::Filter(format!("ASCII85Decode: invalid byte 0x{c:02X}"))),
        }
    }
    if n > 1 {
        // Partial final group: pad with 'u' (84) and keep n-1 bytes.
        for d in group.iter_mut().skip(n) {
            *d = 84;
        }
        let v = group.iter().fold(0u64, |acc, &d| acc * 85 + d as u64).min(u32::MAX as u64);
        out.extend_from_slice(&(v as u32).to_be_bytes()[..n - 1]);
    }
    Ok(out)
}

fn run_length(input: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(input.len() * 2);
    let mut i = 0;
    while i < input.len() {
        let len = input[i];
        i += 1;
        match len {
            128 => break, // EOD
            0..=127 => {
                let n = len as usize + 1;
                let end = (i + n).min(input.len());
                out.extend_from_slice(&input[i..end]);
                i = end;
            }
            _ => {
                if let Some(&b) = input.get(i) {
                    out.extend(std::iter::repeat_n(b, 257 - len as usize));
                }
                i += 1;
            }
        }
    }
    out
}

/// Reverses the TIFF (2) or PNG (10–15) predictor applied before compression.
fn apply_predictor(data: Vec<u8>, parm: Option<&Dictionary>) -> Result<Vec<u8>> {
    let Some(parm) = parm else { return Ok(data) };
    let get = |k: &[u8], default: i64| parm.get(k).and_then(Object::as_i64).unwrap_or(default);
    let predictor = get(b"Predictor", 1);
    if predictor <= 1 {
        return Ok(data);
    }
    let colors = get(b"Colors", 1).clamp(1, 32) as usize;
    let bpc = get(b"BitsPerComponent", 8).clamp(1, 16) as usize;
    let columns = get(b"Columns", 1).max(1) as usize;
    let bpp = (colors * bpc).div_ceil(8); // bytes per pixel, at least 1
    let row_len = (colors * bpc * columns).div_ceil(8);

    if predictor == 2 {
        return Ok(tiff_predictor(data, row_len, bpp, bpc, colors));
    }

    // PNG predictors: every row is prefixed by a filter-type byte.
    let mut out = Vec::with_capacity(data.len());
    let mut prev_row = vec![0u8; row_len];
    for chunk in data.chunks(row_len + 1) {
        let filter = chunk[0];
        let mut row = chunk[1..].to_vec();
        row.resize(row_len, 0);
        for i in 0..row_len {
            let left = if i >= bpp { row[i - bpp] } else { 0 };
            let up = prev_row[i];
            let up_left = if i >= bpp { prev_row[i - bpp] } else { 0 };
            row[i] = match filter {
                0 => row[i],
                1 => row[i].wrapping_add(left),
                2 => row[i].wrapping_add(up),
                3 => row[i].wrapping_add(((left as u16 + up as u16) / 2) as u8),
                4 => row[i].wrapping_add(paeth(left, up, up_left)),
                f => return Err(Error::Filter(format!("unknown PNG predictor type {f}"))),
            };
        }
        out.extend_from_slice(&row);
        prev_row = row;
    }
    Ok(out)
}

fn paeth(a: u8, b: u8, c: u8) -> u8 {
    let p = a as i16 + b as i16 - c as i16;
    let (pa, pb, pc) = ((p - a as i16).abs(), (p - b as i16).abs(), (p - c as i16).abs());
    if pa <= pb && pa <= pc {
        a
    } else if pb <= pc {
        b
    } else {
        c
    }
}

fn tiff_predictor(mut data: Vec<u8>, row_len: usize, bpp: usize, bpc: usize, colors: usize) -> Vec<u8> {
    if bpc == 8 {
        for row in data.chunks_mut(row_len) {
            for i in bpp..row.len() {
                row[i] = row[i].wrapping_add(row[i - bpp]);
            }
        }
    } else if bpc == 16 {
        for row in data.chunks_mut(row_len) {
            let step = colors * 2;
            let mut i = step;
            while i + 1 < row.len() {
                let cur = u16::from_be_bytes([row[i], row[i + 1]]);
                let left = u16::from_be_bytes([row[i - step], row[i - step + 1]]);
                row[i..i + 2].copy_from_slice(&cur.wrapping_add(left).to_be_bytes());
                i += 2;
            }
        }
    }
    // Sub-byte TIFF prediction is extremely rare in PDFs; left as-is.
    data
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn dict_with(filter: &str) -> Dictionary {
        let mut d = Dictionary::new();
        d.set(b"Filter".to_vec(), Object::Name(filter.as_bytes().to_vec()));
        d
    }

    #[test]
    fn flate_roundtrip() {
        let mut enc = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
        enc.write_all(b"hello hello hello").unwrap();
        let raw = enc.finish().unwrap();
        assert_eq!(decode(&raw, &dict_with("FlateDecode")).unwrap().data, b"hello hello hello");
    }

    #[test]
    fn ascii_hex_and_85() {
        assert_eq!(ascii_hex(b"48 65 6c 6C 6f>"), b"Hello");
        assert_eq!(ascii_hex(b"414>"), b"A@");
        assert_eq!(ascii85(b"87cURD]i,\"Ebo7~>").unwrap(), b"Hello World");
        assert_eq!(ascii85(b"z~>").unwrap(), [0, 0, 0, 0]);
    }

    #[test]
    fn run_length_decode() {
        assert_eq!(run_length(&[2, b'a', b'b', b'c', 254, b'x', 128]), b"abcxxx");
    }

    #[test]
    fn lzw_spec_example() {
        // Example from ISO 32000-1 §7.4.4.2: "-----A---B" (early change = 1).
        let enc = [0x80, 0x0B, 0x60, 0x50, 0x22, 0x0C, 0x0C, 0x85, 0x01];
        assert_eq!(lzw(&enc, true), b"-----A---B");
    }

    #[test]
    fn png_up_predictor() {
        let mut p = Dictionary::new();
        p.set(b"Predictor".to_vec(), Object::Integer(12));
        p.set(b"Columns".to_vec(), Object::Integer(3));
        let data = vec![2, 1, 2, 3, 2, 1, 1, 1];
        assert_eq!(apply_predictor(data, Some(&p)).unwrap(), vec![1, 2, 3, 2, 3, 4]);
    }

    #[test]
    fn image_filter_stops_decoding() {
        let mut d = Dictionary::new();
        d.set(
            b"Filter".to_vec(),
            Object::Array(vec![Object::Name(b"ASCIIHexDecode".to_vec()), Object::Name(b"DCTDecode".to_vec())]),
        );
        let r = decode(b"FFD8>", &d).unwrap();
        assert_eq!(r.data, [0xFF, 0xD8]);
        assert_eq!(r.remaining_filter.as_deref(), Some(&b"DCTDecode"[..]));
    }
}
