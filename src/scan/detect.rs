//! Finding the page in a photo.
//!
//! Everything happens on a small copy of the photo (480 pixels), in "bright
//! minus colorful", which tells paper apart from most things it lies on.
//! Two ways of finding the page each suggest corners:
//!
//! - **Regions:** Otsu's threshold splits the picture into light and dark;
//!   the largest region, holes (the writing) filled, has a convex hull,
//!   and the largest four-cornered shape on that hull is a candidate.
//!   Quick and robust on a contrasting background, but fooled by strong
//!   shadows.
//! - **Edges:** straight edges are found with a Hough transform on the
//!   gradient (each edge pixel votes only for lines along it), and every
//!   two roughly horizontal and two roughly vertical lines make a
//!   candidate. This copes with shadows and light backgrounds.
//!
//! The candidate whose outline runs along the most edge (with the
//! gradient across it) wins, and its sides are then fitted to the edge
//! pixels along them, for corners accurate to a pixel or two in the photo.

use super::{Image, Quad};

/// The size of the copy the page is looked for in.
const WORK_SIZE: u32 = 480;

/// The page's corners in `photo`, or `None` when no page stands out (for
/// example when the photo is filled by the page, or by a busy background).
pub fn find_page(photo: &Image) -> Option<Quad> {
    let (small, k) = photo.shrink(WORK_SIZE);
    let (w, h) = (small.width as usize, small.height as usize);
    if w < 16 || h < 16 {
        return None;
    }
    let (lum, sat): (Vec<f32>, Vec<f32>) = match small.channels {
        1 => (small.data.iter().map(|&v| v as f32).collect(), vec![0.0; w * h]),
        _ => small
            .data
            .chunks_exact(3)
            .map(|p| {
                let (mx, mn) = (p[0].max(p[1]).max(p[2]), p[0].min(p[1]).min(p[2]));
                (super::luma(p[0], p[1], p[2]) as f32, (mx - mn) as f32)
            })
            .unzip(),
    };
    let score: Vec<f32> = lum.iter().zip(&sat).map(|(l, s)| l - s).collect();
    let score = blur(&blur(&score, w, h, 2), w, h, 2);
    let edges = Edges::new(&score, w, h);

    let mut candidates = region_candidates(&score, w, h);
    candidates.extend(edges.hough_candidates());
    let area = (w * h) as f32;
    let best = candidates
        .into_iter()
        .filter(|q| plausible(q, w as f32, h as f32) && polygon_area(q) < 0.97 * area)
        .filter_map(|q| {
            let (length, fraction) = edges.support(&q);
            (fraction >= 0.5).then_some((length, q))
        })
        .max_by(|a, b| a.0.total_cmp(&b.0))?
        .1;
    let quad = edges.fit(&best).filter(|q| plausible(q, w as f32, h as f32)).unwrap_or(best);
    let (fw, fh) = (photo.width as f32, photo.height as f32);
    Some(quad.map(|[x, y]| [(x * k as f32).clamp(0.0, fw), (y * k as f32).clamp(0.0, fh)]))
}

/// Candidates from light and dark regions (paper is usually lighter than
/// what it lies on, but not always).
fn region_candidates(score: &[f32], w: usize, h: usize) -> Vec<Quad> {
    let threshold = otsu(score);
    let mut out = Vec::new();
    for bright in [true, false] {
        let mask: Vec<bool> = score.iter().map(|&s| (s > threshold) == bright).collect();
        let mask = erode(&dilate(&mask, w, h, 2), w, h, 2);
        let Some((region, area)) = largest_region(&mask, w, h) else { continue };
        // Too small, or along most of the photo's edge (the background,
        // or a page that fills the photo).
        if (area as f32) < 0.1 * (w * h) as f32 || edge_fraction(&region, w, h) > 0.8 {
            continue;
        }
        let outline = outline(&region, w, h);
        let hull = convex_hull(outline.clone());
        if hull.len() < 4 {
            continue;
        }
        let quad = largest_quad(&hull);
        let quad = refine(quad, &outline, w, h).unwrap_or(quad);
        // The region should fill the quadrilateral.
        if (area as f32) >= 0.85 * polygon_area(&quad) {
            out.push(quad);
        }
    }
    out
}

