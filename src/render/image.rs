//! Image XObjects and inline images (ISO 32000 §8.9) → RGBA pixmaps.
//!
//! To keep memory low, images are decoded directly at (roughly) the size
//! they will occupy on screen, not at their full resolution.

use tiny_skia::{ColorU8, Pixmap, PremultipliedColorU8};

use super::color::ColorSpace;
use super::function::read_bits;
use crate::document::Document;
use crate::object::{Dictionary, Object, Stream};

/// Refuse to allocate decoded images beyond this many pixels.
const MAX_PIXELS: u64 = 64 * 1024 * 1024;

/// Raw image samples, row-major, `bpc` bits per component.
struct Samples {
    data: Vec<u8>,
    width: usize,
    height: usize,
    comps: usize,
    bpc: u32,
}

impl Samples {
    fn row_bytes(&self) -> usize {
        (self.width * self.comps * self.bpc as usize).div_ceil(8)
    }

    /// Raw value of component `c` of pixel (x, y).
    fn get(&self, x: usize, y: usize, c: usize) -> u32 {
        let row = y * self.row_bytes();
        match self.bpc {
            8 => self.data.get(row + x * self.comps + c).copied().unwrap_or(0) as u32,
            16 => {
                let i = row + (x * self.comps + c) * 2;
                (self.data.get(i).copied().unwrap_or(0) as u32) << 8 | self.data.get(i + 1).copied().unwrap_or(0) as u32
            }
            bpc => read_bits(&self.data, row * 8 + (x * self.comps + c) * bpc as usize, bpc),
        }
    }

    fn max_value(&self) -> f64 {
        ((1u64 << self.bpc) - 1) as f64
    }
}

/// What an image is drawn with: its own colors, or the fill color for a
/// stencil mask (`/ImageMask true`).
pub(crate) enum Paint {
    Colors,
    Stencil([f32; 4]),
}

