//! The `Document`: entry point for reading a PDF.
//!
//! Memory use is kept low by:
//! - memory-mapping the file instead of reading it into RAM (the OS pages
//!   in only what is touched);
//! - parsing objects lazily, only when requested, with no global cache;
//! - keeping only the few most recently used decoded object streams in
//!   memory.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
#[cfg(not(windows))]
use std::fs::File;
use std::ops::Deref;
use std::path::Path;

use crate::encoding::decode_text_string;
use crate::error::{Error, Result};
use crate::filters;
use crate::object::{Dictionary, ObjRef, Object, Stream};
use crate::parser::{find, Lexer, Parser, Token};
use crate::xref::{self, XrefEntry, XrefTable};

/// Guards against reference cycles (`1 0 obj 1 0 R endobj`) and runaway
/// recursion while resolving.
const MAX_RESOLVE_DEPTH: u32 = 32;

enum Source {
    #[cfg(not(windows))]
    Mapped(memmap2::Mmap),
    Owned(Vec<u8>),
    /// A file read into memory, shared by every document open on it.
    #[cfg(windows)]
    Shared(std::sync::Arc<[u8]>),
}

impl Deref for Source {
    type Target = [u8];
    fn deref(&self) -> &[u8] {
        match self {
            #[cfg(not(windows))]
            Source::Mapped(m) => m,
            Source::Owned(v) => v,
            #[cfg(windows)]
            Source::Shared(d) => d,
        }
    }
}

/// Decoded object streams kept in memory. Consecutive lookups usually hit
/// the same one, but walks such as the outline go back and forth between a
/// few (the outline's, the destinations', the pages').
const OBJSTM_CACHE: usize = 4;

/// A decoded object stream (PDF 1.5+), kept around because consecutive
/// lookups usually hit the same one.
struct ObjStmCache {
    num: u32,
    data: Vec<u8>,
    /// (object number, absolute offset into `data`)
    members: Vec<(u32, usize)>,
}

/// A page, with inheritable attributes (`Resources`, `MediaBox`, `CropBox`,
/// `Rotate`) already copied down from its ancestors.
#[derive(Debug, Clone)]
pub struct Page {
    pub id: ObjRef,
    pub dict: Dictionary,
}

impl Page {
    /// Page boundaries `[llx lly urx ury]`, defaulting to US Letter.
    pub fn media_box(&self, doc: &Document) -> [f64; 4] {
        doc.rect(self.dict.get(b"MediaBox")).unwrap_or([0.0, 0.0, 612.0, 792.0])
    }

    /// The visible region: `/CropBox` clipped to the media box.
    pub fn crop_box(&self, doc: &Document) -> [f64; 4] {
        let media = self.media_box(doc);
        match doc.rect(self.dict.get(b"CropBox")) {
            Some(c) => {
                let r = [c[0].max(media[0]), c[1].max(media[1]), c[2].min(media[2]), c[3].min(media[3])];
                if r[2] - r[0] >= 1.0 && r[3] - r[1] >= 1.0 { r } else { media }
            }
            None => media,
        }
    }

    /// Maps page space to display space: points from the top-left corner of
    /// the page as shown (y down), after applying the crop box and /Rotate.
    pub fn display_matrix(&self, doc: &Document) -> [f64; 6] {
        let [x0, y0, x1, y1] = self.crop_box(doc);
        match self.rotation() {
            90 => [0.0, 1.0, 1.0, 0.0, -y0, -x0],
            180 => [-1.0, 0.0, 0.0, 1.0, x1, -y0],
            270 => [0.0, -1.0, -1.0, 0.0, y1, x1],
            _ => [1.0, 0.0, 0.0, -1.0, -x0, y1],
        }
    }

    /// Displayed size in points (width, height), after rotation.
    pub fn display_size(&self, doc: &Document) -> (f64, f64) {
        let [x0, y0, x1, y1] = self.crop_box(doc);
        let (w, h) = (x1 - x0, y1 - y0);
        if self.rotation() % 180 == 0 { (w, h) } else { (h, w) }
    }

