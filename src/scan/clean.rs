//! Turning the page in a photo into a clean scan: flatten, even out the
//! light, straighten, then finish for the chosen mode.

use super::detect::otsu;
use super::{Image, Mode, Quad, ScanOptions, Scanned};

pub(super) fn scan(photo: &Image, o: &ScanOptions) -> Scanned {
    let (w, h) = output_size(&o.quad, o.max_size);
    // Bilinear sampling skips pixels when shrinking a lot: average blocks
    // of the photo first.
    let ratio = side_lengths(&o.quad).iter().fold(0.0f32, |m, &l| m.max(l)) / w.max(h) as f32;
    let k = (ratio / 1.5).floor().max(1.0) as u32;
    let page = if k > 1 {
        let small = photo.shrink_by(k);
        let quad = o.quad.map(|[x, y]| [x / k as f32, y / k as f32]);
        warp(&small, &quad, w, h)
    } else {
        warp(photo, &o.quad, w, h)
    };

    let mut page = if o.mode == Mode::Original { page } else { even_light(&page) };
    if o.straighten {
        let angle = if o.mode == Mode::Original { skew_angle(&even_light(&page)) } else { skew_angle(&page) };
        if angle != 0.0 {
            page = rotate(&page, angle, (o.mode != Mode::Original).then_some(255));
        }
    }
    let gray = |img: Image| Image { width: img.width, height: img.height, channels: 1, data: img.luma() };
    let (image, bilevel) = match (o.mode, o.handwriting) {
        (Mode::Original, _) => (page, false),
        (Mode::Color, false) => (sharpen(&plain_levels(page)), false),
        (Mode::Gray, false) => (sharpen(&plain_levels(gray(page))), false),
        (Mode::BlackWhite, false) => (plain_threshold(&page), true),
        (Mode::Color, true) => (sharpen(&levels(page, false)), false),
        (Mode::Gray, true) => (sharpen(&gray(levels(page, false))), false),
        (Mode::BlackWhite, true) => (threshold(&page), true),
    };
    Scanned { image: image.turned(o.turns), bilevel }
}

fn side_lengths(q: &Quad) -> [f32; 4] {
    std::array::from_fn(|j| {
        let (a, b) = (q[j], q[(j + 1) % 4]);
        (b[0] - a[0]).hypot(b[1] - a[1])
    })
}

/// The size of the flattened page: as long as the quadrilateral's longer
/// sides, snapped to the proportions of A4, Letter or Legal when close
/// (perspective makes a page look a few percent off), and at most
/// `max_size` on the longer side.
pub(super) fn output_size(q: &Quad, max_size: u32) -> (u32, u32) {
    let s = side_lengths(q);
    let (mut w, mut h) = (s[0].max(s[2]).max(1.0), s[1].max(s[3]).max(1.0));
    let long = w.max(h);
    let aspect = long / w.min(h);
    // A quadrilateral with right angles (the whole photo, a flat scan) has
    // its true proportions already.
    let snapped = if is_rectangle(q) {
        aspect
    } else {
        [841.89 / 595.28, 792.0 / 612.0, 1008.0 / 612.0]
            .into_iter()
            .find(|paper| (aspect / paper - 1.0).abs() < 0.05)
            .unwrap_or(aspect)
    };
    let long = long.min(max_size as f32);
    if w >= h {
        (w, h) = (long, long / snapped);
    } else {
        (w, h) = (long / snapped, long);
    }
    (w.round().max(1.0) as u32, h.round().max(1.0) as u32)
}

/// Whether every corner of `q` is a right angle (within about a degree).
fn is_rectangle(q: &Quad) -> bool {
    (0..4).all(|j| {
        let (a, b, c) = (q[(j + 3) % 4], q[j], q[(j + 1) % 4]);
        let (u, v) = ([a[0] - b[0], a[1] - b[1]], [c[0] - b[0], c[1] - b[1]]);
        let cos = (u[0] * v[0] + u[1] * v[1]) / (u[0].hypot(u[1]) * v[0].hypot(v[1])).max(1e-6);
        cos.abs() < 0.02
    })
}