/// Decodes an image at about `target` device pixels (width, height).
/// Returns `None` for unsupported encodings (JPX, JBIG2, CCITT).
pub(crate) fn decode(
    doc: &Document,
    stream: &Stream,
    resources: &Dictionary,
    paint: Paint,
    target: (f64, f64),
) -> Option<Pixmap> {
    let dict = &stream.dict;
    let int = |k: &[u8]| doc.get_resolved(dict, k).ok().and_then(|o| o.as_i64());
    let is_mask = doc.get_resolved(dict, b"ImageMask").ok().and_then(|o| o.as_bool()).unwrap_or(false);

    // A plain JPEG (the usual case for photos) is decoded straight from the
    // file's bytes, not from a copy of them.
    let only_dct = match doc.get_resolved(dict, b"Filter") {
        Ok(Object::Name(n)) => n == b"DCTDecode" || n == b"DCT",
        Ok(Object::Array(a)) => a.len() == 1 && matches!(a[0].as_name(), Some(b"DCTDecode" | b"DCT")),
        _ => false,
    };
    let decoded = if only_dct {
        crate::filters::Decoded { data: Vec::new(), remaining_filter: Some(b"DCTDecode".to_vec()) }
    } else {
        doc.decode_stream_full(stream).ok()?
    };
    let mut space = None;
    let samples = match decoded.remaining_filter.as_deref() {
        None => {
            let width = int(b"Width")?.max(1) as usize;
            let height = int(b"Height")?.max(1) as usize;
            let bpc = if is_mask { 1 } else { int(b"BitsPerComponent").unwrap_or(8).clamp(1, 16) as u32 };
            if !is_mask {
                space = color_space(doc, dict, resources);
            }
            let comps = if is_mask { 1 } else { space.as_ref().map_or(1, ColorSpace::components).max(1) };
            Samples { data: decoded.data, width, height, comps, bpc }
        }
        Some(b"DCTDecode" | b"DCT") => {
            let s = decode_jpeg(if only_dct { &stream.raw } else { &decoded.data }, target)?;
            if !is_mask {
                space = color_space(doc, dict, resources).filter(|cs| cs.components() == s.comps);
                if space.is_none() {
                    space = Some(ColorSpace::from_components(s.comps));
                }
            }
            s
        }
        Some(_) => return None,
    };
    if (samples.width as u64) * (samples.height as u64) > MAX_PIXELS * 16 {
        return None;
    }

    // Output size: never larger than the source, at most ~device size.
    let ow = (target.0.ceil() as usize).clamp(1, samples.width);
    let oh = (target.1.ceil() as usize).clamp(1, samples.height);
    if (ow as u64) * (oh as u64) > MAX_PIXELS {
        return None;
    }
    let mut pixmap = Pixmap::new(ow as u32, oh as u32)?;

    let decode_arr: Vec<f64> = doc
        .get_resolved(dict, b"Decode")
        .ok()
        .and_then(|o| o.as_array().map(|a| a.iter().filter_map(Object::as_f64).collect()))
        .unwrap_or_default();

    // Each output pixel averages a block of source pixels (box filter), so
    // downscaled scans and bitmap glyphs stay smooth instead of aliasing.
    let src_x = spans(samples.width, ow);
    let src_y = spans(samples.height, oh);
    let pixels = pixmap.pixels_mut();

    if is_mask {
        let Paint::Stencil(color) = paint else { return None };
        // Sample 0 paints (with the default Decode [0 1]); [1 0] inverts.
        let paint_value = if decode_arr.first() == Some(&1.0) { 1 } else { 0 };
        let c = to_u8(color);
        for (oy, ys) in src_y.iter().enumerate() {
            for (ox, xs) in src_x.iter().enumerate() {
                let (mut hit, mut n) = (0u32, 0u32);
                for sy in ys.clone() {
                    for sx in xs.clone() {
                        hit += (samples.get(sx, sy, 0) == paint_value) as u32;
                        n += 1;
                    }
                }
                if hit > 0 {
                    let a = (c[3] as u32 * hit / n) as u8;
                    pixels[oy * ow + ox] = ColorU8::from_rgba(c[0], c[1], c[2], a).premultiply();
                }
            }
        }
        return Some(pixmap);
    }

    let space = space.unwrap_or(ColorSpace::Gray);
    let comps = samples.comps;
    let max = samples.max_value();
    let ranges: Vec<(f64, f64)> = (0..comps)
        .map(|i| match (decode_arr.get(2 * i), decode_arr.get(2 * i + 1)) {
            (Some(&a), Some(&b)) => (a, b),
            _ => space.default_decode(i, samples.bpc),
        })
        .collect();

    // Optional transparency.
    let alpha = soft_mask(doc, dict, resources, ow, oh)
        .or_else(|| stencil_alpha(doc, dict, resources, ow, oh));
    let color_key: Option<Vec<i64>> = match doc.get_resolved(dict, b"Mask") {
        Ok(Object::Array(a)) => Some(a.iter().filter_map(Object::as_i64).collect()),
        _ => None,
    };

    // A lookup table makes single-component images (the most common case
    // for scans, masks and indexed images) cheap to convert.
    let lut: Option<Vec<[u8; 3]>> = (comps == 1 && samples.bpc <= 8).then(|| {
        (0..=max as u32)
            .map(|raw| {
                let v = ranges[0].0 + raw as f64 * (ranges[0].1 - ranges[0].0) / max;
                rgb_u8(space.to_rgb(&[v]).unwrap_or([1.0; 3]))
            })
            .collect()
    });

    // Fast path for the usual photos and scans: 8-bit gray (through the
    // lookup table) or RGB with the default decoding, without a color key.
    let direct_rgb = comps == 3 && matches!(space, ColorSpace::Rgb) && ranges.iter().all(|&r| r == (0.0, 1.0));
    if color_key.is_none() && samples.bpc == 8 && (lut.is_some() || direct_rgb) {
        let row_bytes = samples.row_bytes();
        let mut data = samples.data;
        if data.len() < row_bytes * samples.height {
            data.resize(row_bytes * samples.height, 0); // short data reads as 0, like `Samples::get`
        }
        for (oy, ys) in src_y.iter().enumerate() {
            for (ox, xs) in src_x.iter().enumerate() {
                let (mut sum, mut n) = ([0u32; 3], 0u32);
                for sy in ys.clone() {
                    let row = &data[sy * row_bytes..(sy + 1) * row_bytes];
                    for sx in xs.clone() {
                        let rgb = match &lut {
                            Some(lut) => lut[row[sx] as usize],
                            None => [row[3 * sx], row[3 * sx + 1], row[3 * sx + 2]],
                        };
                        sum[0] += rgb[0] as u32;
                        sum[1] += rgb[1] as u32;
                        sum[2] += rgb[2] as u32;
                        n += 1;
                    }
                }
                let rgb = sum.map(|v| (v / n) as u8);
                let a = alpha.as_ref().map_or(255, |m| m[oy * ow + ox]);
                pixels[oy * ow + ox] = ColorU8::from_rgba(rgb[0], rgb[1], rgb[2], a).premultiply();
            }
        }
        return Some(pixmap);
    }

    let mut comp_buf = vec![0.0f64; comps];
    let mut raw_buf = vec![0u32; comps];
    for (oy, ys) in src_y.iter().enumerate() {
        for (ox, xs) in src_x.iter().enumerate() {
            let (mut sum, mut visible, mut n) = ([0u32; 3], 0u32, 0u32);
            for sy in ys.clone() {
                for sx in xs.clone() {
                    n += 1;
                    for (c, raw) in raw_buf.iter_mut().enumerate() {
                        *raw = samples.get(sx, sy, c);
                    }
                    if let Some(key) = &color_key {
                        let masked = raw_buf.iter().enumerate().all(|(c, &v)| {
                            let (lo, hi) = (key.get(2 * c).copied().unwrap_or(0), key.get(2 * c + 1).copied().unwrap_or(-1));
                            (lo..=hi).contains(&(v as i64))
                        });
                        if masked {
                            continue;
                        }
                    }
                    let rgb = match &lut {
                        Some(lut) => lut[raw_buf[0] as usize],
                        None => {
                            for c in 0..comps {
                                let (lo, hi) = ranges[c];
                                comp_buf[c] = lo + raw_buf[c] as f64 * (hi - lo) / max;
                            }
                            rgb_u8(space.to_rgb(&comp_buf).unwrap_or([1.0; 3]))
                        }
                    };
                    for (s, v) in sum.iter_mut().zip(rgb) {
                        *s += v as u32;
                    }
                    visible += 1;
                }
            }
            if visible == 0 {
                continue; // fully color-key masked: leave transparent
            }
            let rgb = sum.map(|v| (v / visible) as u8);
            let a = alpha.as_ref().map_or(255, |m| m[oy * ow + ox]) as u32 * visible / n;
            pixels[oy * ow + ox] = ColorU8::from_rgba(rgb[0], rgb[1], rgb[2], a as u8).premultiply();
        }
    }
    Some(pixmap)
}