    /// Rotation in degrees, normalised to 0, 90, 180 or 270.
    pub fn rotation(&self) -> i64 {
        let r = self.dict.get(b"Rotate").and_then(Object::as_i64).unwrap_or(0);
        (r.rem_euclid(360) / 90) * 90
    }
}

/// Document information dictionary (`/Info`), decoded to Rust strings.
#[derive(Debug, Default, Clone)]
pub struct Metadata {
    pub title: Option<String>,
    pub author: Option<String>,
    pub subject: Option<String>,
    pub keywords: Option<String>,
    pub creator: Option<String>,
    pub producer: Option<String>,
    pub creation_date: Option<String>,
    pub mod_date: Option<String>,
}

pub struct Document {
    data: Source,
    /// Offset of `%PDF-` (non-zero when junk precedes the header).
    header_offset: usize,
    header_version: String,
    xref: RefCell<XrefTable>,
    reconstructed: Cell<bool>,
    /// Most recently used first.
    objstm: RefCell<Vec<ObjStmCache>>,
    depth: Cell<u32>,
    /// Edited and added objects (not saved yet), seen in place of the
    /// file's own; see [`Document::set_overlay`].
    overlay: RefCell<HashMap<u32, Object>>,
    /// For a password-protected file: its key, to decrypt objects as they
    /// are read; and the encryption dictionary's object number (whose
    /// strings aren't encrypted).
    crypt: Option<Box<crate::crypt::Crypt>>,
    crypt_num: Option<u32>,
}

impl Document {
    /// Opens a PDF file by memory-mapping it.
    ///
    /// On Windows, a file that is mapped can't be replaced (as saving over
    /// an open document does), so there it is read into memory instead,
    /// once: documents open on the same unchanged file share it.
    ///
    /// A password-protected file opens if its user password is empty (it
    /// only restricts what may be done with it); otherwise this fails with
    /// [`Error::PasswordNeeded`]: see [`Document::open_with_password`].
    pub fn open(path: impl AsRef<Path>) -> Result<Document> {
        Self::open_with_password(path, "")
    }

    /// Opens a password-protected PDF with its user or owner password
    /// (unprotected files open whatever it is). Fails with
    /// [`Error::WrongPassword`] when it doesn't open the file.
    pub fn open_with_password(path: impl AsRef<Path>, password: &str) -> Result<Document> {
        #[cfg(windows)]
        let document = Self::from_source(Source::Shared(shared_file(path.as_ref())?), password);
        #[cfg(not(windows))]
        let document = Self::open_mapped(path.as_ref(), password);
        document
    }

    #[cfg(not(windows))]
    fn open_mapped(path: &Path, password: &str) -> Result<Document> {
        let file = File::open(path)?;
        if file.metadata()?.len() == 0 {
            return Err(Error::parse(0, "file is empty"));
        }
        // SAFETY: the mapping is read-only. As with any mmap, modifying the
        // file from another process while it is open is not supported.
        let map = unsafe { memmap2::Mmap::map(&file)? };
        Self::from_source(Source::Mapped(map), password)
    }

    /// Reads a PDF from an in-memory buffer.
    pub fn from_bytes(data: Vec<u8>) -> Result<Document> {
        Self::from_source(Source::Owned(data), "")
    }

    /// [`Document::from_bytes`] for a password-protected PDF.
    pub fn from_bytes_with_password(data: Vec<u8>, password: &str) -> Result<Document> {
        Self::from_source(Source::Owned(data), password)
    }

