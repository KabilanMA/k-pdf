//! Reading text in images (OCR), with the [ocrs](https://github.com/robertknight/ocrs)
//! engine and its two neural networks: one finds words, the other reads
//! lines of them. They work on Latin script (English and other languages
//! written with the same letters).
//!
//! The networks' files ([`MODEL_FILES`], about 12 MB) are not part of the
//! program: release packages carry them next to it, and
//! `packaging/fetch-ocr-models.sh` downloads them for builds from source.
//! [`find_models`] looks in the usual places ([`model_dirs`]).
//!
//! [`read_page_images`] reads the words in a page's images and returns
//! them as [`TextChar`]s in display points, like the page's own text, so
//! searching and selecting work on them the same way.

use std::path::{Path, PathBuf};

use crate::document::{Document, Page};
use crate::error::{Error, Result};
use crate::object::{Dictionary, Object};
use crate::render::Renderer;
use crate::text::TextChar;

/// The networks' files: finding words, and reading them.
pub const MODEL_FILES: [&str; 2] = ["text-detection.onnx", "text-recognition.onnx"];

/// Pages are rendered for reading at this many pixels per point (180 dpi:
/// at 144, small print in scans is misread noticeably more often).
const SCALE: f64 = 2.5;
/// … but no larger than this on their longer side.
const MAX_SIDE: f64 = 3200.0;

pub struct Ocr {
    engine: ocrs::OcrEngine,
}

fn failed(e: impl std::fmt::Display) -> Error {
    Error::Image(format!("could not read text in images ({e})"))
}

impl Ocr {
    /// Loads the networks from `dir` (which holds [`MODEL_FILES`]).
    pub fn load(dir: &Path) -> Result<Ocr> {
        let model = |name: &str| rten::Model::load_file(dir.join(name)).map_err(|e| failed(format!("{name}: {e}")));
        let engine = ocrs::OcrEngine::new(ocrs::OcrEngineParams {
            detection_model: Some(model(MODEL_FILES[0])?),
            recognition_model: Some(model(MODEL_FILES[1])?),
            ..Default::default()
        })
        .map_err(failed)?;
        Ok(Ocr { engine })
    }

    /// Loads the networks from the first place they're found.
    pub fn find() -> Result<Ocr> {
        let dir = find_models().ok_or_else(|| Error::Image(not_installed()))?;
        Ocr::load(&dir)
    }

    /// The text in an RGB image, line by line (`"\n"` between lines, `" "`
    /// between words), each character with its rectangle in pixels.
    ///
    /// Text needn't be level: the image is first turned so that most of
    /// its words are (a crooked scan, a page scanned sideways or upside
    /// down), since lines read on the slant take in parts of the lines
    /// above and below. The characters' rectangles are turned back.
    ///
    /// Memory is kept down (the networks' working memory grows with the
    /// image): words are found a tile at a time, and lines are read a few
    /// at a time.
    pub fn read(&self, rgb: &[u8], width: u32, height: u32) -> Result<Vec<TextChar>> {
        let (w, h) = (width as usize, height as usize);
        let mut words = self.find_words(rgb, w, h)?;
        let mut turned: Option<(Vec<u8>, Turn)> = None;
        if let Some(angle) = text_angle(&self.engine, &words) {
            let (pixels, turn) = rotate(rgb, w, h, angle);
            // The words turn with the image (finding them again in the
            // turned image gives the same).
            words = words.iter().map(|r| turn.rect_forward(r)).collect();
            turned = Some((pixels, turn));
        }
        let (pixels, (tw, th)) = turned.as_ref().map_or((rgb, (w, h)), |(p, t)| (&p[..], t.size));
        if upside_down(pixels, tw, th, &words) {
            // Half a turn is the pixels in reverse order.
            let pixels: Vec<u8> = pixels.chunks_exact(3).rev().flatten().copied().collect();
            let turn = match turned.take() {
                Some((_, t)) => t.and_half_turn(),
                None => Turn::new((w, h), 0.0).and_half_turn(),
            };
            let flip = |r: &rten_imageproc::RotatedRect| {
                let c = r.center();
                rten_imageproc::RotatedRect::new(
                    rten_imageproc::PointF::from_yx(th as f32 - c.y, tw as f32 - c.x),
                    -r.up_axis(),
                    r.width(),
                    r.height(),
                )
            };
            words = words.iter().map(flip).collect();
            turned = Some((pixels, turn));
        }
        let lines = text_lines(&self.engine, &words);
        let (pixels, turn) = match &turned {
            Some((p, t)) => (&p[..], Some(t)),
            None => (rgb, None),
        };
        let source = ocrs::ImageSource::from_bytes(pixels, (tw as u32, th as u32)).map_err(failed)?;
        let input = self.engine.prepare_input(source).map_err(failed)?;
        let mut read = Vec::with_capacity(lines.len());
        for group in lines.chunks(LINES_AT_ONCE) {
            read.extend(self.engine.recognize_text(&input, group).map_err(failed)?);
        }
        let mut out = Vec::new();
        for line in read.into_iter().flatten() {
            if !out.is_empty() {
                out.push(TextChar { text: "\n".into(), rect: None });
            }
            for (i, word) in ocrs::TextItem::chars(&line).split(|c| c.char == ' ').filter(|w| !w.is_empty()).enumerate() {
                if i > 0 {
                    out.push(TextChar { text: " ".into(), rect: None });
                }
                let letters = same_case(word.iter().map(|c| c.char).collect());
                for (c, letter) in word.iter().zip(letters) {
                    let r = c.rect;
                    let mut rect = [r.left() as f32, r.top() as f32, r.right() as f32, r.bottom() as f32];
                    if let Some(turn) = turn {
                        rect = turn.back(rect);
                    }
                    out.push(TextChar { text: letter.to_string(), rect: Some(rect) });
                }
            }
        }
        Ok(out)
    }

