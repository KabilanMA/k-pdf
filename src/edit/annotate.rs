//! Comments: highlights and notes, stored as standard PDF markup
//! annotations (ISO 32000 §12.5.6) so other readers show them too.
//!
//! A highlight is a `/Highlight` annotation over some text, with an
//! optional comment in its `/Contents`; a note is a `/Text` annotation (a
//! small speech bubble) at a spot on the page, with its comment. Both get
//! an appearance stream, so they look the same everywhere. Comments made
//! by other programs (underlines, strike-outs, shapes, free text…) are
//! listed and can be edited and deleted too.

use std::time::{SystemTime, UNIX_EPOCH};

use crate::document::{Document, Page};
use crate::encoding::decode_text_string;
use crate::error::{Error, Result};
use crate::object::{Dictionary, ObjRef, Object, Stream};
use crate::writer::Changes;

/// A comment on a page.
#[derive(Debug, Clone, PartialEq)]
pub struct Comment {
    /// The annotation object.
    pub id: ObjRef,
    /// Page index.
    pub page: usize,
    pub kind: CommentKind,
    /// `[left, top, right, bottom]` in display points of the page.
    pub rect: [f32; 4],
    /// The marked areas (a highlight's lines), in display points; just
    /// `rect` for other kinds.
    pub areas: Vec<[f32; 4]>,
    /// The comment's text (may be empty: a highlight without a comment).
    pub text: String,
    pub author: String,
    /// When it was last changed, as a PDF date (`D:20260131…`).
    pub modified: Option<String>,
    pub color: [u8; 3],
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommentKind {
    Highlight,
    /// A sticky note.
    Note,
    /// Another kind of markup, by its PDF name (`Underline`, `FreeText`…).
    Other(String),
}

/// Annotation kinds that are comments (markup annotations).
const MARKUP: &[&[u8]] = &[
    b"Text",
    b"Highlight",
    b"Underline",
    b"StrikeOut",
    b"Squiggly",
    b"FreeText",
    b"Square",
    b"Circle",
    b"Line",
    b"Polygon",
    b"PolyLine",
    b"Ink",
    b"Stamp",
    b"Caret",
    b"FileAttachment",
    b"Sound",
];

/// The default highlight color (yellow).
pub const HIGHLIGHT_YELLOW: [u8; 3] = [255, 226, 77];
/// Size of a note's icon, in points.
const NOTE_SIZE: f64 = 20.0;

/// The comments on every page, page by page.
pub fn comments(doc: &Document) -> Result<Vec<Comment>> {
    let pages = doc.pages()?;
    Ok((0..pages.len()).flat_map(|i| page_comments_in(doc, &pages, i)).collect())
}

/// The comments on page `page`, in the file's order.
pub fn page_comments(doc: &Document, page: usize) -> Result<Vec<Comment>> {
    let pages = doc.pages()?;
    Ok(page_comments_in(doc, &pages, page))
}

fn page_comments_in(doc: &Document, pages: &[Page], index: usize) -> Vec<Comment> {
    let Some(page) = pages.get(index) else { return Vec::new() };
    let Ok(Object::Array(annots)) = doc.get_resolved(&page.dict, b"Annots") else { return Vec::new() };
    let m = page.display_matrix(doc);
    let mut out = Vec::new();
    for annot in &annots {
        // Only annotations stored as objects of their own can be edited.
        let Object::Reference(id) = annot else { continue };
        let Some(dict) = doc.get_object(*id).ok().and_then(|o| o.as_dict().cloned()) else { continue };
        let Some(subtype) = dict.get(b"Subtype").and_then(Object::as_name) else { continue };
        if !MARKUP.contains(&subtype) {
            continue;
        }
        let flags = doc.get_resolved(&dict, b"F").ok().and_then(|o| o.as_i64()).unwrap_or(0);
        if flags & 2 != 0 {
            continue; // Hidden
        }
        // Replies to another comment ("In Reply To") belong to it.
        if dict.get(b"IRT").is_some() {
            continue;
        }
        let Some(r) = doc.rect(dict.get(b"Rect")) else { continue };
        let rect = display_rect(&m, &[(r[0], r[1]), (r[2], r[1]), (r[0], r[3]), (r[2], r[3])]);
        let kind = match subtype {
            b"Highlight" => CommentKind::Highlight,
            b"Text" => CommentKind::Note,
            other => CommentKind::Other(String::from_utf8_lossy(other).into_owned()),
        };
        let mut areas = Vec::new();
        if let Ok(Object::Array(q)) = doc.get_resolved(&dict, b"QuadPoints") {
            let v: Vec<f64> = q.iter().filter_map(|o| doc.resolve(o).ok().and_then(|o| o.as_f64())).collect();
            for quad in v.chunks_exact(8) {
                areas.push(display_rect(&m, &[(quad[0], quad[1]), (quad[2], quad[3]), (quad[4], quad[5]), (quad[6], quad[7])]));
            }
        }
        if areas.is_empty() {
            areas.push(rect);
        }
        let text_of = |key: &[u8]| {
            doc.get_resolved(&dict, key).ok().and_then(|o| o.as_string().map(decode_text_string)).unwrap_or_default()
        };
        let default = if kind == CommentKind::Highlight || kind == CommentKind::Note { HIGHLIGHT_YELLOW } else { [255, 0, 0] };
        out.push(Comment {
            id: *id,
            page: index,
            kind,
            rect,
            areas,
            text: text_of(b"Contents").replace("\r\n", "\n").replace('\r', "\n"),
            author: text_of(b"T"),
            modified: Some(text_of(b"M")).filter(|s| !s.is_empty()),
            color: color_of(doc, &dict).unwrap_or(default),
        });
    }
    out
}

/// Highlights `areas` (`[left, top, right, bottom]` in display points, one
/// per line of text) on page `page` in `color`, with comment `text` (may
/// be empty). Returns the new annotation.
pub fn add_highlight(
    doc: &Document,
    page: usize,
    areas: &[[f32; 4]],
    color: [u8; 3],
    text: &str,
    author: &str,
    changes: &mut Changes,
) -> Result<ObjRef> {
    if areas.is_empty() {
        return Err(Error::Object("nothing to highlight".into()));
    }
    doc.set_overlay(changes);
    let pages = doc.pages()?;
    let p = page_of(&pages, page)?;
    let to_user = to_user(&p.display_matrix(doc));
    // Corners in the order viewers expect: upper left, upper right, lower
    // left, lower right (as the text reads).
    let quads: Vec<[(f64, f64); 4]> = areas
        .iter()
        .map(|&[l, t, r, b]| [to_user(l, t), to_user(r, t), to_user(l, b), to_user(r, b)])
        .collect();
    let rect = bounds(quads.iter().flatten().copied());

    // Appearance: the areas filled in the color, multiplied with the page
    // so the text stays readable.
    let [cr, cg, cb] = color.map(|v| v as f64 / 255.0);
    let mut content = format!("/K0 gs {} {} {} rg\n", num(cr), num(cg), num(cb));
    for q in &quads {
        let [ul, ur, ll, lr] = q.map(|(x, y)| format!("{} {}", num(x), num(y)));
        content.push_str(&format!("{ul} m {ur} l {lr} l {ll} l h f\n"));
    }
    let mut gs = Dictionary::new();
    gs.set(b"Type".to_vec(), name(b"ExtGState"));
    gs.set(b"BM".to_vec(), name(b"Multiply"));
    let mut ext = Dictionary::new();
    ext.set(b"K0".to_vec(), Object::Dictionary(gs));
    let mut resources = Dictionary::new();
    resources.set(b"ExtGState".to_vec(), Object::Dictionary(ext));

    let mut dict = markup(b"Highlight", rect, color, text, author);
    let quad_points = quads.iter().flatten().flat_map(|&(x, y)| [real(x), real(y)]).collect();
    dict.set(b"QuadPoints".to_vec(), Object::Array(quad_points));
    add_annotation(doc, p, dict, appearance(rect, resources, content), changes)
}

/// Adds a note (a speech-bubble icon) with comment `text` on page `page`,
/// its top-left corner at `at` (display points). Returns the new
/// annotation.
pub fn add_note(
    doc: &Document,
    page: usize,
    at: [f32; 2],
    color: [u8; 3],
    text: &str,
    author: &str,
    changes: &mut Changes,
) -> Result<ObjRef> {
    doc.set_overlay(changes);
    let pages = doc.pages()?;
    let p = page_of(&pages, page)?;
    let to_user = to_user(&p.display_matrix(doc));
    let s = NOTE_SIZE as f32;
    let [x, y] = at;
    let rect = bounds([to_user(x, y), to_user(x + s, y + s)].into_iter());

    // A speech bubble with two lines of "text" in it, drawn in a 20 × 20
    // box (stretched over the rectangle).
    let [cr, cg, cb] = color.map(|v| v as f64 / 255.0);
    let content = format!(
        "{} {} {} rg 0.35 0.3 0.15 RG 1 w 1 j\n\
         2 7 m 2 18 l 18 18 l 18 7 l 10 7 l 5 2.5 l 6 7 l h B\n\
         0.35 0.3 0.15 rg 5 13.5 m 15 13.5 l 15 12.3 l 5 12.3 l h f 5 10.5 m 12 10.5 l 12 9.3 l 5 9.3 l h f\n",
        num(cr),
        num(cg),
        num(cb)
    );
    let ap = appearance([0.0, 0.0, NOTE_SIZE, NOTE_SIZE], Dictionary::new(), content);
    let mut dict = markup(b"Text", rect, color, text, author);
    dict.set(b"Name".to_vec(), name(b"Comment"));
    dict.set(b"Open".to_vec(), Object::Bool(false));
    add_annotation(doc, p, dict, ap, changes)
}

/// Changes the text of comment `id`.
pub fn set_comment_text(doc: &Document, id: ObjRef, text: &str, changes: &mut Changes) -> Result<()> {
    doc.set_overlay(changes);
    let mut dict = doc
        .get_object(id)?
        .as_dict()
        .cloned()
        .ok_or_else(|| Error::Object(format!("object {} is not an annotation", id.num)))?;
    dict.set(b"Contents".to_vec(), Object::String(crate::forms::encode_text_string(text)));
    dict.set(b"M".to_vec(), Object::String(pdf_date_now().into_bytes()));
    store(changes, id, Object::Dictionary(dict));
    doc.set_overlay(changes);
    Ok(())
}

/// Deletes comment `id` from page `page`, with its pop-up window and the
/// replies to it.
pub fn delete_comment(doc: &Document, page: usize, id: ObjRef, changes: &mut Changes) -> Result<()> {
    doc.set_overlay(changes);
    let pages = doc.pages()?;
    let p = page_of(&pages, page)?;
    let mut annots = page_annots(doc, p)?;
    let popup = doc.get_object(id).ok().and_then(|o| o.as_dict().and_then(|d| d.get(b"Popup").cloned()));
    let before = annots.len();
    annots.retain(|a| {
        if *a == Object::Reference(id) || popup.as_ref() == Some(a) {
            return false;
        }
        // Replies to it.
        let reply_to = doc.resolve(a).ok().and_then(|o| o.as_dict().and_then(|d| d.get(b"IRT").cloned()));
        reply_to != Some(Object::Reference(id))
    });
    if annots.len() == before {
        return Err(Error::Object("the comment isn't on that page".into()));
    }
    set_page_annots(doc, p, annots, changes)?;
    doc.set_overlay(changes);
    Ok(())
}

/// Removes every comment from page `page` (highlights, notes and other
/// markup, with their pop-up windows), keeping links and form fields; for
/// printing without them. Returns how many were removed.
pub fn remove_comments(doc: &Document, page: usize, changes: &mut Changes) -> Result<usize> {
    doc.set_overlay(changes);
    let pages = doc.pages()?;
    let p = page_of(&pages, page)?;
    let mut annots = page_annots(doc, p)?;
    let before = annots.len();
    annots.retain(|a| {
        let subtype = doc.resolve(a).ok().and_then(|o| {
            o.as_dict().and_then(|d| doc.get_resolved(d, b"Subtype").ok()).and_then(|s| s.as_name().map(<[u8]>::to_vec))
        });
        !matches!(subtype.as_deref(), Some(s) if MARKUP.contains(&s) || s == b"Popup")
    });
    let removed = before - annots.len();
    if removed > 0 {
        set_page_annots(doc, p, annots, changes)?;
        doc.set_overlay(changes);
    }
    Ok(removed)
}

/// The current date and time as a PDF date, in UTC.
pub fn pdf_date_now() -> String {
    let secs = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs()) as i64;
    let (days, rest) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    // Days since 1970-01-01 → civil date (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!("D:{year:04}{month:02}{day:02}{:02}{:02}{:02}Z", rest / 3600, rest / 60 % 60, rest % 60)
}