    fn from_source(data: Source, password: &str) -> Result<Document> {
        // The header should be at byte 0, but readers accept it anywhere in
        // the first 1024 bytes.
        let head = &data[..data.len().min(1024)];
        let (header_offset, header_version) = match find(head, b"%PDF-") {
            Some(off) => {
                let v: String = head[off + 5..]
                    .iter()
                    .take_while(|c| c.is_ascii_digit() || **c == b'.')
                    .map(|&c| c as char)
                    .collect();
                (off, v)
            }
            None => return Err(Error::parse(0, "not a PDF file (no %PDF- header)")),
        };

        let (table, reconstructed) = match xref::load(&data, header_offset) {
            Ok(t) => (t, false),
            Err(_) => (xref::reconstruct(&data)?, true),
        };
        let mut doc = Document {
            data,
            header_offset,
            header_version,
            xref: RefCell::new(table),
            reconstructed: Cell::new(reconstructed),
            objstm: RefCell::new(Vec::new()),
            depth: Cell::new(0),
            overlay: RefCell::new(HashMap::new()),
            crypt: None,
            crypt_num: None,
        };
        doc.unlock(password)?;

        // A readable catalog is the minimum for a usable document. If the
        // xref looked fine but leads nowhere, rebuild it from scratch.
        match doc.catalog() {
            Ok(_) => {}
            Err(_) if !doc.reconstructed.get() => {
                doc.reconstruct()?;
                doc.catalog()?;
            }
            Err(e) => return Err(e),
        }
        Ok(doc)
    }

    /// For an encrypted file: works out its key from `password` (§7.6),
    /// so objects are decrypted as they are read.
    fn unlock(&mut self, password: &str) -> Result<()> {
        let trailer = self.trailer();
        let Some(encrypt) = trailer.get(b"Encrypt").cloned() else { return Ok(()) };
        // The dictionary, with its entries (and its crypt filters')
        // resolved: they may be indirect.
        let mut dict = match self.resolve(&encrypt)? {
            Object::Dictionary(d) => d,
            _ => return Err(Error::Object("the encryption dictionary is missing".into())),
        };
        let resolve_all = |d: &Dictionary| {
            let mut out = Dictionary::new();
            for (k, v) in d.iter() {
                out.set(k.to_vec(), self.resolve(v).unwrap_or(Object::Null));
            }
            out
        };
        dict = resolve_all(&dict);
        if let Some(Object::Dictionary(cf)) = dict.get(b"CF").cloned() {
            let mut filters = Dictionary::new();
            for (k, v) in cf.iter() {
                let f = match self.resolve(v) {
                    Ok(Object::Dictionary(f)) => Object::Dictionary(resolve_all(&f)),
                    other => other.unwrap_or(Object::Null),
                };
                filters.set(k.to_vec(), f);
            }
            dict.set(b"CF".to_vec(), Object::Dictionary(filters));
        }
        let id = match self.get_resolved(&trailer, b"ID") {
            Ok(Object::Array(ids)) => ids.first().and_then(|i| self.resolve(i).ok()).and_then(|i| i.as_string().map(<[u8]>::to_vec)),
            _ => None,
        };
        let crypt = crate::crypt::Crypt::unlock(&dict, id.as_deref().unwrap_or_default(), password)?;
        self.crypt = Some(Box::new(crypt));
        self.crypt_num = encrypt.as_reference().map(|r| r.num);
        Ok(())
    }

    /// How the file is encrypted ("AES-256", "RC4, 128-bit", …), if it is.
    pub fn encryption(&self) -> Option<String> {
        self.crypt.as_ref().map(|c| c.describe())
    }

    /// The file's key and encryption dictionary, to save it encrypted the
    /// same way.
    pub(crate) fn crypt(&self) -> Option<&crate::crypt::Crypt> {
        self.crypt.as_deref()
    }

    /// Raw file bytes.
    pub fn data(&self) -> &[u8] {
        &self.data
    }

    /// PDF version: the header version, or the catalog's `/Version` if it
    /// is newer (PDF 1.4+ allows upgrading the version that way).
    pub fn version(&self) -> String {
        let catalog_version = self
            .catalog()
            .ok()
            .and_then(|c| c.get(b"Version").and_then(|v| v.as_name().map(|n| n.to_vec())))
            .map(|n| String::from_utf8_lossy(&n).into_owned());
        match catalog_version {
            Some(cv) if version_tuple(&cv) > version_tuple(&self.header_version) => cv,
            _ => self.header_version.clone(),
        }
    }

    /// True if the cross-reference table had to be rebuilt by scanning the
    /// file (i.e. the file is damaged).
    pub fn was_repaired(&self) -> bool {
        self.reconstructed.get()
    }

