//! Readers for the other common image formats: GIF, BMP, TIFF and WebP.
//!
//! Each gives an [`Image`] (gray or RGB, 8 bits). Transparent pixels are
//! put on white, as on paper. Only the first picture of an animated GIF or
//! WebP, or of a TIFF with several pages, is read.

use super::{Image, MAX_PIXELS};
use crate::error::{Error, Result};

fn damaged(format: &str, what: &str) -> Error {
    Error::Image(format!("the {format} image is damaged ({what})"))
}

fn check_size(width: u64, height: u64) -> Result<()> {
    if width == 0 || height == 0 {
        return Err(Error::Image("the image is empty".into()));
    }
    if width * height > MAX_PIXELS {
        return Err(Error::Image("the photo is too large".into()));
    }
    Ok(())
}

/// Puts a color with `alpha` (0–255) on white.
fn on_white(v: u8, alpha: u8) -> u8 {
    let a = alpha as u32;
    ((v as u32 * a + 255 * (255 - a) + 127) / 255) as u8
}

// ----------------------------------------------------------------------
// GIF (GIF89a specification)
// ----------------------------------------------------------------------

pub(super) fn decode_gif(data: &[u8]) -> Result<Image> {
    let bad = |what| damaged("GIF", what);
    let u16_at = |i: usize| data.get(i..i + 2).map(|b| u16::from_le_bytes([b[0], b[1]]) as usize);
    let width = u16_at(6).ok_or_else(|| bad("cut short"))?;
    let height = u16_at(8).ok_or_else(|| bad("cut short"))?;
    check_size(width as u64, height as u64)?;
    let flags = data[10];
    let mut pos = 13;
    let mut global: &[u8] = &[];
    if flags & 0x80 != 0 {
        let n = 3 << ((flags & 7) + 1);
        global = data.get(pos..pos + n).ok_or_else(|| bad("cut short"))?;
        pos += n;
    }
    let mut transparent = None;
    // The canvas starts white (the background shows through).
    let mut out = Image { width: width as u32, height: height as u32, channels: 3, data: vec![255; width * height * 3] };
    loop {
        match data.get(pos) {
            Some(0x21) => {
                // An extension: the graphic control one says which color
                // is transparent.
                let label = *data.get(pos + 1).ok_or_else(|| bad("cut short"))?;
                pos += 2;
                if label == 0xF9 && data.get(pos) == Some(&4) && data.len() > pos + 4 {
                    transparent = (data[pos + 1] & 1 != 0).then_some(data[pos + 4]);
                }
                pos = skip_blocks(data, pos);
            }
            Some(0x2C) => {
                let (left, top) = (u16_at(pos + 1).unwrap_or(0), u16_at(pos + 3).unwrap_or(0));
                let w = u16_at(pos + 5).ok_or_else(|| bad("cut short"))?;
                let h = u16_at(pos + 7).ok_or_else(|| bad("cut short"))?;
                let f = *data.get(pos + 9).ok_or_else(|| bad("cut short"))?;
                pos += 10;
                let mut colors = global;
                if f & 0x80 != 0 {
                    let n = 3 << ((f & 7) + 1);
                    colors = data.get(pos..pos + n).ok_or_else(|| bad("cut short"))?;
                    pos += n;
                }
                let min_code = *data.get(pos).ok_or_else(|| bad("cut short"))?;
                if !(1..=11).contains(&min_code) {
                    return Err(bad("bad code size"));
                }
                // The image data comes in blocks of up to 255 bytes.
                let mut packed = Vec::new();
                let mut i = pos + 1;
                while let Some(&n) = data.get(i) {
                    if n == 0 {
                        break;
                    }
                    packed.extend_from_slice(data.get(i + 1..i + 1 + n as usize).unwrap_or(&data[(i + 1).min(data.len())..]));
                    i += 1 + n as usize;
                }
                let indices = gif_lzw(&packed, min_code, w * h);
                // Interlaced images store every 8th row, then the 4th, ...
                let rows: Vec<usize> = if f & 0x40 != 0 {
                    [(0, 8), (4, 8), (2, 4), (1, 2)].iter().flat_map(|&(start, step)| (start..h).step_by(step)).collect()
                } else {
                    (0..h).collect()
                };
                for (k, &y) in rows.iter().enumerate() {
                    let (cy, row) = (top + y, &indices[(k * w).min(indices.len())..((k + 1) * w).min(indices.len())]);
                    if cy >= height {
                        continue;
                    }
                    for (x, &index) in row.iter().enumerate() {
                        let cx = left + x;
                        if cx >= width || Some(index) == transparent {
                            continue;
                        }
                        if let Some(c) = colors.get(index as usize * 3..index as usize * 3 + 3) {
                            out.data[(cy * width + cx) * 3..][..3].copy_from_slice(c);
                        }
                    }
                }
                return Ok(out);
            }
            _ => return Err(bad("no picture")),
        }
    }
}