/// Edge pixels: thinned to one pixel (local maxima of the gradient across
/// the edge) and strong compared to the rest of the picture.
struct Edges {
    w: usize,
    h: usize,
    gx: Vec<f32>,
    gy: Vec<f32>,
    strong: Vec<bool>,
}

/// A line `x cos θ + y sin θ = ρ`, as (θ, ρ).
type Line = (f32, f32);

impl Edges {
    fn new(v: &[f32], w: usize, h: usize) -> Edges {
        let (mut gx, mut gy, mut mag) = (vec![0.0; w * h], vec![0.0; w * h], vec![0.0f32; w * h]);
        for y in 1..h - 1 {
            for x in 1..w - 1 {
                let at = |dx: usize, dy: usize| v[(y + dy - 1) * w + x + dx - 1];
                let i = y * w + x;
                gx[i] = (at(2, 0) + 2.0 * at(2, 1) + at(2, 2) - at(0, 0) - 2.0 * at(0, 1) - at(0, 2)) / 8.0;
                gy[i] = (at(0, 2) + 2.0 * at(1, 2) + at(2, 2) - at(0, 0) - 2.0 * at(1, 0) - at(2, 0)) / 8.0;
                mag[i] = gx[i].hypot(gy[i]);
            }
        }
        let mut sorted: Vec<f32> = mag.iter().copied().step_by(3).collect();
        sorted.sort_by(f32::total_cmp);
        let threshold = sorted[sorted.len() * 9 / 10].max(2.0);
        let mut strong = vec![false; w * h];
        for y in 1..h - 1 {
            for x in 1..w - 1 {
                let i = y * w + x;
                if mag[i] < threshold {
                    continue;
                }
                // Neighbors across the edge, along the gradient.
                let (dx, dy) = ((gx[i] / mag[i]).round() as isize, (gy[i] / mag[i]).round() as isize);
                let j = |s: isize| ((y as isize + s * dy) as usize) * w + (x as isize + s * dx) as usize;
                strong[i] = mag[i] >= mag[j(1)] && mag[i] >= mag[j(-1)];
            }
        }
        Edges { w, h, gx, gy, strong }
    }

    /// Whether (x, y) is an edge pixel whose gradient is along `n`.
    fn across(&self, x: isize, y: isize, n: [f32; 2]) -> bool {
        if x < 0 || y < 0 || x >= self.w as isize || y >= self.h as isize {
            return false;
        }
        let i = y as usize * self.w + x as usize;
        self.strong[i] && {
            let (gx, gy) = (self.gx[i], self.gy[i]);
            (gx * n[0] + gy * n[1]).abs() > 0.85 * gx.hypot(gy)
        }
    }

    /// How much of the outline of `q` lies on edges: (length, share).
    /// Stretches along the photo's border don't count either way (the page
    /// may go on past it), but at most half the outline may be there.
    fn support(&self, q: &Quad) -> (f32, f32) {
        let (mut on, mut inside, mut total) = (0.0f32, 0.0f32, 0.0f32);
        for j in 0..4 {
            let (a, b) = (q[j], q[(j + 1) % 4]);
            let len = (b[0] - a[0]).hypot(b[1] - a[1]);
            let u = [(b[0] - a[0]) / len, (b[1] - a[1]) / len];
            let n = [-u[1], u[0]];
            let mut t = 0.0;
            while t < len {
                let p = [a[0] + u[0] * t, a[1] + u[1] * t];
                total += 1.0;
                t += 1.0;
                if p[0] < 2.0 || p[1] < 2.0 || p[0] > self.w as f32 - 2.0 || p[1] > self.h as f32 - 2.0 {
                    continue;
                }
                inside += 1.0;
                let hit = (-2..=2).any(|d| {
                    let (x, y) = (p[0] + n[0] * d as f32, p[1] + n[1] * d as f32);
                    self.across(x as isize, y as isize, n)
                });
                if hit {
                    on += 1.0;
                }
            }
        }
        if inside < 0.5 * total {
            return (0.0, 0.0);
        }
        (on, on / inside.max(1.0))
    }