/// The projective map from the unit square to `q` (Heckbert, "Fundamentals
/// of Texture Mapping", 1989): (0,0) → q[0], (1,0) → q[1], (1,1) → q[2],
/// (0,1) → q[3].
fn square_to_quad(q: &Quad) -> [f64; 8] {
    let [x0, y0] = q[0].map(f64::from);
    let [x1, y1] = q[1].map(f64::from);
    let [x2, y2] = q[2].map(f64::from);
    let [x3, y3] = q[3].map(f64::from);
    let (sx, sy) = (x0 - x1 + x2 - x3, y0 - y1 + y2 - y3);
    if sx.abs() < 1e-9 && sy.abs() < 1e-9 {
        return [x1 - x0, x3 - x0, x0, y1 - y0, y3 - y0, y0, 0.0, 0.0];
    }
    let (dx1, dx2, dy1, dy2) = (x1 - x2, x3 - x2, y1 - y2, y3 - y2);
    let den = dx1 * dy2 - dx2 * dy1;
    let g = (sx * dy2 - dx2 * sy) / den;
    let h = (dx1 * sy - sx * dy1) / den;
    [x1 - x0 + g * x1, x3 - x0 + h * x3, x0, y1 - y0 + g * y1, y3 - y0 + h * y3, y0, g, h]
}

/// The page at `q` in `src`, flattened to `w`×`h` pixels.
pub(super) fn warp(src: &Image, q: &Quad, w: u32, h: u32) -> Image {
    let [a, b, c, d, e, f, g, hh] = square_to_quad(q);
    let mut out = Image::new(w, h, src.channels);
    let ch = src.channels as usize;
    for y in 0..h as usize {
        let v = (y as f64 + 0.5) / h as f64;
        for x in 0..w as usize {
            let u = (x as f64 + 0.5) / w as f64;
            let z = g * u + hh * v + 1.0;
            let sx = (a * u + b * v + c) / z;
            let sy = (d * u + e * v + f) / z;
            let o = (y * w as usize + x) * ch;
            sample(src, sx as f32 - 0.5, sy as f32 - 0.5, &mut out.data[o..o + ch], None);
        }
    }
    out
}

/// Bilinear sample at (x, y) (pixel centers at whole numbers). Outside the
/// image: `fill`, or the nearest edge pixel.
fn sample(src: &Image, x: f32, y: f32, out: &mut [u8], fill: Option<u8>) {
    let (w, h) = (src.width as i32, src.height as i32);
    if let Some(v) = fill
        && (x < -0.5 || y < -0.5 || x > w as f32 - 0.5 || y > h as f32 - 0.5)
    {
        out.fill(v);
        return;
    }
    let x = x.clamp(0.0, (w - 1) as f32);
    let y = y.clamp(0.0, (h - 1) as f32);
    let (x0, y0) = (x as i32, y as i32);
    let (x1, y1) = ((x0 + 1).min(w - 1), (y0 + 1).min(h - 1));
    let (fx, fy) = (x - x0 as f32, y - y0 as f32);
    let ch = src.channels as usize;
    let at = |x: i32, y: i32| (y as usize * w as usize + x as usize) * ch;
    let (p00, p10, p01, p11) = (at(x0, y0), at(x1, y0), at(x0, y1), at(x1, y1));
    for (i, o) in out.iter_mut().enumerate() {
        let top = src.data[p00 + i] as f32 * (1.0 - fx) + src.data[p10 + i] as f32 * fx;
        let bottom = src.data[p01 + i] as f32 * (1.0 - fx) + src.data[p11 + i] as f32 * fx;
        *o = (top * (1.0 - fy) + bottom * fy + 0.5) as u8;
    }
}

