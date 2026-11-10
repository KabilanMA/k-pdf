//! Making forms: adding form fields to pages, changing and moving them,
//! and deleting them (interactive forms, ISO 32000 §12.7).
//!
//! A new field is a single object that is both the field and its widget
//! annotation (§12.5.6.19 allows merging them), listed in the page's
//! `/Annots` and in the form's `/Fields`. The document gets an
//! `/AcroForm` if it has none, with Helvetica and ZapfDingbats in its
//! resources. Every field is given appearance streams, so other viewers
//! show it as it is drawn here; `NeedAppearances` is not relied on.
//!
//! Like other edits, these record into [`Changes`] and set the document's
//! overlay, so [`crate::form_fields`] sees the new fields at once and
//! [`crate::fill_form`] can fill them in before the file is saved.

use super::annotate::{bounds, name, num, page_annots, real, set_page_annots, store, to_user};
use crate::document::{Document, Page};
use crate::error::{Error, Result};
use crate::forms::{self, encode_text_string, FieldKind, FieldValue, FormField};
use crate::object::{Dictionary, ObjRef, Object, Stream};
use crate::writer::Changes;

/// The kinds of field that can be made.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NewField {
    /// A text box (one line, or several with [`FieldSpec::multiline`]).
    Text,
    CheckBox,
    /// A drop-down list of [`FieldSpec::options`].
    Dropdown,
}

/// A field's settings, for making it or changing it.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct FieldSpec {
    /// The field's name (its partial name, `/T`): no periods, unique in
    /// the form.
    pub name: String,
    /// A description shown by viewers as a tooltip (`/TU`); empty for none.
    pub tooltip: String,
    pub required: bool,
    /// Text fields: several lines, wrapped.
    pub multiline: bool,
    /// Text size in points; 0 fits the text to the field.
    pub font_size: f32,
    /// Drop-down lists: the choices, in order.
    pub options: Vec<String>,
}

impl FieldSpec {
    /// The settings `field` has now.
    pub fn of(field: &FormField) -> FieldSpec {
        let (multiline, options) = match &field.kind {
            FieldKind::Text { multiline, .. } => (*multiline, Vec::new()),
            FieldKind::ComboBox { options, .. } | FieldKind::ListBox { options, .. } => {
                (false, options.iter().map(|o| o.label.clone()).collect())
            }
            _ => (false, Vec::new()),
        };
        FieldSpec {
            name: field.name.rsplit('.').next().unwrap_or_default().to_string(),
            tooltip: field.tooltip.clone().unwrap_or_default(),
            required: field.required,
            multiline,
            font_size: field.font_size,
            options,
        }
    }
}

/// The border color of new fields (a mid gray, visible on white and on
/// most colored backgrounds).
const BORDER: [f64; 3] = [0.45, 0.45, 0.45];

/// A name for a new field of this kind that the form doesn't use yet:
/// "Text 1", "Check box 2"…
pub fn new_field_name(doc: &Document, kind: NewField) -> String {
    let base = match kind {
        NewField::Text => "Text",
        NewField::CheckBox => "Check box",
        NewField::Dropdown => "Drop-down",
    };
    let names: Vec<String> = crate::form_fields(doc).unwrap_or_default().into_iter().map(|f| f.name).collect();
    (1..).map(|i| format!("{base} {i}")).find(|n| !names.contains(n)).unwrap_or_default()
}

/// Checks that `name` can name a field of the form (other than `except`).
pub fn check_field_name(doc: &Document, name: &str, except: Option<ObjRef>) -> Result<()> {
    let name = name.trim();
    if name.is_empty() {
        return Err(Error::Object("a field needs a name".into()));
    }
    if name.contains('.') {
        return Err(Error::Object("field names can't contain periods".into()));
    }
    let taken = crate::form_fields(doc)?
        .iter()
        .any(|f| Some(f.id) != except && f.name.rsplit('.').next() == Some(name) && !f.name.contains('.'));
    if taken {
        return Err(Error::Object(format!("another field is already called \"{name}\"")));
    }
    Ok(())
}

