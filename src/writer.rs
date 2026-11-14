//! Writing documents: saving a copy with its pages in a new order, or
//! combined with pages from other documents.
//!
//! The output is a complete, freshly laid out file. Every object is copied
//! as-is (stream data stays compressed exactly as it was), objects from
//! object streams are written out individually, and the page tree is
//! replaced by a single flat `/Pages` node listing the pages in the new
//! order. Attributes pages inherited from intermediate tree nodes
//! (`Resources`, `MediaBox`, `CropBox`, `Rotate`) are copied into each page,
//! so flattening the tree changes nothing about how pages look.
//!
//! PDF 1.5+ documents are written with compressed object streams and a
//! cross-reference stream (as such files usually are); older versions get
//! plain objects and a classic cross-reference table.
//!
//! Objects are read and written one at a time (at most one object stream's
//! worth is held back for compression), so memory use stays small
//! regardless of the file size. Damaged files are saved as repaired.

use std::cell::Cell;
use crate::hash::{HashMap, HashSet};
use std::io::{self, Write};

use crate::document::{Document, Page};
use crate::error::{Error, Result};
use crate::object::{Dictionary, ObjRef, Object, Stream};
use crate::xref::XrefEntry;

/// Writes `doc` to `out` with its pages in `order` (indices into
/// [`Document::pages`]). `order` may leave pages out (they are dropped) or
/// repeat one.
///
/// A password-protected document is saved with the same protection (see
/// [`write_document_with_options`]).
pub fn write_with_page_order(doc: &Document, order: &[usize], out: impl Write) -> Result<()> {
    let pages: Vec<(usize, usize)> = order.iter().map(|&p| (0, p)).collect();
    write_document(&[doc], &pages, out)
}

/// Writes a document made of pages from several documents: `pages` lists
/// `(document, page)` pairs, indices into `docs` and into that document's
/// [`Document::pages`], in the order they should appear.
///
/// `docs[0]` is the base: its catalog, metadata, outlines and so on are
/// kept. From the other documents only the pages used, and what they
/// reference (content, fonts, images, annotations…), are copied in.
/// Their references to pages that aren't copied become null.
pub fn write_document(docs: &[&Document], pages: &[(usize, usize)], out: impl Write) -> Result<()> {
    write_document_with_changes(docs, pages, &Changes::default(), out)
}

/// Edits to the base document applied while writing: objects replaced by
/// new versions, and new objects. New objects must be numbered above every
/// object of the base document (so they don't collide with it).
#[derive(Debug, Default, Clone)]
pub struct Changes {
    /// Object number → (reference, new object).
    pub replace: std::collections::HashMap<u32, (ObjRef, Object)>,
    pub added: Vec<(ObjRef, Object)>,
}

/// How files are written.
#[derive(Debug, Default, Clone)]
pub struct WriteOptions {
    /// Compress object streams and the cross-reference stream as tightly as
    /// possible (slower; see [`crate::compress`]).
    pub max_compression: bool,
    /// The version to declare in the file header, instead of the newest of
    /// the documents' own.
    pub version: Option<String>,
    /// Save a password-protected document without its password (it is
    /// kept otherwise).
    pub decrypt: bool,
}

/// [`write_document`] with `changes` (e.g. from [`crate::fill_form`])
/// applied to `docs[0]`.
pub fn write_document_with_changes(
    docs: &[&Document],
    pages: &[(usize, usize)],
    changes: &Changes,
    out: impl Write,
) -> Result<()> {
    write_document_with_options(docs, pages, changes, &WriteOptions::default(), out)
}

/// [`write_document_with_changes`], written as `options` say.
pub fn write_document_with_options(
    docs: &[&Document],
    pages: &[(usize, usize)],
    changes: &Changes,
    options: &WriteOptions,
    out: impl Write,
) -> Result<()> {
    // Saving visits every object in reference order, which jumps between
    // object streams: keep them all decoded while writing (see
    // `Document::with_object_cache`).
    fn with_caches<R>(docs: &[&Document], f: impl FnOnce() -> R) -> R {
        match docs.split_first() {
            None => f(),
            Some((d, rest)) => d.with_object_cache(64 << 20, || with_caches(rest, f)),
        }
    }
    with_caches(docs, || write_documents(docs, pages, changes, Preloaded::default(), options, out).map(drop))
}

/// Objects of the (first) document already read by the caller, by number,
/// so the writer doesn't read them again.
pub(crate) type Preloaded = crate::hash::HashMap<u32, Object>;