/// Skips data sub-blocks (each a length byte, then that many bytes)
/// starting at `pos`, up to the empty one that ends them.
fn skip_blocks(data: &[u8], mut pos: usize) -> usize {
    while let Some(&n) = data.get(pos) {
        pos += 1 + n as usize;
        if n == 0 {
            break;
        }
    }
    pos
}

/// GIF's LZW: codes stored least significant bit first, starting one bit
/// longer than `min_code` and growing up to 12 bits.
fn gif_lzw(input: &[u8], min_code: u8, expected: usize) -> Vec<u8> {
    let clear = 1usize << min_code;
    let end = clear + 1;
    let mut table: Vec<(u16, u8)> = Vec::with_capacity(4096);
    let reset = |t: &mut Vec<(u16, u8)>| {
        t.clear();
        t.extend((0..clear).map(|b| (u16::MAX, b as u8)));
        t.push((u16::MAX, 0));
        t.push((u16::MAX, 0));
    };
    reset(&mut table);
    let mut size = min_code as u32 + 1;
    let (mut buf, mut bits) = (0u32, 0u32);
    let mut prev: Option<usize> = None;
    let mut out = Vec::with_capacity(expected);
    let mut scratch = Vec::new();
    let write = |code: usize, table: &[(u16, u8)], out: &mut Vec<u8>, scratch: &mut Vec<u8>| -> u8 {
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
        *scratch.last().unwrap()
    };
    let mut bytes = input.iter();
    while out.len() < expected {
        while bits < size {
            match bytes.next() {
                Some(&b) => {
                    buf |= (b as u32) << bits;
                    bits += 8;
                }
                None => return out,
            }
        }
        let code = (buf & ((1 << size) - 1)) as usize;
        buf >>= size;
        bits -= size;
        if code == clear {
            reset(&mut table);
            size = min_code as u32 + 1;
            prev = None;
            continue;
        }
        if code == end {
            break;
        }
        match prev {
            None if code < table.len() => {
                write(code, &table, &mut out, &mut scratch);
            }
            None => break,
            Some(p) => {
                let first = if code < table.len() {
                    write(code, &table, &mut out, &mut scratch)
                } else if code == table.len() {
                    let f = write(p, &table, &mut out, &mut scratch);
                    out.push(f);
                    f
                } else {
                    break;
                };
                if table.len() < 4096 {
                    table.push((p as u16, first));
                    if table.len() == 1 << size && size < 12 {
                        size += 1;
                    }
                }
            }
        }
        prev = Some(code);
    }
    out.truncate(expected);
    out
}

// ----------------------------------------------------------------------
// BMP (Windows bitmaps)
// ----------------------------------------------------------------------