/// Divides away uneven light and color casts so the paper becomes white.
///
/// The paper's color is measured in a grid of cells (the brightest fifth
/// of each cell is taken to be paper). Cells much darker than the paper
/// elsewhere are covered by a picture or a dark area; they get the paper
/// color of their neighbors, so pictures keep their own colors. The grid
/// is smoothed and each pixel divided by the paper color at its place.
pub(super) fn even_light(img: &Image) -> Image {
    let (w, h, ch) = (img.width as usize, img.height as usize, img.channels as usize);
    let cell = (w.max(h) / 40).max(8);
    let (gw, gh) = (w.div_ceil(cell), h.div_ceil(cell));
    let luma = img.luma();
    let mut grid = vec![[0f32; 3]; gw * gh];
    let mut level = vec![0f32; gw * gh];
    let step = if cell >= 24 { 2 } else { 1 };
    for gy in 0..gh {
        for gx in 0..gw {
            let mut hist = [0u32; 256];
            let (x0, y0) = (gx * cell, gy * cell);
            let (x1, y1) = ((x0 + cell).min(w), (y0 + cell).min(h));
            for y in (y0..y1).step_by(step) {
                for x in (x0..x1).step_by(step) {
                    hist[luma[y * w + x] as usize] += 1;
                }
            }
            let count: u32 = hist.iter().sum();
            // The 80th percentile of brightness.
            let mut seen = 0;
            let cut = hist
                .iter()
                .position(|&c| {
                    seen += c;
                    seen * 5 >= count * 4
                })
                .unwrap_or(255) as u8;
            let mut sum = [0f32; 3];
            let mut n = 0f32;
            for y in (y0..y1).step_by(step) {
                for x in (x0..x1).step_by(step) {
                    if luma[y * w + x] >= cut {
                        let p = &img.data[(y * w + x) * ch..][..ch];
                        for c in 0..3 {
                            sum[c] += p[c.min(ch - 1)] as f32;
                        }
                        n += 1.0;
                    }
                }
            }
            let i = gy * gw + gx;
            grid[i] = sum.map(|s| s / n.max(1.0));
            level[i] = (grid[i][0] * 0.299 + grid[i][1] * 0.587 + grid[i][2] * 0.114).max(1.0);
        }
    }
    // Cells covered by pictures: far darker than the paper is in most cells.
    let mut sorted = level.clone();
    sorted.sort_by(f32::total_cmp);
    let paper = sorted[sorted.len() * 3 / 4];
    let mut known: Vec<bool> = level.iter().map(|&l| l >= 0.45 * paper).collect();
    if known.iter().any(|&k| k) {
        loop {
            let mut filled = Vec::new();
            for gy in 0..gh {
                for gx in 0..gw {
                    if known[gy * gw + gx] {
                        continue;
                    }
                    let mut sum = [0f32; 3];
                    let mut n = 0.0;
                    for (nx, ny) in neighbors(gx, gy, gw, gh) {
                        if known[ny * gw + nx] {
                            let g = grid[ny * gw + nx];
                            (0..3).for_each(|c| sum[c] += g[c]);
                            n += 1.0;
                        }
                    }
                    if n > 0.0 {
                        filled.push((gy * gw + gx, sum.map(|s| s / n)));
                    }
                }
            }
            if filled.is_empty() {
                break;
            }
            for (i, g) in filled {
                grid[i] = g;
                known[i] = true;
            }
        }
    }
    // Smooth the grid (twice 3×3). Beyond its edges the grid is extended
    // linearly, so light that falls off towards an edge stays measured
    // right there.
    for _ in 0..2 {
        for (len, count, stride, step) in [(gw, gh, gw, 1), (gh, gw, 1, gw)] {
            let prev = grid.clone();
            for line in 0..count {
                let at = |i: usize| prev[line * stride + i * step];
                for i in 0..len {
                    let mid = at(i);
                    let before = if i > 0 { at(i - 1) } else if len > 1 { std::array::from_fn(|c| 2.0 * mid[c] - at(1)[c]) } else { mid };
                    let after = if i + 1 < len { at(i + 1) } else if len > 1 { std::array::from_fn(|c| 2.0 * mid[c] - at(i - 1)[c]) } else { mid };
                    grid[line * stride + i * step] = std::array::from_fn(|c| (before[c] + mid[c] + after[c]) / 3.0);
                }
            }
        }
    }
    // Divide each pixel by the paper color there (bilinear between cell
    // centers).
    let mut out = img.clone();
    let mut gains = vec![[0f32; 3]; w];
    for y in 0..h {
        let gyf = ((y as f32 + 0.5) / cell as f32 - 0.5).clamp(0.0, (gh - 1) as f32);
        let (gy0, gy1) = (gyf as usize, (gyf as usize + 1).min(gh - 1));
        let fy = gyf - gy0 as f32;
        for (x, gain) in gains.iter_mut().enumerate() {
            let gxf = ((x as f32 + 0.5) / cell as f32 - 0.5).clamp(0.0, (gw - 1) as f32);
            let (gx0, gx1) = (gxf as usize, (gxf as usize + 1).min(gw - 1));
            let fx = gxf - gx0 as f32;
            for c in 0..3 {
                let top = grid[gy0 * gw + gx0][c] * (1.0 - fx) + grid[gy0 * gw + gx1][c] * fx;
                let bottom = grid[gy1 * gw + gx0][c] * (1.0 - fx) + grid[gy1 * gw + gx1][c] * fx;
                gain[c] = 255.0 / (top * (1.0 - fy) + bottom * fy).max(16.0);
            }
        }
        let row = &mut out.data[y * w * ch..][..w * ch];
        for (x, p) in row.chunks_exact_mut(ch).enumerate() {
            for (c, v) in p.iter_mut().enumerate() {
                *v = (*v as f32 * gains[x][if ch == 1 { 1 } else { c }]).min(255.0) as u8;
            }
        }
    }
    out
}

fn neighbors(x: usize, y: usize, w: usize, h: usize) -> impl Iterator<Item = (usize, usize)> {
    (-1i32..=1).flat_map(move |dy| (-1i32..=1).map(move |dx| (dx, dy))).filter_map(move |(dx, dy)| {
        let (nx, ny) = (x as i32 + dx, y as i32 + dy);
        ((dx, dy) != (0, 0) && nx >= 0 && ny >= 0 && nx < w as i32 && ny < h as i32).then_some((nx as usize, ny as usize))
    })
}

