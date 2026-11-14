//! Redaction: permanently removing what is shown in areas of a page.
//!
//! Unlike drawing a black box (which leaves the text underneath in the
//! file, where it can be copied or extracted), [`redact_page`] takes the
//! content itself out of the page:
//!
//! - text: every character in an area is removed from the page content
//!   (the rest of the line keeps its place);
//! - images: the pixels in an area are blacked out in a new copy of the
//!   image; images that can't be decoded, and inline images, are removed
//!   whole;
//! - vector graphics: shapes lying entirely inside an area are removed
//!   (shapes crossing its edge stay, covered by the box);
//! - form XObjects (reused content, e.g. headers or stamps) drawn over an
//!   area are redacted the same way, in a copy used only by this page;
//! - annotations over an area (links, comments, form fields) are removed;
//!   a form field's value is cleared too.
//!
//! A black box is then drawn over each area. The page's thumbnail image
//! (`/Thumb`), which could show the old content, is dropped.
//!
//! Saving with [`crate::write_document_with_changes`] writes only objects
//! still in use, so the original content doesn't stay in the file.

use std::collections::{HashMap, HashSet};
use std::io::Write as _;

use super::content::{self, mul, Matrix, IDENTITY};
use super::{num, Layout};
use crate::document::Document;
use crate::error::{Error, Result};
use crate::object::{Dictionary, ObjRef, Object, Stream};
use crate::writer::Changes;

/// How deeply nested form XObjects are followed.
const MAX_DEPTH: usize = 8;

/// What a redaction removed.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Redacted {
    /// Characters of text.
    pub chars: usize,
    /// Images with pixels blacked out.
    pub images: usize,
    /// Images removed whole (not decodable, or inline).
    pub images_removed: usize,
    /// Vector shapes.
    pub shapes: usize,
    /// Annotations (links, comments, form fields).
    pub annotations: usize,
}

impl std::ops::AddAssign for Redacted {
    fn add_assign(&mut self, o: Redacted) {
        self.chars += o.chars;
        self.images += o.images;
        self.images_removed += o.images_removed;
        self.shapes += o.shapes;
        self.annotations += o.annotations;
    }
}

/// Redacts `areas` (`[left, top, right, bottom]` in display points of the
/// page as currently shown) of page `page`, recording the result in
/// `changes` like [`super::edit_text`].
pub fn redact_page(doc: &Document, page: usize, areas: &[[f32; 4]], changes: &mut Changes) -> Result<Redacted> {
    doc.set_overlay(changes);
    let pages = doc.pages()?;
    let p = pages.get(page).ok_or_else(|| Error::Object(format!("page {} does not exist", page + 1)))?;
    if p.id.num == 0 {
        return Err(Error::Object("this page can't be redacted (it isn't stored as an object of its own)".into()));
    }
    let areas: Vec<[f32; 4]> = areas
        .iter()
        .map(|a| [a[0].min(a[2]), a[1].min(a[3]), a[0].max(a[2]), a[1].max(a[3])])
        .filter(|a| a[2] - a[0] > 0.01 && a[3] - a[1] > 0.01)
        .collect();
    if areas.is_empty() {
        return Err(Error::Object("nothing to redact".into()));
    }

    let display = p.display_matrix(doc);
    let next = doc
        .max_object_number()
        .max(changes.added.iter().map(|(r, _)| r.num).max().unwrap_or(0))
        .max(changes.replace.keys().copied().max().unwrap_or(0))
        + 1;
    let mut r = Redactor { doc, areas: &areas, next, added: Vec::new(), replaced: Vec::new(), done: Redacted::default(), names: 0 };

    let resources = doc.get_dict(&p.dict, b"Resources").unwrap_or_default();
    let data = doc.page_content(p)?;
    let (body, resources) = r.content(&data, resources, &display, 0);

    // The old content in its own graphics state, then the boxes.
    let mut new_content = b"q\n".to_vec();
    new_content.extend(body);
    new_content.extend_from_slice(b"Q\nq 0 g\n");
    let inv = Layout::new(&display).inverse;
    for a in &areas {
        let pt = |x: f32, y: f32| {
            let (x, y) = (x as f64, y as f64);
            format!("{} {}", num(x * inv[0] + y * inv[2] + inv[4]), num(x * inv[1] + y * inv[3] + inv[5]))
        };
        new_content.extend_from_slice(
            format!("{} m {} l {} l {} l h f\n", pt(a[0], a[1]), pt(a[2], a[1]), pt(a[2], a[3]), pt(a[0], a[3])).as_bytes(),
        );
    }
    new_content.extend_from_slice(b"Q\n");
    let contents = r.add(Object::Stream(flate_stream(Dictionary::new(), &new_content)?));

    let mut dict = p.dict.clone();
    dict.set(b"Resources".to_vec(), Object::Dictionary(resources));
    dict.set(b"Contents".to_vec(), contents);
    dict.remove(b"Thumb");
    r.annotations(&mut dict, &display);

    changes.replace.insert(p.id.num, (p.id, Object::Dictionary(dict)));
    for (id, obj) in std::mem::take(&mut r.replaced) {
        changes.replace.insert(id.num, (id, obj));
    }
    changes.added.extend(std::mem::take(&mut r.added));
    doc.set_overlay(changes);
    Ok(r.done)
}

