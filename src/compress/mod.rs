//! Lossless compression: making a file smaller without changing anything
//! it shows or contains.
//!
//! [`compress`] rewrites a document with:
//!
//! - every stream re-encoded as tightly as possible: the smallest of
//!   Deflate at libdeflate's highest level, Deflate after PNG row
//!   prediction (for images), or no compression (for tiny streams); a
//!   stream is replaced only when the result is smaller. Older encodings
//!   (LZW, ASCII85, ASCIIHex, RunLength) are converted along the way.
//!   Streams that don't decode cleanly, and XMP metadata (PDF/A keeps it
//!   uncompressed), are left as they are;
//! - JPEG images optimized losslessly with MozJPEG: the same DCT
//!   coefficients with optimal Huffman tables or progressive scans,
//!   verified to decode to identical pixels;
//! - identical objects and streams merged into one, repeatedly (so objects
//!   that differ only by referring to duplicates merge too);
//! - only objects still in use written, packed into large object streams
//!   with a predicted cross-reference stream.
//!
//! Every reader can open the result: it uses only standard filters.
//!
//! With [`CompressOptions::brotli`], streams may instead use Brotli, the
//! PDF Association's 2026 extension to PDF 2.0 (`BrotliDecode`). That
//! gives smaller files, but **readers without Brotli support can't read
//! them**: the file declares PDF 2.0 and the extension so such readers
//! can at least tell why.

#[cfg(feature = "compress")]
pub(crate) mod jpeg;

use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::io::Write;

use crate::document::Document;
use crate::error::{Error, Result};
use crate::object::{Dictionary, ObjRef, Object, Stream};
use crate::writer::{Changes, WriteOptions};
use crate::xref::XrefEntry;

/// What to use when compressing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CompressOptions {
    /// Optimize JPEG images losslessly (MozJPEG). On by default.
    pub jpeg: bool,
    /// Allow Brotli compression (PDF 2.0 extension, 2026). Smaller, but
    /// many readers can't open such files. Off by default.
    pub brotli: bool,
}

impl Default for CompressOptions {
    fn default() -> Self {
        CompressOptions { jpeg: true, brotli: false }
    }
}

/// What [`compress`] did.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct CompressReport {
    /// Streams made smaller (of every kind).
    pub streams: usize,
    /// JPEG images among them.
    pub jpegs: usize,
    /// Streams now using Brotli (only with [`CompressOptions::brotli`]).
    pub brotli_streams: usize,
    /// Duplicate objects merged away.
    pub duplicates: usize,
    /// Size of the written file in bytes.
    pub size: u64,
}

/// The registered Brotli extension (PDF Association, prefix `PDFa`).
const BROTLI_EXTENSION_URL: &str = "https://pdfa.org/resource/extension-brotli";

