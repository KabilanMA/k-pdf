//! Editing the text of a page.
//!
//! [`page_text`] finds the text a page draws itself, character by
//! character, with each character's font, size and color, grouped into
//! lines and paragraph blocks. [`edit_text`] replaces a block (or adds a
//! new one): the original characters are taken out of the page content,
//! and the new text is laid out in their place, word-wrapped to the
//! block's width, with per-character fonts, sizes, colors, bold, italic and
//! underline, alignment and line spacing.
//!
//! Text keeps its original font whenever that font has the characters
//! needed; otherwise a matching system font is embedded (with just the
//! glyphs used), or a standard font is used (see [`fonts`]).
//!
//! Limits: text drawn inside form XObjects, rotated or skewed text, and
//! invisible text (e.g. OCR layers) are not editable.

pub mod annotate;
mod content;
pub mod fields;
pub mod fonts;
mod picture;
mod redact;
#[cfg(feature = "render")]
mod subset;

use std::collections::{HashMap, HashSet};
use std::io::Write as _;

pub use content::Glyph;
pub use fonts::FontLook;
pub use picture::place_image;
pub use redact::{find_text, redact_page, Redacted};

use crate::document::Document;
use crate::error::{Error, Result};
use crate::font::Font;
use crate::object::{Dictionary, ObjRef, Object, Stream};
use crate::writer::Changes;

/// Characters a font is known to have: char → (code bytes, width per unit
/// of font size).
type FontGlyphs = HashMap<char, (Vec<u8>, f32)>;

/// The editable text of a page.
#[derive(Debug, Clone)]
pub struct PageText {
    pub glyphs: Vec<Glyph>,
    pub blocks: Vec<TextBlock>,
    /// The look of each font the page uses, by resource name.
    pub fonts: HashMap<Vec<u8>, FontLook>,
    /// For each page font: the characters it is known to have (the page
    /// shows them), with their codes and widths.
    font_glyphs: HashMap<Vec<u8>, FontGlyphs>,
}

/// A paragraph-like group of lines.
#[derive(Debug, Clone)]
pub struct TextBlock {
    /// Indices into [`PageText::glyphs`], in reading order.
    pub glyphs: Vec<usize>,
    pub lines: Vec<TextLine>,
    /// Bounds in display points.
    pub rect: [f32; 4],
    /// Upright, visible text that can be edited.
    pub editable: bool,
    /// How the lines seem to be aligned.
    pub align: Align,
    /// Distance between baselines as a multiple of the font size.
    pub line_spacing: f32,
    /// First-line indent of its paragraphs, in points.
    pub indent: f32,
}