// ----------------------------------------------------------------------

/// Records a changed object: in place if it is new, else as a replacement.
pub(super) fn store(changes: &mut Changes, id: ObjRef, obj: Object) {
    match changes.added.iter_mut().find(|(r, _)| *r == id) {
        Some(slot) => slot.1 = obj,
        None => {
            changes.replace.insert(id.num, (id, obj));
        }
    }
}

fn page_of(pages: &[Page], page: usize) -> Result<&Page> {
    let p = pages.get(page).ok_or_else(|| Error::Object(format!("page {} does not exist", page + 1)))?;
    if p.id.num == 0 {
        return Err(Error::Object("this page can't have comments (it isn't stored as an object of its own)".into()));
    }
    Ok(p)
}

/// The common entries of a new markup annotation.
fn markup(subtype: &[u8], rect: [f64; 4], color: [u8; 3], text: &str, author: &str) -> Dictionary {
    let now = pdf_date_now();
    let mut d = Dictionary::new();
    d.set(b"Type".to_vec(), name(b"Annot"));
    d.set(b"Subtype".to_vec(), name(subtype));
    d.set(b"Rect".to_vec(), Object::Array(rect.map(real).to_vec()));
    d.set(b"C".to_vec(), Object::Array(color.map(|v| real(v as f64 / 255.0)).to_vec()));
    d.set(b"F".to_vec(), Object::Integer(4)); // Print
    d.set(b"Contents".to_vec(), Object::String(crate::forms::encode_text_string(text)));
    d.set(b"T".to_vec(), Object::String(crate::forms::encode_text_string(author)));
    d.set(b"M".to_vec(), Object::String(now.clone().into_bytes()));
    d.set(b"CreationDate".to_vec(), Object::String(now.into_bytes()));
    d
}