/// Adds a field of `kind` on page `page`, covering `rect` (`[left, top,
/// right, bottom]` in display points). Returns the new field.
pub fn add_field(doc: &Document, page: usize, rect: [f32; 4], kind: NewField, spec: &FieldSpec, changes: &mut Changes) -> Result<ObjRef> {
    check_field_name(doc, &spec.name, None)?;
    doc.set_overlay(changes);
    let pages = doc.pages()?;
    let p = page_of(&pages, page)?;
    let acroform = acroform(doc, changes)?;

    let mut dict = Dictionary::new();
    dict.set(b"Type".to_vec(), name(b"Annot"));
    dict.set(b"Subtype".to_vec(), name(b"Widget"));
    dict.set(b"F".to_vec(), Object::Integer(4)); // Print
    dict.set(b"P".to_vec(), Object::Reference(p.id));
    dict.set(b"Rect".to_vec(), Object::Array(user_rect(doc, p, rect).map(real).to_vec()));
    let mut mk = Dictionary::new();
    mk.set(b"BC".to_vec(), Object::Array(BORDER.map(real).to_vec()));
    // Drawn upright on rotated pages.
    let rotation = p.rotation().rem_euclid(360);
    if rotation != 0 {
        mk.set(b"R".to_vec(), Object::Integer(rotation));
    }
    let mut bs = Dictionary::new();
    bs.set(b"W".to_vec(), Object::Integer(1));
    bs.set(b"S".to_vec(), name(b"S"));
    dict.set(b"BS".to_vec(), Object::Dictionary(bs));
    match kind {
        NewField::Text => {
            dict.set(b"FT".to_vec(), name(b"Tx"));
            dict.set(b"V".to_vec(), Object::String(Vec::new()));
        }
        NewField::CheckBox => {
            dict.set(b"FT".to_vec(), name(b"Btn"));
            dict.set(b"V".to_vec(), name(b"Off"));
            dict.set(b"AS".to_vec(), name(b"Off"));
            mk.set(b"CA".to_vec(), Object::String(b"4".to_vec())); // a check mark
        }
        NewField::Dropdown => {
            dict.set(b"FT".to_vec(), name(b"Ch"));
        }
    }
    dict.set(b"MK".to_vec(), Object::Dictionary(mk));
    apply_spec(&mut dict, kind, spec);

    let id = ObjRef::new(next_number(doc, changes), 0);
    changes.added.push((id, Object::Dictionary(dict)));
    let mut annots = page_annots(doc, p)?;
    annots.push(Object::Reference(id));
    set_page_annots(doc, p, annots, changes)?;
    let (af_ref, mut af) = acroform;
    let mut fields = match doc.get_resolved(&af, b"Fields") {
        Ok(Object::Array(f)) => f,
        _ => Vec::new(),
    };
    fields.push(Object::Reference(id));
    af.set(b"Fields".to_vec(), Object::Array(fields));
    store(changes, af_ref, Object::Dictionary(af));
    doc.set_overlay(changes);
    refresh_appearances(doc, id, changes)?;
    Ok(id)
}

