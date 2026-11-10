//! Interactive forms (AcroForm, ISO 32000 §12.7): reading the fields of a
//! document and filling them in.
//!
//! [`form_fields`] lists the fields with their kind, current value and
//! widgets (where they appear on which page). [`fill_form`] turns new
//! values into [`Changes`] for the writer: each field's `/V`, the on/off
//! state of check boxes and radio buttons, and new appearance streams for
//! text and choice fields, so every viewer shows the new values. XFA form
//! data, which would override the values in some viewers, is removed.

use std::collections::HashMap;

use crate::document::{Document, Page};
use crate::encoding::{decode_text_string, BaseEncoding};
use crate::error::{Error, Result};
use crate::font::Font;
use crate::object::{Dictionary, ObjRef, Object, Stream};
use crate::writer::Changes;

// Field flags (/Ff).
pub(crate) const READ_ONLY: i64 = 1;
pub(crate) const REQUIRED: i64 = 1 << 1;
pub(crate) const MULTILINE: i64 = 1 << 12;
const PASSWORD: i64 = 1 << 13;
const RADIO: i64 = 1 << 15;
const PUSHBUTTON: i64 = 1 << 16;
pub(crate) const COMBO: i64 = 1 << 17;
const EDIT: i64 = 1 << 18;
const MULTI_SELECT: i64 = 1 << 21;
const COMB: i64 = 1 << 24;

/// Limit on field tree depth and size, against hostile files.
const MAX_DEPTH: usize = 32;
const MAX_FIELDS: usize = 100_000;