/// A form XObject drawing `content` over `bbox`.
pub(super) fn appearance(bbox: [f64; 4], resources: Dictionary, content: String) -> Stream {
    let mut d = Dictionary::new();
    d.set(b"Type".to_vec(), name(b"XObject"));
    d.set(b"Subtype".to_vec(), name(b"Form"));
    d.set(b"BBox".to_vec(), Object::Array(bbox.map(real).to_vec()));
    d.set(b"Resources".to_vec(), Object::Dictionary(resources));
    d.set(b"Length".to_vec(), Object::Integer(content.len() as i64));
    Stream { dict: d, raw: content.into_bytes().into() }
}

/// Adds annotation `dict` (with appearance `ap`) to page `p`.
fn add_annotation(doc: &Document, p: &Page, mut dict: Dictionary, ap: Stream, changes: &mut Changes) -> Result<ObjRef> {
    let next = doc
        .max_object_number()
        .max(changes.added.iter().map(|(r, _)| r.num).max().unwrap_or(0))
        .max(changes.replace.keys().copied().max().unwrap_or(0))
        + 1;
    let (annot, ap_ref) = (ObjRef::new(next, 0), ObjRef::new(next + 1, 0));
    let mut aps = Dictionary::new();
    aps.set(b"N".to_vec(), Object::Reference(ap_ref));
    dict.set(b"AP".to_vec(), Object::Dictionary(aps));
    dict.set(b"P".to_vec(), Object::Reference(p.id));
    dict.set(b"NM".to_vec(), Object::String(format!("k-pdf-{}-{}", annot.num, pdf_date_now()).into_bytes()));
    let mut annots = page_annots(doc, p)?;
    annots.push(Object::Reference(annot));
    changes.added.push((ap_ref, Object::Stream(ap)));
    changes.added.push((annot, Object::Dictionary(dict)));
    set_page_annots(doc, p, annots, changes)?;
    doc.set_overlay(changes);
    Ok(annot)
}