/// Changes field `id`'s settings. Its kind stays the same.
pub fn update_field(doc: &Document, id: ObjRef, spec: &FieldSpec, changes: &mut Changes) -> Result<()> {
    check_field_name(doc, &spec.name, Some(id))?;
    doc.set_overlay(changes);
    let field = find_field(doc, id)?;
    let kind = match field.kind {
        FieldKind::Text { .. } => NewField::Text,
        FieldKind::CheckBox | FieldKind::Radio => NewField::CheckBox,
        FieldKind::ComboBox { .. } | FieldKind::ListBox { .. } => NewField::Dropdown,
        FieldKind::PushButton | FieldKind::Signature => {
            return Err(Error::Object("buttons and signature fields can't be changed".into()));
        }
    };
    let mut dict = object_dict(doc, id)?;
    // Keep the flags this module doesn't set, but take the inherited ones
    // into account (they are what the field has now).
    apply_spec(&mut dict, kind, spec);
    if kind == NewField::Dropdown
        && let FieldValue::Choice(selected) = &field.value
        && selected.iter().any(|v| !spec.options.contains(v))
    {
        dict.remove(b"V"); // The chosen value is no longer a choice.
    }
    store(changes, id, Object::Dictionary(dict));
    // Widgets that are objects of their own may have their own text size.
    for w in field.widgets.iter().filter(|w| w.id != id) {
        let mut wd = object_dict(doc, w.id)?;
        if wd.contains(b"DA") {
            set_da(&mut wd, spec.font_size);
            store(changes, w.id, Object::Dictionary(wd));
        }
    }
    doc.set_overlay(changes);
    refresh_appearances(doc, id, changes)
}

/// Moves widget `widget` (of a field) to `rect` on its page (display
/// points), resizing it to fit.
pub fn move_widget(doc: &Document, widget: ObjRef, rect: [f32; 4], changes: &mut Changes) -> Result<()> {
    doc.set_overlay(changes);
    let fields = crate::form_fields(doc)?;
    let field = fields
        .iter()
        .find(|f| f.widgets.iter().any(|w| w.id == widget))
        .ok_or_else(|| Error::Object("the field isn't in the form".into()))?;
    let page = field.widgets.iter().find(|w| w.id == widget).map(|w| w.page).unwrap_or(0);
    let pages = doc.pages()?;
    let p = page_of(&pages, page)?;
    let mut dict = object_dict(doc, widget)?;
    dict.set(b"Rect".to_vec(), Object::Array(user_rect(doc, p, rect).map(real).to_vec()));
    store(changes, widget, Object::Dictionary(dict));
    doc.set_overlay(changes);
    refresh_appearances(doc, field.id, changes)
}

/// Deletes field `id`: its widgets leave their pages, and the field leaves
/// the form.
pub fn delete_field(doc: &Document, id: ObjRef, changes: &mut Changes) -> Result<()> {
    doc.set_overlay(changes);
    let field = find_field(doc, id)?;
    let pages = doc.pages()?;
    let mut widget_pages: Vec<usize> = field.widgets.iter().map(|w| w.page).collect();
    widget_pages.dedup();
    for page in widget_pages {
        let p = page_of(&pages, page)?;
        let mut annots = page_annots(doc, p)?;
        annots.retain(|a| !matches!(a, Object::Reference(r) if *r == id || field.widgets.iter().any(|w| w.id == *r)));
        set_page_annots(doc, p, annots, changes)?;
    }
    // Out of its parent's kids, or the form's list of fields.
    let dict = object_dict(doc, id)?;
    let (holder, key, mut holder_dict) = match dict.get(b"Parent").and_then(Object::as_reference) {
        Some(parent) => (parent, &b"Kids"[..], object_dict(doc, parent)?),
        None => {
            let (r, af) = acroform(doc, changes)?;
            (r, &b"Fields"[..], af)
        }
    };
    if let Ok(Object::Array(mut list)) = doc.get_resolved(&holder_dict, key) {
        list.retain(|o| o.as_reference() != Some(id));
        match holder_dict.get(key) {
            // A list stored as an object of its own.
            Some(Object::Reference(r)) => store(changes, *r, Object::Array(list)),
            _ => {
                holder_dict.set(key.to_vec(), Object::Array(list));
                store(changes, holder, Object::Dictionary(holder_dict));
            }
        }
    }
    doc.set_overlay(changes);
    Ok(())
}