#[derive(Debug, Clone)]
pub struct TextLine {
    pub glyphs: Vec<usize>,
    /// Baseline height in display points.
    pub baseline: f32,
    pub left: f32,
    pub right: f32,
    /// Starts a new paragraph within its block.
    pub paragraph_start: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Align {
    #[default]
    Left,
    Center,
    Right,
    Justify,
}

/// How a piece of text looks.
#[derive(Debug, Clone, PartialEq)]
pub struct TextStyle {
    /// Font family, e.g. "Times New Roman".
    pub family: String,
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    /// Size in points.
    pub size: f32,
    /// RGB, 0–1.
    pub color: [f32; 3],
}

impl Default for TextStyle {
    fn default() -> Self {
        TextStyle { family: "Helvetica".into(), bold: false, italic: false, underline: false, size: 12.0, color: [0.0; 3] }
    }
}

/// A replacement for a block of text, or a new block.
#[derive(Debug, Clone)]
pub struct BlockEdit {
    /// Index into [`Document::pages`].
    pub page: usize,
    /// Glyphs to remove (indices into [`PageText::glyphs`] of the page as
    /// it is now); empty for a new block.
    pub remove: Vec<usize>,
    /// The new text with its styles; `'\n'` starts a new paragraph.
    pub runs: Vec<(String, TextStyle)>,
    /// Left edge and width of the text area, in display points.
    pub left: f32,
    pub width: f32,
    /// Baseline of the first line, in display points.
    pub first_baseline: f32,
    /// Distance between baselines as a multiple of the line's font size.
    pub line_spacing: f32,
    pub align: Align,
    /// Extra indent of each paragraph's first line, in points.
    pub indent: f32,
    /// Word-wrap at `width`. Without wrapping (for headings and other
    /// one-line text), lines grow as needed and are aligned to the box:
    /// left-aligned text starts at `left`, centered text stays centered on
    /// the box, right-aligned text ends at its right edge.
    pub wrap: bool,
}

/// Finds the editable text of `doc.pages()[page]`.
pub fn page_text(doc: &Document, page: usize) -> Result<PageText> {
    let pages = doc.pages()?;
    let page = pages.get(page).ok_or_else(|| Error::Object(format!("page {} does not exist", page + 1)))?;
    let resources = doc.get_dict(&page.dict, b"Resources").unwrap_or_default();
    let data = doc.page_content(page)?;
    let ops = content::parse_ops(&data);
    let glyphs = content::glyphs(doc, &page.display_matrix(doc), &ops, &resources);

    // Font looks, and the characters each font is known to have.
    let font_dict = doc.get_dict(&resources, b"Font").unwrap_or_default();
    let mut fonts = HashMap::new();
    let mut font_glyphs: HashMap<Vec<u8>, FontGlyphs> = HashMap::new();
    for g in &glyphs {
        if !fonts.contains_key(&g.font) {
            let dict = font_dict.get(&g.font).and_then(|f| doc.resolve(f).ok()).and_then(|o| o.as_dict().cloned());
            let look = dict.map_or_else(|| fonts::look_of("Helvetica", 0), |d| font_look(doc, &d));
            fonts.insert(g.font.clone(), look);
        }
        let mut chars = g.text.chars();
        if let (Some(c), None) = (chars.next(), chars.next()) {
            let bytes = g.code.to_be_bytes()[4 - g.len.clamp(1, 4)..].to_vec();
            font_glyphs.entry(g.font.clone()).or_default().entry(c).or_insert((bytes, g.width));
        }
    }

    let mut blocks = group(&glyphs);
    // A single line centered on the page is a centered heading.
    let (page_width, _) = page.display_size(doc);
    for b in blocks.iter_mut().filter(|b| b.lines.len() == 1) {
        let center = (b.rect[0] + b.rect[2]) / 2.0;
        if (center - page_width as f32 / 2.0).abs() < 0.02 * page_width as f32 {
            b.align = Align::Center;
        }
    }
    Ok(PageText { glyphs, blocks, fonts, font_glyphs })
}

fn font_look(doc: &Document, dict: &Dictionary) -> FontLook {
    let base = dict.get(b"BaseFont").and_then(Object::as_name).map(|n| String::from_utf8_lossy(n).into_owned());
    let info = Font::load(doc, dict);
    let flags = info
        .descriptor
        .as_ref()
        .and_then(|d| doc.get_resolved(d, b"Flags").ok())
        .and_then(|o| o.as_i64())
        .unwrap_or(0);
    let desc_num = |key: &[u8]| {
        info.descriptor.as_ref().and_then(|d| doc.get_resolved(d, key).ok()).and_then(|o| o.as_f64())
    };
    // Heavy stems also mean bold when the name doesn't say so.
    let weight_bold = desc_num(b"FontWeight").is_some_and(|w| w >= 600.0) || desc_num(b"StemV").is_some_and(|v| v >= 120.0);
    let mut look = fonts::look_of(base.as_deref().unwrap_or("Helvetica"), flags);
    look.bold |= weight_bold;
    look
}

impl PageText {
    /// The text of a block and the style of each character, ready for
    /// editing: words get spaces between them, the lines of a paragraph
    /// are joined (a hyphen at a line end joins the word back), and the
    /// result is one run per character.
    pub fn block_text(&self, block: &TextBlock) -> Vec<(char, TextStyle)> {
        let mut out: Vec<(char, TextStyle)> = Vec::new();
        for (li, line) in block.lines.iter().enumerate() {
            if li > 0 && line.paragraph_start {
                let style = out.last().map(|(_, s)| s.clone()).unwrap_or_default();
                while out.last().is_some_and(|(c, _)| *c == ' ') {
                    out.pop();
                }
                out.push(('\n', style));
            } else if li > 0 {
                let prev_hyphen = out.last().is_some_and(|(c, _)| *c == '-');
                let next_lower = line
                    .glyphs
                    .first()
                    .and_then(|&g| self.glyphs[g].text.chars().next())
                    .is_some_and(char::is_lowercase);
                if prev_hyphen && next_lower {
                    out.pop();
                } else if out.last().is_some_and(|(c, _)| *c != ' ') {
                    let style = out.last().map(|(_, s)| s.clone()).unwrap_or_default();
                    out.push((' ', style));
                }
            }
            let mut prev: Option<&Glyph> = None;
            for &gi in &line.glyphs {
                let g = &self.glyphs[gi];
                let style = self.style_of(g);
                // A gap between glyphs is a space the PDF didn't draw.
                if let Some(p) = prev
                    && g.origin[0] - p.end_x > 0.15 * g.size.max(p.size)
                    && !p.text.ends_with(' ')
                    && !g.text.starts_with(' ')
                {
                    out.push((' ', style.clone()));
                }
                for c in g.text.chars() {
                    out.push((c, style.clone()));
                }
                prev = Some(g);
            }
        }
        out
    }