/// The page's `/Annots` array, as stored.
pub(super) fn page_annots(doc: &Document, p: &Page) -> Result<Vec<Object>> {
    let raw = doc.get_object(p.id)?;
    let raw = raw.as_dict().ok_or_else(|| Error::Object("the page is not a dictionary".into()))?;
    Ok(match raw.get(b"Annots") {
        Some(a) => doc.resolve(a)?.as_array().map(<[Object]>::to_vec).unwrap_or_default(),
        None => Vec::new(),
    })
}

/// Stores the page's new `/Annots`: in the array object if it is one of
/// its own, else in the page.
pub(super) fn set_page_annots(doc: &Document, p: &Page, annots: Vec<Object>, changes: &mut Changes) -> Result<()> {
    let raw = doc.get_object(p.id)?;
    let mut raw = raw.as_dict().cloned().ok_or_else(|| Error::Object("the page is not a dictionary".into()))?;
    match raw.get(b"Annots") {
        Some(Object::Reference(r)) => {
            changes.replace.insert(r.num, (*r, Object::Array(annots)));
        }
        _ => {
            raw.set(b"Annots".to_vec(), Object::Array(annots));
            changes.replace.insert(p.id.num, (p.id, Object::Dictionary(raw)));
        }
    }
    Ok(())
}