/// [`write_document_with_options`] for one document whose objects (the
/// unchanged ones; changed ones are in `changes`) were all read already.
/// Returns what is left of them, to be dropped when convenient.
pub(crate) fn write_preloaded(
    doc: &Document,
    changes: &Changes,
    preloaded: Preloaded,
    options: &WriteOptions,
    out: impl Write,
) -> Result<Vec<Object>> {
    let pages: Vec<(usize, usize)> = (0..doc.page_count()?).map(|p| (0, p)).collect();
    write_documents(&[doc], &pages, changes, preloaded, options, out)
}

fn write_documents(
    docs: &[&Document],
    pages: &[(usize, usize)],
    changes: &Changes,
    preloaded: Preloaded,
    options: &WriteOptions,
    out: impl Write,
) -> Result<Vec<Object>> {
    let Some(&doc) = docs.first() else { return Err(Error::Object("no document to write".into())) };
    let all_pages: Vec<Vec<Page>> = docs.iter().map(|d| d.pages()).collect::<Result<_>>()?;
    for &(d, p) in pages {
        let Some(list) = all_pages.get(d) else {
            return Err(Error::Object(format!("document {d} was not given")));
        };
        if p >= list.len() {
            return Err(Error::Object(format!("page {} does not exist (the document has {})", p + 1, list.len())));
        }
    }

    let entries = doc.xref_entries();
    let highest = entries.iter().map(|(n, _)| *n).chain(changes.added.iter().map(|(r, _)| r.num)).max().unwrap_or(0);
    let counter = Cell::new(highest + 1);
    let mut new_ref = || next_ref(&counter);

    // The catalog and page tree root; either may be missing or (in odd
    // files) direct, in which case they get new object numbers.
    let trailer = doc.trailer();
    let catalog_ref = trailer.get(b"Root").and_then(Object::as_reference).unwrap_or_else(&mut new_ref);
    let mut catalog = doc.catalog()?;
    let pages_ref = catalog.get(b"Pages").and_then(Object::as_reference).unwrap_or_else(&mut new_ref);
    catalog.set(b"Pages".to_vec(), Object::Reference(pages_ref));

    // Replacement objects: the catalog, the new flat page tree, and each
    // base page with its inherited attributes and new parent.
    let mut replace: HashMap<u32, (ObjRef, Object)> = HashMap::default();
    replace.insert(catalog_ref.num, (catalog_ref, Object::Dictionary(catalog)));
    // Output reference of every page used, by (document, page).
    let mut page_refs: HashMap<(usize, usize), ObjRef> = HashMap::default();
    let mut importers: Vec<Importer> = docs.iter().map(|&d| Importer::new(d)).collect();
    for &(d, p) in pages {
        if page_refs.contains_key(&(d, p)) {
            continue;
        }
        let page = &all_pages[d][p];
        let r = if d == 0 {
            // Pages stored as direct objects in /Kids (invalid, but seen)
            // get a number of their own.
            let r = if page.id.num == 0 { new_ref() } else { page.id };
            let mut dict = page.dict.clone();
            dict.set(b"Parent".to_vec(), Object::Reference(pages_ref));
            replace.insert(r.num, (r, Object::Dictionary(dict)));
            r
        } else {
            importers[d].add_page(page, &mut new_ref)
        };
        page_refs.insert((d, p), r);
    }
    let kids: Vec<Object> = pages.iter().map(|k| Object::Reference(page_refs[k])).collect();
    let mut tree = Dictionary::new();
    tree.set(b"Type".to_vec(), Object::Name(b"Pages".to_vec()));
    tree.set(b"Count".to_vec(), Object::Integer(kids.len() as i64));
    tree.set(b"Kids".to_vec(), Object::Array(kids));
    replace.insert(pages_ref.num, (pages_ref, Object::Dictionary(tree)));

    // Left out: the old page tree's inner nodes, dropped pages, and the
    // file-structure objects that don't carry over (object streams, xref
    // streams, linearization data).
    let mut skip = old_tree_nodes(doc, &catalog_ref, pages_ref);
    for (i, page) in all_pages[0].iter().enumerate() {
        if !page_refs.contains_key(&(0, i)) && page.id.num != 0 {
            skip.insert(page.id.num);
        }
    }
    // Form fields that were only on dropped pages go with them.
    let dropped: HashSet<u32> = all_pages[0]
        .iter()
        .enumerate()
        .filter(|(i, p)| !page_refs.contains_key(&(0, *i)) && p.id.num != 0)
        .map(|(_, p)| p.id.num)
        .collect();
    if !dropped.is_empty() {
        FieldPruner::new(doc, changes, &dropped, &all_pages[0]).prune(catalog_ref, &mut replace);
    }

    // Only objects still in use are written: whatever edits replaced
    // (old page content, redacted images, earlier edits' objects) must not
    // stay in the file.
    let generations: HashMap<u32, u16> = entries
        .iter()
        .map(|(n, e)| (*n, if let XrefEntry::InFile { generation, .. } = e { *generation } else { 0 }))
        .collect();
    // The newest version among the documents.
    let version = options.version.clone().unwrap_or_else(|| {
        docs.iter()
            .map(|d| d.version())
            .filter(|v| !v.is_empty())
            .max_by(|a, b| version_tuple(a).cmp(&version_tuple(b)))
            .unwrap_or_else(|| "1.7".to_string())
    });
    // PDF 1.5+ files are saved with compressed object streams, like the
    // original most likely was; older versions get a classic xref table.
    let compress = version_tuple(&version) >= (1, 5);
    let added: HashMap<u32, &Object> = changes.added.iter().map(|(r, o)| (r.num, o)).collect();
    let mut used: HashSet<u32> = HashSet::default();
    // Objects read from the file while finding the ones in use, kept for
    // writing so they aren't parsed twice: as their text when they will go
    // into an object stream (much smaller than the parsed object), else as
    // they are.
    let mut read: crate::hash::HashMap<u32, Read> = preloaded.into_iter().map(|(n, o)| (n, Read::Object(o))).collect();
    let mut todo: Vec<u32> = vec![catalog_ref.num];
    todo.extend(trailer.get(b"Info").and_then(Object::as_reference).map(|r| r.num));
    while let Some(num) = todo.pop() {
        if skip.contains(&num) || !used.insert(num) {
            continue;
        }
        let loaded;
        let obj = match replace.get(&num).or(changes.replace.get(&num)) {
            Some((_, o)) => o,
            None => match added.get(&num) {
                Some(o) => *o,
                None if read.contains_key(&num) => {
                    if let Some(Read::Object(o)) = read.get(&num) {
                        references(o, &mut todo);
                    }
                    continue;
                }
                None => {
                    let generation = generations.get(&num).copied().unwrap_or(0);
                    loaded = doc.get_object(ObjRef::new(num, generation)).unwrap_or(Object::Null);
                    references(&loaded, &mut todo);
                    let packed = compress && generation == 0 && !matches!(loaded, Object::Stream(_));
                    read.insert(num, if packed { Read::Text(loaded.to_bytes()) } else { Read::Object(loaded) });
                    continue;
                }
            },
        };
        references(obj, &mut todo);
    }

    let mut sink = Sink::new(out, &version, compress, options.max_compression)?;
    // A password-protected base document stays protected: encrypted again
    // with its key, with its encryption dictionary and file ID, so the
    // same passwords open it with the same permissions. (Pages from other
    // documents were decrypted as they were read.)
    sink.crypt = if options.decrypt { None } else { doc.crypt().cloned() };

    let mut changed: HashMap<u32, (ObjRef, Object)> = changes.replace.iter().map(|(&n, v)| (n, v.clone())).collect();
    // Objects written (and preloaded ones not needed), dropped by the
    // caller: freeing a whole document's objects one by one is slow.
    let mut done: Vec<Object> = Vec::new();
    for (num, entry) in &entries {
        if !used.contains(num) {
            continue;
        }
        let edit = changed.remove(num);
        let (r, obj) = match replace.remove(num).or(edit) {
            Some(v) => v,
            None => {
                let generation = match entry {
                    XrefEntry::InFile { generation, .. } => *generation,
                    XrefEntry::InStream { .. } => 0,
                };
                let r = ObjRef::new(*num, generation);
                // An unreadable object becomes null rather than failing
                // the whole save.
                let obj = match read.remove(num) {
                    Some(Read::Text(text)) => {
                        sink.add_text(r.num, text, &mut new_ref)?;
                        continue;
                    }
                    Some(Read::Object(obj)) => obj,
                    None => doc.get_object(r).unwrap_or(Object::Null),
                };
                if is_file_structure(&obj) {
                    continue;
                }
                (r, obj)
            }
        };
        sink.add(r, &obj, &mut new_ref)?;
        done.push(obj);
    }
    done.extend(read.into_values().filter_map(|r| match r {
        Read::Object(o) => Some(o),
        Read::Text(_) => None,
    }));
    // New objects, and replacements for objects missing from the xref.
    let mut rest: Vec<_> = replace.into_values().chain(changed.into_values()).collect();
    // (A new object that was replaced since is written once, replaced.)
    rest.extend(changes.added.iter().filter(|(r, _)| !changes.replace.contains_key(&r.num)).cloned());
    rest.retain(|(r, _)| used.contains(&r.num));
    rest.sort_by_key(|(r, _)| r.num);
    for (r, obj) in rest {
        sink.add(r, &obj, &mut new_ref)?;
    }
    // Pages from the other documents, and everything they use.
    for importer in importers.iter_mut().skip(1) {
        importer.copy(pages_ref, &mut sink, &counter)?;
    }

    let mut new_trailer = Dictionary::new();
    new_trailer.set(b"Root".to_vec(), Object::Reference(catalog_ref));
    for key in [&b"Info"[..], b"ID"] {
        if let Some(v) = trailer.get(key) {
            new_trailer.set(key.to_vec(), v.clone());
        }
    }
    sink.finish(new_trailer, &mut new_ref)?;
    Ok(done)
}

