//! Dark pages for reading at night: page colors with their lightness
//! turned around and their hue kept.
//!
//! White paper becomes a soft dark gray and black text a soft light gray
//! (not pure black and white, which glare); colors keep their hue, a little
//! softened, so a light-blue box in a diagram becomes a dark-blue one and a
//! dark-red line a light-red one. The change is made in OKLab, where
//! lightness matches what the eye sees, so colors of equal lightness stay
//! equal and text keeps its contrast.
//!
//! Photos are left as they are (turned around, they look like negatives):
//! the renderer protects images that aren't mostly white background.

use std::sync::OnceLock;

/// The page background in dark mode (sRGB).
pub const PAPER: [u8; 3] = [37, 38, 41];
/// What black text becomes (sRGB).
pub const INK: [u8; 3] = [222, 223, 226];
/// How much of their color colors keep (softer on a dark background).
const CHROMA: f32 = 0.85;

struct Tables {
    to_linear: [f32; 256],
    /// PAPER and INK in OKLab.
    paper: [f32; 3],
    ink: [f32; 3],
}

fn tables() -> &'static Tables {
    static T: OnceLock<Tables> = OnceLock::new();
    T.get_or_init(|| {
        let mut to_linear = [0.0; 256];
        for (i, v) in to_linear.iter_mut().enumerate() {
            let c = i as f32 / 255.0;
            *v = if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) };
        }
        let lab = |c: [u8; 3]| oklab(c.map(|v| to_linear[v as usize]));
        Tables { to_linear, paper: lab(PAPER), ink: lab(INK) }
    })
}

fn oklab([r, g, b]: [f32; 3]) -> [f32; 3] {
    let l = (0.412_221_47 * r + 0.536_332_55 * g + 0.051_445_99 * b).cbrt();
    let m = (0.211_903_5 * r + 0.680_699_5 * g + 0.107_396_96 * b).cbrt();
    let s = (0.088_302_46 * r + 0.281_718_85 * g + 0.629_978_7 * b).cbrt();
    [
        0.210_454_26 * l + 0.793_617_8 * m - 0.004_072_047 * s,
        1.977_998_5 * l - 2.428_592_2 * m + 0.450_593_7 * s,
        0.025_904_037 * l + 0.782_771_77 * m - 0.808_675_77 * s,
    ]
}

fn linear_rgb([l, a, b]: [f32; 3]) -> [f32; 3] {
    let l_ = (l + 0.396_337_78 * a + 0.215_803_76 * b).powi(3);
    let m_ = (l - 0.105_561_346 * a - 0.063_854_17 * b).powi(3);
    let s_ = (l - 0.089_484_18 * a - 1.291_485_5 * b).powi(3);
    [
        4.076_741_7 * l_ - 3.307_711_6 * m_ + 0.230_969_94 * s_,
        -1.268_438 * l_ + 2.609_757_4 * m_ - 0.341_319_38 * s_,
        -0.004_196_086_3 * l_ - 0.703_418_6 * m_ + 1.707_614_7 * s_,
    ]
}

fn to_srgb(v: f32) -> u8 {
    let v = v.clamp(0.0, 1.0);
    let c = if v <= 0.003_130_8 { v * 12.92 } else { 1.055 * v.powf(1.0 / 2.4) - 0.055 };
    (c * 255.0).round() as u8
}

/// The dark-page version of a color.
pub fn dark_page_color(rgb: [u8; 3]) -> [u8; 3] {
    let t = tables();
    let [l, a, b] = oklab(rgb.map(|v| t.to_linear[v as usize]));
    let l = l.clamp(0.0, 1.0);
    // White goes to the paper and black to the ink, with their slight
    // tints; everything between in proportion.
    let (paper, ink) = (t.paper, t.ink);
    let new_l = ink[0] - (ink[0] - paper[0]) * l;
    let tint = [paper[1] * l + ink[1] * (1.0 - l), paper[2] * l + ink[2] * (1.0 - l)];
    // Keep the hue; reduce chroma until the color fits in sRGB.
    let mut k = CHROMA;
    for _ in 0..8 {
        let c = linear_rgb([new_l, a * k + tint[0], b * k + tint[1]]);
        if c.iter().all(|&v| (-0.001..=1.001).contains(&v)) {
            return c.map(to_srgb);
        }
        k *= 0.7;
    }
    linear_rgb([new_l, tint[0], tint[1]]).map(to_srgb)
}