/// Stretches contrast: the faint paper texture left after evening the
/// light turns white, the darkest ink black, and the middle tones a little
/// darker so thin strokes stay readable.
fn plain_levels(mut img: Image) -> Image {
    let luma = img.luma();
    let mut hist = [0u32; 256];
    luma.iter().step_by(7).for_each(|&v| hist[v as usize] += 1);
    let total: u32 = hist.iter().sum();
    let mut seen = 0;
    let black = hist
        .iter()
        .position(|&c| {
            seen += c;
            seen * 200 >= total
        })
        .unwrap_or(0)
        .min(110) as f32;
    let white = 232.0;
    let table: [u8; 256] = std::array::from_fn(|v| {
        let t = ((v as f32 - black) / (white - black)).clamp(0.0, 1.0);
        (t.powf(1.25) * 255.0 + 0.5) as u8
    });
    img.data.iter_mut().for_each(|v| *v = table[*v as usize]);
    img
}

/// Black and white: ink is what's clearly darker than the (now white)
/// paper.
fn plain_threshold(img: &Image) -> Image {
    let luma = img.luma();
    let values: Vec<f32> = luma.iter().step_by(5).map(|&v| v as f32).collect();
    let t = otsu(&values).clamp(130.0, 210.0) as u8;
    Image { width: img.width, height: img.height, channels: 1, data: luma.iter().map(|&v| if v < t { 0 } else { 255 }).collect() }
}

/// The paper's brightness and its noise (after the light is evened out):
/// the median of the bright pixels, and how much they vary (from the
/// median absolute deviation).
fn paper_stats(luma: &[u8]) -> (f32, f32) {
    let mut hist = [0u32; 256];
    luma.iter().step_by(3).for_each(|&v| hist[v as usize] += 1);
    let total: u32 = hist.iter().sum();
    // The paper is most of the page: the median of the brighter 70%.
    let percentile = |from: usize, share: f32| {
        let count: u32 = hist[from..].iter().sum();
        let mut seen = 0;
        hist[from..]
            .iter()
            .position(|&c| {
                seen += c;
                seen as f32 >= count as f32 * share
            })
            .map_or(255, |i| i + from)
    };
    let mut seen = 0;
    let dark_30 = hist.iter().position(|&c| {
        seen += c;
        seen * 10 >= total * 3
    });
    let paper = percentile(dark_30.unwrap_or(0), 0.5) as f32;
    // Spread around it, from the pixels near the paper level.
    let mut dev = [0u32; 64];
    for (v, &c) in hist.iter().enumerate() {
        let d = (v as f32 - paper).abs() as usize;
        if d < 64 && v as f32 >= paper - 40.0 {
            dev[d] += c;
        }
    }
    let n: u32 = dev.iter().sum();
    let mut seen = 0;
    let mad = dev.iter().position(|&c| {
        seen += c;
        seen * 2 >= n
    });
    (paper, (mad.unwrap_or(2) as f32 * 1.48).max(1.0))
}

/// For each pixel, about how dark the darkest ink near it is: the minimum
/// of blocks `r` pixels wide, taken over the 3×3 blocks around, smoothed
/// and read back bilinearly. Returned as a function of (x, y).
fn local_ink(luma: &[u8], w: usize, h: usize, r: usize) -> impl Fn(usize, usize) -> f32 {
    let (gw, gh) = (w.div_ceil(r), h.div_ceil(r));
    let mut blocks = vec![255u8; gw * gh];
    for y in 0..h {
        let row = &luma[y * w..(y + 1) * w];
        for (x, &v) in row.iter().enumerate() {
            let b = &mut blocks[(y / r) * gw + x / r];
            *b = (*b).min(v);
        }
    }
    let mut grid = vec![0f32; gw * gh];
    for gy in 0..gh {
        for gx in 0..gw {
            let mut m = blocks[gy * gw + gx];
            for (nx, ny) in neighbors(gx, gy, gw, gh) {
                m = m.min(blocks[ny * gw + nx]);
            }
            grid[gy * gw + gx] = m as f32;
        }
    }
    // Smooth, so the strengthening doesn't change in steps.
    let prev = grid.clone();
    for gy in 0..gh {
        for gx in 0..gw {
            let (mut sum, mut n) = (prev[gy * gw + gx], 1.0);
            for (nx, ny) in neighbors(gx, gy, gw, gh) {
                sum += prev[ny * gw + nx];
                n += 1.0;
            }
            grid[gy * gw + gx] = sum / n;
        }
    }
    move |x: usize, y: usize| {
        let gxf = ((x as f32 + 0.5) / r as f32 - 0.5).clamp(0.0, (gw - 1) as f32);
        let gyf = ((y as f32 + 0.5) / r as f32 - 0.5).clamp(0.0, (gh - 1) as f32);
        let (gx0, gy0) = (gxf as usize, gyf as usize);
        let (gx1, gy1) = ((gx0 + 1).min(gw - 1), (gy0 + 1).min(gh - 1));
        let (fx, fy) = (gxf - gx0 as f32, gyf - gy0 as f32);
        let top = grid[gy0 * gw + gx0] * (1.0 - fx) + grid[gy0 * gw + gx1] * fx;
        let bottom = grid[gy1 * gw + gx0] * (1.0 - fx) + grid[gy1 * gw + gx1] * fx;
        top * (1.0 - fy) + bottom * fy
    }
}