pub(super) fn decode_bmp(data: &[u8]) -> Result<Image> {
    let bad = |what| damaged("BMP", what);
    let u16_at = |i: usize| data.get(i..i + 2).map(|b| u16::from_le_bytes([b[0], b[1]]) as u32);
    let u32_at = |i: usize| data.get(i..i + 4).map(|b| u32::from_le_bytes(b.try_into().unwrap()));
    let offset = u32_at(10).ok_or_else(|| bad("cut short"))? as usize;
    let header = u32_at(14).ok_or_else(|| bad("cut short"))? as usize;
    let (width, height, bpp, compression, colors_used, palette_entry) = if header == 12 {
        // The old OS/2 header: 16-bit sizes, 3-byte palette entries.
        let w = u16_at(18).ok_or_else(|| bad("cut short"))? as i64;
        let h = u16_at(20).ok_or_else(|| bad("cut short"))? as i16 as i64;
        (w, h, u16_at(24).unwrap_or(0), 0, 0, 3)
    } else {
        let w = u32_at(18).ok_or_else(|| bad("cut short"))? as i32 as i64;
        let h = u32_at(22).ok_or_else(|| bad("cut short"))? as i32 as i64;
        (w, h, u16_at(28).unwrap_or(0), u32_at(30).unwrap_or(0), u32_at(46).unwrap_or(0) as usize, 4)
    };
    // Rows are stored bottom up, unless the height is negative.
    let top_down = height < 0;
    let (width, height) = (width.unsigned_abs(), height.unsigned_abs());
    check_size(width, height)?;
    let (w, h) = (width as usize, height as usize);

    // Color masks for 16- and 32-bit pixels: after the 40-byte header, or
    // inside the larger ones.
    let masks = if compression == 3 || compression == 6 {
        let at = if header >= 52 { 54 } else { 14 + header };
        [u32_at(at), u32_at(at + 4), u32_at(at + 8)].map(|m| m.unwrap_or(0))
    } else if bpp == 16 {
        [0x7C00, 0x03E0, 0x001F]
    } else {
        [0xFF_0000, 0xFF00, 0xFF]
    };
    let palette_at = 14 + header + if compression == 3 && header == 40 { 12 } else { 0 };
    let palette_len = if bpp <= 8 { if colors_used > 0 { colors_used.min(256) } else { 1 << bpp } } else { 0 };
    let palette: Vec<[u8; 3]> = (0..palette_len)
        .map_while(|i| data.get(palette_at + i * palette_entry..palette_at + i * palette_entry + 3).map(|c| [c[2], c[1], c[0]]))
        .collect();
    let color = |i: usize| palette.get(i).copied().unwrap_or([0, 0, 0]);
    let pixels = data.get(offset..).ok_or_else(|| bad("cut short"))?;

    let mut out = Image { width: width as u32, height: height as u32, channels: 3, data: vec![255; w * h * 3] };
    let mut put = |x: usize, y: usize, c: [u8; 3]| {
        if x < w && y < h {
            let row = if top_down { y } else { h - 1 - y };
            out.data[(row * w + x) * 3..][..3].copy_from_slice(&c);
        }
    };
    match (compression, bpp) {
        (1, 8) | (2, 4) => {
            // Run-length encoded: (count, color) pairs, or escapes.
            let (mut x, mut y, mut i) = (0usize, 0usize, 0usize);
            let four = bpp == 4;
            while i + 1 < pixels.len() && y < h {
                let (n, v) = (pixels[i] as usize, pixels[i + 1]);
                i += 2;
                if n > 0 {
                    for k in 0..n {
                        let index = if four { if k % 2 == 0 { v >> 4 } else { v & 15 } } else { v };
                        put(x, y, color(index as usize));
                        x += 1;
                    }
                    continue;
                }
                match v {
                    0 => (x, y) = (0, y + 1),
                    1 => break,
                    2 => {
                        x += *pixels.get(i).unwrap_or(&0) as usize;
                        y += *pixels.get(i + 1).unwrap_or(&0) as usize;
                        i += 2;
                    }
                    count => {
                        let count = count as usize;
                        let bytes = if four { count.div_ceil(2) } else { count };
                        for k in 0..count {
                            let b = *pixels.get(i + if four { k / 2 } else { k }).unwrap_or(&0);
                            let index = if four { if k % 2 == 0 { b >> 4 } else { b & 15 } } else { b };
                            put(x, y, color(index as usize));
                            x += 1;
                        }
                        // Runs of literal pixels are padded to two bytes.
                        i += bytes + bytes % 2;
                    }
                }
            }
        }
        (0 | 3 | 6, 1 | 2 | 4 | 8 | 16 | 24 | 32) => {
            let stride = (w * bpp as usize).div_ceil(32) * 4;
            let channel = |v: u32, mask: u32| -> u8 {
                if mask == 0 {
                    return 0;
                }
                let shift = mask.trailing_zeros();
                let max = mask >> shift;
                (((v & mask) >> shift) * 255 / max) as u8
            };
            for y in 0..h {
                let Some(row) = pixels.get(y * stride..y * stride + stride) else { break };
                for x in 0..w {
                    let c = match bpp {
                        1 | 2 | 4 | 8 => {
                            let bits = bpp as usize;
                            let byte = row[x * bits / 8];
                            let shift = 8 - bits - (x * bits) % 8;
                            color(((byte >> shift) & ((1 << bits) - 1) as u8) as usize)
                        }
                        16 => {
                            let v = u16::from_le_bytes([row[x * 2], row[x * 2 + 1]]) as u32;
                            masks.map(|m| channel(v, m))
                        }
                        24 => [row[x * 3 + 2], row[x * 3 + 1], row[x * 3]],
                        _ => {
                            let v = u32::from_le_bytes(row[x * 4..x * 4 + 4].try_into().unwrap());
                            masks.map(|m| channel(v, m))
                        }
                    };
                    put(x, y, c);
                }
            }
        }
        _ => return Err(Error::Image(format!("this kind of BMP image can't be read ({bpp}-bit, compression {compression})"))),
    }
    Ok(out)
}