/// Source index ranges for each of `out` output pixels along an axis of
/// `src` pixels. Very large blocks are subsampled (at most 8 per axis).
fn spans(src: usize, out: usize) -> Vec<std::iter::StepBy<std::ops::Range<usize>>> {
    (0..out)
        .map(|i| {
            let start = i * src / out;
            let end = ((i + 1) * src / out).max(start + 1).min(src);
            let step = ((end - start) / 8).max(1);
            (start..end).step_by(step)
        })
        .collect()
}

fn color_space(doc: &Document, dict: &Dictionary, resources: &Dictionary) -> Option<ColorSpace> {
    let cs = dict.get(b"ColorSpace")?;
    ColorSpace::parse(doc, cs, resources)
}

/// `/SMask`: a grayscale image giving per-pixel alpha.
fn soft_mask(doc: &Document, dict: &Dictionary, resources: &Dictionary, w: usize, h: usize) -> Option<Vec<u8>> {
    let Object::Stream(mask) = doc.get_resolved(dict, b"SMask").ok()? else { return None };
    let mut mask_dict = mask.dict.clone();
    mask_dict.set(b"ColorSpace".to_vec(), Object::Name(b"DeviceGray".to_vec()));
    mask_dict.remove(b"SMask");
    let mask = Stream { dict: mask_dict, raw: mask.raw };
    let pm = decode(doc, &mask, resources, Paint::Colors, (w as f64, h as f64))?;
    Some(resample_red(&pm, w, h))
}

