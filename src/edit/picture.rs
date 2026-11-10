//! Putting a picture on a page, such as a signature: an image with a soft
//! mask (ISO 32000 §11.6.5.3) for its transparent parts, drawn over what
//! the page already shows.
//!
//! The picture becomes part of the page's content, not an annotation: it
//! prints, isn't listed among the comments, and isn't removed with them.
//! The page's existing content streams are left as they are; two small
//! streams are added around them (`q` before, `Q` and the picture after),
//! so whatever state the old content leaves behind can't move the picture.

use std::io::Write;

use super::annotate::{num, to_user};
use crate::document::Document;
use crate::error::{Error, Result};
use crate::object::{Dictionary, ObjRef, Object, Stream};
use crate::writer::Changes;

/// Draws a `width` × `height` picture (`rgba`: 4 bytes a pixel, rows top
/// to bottom, alpha not premultiplied) on page `page`, filling `rect`
/// (`[left, top, right, bottom]` in display points of the page as shown),
/// upright as the page is shown. Records it in `changes` like
/// [`super::edit_text`].
pub fn place_image(doc: &Document, page: usize, width: u32, height: u32, rgba: &[u8], rect: [f32; 4], changes: &mut Changes) -> Result<()> {
    let (w, h) = (width as usize, height as usize);
    if w == 0 || h == 0 || rgba.len() != w * h * 4 {
        return Err(Error::Object("the picture is empty".into()));
    }
    let [l, t, r, b] = rect;
    if r - l < 0.5 || b - t < 0.5 {
        return Err(Error::Object("the picture's area is too small".into()));
    }
    doc.set_overlay(changes);
    let pages = doc.pages()?;
    let p = pages.get(page).ok_or_else(|| Error::Object(format!("page {} does not exist", page + 1)))?;
    if p.id.num == 0 {
        return Err(Error::Object("this page can't be changed (it isn't stored as an object of its own)".into()));
    }
    let mut next = doc
        .max_object_number()
        .max(changes.added.iter().map(|(r, _)| r.num).max().unwrap_or(0))
        .max(changes.replace.keys().copied().max().unwrap_or(0));
    let mut add = |obj: Object| {
        next += 1;
        let id = ObjRef::new(next, 0);
        changes.added.push((id, obj));
        id
    };

    // The colors (gray when there are none, a third the size), and the
    // transparency as a soft mask.
    let pixels = rgba.chunks_exact(4);
    let gray = pixels.clone().all(|p| p[0] == p[1] && p[1] == p[2]);
    let colors: Vec<u8> = if gray { pixels.clone().map(|p| p[0]).collect() } else { pixels.clone().flat_map(|p| [p[0], p[1], p[2]]).collect() };
    let alpha: Vec<u8> = pixels.map(|p| p[3]).collect();
    let image_dict = |color_space: &[u8]| {
        let mut d = Dictionary::new();
        d.set(b"Type".to_vec(), Object::Name(b"XObject".to_vec()));
        d.set(b"Subtype".to_vec(), Object::Name(b"Image".to_vec()));
        d.set(b"Width".to_vec(), Object::Integer(w as i64));
        d.set(b"Height".to_vec(), Object::Integer(h as i64));
        d.set(b"ColorSpace".to_vec(), Object::Name(color_space.to_vec()));
        d.set(b"BitsPerComponent".to_vec(), Object::Integer(8));
        // Smooth when scaled, as signatures are drawn small.
        d.set(b"Interpolate".to_vec(), Object::Bool(true));
        d
    };
    let mut image = image_dict(if gray { b"DeviceGray" } else { b"DeviceRGB" });
    if alpha.iter().any(|&a| a < 255) {
        let mask = add(Object::Stream(flate_stream(image_dict(b"DeviceGray"), &alpha)?));
        image.set(b"SMask".to_vec(), Object::Reference(mask));
    }
    let image = add(Object::Stream(flate_stream(image, &colors)?));

    // A name for it that the page's resources don't use yet.
    let mut resources = doc.get_dict(&p.dict, b"Resources").unwrap_or_default();
    let mut xobjects = doc.get_dict(&resources, b"XObject").unwrap_or_default();
    let name = (1..).map(|i| format!("KPic{i}").into_bytes()).find(|n| xobjects.get(n).is_none()).expect("a free name");
    xobjects.set(name.clone(), Object::Reference(image));
    resources.set(b"XObject".to_vec(), Object::Dictionary(xobjects));

    // The image's unit square onto the area: its bottom left corner at the
    // area's bottom left as shown, whatever the page's rotation.
    let to_user = to_user(&p.display_matrix(doc));
    let (o, x, y) = (to_user(l, b), to_user(r, b), to_user(l, t));
    let draw = format!(
        "Q\nq {} {} {} {} {} {} cm /{} Do Q\n",
        num(x.0 - o.0),
        num(x.1 - o.1),
        num(y.0 - o.0),
        num(y.1 - o.1),
        num(o.0),
        num(o.1),
        String::from_utf8_lossy(&name)
    );
    let before = add(Object::Stream(flate_stream(Dictionary::new(), b"q\n")?));
    let after = add(Object::Stream(flate_stream(Dictionary::new(), draw.as_bytes())?));

    let mut contents = vec![Object::Reference(before)];
    match p.dict.get(b"Contents") {
        Some(Object::Reference(r)) => match doc.resolve(&Object::Reference(*r))? {
            Object::Array(items) => contents.extend(items),
            _ => contents.push(Object::Reference(*r)),
        },
        Some(Object::Array(items)) => contents.extend(items.iter().cloned()),
        _ => {}
    }
    contents.push(Object::Reference(after));

    let mut dict = p.dict.clone();
    dict.set(b"Resources".to_vec(), Object::Dictionary(resources));
    dict.set(b"Contents".to_vec(), Object::Array(contents));
    // A stored thumbnail would no longer match.
    dict.remove(b"Thumb");
    super::annotate::store(changes, p.id, Object::Dictionary(dict));
    doc.set_overlay(changes);
    Ok(())
}