// ----------------------------------------------------------------------
// TIFF (TIFF 6.0): strips or tiles, uncompressed, PackBits, LZW, Deflate,
// JPEG and the fax codings (CCITT), which scanners often use.
// ----------------------------------------------------------------------

pub(super) fn decode_tiff(data: &[u8]) -> Result<Image> {
    let bad = |what| damaged("TIFF", what);
    let little = match data.get(0..2) {
        Some(b"II") => true,
        Some(b"MM") => false,
        _ => return Err(bad("bad header")),
    };
    let u16_at = |i: usize| -> Option<u32> {
        let b: [u8; 2] = data.get(i..i + 2)?.try_into().ok()?;
        Some(if little { u16::from_le_bytes(b) } else { u16::from_be_bytes(b) } as u32)
    };
    let u32_at = |i: usize| -> Option<u32> {
        let b: [u8; 4] = data.get(i..i + 4)?.try_into().ok()?;
        Some(if little { u32::from_le_bytes(b) } else { u32::from_be_bytes(b) })
    };
    let ifd = u32_at(4).ok_or_else(|| bad("cut short"))? as usize;
    let entries = u16_at(ifd).ok_or_else(|| bad("cut short"))? as usize;
    // Each tag's values, as numbers (for the tags that are numbers), and
    // where its bytes are (for JPEG tables).
    let mut tags: std::collections::HashMap<u32, (Vec<u32>, usize, usize)> = std::collections::HashMap::new();
    for k in 0..entries.min(1000) {
        let e = ifd + 2 + 12 * k;
        let (Some(tag), Some(kind), Some(count)) = (u16_at(e), u16_at(e + 2), u32_at(e + 4)) else { break };
        let size = match kind {
            1 | 2 | 6 | 7 => 1,
            3 | 8 => 2,
            4 | 9 | 11 => 4,
            5 | 10 | 12 => 8,
            _ => continue,
        };
        let count = count as usize;
        let bytes = size * count;
        let at = if bytes <= 4 { e + 8 } else { u32_at(e + 8).unwrap_or(0) as usize };
        let values = match kind {
            3 => (0..count.min(1 << 20)).map_while(|i| u16_at(at + 2 * i)).collect(),
            4 => (0..count.min(1 << 20)).map_while(|i| u32_at(at + 4 * i)).collect(),
            1 => (0..count.min(1 << 20)).map_while(|i| data.get(at + i).map(|&b| b as u32)).collect(),
            _ => Vec::new(),
        };
        tags.insert(tag, (values, at, bytes));
    }
    let one = |tag: u32, default: u32| tags.get(&tag).and_then(|t| t.0.first().copied()).unwrap_or(default);
    let list = |tag: u32| tags.get(&tag).map(|t| t.0.clone()).unwrap_or_default();
    let (width, height) = (one(256, 0) as usize, one(257, 0) as usize);
    check_size(width as u64, height as u64)?;
    let samples = one(277, 1).clamp(1, 8) as usize;
    let bits = one(258, 1) as usize;
    let compression = one(259, 1);
    let photometric = one(262, if samples >= 3 { 2 } else { 1 });
    if one(284, 1) == 2 && samples > 1 {
        return Err(Error::Image("TIFF images with separate color planes can't be read yet".into()));
    }
    if !matches!(bits, 1 | 2 | 4 | 8 | 16) {
        return Err(Error::Image(format!("{bits}-bit TIFF images can't be read")));
    }
    let predictor = one(317, 1);
    let lsb_first = one(266, 1) == 2;

    // The image comes in pieces: strips (full rows) or tiles.
    let tiled = tags.contains_key(&324);
    let (piece_w, piece_h, offsets, counts) = if tiled {
        (one(322, 256) as usize, one(323, 256) as usize, list(324), list(325))
    } else {
        (width, (one(278, height as u32) as usize).clamp(1, height), list(273), list(279))
    };
    if piece_w == 0 || offsets.is_empty() {
        return Err(bad("no image data"));
    }
    let across = width.div_ceil(piece_w);
    let row_bytes = (piece_w * samples * bits).div_ceil(8);
    let jpeg_tables = tags.get(&347).and_then(|&(_, at, len)| data.get(at..at + len));

    // Gray or RGB output; JPEG pieces arrive already as pixels.
    let channels: usize = if photometric == 3 || samples >= 3 { 3 } else { 1 };
    let mut out = Image { width: width as u32, height: height as u32, channels: channels as u8, data: vec![255; width * height * channels] };
    let colormap = list(320);
    let max = ((1u32 << bits.min(16)) - 1).max(1);
    for (i, &offset) in offsets.iter().enumerate() {
        let (px, py) = if tiled { ((i % across) * piece_w, (i / across) * piece_h) } else { (0, i * piece_h) };
        if py >= height {
            break;
        }
        let rows = piece_h.min(if tiled { piece_h } else { height - py });
        let len = counts.get(i).copied().unwrap_or(0) as usize;
        let raw = data.get(offset as usize..(offset as usize + len).min(data.len())).unwrap_or(&[]);
        let fax = |k: i64, byte_align: bool| {
            let input: Vec<u8> = if lsb_first { raw.iter().map(|b| b.reverse_bits()).collect() } else { raw.to_vec() };
            let p = crate::ccitt::Params { k, columns: piece_w, rows, black_is_1: false, byte_align };
            crate::ccitt::decode(&input, &p)
        };
        let t4 = one(292, 0);
        let mut piece = match compression {
            1 => raw.to_vec(),
            2 => fax(0, true),
            3 => fax(if t4 & 1 != 0 { 1 } else { 0 }, t4 & 4 != 0),
            4 => fax(-1, false),
            5 => crate::filters::lzw(raw, true),
            8 | 32946 => {
                let mut v = Vec::new();
                use std::io::Read;
                flate2::read::ZlibDecoder::new(raw).read_to_end(&mut v).ok();
                v
            }
            32773 => pack_bits(raw),
            7 => {
                // Each piece is a JPEG; the tables they share are kept apart.
                let mut jpeg = Vec::new();
                match jpeg_tables {
                    Some(t) if t.len() > 4 && raw.len() > 2 => {
                        jpeg.extend_from_slice(&t[..t.len() - 2]);
                        jpeg.extend_from_slice(&raw[2..]);
                    }
                    _ => jpeg.extend_from_slice(raw),
                }
                let picture = super::decode_jpeg(&jpeg)?;
                let picture = if channels == 3 { picture.to_rgb() } else { Image { data: picture.luma(), channels: 1, ..picture } };
                for y in 0..(picture.height as usize).min(rows) {
                    for x in 0..(picture.width as usize).min(piece_w) {
                        let (ox, oy) = (px + x, py + y);
                        if ox < width && oy < height {
                            let s = (y * picture.width as usize + x) * channels;
                            out.data[(oy * width + ox) * channels..][..channels].copy_from_slice(&picture.data[s..s + channels]);
                        }
                    }
                }
                continue;
            }
            6 => return Err(Error::Image("old-style JPEG TIFF images can't be read".into())),
            c => return Err(Error::Image(format!("this TIFF compression ({c}) can't be read"))),
        };
        // Fax pieces come out as 1-bit rows, the codes' white runs as 1.
        let fax_coded = matches!(compression, 2..=4);
        let (bits, samples) = if fax_coded { (1, 1) } else { (bits, samples) };
        let row_bytes = if fax_coded { piece_w.div_ceil(8) } else { row_bytes };
        if predictor == 2 && !fax_coded {
            undo_predictor(&mut piece, row_bytes, samples, bits, little);
        }
        for y in 0..rows {
            let Some(row) = piece.get(y * row_bytes..(y + 1) * row_bytes) else { break };
            let oy = py + y;
            if oy >= height {
                break;
            }
            for x in 0..piece_w.min(width.saturating_sub(px)) {
                let sample = |s: usize| -> u32 {
                    let n = x * samples + s;
                    match bits {
                        8 => row[n] as u32,
                        16 => {
                            let b = [row[2 * n], row[2 * n + 1]];
                            (if little { u16::from_le_bytes(b) } else { u16::from_be_bytes(b) }) as u32
                        }
                        _ => ((row[n * bits / 8] >> (8 - bits - (n * bits) % 8)) as u32) & max,
                    }
                };
                let scale = |v: u32| (v * 255 / max) as u8;
                let o = (oy * width + px + x) * channels;
                if fax_coded {
                    // Fax codes' "white" runs are 0 bits in the image, which
                    // are white unless the image says black is zero.
                    let white = (sample(0) != 0) != (photometric == 1);
                    out.data[o] = if white { 255 } else { 0 };
                    continue;
                }
                match photometric {
                    0 => out.data[o] = 255 - scale(sample(0)), // white is zero
                    3 => {
                        // Palette: 16-bit red, green and blue tables.
                        let index = sample(0) as usize;
                        let n = colormap.len() / 3;
                        for c in 0..3 {
                            out.data[o + c] = (colormap.get(c * n + index).copied().unwrap_or(0) >> 8) as u8;
                        }
                    }
                    5 if samples >= 4 => {
                        // CMYK, made into RGB the simple way.
                        let k = 255 - scale(sample(3)) as u32;
                        for c in 0..3 {
                            out.data[o + c] = ((255 - scale(sample(c)) as u32) * k / 255) as u8;
                        }
                    }
                    _ => {
                        // Gray or RGB, maybe with alpha after the colors.
                        let colors = if channels == 3 { 3 } else { 1 };
                        let alpha = (samples > colors && one(338, 0) != 0).then(|| scale(sample(colors)));
                        for c in 0..colors {
                            let v = scale(sample(c));
                            out.data[o + c] = alpha.map_or(v, |a| on_white(v, a));
                        }
                    }
                }
            }
        }
    }
    Ok(out)
}