/// Adds the object numbers `obj` refers to (directly) to `out`.
fn references(obj: &Object, out: &mut Vec<u32>) {
    match obj {
        Object::Reference(r) => out.push(r.num),
        Object::Array(items) => items.iter().for_each(|o| references(o, out)),
        Object::Dictionary(d) => d.iter().for_each(|(_, o)| references(o, out)),
        Object::Stream(s) => s.dict.iter().for_each(|(_, o)| references(o, out)),
        _ => {}
    }
}

fn next_ref(counter: &Cell<u32>) -> ObjRef {
    let n = counter.get();
    counter.set(n + 1);
    ObjRef::new(n, 0)
}

fn version_tuple(version: &str) -> (u32, u32) {
    let mut parts = version.split('.').map(|p| p.parse::<u32>().unwrap_or(0));
    (parts.next().unwrap_or(0), parts.next().unwrap_or(0))
}

/// Copies pages of another document, with everything they reference,
/// under new object numbers.
struct Importer<'a> {
    doc: &'a Document,
    /// Old object number → new reference.
    map: HashMap<u32, ObjRef>,
    /// Objects still to copy (old references).
    queue: Vec<ObjRef>,
    /// The pages being copied, by old object number: their dictionaries
    /// (with inherited attributes) replace the stored ones.
    pages: HashMap<u32, Dictionary>,
    /// Pages that were direct objects, already numbered.
    direct_pages: Vec<(ObjRef, Dictionary)>,
}

