//! Lifting a signature off a photo or scan: the ink kept, the paper made
//! transparent, so it can be put on a page ([`crate::edit::place_image`]).
//!
//! 1. The paper's color is measured around the area (the brightest pixels
//!    of each cell of a grid, smoothed between cells), so shadows and an
//!    uneven light across a photo don't count as ink.
//! 2. Each pixel's darkness against the paper there becomes its opacity:
//!    nothing up to a little above the paper's own grain, fully opaque from
//!    the darkness most of the strokes have, smoothly in between (so the
//!    strokes' edges stay smooth).
//! 3. Specks (dust, the paper's texture, JPEG noise) are dropped: the
//!    pieces of ink far smaller than the signature's strokes.
//! 4. The ink is given one color (its own average color, or black or blue)
//!    and the picture is trimmed to the ink.

use super::Image;

/// What color the lifted ink gets.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Ink {
    /// The pen's own color, a little deepened.
    #[default]
    Original,
    Black,
    Blue,
}

impl Ink {
    pub const ALL: [Ink; 3] = [Ink::Original, Ink::Black, Ink::Blue];

    pub fn label(self) -> &'static str {
        match self {
            Ink::Original => "As written",
            Ink::Black => "Black",
            Ink::Blue => "Blue",
        }
    }

    /// The pen's color for drawn and typed signatures (which have no
    /// color of their own: black).
    pub fn pen(self) -> [u8; 3] {
        match self {
            Ink::Blue => [22, 48, 150],
            Ink::Original | Ink::Black => [24, 24, 28],
        }
    }
}

/// A lifted signature: `rgba` has 4 bytes a pixel (alpha not
/// premultiplied), rows top to bottom.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Signature {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

/// Signatures are kept at most this many pixels on their longer side
/// (plenty for a signature printed a few inches wide).
const MAX_SIDE: u32 = 1600;

/// Lifts the signature in `area` (`[left, top, right, bottom]`, pixels) of
/// `image`. `None` when there is no ink in it.
pub fn lift_signature(image: &Image, area: [u32; 4], ink: Ink) -> Option<Signature> {
    let x0 = area[0].min(image.width);
    let y0 = area[1].min(image.height);
    let x1 = area[2].min(image.width);
    let y1 = area[3].min(image.height);
    if x1 < x0 + 4 || y1 < y0 + 4 {
        return None;
    }
    let crop = crop(image, x0, y0, x1 - x0, y1 - y0);
    // Big crops (a signature on a 12-megapixel photo) are shrunk first.
    let crop = if crop.width.max(crop.height) > MAX_SIDE { crop.shrink(MAX_SIDE).0 } else { crop };
    let (w, h) = (crop.width as usize, crop.height as usize);
    let rgb = crop.to_rgb().data;

    // 1. The paper.
    let paper = paper_colors(&rgb, w, h);
    let lum = |c: [f32; 3]| 0.299 * c[0] + 0.587 * c[1] + 0.114 * c[2];

    // 2. Darkness against the paper, and opacity from it.
    let darkness: Vec<f32> = (0..w * h)
        .map(|i| {
            let px = [rgb[i * 3] as f32, rgb[i * 3 + 1] as f32, rgb[i * 3 + 2] as f32];
            (1.0 - lum(px) / lum(paper[i]).max(1.0)).clamp(0.0, 1.0)
        })
        .collect();
    let mut sorted = darkness.clone();
    sorted.sort_by(f32::total_cmp);
    let quantile = |v: &[f32], q: f32| v[((v.len() - 1) as f32 * q) as usize];
    // The paper's grain: the median and spread of most pixels (the paper
    // is most of the area).
    let median = quantile(&sorted, 0.5);
    let mut spread: Vec<f32> = sorted.iter().map(|d| (d - median).abs()).collect();
    spread.sort_by(f32::total_cmp);
    let grain = quantile(&spread, 0.5) * 1.4826;
    let low = (median + 5.0 * grain).max(median + 0.06).min(0.5);
    let start = sorted.partition_point(|&d| d <= low);
    let strokes = &sorted[start..];
    if strokes.len() < 8 {
        return None;
    }
    // Most strokes are fully opaque: from 70% of their middling darkness.
    let high = (quantile(strokes, 0.5) * 0.7).max(low + 0.08);
    let mut alpha: Vec<f32> = darkness
        .iter()
        .map(|&d| {
            let t = ((d - low) / (high - low)).clamp(0.0, 1.0);
            t * t * (3.0 - 2.0 * t)
        })
        .collect();

    // 3. Specks.
    drop_specks(&mut alpha, w, h);

    // 4. The color, and trimming.
    let color = match ink {
        Ink::Black | Ink::Blue => ink.pen().map(f32::from),
        Ink::Original => {
            // The ink against its paper (which takes out a tinted light),
            // over the solid parts of the strokes.
            let (mut sum, mut weight) = ([0.0f32; 3], 0.0);
            for i in 0..w * h {
                let a = alpha[i];
                if a > 0.8 {
                    for c in 0..3 {
                        sum[c] += a * (rgb[i * 3 + c] as f32 / paper[i][c].max(1.0)).min(1.0);
                    }
                    weight += a;
                }
            }
            if weight == 0.0 {
                return None;
            }
            // Deepened a little: a photo's ink is lighter than the pen's.
            sum.map(|v| (v / weight * 255.0 * 0.8).min(255.0))
        }
    };
    let ink_at = |i: usize| alpha[i] > 0.02;
    let rows: Vec<usize> = (0..h).filter(|&y| (0..w).any(|x| ink_at(y * w + x))).collect();
    let cols: Vec<usize> = (0..w).filter(|&x| (0..h).any(|y| ink_at(y * w + x))).collect();
    let (&top, &bottom) = (rows.first()?, rows.last()?);
    let (&left, &right) = (cols.first()?, cols.last()?);
    // A pixel of room around the ink, for its smooth edges.
    let (left, top) = (left.saturating_sub(2), top.saturating_sub(2));
    let (right, bottom) = ((right + 2).min(w - 1), (bottom + 2).min(h - 1));
    let (sw, sh) = (right - left + 1, bottom - top + 1);
    let color = color.map(|v| v.round() as u8);
    let mut out = Vec::with_capacity(sw * sh * 4);
    for y in top..=bottom {
        for x in left..=right {
            out.extend_from_slice(&color);
            out.push((alpha[y * w + x] * 255.0).round() as u8);
        }
    }
    Some(Signature { width: sw as u32, height: sh as u32, rgba: out })
}