/// A PDF with one blank page of `width` × `height` points, to start a form
/// from.
pub fn blank_document(width: f32, height: f32) -> Vec<u8> {
    let objects = [
        "<< /Type /Catalog /Pages 2 0 R >>".to_string(),
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_string(),
        format!("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 {} {}] /Resources << >> >>", num(width as f64), num(height as f64)),
    ];
    let mut out = b"%PDF-1.7\n%\xe2\xe3\xcf\xd3\n".to_vec();
    let mut offsets = Vec::new();
    for (i, o) in objects.iter().enumerate() {
        offsets.push(out.len());
        out.extend_from_slice(format!("{} 0 obj\n{o}\nendobj\n", i + 1).as_bytes());
    }
    let xref = out.len();
    out.extend_from_slice(format!("xref\n0 {}\n0000000000 65535 f \n", objects.len() + 1).as_bytes());
    for o in offsets {
        out.extend_from_slice(format!("{o:010} 00000 n \n").as_bytes());
    }
    out.extend_from_slice(format!("trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n", objects.len() + 1).as_bytes());
    out
}

// ----------------------------------------------------------------------

/// Sets the entries `spec` describes on a field's dictionary.
fn apply_spec(dict: &mut Dictionary, kind: NewField, spec: &FieldSpec) {
    dict.set(b"T".to_vec(), Object::String(encode_text_string(spec.name.trim())));
    let tooltip = spec.tooltip.trim();
    if tooltip.is_empty() {
        dict.remove(b"TU");
    } else {
        dict.set(b"TU".to_vec(), Object::String(encode_text_string(tooltip)));
    }
    let mut ff = dict.get(b"Ff").and_then(Object::as_i64).unwrap_or(0);
    let mut flag = |bit: i64, on: bool| {
        if on { ff |= bit } else { ff &= !bit }
    };
    flag(forms::REQUIRED, spec.required);
    match kind {
        NewField::Text => flag(forms::MULTILINE, spec.multiline),
        NewField::Dropdown => {
            flag(forms::COMBO, true);
            let options = spec.options.iter().map(|o| o.trim()).filter(|o| !o.is_empty());
            dict.set(b"Opt".to_vec(), Object::Array(options.map(|o| Object::String(encode_text_string(o))).collect()));
        }
        NewField::CheckBox => {}
    }
    if ff == 0 {
        dict.remove(b"Ff");
    } else {
        dict.set(b"Ff".to_vec(), Object::Integer(ff));
    }
    if kind != NewField::CheckBox {
        set_da(dict, spec.font_size);
    }
}

/// Sets the text size in a default appearance string, keeping its font
/// and color (Helvetica in black for a new one).
fn set_da(dict: &mut Dictionary, size: f32) {
    let old = dict.get(b"DA").and_then(Object::as_string).map(<[u8]>::to_vec);
    let da = forms::parse_da(old.as_deref().unwrap_or(b"/Helv 0 Tf 0 g"));
    let [r, g, b] = da.color.map(f64::from);
    let color = if r == g && g == b { format!("{} g", num(r)) } else { format!("{} {} {} rg", num(r), num(g), num(b)) };
    let font = String::from_utf8_lossy(&da.font).into_owned();
    dict.set(b"DA".to_vec(), Object::String(format!("/{font} {} Tf {color}", num(size.max(0.0) as f64)).into_bytes()));
}

