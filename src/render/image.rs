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
/// Returns `None` if the image can't be decoded.
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

    // An image with only an image codec as its filter (the usual case for
    // JPEG photos and JPEG 2000 or JBIG2 scans) is decoded straight from the
    // file's bytes, not from a copy of them.
    let codec = match doc.get_resolved(dict, b"Filter") {
        Ok(Object::Name(n)) => Some(n),
        Ok(Object::Array(a)) if a.len() == 1 => a[0].as_name().map(<[u8]>::to_vec),
        _ => None,
    }
    .filter(|n| matches!(n.as_slice(), b"DCTDecode" | b"DCT" | b"JPXDecode" | b"JBIG2Decode"));
    let only_codec = codec.is_some();
    let decoded = match codec {
        Some(name) => crate::filters::Decoded { data: Vec::new(), remaining_filter: Some(name) },
        None => doc.decode_stream_full(stream).ok()?,
    };
    let encoded: &[u8] = if only_codec { &stream.raw } else { &decoded.data };
    let mut space = None;
    // The alpha channel of a JPEG 2000 image with /SMaskInData.
    let mut own_alpha: Option<Samples> = None;
    // JPEG 2000 images ignore /Decode, except as stencil masks (§8.9.5.1).
    let mut ignore_decode = false;
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
            let s = decode_jpeg(encoded, target)?;
            if !is_mask {
                space = color_space(doc, dict, resources).filter(|cs| cs.components() == s.comps);
                if space.is_none() {
                    space = Some(ColorSpace::from_components(s.comps));
                }
            }
            s
        }
        Some(b"JPXDecode") => {
            ignore_decode = !is_mask;
            let given = if is_mask { None } else { color_space(doc, dict, resources) };
            let indexed = matches!(given, Some(ColorSpace::Indexed { .. }));
            let jpx = decode_jpx(encoded, target, indexed)?;
            // With /SMaskInData the image's own alpha channel is its soft mask.
            let smask_in_data = int(b"SMaskInData").unwrap_or(0) != 0;
            own_alpha = jpx.alpha.filter(|_| smask_in_data && !is_mask);
            if !is_mask {
                // The dictionary's color space wins over the one in the file
                // (§8.9.5.1), if it fits the number of components.
                space = given
                    .filter(|cs| cs.components() == jpx.samples.comps)
                    .or(jpx.space)
                    .or_else(|| Some(ColorSpace::from_components(jpx.samples.comps)));
            }
            jpx.samples
        }
        Some(b"JBIG2Decode") => {
            if !is_mask {
                space = Some(ColorSpace::Gray);
            }
            decode_jbig2(doc, stream, encoded)?
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
        .filter(|_| !ignore_decode)
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
        if samples.bpc == 1 {
            let ones = ones_coverage(&samples, ow, oh);
            for (px, &f) in pixels.iter_mut().zip(&ones) {
                let hit = if paint_value == 1 { f } else { 255 - f } as u32;
                if hit > 0 {
                    let a = (c[3] as u32 * hit / 255) as u8;
                    *px = ColorU8::from_rgba(c[0], c[1], c[2], a).premultiply();
                }
            }
            return Some(pixmap);
        }
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
    let alpha = match own_alpha {
        Some(a) => Some(resample_alpha(&a, ow, oh)),
        None => soft_mask(doc, dict, resources, ow, oh).or_else(|| stencil_alpha(doc, dict, resources, ow, oh)),
    };
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

    // Fast path for black-and-white scans (JBIG2, CCITT fax, 1-bit images):
    // each output pixel mixes the two colors by how much of it is 1 bits.
    if color_key.is_none() && samples.bpc == 1 && comps == 1 {
        let lut = lut.as_ref()?;
        let ones = ones_coverage(&samples, ow, oh);
        for (i, (px, &f)) in pixels.iter_mut().zip(&ones).enumerate() {
            let (f, g) = (f as u32, 255 - f as u32);
            let rgb: [u8; 3] = std::array::from_fn(|c| ((lut[0][c] as u32 * g + lut[1][c] as u32 * f + 127) / 255) as u8);
            let a = alpha.as_ref().map_or(255, |m| m[i]);
            *px = ColorU8::from_rgba(rgb[0], rgb[1], rgb[2], a).premultiply();
        }
        return Some(pixmap);
    }

    // Fast path for the usual photos and scans: 8-bit gray (through the
    // lookup table) or RGB with the default decoding, without a color key.
    let direct_rgb = comps == 3 && matches!(space, ColorSpace::Rgb) && ranges.iter().all(|&r| r == (0.0, 1.0));
    if color_key.is_none() && samples.bpc == 8 && (lut.is_some() || direct_rgb) {
        let row_bytes = samples.row_bytes();
        let mut data = samples.data;
        if data.len() < row_bytes * samples.height {
            data.resize(row_bytes * samples.height, 0); // short data reads as 0, like `Samples::get`
        }
        // Here each source pixel counts by how much of the output pixel it
        // covers. Averaging whole pixels instead makes an image shown a little
        // smaller than it is stored (1275 pixels shown at 1224) mostly skip
        // pixels, which makes sharp text in scans jagged.
        let wx = weights(samples.width, ow);
        let wy = weights(samples.height, oh);
        for (oy, ys) in wy.iter().enumerate() {
            for (ox, xs) in wx.iter().enumerate() {
                let (mut sum, mut n) = ([0u32; 3], 0u32);
                for &(sy, fy) in ys {
                    let row = &data[sy * row_bytes..(sy + 1) * row_bytes];
                    for &(sx, fx) in xs {
                        let rgb = match &lut {
                            Some(lut) => lut[row[sx] as usize],
                            None => [row[3 * sx], row[3 * sx + 1], row[3 * sx + 2]],
                        };
                        let f = fx * fy;
                        sum[0] += rgb[0] as u32 * f;
                        sum[1] += rgb[1] as u32 * f;
                        sum[2] += rgb[2] as u32 * f;
                        n += f;
                    }
                }
                let rgb = sum.map(|v| ((v + n / 2) / n) as u8);
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

/// Source pixels and their weights (out of 64) for each of `out` output
/// pixels along an axis of `src` pixels: how much of the output pixel each
/// covers. Shrinking by more than 8, whole pixels are sampled as in
/// [`spans`], where a pixel more or less doesn't show.
fn weights(src: usize, out: usize) -> Vec<Vec<(usize, u32)>> {
    if src > 8 * out {
        return spans(src, out).into_iter().map(|s| s.map(|i| (i, 1)).collect()).collect();
    }
    let scale = src as f64 / out as f64;
    (0..out)
        .map(|i| {
            let (start, end) = (i as f64 * scale, ((i + 1) as f64 * scale).min(src as f64));
            let mut taps: Vec<(usize, u32)> = (start.floor() as usize..(end.ceil() as usize).min(src))
                .map(|p| {
                    let cover = (end.min(p as f64 + 1.0) - start.max(p as f64)).max(0.0);
                    (p, (cover * 64.0 / scale).round() as u32)
                })
                .filter(|&(_, w)| w > 0)
                .collect();
            if taps.is_empty() {
                taps.push(((start as usize).min(src - 1), 1));
            }
            taps
        })
        .collect()
}

/// For a 1-bit image shown at `ow` × `oh`: how much of each output pixel
/// (0 to 255) is covered by 1 bits.
///
/// The bits are read straight from the packed rows, a row of output at a
/// time, so a 600 dpi page (5100 × 6600) shrinks to the screen without
/// unpacking it. Up to 8 times smaller, pixels are weighted as in
/// [`weights`]; beyond that (thumbnails) every pixel is counted, eight at a
/// time, so thin lines of text aren't skipped.
fn ones_coverage(samples: &Samples, ow: usize, oh: usize) -> Vec<u8> {
    let row_bytes = samples.row_bytes();
    let count_all = samples.width > 8 * ow;
    // Each output column: its taps as (byte, bit shift, weight), stored one
    // after another, the bytes they span, and their total weight. When every
    // pixel is counted, the columns are bit ranges instead.
    let mut taps: Vec<(usize, u32, u32)> = Vec::new();
    let mut cols: Vec<(usize, usize, usize, usize, u32)> = Vec::with_capacity(ow); // taps from..to, bytes first..=last, weight
    if count_all {
        for i in 0..ow {
            let (x0, x1) = (i * samples.width / ow, ((i + 1) * samples.width / ow).max(i * samples.width / ow + 1));
            cols.push((x0, x1, x0 / 8, (x1 - 1) / 8, (x1 - x0) as u32));
        }
    } else {
        for col in weights(samples.width, ow) {
            let from = taps.len();
            let total = col.iter().map(|t| t.1).sum();
            let (first, last) = (col[0].0 / 8, col[col.len() - 1].0 / 8);
            taps.extend(col.into_iter().map(|(x, w)| (x / 8, 7 - (x % 8) as u32, w)));
            cols.push((from, taps.len(), first, last, total));
        }
    }
    let rows: Vec<Vec<(usize, u32)>> = if samples.height > 8 * oh {
        (0..oh).map(|i| (i * samples.height / oh..((i + 1) * samples.height / oh).max(i * samples.height / oh + 1)).map(|y| (y, 1)).collect()).collect()
    } else {
        weights(samples.height, oh)
    };
    let mut out = Vec::with_capacity(ow * oh);
    let mut sum = vec![0u32; ow];
    for ys in rows {
        sum.fill(0);
        let mut total_y = 0u32;
        for (sy, fy) in ys {
            total_y += fy;
            let Some(row) = samples.data.get(sy * row_bytes..(sy + 1) * row_bytes) else { continue };
            for (s, &(from, to, first, last, total)) in sum.iter_mut().zip(&cols) {
                // Paper and solid ink: whole bytes of one color, no bits to read.
                let bytes = &row[first..=last];
                let h = if bytes.iter().all(|&b| b == 0xFF) {
                    total
                } else if bytes.iter().all(|&b| b == 0) {
                    0
                } else if count_all {
                    count_ones(row, from, to)
                } else {
                    taps[from..to].iter().map(|&(byte, shift, w)| (row[byte] as u32 >> shift & 1) * w).sum()
                };
                *s += h * fy;
            }
        }
        for (s, &(.., total)) in sum.iter().zip(&cols) {
            let total = total * total_y;
            out.push(((s * 255 + total / 2) / total.max(1)) as u8);
        }
    }
    out
}

/// The number of 1 bits in bits `from..to` of a packed row (bit 0 is the
/// high bit of the first byte).
fn count_ones(row: &[u8], from: usize, to: usize) -> u32 {
    let (first, last) = (from / 8, (to - 1) / 8);
    let head = 0xFFu8 >> (from % 8);
    let tail = 0xFFu8 << (7 - (to - 1) % 8);
    if first == last {
        return (row[first] & head & tail).count_ones();
    }
    (row[first] & head).count_ones() + row[first + 1..last].iter().map(|b| b.count_ones()).sum::<u32>() + (row[last] & tail).count_ones()
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

/// A decoded JPEG 2000 image: its color samples, the color space given in
/// the file, and its alpha channel if it has one.
struct Jpx {
    samples: Samples,
    space: Option<ColorSpace>,
    alpha: Option<Samples>,
}

/// Decodes a JPEG 2000 image (JPXDecode, §7.4.9), a JP2 file or a bare
/// codestream.
///
/// JPEG 2000 stores an image as a pyramid of resolutions, so an image shown
/// smaller than it is stored is decoded only up to the level that covers the
/// screen (at most twice the size needed): a 6000-pixel scan shown 1000
/// pixels wide is decoded at about 1500 pixels, never at full size. With
/// `indexed`, the samples are palette indices that the dictionary's
/// /Indexed color space looks up, so the file's own palette is not applied.
fn decode_jpx(data: &[u8], target: (f64, f64), indexed: bool) -> Option<Jpx> {
    use hayro_jpeg2000::{DecodeSettings, DecoderContext, Image};
    let settings = DecodeSettings {
        resolve_palette_indices: !indexed,
        strict: false,
        target_resolution: Some((target.0.ceil().max(1.0) as u32, target.1.ceil().max(1.0) as u32)),
    };
    let image = Image::new(data, &settings).ok()?;
    // The size it will be decoded at, with the levels it skips left out.
    let (width, height) = (image.width() as usize, image.height() as usize);
    // The decoder keeps each component as 32-bit floats while it works:
    // refuse what would need gigabytes.
    if (width as u64) * (height as u64) > MAX_PIXELS {
        return None;
    }
    let color_space = image.color_space().clone();
    let has_alpha = image.has_alpha();
    let mut ctx = DecoderContext::default();
    let decoded = image.decode(&mut ctx).ok()?;
    let components = decoded.components();
    let n = components.len();
    let pixels = width * height;
    let color_n = n.checked_sub(has_alpha as usize).filter(|&c| c > 0)?;
    if components.iter().any(|c| c.samples().len() != pixels) {
        return None;
    }
    if indexed {
        // Palette indices as they are, not scaled to 8 bits.
        let data = components[0].samples().iter().map(|&v| v.round().clamp(0.0, 255.0) as u8).collect();
        return Some(Jpx { samples: Samples { data, width, height, comps: 1, bpc: 8 }, space: None, alpha: None });
    }
    let all = decoded.data_u8();
    let space = Some(jpx_space(&color_space, color_n));
    if !has_alpha {
        return Some(Jpx { samples: Samples { data: all, width, height, comps: n, bpc: 8 }, space, alpha: None });
    }
    let mut color = Vec::with_capacity(pixels * color_n);
    let mut alpha = Vec::with_capacity(pixels);
    for px in all.chunks_exact(n) {
        color.extend_from_slice(&px[..color_n]);
        alpha.push(px[color_n]);
    }
    Some(Jpx {
        samples: Samples { data: color, width, height, comps: color_n, bpc: 8 },
        space,
        alpha: Some(Samples { data: alpha, width, height, comps: 1, bpc: 8 }),
    })
}

/// Our color space for the one a JPEG 2000 file declares.
fn jpx_space(space: &hayro_jpeg2000::ColorSpace, n: usize) -> ColorSpace {
    use hayro_jpeg2000::ColorSpace as Jcs;
    match space {
        Jcs::Gray => ColorSpace::Gray,
        Jcs::RGB => ColorSpace::Rgb,
        Jcs::CMYK => ColorSpace::Cmyk,
        // ICC profiles aren't applied anywhere (see color.rs): use the
        // device space with as many components.
        Jcs::Icc { .. } | Jcs::Unknown { .. } => ColorSpace::from_components(n),
    }
}

/// Decodes a JBIG2 image (JBIG2Decode, §7.4.7) into one bit per pixel, with
/// 0 for black as PDF images have it (JBIG2 itself uses 1 for black).
///
/// The image is in the embedded organization (no file header), and its
/// symbol dictionaries may be shared between images in a /JBIG2Globals
/// stream. JBIG2 has no smaller resolutions to decode, but the result is
/// packed eight pixels to a byte, as the decoder holds it too: a 5100×6600
/// scan takes about 4 MB.
fn decode_jbig2(doc: &Document, stream: &Stream, data: &[u8]) -> Option<Samples> {
    use hayro_jbig2::{DecodeSettings, Globals, Page};
    let settings = DecodeSettings::default();
    let parms = match doc.get_resolved(&stream.dict, b"DecodeParms").ok()? {
        Object::Dictionary(d) => Some(d),
        Object::Array(a) => a.last().and_then(|o| doc.resolve(o).ok()).and_then(|o| o.as_dict().cloned()),
        _ => None,
    };
    let globals_data = match parms.as_ref().map(|p| doc.get_resolved(p, b"JBIG2Globals")) {
        Some(Ok(Object::Stream(g))) => Some(doc.decode_stream_full(&g).ok()?.data),
        _ => None,
    };
    let globals = match &globals_data {
        Some(g) => Some(Globals::new(g, &settings).ok()?),
        None => None,
    };
    let page = Page::new_embedded(data, globals.as_ref(), &settings).ok()?;
    let (width, height) = (page.width() as usize, page.height() as usize);
    if (width as u64) * (height as u64) > MAX_PIXELS * 16 {
        return None;
    }
    let row_bytes = width.div_ceil(8);
    let mut out = Packer { data: vec![0xFF; row_bytes * height], row_bytes, row: 0, x: 0 };
    page.decode(&mut out).ok()?;
    Some(Samples { data: out.data, width, height, comps: 1, bpc: 1 })
}

/// Collects JBIG2 pixels into rows of bits, black as 0 (the rows start
/// white, so only black pixels are written).
struct Packer {
    data: Vec<u8>,
    row_bytes: usize,
    row: usize,
    x: usize,
}

impl hayro_jbig2::Decoder for Packer {
    fn push_pixel(&mut self, black: bool) {
        if black && let Some(b) = self.data.get_mut(self.row * self.row_bytes + self.x / 8) {
            *b &= !(0x80 >> (self.x % 8));
        }
        self.x += 1;
    }

    fn push_pixel_chunk(&mut self, black: bool, chunk_count: u32) {
        // Called only at whole bytes.
        let start = self.row * self.row_bytes + self.x / 8;
        let end = (start + chunk_count as usize).min((self.row + 1) * self.row_bytes).min(self.data.len());
        if black && start < end {
            self.data[start..end].fill(0);
        }
        self.x += 8 * chunk_count as usize;
    }

    fn next_line(&mut self) {
        self.row += 1;
        self.x = 0;
    }
}

/// An alpha channel at the output size (nearest neighbor).
fn resample_alpha(a: &Samples, w: usize, h: usize) -> Vec<u8> {
    (0..w * h)
        .map(|i| {
            let (x, y) = (i % w * a.width / w, i / w * a.height / h);
            a.data.get(y * a.width + x).copied().unwrap_or(255)
        })
        .collect()
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

    #[test]
    fn coverage_weights() {
        // 3 pixels into 2: the middle one is split between both halves.
        assert_eq!(weights(3, 2), vec![vec![(0, 43), (1, 21)], vec![(1, 21), (2, 43)]]);
        // Same size: one pixel each.
        assert_eq!(weights(2, 2), vec![vec![(0, 64)], vec![(1, 64)]]);
        // Shrinking more than 8 times samples whole pixels.
        assert!(weights(100, 2).iter().all(|taps| taps.len() <= 9 && taps.iter().all(|t| t.1 == 1)));
    }

    #[test]
    fn one_bit_coverage() {
        // 16 × 1 pixels, the first 4 bits 1: into 2 pixels, the first is half 1s.
        let s = Samples { data: vec![0b1111_0000, 0], width: 16, height: 1, comps: 1, bpc: 1 };
        assert_eq!(ones_coverage(&s, 2, 1), vec![128, 0]);
        // Whole bytes of one color take the shortcut and give the same result.
        let s = Samples { data: vec![0xFF, 0x00, 0xFF, 0xF0], width: 16, height: 2, comps: 1, bpc: 1 };
        assert_eq!(ones_coverage(&s, 2, 1), vec![255, 64]);
        // Shrunk 18 times every bit is counted: 3 of 18 bits are 1.
        let s = Samples { data: vec![0b0010_0000, 0b1000_0001, 0], width: 18, height: 1, comps: 1, bpc: 1 };
        assert_eq!(ones_coverage(&s, 1, 1), vec![43]);
        assert_eq!(count_ones(&[0b1111_1111, 0b1010_1010], 3, 13), 5 + 3);
    }
}