impl<'a> Importer<'a> {
    fn new(doc: &'a Document) -> Self {
        Importer { doc, map: HashMap::default(), queue: Vec::new(), pages: HashMap::default(), direct_pages: Vec::new() }
    }

    fn add_page(&mut self, page: &Page, new_ref: &mut impl FnMut() -> ObjRef) -> ObjRef {
        let r = new_ref();
        if page.id.num == 0 {
            self.direct_pages.push((r, page.dict.clone()));
        } else {
            self.map.insert(page.id.num, r);
            self.pages.insert(page.id.num, page.dict.clone());
            self.queue.push(page.id);
        }
        r
    }

    /// Writes the pages and everything reachable from them.
    fn copy<W: Write>(&mut self, parent: ObjRef, sink: &mut Sink<W>, counter: &Cell<u32>) -> io::Result<()> {
        for (r, mut dict) in std::mem::take(&mut self.direct_pages) {
            dict.remove(b"Parent");
            let mut obj = self.rewrite(Object::Dictionary(dict), counter);
            if let Object::Dictionary(d) = &mut obj {
                d.set(b"Parent".to_vec(), Object::Reference(parent));
            }
            sink.add(r, &obj, &mut || next_ref(counter))?;
        }
        while let Some(old) = self.queue.pop() {
            let new = self.map[&old.num];
            let obj = match self.pages.remove(&old.num) {
                Some(mut dict) => {
                    dict.remove(b"Parent");
                    let mut obj = self.rewrite(Object::Dictionary(dict), counter);
                    if let Object::Dictionary(d) = &mut obj {
                        d.set(b"Parent".to_vec(), Object::Reference(parent));
                    }
                    obj
                }
                None => {
                    let obj = self.doc.get_object(old).unwrap_or(Object::Null);
                    // Other pages and the page tree aren't copied: links to
                    // them (e.g. an annotation's /P) become null.
                    if is_page_tree_node(&obj) || is_file_structure(&obj) {
                        Object::Null
                    } else {
                        self.rewrite(obj, counter)
                    }
                }
            };
            sink.add(new, &obj, &mut || next_ref(counter))?;
        }
        Ok(())
    }