/// The document's form dictionary, as an object of its own (made, or
/// moved out of the catalog, if needed) with the resources new fields
/// use. The changes to it are recorded.
fn acroform(doc: &Document, changes: &mut Changes) -> Result<(ObjRef, Dictionary)> {
    let root = doc
        .trailer()
        .get(b"Root")
        .and_then(Object::as_reference)
        .ok_or_else(|| Error::Object("the document has no catalog".into()))?;
    let mut catalog = object_dict(doc, root)?;
    let (r, mut af) = match catalog.get(b"AcroForm") {
        Some(Object::Reference(r)) => (*r, object_dict(doc, *r)?),
        other => {
            let af = other.and_then(Object::as_dict).cloned().unwrap_or_default();
            let r = ObjRef::new(next_number(doc, changes), 0);
            changes.added.push((r, Object::Dictionary(af.clone())));
            catalog.set(b"AcroForm".to_vec(), Object::Reference(r));
            store(changes, root, Object::Dictionary(catalog));
            (r, af)
        }
    };
    if !af.contains(b"Fields") {
        af.set(b"Fields".to_vec(), Object::Array(Vec::new()));
    }
    if !af.contains(b"DA") {
        af.set(b"DA".to_vec(), Object::String(b"/Helv 0 Tf 0 g".to_vec()));
    }
    // The fonts fields' text uses, by the names in their /DA.
    let mut dr = doc.get_dict(&af, b"DR").unwrap_or_default();
    let mut fonts = doc.get_dict(&dr, b"Font").unwrap_or_default();
    for (key, base) in [(&b"Helv"[..], &b"Helvetica"[..]), (b"ZaDb", b"ZapfDingbats")] {
        if !fonts.contains(key) {
            fonts.set(key.to_vec(), Object::Dictionary(std_font(base)));
        }
    }
    dr.set(b"Font".to_vec(), Object::Dictionary(fonts));
    af.set(b"DR".to_vec(), Object::Dictionary(dr));
    // Appearances are made for every field.
    af.remove(b"NeedAppearances");
    store(changes, r, Object::Dictionary(af.clone()));
    doc.set_overlay(changes);
    Ok((r, af))
}

fn std_font(base: &[u8]) -> Dictionary {
    let mut f = Dictionary::new();
    f.set(b"Type".to_vec(), name(b"Font"));
    f.set(b"Subtype".to_vec(), name(b"Type1"));
    f.set(b"BaseFont".to_vec(), name(base));
    if base != b"ZapfDingbats" {
        f.set(b"Encoding".to_vec(), name(b"WinAnsiEncoding"));
    }
    f
}

/// Makes new appearances for each of field `id`'s widgets, as they are
/// now (size, settings and value).
fn refresh_appearances(doc: &Document, id: ObjRef, changes: &mut Changes) -> Result<()> {
    let field = find_field(doc, id)?;
    let (_, af) = acroform(doc, changes)?;
    let fonts = doc.get_dict(&af, b"DR").and_then(|dr| doc.get_dict(&dr, b"Font")).unwrap_or_default();
    for w in &field.widgets {
        let mut wd = object_dict(doc, w.id)?;
        let mut ap = Dictionary::new();
        match field.kind {
            FieldKind::CheckBox | FieldKind::Radio => {
                let on = w.on_state.clone().unwrap_or_else(|| "Yes".into());
                let [x0, y0, x1, y1] = doc.rect(wd.get(b"Rect")).unwrap_or([0.0; 4]);
                let rotation = doc.get_dict(&wd, b"MK").and_then(|mk| mk.get(b"R").and_then(Object::as_i64)).unwrap_or(0).rem_euclid(360);
                let style = w.check_style.unwrap_or('4');
                let mut states = Dictionary::new();
                for (state, checked) in [(on.as_bytes(), true), (&b"Off"[..], false)] {
                    let stream = check_box_appearance(x1 - x0, y1 - y0, rotation, style, checked);
                    let r = ObjRef::new(next_number(doc, changes), 0);
                    changes.added.push((r, Object::Stream(stream)));
                    states.set(state.to_vec(), Object::Reference(r));
                }
                ap.set(b"N".to_vec(), Object::Dictionary(states));
                if !wd.contains(b"AS") {
                    wd.set(b"AS".to_vec(), name(b"Off"));
                }
            }
            FieldKind::Text { .. } | FieldKind::ComboBox { .. } | FieldKind::ListBox { .. } => {
                let Some(stream) = forms::appearance(doc, &field, &field.value, &wd, &fonts) else { continue };
                let r = ObjRef::new(next_number(doc, changes), 0);
                changes.added.push((r, Object::Stream(stream)));
                ap.set(b"N".to_vec(), Object::Reference(r));
            }
            FieldKind::PushButton | FieldKind::Signature => continue,
        }
        wd.set(b"AP".to_vec(), Object::Dictionary(ap));
        store(changes, w.id, Object::Dictionary(wd));
    }
    doc.set_overlay(changes);
    Ok(())
}