/// A signature drawn by hand (with a mouse, touchpad or pen): `strokes`
/// are the pointer's paths, in any unit, and `pen` the line's width in the
/// same unit. Each path is smoothed (quadratic curves through the middles
/// of its steps, so the pointer's sampling doesn't show as corners); a
/// path of one point is a dot. `None` when nothing was drawn.
#[cfg(feature = "render")]
pub fn drawn_signature(strokes: &[Vec<[f32; 2]>], pen: f32, ink: Ink) -> Option<Signature> {
    let points = strokes.iter().flatten();
    let (mut x0, mut y0, mut x1, mut y1) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
    for p in points {
        (x0, y0, x1, y1) = (x0.min(p[0]), y0.min(p[1]), x1.max(p[0]), y1.max(p[1]));
    }
    if x0 > x1 || pen.is_nan() || pen <= 0.0 {
        return None;
    }
    // Drawn a few times finer than it was on screen (it is printed finer
    // than a screen shows it), up to the usual size.
    let side = (x1 - x0).max(y1 - y0) + pen;
    let k = (MAX_SIDE as f32 / side).min(4.0);
    let margin = pen / 2.0 + 2.0 / k;
    let (w, h) = (((x1 - x0 + 2.0 * margin) * k).ceil() as u32, ((y1 - y0 + 2.0 * margin) * k).ceil() as u32);
    let mut pixmap = tiny_skia::Pixmap::new(w.max(1), h.max(1))?;
    let at = |p: &[f32; 2]| ((p[0] - x0 + margin) * k, (p[1] - y0 + margin) * k);
    let mut path = tiny_skia::PathBuilder::new();
    let mut dots = tiny_skia::PathBuilder::new();
    for stroke in strokes.iter().filter(|s| !s.is_empty()) {
        let (sx, sy) = at(&stroke[0]);
        if stroke.len() == 1 {
            dots.push_circle(sx, sy, pen * k / 2.0);
            continue;
        }
        path.move_to(sx, sy);
        for pair in stroke[1..].windows(2) {
            let (cx, cy) = at(&pair[0]);
            let (nx, ny) = at(&pair[1]);
            path.quad_to(cx, cy, (cx + nx) / 2.0, (cy + ny) / 2.0);
        }
        let (lx, ly) = at(stroke.last().expect("a point"));
        path.line_to(lx, ly);
    }
    let [r, g, b] = ink.pen();
    let mut paint = tiny_skia::Paint::default();
    paint.set_color_rgba8(r, g, b, 255);
    paint.anti_alias = true;
    let line = tiny_skia::Stroke { width: pen * k, line_cap: tiny_skia::LineCap::Round, line_join: tiny_skia::LineJoin::Round, ..Default::default() };
    if let Some(path) = path.finish() {
        pixmap.stroke_path(&path, &paint, &line, tiny_skia::Transform::identity(), None);
    }
    if let Some(dots) = dots.finish() {
        pixmap.fill_path(&dots, &paint, tiny_skia::FillRule::Winding, tiny_skia::Transform::identity(), None);
    }
    from_pixmap(&pixmap, ink)
}