    /// The words' outlines. The word-finding network takes images of a
    /// fixed size ([`DETECT_SIZE`]) and ocrs squeezes larger images into
    /// it, which at 180 dpi shrinks a page's print to well under half its
    /// width, so small print ran together. Instead the image is cut into
    /// tiles that size (overlapping, so words on the edges are seen whole
    /// by one of them); the network's verdicts are put together into a mask
    /// of the whole image, and words are its connected parts, as in ocrs.
    /// This also keeps the networks' working memory to one tile's.
    fn find_words(&self, rgb: &[u8], w: usize, h: usize) -> Result<Vec<rten_imageproc::RotatedRect>> {
        use rten_tensor::prelude::*;
        let xs = tiles(w, DETECT_SIZE.0);
        let ys = tiles(h, DETECT_SIZE.1);
        let mut mask = vec![false; w * h];
        let threshold = self.engine.detection_threshold();
        for (yi, &(y0, y1)) in ys.iter().enumerate() {
            for (xi, &(x0, x1)) in xs.iter().enumerate() {
                let mut tile = Vec::with_capacity((x1 - x0) * (y1 - y0) * 3);
                for y in y0..y1 {
                    tile.extend_from_slice(&rgb[(y * w + x0) * 3..(y * w + x1) * 3]);
                }
                if blank(&tile) {
                    continue;
                }
                let source = ocrs::ImageSource::from_bytes(&tile, ((x1 - x0) as u32, (y1 - y0) as u32)).map_err(failed)?;
                let input = self.engine.prepare_input(source).map_err(failed)?;
                let probability = self.engine.detect_text_pixels(&input).map_err(failed)?.to_vec();
                // The part of the tile nearer to its middle than to the
                // next tile's.
                let (cx0, cx1) = core(&xs, xi);
                let (cy0, cy1) = core(&ys, yi);
                for y in cy0..cy1 {
                    let row = &probability[(y - y0) * (x1 - x0)..][..x1 - x0];
                    for x in cx0..cx1 {
                        mask[y * w + x] = row[x - x0] > threshold;
                    }
                }
            }
        }
        let mask = rten_tensor::NdTensorView::from_data([h, w], &mask[..]);
        let words = rten_imageproc::find_contours(mask, rten_imageproc::RetrievalMode::External)
            .iter()
            .filter_map(|outline| {
                let points: Vec<_> = outline.iter().map(|p| p.to_f32()).collect();
                let mut rect = rten_imageproc::min_area_rect(&rten_imageproc::simplify_polygon(&points, 2.0))?;
                // The network marks a little less than the whole word.
                rect.resize(rect.width() + 6.0, rect.height() + 6.0);
                Some(rect)
            })
            .filter(|r| r.area() >= 100.0)
            .collect();
        Ok(contrasting(words, rgb, w, h))
    }
}