/// A form field (a terminal field of the field tree).
#[derive(Debug, Clone, PartialEq)]
pub struct FormField {
    /// The field's object; identifies it when filling the form.
    pub id: ObjRef,
    /// Fully qualified name (`parent.child`).
    pub name: String,
    /// The field's description (`/TU`), meant to be shown to users.
    pub tooltip: Option<String>,
    pub kind: FieldKind,
    pub value: FieldValue,
    pub read_only: bool,
    pub required: bool,
    /// Text size from the default appearance (0 = fit to the field).
    pub font_size: f32,
    /// Text color from the default appearance (RGB, 0–1).
    pub text_color: [f32; 3],
    /// 0 = left, 1 = centered, 2 = right.
    pub alignment: u8,
    pub widgets: Vec<Widget>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum FieldKind {
    Text { multiline: bool, password: bool, comb: bool, max_len: Option<usize> },
    CheckBox,
    Radio,
    ComboBox { options: Vec<ChoiceOption>, editable: bool },
    ListBox { options: Vec<ChoiceOption>, multi_select: bool },
    PushButton,
    Signature,
}

/// One entry of a choice field: the value stored in the file, and the text
/// shown for it (often the same).
#[derive(Debug, Clone, PartialEq)]
pub struct ChoiceOption {
    pub value: String,
    pub label: String,
}

#[derive(Debug, Clone, PartialEq)]
pub enum FieldValue {
    Text(String),
    /// Check boxes and radio buttons: the "on" state that is selected
    /// (each widget has its own; see [`Widget::on_state`]), or none.
    Button(Option<String>),
    /// Choice fields: the selected values.
    Choice(Vec<String>),
    /// Push buttons and signatures.
    None,
}

/// Where a field appears: one of its widget annotations.
#[derive(Debug, Clone, PartialEq)]
pub struct Widget {
    pub id: ObjRef,
    /// Index into [`Document::pages`].
    pub page: usize,
    /// `[left, top, right, bottom]` in display points on that page.
    pub rect: [f32; 4],
    /// Check boxes and radio buttons: the name of this widget's "on" state.
    pub on_state: Option<String>,
    /// Background and border colors from `/MK` (RGB, 0–1).
    pub background: Option<[f32; 3]>,
    pub border: Option<[f32; 3]>,
    /// The ZapfDingbats character drawn when checked (`/MK /CA`), if
    /// given: '4' is a check mark, '8' a cross, 'l' a dot, 'n' a square,
    /// 'u' a diamond, 'H' a star.
    pub check_style: Option<char>,
}

/// Attributes fields inherit from their ancestors.
#[derive(Clone, Default)]
struct Inherited {
    name: String,
    ft: Option<Vec<u8>>,
    ff: i64,
    v: Option<Object>,
    da: Option<Vec<u8>>,
    q: i64,
    opt: Option<Object>,
    max_len: Option<i64>,
}

/// Lists the document's form fields, in field tree order. Documents
/// without a form give an empty list.
pub fn form_fields(doc: &Document) -> Result<Vec<FormField>> {
    let catalog = doc.catalog()?;
    let Some(acroform) = doc.get_dict(&catalog, b"AcroForm") else { return Ok(Vec::new()) };
    let Ok(Object::Array(roots)) = doc.get_resolved(&acroform, b"Fields") else { return Ok(Vec::new()) };
    let pages = doc.pages()?;
    let page_of = widget_pages(doc, &pages);
    let default = Inherited {
        da: doc.get_resolved(&acroform, b"DA").ok().and_then(|o| o.as_string().map(<[u8]>::to_vec)),
        q: doc.get_resolved(&acroform, b"Q").ok().and_then(|o| o.as_i64()).unwrap_or(0),
        ..Inherited::default()
    };

    let mut fields = Vec::new();
    let mut seen = std::collections::HashSet::new();
    let mut stack: Vec<(Object, Inherited, usize)> = roots.into_iter().rev().map(|r| (r, default.clone(), 0)).collect();
    while let Some((node, parent, depth)) = stack.pop() {
        let Some(id) = node.as_reference() else { continue };
        if depth > MAX_DEPTH || !seen.insert(id) || fields.len() > MAX_FIELDS {
            continue;
        }
        let Some(dict) = doc.resolve(&node).ok().and_then(|o| o.as_dict().cloned()) else { continue };
        let mut inh = parent;
        if let Some(t) = doc.get_resolved(&dict, b"T").ok().and_then(|o| o.as_string().map(decode_text_string)) {
            inh.name = if inh.name.is_empty() { t } else { format!("{}.{t}", inh.name) };
        }
        let get = |key: &[u8]| doc.get_resolved(&dict, key).ok().filter(|o| !o.is_null());
        if let Some(ft) = get(b"FT").and_then(|o| o.as_name().map(<[u8]>::to_vec)) {
            inh.ft = Some(ft);
        }
        if let Some(ff) = get(b"Ff").and_then(|o| o.as_i64()) {
            inh.ff = ff;
        }
        if let Some(v) = get(b"V") {
            inh.v = Some(v);
        }
        if let Some(da) = get(b"DA").and_then(|o| o.as_string().map(<[u8]>::to_vec)) {
            inh.da = Some(da);
        }
        if let Some(q) = get(b"Q").and_then(|o| o.as_i64()) {
            inh.q = q;
        }
        if let Some(opt) = get(b"Opt") {
            inh.opt = Some(opt);
        }
        if let Some(m) = get(b"MaxLen").and_then(|o| o.as_i64()) {
            inh.max_len = Some(m);
        }

        // Kids with a /T are fields of their own; kids without are this
        // field's widgets.
        let kids: Vec<Object> = match doc.get_resolved(&dict, b"Kids") {
            Ok(Object::Array(k)) => k,
            _ => Vec::new(),
        };
        let kid_dicts: Vec<(Object, Option<Dictionary>)> =
            kids.iter().map(|k| (k.clone(), doc.resolve(k).ok().and_then(|o| o.as_dict().cloned()))).collect();
        let child_fields = kid_dicts.iter().any(|(_, d)| d.as_ref().is_some_and(|d| d.contains(b"T")));
        if child_fields {
            for (kid, _) in kid_dicts.into_iter().rev() {
                stack.push((kid, inh.clone(), depth + 1));
            }
            continue;
        }
        let widget_objs: Vec<(Object, Dictionary)> = if kid_dicts.is_empty() {
            vec![(node.clone(), dict.clone())]
        } else {
            kid_dicts.into_iter().filter_map(|(k, d)| Some((k, d?))).collect()
        };
        let widgets: Vec<Widget> =
            widget_objs.iter().filter_map(|(obj, d)| widget(doc, &pages, &page_of, obj.as_reference()?, d)).collect();
        fields.push(make_field(doc, id, &dict, &inh, widgets));
    }
    Ok(fields)
}

/// Widget annotation object number → page index.
fn widget_pages(doc: &Document, pages: &[Page]) -> HashMap<u32, usize> {
    let mut map = HashMap::new();
    for (i, page) in pages.iter().enumerate() {
        if let Ok(Object::Array(annots)) = doc.get_resolved(&page.dict, b"Annots") {
            for a in annots {
                if let Some(r) = a.as_reference() {
                    map.entry(r.num).or_insert(i);
                }
            }
        }
    }
    map
}

fn widget(doc: &Document, pages: &[Page], page_of: &HashMap<u32, usize>, id: ObjRef, dict: &Dictionary) -> Option<Widget> {
    let flags = doc.get_resolved(dict, b"F").ok().and_then(|o| o.as_i64()).unwrap_or(0);
    if flags & 2 != 0 {
        return None; // hidden
    }
    let page = match page_of.get(&id.num) {
        Some(&p) => p,
        None => {
            let p = dict.get(b"P").and_then(Object::as_reference)?;
            pages.iter().position(|pg| pg.id == p)?
        }
    };
    let [x0, y0, x1, y1] = doc.rect(dict.get(b"Rect"))?;
    let m = pages[page].display_matrix(doc);
    let pts = [(x0, y0), (x1, y1)].map(|(x, y)| (m[0] * x + m[2] * y + m[4], m[1] * x + m[3] * y + m[5]));
    let rect = [
        pts[0].0.min(pts[1].0) as f32,
        pts[0].1.min(pts[1].1) as f32,
        pts[0].0.max(pts[1].0) as f32,
        pts[0].1.max(pts[1].1) as f32,
    ];
    let on_state = doc
        .get_dict(dict, b"AP")
        .and_then(|ap| doc.get_dict(&ap, b"N"))
        .and_then(|n| n.iter().map(|(k, _)| k).find(|k| *k != b"Off").map(|k| String::from_utf8_lossy(k).into_owned()));
    let mk = doc.get_dict(dict, b"MK");
    let color = |key: &[u8]| mk.as_ref().and_then(|mk| color_of(doc.get_resolved(mk, key).ok()?.as_array()?));
    let check_style = mk
        .as_ref()
        .and_then(|mk| doc.get_resolved(mk, b"CA").ok())
        .and_then(|o| o.as_string().and_then(|s| s.first().copied()))
        .map(|b| b as char);
    Some(Widget { id, page, rect, on_state, background: color(b"BG"), border: color(b"BC"), check_style })
}

/// RGB from a gray, RGB or CMYK color array (an empty array is "no color").
fn color_of(arr: &[Object]) -> Option<[f32; 3]> {
    let c: Vec<f32> = arr.iter().filter_map(Object::as_f64).map(|v| v as f32).collect();
    match c[..] {
        [g] => Some([g, g, g]),
        [r, g, b] => Some([r, g, b]),
        [c, m, y, k] => Some([(1.0 - c) * (1.0 - k), (1.0 - m) * (1.0 - k), (1.0 - y) * (1.0 - k)]),
        _ => None,
    }
}

fn make_field(doc: &Document, id: ObjRef, dict: &Dictionary, inh: &Inherited, widgets: Vec<Widget>) -> FormField {
    let ff = inh.ff;
    let options = || -> Vec<ChoiceOption> {
        let Some(Object::Array(opts)) = &inh.opt else { return Vec::new() };
        opts.iter()
            .filter_map(|o| match doc.resolve(o).ok()? {
                Object::String(s) => {
                    let s = decode_text_string(&s);
                    Some(ChoiceOption { value: s.clone(), label: s })
                }
                Object::Array(pair) => {
                    let text = |i: usize| pair.get(i).and_then(|o| doc.resolve(o).ok()).and_then(|o| o.as_string().map(decode_text_string));
                    let value = text(0)?;
                    Some(ChoiceOption { label: text(1).unwrap_or_else(|| value.clone()), value })
                }
                _ => None,
            })
            .collect()
    };
    let kind = match inh.ft.as_deref() {
        Some(b"Btn") if ff & PUSHBUTTON != 0 => FieldKind::PushButton,
        Some(b"Btn") if ff & RADIO != 0 => FieldKind::Radio,
        Some(b"Btn") => FieldKind::CheckBox,
        Some(b"Ch") if ff & COMBO != 0 => FieldKind::ComboBox { options: options(), editable: ff & EDIT != 0 },
        Some(b"Ch") => FieldKind::ListBox { options: options(), multi_select: ff & MULTI_SELECT != 0 },
        Some(b"Sig") => FieldKind::Signature,
        _ => FieldKind::Text {
            multiline: ff & MULTILINE != 0,
            password: ff & PASSWORD != 0,
            comb: ff & COMB != 0,
            max_len: inh.max_len.and_then(|m| usize::try_from(m).ok()).filter(|&m| m > 0),
        },
    };
    let text_of = |o: &Object| match o {
        Object::String(s) => Some(decode_text_string(s)),
        Object::Name(n) => Some(String::from_utf8_lossy(n).into_owned()),
        _ => None,
    };
    let value = match &kind {
        FieldKind::Text { .. } => FieldValue::Text(inh.v.as_ref().and_then(text_of).unwrap_or_default()),
        FieldKind::CheckBox | FieldKind::Radio => {
            let v = inh.v.as_ref().and_then(Object::as_name).map(|n| String::from_utf8_lossy(n).into_owned());
            // No /V: fall back to the state the widgets show.
            let v = v.or_else(|| {
                widgets.iter().find_map(|w| {
                    let wd = doc.get_object(w.id).ok()?;
                    let state = wd.as_dict()?.get(b"AS")?.as_name()?.to_vec();
                    (state != b"Off").then(|| String::from_utf8_lossy(&state).into_owned())
                })
            });
            FieldValue::Button(v.filter(|v| v != "Off"))
        }
        FieldKind::ComboBox { .. } | FieldKind::ListBox { .. } => FieldValue::Choice(match &inh.v {
            Some(Object::Array(a)) => a.iter().filter_map(text_of).collect(),
            Some(o) => text_of(o).into_iter().collect(),
            None => Vec::new(),
        }),
        FieldKind::PushButton | FieldKind::Signature => FieldValue::None,
    };
    let da = parse_da(inh.da.as_deref().unwrap_or(b""));
    FormField {
        id,
        name: inh.name.clone(),
        tooltip: doc.get_resolved(dict, b"TU").ok().and_then(|o| o.as_string().map(decode_text_string)),
        kind,
        value,
        read_only: ff & READ_ONLY != 0,
        required: ff & REQUIRED != 0,
        font_size: da.size,
        text_color: da.color,
        alignment: inh.q.clamp(0, 2) as u8,
        widgets,
    }
}

/// The parts of a default appearance string (`/Helv 0 Tf 0 g`) we use.
pub(crate) struct DefaultAppearance {
    pub font: Vec<u8>,
    pub size: f32,
    pub color: [f32; 3],
}

pub(crate) fn parse_da(da: &[u8]) -> DefaultAppearance {
    let text = String::from_utf8_lossy(da);
    let tokens: Vec<&str> = text.split_whitespace().collect();
    let mut out = DefaultAppearance { font: b"Helv".to_vec(), size: 0.0, color: [0.0; 3] };
    let num = |i: usize| tokens.get(i).and_then(|t| t.parse::<f32>().ok());
    for (i, tok) in tokens.iter().enumerate() {
        match *tok {
            "Tf" if i >= 2 => {
                if let Some(name) = tokens[i - 2].strip_prefix('/') {
                    out.font = name.as_bytes().to_vec();
                }
                out.size = num(i - 1).unwrap_or(0.0).max(0.0);
            }
            "g" if i >= 1 => {
                let g = num(i - 1).unwrap_or(0.0);
                out.color = [g; 3];
            }
            "rg" if i >= 3 => out.color = [num(i - 3), num(i - 2), num(i - 1)].map(|v| v.unwrap_or(0.0)),
            "k" if i >= 4 => {
                let [c, m, y, k] = [num(i - 4), num(i - 3), num(i - 2), num(i - 1)].map(|v| v.unwrap_or(0.0));
                out.color = [(1.0 - c) * (1.0 - k), (1.0 - m) * (1.0 - k), (1.0 - y) * (1.0 - k)];
            }
            _ => {}
        }
    }
    out
}

/// Fillable widget kinds, for viewers that draw editable fields themselves.
#[cfg_attr(not(feature = "render"), allow(dead_code))]
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum Fillable {
    /// Text and choice fields.
    Text,
    /// Check boxes and radio buttons.
    Button,
}

/// Whether an annotation is a field the user fills in (text, choice, check
/// box or radio button), as opposed to push buttons, signatures and other
/// annotations.
#[cfg_attr(not(feature = "render"), allow(dead_code))]
pub(crate) fn fillable(doc: &Document, annot: &Dictionary) -> Option<Fillable> {
    if annot.get(b"Subtype").and_then(Object::as_name) != Some(b"Widget") {
        return None;
    }
    let (mut ft, mut ff) = (None, None);
    let mut node = annot.clone();
    for _ in 0..MAX_DEPTH {
        if ft.is_none() {
            ft = doc.get_resolved(&node, b"FT").ok().and_then(|o| o.as_name().map(<[u8]>::to_vec));
        }
        if ff.is_none() {
            ff = doc.get_resolved(&node, b"Ff").ok().and_then(|o| o.as_i64());
        }
        match doc.get_dict(&node, b"Parent") {
            Some(parent) if ft.is_none() || ff.is_none() => node = parent,
            _ => break,
        }
    }
    match ft.as_deref() {
        Some(b"Tx") | Some(b"Ch") => Some(Fillable::Text),
        Some(b"Btn") if ff.unwrap_or(0) & PUSHBUTTON == 0 => Some(Fillable::Button),
        _ => None,
    }
}

// ----------------------------------------------------------------------
// Filling
// ----------------------------------------------------------------------

/// Prepares the changes that set `values` (field id, new value) in `doc`.
/// `fields` is what [`form_fields`] returned for the same document. Pass
/// the result to [`crate::write_document_with_changes`].
pub fn fill_form(doc: &Document, fields: &[FormField], values: &[(ObjRef, FieldValue)]) -> Result<Changes> {
    let catalog = doc.catalog()?;
    let acroform = doc.get_dict(&catalog, b"AcroForm").ok_or_else(|| Error::Object("the document has no form".into()))?;
    let resources_font = doc
        .get_dict(&acroform, b"DR")
        .and_then(|dr| doc.get_dict(&dr, b"Font"))
        .unwrap_or_default();
    let mut next = doc.max_object_number() + 1;
    let mut changes = Changes::default();
    // Objects being modified, loaded once (a field and its widget are
    // often the same object).
    let mut edited: HashMap<u32, (ObjRef, Dictionary)> = HashMap::new();
    let load = |r: ObjRef, edited: &mut HashMap<u32, (ObjRef, Dictionary)>| -> Option<()> {
        if let std::collections::hash_map::Entry::Vacant(e) = edited.entry(r.num) {
            let dict = doc.get_object(r).ok()?.as_dict()?.clone();
            e.insert((r, dict));
        }
        Some(())
    };

    for (id, value) in values {
        let Some(field) = fields.iter().find(|f| f.id == *id) else { continue };
        if load(field.id, &mut edited).is_none() {
            continue;
        }
        let v = match (&field.kind, value) {
            (FieldKind::Text { .. }, FieldValue::Text(s)) => Object::String(encode_text_string(s)),
            (FieldKind::CheckBox | FieldKind::Radio, FieldValue::Button(state)) => {
                Object::Name(state.clone().unwrap_or_else(|| "Off".into()).into_bytes())
            }
            (FieldKind::ListBox { multi_select: true, .. }, FieldValue::Choice(sel)) if sel.len() > 1 => {
                Object::Array(sel.iter().map(|s| Object::String(encode_text_string(s))).collect())
            }
            (FieldKind::ComboBox { .. } | FieldKind::ListBox { .. }, FieldValue::Choice(sel)) => {
                sel.first().map_or(Object::Null, |s| Object::String(encode_text_string(s)))
            }
            _ => continue,
        };
        edited.get_mut(&field.id.num).unwrap().1.set(b"V".to_vec(), v);

        for w in &field.widgets {
            if load(w.id, &mut edited).is_none() {
                continue;
            }
            let wdict = &mut edited.get_mut(&w.id.num).unwrap().1;
            match value {
                FieldValue::Button(state) => {
                    let on = state.is_some() && *state == w.on_state;
                    let name = if on { w.on_state.clone().unwrap_or_default() } else { "Off".into() };
                    wdict.set(b"AS".to_vec(), Object::Name(name.into_bytes()));
                }
                FieldValue::Text(_) | FieldValue::Choice(_) => {
                    let wdict = wdict.clone();
                    let Some(stream) = appearance(doc, field, value, &wdict, &resources_font) else { continue };
                    let r = ObjRef::new(next, 0);
                    next += 1;
                    changes.added.push((r, Object::Stream(stream)));
                    let mut ap = Dictionary::new();
                    ap.set(b"N".to_vec(), Object::Reference(r));
                    edited.get_mut(&w.id.num).unwrap().1.set(b"AP".to_vec(), Object::Dictionary(ap));
                }
                FieldValue::None => {}
            }
        }
    }

    // XFA data would take precedence over the new values in some viewers.
    if acroform.contains(b"XFA")
        && let Some(r) = catalog.get(b"AcroForm").and_then(Object::as_reference)
    {
        let mut af = acroform.clone();
        af.remove(b"XFA");
        changes.replace.insert(r.num, (r, Object::Dictionary(af)));
    }
    for (num, (r, dict)) in edited {
        changes.replace.insert(num, (r, Object::Dictionary(dict)));
    }
    Ok(changes)
}

/// PDF text string: PDFDocEncoding when possible, UTF-16BE otherwise.
pub(crate) fn encode_text_string(s: &str) -> Vec<u8> {
    let reverse: HashMap<char, u8> = (0..=255u8).filter_map(|b| Some((BaseEncoding::PdfDoc.decode(b)?, b))).collect();
    if let Some(bytes) = s.chars().map(|c| reverse.get(&c).copied()).collect::<Option<Vec<u8>>>() {
        return bytes;
    }
    let mut out = vec![0xFE, 0xFF];
    for unit in s.encode_utf16() {
        out.extend_from_slice(&unit.to_be_bytes());
    }
    out
}

/// A new normal appearance for a text or choice field's widget.
pub(crate) fn appearance(doc: &Document, field: &FormField, value: &FieldValue, widget: &Dictionary, fonts: &Dictionary) -> Option<Stream> {
    let [x0, y0, x1, y1] = doc.rect(widget.get(b"Rect"))?;
    let rotation = doc
        .get_dict(widget, b"MK")
        .and_then(|mk| doc.get_resolved(&mk, b"R").ok()?.as_i64())
        .unwrap_or(0)
        .rem_euclid(360);
    let (rw, rh) = ((x1 - x0) as f32, (y1 - y0) as f32);
    // The appearance is drawn upright in its own box, then rotated onto
    // the widget.
    let (w, h, matrix) = match rotation {
        90 => (rh, rw, [0.0, 1.0, -1.0, 0.0, rw, 0.0]),
        180 => (rw, rh, [-1.0, 0.0, 0.0, -1.0, rw, rh]),
        270 => (rh, rw, [0.0, -1.0, 1.0, 0.0, 0.0, rh]),
        _ => (rw, rh, [1.0, 0.0, 0.0, 1.0, 0.0, 0.0]),
    };

    let da_bytes = doc
        .get_resolved(widget, b"DA")
        .ok()
        .and_then(|o| o.as_string().map(<[u8]>::to_vec));
    let da = match da_bytes {
        Some(d) => parse_da(&d),
        None => DefaultAppearance { font: b"Helv".to_vec(), size: field.font_size, color: field.text_color },
    };
    // The font: from the form's resources, or plain Helvetica.
    let font_obj = fonts.get(&da.font).cloned().unwrap_or_else(|| {
        let mut f = Dictionary::new();
        f.set(b"Type".to_vec(), Object::Name(b"Font".to_vec()));
        f.set(b"Subtype".to_vec(), Object::Name(b"Type1".to_vec()));
        f.set(b"BaseFont".to_vec(), Object::Name(b"Helvetica".to_vec()));
        f.set(b"Encoding".to_vec(), Object::Name(b"WinAnsiEncoding".to_vec()));
        Object::Dictionary(f)
    });
    let font = doc.resolve(&font_obj).ok().and_then(|o| o.as_dict().cloned()).map(|d| Font::load(doc, &d));
    let encoder = Encoder::new(font.as_ref());

    let mk = doc.get_dict(widget, b"MK");
    let color = |key: &[u8]| mk.as_ref().and_then(|mk| color_of(doc.get_resolved(mk, key).ok()?.as_array()?));
    let border_width = doc
        .get_dict(widget, b"BS")
        .and_then(|bs| doc.get_resolved(&bs, b"W").ok()?.as_f64())
        .unwrap_or(1.0) as f32;

    let mut ops = String::new();
    // Background and border, as the widget's /MK asks.
    if let Some([r, g, b]) = color(b"BG") {
        ops += &format!("{} {} {} rg 0 0 {} {} re f\n", n(r), n(g), n(b), n(w), n(h));
    }
    let border = color(b"BC").map(|_| border_width).unwrap_or(0.0);
    if let Some([r, g, b]) = color(b"BC")
        && border > 0.0
    {
        ops += &format!(
            "{} {} {} RG {} w {} {} {} {} re S\n",
            n(r),
            n(g),
            n(b),
            n(border),
            n(border / 2.0),
            n(border / 2.0),
            n(w - border),
            n(h - border)
        );
    }

    let inset = border.max(1.0) + 1.0;
    let [cr, cg, cb] = da.color;
    let fname = String::from_utf8_lossy(&da.font).into_owned();
    ops += "/Tx BMC\nq\n";
    ops += &format!("{} {} {} {} re W n\n", n(border), n(border), n(w - 2.0 * border), n(h - 2.0 * border));

    let shown: Vec<String> = match (value, &field.kind) {
        (FieldValue::Text(s), FieldKind::Text { password: true, .. }) => vec!["*".repeat(s.chars().count())],
        (FieldValue::Text(s), _) => vec![s.clone()],
        (FieldValue::Choice(sel), FieldKind::ComboBox { options, .. }) => {
            sel.first().map(|v| label_of(options, v)).into_iter().collect()
        }
        (FieldValue::Choice(_), FieldKind::ListBox { options, .. }) => options.iter().map(|o| o.label.clone()).collect(),
        _ => Vec::new(),
    };
    let width_of = |s: &str, size: f32| encoder.width(s) * size;
    let avail = (w - 2.0 * inset).max(1.0);

    match &field.kind {
        FieldKind::ListBox { options, .. } => {
            // Every option on its own line, the selected ones highlighted.
            let size = if da.size > 0.0 { da.size } else { 12.0 };
            let line = size * 1.15;
            let selected = match value {
                FieldValue::Choice(sel) => sel.clone(),
                _ => Vec::new(),
            };
            let mut y = h - inset - line;
            for (i, opt) in options.iter().enumerate() {
                if selected.contains(&opt.value) {
                    ops += &format!("0.6 0.75 0.86 rg {} {} {} {} re f\n", n(border), n(y - size * 0.25), n(w - 2.0 * border), n(line));
                }
                ops += &format!("BT /{fname} {} Tf {} {} {} rg 1 0 0 1 {} {} Tm {} Tj ET\n", n(size), n(cr), n(cg), n(cb), n(inset), n(y), encoder.string(&shown[i]));
                y -= line;
            }
        }
        FieldKind::Text { multiline: true, .. } => {
            let size = if da.size > 0.0 { da.size } else { 10.0 };
            let line = size * 1.15;
            let mut y = h - inset - size * 0.9;
            for l in wrap(shown.first().map_or("", String::as_str), avail, |s| width_of(s, size)) {
                let x = align(field.alignment, inset, avail, width_of(&l, size));
                ops += &format!("BT /{fname} {} Tf {} {} {} rg 1 0 0 1 {} {} Tm {} Tj ET\n", n(size), n(cr), n(cg), n(cb), n(x), n(y), encoder.string(&l));
                y -= line;
            }
        }
        FieldKind::Text { comb: true, max_len: Some(cells), .. } => {
            let text = shown.first().cloned().unwrap_or_default();
            let size = if da.size > 0.0 { da.size } else { ((h - 2.0 * inset) / 1.15).clamp(4.0, 12.0) };
            let cell = w / *cells as f32;
            let y = (h - size * 0.72) / 2.0;
            for (i, ch) in text.chars().take(*cells).enumerate() {
                let s = ch.to_string();
                let x = cell * i as f32 + (cell - width_of(&s, size)) / 2.0;
                ops += &format!("BT /{fname} {} Tf {} {} {} rg 1 0 0 1 {} {} Tm {} Tj ET\n", n(size), n(cr), n(cg), n(cb), n(x), n(y), encoder.string(&s));
            }
        }
        _ => {
            let text = shown.first().cloned().unwrap_or_default();
            let mut size = if da.size > 0.0 { da.size } else { ((h - 2.0 * inset) / 1.15).clamp(4.0, 12.0) };
            if da.size <= 0.0 {
                // Auto size also shrinks long text to fit the width.
                let tw = width_of(&text, size);
                if tw > avail {
                    size = (size * avail / tw).max(4.0);
                }
            }
            let x = align(field.alignment, inset, avail, width_of(&text, size));
            // Centered vertically on the cap height.
            let y = (h - size * 0.72) / 2.0;
            ops += &format!("BT /{fname} {} Tf {} {} {} rg 1 0 0 1 {} {} Tm {} Tj ET\n", n(size), n(cr), n(cg), n(cb), n(x), n(y), encoder.string(&text));
        }
    }
    ops += "Q\nEMC\n";

    let mut font_res = Dictionary::new();
    font_res.set(da.font.clone(), font_obj);
    let mut resources = Dictionary::new();
    resources.set(b"Font".to_vec(), Object::Dictionary(font_res));
    let mut dict = Dictionary::new();
    dict.set(b"Type".to_vec(), Object::Name(b"XObject".to_vec()));
    dict.set(b"Subtype".to_vec(), Object::Name(b"Form".to_vec()));
    dict.set(b"BBox".to_vec(), Object::Array([0.0, 0.0, w, h].map(|v| Object::Real(v as f64)).to_vec()));
    if rotation != 0 {
        dict.set(b"Matrix".to_vec(), Object::Array(matrix.map(|v: f32| Object::Real(v as f64)).to_vec()));
    }
    dict.set(b"Resources".to_vec(), Object::Dictionary(resources));
    Some(Stream { dict, raw: ops.into_bytes() })
}

fn label_of(options: &[ChoiceOption], value: &str) -> String {
    options.iter().find(|o| o.value == value).map_or_else(|| value.to_string(), |o| o.label.clone())
}

fn align(q: u8, inset: f32, avail: f32, width: f32) -> f32 {
    match q {
        1 => inset + (avail - width) / 2.0,
        2 => inset + avail - width,
        _ => inset,
    }
}

/// A number for a content stream: short, never in exponent notation.
fn n(v: f32) -> String {
    let s = format!("{:.3}", v);
    let s = s.trim_end_matches('0').trim_end_matches('.');
    if s == "-0" { "0".into() } else { s.into() }
}

/// Breaks text into lines no wider than `max`, at spaces where possible;
/// explicit line breaks are kept.
fn wrap(text: &str, max: f32, width: impl Fn(&str) -> f32) -> Vec<String> {
    let mut lines = Vec::new();
    for para in text.split('\n') {
        let mut line = String::new();
        for word in para.split(' ') {
            let candidate = if line.is_empty() { word.to_string() } else { format!("{line} {word}") };
            if width(&candidate) <= max || line.is_empty() {
                line = candidate;
            } else {
                lines.push(std::mem::take(&mut line));
                line = word.to_string();
            }
            // A single word wider than the field is broken by characters.
            while width(&line) > max && line.chars().count() > 1 {
                let mut cut = line.len();
                while cut > 0 && width(&line[..cut]) > max {
                    cut = line[..cut].char_indices().last().map_or(0, |(i, _)| i);
                }
                let cut = cut.max(line.chars().next().map_or(1, char::len_utf8));
                lines.push(line[..cut].to_string());
                line = line[cut..].to_string();
            }
        }
        lines.push(line);
    }
    lines
}

/// Turns text into the bytes of a simple font, and measures it.
struct Encoder<'a> {
    font: Option<&'a Font>,
    codes: HashMap<char, u8>,
}

