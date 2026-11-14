//! Lossless compression: making a file smaller without changing anything
//! it shows or contains.
//!
//! [`compress`] rewrites a document with:
//!
//! - every stream re-encoded: the smallest of Deflate, Deflate after PNG
//!   row prediction (for images), or no compression (for tiny streams); a
//!   stream is replaced only when the result is smaller. Older encodings
//!   (LZW, ASCII85, ASCIIHex, RunLength) are converted along the way.
//!   Streams that don't decode cleanly, and XMP metadata (PDF/A keeps it
//!   uncompressed), are left as they are. Streams are decoded and
//!   re-encoded on all cores at once;
//! - with [`Effort::Best`], Deflate at libdeflate's highest level (several
//!   times slower than [`Effort::Fast`]'s level for a few percent more),
//!   and JPEG images optimized losslessly with MozJPEG: the same DCT
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
use std::hash::Hasher;
use std::io::Write;

use crate::document::Document;
use crate::error::Result;
use crate::object::{Dictionary, ObjRef, Object, Stream};
use crate::writer::{Changes, WriteOptions};
use crate::xref::XrefEntry;

/// How hard to try.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Effort {
    /// A middle Deflate level, no JPEG optimization: fast (faster than
    /// MuPDF's `mutool clean` on most files) and still smaller.
    #[default]
    Fast,
    /// Deflate at libdeflate's highest level, and JPEGs optimized (when
    /// [`CompressOptions::jpeg`] is on): the smallest files, several times
    /// slower.
    Best,
}

/// What to use when compressing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CompressOptions {
    /// Optimize JPEG images losslessly (MozJPEG), with [`Effort::Best`].
    /// On by default.
    pub jpeg: bool,
    /// Allow Brotli compression (PDF 2.0 extension, 2026). Smaller, but
    /// many readers can't open such files. Off by default.
    pub brotli: bool,
    /// How hard to try. [`Effort::Fast`] by default.
    pub effort: Effort,
}

impl Default for CompressOptions {
    fn default() -> Self {
        CompressOptions { jpeg: true, brotli: false, effort: Effort::Fast }
    }
}