/// Leaves out the "words" with hardly any contrast: at full size the
/// network now and then takes the speckles of a scan's blank paper for
/// print. A word's spread of brightness (from its 5th to its 95th
/// percentile) has to be at least a quarter of the image's (1st to 99th).
fn contrasting(words: Vec<rten_imageproc::RotatedRect>, rgb: &[u8], w: usize, h: usize) -> Vec<rten_imageproc::RotatedRect> {
    let luma = |i: usize| ((rgb[i * 3] as u32 * 77 + rgb[i * 3 + 1] as u32 * 150 + rgb[i * 3 + 2] as u32 * 29) >> 8) as usize;
    let spread = |hist: &[u32; 256], lo: f32| {
        let total: u32 = hist.iter().sum();
        let at = |q: f32| {
            let target = (total as f32 * q) as u32;
            let mut sum = 0;
            hist.iter().position(|&n| {
                sum += n;
                sum > target
            })
            .unwrap_or(255)
        };
        at(1.0 - lo) as i32 - at(lo) as i32
    };
    let mut hist = [0u32; 256];
    // Every fourth pixel of every fourth row is plenty for the whole image.
    for y in (0..h).step_by(4) {
        for x in (0..w).step_by(4) {
            hist[luma(y * w + x)] += 1;
        }
    }
    let least = (spread(&hist, 0.01) / 4).max(16);
    words
        .into_iter()
        .filter(|word| {
            let b = rten_imageproc::BoundingRect::bounding_rect(word);
            let (x0, y0) = (b.left().max(0.0) as usize, b.top().max(0.0) as usize);
            let (x1, y1) = ((b.right().max(0.0) as usize).min(w), (b.bottom().max(0.0) as usize).min(h));
            let mut hist = [0u32; 256];
            for y in y0..y1 {
                for x in x0..x1 {
                    hist[luma(y * w + x)] += 1;
                }
            }
            x1 > x0 && y1 > y0 && spread(&hist, 0.05) >= least
        })
        .collect()
}

/// Letters that look the same as capitals and small letters (only their
/// size differs), which the network now and then gets the wrong way round
/// ("PGo", "macoS"), take the case of the rest of their word when that is
/// all one case: the word's other letters, at least two of them. (Except
/// that capitals are often followed by a small s: "APIs", "TACO's".)
fn same_case(mut word: Vec<char>) -> Vec<char> {
    const LOOK_ALIKE: &str = "cosuvwxzCOSUVWXZ";
    for i in 0..word.len() {
        if !LOOK_ALIKE.contains(word[i]) {
            continue;
        }
        // Capitalized words are normal, so a first letter is only made a
        // capital, in a word of capitals.
        let others = word.iter().enumerate().filter(|&(j, c)| j != i && c.is_alphabetic());
        let (others, upper) = others.fold((0, 0), |(n, u), (_, c)| (n + 1, u + c.is_uppercase() as usize));
        let plural = word[i] == 's' && !word[i + 1..].iter().any(|c| c.is_alphabetic());
        if others >= 2 && upper == others && !plural {
            word[i] = word[i].to_ascii_uppercase();
        } else if others >= 2 && upper == 0 && i > 0 {
            word[i] = word[i].to_ascii_lowercase();
        }
    }
    word
}

/// Whether an RGB tile is blank paper (a page's margins), with no words
/// to find: fewer than 24 of its pixels differ from its middling
/// brightness by more than 56 (a scan's speckles hardly ever do; a page
/// number has a hundred or so).
fn blank(rgb: &[u8]) -> bool {
    let luma = |p: &[u8]| ((p[0] as u16 + 2 * p[1] as u16 + p[2] as u16) / 4) as u8;
    let mut hist = [0u32; 256];
    for p in rgb.chunks_exact(3) {
        hist[luma(p) as usize] += 1;
    }
    let half = (rgb.len() / 6) as u32;
    let mut sum = 0;
    let middle = hist.iter().position(|&n| {
        sum += n;
        sum > half
    })
    .unwrap_or(0);
    let far: u32 = hist.iter().enumerate().filter(|&(v, _)| v.abs_diff(middle) > 56).map(|(_, &n)| n).sum();
    far < 24
}

/// The size of image the word-finding network takes (width, height).
const DETECT_SIZE: (usize, usize) = (600, 800);

/// The words grouped into lines, in reading order (ocrs's layout).
fn text_lines(engine: &ocrs::OcrEngine, words: &[rten_imageproc::RotatedRect]) -> Vec<Vec<rten_imageproc::RotatedRect>> {
    // `find_text_lines` doesn't look at the image.
    let empty = engine.prepare_input(ocrs::ImageSource::from_bytes(&[255; 3], (1, 1)).expect("one pixel")).expect("one pixel");
    engine.find_text_lines(&empty, words)
}