/// PackBits run-length decoding.
fn pack_bits(input: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(input.len() * 2);
    let mut i = 0;
    while i < input.len() {
        let n = input[i] as i8;
        i += 1;
        if n >= 0 {
            let end = (i + n as usize + 1).min(input.len());
            out.extend_from_slice(&input[i..end]);
            i = end;
        } else if n != -128 {
            if let Some(&b) = input.get(i) {
                out.extend(std::iter::repeat_n(b, 1 - n as isize as usize));
            }
            i += 1;
        }
    }
    out
}

/// Horizontal differencing (TIFF predictor 2): each sample was stored as
/// the difference from the one to its left.
fn undo_predictor(data: &mut [u8], row_bytes: usize, samples: usize, bits: usize, little: bool) {
    for row in data.chunks_mut(row_bytes) {
        match bits {
            8 => {
                for i in samples..row.len() {
                    row[i] = row[i].wrapping_add(row[i - samples]);
                }
            }
            16 => {
                let get = |r: &[u8], i: usize| {
                    let b = [r[2 * i], r[2 * i + 1]];
                    if little { u16::from_le_bytes(b) } else { u16::from_be_bytes(b) }
                };
                for i in samples..row.len() / 2 {
                    let v = get(row, i).wrapping_add(get(row, i - samples));
                    let b = if little { v.to_le_bytes() } else { v.to_be_bytes() };
                    row[2 * i..2 * i + 2].copy_from_slice(&b);
                }
            }
            _ => {}
        }
    }
}