    /// Renumbers the references in `obj`, queueing objects not seen yet.
    fn rewrite(&mut self, obj: Object, counter: &Cell<u32>) -> Object {
        match obj {
            Object::Reference(r) => {
                let new = *self.map.entry(r.num).or_insert_with(|| {
                    self.queue.push(r);
                    next_ref(counter)
                });
                Object::Reference(new)
            }
            Object::Array(items) => Object::Array(items.into_iter().map(|o| self.rewrite(o, counter)).collect()),
            Object::Dictionary(d) => Object::Dictionary(self.rewrite_dict(d, counter)),
            Object::Stream(mut s) => {
                s.dict = self.rewrite_dict(s.dict, counter);
                Object::Stream(s)
            }
            other => other,
        }
    }

    fn rewrite_dict(&mut self, d: Dictionary, counter: &Cell<u32>) -> Dictionary {
        let mut out = Dictionary::new();
        for (k, v) in d.iter() {
            out.set(k.to_vec(), self.rewrite(v.clone(), counter));
        }
        out
    }
}

fn is_page_tree_node(obj: &Object) -> bool {
    matches!(obj.as_dict().and_then(Dictionary::type_name), Some(b"Page") | Some(b"Pages"))
}

/// Objects per object stream.
const OBJSTM_SIZE: usize = 100;
const OBJSTM_SIZE_MAX: usize = 400;
/// Object streams compressed together, one per thread.
const PACKED_AT_ONCE: usize = 16;

enum Entry {
    InFile { generation: u16, offset: u64 },
    InStream { stream: u32, index: u32 },
}

/// Writes objects and remembers where they went, then writes the
/// cross-reference section.
/// An object read while finding the objects in use (see `write_documents`).
enum Read {
    /// Its text, for an object stream.
    Text(Vec<u8>),
    Object(Object),
}

struct Sink<W: Write> {
    w: CountingWriter<W>,
    entries: HashMap<u32, Entry>,
    /// With compression: objects waiting to go into the next object stream,
    /// already serialized.
    batch: Vec<(u32, Vec<u8>)>,
    /// Object streams packed but not yet compressed (their reference,
    /// dictionary and data): they are compressed several at a time on
    /// separate threads.
    packed: Vec<(ObjRef, Dictionary, Vec<u8>)>,
    compress: bool,
    /// Compress as tightly as possible.
    max: bool,
    /// Encrypts each object written directly (object streams' members are
    /// encrypted with their stream).
    crypt: Option<crate::crypt::Crypt>,
}

impl<W: Write> Sink<W> {
    fn new(out: W, version: &str, compress: bool, max: bool) -> io::Result<Self> {
        let mut w = CountingWriter { inner: io::BufWriter::new(out), written: 0 };
        w.write_all(format!("%PDF-{version}\n%\u{e2}\u{e3}\u{cf}\u{d3}\n").as_bytes())?;
        Ok(Sink { w, entries: HashMap::default(), batch: Vec::new(), packed: Vec::new(), compress, max, crypt: None })
    }

    fn add(&mut self, r: ObjRef, obj: &Object, new_ref: &mut impl FnMut() -> ObjRef) -> io::Result<()> {
        // Streams can't go into object streams, and objects there must have
        // generation 0.
        if self.compress && r.generation == 0 && !matches!(obj, Object::Stream(_)) {
            return self.add_text(r.num, obj.to_bytes(), new_ref);
        }
        self.write_direct(r, obj)
    }

    /// Adds an object, already serialized, to the next object stream.
    fn add_text(&mut self, num: u32, text: Vec<u8>, new_ref: &mut impl FnMut() -> ObjRef) -> io::Result<()> {
        debug_assert!(self.compress);
        self.batch.push((num, text));
        // Bigger object streams compress better (similar objects share
        // one compression context), smaller ones load faster.
        if self.batch.len() >= if self.max { OBJSTM_SIZE_MAX } else { OBJSTM_SIZE } {
            self.flush_batch(new_ref)?;
        }
        Ok(())
    }

    fn write_direct(&mut self, r: ObjRef, obj: &Object) -> io::Result<()> {
        self.entries.insert(r.num, Entry::InFile { generation: r.generation, offset: self.w.written });
        if let Some(crypt) = &self.crypt {
            let mut sealed = obj.clone();
            crypt.encrypt_object(r.num, r.generation, &mut sealed);
            return write_object(&mut self.w, r, &sealed);
        }
        write_object(&mut self.w, r, obj)
    }

