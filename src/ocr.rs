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
    /// Memory is kept down (the networks' working memory grows with the
    /// image): words are found in bands of the image, and lines are read a
    /// few at a time.
    pub fn read(&self, rgb: &[u8], width: u32, height: u32) -> Result<Vec<TextChar>> {
        let source = ocrs::ImageSource::from_bytes(rgb, (width, height)).map_err(failed)?;
        let input = self.engine.prepare_input(source).map_err(failed)?;
        let words = self.find_words(rgb, width, height)?;
        let lines = self.engine.find_text_lines(&input, &words);
        let mut read = Vec::with_capacity(lines.len());
        for group in lines.chunks(LINES_AT_ONCE) {
            read.extend(self.engine.recognize_text(&input, group).map_err(failed)?);
        }
        let lines = read;
        let mut out = Vec::new();
        for line in lines.into_iter().flatten() {
            if !out.is_empty() {
                out.push(TextChar { text: "\n".into(), rect: None });
            }
            for (i, word) in ocrs::TextItem::chars(&line).split(|c| c.char == ' ').filter(|w| !w.is_empty()).enumerate() {
                if i > 0 {
                    out.push(TextChar { text: " ".into(), rect: None });
                }
                for c in word {
                    let r = c.rect;
                    out.push(TextChar {
                        text: c.char.to_string(),
                        rect: Some([r.left() as f32, r.top() as f32, r.right() as f32, r.bottom() as f32]),
                    });
                }
            }
        }
        Ok(out)
    }

    /// The words' outlines, found band by band. Bands overlap by more than
    /// a line of large print, and each word is kept by the band its middle
    /// is in (away from the overlap's other half).
    fn find_words(&self, rgb: &[u8], width: u32, height: u32) -> Result<Vec<rten_imageproc::RotatedRect>> {
        let mut words = Vec::new();
        let row = width as usize * 3;
        let mut y0 = 0;
        loop {
            let y1 = (y0 + BAND + OVERLAP).min(height);
            let band = &rgb[y0 as usize * row..y1 as usize * row];
            let source = ocrs::ImageSource::from_bytes(band, (width, y1 - y0)).map_err(failed)?;
            let input = self.engine.prepare_input(source).map_err(failed)?;
            let lo = if y0 == 0 { f32::MIN } else { (y0 + OVERLAP / 2) as f32 };
            let hi = if y1 == height { f32::MAX } else { (y1 - OVERLAP / 2) as f32 };
            for w in self.engine.detect_words(&input).map_err(failed)? {
                let center = w.center().translate(y0 as f32, 0.0);
                if (lo..hi).contains(&center.y) {
                    words.push(rten_imageproc::RotatedRect::new(center, w.up_axis(), w.width(), w.height()));
                }
            }
            if y1 == height {
                break;
            }
            y0 = y1 - OVERLAP;
        }
        Ok(words)
    }
}

/// Words are found in bands of the image this tall (pixels)…
const BAND: u32 = 1024;
/// … overlapping by this much.
const OVERLAP: u32 = 128;
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
    fn model_dirs_include_the_users_folder() {
        assert!(model_dirs().iter().any(|d| d.ends_with("models")));
        assert!(not_installed().contains(MODEL_FILES[1]));
    }
}