/// A Flate-compressed stream with `dict` (plus the filter) and `data`.
fn flate_stream(mut dict: Dictionary, data: &[u8]) -> Result<Stream> {
    let mut enc = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
    enc.write_all(data)?;
    dict.set(b"Filter".to_vec(), Object::Name(b"FlateDecode".to_vec()));
    let raw = enc.finish()?;
    dict.set(b"Length".to_vec(), Object::Integer(raw.len() as i64));
    Ok(Stream { dict, raw })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_picture_is_drawn_after_the_page_content() {
        let doc = Document::from_bytes(crate::edit::fields::blank_document(612.0, 792.0)).unwrap();
        let mut changes = Changes::default();
        // A 2 × 1 picture: a dark blue pixel and a transparent one.
        let rgba = [20, 40, 140, 255, 20, 40, 140, 0];
        place_image(&doc, 0, 2, 1, &rgba, [100.0, 600.0, 300.0, 700.0], &mut changes).unwrap();
        place_image(&doc, 0, 2, 1, &rgba, [100.0, 100.0, 300.0, 200.0], &mut changes).unwrap();
        let page = &doc.pages().unwrap()[0];
        let content = String::from_utf8_lossy(&doc.page_content(page).unwrap()).into_owned();
        // Both, each after a balanced q … Q, at the right place: the
        // first's bottom left is 100 pt from the left and 92 pt up.
        assert!(content.contains("200 0 0 100 100 92 cm /KPic1 Do"), "{content}");
        assert!(content.contains("/KPic2 Do"), "{content}");
        let xobjects = doc.get_dict(&doc.get_dict(&page.dict, b"Resources").unwrap(), b"XObject").unwrap();
        let Ok(Object::Stream(image)) = doc.get_resolved(&xobjects, b"KPic1") else { panic!("no image") };
        assert!(image.dict.get(b"SMask").is_some());
    }
}