    /// Lines through many edge pixels (a Hough transform where each edge
    /// pixel votes only for the few directions its gradient allows).
    fn lines(&self) -> Vec<Line> {
        let (w, h) = (self.w, self.h);
        let diag = (w as f32).hypot(h as f32).ceil() as usize;
        let rows = 2 * diag + 1;
        let mut acc = vec![0u32; 180 * rows];
        let trig: Vec<(f32, f32)> = (0..180).map(|t| (t as f32).to_radians().sin_cos()).collect();
        for y in 0..h {
            for x in 0..w {
                let i = y * w + x;
                if !self.strong[i] {
                    continue;
                }
                let theta = self.gy[i].atan2(self.gx[i]).to_degrees().rem_euclid(180.0).round() as i32;
                for dt in -2..=2 {
                    let t = (theta + dt).rem_euclid(180) as usize;
                    let (s, c) = trig[t];
                    let rho = (x as f32 + 0.5) * c + (y as f32 + 0.5) * s;
                    acc[t * rows + (rho.round() as isize + diag as isize) as usize] += 1;
                }
            }
        }
        let min_votes = (w.min(h) as f32 * 0.12) as u32;
        let mut peaks = Vec::new();
        for t in 0..180 {
            for r in 0..rows {
                let v = acc[t * rows + r];
                if v < min_votes {
                    continue;
                }
                let is_max = (-3i32..=3).all(|dt| {
                    (-4i32..=4).all(|dr| {
                        // Past 180°, the same line has the opposite ρ.
                        let mut tt = t as i32 + dt;
                        let mut rr = r as i32 + dr - diag as i32;
                        if !(0..180).contains(&tt) {
                            tt = tt.rem_euclid(180);
                            rr = -rr;
                        }
                        let rr = rr + diag as i32;
                        if (dt, dr) == (0, 0) || rr < 0 || rr >= rows as i32 {
                            return true;
                        }
                        let o = acc[tt as usize * rows + rr as usize];
                        // Ties: the first one wins.
                        o < v || (o == v && (tt as usize, rr as usize) > (t, r))
                    })
                });
                if is_max {
                    peaks.push((v, t, r));
                }
            }
        }
        peaks.sort_by(|a, b| b.0.cmp(&a.0));
        peaks.truncate(24);
        peaks.into_iter().map(|(_, t, r)| (t as f32, r as f32 - diag as f32)).collect()
    }

    /// Every two horizontal and two vertical lines (the photo's borders
    /// included) as a quadrilateral.
    fn hough_candidates(&self) -> Vec<Quad> {
        let (w, h) = (self.w as f32, self.h as f32);
        let (mut across, mut down): (Vec<Line>, Vec<Line>) = self.lines().into_iter().partition(|l| (45.0..135.0).contains(&l.0));
        across.truncate(12);
        down.truncate(12);
        across.extend([(90.0, 0.0), (90.0, h)]);
        down.extend([(0.0, 0.0), (0.0, w)]);
        let mut out = Vec::new();
        let center = [w / 2.0, h / 2.0];
        for (i, &a) in across.iter().enumerate() {
            for &b in &across[i + 1..] {
                // Which one is on top: its height at the middle.
                let (top, bottom) = if y_at(a, center[0]) < y_at(b, center[0]) { (a, b) } else { (b, a) };
                for (j, &c) in down.iter().enumerate() {
                    for &d in &down[j + 1..] {
                        let (left, right) = if x_at(c, center[1]) < x_at(d, center[1]) { (c, d) } else { (d, c) };
                        let corners = [meet(top, left), meet(top, right), meet(bottom, right), meet(bottom, left)];
                        if let [Some(a), Some(b), Some(c), Some(d)] = corners {
                            out.push([a, b, c, d]);
                        }
                    }
                }
            }
        }
        out
    }