/// A signature typed in a font (`font`: the font file, `index`: the face
/// in it), `em` pixels to the em. Letters are spaced by their advances and
/// the font's kerning; `None` when the font has none of the letters.
#[cfg(feature = "render")]
pub fn typed_signature(text: &str, font: &[u8], index: u32, em: f32, ink: Ink) -> Option<Signature> {
    let face = ttf_parser::Face::parse(font, index).ok()?;
    let k = em / face.units_per_em() as f32;
    let kern = |a: ttf_parser::GlyphId, b: ttf_parser::GlyphId| -> f32 {
        let Some(table) = face.tables().kern else { return 0.0 };
        table.subtables.into_iter().filter(|s| s.horizontal && !s.variable).find_map(|s| s.glyphs_kerning(a, b)).map_or(0.0, f32::from)
    };
    // Each letter's outline, placed along the line (font units, y up).
    let mut glyphs: Vec<tiny_skia::Path> = Vec::new();
    let (mut x, mut previous) = (0.0f32, None);
    for c in text.trim().chars() {
        let Some(gid) = face.glyph_index(c).or_else(|| face.glyph_index(' ')) else { continue };
        if let Some(p) = previous {
            x += kern(p, gid);
        }
        let mut b = Outline(tiny_skia::PathBuilder::new());
        if face.outline_glyph(gid, &mut b).is_some()
            && let Some(path) = b.0.finish()
            && let Some(path) = path.transform(tiny_skia::Transform::from_row(k, 0.0, 0.0, -k, x * k, 0.0))
        {
            glyphs.push(path);
        }
        x += face.glyph_hor_advance(gid).unwrap_or(0) as f32;
        previous = Some(gid);
    }
    // The ink's extent (a script's swashes reach past the advances).
    let mut bounds: Option<tiny_skia::Rect> = None;
    for g in &glyphs {
        let r = g.bounds();
        bounds = Some(match bounds {
            Some(b) => tiny_skia::Rect::from_ltrb(b.left().min(r.left()), b.top().min(r.top()), b.right().max(r.right()), b.bottom().max(r.bottom()))?,
            None => r,
        });
    }
    let bounds = bounds?;
    let margin = 2.0;
    let (w, h) = ((bounds.width() + 2.0 * margin).ceil() as u32, (bounds.height() + 2.0 * margin).ceil() as u32);
    if w.max(h) > 8 * MAX_SIDE {
        return None;
    }
    let mut pixmap = tiny_skia::Pixmap::new(w, h)?;
    let [r, g, b] = ink.pen();
    let mut paint = tiny_skia::Paint::default();
    paint.set_color_rgba8(r, g, b, 255);
    paint.anti_alias = true;
    let shift = tiny_skia::Transform::from_translate(margin - bounds.left(), margin - bounds.top());
    for path in &glyphs {
        pixmap.fill_path(path, &paint, tiny_skia::FillRule::Winding, shift, None);
    }
    from_pixmap(&pixmap, ink)
}

/// The signature in a pixmap drawn in one color: that color, with the
/// pixmap's coverage as opacity. `None` when it is empty.
#[cfg(feature = "render")]
fn from_pixmap(pixmap: &tiny_skia::Pixmap, ink: Ink) -> Option<Signature> {
    let color = ink.pen();
    let rgba: Vec<u8> = pixmap.data().chunks_exact(4).flat_map(|p| [color[0], color[1], color[2], p[3]]).collect();
    if rgba.chunks_exact(4).all(|p| p[3] == 0) {
        return None;
    }
    Some(Signature { width: pixmap.width(), height: pixmap.height(), rgba })
}

/// Adapts ttf-parser's outline callbacks to a tiny-skia path.
#[cfg(feature = "render")]
struct Outline(tiny_skia::PathBuilder);