    /// Packs the pending objects into one compressed object stream.
    fn flush_batch(&mut self, new_ref: &mut impl FnMut() -> ObjRef) -> io::Result<()> {
        if self.batch.is_empty() {
            return Ok(());
        }
        let stream_ref = new_ref();
        let mut header = String::new();
        let mut body = Vec::new();
        for (index, (num, bytes)) in self.batch.iter().enumerate() {
            header += &format!("{num} {} ", body.len());
            body.extend_from_slice(bytes);
            body.push(b'\n');
            self.entries.insert(*num, Entry::InStream { stream: stream_ref.num, index: index as u32 });
        }
        let first = header.len();
        let mut data = header.into_bytes();
        data.extend_from_slice(&body);
        let mut dict = Dictionary::new();
        dict.set(b"Type".to_vec(), Object::Name(b"ObjStm".to_vec()));
        dict.set(b"N".to_vec(), Object::Integer(self.batch.len() as i64));
        dict.set(b"First".to_vec(), Object::Integer(first as i64));
        dict.set(b"Filter".to_vec(), Object::Name(b"FlateDecode".to_vec()));
        self.batch.clear();
        self.packed.push((stream_ref, dict, data));
        if self.packed.len() >= PACKED_AT_ONCE {
            self.write_packed()?;
        }
        Ok(())
    }

    /// Compresses the packed object streams (in parallel) and writes them.
    fn write_packed(&mut self) -> io::Result<()> {
        let packed = std::mem::take(&mut self.packed);
        let max = self.max;
        let compressed: Vec<io::Result<Vec<u8>>> = if packed.len() == 1 {
            vec![deflate(&packed[0].2, max)]
        } else {
            std::thread::scope(|s| {
                let handles: Vec<_> = packed.iter().map(|(_, _, data)| s.spawn(move || deflate(data, max))).collect();
                handles.into_iter().map(|h| h.join().expect("compression thread panicked")).collect()
            })
        };
        for ((r, dict, _), raw) in packed.into_iter().zip(compressed) {
            self.write_direct(r, &Object::Stream(Stream { dict, raw: raw?.into() }))?;
        }
        Ok(())
    }

    /// Writes the cross-reference section (a table, or a compressed xref
    /// stream) with `trailer`'s entries.
    fn finish(mut self, mut trailer: Dictionary, new_ref: &mut impl FnMut() -> ObjRef) -> Result<()> {
        self.flush_batch(new_ref)?;
        self.write_packed()?;
        // The encryption dictionary, itself not encrypted.
        if let Some(crypt) = self.crypt.take() {
            let r = new_ref();
            self.entries.insert(r.num, Entry::InFile { generation: 0, offset: self.w.written });
            write_object(&mut self.w, r, &Object::Dictionary(crypt.dict))?;
            trailer.set(b"Encrypt".to_vec(), Object::Reference(r));
        }
        let xref_ref = new_ref();
        // Drop /Info if it wasn't written.
        if let Some(Object::Reference(info)) = trailer.get(b"Info")
            && !self.entries.contains_key(&info.num)
        {
            trailer.remove(b"Info");
        }
        let xref_at = self.w.written;
        if self.compress {
            self.entries.insert(xref_ref.num, Entry::InFile { generation: 0, offset: xref_at });
        }
        let size = self.entries.keys().max().map_or(1, |m| m + 1);
        trailer.set(b"Size".to_vec(), Object::Integer(size as i64));

        // Free entries form a linked list headed by object 0.
        let mut next_free = vec![0u32; size as usize];
        let mut following = 0;
        for i in (0..size).rev() {
            next_free[i as usize] = following;
            if i != 0 && !self.entries.contains_key(&i) {
                following = i;
            }
        }

        if self.compress {
            let rows: Vec<(u8, u64, u32)> = (0..size)
                .map(|i| match self.entries.get(&i) {
                    Some(Entry::InFile { generation, offset }) => (1, *offset, u32::from(*generation)),
                    Some(Entry::InStream { stream, index }) => (2, u64::from(*stream), *index),
                    None => (0, u64::from(next_free[i as usize]), if i == 0 { 65535 } else { 0 }),
                })
                .collect();
            // Field widths: type (1 byte), offset or stream (5), generation
            // or index (2). Tightest: just the bytes the values need.
            let bytes = |m: u64| (8 - m.leading_zeros() as usize / 8).max(1);
            let (wa, wb) = if self.max {
                (bytes(rows.iter().map(|r| r.1).max().unwrap_or(0)), bytes(rows.iter().map(|r| u64::from(r.2)).max().unwrap_or(0)))
            } else {
                (5, 2)
            };
            let row_len = 1 + wa + wb;
            let mut data = Vec::with_capacity(size as usize * (row_len + 1));
            for &(kind, a, b) in &rows {
                if self.max {
                    data.push(0); // PNG row filter, set below
                }
                data.push(kind);
                data.extend_from_slice(&a.to_be_bytes()[8 - wa..]);
                data.extend_from_slice(&u64::from(b).to_be_bytes()[8 - wb..]);
            }
            let mut dict = trailer;
            dict.set(b"Type".to_vec(), Object::Name(b"XRef".to_vec()));
            let w = |n: usize| Object::Integer(n as i64);
            dict.set(b"W".to_vec(), Object::Array(vec![w(1), w(wa), w(wb)]));
            dict.set(b"Filter".to_vec(), Object::Name(b"FlateDecode".to_vec()));
            if self.max {
                // PNG "Up" prediction: consecutive entries differ little.
                for r in (1..rows.len()).rev() {
                    let (prev, cur) = data.split_at_mut(r * (row_len + 1));
                    let prev = &prev[(r - 1) * (row_len + 1)..];
                    cur[0] = 2;
                    for i in 1..=row_len {
                        cur[i] = cur[i].wrapping_sub(prev[i]);
                    }
                }
                if let Some(first) = data.first_mut() {
                    *first = 2; // Up from an all-zero row: unchanged
                }
                let mut parms = Dictionary::new();
                parms.set(b"Predictor".to_vec(), Object::Integer(12));
                parms.set(b"Columns".to_vec(), w(row_len));
                dict.set(b"DecodeParms".to_vec(), Object::Dictionary(parms));
            }
            let stream = Stream { dict, raw: deflate(&data, self.max)?.into() };
            write_object(&mut self.w, xref_ref, &Object::Stream(stream))?;
            self.w.write_all(format!("startxref\n{xref_at}\n%%EOF\n").as_bytes())?;
        } else {
            self.w.write_all(format!("xref\n0 {size}\n").as_bytes())?;
            for i in 0..size {
                let line = match self.entries.get(&i) {
                    Some(Entry::InFile { generation, offset }) => format!("{offset:010} {generation:05} n\r\n"),
                    _ => format!("{:010} {:05} f\r\n", next_free[i as usize], if i == 0 { 65535 } else { 0 }),
                };
                self.w.write_all(line.as_bytes())?;
            }
            let trailer = Object::Dictionary(trailer);
            self.w.write_all(format!("trailer\n{trailer}\nstartxref\n{xref_at}\n%%EOF\n").as_bytes())?;
        }
        self.w.inner.flush()?;
        Ok(())
    }
}

