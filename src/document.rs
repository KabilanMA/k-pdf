//! The `Document`: entry point for reading a PDF.
//!
//! Memory use is kept low by:
//! - memory-mapping the file instead of reading it into RAM (the OS pages
//!   in only what is touched);
//! - parsing objects lazily, only when requested, with no global cache;
//! - keeping only the few most recently used decoded object streams in
//!   memory.

use std::cell::{Cell, RefCell};
use crate::hash::{HashMap, HashSet};
#[cfg(not(windows))]
use std::fs::File;
use std::ops::Deref;
use std::sync::Arc;
use std::path::Path;

use crate::encoding::decode_text_string;
use crate::error::{Error, Result};
use crate::filters;
use crate::object::{Dictionary, FileData, ObjRef, Object, SharedData, Stream};
use crate::parser::{find, Lexer, Parser, Token};
use crate::xref::{self, XrefEntry, XrefTable};

/// Guards against reference cycles (`1 0 obj 1 0 R endobj`) and runaway
/// recursion while resolving.
const MAX_RESOLVE_DEPTH: u32 = 32;

enum Source {
    /// The file mapped into memory, and the file itself (see `peek`).
    #[cfg(not(windows))]
    Mapped(memmap2::Mmap, File),
    Owned(Vec<u8>),
    /// A file read into memory, shared by every document open on it.
    #[cfg(windows)]
    Shared(std::sync::Arc<[u8]>),
}

impl Source {
    fn is_mapped(&self) -> bool {
        #[cfg(not(windows))]
        if let Source::Mapped(..) = self {
            return true;
        }
        false
    }
}

impl FileData for Source {
    fn bytes(&self) -> &[u8] {
        self
    }

    /// Reads a mapped file's bytes with a system call instead of through
    /// the mapping. Touching a mapped page makes the kernel map the pages
    /// around it too (up to 64 KB), so checking where each of a thousand
    /// small images ends would otherwise bring nearly the whole file into
    /// memory, though the images themselves are never read.
    fn peek(&self, offset: usize, buf: &mut [u8]) -> usize {
        match self {
            #[cfg(unix)]
            Source::Mapped(_, file) => {
                use std::os::unix::fs::FileExt;
                file.read_at(buf, offset as u64).unwrap_or(0)
            }
            _ => {
                let data = self.bytes();
                let n = buf.len().min(data.len().saturating_sub(offset));
                buf[..n].copy_from_slice(&data[offset..offset + n]);
                n
            }
        }
    }
}

impl Deref for Source {
    type Target = [u8];
    fn deref(&self) -> &[u8] {
        match self {
            #[cfg(not(windows))]
            Source::Mapped(m, _) => m,
            Source::Owned(v) => v,
            #[cfg(windows)]
            Source::Shared(d) => d,
        }
    }
}

/// Decoded object streams kept in memory, by size. Consecutive lookups
/// usually hit the same one, but walks such as the outline go back and
/// forth between a few (the outline's, the destinations', the pages'), and
/// whole-document jobs (saving) visit every object in reference order,
/// which jumps between all of them: see [`Document::with_object_cache`].
const OBJSTM_CACHE_BYTES: usize = 256 << 10;
/// Kept whatever their size.
const OBJSTM_CACHE_MIN: usize = 4;

/// Recently used object streams, least recently used dropped first.
#[derive(Default)]
struct ObjStmLru {
    entries: HashMap<u32, (std::rc::Rc<ObjStmCache>, u64)>,
    bytes: usize,
    tick: u64,
}

impl ObjStmLru {
    fn get(&mut self, num: u32) -> Option<std::rc::Rc<ObjStmCache>> {
        self.tick += 1;
        let tick = self.tick;
        self.entries.get_mut(&num).map(|e| {
            e.1 = tick;
            e.0.clone()
        })
    }

    fn insert(&mut self, cache: ObjStmCache, budget: usize) -> std::rc::Rc<ObjStmCache> {
        self.tick += 1;
        while self.entries.len() >= OBJSTM_CACHE_MIN && self.bytes + cache.data.len() > budget {
            let Some((&oldest, _)) = self.entries.iter().min_by_key(|(_, e)| e.1) else { break };
            if let Some((old, _)) = self.entries.remove(&oldest) {
                self.bytes -= old.data.len();
            }
        }
        self.bytes += cache.data.len();
        let num = cache.num;
        let cache = std::rc::Rc::new(cache);
        self.entries.insert(num, (cache.clone(), self.tick));
        cache
    }