    /// Fits each side of `q` to the edge pixels along it (weighted by
    /// gradient), and puts the corners where the fitted sides meet.
    fn fit(&self, q: &Quad) -> Option<Quad> {
        let mut lines = [([0.0f32; 2], [0.0f32; 2]); 4];
        for j in 0..4 {
            let (a, b) = (q[j], q[(j + 1) % 4]);
            let len = (b[0] - a[0]).hypot(b[1] - a[1]);
            let u = [(b[0] - a[0]) / len, (b[1] - a[1]) / len];
            let n = [-u[1], u[0]];
            let mut points = Vec::new();
            let mut t = 0.08 * len;
            while t < 0.92 * len {
                let p = [a[0] + u[0] * t, a[1] + u[1] * t];
                for d in -3..=3 {
                    let (x, y) = ((p[0] + n[0] * d as f32) as isize, (p[1] + n[1] * d as f32) as isize);
                    if self.across(x, y, n) {
                        points.push([x as f32 + 0.5, y as f32 + 0.5]);
                    }
                }
                t += 1.0;
            }
            points.dedup();
            lines[j] = if points.len() as f32 >= 0.3 * len { fit_line(&points) } else { (a, u) };
        }
        let mut out = *q;
        for j in 0..4 {
            out[j] = intersect(lines[(j + 3) % 4], lines[j])?;
            if (out[j][0] - q[j][0]).hypot(out[j][1] - q[j][1]) > 6.0 {
                return None;
            }
        }
        Some(out)
    }
}

fn y_at((t, r): Line, x: f32) -> f32 {
    let (s, c) = t.to_radians().sin_cos();
    (r - x * c) / s
}

fn x_at((t, r): Line, y: f32) -> f32 {
    let (s, c) = t.to_radians().sin_cos();
    (r - y * s) / c
}

/// Where two lines cross.
fn meet(a: Line, b: Line) -> Option<[f32; 2]> {
    let (s1, c1) = a.0.to_radians().sin_cos();
    let (s2, c2) = b.0.to_radians().sin_cos();
    let det = c1 * s2 - s1 * c2;
    if det.abs() < 1e-3 {
        return None;
    }
    Some([(a.1 * s2 - b.1 * s1) / det, (c1 * b.1 - c2 * a.1) / det])
}

/// A box blur of radius `r`, rows then columns (edges repeat).
fn blur(v: &[f32], w: usize, h: usize, r: usize) -> Vec<f32> {
    let pass = |src: &[f32], len: usize, count: usize, at: &dyn Fn(usize, usize) -> usize| {
        let mut out = vec![0.0; src.len()];
        for line in 0..count {
            for i in 0..len {
                let lo = i.saturating_sub(r);
                let hi = (i + r).min(len - 1);
                let sum: f32 = (lo..=hi).map(|j| src[at(line, j)]).sum();
                out[at(line, i)] = sum / (hi - lo + 1) as f32;
            }
        }
        out
    };
    let rows = pass(v, w, h, &|y, x| y * w + x);
    pass(&rows, h, w, &|x, y| y * w + x)
}

/// Otsu's threshold: the value that best splits the values into two groups.
pub(super) fn otsu(v: &[f32]) -> f32 {
    let (lo, hi) = v.iter().fold((f32::MAX, f32::MIN), |(lo, hi), &x| (lo.min(x), hi.max(x)));
    if hi <= lo {
        return lo;
    }
    let bins = 256;
    let mut hist = vec![0u32; bins];
    for &x in v {
        hist[(((x - lo) / (hi - lo)) * (bins - 1) as f32) as usize] += 1;
    }
    let total = v.len() as f64;
    let sum: f64 = hist.iter().enumerate().map(|(i, &c)| i as f64 * c as f64).sum();
    let (mut below, mut below_sum, mut best, mut best_i) = (0.0, 0.0, 0.0, 0);
    for (i, &c) in hist.iter().enumerate() {
        below += c as f64;
        below_sum += i as f64 * c as f64;
        let above = total - below;
        if below == 0.0 || above == 0.0 {
            continue;
        }
        let diff = below_sum / below - (sum - below_sum) / above;
        let between = below * above * diff * diff;
        if between > best {
            best = between;
            best_i = i;
        }
    }
    lo + (best_i as f32 + 0.5) / (bins - 1) as f32 * (hi - lo)
}

fn dilate(m: &[bool], w: usize, h: usize, r: usize) -> Vec<bool> {
    morph(m, w, h, r, true)
}

fn erode(m: &[bool], w: usize, h: usize, r: usize) -> Vec<bool> {
    morph(m, w, h, r, false)
}