/// `/Mask` given as a stencil image: 1 bits are masked out.
fn stencil_alpha(doc: &Document, dict: &Dictionary, resources: &Dictionary, w: usize, h: usize) -> Option<Vec<u8>> {
    let Object::Stream(mask) = doc.get_resolved(dict, b"Mask").ok()? else { return None };
    let pm = decode(doc, &mask, resources, Paint::Stencil([1.0, 1.0, 1.0, 1.0]), (w as f64, h as f64))?;
    let (mw, mh) = (pm.width() as usize, pm.height() as usize);
    let px = pm.pixels();
    Some(
        (0..w * h)
            .map(|i| {
                let (x, y) = (i % w * mw / w, i / w * mh / h);
                px[y * mw + x].alpha()
            })
            .collect(),
    )
}

/// The red channel of `pm`, resampled to w × h (nearest neighbor).
fn resample_red(pm: &Pixmap, w: usize, h: usize) -> Vec<u8> {
    let (mw, mh) = (pm.width() as usize, pm.height() as usize);
    let px: &[PremultipliedColorU8] = pm.pixels();
    (0..w * h)
        .map(|i| {
            let (x, y) = (i % w * mw / w, i / w * mh / h);
            px[y * mw + x].red()
        })
        .collect()
}

fn decode_jpeg(data: &[u8], target: (f64, f64)) -> Option<Samples> {
    let mut decoder = crate::jpeg::Decoder::new(data);
    decoder.read_info().ok()?;
    let info = decoder.info()?;
    // Let the decoder downscale (by up to 8×) when the image is shown small.
    let req_w = (target.0.ceil() as u16).clamp(1, info.width);
    let req_h = (target.1.ceil() as u16).clamp(1, info.height);
    let (width, height) = decoder.scale(req_w, req_h).unwrap_or((info.width, info.height));
    let mut pixels = decoder.decode().ok()?;
    let comps = match info.pixel_format {
        crate::jpeg::PixelFormat::L8 => 1,
        crate::jpeg::PixelFormat::L16 => {
            pixels = pixels.chunks_exact(2).map(|c| c[0]).collect();
            1
        }
        crate::jpeg::PixelFormat::RGB24 => 3,
        crate::jpeg::PixelFormat::CMYK32 => {
            // Adobe applications write CMYK JPEGs inverted.
            if has_adobe_marker(data) {
                pixels.iter_mut().for_each(|v| *v = 255 - *v);
            }
            4
        }
    };
    Some(Samples { data: pixels, width: width as usize, height: height as usize, comps, bpc: 8 })
}

/// Looks for an APP14 "Adobe" segment among the JPEG headers.
fn has_adobe_marker(data: &[u8]) -> bool {
    let head = &data[..data.len().min(64 * 1024)];
    head.windows(7).any(|w| w[0] == 0xFF && w[1] == 0xEE && &w[4..7] == b"Ado")
}

fn rgb_u8(c: [f64; 3]) -> [u8; 3] {
    c.map(|v| (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8)
}

fn to_u8(c: [f32; 4]) -> [u8; 4] {
    c.map(|v| (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sample_access() {
        // 2×1 image, 4 bits per component, one component.
        let s = Samples { data: vec![0xA5], width: 2, height: 1, comps: 1, bpc: 4 };
        assert_eq!((s.get(0, 0, 0), s.get(1, 0, 0)), (0xA, 0x5));
        // Rows are padded to whole bytes.
        let s = Samples { data: vec![0b1000_0000, 0b0100_0000], width: 3, height: 2, comps: 1, bpc: 1 };
        assert_eq!((s.get(0, 0, 0), s.get(0, 1, 0), s.get(1, 1, 0)), (1, 0, 1));
    }
}