/// Finds `needle` in the text of page `page` (ignoring case, and treating
/// any run of white space as one space). Returns the areas covering each
/// match, in display points: one per line the match spans.
pub fn find_text(doc: &Document, page: usize, needle: &str) -> Result<Vec<[f32; 4]>> {
    let normalize = |s: &str| -> Vec<char> {
        let mut out: Vec<char> = Vec::new();
        for c in s.chars().flat_map(char::to_lowercase) {
            if c.is_whitespace() {
                if out.last().is_some_and(|l| *l != ' ') {
                    out.push(' ');
                }
            } else {
                out.push(c);
            }
        }
        out
    };
    let needle = normalize(needle.trim());
    if needle.is_empty() {
        return Ok(Vec::new());
    }
    let pages = doc.pages()?;
    let p = pages.get(page).ok_or_else(|| Error::Object(format!("page {} does not exist", page + 1)))?;
    let chars = crate::layout::reading_order(&crate::text::TextExtractor::new(doc).extract_chars(p)?);

    // The page text, normalized like the needle, with each character's
    // source (index into `chars`).
    let mut hay: Vec<(char, usize)> = Vec::new();
    for (i, ch) in chars.iter().enumerate() {
        for c in ch.text.chars().flat_map(char::to_lowercase) {
            if c.is_whitespace() || ch.rect.is_none() && c.is_control() {
                if hay.last().is_some_and(|l| l.0 != ' ') {
                    hay.push((' ', i));
                }
            } else {
                hay.push((c, i));
            }
        }
    }

    let mut out = Vec::new();
    let mut start = 0;
    while start + needle.len() <= hay.len() {
        if !hay[start..start + needle.len()].iter().zip(&needle).all(|(h, n)| h.0 == *n) {
            start += 1;
            continue;
        }
        let (first, last) = (hay[start].1, hay[start + needle.len() - 1].1);
        // One area per line: characters overlapping vertically join.
        let mut lines: Vec<[f32; 4]> = Vec::new();
        for r in chars[first..=last].iter().filter_map(|c| c.rect) {
            match lines.last_mut() {
                Some(l) if r[1].max(l[1]) < r[3].min(l[3]) - 0.5 * (r[3] - r[1]).min(l[3] - l[1]) => {
                    *l = [l[0].min(r[0]), l[1].min(r[1]), l[2].max(r[2]), l[3].max(r[3])];
                }
                _ => lines.push(r),
            }
        }
        out.extend(lines);
        start += needle.len();
    }
    Ok(out)
}

struct Redactor<'a> {
    doc: &'a Document,
    areas: &'a [[f32; 4]],
    next: u32,
    added: Vec<(ObjRef, Object)>,
    replaced: Vec<(ObjRef, Object)>,
    done: Redacted,
    /// Counter for new resource names.
    names: usize,
}