/// The angle (radians, clockwise) that the text runs at, if the image
/// needs turning to level it: sideways, or off level by more than 0.4°.
///
/// Whether text runs across or down the image is told by the words' long
/// sides (each word votes by its length). The slant is then measured more
/// finely along whole lines: the line through each line's words' middles,
/// the median over the lines by their length.
fn text_angle(engine: &ocrs::OcrEngine, words: &[rten_imageproc::RotatedRect]) -> Option<f32> {
    use rten_imageproc::{PointF, RotatedRect, Vec2};
    use std::f32::consts::FRAC_PI_2;
    let (mut across, mut down, mut long) = (0.0, 0.0, 0);
    for word in words {
        let [a, b, c, _] = word.corners();
        let (e1, e2) = ((b.x - a.x, b.y - a.y), (c.x - b.x, c.y - b.y));
        let (l1, l2) = (e1.0.hypot(e1.1), e2.0.hypot(e2.1));
        let (along, length, width) = if l1 >= l2 { (e1, l1, l2) } else { (e2, l2, l1) };
        if length >= 2.5 * width {
            long += 1;
            if along.0.abs() >= along.1.abs() {
                across += length;
            } else {
                down += length;
            }
        }
    }
    let sideways = long >= 6 && down > 2.0 * across;
    // Lines are found in text that runs across, so sideways words are
    // first turned a quarter (anticlockwise), keeping coordinates positive.
    let words: Vec<RotatedRect> = if sideways {
        let right = words.iter().map(|w| w.center().x).fold(0.0, f32::max);
        words
            .iter()
            .map(|w| {
                let (c, u) = (w.center(), w.up_axis());
                RotatedRect::new(PointF::from_yx(right - c.x, c.y), Vec2::from_yx(-u.x, u.y), w.width(), w.height())
            })
            .collect()
    } else {
        words.to_vec()
    };
    let mut slopes: Vec<(f32, f32)> = Vec::new();
    for line in text_lines(engine, &words) {
        if line.len() < 3 {
            continue;
        }
        let points: Vec<(f32, f32)> = line.iter().map(|w| (w.center().x, w.center().y)).collect();
        let n = points.len() as f32;
        let (mx, my) = points.iter().fold((0.0, 0.0), |a, p| (a.0 + p.0 / n, a.1 + p.1 / n));
        let (sxx, sxy) = points.iter().fold((0.0, 0.0), |a, p| (a.0 + (p.0 - mx).powi(2), a.1 + (p.0 - mx) * (p.1 - my)));
        if sxx > 0.0 {
            slopes.push(((sxy / sxx).atan(), sxx.sqrt()));
        }
    }
    let fine = if slopes.len() < 3 {
        0.0
    } else {
        slopes.sort_by(|a, b| a.0.total_cmp(&b.0));
        let half = slopes.iter().map(|s| s.1).sum::<f32>() / 2.0;
        let mut sum = 0.0;
        slopes.iter().find(|s| {
            sum += s.1;
            sum >= half
        })?
        .0
    };
    // More than this is a muddle of lines rather than a crooked page.
    let fine = if fine.abs() > 15f32.to_radians() { 0.0 } else { fine };
    if sideways {
        // Turned back: the text runs down at a quarter turn plus the slant.
        Some(fine + FRAC_PI_2)
    } else {
        (fine.abs() > 0.4f32.to_radians()).then_some(fine)
    }
}

/// How an image was turned for reading: by `angle` (clockwise) about its
/// middle, onto a canvas `size` big that holds all of it.
struct Turn {
    angle: f32,
    from: (f32, f32),
    to: (f32, f32),
    size: (usize, usize),
}

impl Turn {
    fn new((w, h): (usize, usize), angle: f32) -> Turn {
        let (sin, cos) = angle.sin_cos();
        let nw = (w as f32 * cos.abs() + h as f32 * sin.abs()).round() as usize;
        let nh = (w as f32 * sin.abs() + h as f32 * cos.abs()).round() as usize;
        Turn { angle, from: (w as f32 / 2.0, h as f32 / 2.0), to: (nw as f32 / 2.0, nh as f32 / 2.0), size: (nw.max(1), nh.max(1)) }
    }

    /// The same, then half a turn more.
    fn and_half_turn(self) -> Turn {
        Turn { angle: self.angle + std::f32::consts::PI, ..self }
    }

    /// A word's outline in the turned image.
    fn rect_forward(&self, r: &rten_imageproc::RotatedRect) -> rten_imageproc::RotatedRect {
        let (sin, cos) = self.angle.sin_cos();
        let turn = |x: f32, y: f32| (cos * x + sin * y, -sin * x + cos * y);
        let (c, u) = (r.center(), r.up_axis());
        let (x, y) = turn(c.x - self.from.0, c.y - self.from.1);
        let (ux, uy) = turn(u.x, u.y);
        rten_imageproc::RotatedRect::new(
            rten_imageproc::PointF::from_yx(y + self.to.1, x + self.to.0),
            rten_imageproc::Vec2::from_yx(uy, ux),
            r.width(),
            r.height(),
        )
    }

    /// Where a point of the turned image was in the original.
    fn point_back(&self, (x, y): (f32, f32)) -> (f32, f32) {
        let (sin, cos) = self.angle.sin_cos();
        let (dx, dy) = (x - self.to.0, y - self.to.1);
        (cos * dx - sin * dy + self.from.0, sin * dx + cos * dy + self.from.1)
    }

    /// A rectangle of the turned image, as one in the original (holding
    /// its four corners).
    fn back(&self, r: [f32; 4]) -> [f32; 4] {
        let corners = [(r[0], r[1]), (r[2], r[1]), (r[2], r[3]), (r[0], r[3])].map(|p| self.point_back(p));
        corners.iter().fold([f32::MAX, f32::MAX, f32::MIN, f32::MIN], |a, p| [a[0].min(p.0), a[1].min(p.1), a[2].max(p.0), a[3].max(p.1)])
    }
}