/// Dilation (`grow`) or erosion with a square, rows then columns.
fn morph(m: &[bool], w: usize, h: usize, r: usize, grow: bool) -> Vec<bool> {
    let pass = |src: &[bool], len: usize, count: usize, at: &dyn Fn(usize, usize) -> usize| {
        let mut out = vec![false; src.len()];
        for line in 0..count {
            for i in 0..len {
                let lo = i.saturating_sub(r);
                let hi = (i + r).min(len - 1);
                let mut window = (lo..=hi).map(|j| src[at(line, j)]);
                out[at(line, i)] = if grow { window.any(|b| b) } else { window.all(|b| b) };
            }
        }
        out
    };
    let rows = pass(m, w, h, &|y, x| y * w + x);
    pass(&rows, h, w, &|x, y| y * w + x)
}

/// The largest 4-connected region of `mask`, with its holes filled, and its
/// area.
fn largest_region(mask: &[bool], w: usize, h: usize) -> Option<(Vec<bool>, usize)> {
    let mut label = vec![0u32; w * h];
    let (mut best, mut best_size, mut next) = (0, 0, 1);
    let mut stack = Vec::new();
    for start in 0..w * h {
        if !mask[start] || label[start] != 0 {
            continue;
        }
        let mut size = 0;
        label[start] = next;
        stack.push(start);
        while let Some(i) = stack.pop() {
            size += 1;
            let (x, y) = (i % w, i / w);
            let mut visit = |j: usize| {
                if mask[j] && label[j] == 0 {
                    label[j] = next;
                    stack.push(j);
                }
            };
            if x > 0 {
                visit(i - 1);
            }
            if x + 1 < w {
                visit(i + 1);
            }
            if y > 0 {
                visit(i - w);
            }
            if y + 1 < h {
                visit(i + w);
            }
        }
        if size > best_size {
            best = next;
            best_size = size;
        }
        next += 1;
    }
    if best == 0 {
        return None;
    }
    // Fill the holes: whatever outside the region can't be reached from the
    // photo's edge.
    let inside: Vec<bool> = label.iter().map(|&l| l == best).collect();
    let mut outside = vec![false; w * h];
    for i in 0..w * h {
        let (x, y) = (i % w, i / w);
        if (x == 0 || y == 0 || x == w - 1 || y == h - 1) && !inside[i] {
            outside[i] = true;
            stack.push(i);
        }
    }
    while let Some(i) = stack.pop() {
        let (x, y) = (i % w, i / w);
        let mut visit = |j: usize| {
            if !inside[j] && !outside[j] {
                outside[j] = true;
                stack.push(j);
            }
        };
        if x > 0 {
            visit(i - 1);
        }
        if x + 1 < w {
            visit(i + 1);
        }
        if y > 0 {
            visit(i - w);
        }
        if y + 1 < h {
            visit(i + w);
        }
    }
    let region: Vec<bool> = outside.iter().map(|&o| !o).collect();
    let area = region.iter().filter(|&&b| b).count();
    Some((region, area))
}

/// The share of the photo's edge pixels that are in `region`.
fn edge_fraction(region: &[bool], w: usize, h: usize) -> f32 {
    let mut on = 0;
    for x in 0..w {
        on += region[x] as usize + region[(h - 1) * w + x] as usize;
    }
    for y in 0..h {
        on += region[y * w] as usize + region[y * w + w - 1] as usize;
    }
    on as f32 / (2 * (w + h)) as f32
}

/// The region's outline: the first and last pixel of each row and column.
fn outline(region: &[bool], w: usize, h: usize) -> Vec<[f32; 2]> {
    let mut points = Vec::new();
    for y in 0..h {
        let row = &region[y * w..(y + 1) * w];
        if let (Some(a), Some(b)) = (row.iter().position(|&v| v), row.iter().rposition(|&v| v)) {
            points.push([a as f32, y as f32 + 0.5]);
            points.push([b as f32 + 1.0, y as f32 + 0.5]);
        }
    }
    for x in 0..w {
        let column = |y: &usize| region[y * w + x];
        if let (Some(a), Some(b)) = ((0..h).find(column), (0..h).rev().find(column)) {
            points.push([x as f32 + 0.5, a as f32]);
            points.push([x as f32 + 0.5, b as f32 + 1.0]);
        }
    }
    points
}

fn cross(o: [f32; 2], a: [f32; 2], b: [f32; 2]) -> f32 {
    (a[0] - o[0]) * (b[1] - o[1]) - (a[1] - o[1]) * (b[0] - o[0])
}