#[cfg(feature = "render")]
impl ttf_parser::OutlineBuilder for Outline {
    fn move_to(&mut self, x: f32, y: f32) {
        self.0.move_to(x, y);
    }
    fn line_to(&mut self, x: f32, y: f32) {
        self.0.line_to(x, y);
    }
    fn quad_to(&mut self, x1: f32, y1: f32, x: f32, y: f32) {
        self.0.quad_to(x1, y1, x, y);
    }
    fn curve_to(&mut self, x1: f32, y1: f32, x2: f32, y2: f32, x: f32, y: f32) {
        self.0.cubic_to(x1, y1, x2, y2, x, y);
    }
    fn close(&mut self) {
        self.0.close();
    }
}

fn crop(image: &Image, x0: u32, y0: u32, w: u32, h: u32) -> Image {
    let c = image.channels as usize;
    let mut data = Vec::with_capacity(w as usize * h as usize * c);
    for y in y0..y0 + h {
        let start = (y as usize * image.width as usize + x0 as usize) * c;
        data.extend_from_slice(&image.data[start..start + w as usize * c]);
    }
    Image { width: w, height: h, channels: image.channels, data }
}

/// The paper's color at each pixel: on a grid of cells about a sixth of
/// the area's shorter side, the average of each cell's brightest fifth,
/// spread smoothly (bilinearly) between the cells' middles. Strokes are
/// thin, so even cells they cross keep plenty of paper.
fn paper_colors(rgb: &[u8], w: usize, h: usize) -> Vec<[f32; 3]> {
    let cell = (w.min(h) / 6).max(8);
    let (gw, gh) = (w.div_ceil(cell), h.div_ceil(cell));
    let mut grid = vec![[255.0f32; 3]; gw * gh];
    for gy in 0..gh {
        for gx in 0..gw {
            let mut px: Vec<(u32, [u8; 3])> = Vec::new();
            for y in gy * cell..((gy + 1) * cell).min(h) {
                for x in gx * cell..((gx + 1) * cell).min(w) {
                    let i = (y * w + x) * 3;
                    let p = [rgb[i], rgb[i + 1], rgb[i + 2]];
                    px.push((p[0] as u32 * 299 + p[1] as u32 * 587 + p[2] as u32 * 114, p));
                }
            }
            px.sort_unstable_by_key(|p| std::cmp::Reverse(p.0));
            let n = (px.len() / 5).max(1);
            let mut sum = [0.0f32; 3];
            for (_, p) in &px[..n] {
                for c in 0..3 {
                    sum[c] += p[c] as f32;
                }
            }
            grid[gy * gw + gx] = sum.map(|v| v / n as f32);
        }
    }
    let mut out = Vec::with_capacity(w * h);
    for y in 0..h {
        let fy = ((y as f32 + 0.5) / cell as f32 - 0.5).clamp(0.0, (gh - 1) as f32);
        let (y0, ty) = (fy.floor() as usize, fy.fract());
        let y1 = (y0 + 1).min(gh - 1);
        for x in 0..w {
            let fx = ((x as f32 + 0.5) / cell as f32 - 0.5).clamp(0.0, (gw - 1) as f32);
            let (x0, tx) = (fx.floor() as usize, fx.fract());
            let x1 = (x0 + 1).min(gw - 1);
            let at = |gx: usize, gy: usize, c: usize| grid[gy * gw + gx][c];
            out.push(std::array::from_fn(|c| {
                let top = at(x0, y0, c) * (1.0 - tx) + at(x1, y0, c) * tx;
                let bottom = at(x0, y1, c) * (1.0 - tx) + at(x1, y1, c) * tx;
                top * (1.0 - ty) + bottom * ty
            }));
        }
    }
    out
}