    pub fn is_encrypted(&self) -> bool {
        self.xref.borrow().trailer.contains(b"Encrypt")
    }

    pub fn trailer(&self) -> Dictionary {
        self.xref.borrow().trailer.clone()
    }

    /// Number of objects listed in the cross-reference table.
    pub fn object_count(&self) -> usize {
        self.xref.borrow().entries.len()
    }

    /// Every object in the cross-reference table, in storage order: objects
    /// stored directly by file offset, then those inside object streams
    /// grouped by stream (so each object stream is decoded only once when
    /// they are read in this order).
    pub(crate) fn xref_entries(&self) -> Vec<(u32, XrefEntry)> {
        let mut entries: Vec<(u32, XrefEntry)> = self.xref.borrow().entries.iter().map(|(&n, &e)| (n, e)).collect();
        entries.sort_by_key(|&(num, e)| match e {
            XrefEntry::InFile { offset, .. } => (0, offset as u64, 0, num),
            XrefEntry::InStream { stream, index } => (1, stream as u64, index, num),
        });
        entries
    }

    fn reconstruct(&self) -> Result<()> {
        let table = xref::reconstruct(&self.data)?;
        *self.xref.borrow_mut() = table;
        self.objstm.borrow_mut().clear();
        self.reconstructed.set(true);
        Ok(())
    }

    // ----------------------------------------------------------------
    // Object access
    // ----------------------------------------------------------------

    /// Loads an indirect object. References to missing objects resolve to
    /// `Null`, as the spec requires.
    pub fn get_object(&self, r: ObjRef) -> Result<Object> {
        let depth = self.depth.get();
        if depth >= MAX_RESOLVE_DEPTH {
            return Err(Error::Object(format!("reference chain too deep at {r}")));
        }
        self.depth.set(depth + 1);
        let result = self.get_object_inner(r);
        self.depth.set(depth);
        result
    }

    /// Makes the document read as if `changes` were applied: replaced
    /// objects are returned in place of the file's, and added objects
    /// exist. Pages, text, rendering and saving all see the edited
    /// document. Replaces any earlier overlay.
    pub fn set_overlay(&self, changes: &crate::writer::Changes) {
        let mut map = HashMap::new();
        for (r, obj) in &changes.added {
            map.insert(r.num, obj.clone());
        }
        // A replacement wins, also over a new object it replaces.
        for (num, (_, obj)) in &changes.replace {
            map.insert(*num, obj.clone());
        }
        *self.overlay.borrow_mut() = map;
    }

    /// The highest object number in use (in the file or the overlay); new
    /// objects are numbered above it.
    pub fn max_object_number(&self) -> u32 {
        let file = self.xref.borrow().entries.keys().copied().max().unwrap_or(0);
        let overlay = self.overlay.borrow().keys().copied().max().unwrap_or(0);
        file.max(overlay)
    }

    fn get_object_inner(&self, r: ObjRef) -> Result<Object> {
        if let Some(obj) = self.overlay.borrow().get(&r.num) {
            return Ok(obj.clone());
        }
        let entry = self.xref.borrow().get(r.num);
        let result = match entry {
            None => return Ok(Object::Null),
            Some(XrefEntry::InFile { offset, .. }) => self.load_in_file(r.num, offset),
            Some(XrefEntry::InStream { stream, index }) => self.load_from_objstm(r.num, stream, index),
        };
        match result {
            Ok(o) => Ok(o),
            // A bad offset usually means a damaged xref: rebuild once, retry.
            Err(_) if !self.reconstructed.get() => {
                self.reconstruct()?;
                self.get_object_inner(r)
            }
            Err(e) => Err(e),
        }
    }