/// Makes handwriting clear and the paper white (after the light is evened
/// out).
///
/// What is brighter than the paper's noise turns white (the white point
/// follows the paper measured, not a fixed level). Ink is stretched from
/// there to black, with the middle tones darker. Faint ink (light
/// pressure, pencil, a pale pen) is strengthened where it is: around each
/// place, the darkest ink nearby is brought towards the page's strong ink,
/// up to four times as dark, so faint strokes come out like the others.
/// Where there is no ink nearby the paper is left alone, so its noise
/// isn't strengthened. A notebook's ruled lines and margin aren't
/// strengthened either, nor count as ink nearby: they are the paper's
/// printing (or, with `drop_printing`, go white).
/// Colors keep their hue: the ink of a blue pen gets deeper, not gray.
fn levels(mut img: Image, drop_printing: bool) -> Image {
    let (w, h, ch) = (img.width as usize, img.height as usize, img.channels as usize);
    let luma = img.luma();
    let mut pale = vec![false; w * h];
    let (paper, noise) = paper_stats(&luma);
    let white = (paper - 2.5 * noise - 2.0).clamp(150.0, 250.0);
    // Faint marks in long straight lines along the rows or columns (the
    // page is straightened by now) are ruled lines, even where scanning
    // blurred their color away. (Ink crossing a line doesn't break it.)
    let marked = |i: usize| white - luma[i] as f32 > 4.0 * noise + 4.0;
    let faint = |i: usize| white - (luma[i] as f32) < 110.0;
    mark_ruled_lines(&mut pale, w, h, marked, faint);
    let ink_luma: Vec<u8> = luma.iter().zip(&pale).map(|(&v, &p)| if p { 255 } else { v }).collect();
    // Strong ink: the darker half of what is clearly ink (not the paper's
    // noise, nor the soft edges of strokes). Without much ink, black.
    let clearly = white - (8.0 * noise).max(40.0);
    let mut hist = [0u32; 256];
    luma.iter().step_by(3).filter(|&&v| (v as f32) < clearly).for_each(|&v| hist[v as usize] += 1);
    let ink: u32 = hist.iter().sum();
    let mut seen = 0;
    let strong = if (ink as usize) * 3 * 2000 < w * h {
        0.0
    } else {
        hist.iter()
            .position(|&c| {
                seen += c;
                seen * 2 >= ink
            })
            .map_or(0.0, |v| v as f32)
            .min(white - 100.0)
            .max(0.0)
    };
    let span = (white - strong).max(1.0);
    let near = local_ink(&ink_luma, w, h, (w.max(h) / 250).max(4));
    // Contrast below this near a place is the paper's noise, not ink.
    let real_ink = 8.0 * noise + 6.0;
    for y in 0..h {
        for x in 0..w {
            let i = y * w + x;
            let l = luma[i] as f32;
            let out = if l >= white || (drop_printing && pale[i]) {
                255.0
            } else {
                let contrast = white - near(x, y);
                let t = if pale[i] {
                    // The paper's printing: as light as it was (measured
                    // against black, not against faint ink).
                    1.0 - (white - l) / white
                } else {
                    let gain = if contrast > real_ink { (span / contrast).clamp(1.0, 4.0) } else { 1.0 };
                    1.0 - (white - l) * gain / span
                }
                .clamp(0.0, 1.0);
                255.0 * t.powf(1.4)
            };
            let p = &mut img.data[i * ch..(i + 1) * ch];
            if ch == 1 {
                p[0] = out as u8;
            } else {
                // The darkness scaled by the same amount in each channel.
                let k = (255.0 - out) / (255.0 - l).max(1.0);
                for v in p.iter_mut() {
                    *v = (255.0 - (255.0 - *v as f32) * k).clamp(0.0, 255.0) as u8;
                }
            }
        }
    }
    img
}