/// Writes `doc` compressed losslessly to `out` (see the module docs).
pub fn compress(doc: &Document, options: &CompressOptions, out: impl Write) -> Result<CompressReport> {
    if doc.is_encrypted() {
        return Err(Error::Encrypted);
    }
    let mut report = CompressReport::default();
    let entries = doc.xref_entries();
    let generation = |e: &XrefEntry| if let XrefEntry::InFile { generation, .. } = e { *generation } else { 0 };

    // 1. Every object, with streams re-encoded.
    let mut objects: HashMap<u32, (ObjRef, Object)> = HashMap::new();
    let mut recompressed: HashSet<u32> = HashSet::new();
    for (num, entry) in &entries {
        let r = ObjRef::new(*num, generation(entry));
        let Ok(obj) = doc.get_object(r) else { continue };
        if is_file_structure(&obj) {
            continue;
        }
        let obj = match obj {
            Object::Stream(s) => match recompress(doc, &s, options) {
                Some((new, how)) => {
                    report.streams += 1;
                    report.jpegs += (how == How::Jpeg) as usize;
                    report.brotli_streams += (how == How::Brotli) as usize;
                    recompressed.insert(*num);
                    Object::Stream(new)
                }
                None => Object::Stream(s),
            },
            other => other,
        };
        objects.insert(*num, (r, obj));
    }

    // 2. Merge duplicates, until nothing more merges. The catalog and
    // document info stay themselves.
    let trailer = doc.trailer();
    let roots: HashSet<u32> =
        [&b"Root"[..], b"Info"].iter().filter_map(|k| trailer.get(k).and_then(Object::as_reference)).map(|r| r.num).collect();
    let canonical = merge_duplicates(&objects, &roots);
    report.duplicates = canonical.len();
    let canonical: HashMap<u32, ObjRef> = canonical.into_iter().map(|(from, to)| (from, objects[&to].0)).collect();

    // 3. The changes: re-encoded streams, and objects whose references to
    // duplicates now point to the one kept (the duplicates themselves are
    // no longer used, so they aren't written).
    let mut changes = Changes::default();
    for (num, (r, obj)) in &objects {
        let mut new = obj.clone();
        let remapped = !canonical.is_empty() && remap(&mut new, &canonical);
        if remapped || recompressed.contains(num) {
            changes.replace.insert(*num, (*r, new));
        }
    }

    // 4. Brotli files declare PDF 2.0 and the extension.
    let mut write_options = WriteOptions { max_compression: true, version: None };
    if report.brotli_streams > 0 {
        let catalog_ref = doc.trailer().get(b"Root").and_then(Object::as_reference);
        if let Some(cr) = catalog_ref {
            let mut catalog = match changes.replace.get(&cr.num) {
                Some((_, Object::Dictionary(d))) => d.clone(),
                _ => doc.catalog()?,
            };
            declare_brotli(doc, &mut catalog);
            changes.replace.insert(cr.num, (cr, Object::Dictionary(catalog)));
        }
        write_options.version = Some("2.0".into());
    }

    doc.set_overlay(&changes);
    let pages: Vec<(usize, usize)> = (0..doc.page_count()?).map(|p| (0, p)).collect();
    let mut counter = CountingWriter { inner: out, written: 0 };
    let result = crate::writer::write_document_with_options(&[doc], &pages, &changes, &write_options, &mut counter);
    doc.set_overlay(&Changes::default());
    result?;
    report.size = counter.written;
    Ok(report)
}

/// Zlib data as small as libdeflate can make it (level 12).
pub(crate) fn deflate_max(data: &[u8]) -> Vec<u8> {
    let mut c = libdeflater::Compressor::new(libdeflater::CompressionLvl::best());
    let mut out = vec![0; c.zlib_compress_bound(data.len())];
    let n = c.zlib_compress(data, &mut out).expect("buffer is large enough");
    out.truncate(n);
    out
}