/// Turns an RGB image so that text running at `angle` comes out level.
/// Corners filled in are given the image's middling color (its paper,
/// usually), so they don't make edges that look like print.
fn rotate(rgb: &[u8], w: usize, h: usize, angle: f32) -> (Vec<u8>, Turn) {
    let turn = Turn::new((w, h), angle);
    let (nw, nh) = turn.size;
    let mut fill = [0u8; 3];
    for (c, f) in fill.iter_mut().enumerate() {
        let mut hist = [0u32; 256];
        for i in (0..w * h).step_by(97) {
            hist[rgb[i * 3 + c] as usize] += 1;
        }
        let half: u32 = hist.iter().sum::<u32>() / 2;
        let mut sum = 0;
        *f = hist.iter().position(|&n| {
            sum += n;
            sum > half
        })
        .unwrap_or(255) as u8;
    }
    let mut out = vec![0u8; nw * nh * 3];
    for y in 0..nh {
        for x in 0..nw {
            let (sx, sy) = turn.point_back((x as f32 + 0.5, y as f32 + 0.5));
            let (fx, fy) = (sx - 0.5, sy - 0.5);
            let (x0, y0) = (fx.floor(), fy.floor());
            let (ax, ay) = (fx - x0, fy - y0);
            let (x0, y0) = (x0 as isize, y0 as isize);
            let at = |xx: isize, yy: isize, c: usize| -> f32 {
                if xx < 0 || yy < 0 || xx >= w as isize || yy >= h as isize {
                    fill[c] as f32
                } else {
                    rgb[(yy as usize * w + xx as usize) * 3 + c] as f32
                }
            };
            for c in 0..3 {
                let top = at(x0, y0, c) * (1.0 - ax) + at(x0 + 1, y0, c) * ax;
                let bottom = at(x0, y0 + 1, c) * (1.0 - ax) + at(x0 + 1, y0 + 1, c) * ax;
                out[(y * nw + x) * 3 + c] = (top * (1.0 - ay) + bottom * ay).round() as u8;
            }
        }
    }
    (out, turn)
}

/// Whether level text is upside down. Latin print has much more ink above
/// its lowercase letters' middle band (ascenders, capitals, digits, accents)
/// than below it (descenders), so in each word the rows above and below
/// that band are weighed, over all the words. (Words rather than lines,
/// as a slight slant left over would blur a whole line's band.)
fn upside_down(rgb: &[u8], w: usize, h: usize, words: &[rten_imageproc::RotatedRect]) -> bool {
    use rten_imageproc::BoundingRect;
    let luma = |x: usize, y: usize| {
        let i = (y * w + x) * 3;
        ((rgb[i] as u32 * 77 + rgb[i + 1] as u32 * 150 + rgb[i + 2] as u32 * 29) >> 8) as u8
    };
    let (mut above, mut below, mut counted) = (0u64, 0u64, 0);
    for word in words {
        let b = word.bounding_rect();
        let (x0, y0) = (b.left().max(0.0) as usize, b.top().max(0.0) as usize);
        let (x1, y1) = ((b.right().max(0.0) as usize).min(w), (b.bottom().max(0.0) as usize).min(h));
        if y1 < y0 + 8 || x1 < x0 + 2 * (y1 - y0) {
            continue;
        }
        let mut hist = [0u32; 256];
        for y in y0..y1 {
            for x in x0..x1 {
                hist[luma(x, y) as usize] += 1;
            }
        }
        let total: u32 = hist.iter().sum();
        let at = |q: f32| {
            let mut sum = 0;
            hist.iter().position(|&n| {
                sum += n;
                sum as f32 > total as f32 * q
            })
            .unwrap_or(255) as u8
        };
        let (dark, light, middle) = (at(0.05), at(0.95), at(0.5));
        if light < dark.saturating_add(40) {
            continue;
        }
        let threshold = ((dark as u32 + light as u32) / 2) as u8;
        // Ink is whichever side of the threshold the background isn't.
        let ink_is_dark = middle > threshold;
        let rows: Vec<u32> = (y0..y1)
            .map(|y| (x0..x1).filter(|&x| (luma(x, y) < threshold) == ink_is_dark).count() as u32)
            .collect();
        let most = *rows.iter().max().unwrap_or(&0);
        if most == 0 {
            continue;
        }
        let Some(top) = rows.iter().position(|&n| n * 2 >= most) else { continue };
        let bottom = rows.iter().rposition(|&n| n * 2 >= most).unwrap_or(top);
        counted += 1;
        above += rows[..top].iter().map(|&n| n as u64).sum::<u64>();
        below += rows[bottom + 1..].iter().map(|&n| n as u64).sum::<u64>();
    }
    counted >= 5 && below > above + above / 2 && below > 200
}