impl<'a> Encoder<'a> {
    fn new(font: Option<&'a Font>) -> Self {
        // The font's own character mapping, falling back to WinAnsi.
        let mut codes: HashMap<char, u8> = HashMap::new();
        for b in (0..=255u8).rev() {
            if let Some(c) = BaseEncoding::WinAnsi.decode(b) {
                codes.insert(c, b);
            }
        }
        if let Some(f) = font.filter(|f| !f.composite) {
            for b in (0..=255u8).rev() {
                if let Some(mut chars) = f.to_text(b as u32).map(str::chars)
                    && let (Some(c), None) = (chars.next(), chars.next())
                {
                    codes.insert(c, b);
                }
            }
        }
        Encoder { font, codes }
    }

    fn bytes(&self, s: &str) -> Vec<u8> {
        s.chars().map(|c| self.codes.get(&c).copied().unwrap_or(b'?')).collect()
    }

    /// Width in text space per unit of font size.
    fn width(&self, s: &str) -> f32 {
        match self.font {
            Some(f) if !f.composite => self.bytes(s).iter().map(|&b| f.width(b as u32) as f32).sum(),
            _ => s.chars().count() as f32 * 0.5,
        }
    }

    /// A PDF string operand.
    fn string(&self, s: &str) -> String {
        Object::String(self.bytes(s)).to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_appearance_strings() {
        let da = parse_da(b"/Helv 0 Tf 0 g");
        assert_eq!((da.font.as_slice(), da.size, da.color), (&b"Helv"[..], 0.0, [0.0; 3]));
        let da = parse_da(b"0.2 0.4 0.6 rg /TiRo 11.5 Tf");
        assert_eq!((da.font.as_slice(), da.size, da.color), (&b"TiRo"[..], 11.5, [0.2, 0.4, 0.6]));
    }

    #[test]
    fn wrapping() {
        let w = |s: &str| s.chars().count() as f32;
        assert_eq!(wrap("aa bb cc", 5.0, w), ["aa bb", "cc"]);
        assert_eq!(wrap("abcdefgh", 3.0, w), ["abc", "def", "gh"]);
        assert_eq!(wrap("a\nb", 10.0, w), ["a", "b"]);
    }

    #[test]
    fn numbers_and_text_strings() {
        assert_eq!(n(1.0), "1");
        assert_eq!(n(0.5), "0.5");
        assert_eq!(n(-0.0001), "0");
        assert_eq!(encode_text_string("Café"), b"Caf\xe9".to_vec());
        assert_eq!(encode_text_string("→"), vec![0xFE, 0xFF, 0x21, 0x92]);
    }
}