fn brotli_max(data: &[u8]) -> Vec<u8> {
    let params = brotli::enc::BrotliEncoderParams {
        quality: 11,
        lgwin: if data.len() > 1 << 22 { 24 } else { 22 },
        size_hint: data.len(),
        ..Default::default()
    };
    let mut out = Vec::new();
    // Writing to a Vec can't fail.
    let _ = brotli::BrotliCompress(&mut &data[..], &mut out, &params);
    out
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum How {
    Deflate,
    Jpeg,
    Brotli,
    Other,
}

/// The smallest lossless re-encoding of stream `s`, if it is smaller than
/// it is now.
fn recompress(doc: &Document, s: &Stream, options: &CompressOptions) -> Option<(Stream, How)> {
    let dict = &s.dict;
    // XMP metadata stays readable as it is (PDF/A requires that); external
    // stream data (`/F`) isn't in the file.
    if dict.type_name() == Some(b"Metadata") || dict.contains(b"F") {
        return None;
    }
    let decoded = doc.decode_stream_strict(s).ok()?;
    let mut base = dict.clone();
    for key in [&b"Filter"[..], b"DecodeParms", b"Length", b"DL"] {
        base.remove(key);
    }
    let make = |raw: Vec<u8>, filter: Option<&[u8]>, parms: Option<Object>| {
        let mut d = base.clone();
        if let Some(f) = filter {
            d.set(b"Filter".to_vec(), Object::Name(f.to_vec()));
        }
        if let Some(p) = parms {
            d.set(b"DecodeParms".to_vec(), p);
        }
        Stream { dict: d, raw }
    };

    let mut best: Option<(Stream, How)> = None;
    let mut consider = |candidate: Stream, how: How| {
        if candidate.raw.len() < best.as_ref().map_or(s.raw.len(), |(b, _)| b.raw.len()) {
            best = Some((candidate, how));
        }
    };

    match decoded.remaining_filter {
        // Image codecs: keep the codec, drop any wrappers around it, and
        // optimize JPEGs.
        Some(codec) => {
            let parms = codec_parms(doc, dict);
            if options.jpeg
                && (codec == b"DCTDecode" || codec == b"DCT")
                && let Some(jpeg) = jpeg::optimize(&decoded.data)
            {
                consider(make(jpeg, Some(b"DCTDecode"), parms.clone()), How::Jpeg);
            }
            consider(make(decoded.data, Some(&codec), parms), How::Other);
        }
        None => {
            let data = decoded.data;
            consider(make(deflate_max(&data), Some(b"FlateDecode"), None), How::Deflate);
            if let Some((predicted, parms)) = png_predict(doc, dict, &data) {
                consider(make(deflate_max(&predicted), Some(b"FlateDecode"), Some(parms)), How::Deflate);
            }
            if options.brotli {
                consider(make(brotli_max(&data), Some(b"BrotliDecode"), None), How::Brotli);
            }
            if data.len() < 64 {
                consider(make(data, None, None), How::Other);
            }
        }
    }
    best
}

/// The `DecodeParms` that go with the last filter (the image codec).
fn codec_parms(doc: &Document, dict: &Dictionary) -> Option<Object> {
    let filters = doc.get_resolved(dict, b"Filter").ok()?;
    let parms = doc.get_resolved(dict, b"DecodeParms").ok()?;
    let p = match (filters, parms) {
        (Object::Array(f), Object::Array(p)) => doc.resolve(p.get(f.len().checked_sub(1)?)?).ok()?,
        (Object::Name(_), p) => p,
        _ => return None,
    };
    matches!(p, Object::Dictionary(_)).then_some(p)
}

/// Image samples with PNG row prediction (each row's best filter, as PNG
/// encoders choose it), and the matching `DecodeParms`. `None` for
/// streams that aren't images whose layout is known.
fn png_predict(doc: &Document, dict: &Dictionary, data: &[u8]) -> Option<(Vec<u8>, Object)> {
    if dict.get(b"Subtype").and_then(Object::as_name) != Some(b"Image") {
        return None;
    }
    let int = |k: &[u8]| doc.get_resolved(dict, k).ok().and_then(|o| o.as_i64());
    let mask = doc.get_resolved(dict, b"ImageMask").ok().and_then(|o| o.as_bool()).unwrap_or(false);
    let width = int(b"Width")?.max(1) as usize;
    let height = int(b"Height")?.max(1) as usize;
    let (colors, bpc) = if mask {
        (1, 1)
    } else {
        (components(doc, dict.get(b"ColorSpace")?)?, int(b"BitsPerComponent").unwrap_or(8) as usize)
    };
    if ![1, 2, 4, 8, 16].contains(&bpc) {
        return None;
    }
    let row_len = (colors * bpc * width).div_ceil(8);
    if row_len * height != data.len() || height < 2 {
        return None;
    }
    let bpp = (colors * bpc).div_ceil(8);
    let mut out = Vec::with_capacity(data.len() + height);
    let zero = vec![0u8; row_len];
    let mut candidate = vec![0u8; row_len];
    let mut best_row = vec![0u8; row_len];
    for y in 0..height {
        let row = &data[y * row_len..(y + 1) * row_len];
        let prev = if y == 0 { &zero[..] } else { &data[(y - 1) * row_len..y * row_len] };
        let mut best = (u64::MAX, 0u8);
        for filter in 0..5u8 {
            for i in 0..row_len {
                let a = if i >= bpp { row[i - bpp] } else { 0 };
                let b = prev[i];
                let c = if i >= bpp { prev[i - bpp] } else { 0 };
                candidate[i] = row[i].wrapping_sub(match filter {
                    0 => 0,
                    1 => a,
                    2 => b,
                    3 => ((a as u16 + b as u16) / 2) as u8,
                    _ => paeth(a, b, c),
                });
            }
            // The usual heuristic: smallest sum of the bytes as signed.
            let cost: u64 = candidate.iter().map(|&v| (v as i8).unsigned_abs() as u64).sum();
            if cost < best.0 {
                best = (cost, filter);
                best_row.copy_from_slice(&candidate);
            }
        }
        out.push(best.1);
        out.extend_from_slice(&best_row);
    }
    let mut parms = Dictionary::new();
    let n = |v: usize| Object::Integer(v as i64);
    parms.set(b"Predictor".to_vec(), n(15));
    parms.set(b"Colors".to_vec(), n(colors));
    parms.set(b"BitsPerComponent".to_vec(), n(bpc));
    parms.set(b"Columns".to_vec(), n(width));
    Some((out, Object::Dictionary(parms)))
}

fn paeth(a: u8, b: u8, c: u8) -> u8 {
    let p = a as i16 + b as i16 - c as i16;
    let (pa, pb, pc) = ((p - a as i16).abs(), (p - b as i16).abs(), (p - c as i16).abs());
    if pa <= pb && pa <= pc {
        a
    } else if pb <= pc {
        b
    } else {
        c
    }
}

/// Number of color components of a color space, when known.
fn components(doc: &Document, cs: &Object) -> Option<usize> {
    match doc.resolve(cs).ok()? {
        Object::Name(n) => match n.as_slice() {
            b"DeviceGray" | b"G" | b"CalGray" => Some(1),
            b"DeviceRGB" | b"RGB" | b"CalRGB" => Some(3),
            b"DeviceCMYK" | b"CMYK" => Some(4),
            _ => None,
        },
        Object::Array(a) => match a.first().and_then(Object::as_name)? {
            b"Indexed" | b"I" | b"Separation" => Some(1),
            b"CalGray" => Some(1),
            b"CalRGB" | b"Lab" => Some(3),
            b"DeviceN" => Some(doc.resolve(a.get(1)?).ok()?.as_array()?.len()),
            b"ICCBased" => match doc.resolve(a.get(1)?).ok()? {
                Object::Stream(s) => doc.get_resolved(&s.dict, b"N").ok()?.as_i64().map(|n| n as usize),
                _ => None,
            },
            _ => None,
        },
        _ => None,
    }
}

/// Objects whose identity matters (pages, annotations, form fields,
/// outline and structure items, layers…): never merged, even when equal.
fn mergeable(obj: &Object) -> bool {
    let dict = match obj {
        Object::Dictionary(d) => d,
        Object::Stream(s) => &s.dict,
        _ => return true,
    };
    const LINKS: [&[u8]; 9] = [b"Parent", b"P", b"Kids", b"Next", b"Prev", b"First", b"Last", b"FT", b"T"];
    if LINKS.iter().any(|k| dict.contains(k)) {
        return false;
    }
    !matches!(
        dict.type_name(),
        Some(
            b"Catalog" | b"Pages" | b"Page" | b"Annot" | b"StructTreeRoot" | b"StructElem" | b"OCG" | b"OCMD"
                | b"Sig" | b"Encrypt" | b"ObjStm" | b"XRef"
        )
    ) && dict.get(b"Subtype").and_then(Object::as_name).is_none_or(|s| s != b"Widget" && s != b"Link")
}

/// Finds duplicate objects: object number → the number of the equal object
/// kept. Repeats until stable, as merging can make more objects equal.
fn merge_duplicates(objects: &HashMap<u32, (ObjRef, Object)>, keep: &HashSet<u32>) -> HashMap<u32, u32> {
    let mut canonical: HashMap<u32, ObjRef> = HashMap::new();
    let mut nums: Vec<u32> =
        objects.iter().filter(|(n, (_, o))| !keep.contains(n) && mergeable(o)).map(|(n, _)| *n).collect();
    nums.sort_unstable();
    for _ in 0..16 {
        // Serialized form with references through `canonical`, by hash.
        let key = |num: u32| -> Vec<u8> {
            let mut obj = objects[&num].1.clone();
            remap(&mut obj, &canonical);
            match obj {
                Object::Stream(s) => {
                    let mut k = Object::Dictionary(s.dict).to_string().into_bytes();
                    k.extend_from_slice(b"\0stream\0");
                    k.extend_from_slice(&s.raw);
                    k
                }
                o => o.to_string().into_bytes(),
            }
        };
        let mut groups: HashMap<u64, Vec<u32>> = HashMap::new();
        for &n in &nums {
            if canonical.contains_key(&n) {
                continue;
            }
            let mut h = std::collections::hash_map::DefaultHasher::new();
            key(n).hash(&mut h);
            groups.entry(h.finish()).or_default().push(n);
        }
        // Merges found in this pass (keys all use the same `canonical`).
        let mut merges: Vec<(u32, u32)> = Vec::new();
        for group in groups.values().filter(|g| g.len() > 1) {
            // Equal hashes: compare the real bytes.
            let mut kept: Vec<(u32, Vec<u8>)> = Vec::new();
            let mut found: Vec<(u32, u32)> = Vec::new();
            for &n in group {
                let k = key(n);
                match kept.iter().find(|(_, kk)| *kk == k) {
                    Some(&(to, _)) => found.push((n, to)),
                    None => kept.push((n, k)),
                }
            }
            merges.extend(found);
        }
        if merges.is_empty() {
            break;
        }
        for (from, to) in merges {
            canonical.insert(from, objects[&to].0);
        }
    }
    // A later pass can merge an object others were merged into: follow
    // each chain to the object finally kept.
    let resolve = |mut n: u32| {
        while let Some(to) = canonical.get(&n) {
            n = to.num;
        }
        n
    };
    canonical.keys().map(|&from| (from, resolve(from))).collect()
}

/// Points references to merged objects at the one kept. Returns whether
/// anything changed.
fn remap(obj: &mut Object, canonical: &HashMap<u32, ObjRef>) -> bool {
    match obj {
        Object::Reference(r) => match canonical.get(&r.num) {
            Some(&to) => {
                *r = to;
                true
            }
            None => false,
        },
        Object::Array(items) => items.iter_mut().fold(false, |c, o| remap(o, canonical) | c),
        Object::Dictionary(d) => remap_dict(d, canonical),
        Object::Stream(s) => remap_dict(&mut s.dict, canonical),
        _ => false,
    }
}

fn remap_dict(d: &mut Dictionary, canonical: &HashMap<u32, ObjRef>) -> bool {
    let mut changed = false;
    let keys: Vec<Vec<u8>> = d.iter().map(|(k, _)| k.to_vec()).collect();
    for k in keys {
        if let Some(mut v) = d.get(&k).cloned()
            && remap(&mut v, canonical)
        {
            d.set(k, v);
            changed = true;
        }
    }
    changed
}

/// Adds the Brotli extension to the catalog's `/Extensions` (keeping any
/// others), and sets its `/Version` to 2.0.
fn declare_brotli(doc: &Document, catalog: &mut Dictionary) {
    let mut ext = Dictionary::new();
    ext.set(b"Type".to_vec(), Object::Name(b"DeveloperExtensions".to_vec()));
    ext.set(b"BaseVersion".to_vec(), Object::Name(b"2.0".to_vec()));
    ext.set(b"ExtensionLevel".to_vec(), Object::Integer(1));
    ext.set(b"ExtensionRevision".to_vec(), Object::String(b"2026".to_vec()));
    ext.set(b"URL".to_vec(), Object::String(BROTLI_EXTENSION_URL.as_bytes().to_vec()));
    let mut extensions = doc.get_dict(catalog, b"Extensions").unwrap_or_default();
    let mut list: Vec<Object> = match extensions.get(b"PDFa").map(|o| doc.resolve(o)) {
        Some(Ok(Object::Array(a))) => a,
        Some(Ok(d @ Object::Dictionary(_))) => vec![d],
        _ => Vec::new(),
    };
    let is_brotli = |o: &Object| {
        doc.resolve(o).ok().and_then(|o| o.as_dict().and_then(|d| d.get(b"URL").cloned()))
            == Some(Object::String(BROTLI_EXTENSION_URL.as_bytes().to_vec()))
    };
    if !list.iter().any(is_brotli) {
        list.push(Object::Dictionary(ext));
    }
    extensions.set(b"PDFa".to_vec(), Object::Array(list));
    catalog.set(b"Extensions".to_vec(), Object::Dictionary(extensions));
    catalog.set(b"Version".to_vec(), Object::Name(b"2.0".to_vec()));
}

fn is_file_structure(obj: &Object) -> bool {
    matches!(obj, Object::Stream(s) if matches!(s.dict.type_name(), Some(b"ObjStm" | b"XRef")))
}

struct CountingWriter<W: Write> {
    inner: W,
    written: u64,
}

impl<W: Write> Write for CountingWriter<W> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let n = self.inner.write(buf)?;
        self.written += n as u64;
        Ok(n)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}