impl CompressOptions {
    /// The smallest result, however long it takes.
    pub fn best() -> Self {
        CompressOptions { effort: Effort::Best, ..Self::default() }
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
    // Every object is read twice (here and by the writer): keep the object
    // streams decoded for both.
    doc.with_object_cache(64 << 20, || compress_document(doc, options, out))
}

fn compress_document(doc: &Document, options: &CompressOptions, out: impl Write) -> Result<CompressReport> {
    let mut report = CompressReport::default();
    let entries = doc.xref_entries();
    let generation = |e: &XrefEntry| if let XrefEntry::InFile { generation, .. } = e { *generation } else { 0 };

    // 1. Every object. Streams are prepared here (everything that needs
    // the document), then decoded and re-encoded on all cores.
    let mut objects: Objects = Objects::default();
    let mut recompressed: HashSet<u32> = HashSet::new();
    let mut jobs: Vec<Job> = Vec::new();
    {
        // Objects in object streams are read on all cores first.
        let mut in_streams: crate::hash::HashMap<u32, Object> = doc.objects_in_streams().into_iter().collect();
        objects.reserve(entries.len());
        for (num, entry) in &entries {
            let r = ObjRef::new(*num, generation(entry));
            let obj = match in_streams.remove(num) {
                Some(obj) => obj,
                None => {
                    let Ok(obj) = doc.get_object(r) else { continue };
                    obj
                }
            };
            if is_file_structure(&obj) {
                continue;
            }
            if let Object::Stream(s) = &obj
                && let Some(job) = prepare(doc, *num, s)
            {
                jobs.push(job);
            }
            objects.insert(*num, (r, obj));
        }
    }
    for (num, (new, how)) in recompress_all(jobs, options) {
        report.streams += 1;
        report.jpegs += (how == How::Jpeg) as usize;
        report.brotli_streams += (how == How::Brotli) as usize;
        recompressed.insert(num);
        if let Some(entry) = objects.get_mut(&num) {
            entry.1 = Object::Stream(new);
        }
    }

    // 2. Merge duplicates, until nothing more merges. The catalog and
    // document info stay themselves.
    let trailer = doc.trailer();
    let roots: HashSet<u32> =
        [&b"Root"[..], b"Info"].iter().filter_map(|k| trailer.get(k).and_then(Object::as_reference)).map(|r| r.num).collect();
    let merges = merge_duplicates(&objects, &roots);
    let canonical = merges.canonical;
    report.duplicates = canonical.len();
    let canonical: HashMap<u32, ObjRef> = canonical.into_iter().map(|(from, to)| (from, objects[&to].0)).collect();

    // 3. The changes: re-encoded streams, and objects whose references to
    // duplicates now point to the one kept (the duplicates themselves are
    // no longer used, so they aren't written).
    let mut changes = Changes::default();
    // The others are handed to the writer as they are, so it doesn't read
    // them again.
    let mut unchanged = crate::writer::Preloaded::default();
    for (num, (r, mut obj)) in objects {
        let remapped = merges.referrers.contains(&num) && remap(&mut obj, &canonical);
        if remapped || recompressed.contains(&num) {
            changes.replace.insert(num, (r, obj));
        } else {
            unchanged.insert(num, obj);
        }
    }

    // 4. Brotli files declare PDF 2.0 and the extension.
    let mut write_options = WriteOptions { max_compression: options.effort == Effort::Best, ..WriteOptions::default() };
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
    let mut counter = CountingWriter { inner: out, written: 0 };
    let result = crate::writer::write_preloaded(doc, &changes, unchanged, &write_options, &mut counter);
    doc.set_overlay(&Changes::default());
    // Freeing a whole document's objects one by one takes a while: do it
    // on the side (a program that exits next doesn't wait for it).
    let garbage = result?;
    std::thread::spawn(move || drop((garbage, changes)));
    report.size = counter.written;
    Ok(report)
}

/// Zlib data as small as libdeflate can make it (level 12).
pub(crate) fn deflate_max(data: &[u8]) -> Vec<u8> {
    let mut c = libdeflater::Compressor::new(libdeflater::CompressionLvl::best());
    zlib(&mut c, &mut Vec::new(), data)
}

/// Zlib data at [`Effort::Fast`]'s level.
pub(crate) fn deflate_fast(data: &[u8]) -> Vec<u8> {
    let mut c = libdeflater::Compressor::new(libdeflater::CompressionLvl::new(FAST_LEVEL).expect("valid level"));
    zlib(&mut c, &mut Vec::new(), data)
}

/// libdeflate's level for [`Effort::Fast`]: about as small as zlib's
/// highest level, and several times faster than libdeflate's (12).
const FAST_LEVEL: i32 = 6;

/// Zlib-compresses `data` with `c`, through `scratch` (kept between calls
/// so the worst-case-sized buffer is allocated once, not per stream), into
/// a vector of exactly the compressed size.
fn zlib(c: &mut libdeflater::Compressor, scratch: &mut Vec<u8>, data: &[u8]) -> Vec<u8> {
    let bound = c.zlib_compress_bound(data.len());
    if scratch.len() < bound {
        scratch.resize(bound, 0);
    }
    let n = c.zlib_compress(data, scratch).expect("buffer is large enough");
    scratch[..n].to_vec()
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

/// Every object of the document, by number.
type Objects = crate::hash::HashMap<u32, (ObjRef, Object)>;

/// A stream to re-encode, with what its encoding needs from the document
/// resolved, so the work can run on other threads.
struct Job {
    num: u32,
    stream: Stream,
    /// The filter entries, resolved (as `Document::decode_stream` uses them).
    filters: Dictionary,
    /// The image codec's `DecodeParms`, if it ends with one.
    codec_parms: Option<Object>,
    /// Image layout for PNG prediction: (colors, bits per component, width, height).
    image: Option<(usize, usize, usize, usize)>,
}

fn prepare(doc: &Document, num: u32, s: &Stream) -> Option<Job> {
    let dict = &s.dict;
    // XMP metadata stays readable as it is (PDF/A requires that); external
    // stream data (`/F`) isn't in the file.
    if dict.type_name() == Some(b"Metadata") || dict.contains(b"F") {
        return None;
    }
    let mut filters = Dictionary::new();
    for key in [&b"Filter"[..], b"DecodeParms", b"DP"] {
        let v = match doc.get_resolved(dict, key).ok()? {
            Object::Array(items) => Object::Array(items.iter().map(|o| doc.resolve(o)).collect::<Result<_>>().ok()?),
            other => other,
        };
        if !v.is_null() {
            filters.set(key.to_vec(), v);
        }
    }
    Some(Job { num, stream: s.clone(), filters, codec_parms: codec_parms(doc, dict), image: image_layout(doc, dict) })
}


/// Re-encodes the jobs on all cores (biggest first, so no thread is left
/// with a large one at the end). Returns the streams that got smaller.
fn recompress_all(mut jobs: Vec<Job>, options: &CompressOptions) -> Vec<(u32, (Stream, How))> {
    jobs.sort_by_key(|j| std::cmp::Reverse(j.stream.raw.len()));
    let mut results = Vec::new();
    let next = std::sync::atomic::AtomicUsize::new(0);
    let threads = std::thread::available_parallelism().map_or(1, |n| n.get()).min(jobs.len()).max(1);
    let work = || {
        let level = match options.effort {
            Effort::Fast => libdeflater::CompressionLvl::new(FAST_LEVEL).expect("valid level"),
            Effort::Best => libdeflater::CompressionLvl::best(),
        };
        let mut c = libdeflater::Compressor::new(level);
        let mut scratch = Vec::new();
        let mut done = Vec::new();
        loop {
            let i = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let Some(job) = jobs.get(i) else { break };
            if let Some(result) = recompress(job, options, &mut c, &mut scratch) {
                done.push((job.num, result));
            }
        }
        done
    };
    if threads == 1 || jobs.len() <= 1 {
        results.extend(work());
        return results;
    }
    std::thread::scope(|s| {
        let handles: Vec<_> = (0..threads).map(|_| s.spawn(work)).collect();
        results.extend(handles.into_iter().flat_map(|h| h.join().expect("compression thread panicked")));
    });
    results
}

/// The smallest lossless re-encoding of the job's stream, if it is
/// smaller than it is now.
fn recompress(
    job: &Job,
    options: &CompressOptions,
    c: &mut libdeflater::Compressor,
    scratch: &mut Vec<u8>,
) -> Option<(Stream, How)> {
    let s = &job.stream;
    let dict = &s.dict;
    let decoded = crate::filters::decode_strict(&s.raw, &job.filters).ok()?;
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
        Stream { dict: d, raw: raw.into() }
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
            let parms = job.codec_parms.clone();
            if options.jpeg
                && options.effort == Effort::Best
                && (codec == b"DCTDecode" || codec == b"DCT")
                && let Some(jpeg) = jpeg::optimize(&decoded.data)
            {
                consider(make(jpeg, Some(b"DCTDecode"), parms.clone()), How::Jpeg);
            }
            consider(make(decoded.data, Some(&codec), parms), How::Other);
        }
        None => {
            let data = decoded.data;
            let mut deflate = |data: &[u8]| zlib(c, scratch, data);
            let predicted = job.image.and_then(|layout| png_predict(layout, &data));
            consider(make(deflate(&data), Some(b"FlateDecode"), None), How::Deflate);
            if let Some((predicted, parms)) = predicted {
                consider(make(deflate(&predicted), Some(b"FlateDecode"), Some(parms)), How::Deflate);
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

/// An image's (colors, bits per component, width, height), when its
/// samples can be PNG-predicted.
fn image_layout(doc: &Document, dict: &Dictionary) -> Option<(usize, usize, usize, usize)> {
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
    Some((colors, bpc, width, height))
}

/// Image samples with PNG row prediction (each row's best filter, as PNG
/// encoders choose it), and the matching `DecodeParms`. `None` if the
/// samples don't match the layout.
fn png_predict((colors, bpc, width, height): (usize, usize, usize, usize), data: &[u8]) -> Option<(Vec<u8>, Object)> {
    let row_len = (colors * bpc * width).div_ceil(8);
    if row_len * height != data.len() || height < 2 {
        return None;
    }
    let bpp = (colors * bpc).div_ceil(8).min(row_len);
    let mut out = Vec::with_capacity(data.len() + height);
    let zero = vec![0u8; row_len];
    // The five filtered versions of a row. Each filter is its own simple
    // loop over whole rows, which the compiler vectorizes (one loop with
    // the filter chosen per byte was most of the time compressing images).
    let mut rows = vec![vec![0u8; row_len]; 5];
    for y in 0..height {
        let row = &data[y * row_len..(y + 1) * row_len];
        let prev = if y == 0 { &zero[..] } else { &data[(y - 1) * row_len..y * row_len] };
        let (left, cur) = (&row[..row_len - bpp], &row[bpp..]);
        let (up_left, up) = (&prev[..row_len - bpp], &prev[bpp..]);
        // None
        rows[0].copy_from_slice(row);
        // Sub: minus the byte to the left.
        rows[1][..bpp].copy_from_slice(&row[..bpp]);
        for ((o, &x), &l) in rows[1][bpp..].iter_mut().zip(cur).zip(left) {
            *o = x.wrapping_sub(l);
        }
        // Up: minus the byte above.
        for ((o, &x), &u) in rows[2].iter_mut().zip(row).zip(prev) {
            *o = x.wrapping_sub(u);
        }
        // Average of left and above.
        for ((o, &x), &u) in rows[3][..bpp].iter_mut().zip(&row[..bpp]).zip(&prev[..bpp]) {
            *o = x.wrapping_sub(u / 2);
        }
        for (((o, &x), &l), &u) in rows[3][bpp..].iter_mut().zip(cur).zip(left).zip(up) {
            *o = x.wrapping_sub(((l as u16 + u as u16) / 2) as u8);
        }
        // Paeth (with nothing to the left, it predicts the byte above).
        for ((o, &x), &u) in rows[4][..bpp].iter_mut().zip(&row[..bpp]).zip(&prev[..bpp]) {
            *o = x.wrapping_sub(u);
        }
        for ((((o, &x), &l), &u), &ul) in rows[4][bpp..].iter_mut().zip(cur).zip(left).zip(up).zip(up_left) {
            *o = x.wrapping_sub(paeth(l, u, ul));
        }
        // The usual heuristic: smallest sum of the bytes as signed; the
        // first filter wins ties.
        let cost = |r: &[u8]| -> u64 { r.iter().map(|&v| (v as i8).unsigned_abs() as u32).sum::<u32>() as u64 };
        let mut best = 0;
        let mut best_cost = cost(&rows[0]);
        for (f, r) in rows.iter().enumerate().skip(1) {
            let c = cost(r);
            if c < best_cost {
                (best, best_cost) = (f, c);
            }
        }
        out.push(best as u8);
        out.extend_from_slice(&rows[best]);
    }
    let mut parms = Dictionary::new();
    let n = |v: usize| Object::Integer(v as i64);
    parms.set(b"Predictor".to_vec(), n(15));
    parms.set(b"Colors".to_vec(), n(colors));
    parms.set(b"BitsPerComponent".to_vec(), n(bpc));
    parms.set(b"Columns".to_vec(), n(width));
    Some((out, Object::Dictionary(parms)))
}

#[inline(always)]
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

/// Duplicate objects found by [`merge_duplicates`].
struct Merges {
    /// Object number → the number of the equal object kept.
    canonical: HashMap<u32, u32>,
    /// Objects that refer to a merged object (and so need their references
    /// pointed at the one kept).
    referrers: HashSet<u32>,
}

/// Finds duplicate objects. Repeats until stable, as merging can make more
/// objects equal: after the first pass only the objects referring to
/// something just merged can change, so only they are hashed again.
/// Objects are hashed and compared as they are, with references read
/// through the merges found so far (no copies or serializing).
fn merge_duplicates(objects: &Objects, keep: &HashSet<u32>) -> Merges {
    let mut canonical: HashMap<u32, ObjRef> = HashMap::new();
    let mut nums: Vec<u32> =
        objects.iter().filter(|(n, (_, o))| !keep.contains(n) && mergeable(o)).map(|(n, _)| *n).collect();
    nums.sort_unstable();
    let is_mergeable: HashSet<u32> = nums.iter().copied().collect();
    // Who refers to whom (any object, mergeable or not); each object's
    // references are listed on all cores.
    let all: Vec<(&u32, &(ObjRef, Object))> = objects.iter().collect();
    let lists = in_parallel(&all, |&(&n, (_, obj))| {
        let mut targets = Vec::new();
        references(obj, &mut targets);
        targets.sort_unstable();
        targets.dedup();
        (n, targets)
    });
    let mut referred_by: crate::hash::HashMap<u32, Vec<u32>> = crate::hash::HashMap::default();
    for (n, targets) in lists {
        for t in targets {
            referred_by.entry(t).or_default().push(n);
        }
    }
    let hash_of = |n: u32, canonical: &HashMap<u32, ObjRef>, data_hash: &crate::hash::HashMap<u32, u64>| {
        let mut h = crate::hash::FxHasher::default();
        match &objects[&n].1 {
            Object::Stream(s) => {
                hash_dict(&s.dict, canonical, &mut h);
                h.write_u64(data_hash[&n]);
            }
            o => hash_object(o, canonical, &mut h),
        }
        h.finish()
    };
    // Stream data is hashed once, not on every pass.
    let data_hash: crate::hash::HashMap<u32, u64> = in_parallel(&nums, |n| match &objects[n].1 {
        Object::Stream(s) => {
            let mut h = crate::hash::FxHasher::default();
            h.write(&s.raw);
            Some((*n, h.finish()))
        }
        _ => None,
    })
    .into_iter()
    .flatten()
    .collect();
    let mut hashes: crate::hash::HashMap<u32, u64> =
        in_parallel(&nums, |&n| (n, hash_of(n, &canonical, &data_hash))).into_iter().collect();
    // Objects whose hash changed in the last pass (at first, all).
    let mut changed: HashSet<u32> = nums.iter().copied().collect();
    for _ in 0..16 {
        let mut groups: crate::hash::HashMap<u64, Vec<u32>> = crate::hash::HashMap::default();
        for &n in &nums {
            if !canonical.contains_key(&n) {
                groups.entry(hashes[&n]).or_default().push(n);
            }
        }
        // Merges found in this pass (comparisons all use the same `canonical`).
        let mut merges: Vec<(u32, u32)> = Vec::new();
        for group in groups.values().filter(|g| g.len() > 1 && g.iter().any(|n| changed.contains(n))) {
            // Equal hashes: compare the objects themselves.
            let mut kept: Vec<u32> = Vec::new();
            for &n in group {
                let obj = &objects[&n].1;
                match kept.iter().find(|&&k| same_object(&objects[&k].1, obj, &canonical)) {
                    Some(&to) => merges.push((n, to)),
                    None => kept.push(n),
                }
            }
        }
        if merges.is_empty() {
            break;
        }
        changed.clear();
        for &(from, to) in &merges {
            canonical.insert(from, objects[&to].0);
            for &holder in referred_by.get(&from).map_or(&[][..], Vec::as_slice) {
                if is_mergeable.contains(&holder) {
                    changed.insert(holder);
                }
            }
        }
        for &n in &changed {
            hashes.insert(n, hash_of(n, &canonical, &data_hash));
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
    let referrers = canonical.keys().flat_map(|from| referred_by.get(from).into_iter().flatten().copied()).collect();
    Merges { canonical: canonical.keys().map(|&from| (from, resolve(from))).collect(), referrers }
}

/// `f` applied to every item, on all cores (in pieces), in order.
fn in_parallel<T: Sync, R: Send>(items: &[T], f: impl Fn(&T) -> R + Sync) -> Vec<R> {
    let threads = std::thread::available_parallelism().map_or(1, |n| n.get());
    let size = items.len().div_ceil(threads).max(4096);
    if items.len() <= size {
        return items.iter().map(f).collect();
    }
    std::thread::scope(|s| {
        let f = &f;
        let handles: Vec<_> = items.chunks(size).map(|c| s.spawn(move || c.iter().map(f).collect::<Vec<R>>())).collect();
        handles.into_iter().flat_map(|h| h.join().expect("thread panicked")).collect()
    })
}

/// The object numbers `obj` refers to.
fn references(obj: &Object, out: &mut Vec<u32>) {
    match obj {
        Object::Reference(r) => out.push(r.num),
        Object::Array(items) => items.iter().for_each(|o| references(o, out)),
        Object::Dictionary(d) => d.iter().for_each(|(_, v)| references(v, out)),
        Object::Stream(s) => s.dict.iter().for_each(|(_, v)| references(v, out)),
        _ => {}
    }
}

/// Where a reference points after the merges so far.
fn mapped(r: ObjRef, canonical: &HashMap<u32, ObjRef>) -> ObjRef {
    canonical.get(&r.num).copied().unwrap_or(r)
}

fn hash_object(o: &Object, canonical: &HashMap<u32, ObjRef>, h: &mut crate::hash::FxHasher) {
    match o {
        Object::Null => h.write_u8(0),
        Object::Bool(b) => h.write_u16(1 << 8 | *b as u16),
        Object::Integer(i) => {
            h.write_u8(2);
            h.write_u64(*i as u64);
        }
        Object::Real(f) => {
            h.write_u8(3);
            h.write_u64(f.to_bits());
        }
        Object::String(s) => {
            h.write_u8(4);
            h.write_usize(s.len());
            h.write(s);
        }
        Object::Name(n) => {
            h.write_u8(5);
            h.write_usize(n.len());
            h.write(n);
        }
        Object::Array(items) => {
            h.write_u8(6);
            h.write_usize(items.len());
            items.iter().for_each(|i| hash_object(i, canonical, h));
        }
        Object::Dictionary(d) => hash_dict(d, canonical, h),
        Object::Stream(s) => {
            h.write_u8(8);
            hash_dict(&s.dict, canonical, h);
            h.write_usize(s.raw.len());
            h.write(&s.raw);
        }
        Object::Reference(r) => {
            let r = mapped(*r, canonical);
            h.write_u8(9);
            h.write_u32(r.num);
            h.write_u16(r.generation);
        }
    }
}

fn hash_dict(d: &Dictionary, canonical: &HashMap<u32, ObjRef>, h: &mut crate::hash::FxHasher) {
    h.write_u8(7);
    h.write_usize(d.len());
    for (k, v) in d.iter() {
        h.write_usize(k.len());
        h.write(k);
        hash_object(v, canonical, h);
    }
}

/// Whether two objects are equal once references go through the merges.
fn same_object(a: &Object, b: &Object, canonical: &HashMap<u32, ObjRef>) -> bool {
    match (a, b) {
        (Object::Array(x), Object::Array(y)) => {
            x.len() == y.len() && x.iter().zip(y).all(|(p, q)| same_object(p, q, canonical))
        }
        (Object::Dictionary(x), Object::Dictionary(y)) => same_dict(x, y, canonical),
        (Object::Stream(x), Object::Stream(y)) => *x.raw == *y.raw && same_dict(&x.dict, &y.dict, canonical),
        (Object::Reference(x), Object::Reference(y)) => mapped(*x, canonical) == mapped(*y, canonical),
        // Reals compare by their bits, as they are written.
        (Object::Real(x), Object::Real(y)) => x.to_bits() == y.to_bits(),
        _ => a == b,
    }
}

fn same_dict(a: &Dictionary, b: &Dictionary, canonical: &HashMap<u32, ObjRef>) -> bool {
    a.len() == b.len() && a.iter().zip(b.iter()).all(|((ka, va), (kb, vb))| ka == kb && same_object(va, vb, canonical))
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
    d.iter_mut().fold(false, |changed, (_, v)| remap(v, canonical) | changed)
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