    fn load_in_file(&self, num: u32, offset: usize) -> Result<Object> {
        let resolve_length = |lr: ObjRef| self.get_object(lr).ok()?.as_i64();
        let mut last_err = None;
        for off in [offset, offset + self.header_offset] {
            if off >= self.data.len() {
                continue;
            }
            match Parser::new(&self.data, off).parse_indirect(&resolve_length) {
                Ok((id, mut obj)) if id.num == num => {
                    // Objects in object streams were decrypted with
                    // their stream.
                    if let Some(crypt) = &self.crypt
                        && self.crypt_num != Some(num)
                    {
                        crypt.decrypt_object(num, id.generation, &mut obj);
                    }
                    return Ok(obj);
                }
                Ok((id, _)) => {
                    last_err = Some(Error::Object(format!("expected object {num} at offset {off}, found {}", id.num)))
                }
                Err(e) => last_err = Some(e),
            }
            if self.header_offset == 0 {
                break;
            }
        }
        Err(last_err.unwrap_or_else(|| Error::Object(format!("offset {offset} of object {num} is out of range"))))
    }

    fn load_from_objstm(&self, num: u32, stream: u32, index: u32) -> Result<Object> {
        let cached = self.objstm.borrow().iter().position(|c| c.num == stream);
        match cached {
            Some(0) => {}
            Some(i) => {
                let mut caches = self.objstm.borrow_mut();
                let cache = caches.remove(i);
                caches.insert(0, cache);
            }
            None => {
                let cache = self.load_objstm(stream)?;
                let mut caches = self.objstm.borrow_mut();
                caches.truncate(OBJSTM_CACHE - 1);
                caches.insert(0, cache);
            }
        }
        let caches = self.objstm.borrow();
        let cache = &caches[0];
        // Trust `index` if it agrees with the stream's header; otherwise
        // search by object number.
        let offset = match cache.members.get(index as usize) {
            Some(&(n, off)) if n == num => off,
            _ => cache
                .members
                .iter()
                .find(|(n, _)| *n == num)
                .map(|&(_, off)| off)
                .ok_or_else(|| Error::Object(format!("object {num} not found in object stream {stream}")))?,
        };
        Parser::new(&cache.data, offset).parse_object()
    }

    fn load_objstm(&self, num: u32) -> Result<ObjStmCache> {
        let obj = self.get_object(ObjRef::new(num, 0))?;
        let stream = obj
            .as_stream()
            .ok_or_else(|| Error::Object(format!("object stream {num} is not a stream")))?;
        let n = self.resolve_dict_int(&stream.dict, b"N").unwrap_or(0).max(0);
        let first = self.resolve_dict_int(&stream.dict, b"First").unwrap_or(0).max(0) as usize;
        let data = self.decode_stream(stream)?;

        let mut members = Vec::with_capacity(n.min(10_000) as usize);
        let mut lex = Lexer::new(&data, 0);
        for _ in 0..n {
            match (lex.next_token(), lex.next_token()) {
                (Some(Token::Integer(obj)), Some(Token::Integer(off))) if obj >= 0 && off >= 0 => {
                    members.push((obj as u32, first + off as usize));
                }
                _ => break,
            }
        }
        Ok(ObjStmCache { num, data, members })
    }

    /// Follows references until a direct object is reached.
    pub fn resolve(&self, obj: &Object) -> Result<Object> {
        match obj {
            Object::Reference(r) => {
                let mut current = self.get_object(*r)?;
                let mut hops = 0;
                while let Object::Reference(next) = current {
                    hops += 1;
                    if hops > MAX_RESOLVE_DEPTH {
                        return Err(Error::Object(format!("reference loop at {next}")));
                    }
                    current = self.get_object(next)?;
                }
                Ok(current)
            }
            other => Ok(other.clone()),
        }
    }

    /// `dict[key]`, resolved. Missing keys give `Null`.
    pub fn get_resolved(&self, dict: &Dictionary, key: &[u8]) -> Result<Object> {
        match dict.get(key) {
            Some(o) => self.resolve(o),
            None => Ok(Object::Null),
        }
    }

    /// `dict[key]` resolved to a dictionary (or a stream's dictionary).
    pub fn get_dict(&self, dict: &Dictionary, key: &[u8]) -> Option<Dictionary> {
        match self.get_resolved(dict, key).ok()? {
            Object::Dictionary(d) => Some(d),
            Object::Stream(s) => Some(s.dict),
            _ => None,
        }
    }