/// Splits `0..len` into tiles `size` long, overlapping by at least 64.
fn tiles(len: usize, size: usize) -> Vec<(usize, usize)> {
    if len <= size {
        return vec![(0, len)];
    }
    let n = (len - 64).div_ceil(size - 64);
    (0..n).map(|i| (i * (len - size) / (n - 1), i * (len - size) / (n - 1) + size)).collect()
}

/// The part of tile `i` that is its own: up to halfway through its
/// overlaps with its neighbors.
fn core(tiles: &[(usize, usize)], i: usize) -> (usize, usize) {
    let start = if i == 0 { tiles[0].0 } else { (tiles[i].0 + tiles[i - 1].1) / 2 };
    let end = if i + 1 == tiles.len() { tiles[i].1 } else { (tiles[i].1 + tiles[i + 1].0) / 2 };
    (start, end)
}

/// Lines read at once.
const LINES_AT_ONCE: usize = 8;

/// Gives memory freed by reading back to the system. The networks need a
/// few hundred megabytes while reading a page, spread over threads; glibc
/// keeps such memory for reuse, which in a long-running viewer would stay
/// with the process. (Elsewhere this does nothing.)
pub fn release_memory() {
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    {
        unsafe extern "C" {
            fn malloc_trim(pad: usize) -> std::ffi::c_int;
        }
        // SAFETY: malloc_trim only returns free memory to the system.
        unsafe {
            malloc_trim(0);
        }
    }
}

/// Where the networks' files are looked for, in order:
/// - the folder in `K_PDF_OCR_MODELS`;
/// - `models` next to the program (Windows and Linux packages), `../share/k-pdf/models`
///   (installed under a prefix) and `../Resources/models` (the macOS app);
/// - the user's data folder: `~/.local/share/k-pdf/models` (or under
///   `$XDG_DATA_HOME`), `~/Library/Application Support/k-pdf/models`, or
///   `%LOCALAPPDATA%\k-pdf\models`.
pub fn model_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(d) = std::env::var_os("K_PDF_OCR_MODELS") {
        dirs.push(PathBuf::from(d));
    }
    if let Some(dir) = std::env::current_exe().ok().and_then(|e| e.parent().map(Path::to_path_buf)) {
        dirs.push(dir.join("models"));
        dirs.push(dir.join("../share/k-pdf/models"));
        dirs.push(dir.join("../Resources/models"));
    }
    if let Some(d) = user_models_dir() {
        dirs.push(d);
    }
    dirs
}

/// The user's own models folder (where `fetch-ocr-models.sh` puts them).
pub fn user_models_dir() -> Option<PathBuf> {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    if cfg!(windows) {
        std::env::var_os("LOCALAPPDATA").map(|d| PathBuf::from(d).join("k-pdf").join("models"))
    } else if cfg!(target_os = "macos") {
        home.map(|h| h.join("Library/Application Support/k-pdf/models"))
    } else {
        std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .filter(|d| d.is_absolute())
            .or_else(|| home.map(|h| h.join(".local/share")))
            .map(|d| d.join("k-pdf/models"))
    }
}

/// The first of [`model_dirs`] that has both files.
pub fn find_models() -> Option<PathBuf> {
    model_dirs().into_iter().find(|d| MODEL_FILES.iter().all(|f| d.join(f).is_file()))
}

/// What to tell users when the models aren't there.
pub fn not_installed() -> String {
    let dir = user_models_dir().map(|d| d.display().to_string()).unwrap_or_else(|| "a models folder next to k-pdf".into());
    format!(
        "the text-recognition models aren't installed: put {} and {} in {dir} (packaging/fetch-ocr-models.sh downloads them)",
        MODEL_FILES[0], MODEL_FILES[1]
    )
}

/// Whether a page draws any images (image XObjects, also inside form
/// XObjects, or inline images). Quick: nothing is decoded.
pub fn page_has_images(doc: &Document, page: &Page) -> bool {
    let resources = doc.get_dict(&page.dict, b"Resources").unwrap_or_default();
    if has_image_xobjects(doc, &resources, 0) {
        return true;
    }
    doc.page_content(page)
        .is_ok_and(|c| c.windows(4).any(|w| w[0].is_ascii_whitespace() && &w[1..3] == b"BI" && w[3].is_ascii_whitespace()))
}

fn has_image_xobjects(doc: &Document, resources: &Dictionary, depth: usize) -> bool {
    let Some(xobjects) = doc.get_dict(resources, b"XObject") else { return false };
    xobjects.iter().any(|(_, v)| {
        let Ok(Object::Stream(s)) = doc.resolve(v) else { return false };
        match s.dict.get(b"Subtype").and_then(Object::as_name) {
            Some(b"Image") => true,
            Some(b"Form") if depth < 4 => {
                doc.get_dict(&s.dict, b"Resources").is_some_and(|r| has_image_xobjects(doc, &r, depth + 1))
            }
            _ => false,
        }
    })
}