// ----------------------------------------------------------------------
// WebP
// ----------------------------------------------------------------------

pub(super) fn decode_webp(data: &[u8]) -> Result<Image> {
    let fail = |e: &dyn std::fmt::Display| Error::Image(format!("the WebP image is damaged ({e})"));
    let mut decoder = image_webp::WebPDecoder::new(std::io::Cursor::new(data)).map_err(|e| fail(&e))?;
    let (width, height) = decoder.dimensions();
    check_size(width as u64, height as u64)?;
    let alpha = decoder.has_alpha();
    let mut pixels = vec![0; decoder.output_buffer_size().ok_or_else(|| fail(&"too large"))?];
    decoder.read_image(&mut pixels).map_err(|e| fail(&e))?;
    let data = if alpha {
        pixels.chunks_exact(4).flat_map(|p| [on_white(p[0], p[3]), on_white(p[1], p[3]), on_white(p[2], p[3])]).collect()
    } else {
        pixels
    };
    Ok(Image { width, height, channels: 3, data })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_a_gif_with_transparency() {
        // 3×2 GIF: palette red, green, blue, white(transparent index 3).
        let mut gif = b"GIF89a\x03\x00\x02\x00\x81\x00\x00".to_vec();
        gif.extend_from_slice(&[255, 0, 0, 0, 255, 0, 0, 0, 255, 9, 9, 9]);
        gif.extend_from_slice(&[0x21, 0xF9, 4, 1, 0, 0, 3, 0]);
        gif.extend_from_slice(&[0x2C, 0, 0, 0, 0, 3, 0, 2, 0, 0]);
        // Indices 0 1 2 / 3 0 0, LZW with code size 2: clear(4), codes, end(5).
        let codes = [4u32, 0, 1, 2, 3, 0, 0, 5];
        let sizes = [3u32, 3, 3, 3, 4, 4, 4, 4];
        let (mut buf, mut n, mut bytes) = (0u64, 0u32, Vec::new());
        for (c, s) in codes.iter().zip(sizes) {
            buf |= (*c as u64) << n;
            n += s;
        }
        while n > 0 {
            bytes.push(buf as u8);
            buf >>= 8;
            n = n.saturating_sub(8);
        }
        gif.push(2);
        gif.push(bytes.len() as u8);
        gif.extend_from_slice(&bytes);
        gif.extend_from_slice(&[0, 0x3B]);
        let image = Image::decode(&gif).unwrap();
        assert_eq!((image.width, image.height, image.channels), (3, 2, 3));
        assert_eq!(&image.data[..9], &[255, 0, 0, 0, 255, 0, 0, 0, 255]);
        // Transparent: white.
        assert_eq!(&image.data[9..12], &[255, 255, 255]);
        assert_eq!(&image.data[12..18], &[255, 0, 0, 255, 0, 0]);
    }

    #[test]
    fn reads_a_bmp() {
        // 2×2, 24-bit, bottom-up: rows padded to 8 bytes.
        let mut bmp = b"BM".to_vec();
        bmp.extend_from_slice(&(54u32 + 16).to_le_bytes());
        bmp.extend_from_slice(&[0, 0, 0, 0]);
        bmp.extend_from_slice(&54u32.to_le_bytes());
        bmp.extend_from_slice(&40u32.to_le_bytes());
        bmp.extend_from_slice(&2i32.to_le_bytes());
        bmp.extend_from_slice(&2i32.to_le_bytes());
        bmp.extend_from_slice(&1u16.to_le_bytes());
        bmp.extend_from_slice(&24u16.to_le_bytes());
        bmp.extend_from_slice(&[0; 24]);
        // Bottom row: blue, green; top row: red, white (BGR order).
        bmp.extend_from_slice(&[255, 0, 0, 0, 255, 0, 0, 0]);
        bmp.extend_from_slice(&[0, 0, 255, 255, 255, 255, 0, 0]);
        let image = Image::decode(&bmp).unwrap();
        assert_eq!(image.data, vec![255, 0, 0, 255, 255, 255, 0, 0, 255, 0, 255, 0]);
    }

    #[test]
    fn reads_a_tiff() {
        // 2×2 8-bit gray, uncompressed, little-endian, one strip.
        let mut t = b"II\x2A\x00".to_vec();
        t.extend_from_slice(&8u32.to_le_bytes());
        let entries: [(u16, u16, u32, u32); 6] =
            [(256, 3, 1, 2), (257, 3, 1, 2), (258, 3, 1, 8), (262, 3, 1, 1), (273, 4, 1, 8 + 2 + 6 * 12 + 4), (279, 4, 1, 4)];
        t.extend_from_slice(&(entries.len() as u16).to_le_bytes());
        for (tag, kind, count, value) in entries {
            t.extend_from_slice(&tag.to_le_bytes());
            t.extend_from_slice(&kind.to_le_bytes());
            t.extend_from_slice(&count.to_le_bytes());
            t.extend_from_slice(&value.to_le_bytes());
        }
        t.extend_from_slice(&[0; 4]);
        t.extend_from_slice(&[0, 64, 128, 255]);
        let image = Image::decode(&t).unwrap();
        assert_eq!((image.width, image.height, image.channels), (2, 2, 1));
        assert_eq!(image.data, vec![0, 64, 128, 255]);
        assert_eq!(pack_bits(&[2, 1, 2, 3, 0xFE, 9]), vec![1, 2, 3, 9, 9, 9]);
    }
}