fn color_of(doc: &Document, dict: &Dictionary) -> Option<[u8; 3]> {
    let Ok(Object::Array(c)) = doc.get_resolved(dict, b"C") else { return None };
    let v: Vec<f64> = c.iter().filter_map(|o| doc.resolve(o).ok().and_then(|o| o.as_f64())).collect();
    let to = |x: f64| (x.clamp(0.0, 1.0) * 255.0).round() as u8;
    match v[..] {
        [g] => Some([to(g); 3]),
        [r, g, b] => Some([to(r), to(g), to(b)]),
        [c, m, y, k] => Some([to((1.0 - c) * (1.0 - k)), to((1.0 - m) * (1.0 - k)), to((1.0 - y) * (1.0 - k))]),
        _ => None, // [] = transparent
    }
}

/// Display point → page space.
pub(super) fn to_user(display: &[f64; 6]) -> impl Fn(f32, f32) -> (f64, f64) + use<> {
    let [a, b, c, d, e, f] = *display;
    let det = a * d - b * c;
    let det = if det.abs() < 1e-12 { 1.0 } else { det };
    let inv = [d / det, -b / det, -c / det, a / det, (c * f - d * e) / det, (b * e - a * f) / det];
    move |x, y| {
        let (x, y) = (x as f64, y as f64);
        (inv[0] * x + inv[2] * y + inv[4], inv[1] * x + inv[3] * y + inv[5])
    }
}

/// The display-space bounding box of page-space points.
fn display_rect(m: &[f64; 6], points: &[(f64, f64)]) -> [f32; 4] {
    let shown = points.iter().map(|&(x, y)| (m[0] * x + m[2] * y + m[4], m[1] * x + m[3] * y + m[5]));
    let [l, t, r, b] = bounds(shown);
    [l as f32, t as f32, r as f32, b as f32]
}

/// `[min x, min y, max x, max y]` of some points.
pub(super) fn bounds(points: impl Iterator<Item = (f64, f64)>) -> [f64; 4] {
    points.fold([f64::MAX, f64::MAX, f64::MIN, f64::MIN], |[l, b, r, t], (x, y)| [l.min(x), b.min(y), r.max(x), t.max(y)])
}

pub(super) fn name(n: &[u8]) -> Object {
    Object::Name(n.to_vec())
}

pub(super) fn real(v: f64) -> Object {
    Object::Real((v * 1000.0).round() / 1000.0)
}

pub(super) fn num(v: f64) -> String {
    let s = format!("{v:.3}");
    let s = s.trim_end_matches('0').trim_end_matches('.');
    if s == "-0" { "0".into() } else { s.to_string() }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dates() {
        let d = pdf_date_now();
        assert_eq!(d.len(), 17, "{d}");
        assert!(d.starts_with("D:20") && d.ends_with('Z'));
    }
}
