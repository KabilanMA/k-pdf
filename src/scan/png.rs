//! A small PNG reader for photos and screenshots (PNG specification,
//! 3rd edition): every color type and bit depth, interlaced or not.
//! Transparent pixels are put on white; 16-bit samples keep their high byte.

use std::io::Read;

use super::{Image, MAX_PIXELS};
use crate::error::{Error, Result};

fn damaged(what: &str) -> Error {
    Error::Image(format!("the PNG image is damaged ({what})"))
}

pub(super) fn decode(data: &[u8]) -> Result<Image> {
    let mut pos = 8;
    let mut header = None;
    let mut palette: &[u8] = &[];
    let mut alpha: &[u8] = &[];
    let mut compressed = Vec::new();
    while pos + 8 <= data.len() {
        let len = u32::from_be_bytes(data[pos..pos + 4].try_into().unwrap()) as usize;
        let kind = &data[pos + 4..pos + 8];
        let body = data.get(pos + 8..pos + 8 + len).ok_or_else(|| damaged("cut short"))?;
        match kind {
            b"IHDR" if body.len() >= 13 => header = Some(body),
            b"PLTE" => palette = body,
            b"tRNS" => alpha = body,
            b"IDAT" => compressed.extend_from_slice(body),
            b"IEND" => break,
            _ => {}
        }
        pos += 12 + len;
    }
    let h = header.ok_or_else(|| damaged("no header"))?;
    let width = u32::from_be_bytes(h[0..4].try_into().unwrap());
    let height = u32::from_be_bytes(h[4..8].try_into().unwrap());
    let (depth, color, interlaced) = (h[8], h[9], h[12] == 1);
    let samples = match color {
        0 => 1,
        2 => 3,
        3 => 1,
        4 => 2,
        6 => 4,
        _ => return Err(damaged("unknown color type")),
    };
    let depth_ok = match color {
        0 => matches!(depth, 1 | 2 | 4 | 8 | 16),
        3 => matches!(depth, 1 | 2 | 4 | 8),
        _ => matches!(depth, 8 | 16),
    };
    if !depth_ok || width == 0 || height == 0 {
        return Err(damaged("bad header"));
    }
    if width as u64 * height as u64 > MAX_PIXELS {
        return Err(Error::Image("the photo is too large".into()));
    }
    if color == 3 && palette.is_empty() {
        return Err(damaged("no palette"));
    }

    let bits = samples * depth as usize;
    let filter_step = bits.div_ceil(8);
    let passes: &[(u32, u32, u32, u32)] = if interlaced {
        &[(0, 0, 8, 8), (4, 0, 8, 8), (0, 4, 4, 8), (2, 0, 4, 4), (0, 2, 2, 4), (1, 0, 2, 2), (0, 1, 1, 2)]
    } else {
        &[(0, 0, 1, 1)]
    };
    let pass_size = |&(x0, y0, dx, dy): &(u32, u32, u32, u32)| {
        let pw = if width > x0 { (width - x0).div_ceil(dx) } else { 0 };
        let ph = if height > y0 { (height - y0).div_ceil(dy) } else { 0 };
        (pw as usize, ph as usize)
    };
    let expected: usize = passes
        .iter()
        .map(|p| {
            let (pw, ph) = pass_size(p);
            if pw == 0 { 0 } else { ph * (1 + (pw * bits).div_ceil(8)) }
        })
        .sum();
    let mut raw = Vec::with_capacity(expected);
    flate2::read::ZlibDecoder::new(&compressed[..])
        .take(expected as u64)
        .read_to_end(&mut raw)
        .map_err(|_| damaged("bad compressed data"))?;
    if raw.len() < expected {
        return Err(damaged("cut short"));
    }

    let channels: u8 = if matches!(color, 0 | 4) { 1 } else { 3 };
    let mut out = Image::new(width, height, channels);
    let max = (1u32 << depth.min(8)) - 1;
    let mut offset = 0;
    for pass in passes {
        let (pw, ph) = pass_size(pass);
        if pw == 0 || ph == 0 {
            continue;
        }
        let row_bytes = (pw * bits).div_ceil(8);
        let mut prev = vec![0u8; row_bytes];
        let mut row = vec![0u8; row_bytes];
        for py in 0..ph {
            let filter = raw[offset];
            row.copy_from_slice(&raw[offset + 1..offset + 1 + row_bytes]);
            offset += 1 + row_bytes;
            unfilter(filter, &mut row, &prev, filter_step)?;
            let y = (pass.1 + py as u32 * pass.3) as usize;
            for px in 0..pw {
                let sample = |s: usize| -> u32 {
                    let i = px * samples + s;
                    match depth {
                        16 => row[i * 2] as u32,
                        8 => row[i] as u32,
                        d => {
                            let bit = i * d as usize;
                            (row[bit / 8] as u32 >> (8 - d as usize - bit % 8)) & max
                        }
                    }
                };
                let scale = |v: u32| if depth < 8 { v * 255 / max } else { v };
                let (rgb, a) = match color {
                    0 => ([scale(sample(0)); 3], 255),
                    2 => ([sample(0), sample(1), sample(2)], 255),
                    3 => {
                        let i = sample(0) as usize;
                        let p = palette.get(i * 3..i * 3 + 3).unwrap_or(&[0, 0, 0]);
                        ([p[0] as u32, p[1] as u32, p[2] as u32], alpha.get(i).copied().unwrap_or(255) as u32)
                    }
                    4 => ([sample(0); 3], sample(1)),
                    _ => ([sample(0), sample(1), sample(2)], sample(3)),
                };
                // On white.
                let rgb = rgb.map(|v| ((v * a + 255 * (255 - a) + 127) / 255) as u8);
                let x = (pass.0 + px as u32 * pass.2) as usize;
                let d = (y * width as usize + x) * channels as usize;
                if channels == 1 {
                    out.data[d] = rgb[0];
                } else {
                    out.data[d..d + 3].copy_from_slice(&rgb);
                }
            }
            std::mem::swap(&mut prev, &mut row);
        }
    }
    Ok(out)
}

/// Reverses a row's filter (PNG specification, section 9).
fn unfilter(filter: u8, row: &mut [u8], prev: &[u8], step: usize) -> Result<()> {
    match filter {
        0 => {}
        1 => {
            for i in step..row.len() {
                row[i] = row[i].wrapping_add(row[i - step]);
            }
        }
        2 => {
            for i in 0..row.len() {
                row[i] = row[i].wrapping_add(prev[i]);
            }
        }
        3 => {
            for i in 0..row.len() {
                let left = if i >= step { row[i - step] as u16 } else { 0 };
                row[i] = row[i].wrapping_add(((left + prev[i] as u16) / 2) as u8);
            }
        }
        4 => {
            for i in 0..row.len() {
                let a = if i >= step { row[i - step] as i16 } else { 0 };
                let b = prev[i] as i16;
                let c = if i >= step { prev[i - step] as i16 } else { 0 };
                let p = a + b - c;
                let (pa, pb, pc) = ((p - a).abs(), (p - b).abs(), (p - c).abs());
                let pred = if pa <= pb && pa <= pc {
                    a
                } else if pb <= pc {
                    b
                } else {
                    c
                };
                row[i] = row[i].wrapping_add(pred as u8);
            }
        }
        _ => return Err(damaged("unknown row filter")),
    }
    Ok(())
}
