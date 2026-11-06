//! Hyperlinks: `/Link` annotations and where they point.
//!
//! Handles URI actions, go-to actions and `/Dest` entries (explicit
//! destination arrays and named destinations, from either the catalog's
//! `/Dests` dictionary or the `/Names` → `/Dests` name tree), and the
//! standard named actions (next/previous/first/last page). Links that would
//! launch programs or open other files are ignored.

use std::collections::HashMap;

use crate::document::{Document, Page};
use crate::encoding::decode_text_string;
use crate::object::{Dictionary, ObjRef, Object};

/// A clickable area on a page.
#[derive(Debug, Clone, PartialEq)]
pub struct Link {
    /// `[left, top, right, bottom]` in display points, like
    /// [`crate::TextChar::rect`].
    pub rect: [f32; 4],
    pub target: LinkTarget,
}

#[derive(Debug, Clone, PartialEq)]
pub enum LinkTarget {
    /// A web address (or other URI) to open outside the viewer.
    Uri(String),
    /// A place in this document: page index, and the spot to scroll to in
    /// display points on that page (`None` = the top of the page).
    Page { page: usize, top: Option<f32> },
}

/// Limits for walking name trees in damaged or hostile files.
const MAX_DEPTH: usize = 32;

/// Finds the links on `pages[index]`.
pub fn page_links(doc: &Document, pages: &[Page], index: usize) -> Vec<Link> {
    let Some(page) = pages.get(index) else { return Vec::new() };
    let Ok(Object::Array(annots)) = doc.get_resolved(&page.dict, b"Annots") else { return Vec::new() };
    let mut resolver = Resolver { doc, pages, page_index: None, index };
    let m = page.display_matrix(doc);
    let mut out = Vec::new();
    for annot in &annots {
        let Some(dict) = doc.resolve(annot).ok().and_then(|o| o.as_dict().cloned()) else { continue };
        if dict.get(b"Subtype").and_then(Object::as_name) != Some(b"Link") {
            continue;
        }
        let flags = doc.get_resolved(&dict, b"F").ok().and_then(|o| o.as_i64()).unwrap_or(0);
        if flags & 2 != 0 {
            continue; // Hidden
        }
        let Some([x0, y0, x1, y1]) = doc.rect(dict.get(b"Rect")) else { continue };
        let Some(target) = resolver.link_target(&dict) else { continue };
        let corners = [(x0, y0), (x1, y0), (x0, y1), (x1, y1)].map(|(x, y)| apply(&m, x, y));
        let (mut l, mut t, mut r, mut b) = (f64::MAX, f64::MAX, f64::MIN, f64::MIN);
        for (x, y) in corners {
            (l, t, r, b) = (l.min(x), t.min(y), r.max(x), b.max(y));
        }
        out.push(Link { rect: [l as f32, t as f32, r as f32, b as f32], target });
    }
    out
}

fn apply(m: &[f64; 6], x: f64, y: f64) -> (f64, f64) {
    (m[0] * x + m[2] * y + m[4], m[1] * x + m[3] * y + m[5])
}

struct Resolver<'a> {
    doc: &'a Document,
    pages: &'a [Page],
    /// Page object → index, built on first use.
    page_index: Option<HashMap<ObjRef, usize>>,
    /// The page the link is on.
    index: usize,
}