/// Unsharp masking with a 3×3 box: crisper edges of letters.
fn sharpen(img: &Image) -> Image {
    let (w, h, ch) = (img.width as usize, img.height as usize, img.channels as usize);
    let mut out = img.clone();
    if w < 3 || h < 3 {
        return out;
    }
    for y in 1..h - 1 {
        for x in 1..w - 1 {
            for c in 0..ch {
                let at = |x: usize, y: usize| img.data[(y * w + x) * ch + c] as i32;
                let mut sum = 0;
                for dy in 0..3 {
                    for dx in 0..3 {
                        sum += at(x + dx - 1, y + dy - 1);
                    }
                }
                let v = at(x, y);
                let sharp = v + (v - sum / 9) * 6 / 10;
                out.data[(y * w + x) * ch + c] = sharp.clamp(0, 255) as u8;
            }
        }
    }
    out
}

/// Marks (in `lines`) the `faint` pixels of ruled lines: lines of
/// `marked` pixels that cover over a third of the page's width (or, for
/// columns, its height). Handwriting covers far less of any line. The
/// lines may still slope a little on a straightened page: the slope is
/// measured first (the one at which the marked pixels pile up in the
/// fewest lines), and lines are followed along it.
fn mark_ruled_lines(lines: &mut [bool], w: usize, h: usize, marked: impl Fn(usize) -> bool, faint: impl Fn(usize) -> bool) {
    let on: Vec<bool> = (0..w * h).map(&marked).collect();
    for (len, count, at) in [
        (w, h, &(|line: usize, i: usize| line * w + i) as &dyn Fn(usize, usize) -> usize),
        (h, w, &(|line: usize, i: usize| i * w + line)),
    ] {
        // The slope, in lines per step along, up to 1° either way.
        let max = 0.0175;
        let points: Vec<(usize, usize)> =
            (0..count).step_by(2).flat_map(|line| (0..len).step_by(3).map(move |i| (line, i))).filter(|&(l, i)| on[at(l, i)]).collect();
        if points.is_empty() {
            continue;
        }
        let mut best = (0.0f32, 0.0f64);
        let mut piles = vec![0u32; count + 2 * (max * len as f32) as usize + 4];
        let offset = max * len as f32 + 2.0;
        for k in -20i32..=20 {
            let slope = max * k as f32 / 20.0;
            piles.fill(0);
            for &(l, i) in &points {
                let r = (l as f32 - slope * i as f32 + offset).round() as usize;
                let last = piles.len() - 1;
                piles[r.min(last)] += 1;
            }
            let score: f64 = piles.iter().map(|&c| (c as f64).powi(2)).sum();
            if score > best.1 {
                best = (slope, score);
            }
        }
        let slope = best.0;
        let row = |l: usize, i: usize| (l as f32 - slope * i as f32 + offset).round() as usize;
        // How much of each sloped line is covered (within a line either
        // side), each column counted once.
        let rows = count + 2 * offset as usize + 2;
        let mut covered = vec![0u32; rows];
        let mut last = vec![usize::MAX; rows];
        for i in 0..len {
            for l in 0..count {
                if on[at(l, i)] {
                    let r = row(l, i);
                    for r in r.saturating_sub(1)..=(r + 1).min(rows - 1) {
                        if last[r] != i {
                            last[r] = i;
                            covered[r] += 1;
                        }
                    }
                }
            }
        }
        let ruled: Vec<bool> = covered.iter().map(|&c| c as usize * 3 > len).collect();
        for l in 0..count {
            for i in 0..len {
                let p = at(l, i);
                let r = row(l, i);
                if on[p] && faint(p) && (r.saturating_sub(2)..=(r + 2).min(rows - 1)).any(|r| ruled[r]) {
                    lines[p] = true;
                }
            }
        }
    }
}

/// Black and white for handwriting, for the ink as [`levels`] shows it
/// (without the paper's ruled lines): pixels clearly
/// dark are ink, and lighter ones are ink too where they join those
/// (hysteresis), so faint strokes stay whole without specks of noise.
/// Large dark areas along the edges (the scanner's lid, a shadow) are
/// dropped.
fn threshold(img: &Image) -> Image {
    let (w, h) = (img.width as usize, img.height as usize);
    // (Black and white can't show pale ruled lines: they go.)
    let toned = levels(img.clone(), true).luma();
    const STRONG: u8 = 110;
    const WEAK: u8 = 185;
    let mut out = vec![255u8; w * h];
    let mut stack: Vec<usize> = Vec::new();
    for i in 0..w * h {
        if toned[i] < STRONG && out[i] == 255 {
            out[i] = 0;
            stack.push(i);
            while let Some(j) = stack.pop() {
                let (x, y) = (j % w, j / w);
                for (nx, ny) in neighbors(x, y, w, h) {
                    let k = ny * w + nx;
                    if out[k] == 255 && toned[k] < WEAK {
                        out[k] = 0;
                        stack.push(k);
                    }
                }
            }
        }
    }
    drop_edge_shadows(&mut out, w, h);
    Image { width: img.width, height: img.height, channels: 1, data: out }
}