/// A pixel rectangle `[x0, y0, x1, y1)` left as it is (a photo).
pub(crate) type Keep = [u32; 4];

/// Turns an opaque RGBA page into a dark page, except inside `keep`.
pub(crate) fn darken(pixels: &mut [u8], width: u32, keep: &[Keep]) {
    // Pages use few distinct colors (text, antialiasing levels, a few
    // fills): remember recent results in a small direct-mapped table.
    const SLOTS: usize = 1 << 14;
    let mut cache = vec![(u32::MAX, [0u8; 3]); SLOTS];
    let w = width as usize;
    for (y, row) in pixels.chunks_exact_mut(w * 4).enumerate() {
        let spans: Vec<(usize, usize)> = keep
            .iter()
            .filter(|k| (k[1] as usize..k[3] as usize).contains(&y))
            .map(|k| (k[0] as usize, k[2] as usize))
            .collect();
        for (x, px) in row.chunks_exact_mut(4).enumerate() {
            if spans.iter().any(|&(a, b)| (a..b).contains(&x)) {
                continue;
            }
            let key = u32::from_le_bytes([px[0], px[1], px[2], 0]);
            let slot = (key.wrapping_mul(0x9E37_79B1) >> 18) as usize & (SLOTS - 1);
            let rgb = if cache[slot].0 == key {
                cache[slot].1
            } else {
                let c = dark_page_color([px[0], px[1], px[2]]);
                cache[slot] = (key, c);
                c
            };
            px[..3].copy_from_slice(&rgb);
        }
    }
}

/// Whether a decoded image (premultiplied RGBA) looks like a photo rather
/// than a drawing, chart, logo or scan: opaque, with little white
/// background. (Images with transparent parts are graphics: logos, badges,
/// icons; leaving their area alone would leave white around them.)
pub(crate) fn looks_like_photo(pixels: &[u8], width: u32, height: u32) -> bool {
    if width < 24 || height < 24 {
        return false; // icons, bullets, small marks: recolor
    }
    let count = (width * height) as usize;
    let step = (count / 20_000).max(1);
    let (mut seen, mut white, mut transparent) = (0u32, 0u32, 0u32);
    for px in pixels.chunks_exact(4).step_by(step) {
        seen += 1;
        if px[3] < 250 {
            transparent += 1;
        } else if px[0] > 225 && px[1] > 225 && px[2] > 225 {
            white += 1;
        }
    }
    seen > 0 && (transparent as f32) < 0.02 * seen as f32 && (white as f32) < 0.3 * seen as f32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paper_ink_and_hue() {
        assert_eq!(dark_page_color([255, 255, 255]), PAPER);
        assert_eq!(dark_page_color([0, 0, 0]), INK);
        // A light blue box turns dark blue (blue still strongest).
        let [r, g, b] = dark_page_color([200, 220, 255]);
        assert!(b > r && b > g && (r as u32 + g as u32 + b as u32) < 3 * 110, "{r} {g} {b}");
        // Pure red stays red.
        let [r, g, b] = dark_page_color([220, 30, 30]);
        assert!(r > 2 * g && r > 2 * b, "{r} {g} {b}");
        // Lightness order is turned around.
        let l = |c: [u8; 3]| c.iter().map(|&v| v as u32).sum::<u32>();
        assert!(l(dark_page_color([60, 60, 60])) > l(dark_page_color([180, 180, 180])));
    }
}