    /// The style a glyph is drawn in.
    pub fn style_of(&self, g: &Glyph) -> TextStyle {
        let look = self.fonts.get(&g.font).cloned().unwrap_or_else(|| fonts::look_of("Helvetica", 0));
        TextStyle {
            family: look.family,
            bold: look.bold,
            italic: look.italic,
            underline: false,
            size: (g.size * 10.0).round() / 10.0,
            color: g.color,
        }
    }

    /// The block containing the point `(x, y)` (display points), if any.
    pub fn block_at(&self, x: f32, y: f32) -> Option<usize> {
        self.blocks.iter().position(|b| {
            let [l, t, r, bt] = b.rect;
            x >= l - 2.0 && x <= r + 2.0 && y >= t - 2.0 && y <= bt + 2.0
        })
    }
}

/// Groups glyphs into lines, and lines into blocks.
fn group(glyphs: &[Glyph]) -> Vec<TextBlock> {
    // Lines, in content order.
    let mut lines: Vec<TextLine> = Vec::new();
    let mut current: Vec<usize> = Vec::new();
    let flush = |current: &mut Vec<usize>, lines: &mut Vec<TextLine>| {
        if current.is_empty() {
            return;
        }
        let ids = std::mem::take(current);
        // The baseline of the line's main text (largest glyphs).
        let main = ids.iter().max_by(|&&a, &&b| glyphs[a].size.total_cmp(&glyphs[b].size)).copied().unwrap_or(ids[0]);
        let left = ids.iter().map(|&i| glyphs[i].rect[0]).fold(f32::MAX, f32::min);
        let right = ids.iter().map(|&i| glyphs[i].rect[2]).fold(f32::MIN, f32::max);
        lines.push(TextLine { baseline: glyphs[main].origin[1], left, right, glyphs: ids, paragraph_start: false });
    };
    for (i, g) in glyphs.iter().enumerate() {
        if let Some(&p) = current.last() {
            let p = &glyphs[p];
            let size = g.size.max(p.size).max(1.0);
            let new_line = (g.origin[1] - p.origin[1]).abs() > 0.5 * size
                || g.origin[0] < p.origin[0] - 0.5 * size
                || g.origin[0] - p.end_x > 1.5 * size
                || g.plain != p.plain;
            if new_line {
                flush(&mut current, &mut lines);
            }
        }
        current.push(i);
    }
    flush(&mut current, &mut lines);

    // Blocks: a line joins the previous block when it sits right below it,
    // overlaps it horizontally, has a similar size, and keeps the spacing.
    let size_of = |l: &TextLine| l.glyphs.iter().map(|&i| glyphs[i].size).fold(0.0f32, f32::max).max(1.0);
    let mut blocks: Vec<Vec<TextLine>> = Vec::new();
    for line in lines {
        // Any recent block may continue here (other text, such as margin
        // line numbers, can be drawn in between).
        let continues = |b: &Vec<TextLine>| {
            let last = b.last().unwrap();
            let (s1, s2) = (size_of(last), size_of(&line));
            let gap = line.baseline - last.baseline;
            let left = b.iter().map(|l| l.left).fold(f32::MAX, f32::min);
            let right = b.iter().map(|l| l.right).fold(f32::MIN, f32::max);
            let spacing_ok = b.len() < 2 || {
                let prev_gap = last.baseline - b[b.len() - 2].baseline;
                (gap - prev_gap).abs() <= 0.25 * prev_gap.max(1.0)
            };
            let plain = |l: &TextLine| l.glyphs.iter().all(|&i| glyphs[i].plain);
            // A heading in another font starts its own block.
            let main_font = |l: &TextLine| {
                let mut counts: HashMap<&[u8], usize> = HashMap::new();
                for &i in &l.glyphs {
                    *counts.entry(glyphs[i].font.as_slice()).or_default() += 1;
                }
                counts.into_iter().max_by_key(|&(f, n)| (n, f.to_vec())).map(|(f, _)| f.to_vec())
            };
            let same_font = main_font(last) == main_font(&line);
            let usual_gap = if b.len() >= 2 { last.baseline - b[b.len() - 2].baseline } else { 1.25 * s1 };
            (same_font || ((s1 / s2 - 1.0).abs() < 0.1 && gap <= usual_gap * 1.1))
                && gap > 0.6 * s1
                && gap < 1.7 * s1.max(s2)
                && (s1 / s2 - 1.0).abs() < 0.25
                && line.right > left
                && line.left < right
                && spacing_ok
                && plain(last) == plain(&line)
        };
        let recent = blocks.len().saturating_sub(8);
        match (recent..blocks.len()).rev().find(|&i| continues(&blocks[i])) {
            Some(i) => blocks[i].push(line),
            None => blocks.push(vec![line]),
        }
    }

    blocks
        .into_iter()
        .map(|mut lines| {
            mark_paragraphs(&mut lines, &size_of);
            let ids: Vec<usize> = lines.iter().flat_map(|l| l.glyphs.iter().copied()).collect();
            let rect = ids.iter().fold([f32::MAX, f32::MAX, f32::MIN, f32::MIN], |r, &i| {
                let g = glyphs[i].rect;
                [r[0].min(g[0]), r[1].min(g[1]), r[2].max(g[2]), r[3].max(g[3])]
            });
            let editable = ids.iter().all(|&i| glyphs[i].plain);
            let size = lines.iter().map(size_of).fold(0.0f32, f32::max);
            let line_spacing = if lines.len() >= 2 {
                (lines.last().unwrap().baseline - lines[0].baseline) / (lines.len() - 1) as f32 / size
            } else {
                1.2
            };
            let align = guess_align(&lines, size);
            let left = lines.iter().map(|l| l.left).fold(f32::MAX, f32::min);
            let indents: Vec<f32> = lines.iter().filter(|l| l.paragraph_start).map(|l| l.left - left).collect();
            let indent = if lines.len() > 1 && !indents.is_empty() {
                indents.iter().sum::<f32>() / indents.len() as f32
            } else {
                0.0
            };
            TextBlock { glyphs: ids, lines, rect, editable, align, line_spacing, indent }
        })
        .collect()
}

/// Marks lines that start a paragraph inside a block: the first line, and
/// lines that are indented or follow a line that ends well short of the
/// block's right edge.
fn mark_paragraphs(lines: &mut [TextLine], size_of: &dyn Fn(&TextLine) -> f32) {
    if lines.is_empty() {
        return;
    }
    let left = lines.iter().map(|l| l.left).fold(f32::MAX, f32::min);
    let right = lines.iter().map(|l| l.right).fold(f32::MIN, f32::max);
    lines[0].paragraph_start = true;
    for i in 1..lines.len() {
        let size = size_of(&lines[i]);
        let indented = lines[i].left - left > 0.8 * size;
        let prev_short = right - lines[i - 1].right > 2.5 * size;
        // Only when the block has enough lines to tell.
        lines[i].paragraph_start = lines.len() > 2 && (indented || prev_short);
    }
}

fn guess_align(lines: &[TextLine], size: f32) -> Align {
    if lines.len() < 2 {
        return Align::Left;
    }
    // Punctuation may hang a little past the margin.
    let tol = (0.3 * size).max(1.0);
    let near = |a: f32, b: f32| (a - b).abs() < tol;
    let left = lines.iter().map(|l| l.left).fold(f32::MAX, f32::min);
    let right = lines.iter().map(|l| l.right).fold(f32::MIN, f32::max);
    // Lines that end a paragraph are short in any alignment but centered
    // and right-aligned text.
    let full: Vec<&TextLine> =
        lines.iter().enumerate().filter(|(i, _)| lines.get(i + 1).is_some_and(|n| !n.paragraph_start)).map(|(_, l)| l).collect();
    let share = |f: &dyn Fn(&TextLine) -> bool, set: &[&TextLine]| {
        !set.is_empty() && set.iter().filter(|l| f(l)).count() as f32 >= 0.8 * set.len() as f32
    };
    let all: Vec<&TextLine> = lines.iter().collect();
    let body: Vec<&TextLine> = lines.iter().filter(|l| !l.paragraph_start).collect();
    let lefts_aligned = share(&|l| near(l.left, left), if body.is_empty() { &all } else { &body });
    if full.len() >= 2 && lefts_aligned && share(&|l| near(l.right, right), &full) {
        Align::Justify
    } else if lefts_aligned {
        Align::Left
    } else if share(&|l| near((l.left + l.right) / 2.0, (left + right) / 2.0), &all) {
        Align::Center
    } else if share(&|l| near(l.right, right), &all) {
        Align::Right
    } else {
        Align::Left
    }
}

// ----------------------------------------------------------------------
// Applying edits
// ----------------------------------------------------------------------

/// Applies `edit` to the page, adding the result to `changes` (which must
/// hold the edits made so far, if any) and updating `doc`'s overlay so it
/// shows the edited page. Save with [`crate::write_document_with_changes`].
pub fn edit_text(doc: &Document, edit: &BlockEdit, changes: &mut Changes) -> Result<()> {
    doc.set_overlay(changes);
    let pages = doc.pages()?;
    let page = pages.get(edit.page).ok_or_else(|| Error::Object(format!("page {} does not exist", edit.page + 1)))?;
    if page.id.num == 0 {
        return Err(Error::Object("this page can't be edited (it isn't stored as an object of its own)".into()));
    }
    let text = page_text(doc, edit.page)?;
    let data = doc.page_content(page)?;
    let ops = content::parse_ops(&data);
    let remove: HashSet<usize> = edit.remove.iter().copied().filter(|&i| i < text.glyphs.len()).collect();

    let mut resources = doc.get_dict(&page.dict, b"Resources").unwrap_or_default();
    let mut font_res = doc.get_dict(&resources, b"Font").unwrap_or_default();
    let display = page.display_matrix(doc);

    // Fonts for each style used.
    let mut faces: Vec<fonts::Face> = Vec::new();
    let mut face_names: Vec<Vec<u8>> = Vec::new();
    // Per style: the main font, and one for characters it lacks.
    let mut face_of: HashMap<(String, bool, bool), (usize, Option<usize>)> = HashMap::new();
    for (_, style) in &edit.runs {
        let key = (style.family.to_lowercase(), style.bold, style.italic);
        if face_of.contains_key(&key) {
            continue;
        }
        let all_text: String = edit
            .runs
            .iter()
            .filter(|(_, s)| (s.family.to_lowercase(), s.bold, s.italic) == key)
            .map(|(t, _)| t.as_str())
            .collect();
        let (mut face, name) = choose_face(&text, style, &all_text, &font_res, &face_names);
        let missing: String = all_text.chars().filter(|c| !c.is_control() && face.encode(*c).is_none()).collect();
        faces.push(face);
        face_names.push(name);
        let main = faces.len() - 1;
        let fallback = if missing.is_empty() {
            None
        } else {
            let (f, n) = choose_new_face(&text, style, &missing, &font_res, &face_names);
            faces.push(f);
            face_names.push(n);
            Some(faces.len() - 1)
        };
        face_of.insert(key, (main, fallback));
    }

    // Lay out the text.
    let layout = Layout::new(&display);
    let mut ops_out = String::new();
    lay_out(edit, &mut faces, &face_of, &face_names, &layout, &mut ops_out);

    // New page content: the old content (minus removed glyphs) kept in its
    // own graphics state, then the new text.
    let mut new_content = b"q\n".to_vec();
    new_content.extend(content::remove_glyphs(&data, &ops, &text.glyphs, &remove, &HashMap::new()));
    new_content.extend_from_slice(b"Q\n");
    new_content.extend_from_slice(ops_out.as_bytes());

    let mut next = doc
        .max_object_number()
        .max(changes.added.iter().map(|(r, _)| r.num).max().unwrap_or(0))
        .max(changes.replace.keys().copied().max().unwrap_or(0))
        + 1;
    let mut added: Vec<(ObjRef, Object)> = Vec::new();
    let mut add = |obj: Object| -> Object {
        let r = ObjRef::new(next, 0);
        next += 1;
        added.push((r, obj));
        Object::Reference(r)
    };
    for (face, name) in faces.iter().zip(&face_names) {
        if let Some(font) = face.font_object(&mut add) {
            font_res.set(name.clone(), font);
        }
    }
    let mut enc = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
    enc.write_all(&new_content)?;
    let mut stream_dict = Dictionary::new();
    stream_dict.set(b"Filter".to_vec(), Object::Name(b"FlateDecode".to_vec()));
    let contents = add(Object::Stream(Stream { dict: stream_dict, raw: enc.finish()?.into() }));

    resources.set(b"Font".to_vec(), Object::Dictionary(font_res));
    let mut page_dict = page.dict.clone();
    page_dict.set(b"Resources".to_vec(), Object::Dictionary(resources));
    page_dict.set(b"Contents".to_vec(), contents);
    changes.replace.insert(page.id.num, (page.id, Object::Dictionary(page_dict)));
    changes.added.extend(added);
    doc.set_overlay(changes);
    Ok(())
}

/// Crops page `page`: only `rect` (`[left, top, right, bottom]` in display
/// points of the page as currently shown) stays visible. `None` shows the
/// whole page again (its media box). Sets the page's `/CropBox`, limited to
/// the media box, and records it in `changes` like [`edit_text`].
pub fn crop_page(doc: &Document, page: usize, rect: Option<[f32; 4]>, changes: &mut Changes) -> Result<()> {
    doc.set_overlay(changes);
    let pages = doc.pages()?;
    let p = pages.get(page).ok_or_else(|| Error::Object(format!("page {} does not exist", page + 1)))?;
    if p.id.num == 0 {
        return Err(Error::Object("this page can't be cropped (it isn't stored as an object of its own)".into()));
    }
    let media = p.media_box(doc);
    let crop = match rect {
        None => media,
        Some([l, t, r, b]) => {
            let inv = Layout::new(&p.display_matrix(doc)).inverse;
            let corners = [(l, t), (r, t), (l, b), (r, b)]
                .map(|(x, y)| (x as f64 * inv[0] + y as f64 * inv[2] + inv[4], x as f64 * inv[1] + y as f64 * inv[3] + inv[5]));
            let (x0, x1) = corners.iter().fold((f64::MAX, f64::MIN), |(a, b), c| (a.min(c.0), b.max(c.0)));
            let (y0, y1) = corners.iter().fold((f64::MAX, f64::MIN), |(a, b), c| (a.min(c.1), b.max(c.1)));
            let r = [x0.max(media[0]), y0.max(media[1]), x1.min(media[2]), y1.min(media[3])];
            if r[2] - r[0] < 1.0 || r[3] - r[1] < 1.0 {
                return Err(Error::Object("the crop area is outside the page".into()));
            }
            r
        }
    };
    let mut dict = p.dict.clone();
    // Set explicitly, so a crop box inherited from the page tree can't
    // come back.
    let round = |v: f64| Object::Real((v * 100.0).round() / 100.0);
    dict.set(b"CropBox".to_vec(), Object::Array(crop.iter().map(|&v| round(v)).collect()));
    changes.replace.insert(p.id.num, (p.id, Object::Dictionary(dict)));
    doc.set_overlay(changes);
    Ok(())
}

/// Turns page `page` by `degrees` (a multiple of 90; positive is
/// clockwise, as the page is shown) by setting its `/Rotate`, and records
/// it in `changes` like [`edit_text`]. The page's content isn't touched.
pub fn rotate_page(doc: &Document, page: usize, degrees: i32, changes: &mut Changes) -> Result<()> {
    if degrees % 90 != 0 {
        return Err(Error::Object(format!("pages turn in steps of 90°, not {degrees}°")));
    }
    doc.set_overlay(changes);
    let pages = doc.pages()?;
    let p = pages.get(page).ok_or_else(|| Error::Object(format!("page {} does not exist", page + 1)))?;
    if p.id.num == 0 {
        return Err(Error::Object("this page can't be rotated (it isn't stored as an object of its own)".into()));
    }
    let rotation = (p.rotation() + degrees as i64).rem_euclid(360);
    let mut dict = p.dict.clone();
    // Set explicitly (also 0), so a rotation inherited from the page tree
    // can't come back.
    dict.set(b"Rotate".to_vec(), Object::Integer(rotation));
    changes.replace.insert(p.id.num, (p.id, Object::Dictionary(dict)));
    doc.set_overlay(changes);
    Ok(())
}

/// Picks the font for a style: the page's own font if it matches and has
/// all of `text`, else a system font, else a standard one. Returns the face
/// and its resource name on the page.
fn choose_face(
    page: &PageText,
    style: &TextStyle,
    text: &str,
    font_res: &Dictionary,
    taken: &[Vec<u8>],
) -> (fonts::Face, Vec<u8>) {
    let norm = |s: &str| s.chars().filter(|c| !c.is_whitespace()).collect::<String>().to_lowercase();
    // The page's own font in this family and style that has the most of
    // the characters (the rest come from a fallback font).
    let best = page
        .fonts
        .iter()
        .filter(|(_, l)| norm(&l.family) == norm(&style.family) && l.bold == style.bold && l.italic == style.italic)
        .map(|(name, _)| {
            let glyphs = page.font_glyphs.get(name).cloned().unwrap_or_default();
            let have = text.chars().filter(|c| c.is_control() || glyphs.contains_key(c)).count();
            (have, name.clone(), glyphs)
        })
        .max_by(|a, b| a.0.cmp(&b.0).then_with(|| b.1.cmp(&a.1)));
    if let Some((have, name, glyphs)) = best
        && have > 0
    {
        return (fonts::Face::Page { glyphs }, name);
    }
    choose_new_face(page, style, text, font_res, taken)
}

/// A font not on the page yet: a system font, else a standard one.
#[cfg_attr(not(feature = "render"), allow(unused_variables))]
fn choose_new_face(
    page: &PageText,
    style: &TextStyle,
    text: &str,
    font_res: &Dictionary,
    taken: &[Vec<u8>],
) -> (fonts::Face, Vec<u8>) {
    let norm = |s: &str| s.chars().filter(|c| !c.is_whitespace()).collect::<String>().to_lowercase();
    let look = page
        .fonts
        .values()
        .find(|l| norm(&l.family) == norm(&style.family))
        .cloned()
        .map(|l| FontLook { bold: style.bold, italic: style.italic, ..l })
        .unwrap_or_else(|| {
            let mut l = fonts::look_of(&style.family, 0);
            l.family = style.family.clone();
            l.bold = style.bold;
            l.italic = style.italic;
            l
        });
    let fresh_name = |prefix: &str| -> Vec<u8> {
        (1..)
            .map(|i| format!("{prefix}{i}").into_bytes())
            .find(|n| !font_res.contains(n) && !taken.contains(n))
            .unwrap()
    };
    #[cfg(feature = "render")]
    if let Some(sys) = fonts::SystemFace::find(&look) {
        let mut face = fonts::Face::System(Box::new(sys));
        if face.covers(text) {
            return (face, fresh_name("EF"));
        }
    }
    let name = fonts::standard_font(look.serif, look.monospace, style.bold, style.italic);
    (fonts::Face::Standard { name }, fresh_name("SF"))
}

/// Maps display points to page space for new text.
struct Layout {
    /// Display → page.
    inverse: [f64; 6],
}

impl Layout {
    fn new(display: &[f64; 6]) -> Self {
        let [a, b, c, d, e, f] = *display;
        let det = a * d - b * c;
        let det = if det.abs() < 1e-12 { 1.0 } else { det };
        let inverse = [d / det, -b / det, -c / det, a / det, (c * f - d * e) / det, (b * e - a * f) / det];
        Layout { inverse }
    }