/// Turns white the black areas that touch the page's edge and are far
/// larger than any letter (dark borders left by scanners and shadows).
fn drop_edge_shadows(bw: &mut [u8], w: usize, h: usize) {
    let limit = (w + h) * 4;
    let mut seen = vec![false; w * h];
    let edge = (0..w).flat_map(|x| [x, (h - 1) * w + x]).chain((0..h).flat_map(|y| [y * w, y * w + w - 1]));
    for start in edge.collect::<Vec<_>>() {
        if bw[start] != 0 || seen[start] {
            continue;
        }
        let mut area = vec![start];
        seen[start] = true;
        let mut i = 0;
        while i < area.len() {
            let (x, y) = (area[i] % w, area[i] / w);
            for (nx, ny) in neighbors(x, y, w, h) {
                let k = ny * w + nx;
                if bw[k] == 0 && !seen[k] {
                    seen[k] = true;
                    area.push(k);
                }
            }
            i += 1;
        }
        if area.len() > limit {
            area.into_iter().for_each(|k| bw[k] = 255);
        }
    }
}

/// How far the lines of text slope, in radians (positive: they go down to
/// the right), or 0 when there's no clear text or the slope is tiny.
///
/// Text lines are found by projection: dark pixels rotated by the right
/// angle pile up in few rows, which makes the sum of squared row counts
/// largest.
pub(super) fn skew_angle(img: &Image) -> f32 {
    let luma = img.luma();
    let (w, h) = (img.width as usize, img.height as usize);
    let step = w.max(h).div_ceil(1000).max(1);
    let (mx, my) = (w / 25, h / 25);
    // Ink: darker than 110, or, on a page of faint writing (pencil, a pale
    // pen), the darkest twenty-fifth of the page, as long as that is
    // clearly darker than the paper.
    let mut hist = [0u32; 256];
    luma.iter().step_by(7).for_each(|&v| hist[v as usize] += 1);
    let total: u32 = hist.iter().sum();
    let mut seen = 0;
    let darkest = hist.iter().position(|&c| {
        seen += c;
        seen * 25 >= total
    });
    let (paper, _) = paper_stats(&luma);
    let cut = (darkest.unwrap_or(0) as f32).min(paper - 30.0).max(110.0) as u8;
    let mut ink = Vec::new();
    for y in (my..h - my).step_by(step) {
        for x in (mx..w - mx).step_by(step) {
            if luma[y * w + x] < cut {
                ink.push([(x / step) as f32, (y / step) as f32]);
            }
        }
    }
    if ink.len() < 300 {
        return 0.0;
    }
    if ink.len() > 120_000 {
        let keep = ink.len().div_ceil(120_000);
        ink = ink.into_iter().step_by(keep).collect();
    }
    let rows = (w + h) / step + 4;
    let mut bins = vec![0u32; rows * 2];
    let mut score = |a: f32| -> f64 {
        bins.fill(0);
        let (s, c) = a.sin_cos();
        for p in &ink {
            let r = (p[1] * c - p[0] * s) as i64 + rows as i64;
            if let Some(b) = bins.get_mut(r.max(0) as usize) {
                *b += 1;
            }
        }
        bins.iter().map(|&b| (b as f64) * (b as f64)).sum()
    };
    let deg = std::f32::consts::PI / 180.0;
    let mut best = (0.0, score(0.0));
    let flat = best.1;
    let mut a = -8.0;
    while a <= 8.0 {
        let s = score(a * deg);
        if s > best.1 {
            best = (a, s);
        }
        a += 0.5;
    }
    let coarse = best.0;
    let mut a = coarse - 0.5;
    while a <= coarse + 0.5 {
        let s = score(a * deg);
        if s > best.1 {
            best = (a, s);
        }
        a += 0.05;
    }
    if best.0.abs() < 0.1 || best.1 < flat * 1.02 { 0.0 } else { best.0 * deg }
}