/// The convex hull (Andrew's monotone chain), clockwise on screen (y down),
/// like the corners of a [`Quad`].
fn convex_hull(mut points: Vec<[f32; 2]>) -> Vec<[f32; 2]> {
    points.sort_by(|a, b| a[0].total_cmp(&b[0]).then(a[1].total_cmp(&b[1])));
    points.dedup();
    if points.len() < 3 {
        return points;
    }
    let mut hull: Vec<[f32; 2]> = Vec::new();
    for pass in 0..2 {
        let start = hull.len();
        let iter: Box<dyn Iterator<Item = &[f32; 2]>> =
            if pass == 0 { Box::new(points.iter()) } else { Box::new(points.iter().rev()) };
        for &p in iter {
            while hull.len() >= start + 2 && cross(hull[hull.len() - 2], hull[hull.len() - 1], p) <= 0.0 {
                hull.pop();
            }
            hull.push(p);
        }
        hull.pop();
    }
    if polygon_area_signed(&hull) < 0.0 {
        hull.reverse();
    }
    hull
}

fn polygon_area_signed(p: &[[f32; 2]]) -> f32 {
    (0..p.len())
        .map(|i| {
            let (a, b) = (p[i], p[(i + 1) % p.len()]);
            a[0] * b[1] - b[0] * a[1]
        })
        .sum::<f32>()
        / 2.0
}

pub(super) fn polygon_area(p: &[[f32; 2]]) -> f32 {
    polygon_area_signed(p).abs()
}

/// The four hull points enclosing the largest area, ordered like a
/// [`Quad`]. Starts from the extreme points (smallest x+y is the top-left
/// corner, and so on) and moves one corner at a time along the hull while
/// the area grows.
fn largest_quad(hull: &[[f32; 2]]) -> Quad {
    let n = hull.len();
    let arg = |f: &dyn Fn([f32; 2]) -> f32| {
        (0..n).max_by(|&a, &b| f(hull[a]).total_cmp(&f(hull[b]))).unwrap()
    };
    let mut idx = [arg(&|p| -(p[0] + p[1])), arg(&|p| p[0] - p[1]), arg(&|p| p[0] + p[1]), arg(&|p| p[1] - p[0])];
    let area = |idx: &[usize; 4]| polygon_area(&idx.map(|i| hull[i]));
    for _ in 0..20 {
        let mut changed = false;
        for j in 0..4 {
            let (prev, next) = (idx[(j + 3) % 4], idx[(j + 1) % 4]);
            let mut best = area(&idx);
            let mut v = (prev + 1) % n;
            while v != next {
                let mut trial = idx;
                trial[j] = v;
                let a = area(&trial);
                if a > best + 1e-3 {
                    best = a;
                    idx[j] = v;
                    changed = true;
                }
                v = (v + 1) % n;
            }
        }
        if !changed {
            break;
        }
    }
    idx.map(|i| hull[i])
}

/// Fits a line to the outline points along each side (away from the
/// corners) and puts the corners where these lines meet.
fn refine(quad: Quad, outline: &[[f32; 2]], w: usize, h: usize) -> Option<Quad> {
    let diag = ((w * w + h * h) as f32).sqrt();
    let tolerance = 0.02 * diag + 1.5;
    let mut lines = [([0.0f32; 2], [0.0f32; 2]); 4];
    for j in 0..4 {
        let (a, b) = (quad[j], quad[(j + 1) % 4]);
        let d = [b[0] - a[0], b[1] - a[1]];
        let len = (d[0] * d[0] + d[1] * d[1]).sqrt().max(1e-3);
        let (u, n) = ([d[0] / len, d[1] / len], [-d[1] / len, d[0] / len]);
        let near: Vec<[f32; 2]> = outline
            .iter()
            .copied()
            .filter(|p| {
                let v = [p[0] - a[0], p[1] - a[1]];
                let t = (v[0] * u[0] + v[1] * u[1]) / len;
                (v[0] * n[0] + v[1] * n[1]).abs() < tolerance && (0.1..0.9).contains(&t)
            })
            .collect();
        lines[j] = if near.len() >= 8 { fit_line(&near) } else { (a, u) };
    }
    let mut out = quad;
    for j in 0..4 {
        let corner = intersect(lines[(j + 3) % 4], lines[j])?;
        let moved = ((corner[0] - quad[j][0]).powi(2) + (corner[1] - quad[j][1]).powi(2)).sqrt();
        if moved > 0.06 * diag {
            return None;
        }
        out[j] = corner;
    }
    Some(out)
}