    /// A text matrix placing text space's origin at display point (x, y),
    /// upright as displayed.
    fn text_matrix(&self, x: f32, y: f32) -> [f64; 6] {
        let m = &self.inverse;
        let (x, y) = (x as f64, y as f64);
        // Display right (1, 0) and up (0, -1), as page-space vectors.
        let right = (m[0], m[1]);
        let up = (-m[2], -m[3]);
        [right.0, right.1, up.0, up.1, x * m[0] + y * m[2] + m[4], x * m[1] + y * m[3] + m[5]]
    }
}

fn num(v: f64) -> String {
    let s = format!("{v:.3}");
    let s = s.trim_end_matches('0').trim_end_matches('.');
    if s == "-0" || s.is_empty() { "0".into() } else { s.into() }
}

/// Upper bound on fonts per edit (for packing style and font indices).
const FACES: usize = 1 << 16;

/// Word-wraps the runs and writes the content operators that draw them.
fn lay_out(
    edit: &BlockEdit,
    faces: &mut [fonts::Face],
    face_of: &HashMap<(String, bool, bool), (usize, Option<usize>)>,
    names: &[Vec<u8>],
    layout: &Layout,
    out: &mut String,
) {
    // One entry per character: (char, style index, face index). Characters
    // the style's main font lacks use its fallback font.
    let styles: Vec<&TextStyle> = edit.runs.iter().map(|(_, s)| s).collect();
    let mut chars: Vec<(char, usize)> = Vec::new();
    for (si, (t, s)) in edit.runs.iter().enumerate() {
        let (main, fallback) = face_of[&(s.family.to_lowercase(), s.bold, s.italic)];
        for c in t.chars() {
            let fi = match fallback {
                Some(f) if !c.is_control() && faces[main].encode(c).is_none() => f,
                _ => main,
            };
            // Face index encoded alongside the style: si * FACES + fi.
            chars.push((c, si * FACES + fi));
        }
    }
    let style_of = |key: usize| key / FACES;
    let face_index = |key: usize| key % FACES;
    let width_of = |faces: &[fonts::Face], c: char, key: usize| faces[face_index(key)].width(c) * styles[style_of(key)].size;

    let mut baseline = edit.first_baseline;
    let mut first_line = true;
    for paragraph in chars.split(|(c, _)| *c == '\n') {
        // Break the paragraph into lines at spaces.
        let mut lines: Vec<&[(char, usize)]> = Vec::new();
        let mut start = 0;
        while start < paragraph.len() || (lines.is_empty() && paragraph.is_empty()) {
            if paragraph.is_empty() {
                lines.push(paragraph);
                break;
            }
            let mut width = 0.0;
            let mut end = start;
            let mut last_space = None;
            while end < paragraph.len() {
                let (c, si) = paragraph[end];
                let w = width_of(faces, c, si);
                let limit = match (edit.wrap, lines.is_empty()) {
                    (false, _) => f32::INFINITY,
                    (true, true) => edit.width - edit.indent,
                    (true, false) => edit.width,
                };
                if width + w > limit && end > start && c != ' ' {
                    break;
                }
                if c == ' ' {
                    last_space = Some(end);
                }
                width += w;
                end += 1;
            }
            if end < paragraph.len()
                && let Some(sp) = last_space
                && sp > start
            {
                end = sp + 1; // break after the space
            }
            lines.push(&paragraph[start..end]);
            start = end;
        }

        for (li, line) in lines.iter().enumerate() {
            let indent = if li == 0 { edit.indent } else { 0.0 };
            let size = line.iter().map(|&(_, k)| styles[style_of(k)].size).fold(0.0f32, f32::max);
            let size = if size > 0.0 { size } else { styles.first().map_or(12.0, |s| s.size) };
            if !first_line {
                baseline += size * edit.line_spacing;
            }
            first_line = false;
            // Trailing spaces don't count for alignment.
            let trimmed = line.iter().rposition(|(c, _)| *c != ' ').map_or(&line[..0], |i| &line[..=i]);
            let width: f32 = trimmed.iter().map(|&(c, si)| width_of(faces, c, si)).sum();
            let spaces = trimmed.iter().filter(|(c, _)| *c == ' ').count();
            let last = li + 1 == lines.len();
            let (left, avail) = (edit.left + indent, edit.width - indent);
            let (mut x, extra_per_space) = match edit.align {
                Align::Left => (left, 0.0),
                Align::Center => (left + (avail - width) / 2.0, 0.0),
                Align::Right => (left + avail - width, 0.0),
                Align::Justify if !last && spaces > 0 => (left, ((avail - width) / spaces as f32).max(0.0)),
                Align::Justify => (left, 0.0),
            };
            // Runs of the same style, split at spaces when justifying.
            let mut i = 0;
            while i < trimmed.len() {
                let si = trimmed[i].1;
                let mut j = i;
                while j < trimmed.len() && trimmed[j].1 == si && !(extra_per_space > 0.0 && j > i && trimmed[j - 1].0 == ' ') {
                    j += 1;
                }
                let piece = &trimmed[i..j];
                let style = styles[style_of(si)];
                let fi = face_index(si);
                let mut bytes = Vec::new();
                for &(c, _) in piece {
                    match faces[fi].encode(c) {
                        Some(b) => bytes.extend(b),
                        None => bytes.extend(faces[fi].encode('?').unwrap_or_default()),
                    }
                }
                let w: f32 = piece.iter().map(|&(c, s)| width_of(faces, c, s)).sum();
                let tm = layout.text_matrix(x, baseline);
                let [r, g, b] = style.color;
                let name = String::from_utf8_lossy(&names[fi]).into_owned();
                let tm_s: Vec<String> = tm.iter().map(|v| num(*v)).collect();
                out.push_str(&format!(
                    "BT /{name} {} Tf {} {} {} rg 0 Tr {} Tm {} Tj ET\n",
                    num(style.size as f64),
                    num(r as f64),
                    num(g as f64),
                    num(b as f64),
                    tm_s.join(" "),
                    Object::String(bytes)
                ));
                if style.underline {
                    let thickness = (style.size * 0.06).max(0.5);
                    out.push_str(&format!(
                        "q {} {} {} rg {} cm 0 {} {} {} re f Q\n",
                        num(r as f64),
                        num(g as f64),
                        num(b as f64),
                        tm_s.join(" "),
                        num(-(style.size * 0.13) as f64),
                        num(w as f64),
                        num(thickness as f64)
                    ));
                }
                x += w;
                if extra_per_space > 0.0 && piece.last().is_some_and(|(c, _)| *c == ' ') {
                    x += extra_per_space;
                }
                i = j;
            }
        }
    }
}