/// A check box's look, checked or not: its border, and a ZapfDingbats
/// mark (`style`, see `Widget::check_style`) when checked.
fn check_box_appearance(w: f64, h: f64, rotation: i64, style: char, checked: bool) -> Stream {
    let (bw, bh, matrix) = match rotation {
        90 => (h, w, Some([0.0, 1.0, -1.0, 0.0, w, 0.0])),
        180 => (w, h, Some([-1.0, 0.0, 0.0, -1.0, w, h])),
        270 => (h, w, Some([0.0, -1.0, 1.0, 0.0, 0.0, h])),
        _ => (w, h, None),
    };
    let [r, g, b] = BORDER;
    let mut ops = format!("{} {} {} RG 1 w 0.5 0.5 {} {} re S\n", num(r), num(g), num(b), num(bw - 1.0), num(bh - 1.0));
    if checked {
        let size = (bw.min(bh) * 0.8).max(1.0);
        // ZapfDingbats marks are about 0.8 em wide and 0.7 em tall.
        let (x, y) = ((bw - size * 0.8) / 2.0, (bh - size * 0.7) / 2.0);
        let c = Object::String(vec![style as u8]).to_string();
        ops += &format!("q BT 0 g /ZaDb {} Tf {} {} Td {c} Tj ET Q\n", num(size), num(x), num(y));
    }
    let mut fonts = Dictionary::new();
    fonts.set(b"ZaDb".to_vec(), Object::Dictionary(std_font(b"ZapfDingbats")));
    let mut resources = Dictionary::new();
    resources.set(b"Font".to_vec(), Object::Dictionary(fonts));
    let mut d = Dictionary::new();
    d.set(b"Type".to_vec(), name(b"XObject"));
    d.set(b"Subtype".to_vec(), name(b"Form"));
    d.set(b"BBox".to_vec(), Object::Array([0.0, 0.0, bw, bh].map(real).to_vec()));
    if let Some(m) = matrix {
        d.set(b"Matrix".to_vec(), Object::Array(m.map(real).to_vec()));
    }
    d.set(b"Resources".to_vec(), Object::Dictionary(resources));
    Stream { dict: d, raw: ops.into_bytes() }
}

fn find_field(doc: &Document, id: ObjRef) -> Result<FormField> {
    crate::form_fields(doc)?
        .into_iter()
        .find(|f| f.id == id)
        .ok_or_else(|| Error::Object("the field isn't in the form".into()))
}

fn object_dict(doc: &Document, id: ObjRef) -> Result<Dictionary> {
    doc.get_object(id)?
        .as_dict()
        .cloned()
        .ok_or_else(|| Error::Object(format!("object {} is not a dictionary", id.num)))
}

fn page_of(pages: &[Page], page: usize) -> Result<&Page> {
    let p = pages.get(page).ok_or_else(|| Error::Object(format!("page {} does not exist", page + 1)))?;
    if p.id.num == 0 {
        return Err(Error::Object("this page can't have form fields (it isn't stored as an object of its own)".into()));
    }
    Ok(p)
}

/// A display-space rectangle in the page's own space.
fn user_rect(doc: &Document, p: &Page, [l, t, r, b]: [f32; 4]) -> [f64; 4] {
    let to_user = to_user(&p.display_matrix(doc));
    bounds([to_user(l, t), to_user(r, t), to_user(l, b), to_user(r, b)].into_iter())
}