    fn resolve_dict_int(&self, dict: &Dictionary, key: &[u8]) -> Option<i64> {
        self.get_resolved(dict, key).ok()?.as_i64()
    }

    pub(crate) fn rect(&self, obj: Option<&Object>) -> Option<[f64; 4]> {
        let arr = self.resolve(obj?).ok()?;
        let vals: Vec<f64> = arr
            .as_array()?
            .iter()
            .map(|o| self.resolve(o).ok().and_then(|v| v.as_f64()))
            .collect::<Option<_>>()?;
        let [a, b, c, d] = vals[..] else { return None };
        // Normalise so the box is [left bottom right top].
        Some([a.min(c), b.min(d), a.max(c), b.max(d)])
    }

    // ----------------------------------------------------------------
    // Streams
    // ----------------------------------------------------------------

    /// Decodes a stream's data, applying all non-image filters.
    pub fn decode_stream(&self, stream: &Stream) -> Result<Vec<u8>> {
        Ok(self.decode_stream_full(stream)?.data)
    }

    /// Like [`decode_stream`](Self::decode_stream), but also reports an image
    /// filter (e.g. `DCTDecode`) that was left undecoded.
    pub fn decode_stream_full(&self, stream: &Stream) -> Result<filters::Decoded> {
        self.decode_stream_with(stream, false)
    }

    /// [`Document::decode_stream_full`], failing on damaged data instead of
    /// decoding what it can.
    #[cfg(feature = "compress")]
    pub(crate) fn decode_stream_strict(&self, stream: &Stream) -> Result<filters::Decoded> {
        self.decode_stream_with(stream, true)
    }

    fn decode_stream_with(&self, stream: &Stream, strict: bool) -> Result<filters::Decoded> {
        // Filter parameters may be indirect; resolve them one level deep.
        let mut dict = Dictionary::new();
        for key in [&b"Filter"[..], b"DecodeParms", b"F", b"DP"] {
            let v = self.get_resolved(&stream.dict, key)?;
            let v = match v {
                Object::Array(items) => {
                    Object::Array(items.iter().map(|o| self.resolve(o)).collect::<Result<_>>()?)
                }
                other => other,
            };
            if !v.is_null() {
                dict.set(key.to_vec(), v);
            }
        }
        if strict { filters::decode_strict(&stream.raw, &dict) } else { filters::decode(&stream.raw, &dict) }
    }

    // ----------------------------------------------------------------
    // Document structure
    // ----------------------------------------------------------------

    /// The document catalog (`/Root`).
    pub fn catalog(&self) -> Result<Dictionary> {
        let root = self
            .xref
            .borrow()
            .trailer
            .get(b"Root")
            .cloned()
            .ok_or_else(|| Error::Object("trailer has no /Root".into()))?;
        match self.resolve(&root)? {
            Object::Dictionary(d) => Ok(d),
            o => Err(Error::Object(format!("catalog is a {}, expected dictionary", o.type_name()))),
        }
    }

    /// Document information (`/Info` in the trailer).
    pub fn metadata(&self) -> Result<Metadata> {
        let trailer = self.trailer();
        let Some(info) = self.get_dict(&trailer, b"Info") else {
            return Ok(Metadata::default());
        };
        let text = |key: &[u8]| -> Option<String> {
            match self.get_resolved(&info, key).ok()? {
                Object::String(s) => Some(decode_text_string(&s)),
                Object::Name(n) => Some(String::from_utf8_lossy(&n).into_owned()),
                _ => None,
            }
        };
        Ok(Metadata {
            title: text(b"Title"),
            author: text(b"Author"),
            subject: text(b"Subject"),
            keywords: text(b"Keywords"),
            creator: text(b"Creator"),
            producer: text(b"Producer"),
            creation_date: text(b"CreationDate"),
            mod_date: text(b"ModDate"),
        })
    }