/// Zlib-compressed `data`; with `max`, as small as libdeflate can make it.
fn deflate(data: &[u8], max: bool) -> io::Result<Vec<u8>> {
    #[cfg(feature = "compress")]
    {
        Ok(if max { crate::compress::deflate_max(data) } else { crate::compress::deflate_fast(data) })
    }
    #[cfg(not(feature = "compress"))]
    {
        let _ = max;
        let mut enc = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
        enc.write_all(data)?;
        enc.finish()
    }
}

/// Inner `/Pages` nodes of the existing page tree (everything except the
/// root, which is reused).
/// Removes form fields whose widgets are all on pages left out of the
/// output (deleted pages), so their names and values don't stay in the
/// file; fields with widgets on kept pages lose just the others.
struct FieldPruner<'a> {
    doc: &'a Document,
    changes: &'a Changes,
    dropped: &'a HashSet<u32>,
    /// Annotations of the dropped pages.
    gone: HashSet<u32>,
    seen: HashSet<u32>,
}

impl<'a> FieldPruner<'a> {
    fn new(doc: &'a Document, changes: &'a Changes, dropped: &'a HashSet<u32>, pages: &[Page]) -> Self {
        let mut gone = HashSet::default();
        for page in pages.iter().filter(|p| dropped.contains(&p.id.num)) {
            if let Ok(Object::Array(annots)) = doc.get_resolved(&page.dict, b"Annots") {
                gone.extend(annots.iter().filter_map(Object::as_reference).map(|r| r.num));
            }
        }
        FieldPruner { doc, changes, dropped, gone, seen: HashSet::default() }
    }

    /// An object as it will be written (with the edits).
    fn current(&self, r: ObjRef, replace: &HashMap<u32, (ObjRef, Object)>) -> Option<Object> {
        replace
            .get(&r.num)
            .or(self.changes.replace.get(&r.num))
            .map(|(_, o)| o.clone())
            .or_else(|| self.changes.added.iter().find(|(a, _)| a.num == r.num).map(|(_, o)| o.clone()))
            .or_else(|| self.doc.get_object(r).ok())
    }