/// The next unused object number.
fn next_number(doc: &Document, changes: &Changes) -> u32 {
    doc.max_object_number()
        .max(changes.added.iter().map(|(r, _)| r.num).max().unwrap_or(0))
        .max(changes.replace.keys().copied().max().unwrap_or(0))
        + 1
}

#[cfg(test)]
mod tests {
    use super::*;

    fn blank() -> Document {
        Document::from_bytes(blank_document(612.0, 792.0)).unwrap()
    }

    #[test]
    fn making_a_form() {
        let doc = blank();
        let mut changes = Changes::default();
        let spec = |name: &str| FieldSpec { name: name.into(), ..FieldSpec::default() };
        let name = add_field(&doc, 0, [72.0, 72.0, 272.0, 94.0], NewField::Text, &spec("Name"), &mut changes).unwrap();
        let agree = add_field(&doc, 0, [72.0, 120.0, 86.0, 134.0], NewField::CheckBox, &spec("Agree"), &mut changes).unwrap();
        let color = FieldSpec { options: vec!["Red".into(), "Blue".into()], ..spec("Color") };
        add_field(&doc, 0, [72.0, 160.0, 200.0, 180.0], NewField::Dropdown, &color, &mut changes).unwrap();
        assert!(add_field(&doc, 0, [0.0, 0.0, 10.0, 10.0], NewField::Text, &spec("Name"), &mut changes).is_err());

        let fields = crate::form_fields(&doc).unwrap();
        let names: Vec<&str> = fields.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(names, ["Name", "Agree", "Color"]);
        let [l, t, r, b] = fields[0].widgets[0].rect;
        assert!((l - 72.0).abs() < 0.01 && (t - 72.0).abs() < 0.01 && (r - 272.0).abs() < 0.01 && (b - 94.0).abs() < 0.01);
        assert_eq!(fields[1].kind, FieldKind::CheckBox);
        assert_eq!(fields[1].widgets[0].on_state.as_deref(), Some("Yes"));
        assert!(matches!(&fields[2].kind, FieldKind::ComboBox { options, .. } if options.len() == 2));

        // Changing, moving and deleting.
        let mut renamed = FieldSpec { multiline: true, required: true, ..spec("Full name") };
        renamed.font_size = 11.0;
        update_field(&doc, name, &renamed, &mut changes).unwrap();
        move_widget(&doc, name, [100.0, 300.0, 400.0, 360.0], &mut changes).unwrap();
        delete_field(&doc, agree, &mut changes).unwrap();
        let fields = crate::form_fields(&doc).unwrap();
        assert_eq!(fields.len(), 2);
        assert_eq!(fields[0].name, "Full name");
        assert!(fields[0].required && fields[0].font_size == 11.0);
        assert!(matches!(fields[0].kind, FieldKind::Text { multiline: true, .. }));
        assert_eq!(fields[0].widgets[0].rect.map(f32::round), [100.0, 300.0, 400.0, 360.0]);

        // Filled in and written, the fields are read back from the file.
        let filled = crate::fill_form(&doc, &fields, &[(name, FieldValue::Text("Ada".into()))]).unwrap();
        let mut all = changes.clone();
        all.replace.extend(filled.replace);
        all.added.extend(filled.added);
        let mut out = Vec::new();
        crate::write_document_with_changes(&[&doc], &[(0, 0)], &all, &mut out).unwrap();
        let saved = Document::from_bytes(out).unwrap();
        let fields = crate::form_fields(&saved).unwrap();
        assert_eq!(fields.len(), 2);
        assert_eq!(fields[0].value, FieldValue::Text("Ada".into()));
        assert_eq!(saved.pages().unwrap()[0].dict.get(b"Annots").and_then(|a| saved.resolve(a).ok()).and_then(|a| a.as_array().map(<[Object]>::len)), Some(2));
    }
}