    /// All pages in order, walking the page tree.
    pub fn pages(&self) -> Result<Vec<Page>> {
        let catalog = self.catalog()?;
        let root_ref = catalog.get(b"Pages").ok_or_else(|| Error::Object("catalog has no /Pages".into()))?;

        const INHERITABLE: [&[u8]; 4] = [b"Resources", b"MediaBox", b"CropBox", b"Rotate"];
        let mut pages = Vec::new();
        let mut visited = HashSet::new();
        // Stack of (node, inherited attributes). Kids are pushed in reverse
        // so pages come out in document order.
        let mut stack: Vec<(Object, Dictionary)> = vec![(root_ref.clone(), Dictionary::new())];

        while let Some((node_obj, inherited)) = stack.pop() {
            let id = node_obj.as_reference();
            if let Some(r) = id
                && !visited.insert(r) {
                    continue; // cycle or shared node
                }
            let Some(node) = self.resolve(&node_obj)?.as_dict().cloned() else { continue };

            let mut attrs = inherited;
            for key in INHERITABLE {
                if let Some(v) = node.get(key) {
                    attrs.set(key.to_vec(), v.clone());
                }
            }

            let kids = self.get_resolved(&node, b"Kids")?;
            let is_pages_node = match node.type_name() {
                Some(b"Pages") => true,
                Some(b"Page") => false,
                _ => kids.as_array().is_some(), // missing /Type: guess from /Kids
            };
            if is_pages_node {
                if let Some(kids) = kids.as_array() {
                    for kid in kids.iter().rev() {
                        stack.push((kid.clone(), attrs.clone()));
                    }
                }
            } else {
                let mut dict = node;
                for (k, v) in attrs.iter() {
                    if !dict.contains(k) {
                        dict.set(k.to_vec(), v.clone());
                    }
                }
                pages.push(Page { id: id.unwrap_or(ObjRef::new(0, 0)), dict });
            }
        }
        Ok(pages)
    }

    /// Number of pages.
    pub fn page_count(&self) -> Result<usize> {
        Ok(self.pages()?.len())
    }

    /// The page's content stream(s), decoded and concatenated.
    pub fn page_content(&self, page: &Page) -> Result<Vec<u8>> {
        let mut out = Vec::new();
        match self.get_resolved(&page.dict, b"Contents")? {
            Object::Stream(s) => out = self.decode_stream(&s)?,
            Object::Array(parts) => {
                for part in &parts {
                    if let Object::Stream(s) = self.resolve(part)? {
                        out.extend_from_slice(&self.decode_stream(&s)?);
                        // Parts may split tokens only at whitespace boundaries.
                        out.push(b'\n');
                    }
                }
            }
            _ => {} // a page without contents is blank
        }
        Ok(out)
    }
}

fn version_tuple(v: &str) -> (u32, u32) {
    let mut it = v.split('.').map(|p| p.parse().unwrap_or(0));
    (it.next().unwrap_or(0), it.next().unwrap_or(0))
}

/// The contents of `path`, shared with documents already open on the same
/// version of the file (same size and modification time). Kept only while
/// some document uses it.
#[cfg(windows)]
fn shared_file(path: &Path) -> Result<std::sync::Arc<[u8]>> {
    use std::sync::{Arc, Mutex, Weak};
    type Entry = (std::path::PathBuf, u64, Option<std::time::SystemTime>, Weak<[u8]>);
    static OPEN: Mutex<Vec<Entry>> = Mutex::new(Vec::new());

    let meta = std::fs::metadata(path)?;
    if meta.len() == 0 {
        return Err(Error::parse(0, "file is empty"));
    }
    let key = std::path::absolute(path)?;
    let modified = meta.modified().ok();
    let mut open = OPEN.lock().unwrap_or_else(|e| e.into_inner());
    open.retain(|e| e.3.strong_count() > 0);
    if let Some(data) = open
        .iter()
        .find(|e| e.0 == key && e.1 == meta.len() && e.2 == modified)
        .and_then(|e| e.3.upgrade())
    {
        return Ok(data);
    }
    let data: Arc<[u8]> = std::fs::read(path)?.into();
    open.push((key, meta.len(), modified, Arc::downgrade(&data)));
    Ok(data)
}