impl Resolver<'_> {
    fn link_target(&mut self, annot: &Dictionary) -> Option<LinkTarget> {
        if let Some(action) = self.doc.get_dict(annot, b"A") {
            return self.action(&action);
        }
        let dest = self.doc.get_resolved(annot, b"Dest").ok()?;
        self.destination(&dest, 0)
    }

    fn action(&mut self, action: &Dictionary) -> Option<LinkTarget> {
        let doc = self.doc;
        let kind = doc.get_resolved(action, b"S").ok()?;
        match kind.as_name()? {
            b"URI" => {
                let uri = doc.get_resolved(action, b"URI").ok()?;
                let uri = String::from_utf8_lossy(uri.as_string()?).trim().to_string();
                Some(LinkTarget::Uri(self.with_base(uri)))
            }
            b"GoTo" => {
                let dest = doc.get_resolved(action, b"D").ok()?;
                self.destination(&dest, 0)
            }
            b"Named" => {
                let last = self.pages.len().checked_sub(1)?;
                let page = match doc.get_resolved(action, b"N").ok()?.as_name()? {
                    b"NextPage" => (self.index + 1).min(last),
                    b"PrevPage" => self.index.saturating_sub(1),
                    b"FirstPage" => 0,
                    b"LastPage" => last,
                    _ => return None,
                };
                Some(LinkTarget::Page { page, top: None })
            }
            _ => None,
        }
    }

    /// Relative URIs are resolved against the catalog's `/URI /Base`.
    fn with_base(&self, uri: String) -> String {
        if uri.contains(':') {
            return uri;
        }
        let base = self
            .doc
            .catalog()
            .ok()
            .and_then(|c| self.doc.get_dict(&c, b"URI"))
            .and_then(|u| self.doc.get_resolved(&u, b"Base").ok())
            .and_then(|b| b.as_string().map(|s| String::from_utf8_lossy(s).into_owned()));
        match base {
            Some(base) => format!("{base}{uri}"),
            None => uri,
        }
    }

    fn destination(&mut self, dest: &Object, depth: usize) -> Option<LinkTarget> {
        if depth > 4 {
            return None;
        }
        match dest {
            Object::Array(items) => self.explicit(items),
            Object::Name(name) => {
                let found = self.named_in_dests(name)?;
                self.destination(&found, depth + 1)
            }
            Object::String(name) => {
                let found = self.named_in_tree(name).or_else(|| self.named_in_dests(name))?;
                self.destination(&found, depth + 1)
            }
            // A dictionary with the destination under /D.
            Object::Dictionary(d) => {
                let inner = self.doc.get_resolved(d, b"D").ok()?;
                self.destination(&inner, depth + 1)
            }
            _ => None,
        }
    }

    /// `[page /XYZ left top zoom]`, `[page /FitH top]`, `[page /FitR l b r t]`, …
    fn explicit(&mut self, items: &[Object]) -> Option<LinkTarget> {
        let page = match items.first()? {
            Object::Reference(r) => {
                let pages = self.pages;
                *self.page_index.get_or_insert_with(|| pages.iter().enumerate().map(|(i, p)| (p.id, i)).collect()).get(r)?
            }
            // Page numbers appear in destinations meant for other files,
            // and some producers use them here too.
            other => usize::try_from(other.as_i64()?).ok()?,
        };
        if page >= self.pages.len() {
            return None;
        }
        let num = |i: usize| items.get(i).and_then(|o| self.doc.resolve(o).ok()).and_then(|o| o.as_f64());
        let kind = items.get(1).and_then(Object::as_name).unwrap_or(b"Fit");
        let point = match kind {
            b"XYZ" => num(3).map(|t| (num(2), t)),
            b"FitH" | b"FitBH" => num(2).map(|t| (None, t)),
            b"FitR" => num(5).map(|t| (num(2), t)),
            _ => None,
        };
        let top = point.map(|(left, top)| {
            let target = &self.pages[page];
            let m = target.display_matrix(self.doc);
            let left = left.unwrap_or(target.crop_box(self.doc)[0]);
            let (_, y) = apply(&m, left, top);
            y.max(0.0) as f32
        });
        Some(LinkTarget::Page { page, top })
    }

    /// PDF 1.1 style: `/Dests` dictionary in the catalog.
    fn named_in_dests(&self, name: &[u8]) -> Option<Object> {
        let catalog = self.doc.catalog().ok()?;
        let dests = self.doc.get_dict(&catalog, b"Dests")?;
        let value = dests.get(name)?;
        self.doc.resolve(value).ok()
    }

    /// PDF 1.2+: the `/Dests` name tree under the catalog's `/Names`.
    fn named_in_tree(&self, name: &[u8]) -> Option<Object> {
        let catalog = self.doc.catalog().ok()?;
        let names = self.doc.get_dict(&catalog, b"Names")?;
        let root = self.doc.get_dict(&names, b"Dests")?;
        self.search_tree(&root, name, 0)
    }

    fn search_tree(&self, node: &Dictionary, name: &[u8], depth: usize) -> Option<Object> {
        if depth > MAX_DEPTH {
            return None;
        }
        let doc = self.doc;
        if let Ok(Object::Array(limits)) = doc.get_resolved(node, b"Limits")
            && let [lo, hi] = &limits[..]
            && let (Some(lo), Some(hi)) = (doc.resolve(lo).ok(), doc.resolve(hi).ok())
            && let (Some(lo), Some(hi)) = (lo.as_string(), hi.as_string())
            && (name < lo || name > hi)
        {
            return None;
        }
        if let Ok(Object::Array(pairs)) = doc.get_resolved(node, b"Names") {
            for pair in pairs.chunks(2) {
                let [key, value] = pair else { break };
                let key = doc.resolve(key).ok()?;
                // Compare as bytes, and also as text in case one side is
                // UTF-16 encoded.
                if let Some(k) = key.as_string()
                    && (k == name || decode_text_string(k) == decode_text_string(name))
                {
                    return doc.resolve(value).ok();
                }
            }
        }
        if let Ok(Object::Array(kids)) = doc.get_resolved(node, b"Kids") {
            for kid in &kids {
                if let Some(kid) = doc.resolve(kid).ok().and_then(|k| k.as_dict().cloned())
                    && let Some(found) = self.search_tree(&kid, name, depth + 1)
                {
                    return Some(found);
                }
            }
        }
        None
    }
}