    fn clear(&mut self) {
        *self = ObjStmLru::default();
    }
}

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
    /// The file's bytes, shared with the streams read from it.
    data: Arc<Source>,
    /// Offset of `%PDF-` (non-zero when junk precedes the header).
    header_offset: usize,
    header_version: String,
    xref: RefCell<XrefTable>,
    reconstructed: Cell<bool>,
    /// Most recently used first.
    objstm: RefCell<ObjStmLru>,
    /// How many bytes of decoded object streams `objstm` may keep.
    objstm_budget: Cell<usize>,
    /// The buffer objects are read into (see `parse_indirect_at`).
    window: RefCell<Vec<u8>>,
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
        Self::from_source(Source::Mapped(map, file), password)
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
            data: Arc::new(data),
            header_offset,
            header_version,
            xref: RefCell::new(table),
            reconstructed: Cell::new(reconstructed),
            objstm: RefCell::new(ObjStmLru::default()),
            objstm_budget: Cell::new(OBJSTM_CACHE_BYTES),
            window: RefCell::new(Vec::new()),
            depth: Cell::new(0),
            overlay: RefCell::new(HashMap::default()),
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
        // File order: objects in the file by offset, then those in object
        // streams by stream and index (the object number breaks ties, so
        // the order is always the same). The key is packed into one
        // integer: this sorts hundreds of thousands of entries in big files.
        entries.sort_unstable_by_key(|&(num, e)| {
            let (kind, a, b) = match e {
                XrefEntry::InFile { offset, .. } => (0u128, offset as u64, 0u32),
                XrefEntry::InStream { stream, index } => (1, stream as u64, index),
            };
            kind << 127 | ((a as u128) & ((1 << 63) - 1)) << 64 | (b as u128) << 32 | num as u128
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
        let mut map = HashMap::default();
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

    /// Parses the indirect object at `off`. A memory-mapped file is read
    /// through a small window copied with a system call (see
    /// `FileData::peek`), not through the mapping: reading an image's
    /// dictionary would otherwise map in the image data around it, and
    /// with it most of a file of small images. The window grows when the
    /// object might not fit. Streams still point into the mapping, so their
    /// data is only read when it is decoded.
    fn parse_indirect_at(&self, off: usize, resolve_length: crate::parser::LengthResolver) -> Result<(ObjRef, Object)> {
        let shared: SharedData = self.data.clone();
        if !self.data.is_mapped() {
            return Parser::new_shared(&self.data, &shared, off).parse_indirect(resolve_length);
        }
        let len = self.data.len();
        let mut window = 4096;
        // One buffer, reused (taken out while in use: reading a /Length
        // reads another object).
        let mut buf = self.window.take();
        loop {
            buf.clear();
            buf.resize(window.min(len - off), 0);
            let got = shared.peek(off, &mut buf);
            buf.truncate(got);
            let whole = off + got >= len;
            let mut parser = Parser::new_window(&buf, &shared, off);
            let result = parser.parse_indirect(resolve_length);
            // The object may have been cut off by the window's end: try a
            // bigger one.
            if !whole && (result.is_err() || parser.header_end() + 64 > got) {
                window *= 8;
                continue;
            }
            if buf.capacity() <= 64 * 1024 {
                self.window.replace(buf);
            }
            return result;
        }
    }

    fn load_in_file(&self, num: u32, offset: usize) -> Result<Object> {
        let resolve_length = |lr: ObjRef| self.get_object(lr).ok()?.as_i64();
        let mut last_err = None;
        for off in [offset, offset + self.header_offset] {
            if off >= self.data.len() {
                continue;
            }
            match self.parse_indirect_at(off, &resolve_length) {
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
        let cached = self.objstm.borrow_mut().get(stream);
        let cache = match cached {
            Some(c) => c,
            None => {
                let loaded = self.load_objstm(stream)?;
                self.objstm.borrow_mut().insert(loaded, self.objstm_budget.get())
            }
        };
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

    /// Runs `f` with up to `bytes` of decoded object streams kept in
    /// memory, for jobs that visit every object (saving, compressing):
    /// they jump between object streams, and with the usual small cache
    /// each would be decompressed again and again (13,000 times for the
    /// 984 streams of a 4,000-page manual).
    pub(crate) fn with_object_cache<R>(&self, bytes: usize, f: impl FnOnce() -> R) -> R {
        let before = self.objstm_budget.replace(bytes.max(self.objstm_budget.get()));
        let result = f();
        self.objstm_budget.set(before);
        let mut lru = self.objstm.borrow_mut();
        if lru.bytes > before {
            lru.clear();
        }
        result
    }

    /// Every object stored in an object stream, parsed: object streams
    /// are independent, so they are decoded and parsed on all cores. For
    /// jobs that read the whole document (compressing). Objects that can't
    /// be read this way are left out (read them with `get_object`).
    pub(crate) fn objects_in_streams(&self) -> Vec<(u32, Object)> {
        // Which objects each stream holds, by the xref.
        let mut wanted: HashMap<u32, Vec<(u32, u32)>> = HashMap::default();
        for (&num, &e) in &self.xref.borrow().entries {
            if let XrefEntry::InStream { stream, index } = e {
                wanted.entry(stream).or_default().push((index, num));
            }
        }
        // The streams, with what decoding them needs from the document.
        struct Job {
            raw: crate::object::Bytes,
            filters: Dictionary,
            n: usize,
            first: usize,
            members: Vec<(u32, u32)>,
        }
        let mut jobs = Vec::with_capacity(wanted.len());
        for (stream, members) in wanted {
            let Ok(Object::Stream(s)) = self.get_object(ObjRef::new(stream, 0)) else { continue };
            let mut filters = Dictionary::new();
            for key in [&b"Filter"[..], b"DecodeParms", b"F", b"DP"] {
                let v = match self.get_resolved(&s.dict, key) {
                    Ok(Object::Array(items)) => Object::Array(items.iter().filter_map(|o| self.resolve(o).ok()).collect()),
                    Ok(v) => v,
                    Err(_) => Object::Null,
                };
                if !v.is_null() {
                    filters.set(key.to_vec(), v);
                }
            }
            let n = self.resolve_dict_int(&s.dict, b"N").unwrap_or(0).clamp(0, 1 << 24) as usize;
            let first = self.resolve_dict_int(&s.dict, b"First").unwrap_or(0).max(0) as usize;
            jobs.push(Job { raw: s.raw, filters, n, first, members });
        }
        let parse = |job: &Job| -> Vec<(u32, Object)> {
            let Ok(decoded) = filters::decode(&job.raw, &job.filters) else { return Vec::new() };
            let data = decoded.data;
            let mut offsets = Vec::with_capacity(job.n.min(10_000));
            let mut lex = Lexer::new(&data, 0);
            for _ in 0..job.n {
                match (lex.next_token(), lex.next_token()) {
                    (Some(Token::Integer(obj)), Some(Token::Integer(off))) if obj >= 0 && off >= 0 => {
                        offsets.push((obj as u32, job.first + off as usize));
                    }
                    _ => break,
                }
            }
            job.members
                .iter()
                .filter_map(|&(index, num)| {
                    // Trust `index` if it agrees with the header, as `load_from_objstm` does.
                    let offset = match offsets.get(index as usize) {
                        Some(&(n, off)) if n == num => off,
                        _ => offsets.iter().find(|(n, _)| *n == num)?.1,
                    };
                    Some((num, Parser::new(&data, offset).parse_object().ok()?))
                })
                .collect()
        };
        let threads = std::thread::available_parallelism().map_or(1, |n| n.get()).min(jobs.len()).max(1);
        let next = std::sync::atomic::AtomicUsize::new(0);
        let work = || {
            let mut out = Vec::new();
            while let Some(job) = jobs.get(next.fetch_add(1, std::sync::atomic::Ordering::Relaxed)) {
                out.extend(parse(job));
            }
            out
        };
        if threads == 1 {
            return work();
        }
        std::thread::scope(|s| {
            let handles: Vec<_> = (0..threads).map(|_| s.spawn(work)).collect();
            handles.into_iter().flat_map(|h| h.join().unwrap_or_default()).collect()
        })
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
        let mut visited = HashSet::default();
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

    /// The page at `index` (0-based), or `None` past the end. Descends the
    /// page tree by each node's `/Count`, so it reads only the nodes on the
    /// way down instead of every page. If a node's counts don't add up (a
    /// damaged tree), it falls back to [`Document::pages`], so the result
    /// is always the page `pages()[index]` would give.
    pub fn page(&self, index: usize) -> Result<Option<Page>> {
        match self.page_by_count(index) {
            Some(page) => Ok(Some(page)),
            None => Ok(self.pages()?.into_iter().nth(index)),
        }
    }

    fn page_by_count(&self, mut index: usize) -> Option<Page> {
        const INHERITABLE: [&[u8]; 4] = [b"Resources", b"MediaBox", b"CropBox", b"Rotate"];
        let count = |d: &Dictionary| self.get_resolved(d, b"Count").ok()?.as_i64().and_then(|c| usize::try_from(c).ok());
        let catalog = self.catalog().ok()?;
        let mut node_obj = catalog.get(b"Pages")?.clone();
        let mut attrs = Dictionary::new();
        for _ in 0..64 {
            let node = self.resolve(&node_obj).ok()?.as_dict()?.clone();
            for key in INHERITABLE {
                if let Some(v) = node.get(key) {
                    attrs.set(key.to_vec(), v.clone());
                }
            }
            if node.type_name() != Some(b"Pages") {
                // A leaf: the page itself, with what it inherits.
                if index != 0 || node.type_name() != Some(b"Page") {
                    return None;
                }
                let mut dict = node;
                for (k, v) in attrs.iter() {
                    if !dict.contains(k) {
                        dict.set(k.to_vec(), v.clone());
                    }
                }
                return Some(Page { id: node_obj.as_reference()?, dict });
            }
            let kids = self.get_resolved(&node, b"Kids").ok()?;
            let kids = kids.as_array()?;
            // Each kid's page count; they must add up to the node's.
            let mut sizes = Vec::with_capacity(kids.len());
            for kid in kids {
                let kid_dict = self.resolve(kid).ok()?.as_dict()?.clone();
                sizes.push(match kid_dict.type_name() {
                    Some(b"Page") => 1,
                    Some(b"Pages") => count(&kid_dict)?,
                    _ => return None,
                });
            }
            if sizes.iter().sum::<usize>() != count(&node)? {
                return None;
            }
            let mut chosen = None;
            for (i, &size) in sizes.iter().enumerate() {
                if index < size {
                    chosen = Some(i);
                    break;
                }
                index -= size;
            }
            node_obj = kids[chosen?].clone();
        }
        None
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