/// The total-least-squares line through `points`: (a point on it, its
/// direction).
fn fit_line(points: &[[f32; 2]]) -> ([f32; 2], [f32; 2]) {
    let n = points.len() as f32;
    let m = [points.iter().map(|p| p[0]).sum::<f32>() / n, points.iter().map(|p| p[1]).sum::<f32>() / n];
    let (mut sxx, mut sxy, mut syy) = (0.0, 0.0, 0.0);
    for p in points {
        let (dx, dy) = (p[0] - m[0], p[1] - m[1]);
        sxx += dx * dx;
        sxy += dx * dy;
        syy += dy * dy;
    }
    let angle = 0.5 * (2.0 * sxy).atan2(sxx - syy);
    (m, [angle.cos(), angle.sin()])
}

fn intersect((p, d): ([f32; 2], [f32; 2]), (q, e): ([f32; 2], [f32; 2])) -> Option<[f32; 2]> {
    let den = d[0] * e[1] - d[1] * e[0];
    if den.abs() < 1e-4 {
        return None;
    }
    let t = ((q[0] - p[0]) * e[1] - (q[1] - p[1]) * e[0]) / den;
    Some([p[0] + t * d[0], p[1] + t * d[1]])
}

/// A convex quadrilateral with no very sharp corner, not tiny, and not
/// far outside the photo.
fn plausible(q: &Quad, w: f32, h: f32) -> bool {
    if polygon_area(q) < 0.1 * w * h {
        return false;
    }
    if q.iter().any(|p| p[0] < -0.05 * w || p[0] > 1.05 * w || p[1] < -0.05 * h || p[1] > 1.05 * h) {
        return false;
    }
    (0..4).all(|j| {
        let (a, b, c) = (q[(j + 3) % 4], q[j], q[(j + 1) % 4]);
        let (u, v) = ([a[0] - b[0], a[1] - b[1]], [c[0] - b[0], c[1] - b[1]]);
        let cos = (u[0] * v[0] + u[1] * v[1]) / ((u[0].hypot(u[1])) * (v[0].hypot(v[1]))).max(1e-6);
        // Convex (all turns the same way) and between 35° and 145°.
        cross(a, b, c) > 0.0 && cos.abs() < 0.82
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A gray photo with a light quadrilateral `q` on a dark background,
    /// and some "text" on the page.
    fn photo(q: Quad, w: u32, h: u32) -> Image {
        let mut img = Image::new(w, h, 1);
        for y in 0..h {
            for x in 0..w {
                let p = [x as f32 + 0.5, y as f32 + 0.5];
                let inside = (0..4).all(|j| cross(q[j], q[(j + 1) % 4], p) >= 0.0);
                let text = inside && (y / 6) % 3 == 0 && (x / 4) % 5 != 0;
                img.data[(y * w + x) as usize] = if text { 40 } else if inside { 225 } else { 70 + ((x * 7 + y * 3) % 30) as u8 };
            }
        }
        img
    }

    #[test]
    fn finds_a_tilted_page() {
        let q: Quad = [[180.0, 90.0], [760.0, 140.0], [700.0, 900.0], [120.0, 820.0]];
        let found = find_page(&photo(q, 900, 1000)).expect("a page");
        for (a, b) in q.iter().zip(&found) {
            assert!((a[0] - b[0]).abs() < 8.0 && (a[1] - b[1]).abs() < 8.0, "{q:?} vs {found:?}");
        }
    }

    #[test]
    fn no_page_in_a_plain_photo() {
        let img = Image { width: 300, height: 200, channels: 1, data: vec![128; 60000] };
        assert_eq!(find_page(&img), None);
    }

    #[test]
    fn clockwise_cross_sign() {
        // Corners of a Quad turn the same way as `plausible` expects.
        let q: Quad = [[0.0, 0.0], [10.0, 0.0], [10.0, 10.0], [0.0, 10.0]];
        assert!(plausible(&q, 10.0, 10.0));
        assert!(cross(q[0], q[1], q[2]) > 0.0);
    }
}