    fn prune(&mut self, catalog_ref: ObjRef, replace: &mut HashMap<u32, (ObjRef, Object)>) {
        let Some(Object::Dictionary(mut catalog)) = self.current(catalog_ref, replace) else { return };
        let (acro_ref, acro) = match catalog.get(b"AcroForm") {
            Some(Object::Reference(r)) => (Some(*r), self.current(*r, replace).and_then(|o| o.as_dict().cloned())),
            Some(Object::Dictionary(d)) => (None, Some(d.clone())),
            _ => return,
        };
        let Some(mut acro) = acro else { return };
        let Some(fields) = acro.get(b"Fields").and_then(|f| self.doc.resolve(f).ok()).and_then(|f| f.as_array().map(<[Object]>::to_vec))
        else {
            return;
        };
        let kept: Vec<Object> = fields.iter().filter(|f| self.keep(f, replace, 0)).cloned().collect();
        if kept.len() == fields.len() {
            return;
        }
        acro.set(b"Fields".to_vec(), Object::Array(kept));
        match acro_ref {
            Some(r) => {
                replace.insert(r.num, (r, Object::Dictionary(acro)));
            }
            None => {
                catalog.set(b"AcroForm".to_vec(), Object::Dictionary(acro));
                replace.insert(catalog_ref.num, (catalog_ref, Object::Dictionary(catalog)));
            }
        }
    }

    /// Whether a field (or widget) keeps anything on a kept page; trims
    /// its kids as needed.
    fn keep(&mut self, field: &Object, replace: &mut HashMap<u32, (ObjRef, Object)>, depth: usize) -> bool {
        let Object::Reference(r) = field else { return true };
        if self.gone.contains(&r.num) {
            return false;
        }
        if depth > 32 || !self.seen.insert(r.num) {
            return true;
        }
        let Some(Object::Dictionary(mut dict)) = self.current(*r, replace) else { return true };
        let kids = dict.get(b"Kids").and_then(|k| self.doc.resolve(k).ok()).and_then(|k| k.as_array().map(<[Object]>::to_vec));
        let Some(kids) = kids else {
            // A widget: gone if it is on a dropped page.
            return !dict.get(b"P").and_then(Object::as_reference).is_some_and(|p| self.dropped.contains(&p.num));
        };
        let kept: Vec<Object> = kids.iter().filter(|k| self.keep(k, replace, depth + 1)).cloned().collect();
        if kept.is_empty() && !kids.is_empty() {
            return false;
        }
        if kept.len() != kids.len() {
            dict.set(b"Kids".to_vec(), Object::Array(kept));
            replace.insert(r.num, (*r, Object::Dictionary(dict)));
        }
        true
    }
}

fn old_tree_nodes(doc: &Document, catalog_ref: &ObjRef, root: ObjRef) -> HashSet<u32> {
    let mut nodes = HashSet::default();
    let mut stack = vec![Object::Reference(root)];
    let mut visited = HashSet::default();
    while let Some(node) = stack.pop() {
        let Some(r) = node.as_reference() else { continue };
        if !visited.insert(r.num) || visited.len() > 1_000_000 {
            continue;
        }
        let Some(dict) = doc.resolve(&node).ok().and_then(|o| o.as_dict().cloned()) else { continue };
        let Ok(Object::Array(kids)) = doc.get_resolved(&dict, b"Kids") else { continue };
        if dict.type_name() == Some(b"Page") {
            continue;
        }
        if r != root && r != *catalog_ref {
            nodes.insert(r.num);
        }
        stack.extend(kids);
    }
    nodes
}

/// Object streams, xref streams and linearization dictionaries describe
/// the old file's layout and must not be copied.
fn is_file_structure(obj: &Object) -> bool {
    let dict = match obj {
        Object::Stream(s) => &s.dict,
        Object::Dictionary(d) => d,
        _ => return false,
    };
    matches!(dict.type_name(), Some(b"ObjStm") | Some(b"XRef")) || dict.contains(b"Linearized")
}

fn write_object(w: &mut impl Write, r: ObjRef, obj: &Object) -> io::Result<()> {
    writeln!(w, "{} {} obj", r.num, r.generation)?;
    match obj {
        Object::Stream(s) => {
            let mut dict = s.dict.clone();
            dict.set(b"Length".to_vec(), Object::Integer(s.raw.len() as i64));
            let mut head = Object::Dictionary(dict).to_bytes();
            head.extend_from_slice(b"\nstream\n");
            w.write_all(&head)?;
            w.write_all(&s.raw)?;
            w.write_all(b"\nendstream")?;
        }
        other => w.write_all(&other.to_bytes())?,
    }
    w.write_all(b"\nendobj\n")
}

struct CountingWriter<W: Write> {
    inner: io::BufWriter<W>,
    written: u64,
}

impl<W: Write> Write for CountingWriter<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let n = self.inner.write(buf)?;
        self.written += n as u64;
        Ok(n)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}