impl Redactor<'_> {
    fn add(&mut self, obj: Object) -> Object {
        let r = ObjRef::new(self.next, 0);
        self.next += 1;
        self.added.push((r, obj));
        Object::Reference(r)
    }

    /// A resource name not used in `dict`.
    fn new_name(&mut self, dict: &Dictionary) -> Vec<u8> {
        loop {
            self.names += 1;
            let name = format!("Rd{}", self.names).into_bytes();
            if !dict.contains(&name) {
                return name;
            }
        }
    }

    /// Bounds in display points of `rect` (in the space `m` maps to
    /// display points).
    fn bounds(m: &Matrix, rect: [f64; 4]) -> [f32; 4] {
        let pts = [(rect[0], rect[1]), (rect[2], rect[1]), (rect[0], rect[3]), (rect[2], rect[3])]
            .map(|(x, y)| (x * m[0] + y * m[2] + m[4], x * m[1] + y * m[3] + m[5]));
        let (l, r) = pts.iter().fold((f64::MAX, f64::MIN), |(a, b), p| (a.min(p.0), b.max(p.0)));
        let (t, b) = pts.iter().fold((f64::MAX, f64::MIN), |(a, b), p| (a.min(p.1), b.max(p.1)));
        [l as f32, t as f32, r as f32, b as f32]
    }

    /// Whether `r` overlaps an area at all.
    fn touches(&self, r: [f32; 4]) -> bool {
        self.areas.iter().any(|a| overlap(a, &r).is_some())
    }

    /// The redacted version of content `data` drawn with `resources`, whose
    /// user space `base` maps to display points; returns the new content
    /// and resources.
    fn content(&mut self, data: &[u8], mut resources: Dictionary, base: &Matrix, depth: usize) -> (Vec<u8>, Dictionary) {
        let doc = self.doc;
        let ops = content::parse_ops(data);

        // Text.
        let glyphs = content::glyphs(doc, base, &ops, &resources);
        let remove: HashSet<usize> =
            (0..glyphs.len()).filter(|&i| self.areas.iter().any(|a| covers(a, &glyphs[i].rect))).collect();
        self.done.chars += remove.iter().filter(|&&i| !glyphs[i].text.trim().is_empty()).count();

        // Images, forms and shapes.
        let mut replace: HashMap<usize, Vec<u8>> = HashMap::new();
        let mut xobjects = doc.get_dict(&resources, b"XObject").unwrap_or_default();
        let mut xobjects_changed = false;
        // Every `Do`: operation index and XObject name.
        let mut draws: Vec<(usize, Vec<u8>)> = Vec::new();
        let mut ctm = IDENTITY;
        let mut stack: Vec<Matrix> = Vec::new();
        // Bounds of the current path in display points.
        let mut path: Option<[f32; 4]> = None;
        for (index, op) in ops.iter().enumerate() {
            let o = &op.operands;
            let n = |i: usize| o.get(i).and_then(Object::as_f64).unwrap_or(0.0);
            let m = mul(&ctm, base);
            let mut extend = |pts: &[(f64, f64)]| {
                for &(x, y) in pts {
                    let (dx, dy) = ((x * m[0] + y * m[2] + m[4]) as f32, (x * m[1] + y * m[3] + m[5]) as f32);
                    let b = path.get_or_insert([dx, dy, dx, dy]);
                    *b = [b[0].min(dx), b[1].min(dy), b[2].max(dx), b[3].max(dy)];
                }
            };
            match op.operator.as_slice() {
                b"q" => stack.push(ctm),
                b"Q" => ctm = stack.pop().unwrap_or(ctm),
                b"cm" if o.len() >= 6 => ctm = mul(&[n(0), n(1), n(2), n(3), n(4), n(5)], &ctm),
                b"m" | b"l" => extend(&[(n(0), n(1))]),
                b"c" => extend(&[(n(0), n(1)), (n(2), n(3)), (n(4), n(5))]),
                b"v" | b"y" => extend(&[(n(0), n(1)), (n(2), n(3))]),
                b"re" => extend(&[(n(0), n(1)), (n(0) + n(2), n(1) + n(3))]),
                b"S" | b"s" | b"f" | b"F" | b"f*" | b"B" | b"B*" | b"b" | b"b*" => {
                    if let Some(b) = path.take()
                        && self.areas.iter().any(|a| inside(a, &b))
                    {
                        // End the path without painting it (a clip set
                        // with it still applies).
                        replace.insert(index, b"n".to_vec());
                        self.done.shapes += 1;
                    }
                }
                b"n" => path = None,
                b"BI" => {
                    if self.touches(Self::bounds(&m, [0.0, 0.0, 1.0, 1.0])) {
                        replace.insert(index, Vec::new());
                        self.done.images_removed += 1;
                    }
                }
                b"Do" => {
                    let Some(name) = o.first().and_then(Object::as_name) else { continue };
                    draws.push((index, name.to_vec()));
                    let Some(Object::Stream(stream)) = xobjects.get(name).and_then(|x| doc.resolve(x).ok()) else { continue };
                    let new = match stream.dict.get(b"Subtype").and_then(Object::as_name) {
                        Some(b"Image") => {
                            if !self.touches(Self::bounds(&m, [0.0, 0.0, 1.0, 1.0])) {
                                continue;
                            }
                            match self.image(&stream, &resources, &m) {
                                Some(Some(s)) => {
                                    self.done.images += 1;
                                    Some(s)
                                }
                                Some(None) => continue, // no pixel in an area
                                None => {
                                    self.done.images_removed += 1;
                                    None
                                }
                            }
                        }
                        Some(b"Form") => {
                            let matrix = match doc.get_resolved(&stream.dict, b"Matrix") {
                                Ok(Object::Array(a)) if a.len() == 6 => {
                                    let v: Vec<f64> = a.iter().map(|x| x.as_f64().unwrap_or(0.0)).collect();
                                    [v[0], v[1], v[2], v[3], v[4], v[5]]
                                }
                                _ => IDENTITY,
                            };
                            let form_base = mul(&matrix, &m);
                            let bbox = doc.rect(stream.dict.get(b"BBox")).unwrap_or([0.0; 4]);
                            if !self.touches(Self::bounds(&form_base, bbox)) {
                                continue;
                            }
                            if depth >= MAX_DEPTH {
                                None
                            } else {
                                let form_res = doc.get_dict(&stream.dict, b"Resources").unwrap_or_else(|| resources.clone());
                                let Ok(form_data) = doc.decode_stream(&stream) else { continue };
                                let (body, res) = self.content(&form_data, form_res, &form_base, depth + 1);
                                let mut dict = stream.dict.clone();
                                for key in [&b"Filter"[..], b"DecodeParms", b"Length"] {
                                    dict.remove(key);
                                }
                                dict.set(b"Resources".to_vec(), Object::Dictionary(res));
                                flate_stream(dict, &body).ok()
                            }
                        }
                        _ => continue,
                    };
                    match new {
                        Some(s) => {
                            let r = self.add(Object::Stream(s));
                            let name = self.new_name(&xobjects);
                            replace.insert(index, format!("{} Do", Object::Name(name.clone())).into_bytes());
                            xobjects.set(name, r);
                            xobjects_changed = true;
                        }
                        None => {
                            replace.insert(index, Vec::new());
                        }
                    }
                }
                _ => {}
            }
        }

        // An original no longer drawn here must not stay in the resources,
        // or it would be saved with the file.
        let (replaced, kept): (Vec<_>, Vec<_>) = draws.into_iter().partition(|(i, _)| replace.contains_key(i));
        let kept: HashSet<Vec<u8>> = kept.into_iter().map(|(_, n)| n).collect();
        for (_, name) in replaced.iter().filter(|(_, n)| !kept.contains(n)) {
            xobjects.remove(name);
            xobjects_changed = true;
        }
        if xobjects_changed {
            resources.set(b"XObject".to_vec(), Object::Dictionary(xobjects));
        }
        (content::remove_glyphs(data, &ops, &glyphs, &remove, &replace), resources)
    }

    /// A copy of image `stream` (drawn with `m`: unit square → display)
    /// with the pixels in the areas blacked out. `Some(None)` if no pixel is
    /// in an area; `None` if the image can't be decoded.
    #[cfg(feature = "render")]
    fn image(&mut self, stream: &Stream, resources: &Dictionary, m: &Matrix) -> Option<Option<Stream>> {
        use crate::render::image::{decode, Paint};
        let doc = self.doc;
        let dict = &stream.dict;
        let int = |k: &[u8]| doc.get_resolved(dict, k).ok().and_then(|o| o.as_i64());
        let (w, h) = (int(b"Width")?.max(1) as usize, int(b"Height")?.max(1) as usize);
        let stencil = doc.get_resolved(dict, b"ImageMask").ok().and_then(|o| o.as_bool()).unwrap_or(false);
        let paint = if stencil { Paint::Stencil([0.0, 0.0, 0.0, 1.0]) } else { Paint::Colors };
        let mut pixmap = decode(doc, stream, resources, paint, (w as f64, h as f64))?;
        let (w, h) = (pixmap.width() as usize, pixmap.height() as usize);

        // Each area in image pixels (the unit square, y up, scaled).
        let inv = Layout::new(m).inverse;
        let mut hit = false;
        // Stencils: not painted; images: opaque black.
        let blank = if stencil {
            tiny_skia::PremultipliedColorU8::TRANSPARENT
        } else {
            tiny_skia::PremultipliedColorU8::from_rgba(0, 0, 0, 255)?
        };
        let pixels = pixmap.pixels_mut();
        for a in self.areas {
            let [l, t, r, b] = Self::bounds(&inv, [a[0] as f64, a[1] as f64, a[2] as f64, a[3] as f64]);
            let x0 = ((l as f64 * w as f64).floor().max(0.0) as usize).min(w);
            let x1 = ((r as f64 * w as f64).ceil().max(0.0) as usize).min(w);
            let y0 = (((1.0 - b as f64) * h as f64).floor().max(0.0) as usize).min(h);
            let y1 = (((1.0 - t as f64) * h as f64).ceil().max(0.0) as usize).min(h);
            for y in y0..y1 {
                if x0 < x1 {
                    pixels[y * w + x0..y * w + x1].fill(blank);
                    hit = true;
                }
            }
        }
        if !hit {
            return Some(None);
        }

        let mut out = Dictionary::new();
        out.set(b"Type".to_vec(), Object::Name(b"XObject".to_vec()));
        out.set(b"Subtype".to_vec(), Object::Name(b"Image".to_vec()));
        out.set(b"Width".to_vec(), Object::Integer(w as i64));
        out.set(b"Height".to_vec(), Object::Integer(h as i64));
        if let Some(i) = dict.get(b"Interpolate") {
            out.set(b"Interpolate".to_vec(), i.clone());
        }
        let pixels = pixmap.pixels();
        if stencil {
            // One bit per pixel, 0 = painted.
            let row = w.div_ceil(8);
            let mut data = vec![0u8; row * h];
            for (i, p) in pixels.iter().enumerate() {
                if p.alpha() < 128 {
                    data[(i / w) * row + (i % w) / 8] |= 0x80 >> (i % w % 8);
                }
            }
            out.set(b"ImageMask".to_vec(), Object::Bool(true));
            out.set(b"BitsPerComponent".to_vec(), Object::Integer(1));
            return flate_stream(out, &data).ok().map(Some);
        }
        let colors: Vec<[u8; 3]> = pixels
            .iter()
            .map(|p| {
                let c = p.demultiply();
                [c.red(), c.green(), c.blue()]
            })
            .collect();
        let gray = colors.iter().all(|c| c[0] == c[1] && c[1] == c[2]);
        let data: Vec<u8> = if gray { colors.iter().map(|c| c[0]).collect() } else { colors.concat() };
        out.set(b"ColorSpace".to_vec(), Object::Name(if gray { b"DeviceGray".to_vec() } else { b"DeviceRGB".to_vec() }));
        out.set(b"BitsPerComponent".to_vec(), Object::Integer(8));
        if pixels.iter().any(|p| p.alpha() < 255) {
            let mut mask = Dictionary::new();
            mask.set(b"Type".to_vec(), Object::Name(b"XObject".to_vec()));
            mask.set(b"Subtype".to_vec(), Object::Name(b"Image".to_vec()));
            mask.set(b"Width".to_vec(), Object::Integer(w as i64));
            mask.set(b"Height".to_vec(), Object::Integer(h as i64));
            mask.set(b"ColorSpace".to_vec(), Object::Name(b"DeviceGray".to_vec()));
            mask.set(b"BitsPerComponent".to_vec(), Object::Integer(8));
            let alpha: Vec<u8> = pixels.iter().map(|p| p.alpha()).collect();
            let smask = self.add(Object::Stream(flate_stream(mask, &alpha).ok()?));
            out.set(b"SMask".to_vec(), smask);
        }
        flate_stream(out, &data).ok().map(Some)
    }

    /// Without the renderer images can't be decoded: they are removed.
    #[cfg(not(feature = "render"))]
    fn image(&mut self, _: &Stream, _: &Dictionary, _: &Matrix) -> Option<Option<Stream>> {
        None
    }

    /// Removes the annotations over an area from the page's `/Annots`.
    /// Their objects may still be used elsewhere (e.g. by the document's
    /// structure tree), so they are emptied as well: comments lose their
    /// text, links their targets, and form fields shown only there their
    /// values.
    fn annotations(&mut self, page: &mut Dictionary, display: &Matrix) {
        let doc = self.doc;
        let Ok(Object::Array(annots)) = doc.get_resolved(page, b"Annots") else { return };
        let mut removed: HashSet<ObjRef> = HashSet::new();
        let mut keep: Vec<Object> = Vec::new();
        for a in &annots {
            let Some(dict) = doc.resolve(a).ok().and_then(|o| o.as_dict().cloned()) else { continue };
            let over = doc.rect(dict.get(b"Rect")).is_some_and(|r| self.touches(Self::bounds(display, r)));
            if !over {
                keep.push(a.clone());
                continue;
            }
            self.done.annotations += 1;
            removed.extend(a.as_reference());
            if let Some(popup) = dict.get(b"Popup").and_then(Object::as_reference) {
                removed.insert(popup);
            }
        }
        if removed.is_empty() && keep.len() == annots.len() {
            return;
        }
        // Popups of removed annotations go too.
        let parent_of = |r: ObjRef| {
            doc.get_object(r).ok().and_then(|o| o.as_dict().and_then(|d| d.get(b"Parent").and_then(Object::as_reference)))
        };
        keep.retain(|a| match a.as_reference() {
            Some(r) => !removed.contains(&r) && parent_of(r).is_none_or(|p| !removed.contains(&p)),
            None => true,
        });
        page.set(b"Annots".to_vec(), Object::Array(keep));

        let mut emptied: HashMap<ObjRef, Dictionary> = HashMap::new();
        let mut fields: Vec<ObjRef> = Vec::new();
        for &r in &removed {
            let Ok(Object::Dictionary(dict)) = doc.get_object(r) else { continue };
            let widget = dict.get(b"Subtype").and_then(Object::as_name) == Some(b"Widget");
            let new = if widget {
                // Still part of the form: keep the field, without its look.
                let mut d = dict.clone();
                d.remove(b"AP");
                if d.contains(b"T") {
                    fields.push(r);
                } else {
                    fields.extend(d.get(b"Parent").and_then(Object::as_reference));
                }
                d
            } else {
                let mut d = Dictionary::new();
                for key in [&b"Type"[..], b"Subtype", b"Rect", b"P"] {
                    if let Some(v) = dict.get(key) {
                        d.set(key.to_vec(), v.clone());
                    }
                }
                d.set(b"F".to_vec(), Object::Integer(2)); // hidden
                d
            };
            emptied.insert(r, new);
        }
        // A field whose widgets were all removed loses its value.
        for field in fields {
            let Some(mut dict) = emptied.get(&field).cloned().or_else(|| doc.get_object(field).ok()?.as_dict().cloned())
            else {
                continue;
            };
            let kids: Vec<ObjRef> = match doc.get_resolved(&dict, b"Kids") {
                Ok(Object::Array(k)) => k.iter().filter_map(Object::as_reference).collect(),
                _ => Vec::new(),
            };
            if kids.iter().any(|k| !removed.contains(k)) {
                continue;
            }
            dict.remove(b"V");
            dict.remove(b"DV");
            emptied.insert(field, dict);
        }
        self.replaced.extend(emptied.into_iter().map(|(r, d)| (r, Object::Dictionary(d))));
    }
}