/// Clears the pieces of ink (touching pixels at least half opaque, with
/// the fainter pixels around them) much smaller than the largest piece:
/// under 1/400 of it, or under 6 pixels. The dot of an i stays.
fn drop_specks(alpha: &mut [f32], w: usize, h: usize) {
    let mut label = vec![0u32; w * h];
    let mut sizes = vec![0usize]; // label 0: no piece
    let mut stack = Vec::new();
    for start in 0..w * h {
        if alpha[start] < 0.5 || label[start] != 0 {
            continue;
        }
        let id = sizes.len() as u32;
        let mut size = 0;
        label[start] = id;
        stack.push(start);
        while let Some(i) = stack.pop() {
            size += 1;
            let (x, y) = ((i % w) as isize, (i / w) as isize);
            for (dx, dy) in [(-1, -1), (0, -1), (1, -1), (-1, 0), (1, 0), (-1, 1), (0, 1), (1, 1)] {
                let (nx, ny) = (x + dx, y + dy);
                if nx < 0 || ny < 0 || nx >= w as isize || ny >= h as isize {
                    continue;
                }
                let j = ny as usize * w + nx as usize;
                if alpha[j] >= 0.5 && label[j] == 0 {
                    label[j] = id;
                    stack.push(j);
                }
            }
        }
        sizes.push(size);
    }
    let largest = sizes.iter().copied().max().unwrap_or(0);
    let keep: Vec<bool> = sizes.iter().map(|&s| s >= 6 && s * 400 >= largest).collect();
    // Faint pixels belong to the piece they touch; those touching none
    // (faint specks on their own) go too.
    let mut near = vec![false; w * h];
    for y in 0..h {
        for x in 0..w {
            let l = label[y * w + x];
            if l != 0 && keep[l as usize] {
                for ny in y.saturating_sub(2)..(y + 3).min(h) {
                    for nx in x.saturating_sub(2)..(x + 3).min(w) {
                        near[ny * w + nx] = true;
                    }
                }
            }
        }
    }
    for (a, near) in alpha.iter_mut().zip(near) {
        if !near {
            *a = 0.0;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Paper getting darker to the right (a shadow), grainy, with a
    /// thick blue stroke across it and a few specks.
    fn photo() -> Image {
        let (w, h) = (300u32, 120u32);
        let mut data = Vec::new();
        for y in 0..h {
            for x in 0..w {
                let light = 235.0 - x as f32 * 0.25;
                let grain = ((x * 7 + y * 13) % 5) as f32 - 2.0;
                let stroke = (y as f32 - (60.0 + 25.0 * (x as f32 / 20.0).sin())).abs() < 3.0 && (30..270).contains(&x);
                let speck = (x, y) == (10, 10) || (x, y) == (290, 110);
                let p = if stroke {
                    [40.0, 55.0, 140.0]
                } else if speck {
                    [90.0, 90.0, 90.0]
                } else {
                    [light + grain, light + grain - 2.0, light + grain - 8.0]
                };
                data.extend(p.map(|v: f32| v.clamp(0.0, 255.0) as u8));
            }
        }
        Image { width: w, height: h, channels: 3, data }
    }

    #[test]
    fn the_ink_is_kept_and_the_paper_goes() {
        let sig = lift_signature(&photo(), [0, 0, 300, 120], Ink::Original).unwrap();
        // Trimmed to the stroke (x 30..270, y about 32..88), with the specks
        // and the shadowed paper gone.
        assert!((240..=250).contains(&sig.width), "{}", sig.width);
        assert!((54..=64).contains(&sig.height), "{}", sig.height);
        let px = |x: u32, y: u32| &sig.rgba[((y * sig.width + x) * 4) as usize..][..4];
        // Corners are paper: transparent.
        assert_eq!(px(0, 0)[3], 0);
        assert_eq!(px(sig.width - 1, sig.height - 1)[3], 0);
        // Fully opaque somewhere, in a blue ink.
        let solid = sig.rgba.chunks_exact(4).find(|p| p[3] == 255).unwrap();
        assert!(solid[2] > solid[0] + 40, "{solid:?}");
        // Black ink, when asked for.
        let black = lift_signature(&photo(), [0, 0, 300, 120], Ink::Black).unwrap();
        assert!(black.rgba.chunks_exact(4).all(|p| p[0] < 40 && p[2] < 40));
    }

    #[cfg(feature = "render")]
    #[test]
    fn a_drawn_signature_is_its_strokes() {
        // A stroke 100 units long, 4 units wide, and a dot.
        let strokes = vec![vec![[10.0, 20.0], [60.0, 20.0], [110.0, 20.0]], vec![[60.0, 40.0]]];
        let sig = drawn_signature(&strokes, 4.0, Ink::Blue).unwrap();
        // Drawn 4 times finer: about 104 × 24 units of ink, with a margin.
        assert!((416..=440).contains(&sig.width), "{}", sig.width);
        assert!((96..=120).contains(&sig.height), "{}", sig.height);
        assert!(sig.rgba.chunks_exact(4).all(|p| p[..3] == [22, 48, 150]));
        let alpha = |x: u32, y: u32| sig.rgba[((y * sig.width + x) * 4 + 3) as usize];
        assert_eq!(alpha(0, sig.height - 1), 0);
        assert_eq!(alpha(sig.width / 2, (sig.height as f32 * 0.1) as u32 + 2), 255);
        assert_eq!(drawn_signature(&[], 4.0, Ink::Black), None);
    }

    #[test]
    fn blank_paper_has_no_signature() {
        let blank = Image { width: 100, height: 50, channels: 1, data: (0..5000).map(|i| 230 + (i % 3) as u8).collect() };
        assert_eq!(lift_signature(&blank, [0, 0, 100, 50], Ink::Black), None);
    }
}