/// Turns the image by -`angle` about its center, so lines sloping by
/// `angle` become level.
fn rotate(img: &Image, angle: f32, fill: Option<u8>) -> Image {
    let (w, h, ch) = (img.width as usize, img.height as usize, img.channels as usize);
    let (s, c) = angle.sin_cos();
    let (cx, cy) = (w as f32 / 2.0, h as f32 / 2.0);
    let mut out = img.clone();
    for y in 0..h {
        for x in 0..w {
            let (dx, dy) = (x as f32 + 0.5 - cx, y as f32 + 0.5 - cy);
            let sx = dx * c - dy * s + cx - 0.5;
            let sy = dx * s + dy * c + cy - 0.5;
            let o = (y * w + x) * ch;
            sample(img, sx, sy, &mut out.data[o..o + ch], fill);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// White page with dark text lines sloping by `deg` degrees.
    fn lines(deg: f32) -> Image {
        let (w, h) = (800u32, 600u32);
        let t = deg.to_radians().tan();
        let mut img = Image::new(w, h, 1);
        for y in 0..h {
            for x in 0..w {
                let base = y as f32 - x as f32 * t;
                let on = (base as i32).rem_euclid(30) < 6 && (x / 9) % 6 != 0 && (60..740).contains(&x);
                img.data[(y * w + x) as usize] = if on { 20 } else { 245 };
            }
        }
        img
    }

    #[test]
    fn finds_and_removes_slope() {
        for deg in [-4.0f32, 2.5, 0.0] {
            let angle = skew_angle(&lines(deg)).to_degrees();
            assert!((angle - deg).abs() < 0.2, "{deg}: found {angle}");
        }
        let fixed = rotate(&lines(3.0), 3.0f32.to_radians(), Some(255));
        assert!(skew_angle(&fixed).to_degrees().abs() < 0.2);
    }

    #[test]
    fn warp_maps_corners() {
        // A gradient image; the quad is the right half.
        let mut img = Image::new(200, 100, 1);
        for y in 0..100 {
            for x in 0..200 {
                img.data[y * 200 + x] = x as u8;
            }
        }
        let out = warp(&img, &[[100.0, 0.0], [200.0, 0.0], [200.0, 100.0], [100.0, 100.0]], 50, 50);
        assert!(out.data[0].abs_diff(101) <= 1, "{}", out.data[0]);
        assert!(out.data[49].abs_diff(198) <= 1, "{}", out.data[49]);
    }

    #[test]
    fn shadows_are_evened_out() {
        // Paper getting darker to the right, with a black square.
        let (w, h) = (400usize, 300usize);
        let mut img = Image::new(w as u32, h as u32, 1);
        for y in 0..h {
            for x in 0..w {
                let paper = 230.0 - 120.0 * x as f32 / w as f32;
                let ink = (150..170).contains(&y) && (x / 10) % 2 == 0;
                img.data[y * w + x] = if ink { (paper * 0.2) as u8 } else { paper as u8 };
            }
        }
        let even = even_light(&img);
        let (left, right) = (even.data[100 * w + 20], even.data[100 * w + 380]);
        assert!(left > 245 && right > 245, "{left} {right}");
        assert!(even.data[160 * w + 380] < 80);
    }

    #[test]
    fn handwriting_darkens_faint_ink_but_not_ruled_lines() {
        // White paper (with a little noise) with a ruled line across the
        // page and short faint pencil strokes under it.
        let (w, h) = (600usize, 400usize);
        let mut img = Image::new(w as u32, h as u32, 1);
        for (i, v) in img.data.iter_mut().enumerate() {
            *v = 250 - (i * 7919 % 5) as u8;
        }
        for x in 0..w {
            img.data[100 * w + x] = 225; // ruled line
        }
        for k in 0..12 {
            for y in 200..230 {
                for x in 40 + 45 * k..43 + 45 * k {
                    img.data[y * w + x] = 205; // faint stroke
                }
            }
        }
        let plain = plain_levels(img.clone());
        let toned = levels(img.clone(), false);
        let at = |i: &Image, x: usize, y: usize| i.data[y * w + x];
        // Faint strokes come out much darker than before.
        assert!(at(&toned, 41, 215) < 80, "{}", at(&toned, 41, 215));
        assert!(at(&plain, 41, 215) > 120, "{}", at(&plain, 41, 215));
        // The ruled line stays light, and the paper white.
        assert!(at(&toned, 300, 100) > 150, "{}", at(&toned, 300, 100));
        assert_eq!(at(&toned, 300, 300), 255);
        // In black and white the strokes are whole and the line is gone.
        let bw = threshold(&img);
        assert_eq!(at(&bw, 41, 215), 0);
        assert_eq!(at(&bw, 300, 100), 255);
    }

    #[test]
    fn sizes_snap_to_paper() {
        // Seen in perspective: snapped to A4.
        let q: Quad = [[60.0, 0.0], [1000.0, 0.0], [1000.0, 1380.0], [0.0, 1380.0]];
        assert_eq!(output_size(&q, 5000), (977, 1381));
        assert_eq!(output_size(&q, 690), (488, 690));
        // A rectangle keeps its proportions.
        let r: Quad = [[0.0, 0.0], [300.0, 0.0], [300.0, 400.0], [0.0, 400.0]];
        assert_eq!(output_size(&r, 5000), (300, 400));
    }
}