/// Overlap of two rectangles (`[left, top, right, bottom]`), if any.
fn overlap(a: &[f32; 4], b: &[f32; 4]) -> Option<f32> {
    let w = a[2].min(b[2]) - a[0].max(b[0]);
    let h = a[3].min(b[3]) - a[1].max(b[1]);
    (w > 0.0 && h > 0.0).then_some(w * h)
}

/// Whether a glyph with bounds `g` is (mostly) in area `a`: its center is
/// inside, or at least a third of it is.
fn covers(a: &[f32; 4], g: &[f32; 4]) -> bool {
    let (cx, cy) = ((g[0] + g[2]) / 2.0, (g[1] + g[3]) / 2.0);
    if cx >= a[0] && cx <= a[2] && cy >= a[1] && cy <= a[3] {
        return true;
    }
    let area = (g[2] - g[0]) * (g[3] - g[1]);
    overlap(a, g).is_some_and(|o| area > 0.0 && o >= area / 3.0)
}

/// Whether `b` lies inside area `a` (with half a point to spare).
fn inside(a: &[f32; 4], b: &[f32; 4]) -> bool {
    b[0] >= a[0] - 0.5 && b[1] >= a[1] - 0.5 && b[2] <= a[2] + 0.5 && b[3] <= a[3] + 0.5
}

/// A Flate-compressed stream with `dict` (plus the filter) and `data`.
fn flate_stream(mut dict: Dictionary, data: &[u8]) -> Result<Stream> {
    let mut enc = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
    enc.write_all(data)?;
    dict.set(b"Filter".to_vec(), Object::Name(b"FlateDecode".to_vec()));
    Ok(Stream { dict, raw: enc.finish()?.into() })
}