/// Reads the words in the images of `page`, as characters with rectangles
/// in display points. Only the areas images are drawn in are read, and
/// words lying on the page's own `text` are left out (scans that already
/// have a text layer, captions over pictures), so nothing is found twice.
pub fn read_page_images(ocr: &Ocr, doc: &Document, renderer: &mut Renderer, page: &Page, text: &[TextChar]) -> Result<Vec<TextChar>> {
    if !page_has_images(doc, page) {
        return Ok(Vec::new());
    }
    let (w, h) = page.display_size(doc);
    let scale = SCALE.min(MAX_SIDE / w.max(h).max(1.0));
    let rendered = renderer.render_page(page, scale)?;
    let (pw, ph) = (rendered.width as usize, rendered.height as usize);

    // Images big enough to hold words, joined where they overlap.
    let mut regions: Vec<[f32; 4]> = Vec::new();
    for r in rendered.images.iter().filter(|r| r[2] - r[0] >= 24.0 && r[3] - r[1] >= 12.0) {
        let mut r = [r[0] - 4.0, r[1] - 4.0, r[2] + 4.0, r[3] + 4.0];
        while let Some(i) = regions.iter().position(|o| o[0] < r[2] && r[0] < o[2] && o[1] < r[3] && r[1] < o[3]) {
            let o = regions.remove(i);
            r = [r[0].min(o[0]), r[1].min(o[1]), r[2].max(o[2]), r[3].max(o[3])];
        }
        regions.push(r);
    }
    regions.sort_by(|a, b| a[1].total_cmp(&b[1]).then(a[0].total_cmp(&b[0])));

    let own: Vec<[f32; 4]> = text.iter().filter_map(|c| c.rect).collect();
    let mut out: Vec<TextChar> = Vec::new();
    for r in regions {
        let x0 = (r[0].max(0.0) as usize).min(pw);
        let y0 = (r[1].max(0.0) as usize).min(ph);
        let x1 = (r[2].ceil().max(0.0) as usize).min(pw);
        let y1 = (r[3].ceil().max(0.0) as usize).min(ph);
        if x1 <= x0 + 8 || y1 <= y0 + 8 {
            continue;
        }
        let mut rgb = Vec::with_capacity((x1 - x0) * (y1 - y0) * 3);
        for y in y0..y1 {
            for px in rendered.pixels[(y * pw + x0) * 4..(y * pw + x1) * 4].chunks_exact(4) {
                rgb.extend_from_slice(&px[..3]);
            }
        }
        let found = ocr.read(&rgb, (x1 - x0) as u32, (y1 - y0) as u32)?;
        // To display points.
        let s = scale as f32;
        let mut chars: Vec<TextChar> = found
            .into_iter()
            .map(|c| TextChar {
                text: c.text,
                rect: c.rect.map(|q| [(q[0] + x0 as f32) / s, (q[1] + y0 as f32) / s, (q[2] + x0 as f32) / s, (q[3] + y0 as f32) / s]),
            })
            .collect();
        drop_covered_words(&mut chars, &own);
        if chars.iter().any(|c| c.rect.is_some()) {
            if !out.is_empty() {
                out.push(TextChar { text: "\n".into(), rect: None });
            }
            out.extend(chars);
        }
    }
    Ok(out)
}

/// Removes the words that mostly lie on the page's own text.
fn drop_covered_words(chars: &mut Vec<TextChar>, own: &[[f32; 4]]) {
    if own.is_empty() {
        return;
    }
    let mut keep = vec![true; chars.len()];
    let mut start = 0;
    while start < chars.len() {
        let end = (start..chars.len()).find(|&i| chars[i].rect.is_none()).unwrap_or(chars.len());
        if end > start {
            let rects: Vec<[f32; 4]> = chars[start..end].iter().filter_map(|c| c.rect).collect();
            let word = rects.iter().fold(rects[0], |a, r| [a[0].min(r[0]), a[1].min(r[1]), a[2].max(r[2]), a[3].max(r[3])]);
            let area = ((word[2] - word[0]) * (word[3] - word[1])).max(1e-3);
            let covered: f32 = own
                .iter()
                .map(|o| (word[2].min(o[2]) - word[0].max(o[0])).max(0.0) * (word[3].min(o[3]) - word[1].max(o[1])).max(0.0))
                .sum();
            if covered > 0.3 * area {
                keep[start..end].iter_mut().for_each(|k| *k = false);
            }
        }
        start = end + 1;
    }
    let mut i = 0;
    chars.retain(|_| {
        i += 1;
        keep[i - 1]
    });
    // Tidy the separators left behind.
    let mut tidy: Vec<TextChar> = Vec::with_capacity(chars.len());
    for c in chars.drain(..) {
        if c.rect.is_none() && tidy.last().is_none_or(|l| l.rect.is_none()) {
            if c.text == "\n"
                && let Some(last) = tidy.last_mut()
            {
                last.text = "\n".into();
            }
            continue;
        }
        tidy.push(c);
    }
    while tidy.last().is_some_and(|c| c.rect.is_none()) {
        tidy.pop();
    }
    *chars = tidy;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ch(t: &str, x: f32) -> TextChar {
        TextChar { text: t.into(), rect: if t == " " || t == "\n" { None } else { Some([x, 0.0, x + 5.0, 10.0]) } }
    }

    #[test]
    fn words_on_the_page_text_are_dropped() {
        let mut chars = vec![ch("a", 0.0), ch("b", 5.0), ch(" ", 0.0), ch("c", 20.0), ch("d", 25.0)];
        drop_covered_words(&mut chars, &[[0.0, 0.0, 10.0, 10.0]]);
        let text: String = chars.iter().map(|c| c.text.as_str()).collect();
        assert_eq!(text, "cd");
    }

    #[test]
    fn tiles_cover_the_image_with_overlaps() {
        assert_eq!(tiles(500, 600), vec![(0, 500)]);
        for len in [601, 1200, 1530, 3200] {
            let t = tiles(len, 600);
            assert_eq!(t[0].0, 0);
            assert_eq!(t.last().unwrap().1, len);
            assert!(t.iter().all(|&(a, b)| b - a == 600));
            assert!(t.windows(2).all(|w| w[0].1 >= w[1].0 + 64));
            // The tiles' own parts follow on from each other.
            let cores: Vec<_> = (0..t.len()).map(|i| core(&t, i)).collect();
            assert!(cores.windows(2).all(|w| w[0].1 == w[1].0));
            assert!(cores.iter().zip(&t).all(|(c, t)| t.0 <= c.0 && c.1 <= t.1));
        }
    }

    #[test]
    fn look_alike_letters_take_their_words_case() {
        let fix = |w: &str| same_case(w.chars().collect()).into_iter().collect::<String>();
        assert_eq!(fix("PGo"), "PGO");
        assert_eq!(fix("LTo)"), "LTO)");
        assert_eq!(fix("deveLopment"), "deveLopment");
        assert_eq!(fix("deCision"), "decision");
        assert_eq!(fix("Sets"), "Sets");
        assert_eq!(fix("APIs"), "APIs");
        assert_eq!(fix("TACO's"), "TACO's");
        assert_eq!(fix("oS"), "oS");
        assert_eq!(fix("macOS"), "macOS");
    }

    #[test]
    fn turned_rectangles_come_back() {
        for angle in [0.03f32, -0.05, std::f32::consts::FRAC_PI_2 + 0.01, std::f32::consts::PI] {
            let turn = Turn::new((1000, 1500), angle);
            let word = rten_imageproc::RotatedRect::new(
                rten_imageproc::PointF::from_yx(300.0, 200.0),
                rten_imageproc::Vec2::from_yx(-1.0, 0.0),
                80.0,
                20.0,
            );
            let turned = turn.rect_forward(&word);
            let b = rten_imageproc::BoundingRect::bounding_rect(&turned);
            let back = turn.back([b.left(), b.top(), b.right(), b.bottom()]);
            let (cx, cy) = ((back[0] + back[2]) / 2.0, (back[1] + back[3]) / 2.0);
            assert!((cx - 200.0).abs() < 0.5 && (cy - 300.0).abs() < 0.5, "{angle}: {cx} {cy}");
            // Inside the turned canvas.
            let c = turned.center();
            assert!(c.x > 0.0 && c.y > 0.0 && c.x < turn.size.0 as f32 && c.y < turn.size.1 as f32);
        }
        // A quarter turn swaps the sides.
        assert_eq!(Turn::new((1000, 1500), std::f32::consts::FRAC_PI_2).size, (1500, 1000));
    }

    #[test]
    fn blank_paper_is_skipped_but_not_a_page_number() {
        let mut noisy: Vec<u8> = (0..600 * 800 * 3).map(|i: usize| 235 + (i.wrapping_mul(2654435761) >> 7 & 15) as u8).collect();
        assert!(blank(&noisy));
        for y in 400..420 {
            for x in 300..306 {
                noisy[(y * 600 + x) * 3..][..3].copy_from_slice(&[20, 20, 20]);
            }
        }
        assert!(!blank(&noisy));
    }

    #[test]
    fn model_dirs_include_the_users_folder() {
        assert!(model_dirs().iter().any(|d| d.ends_with("models")));
        assert!(not_installed().contains(MODEL_FILES[1]));
    }
}
