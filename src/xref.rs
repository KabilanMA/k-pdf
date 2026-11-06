//! Cross-reference loading (ISO 32000 §7.5.4–7.5.8).
//!
//! Handles, in order of preference:
//! 1. classic `xref` tables (all versions), including incremental updates
//!    chained through `/Prev`;
//! 2. cross-reference streams (PDF 1.5+) and hybrid files (`/XRefStm`);
//! 3. reconstruction by scanning the whole file for `N G obj`, used when
//!    the above is missing or damaged — common with old or truncated files.

use std::collections::{HashMap, HashSet};

use crate::error::{Error, Result};
use crate::filters;
use crate::object::{Dictionary, Object};
use crate::parser::{find, rfind, Lexer, Parser, Token};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum XrefEntry {
    /// Object stored directly in the file at a byte offset.
    InFile { offset: usize, generation: u16 },
    /// Object stored inside object stream `stream`, at position `index`.
    InStream { stream: u32, index: u32 },
}

#[derive(Debug, Default)]
pub struct XrefTable {
    pub entries: HashMap<u32, XrefEntry>,
    /// Merged trailer: newest values win.
    pub trailer: Dictionary,
}

impl XrefTable {
    pub fn get(&self, num: u32) -> Option<XrefEntry> {
        self.entries.get(&num).copied()
    }

    /// Adds an entry unless a newer section already defined this object.
    /// Sections are read newest-first, so the first definition wins.
    /// Free entries are recorded as `None` so older sections can't revive them.
    fn add(&mut self, seen: &mut HashSet<u32>, num: u32, entry: Option<XrefEntry>) {
        if seen.insert(num)
            && let Some(e) = entry {
                self.entries.insert(num, e);
            }
    }
}

/// Upper bound for object numbers (ISO 32000 Annex C), also used to reject
/// absurd `/Size` or `/Index` values from damaged files.
const MAX_OBJECTS: u64 = 8_388_607;

/// Loads the cross-reference information starting from `startxref`.
pub fn load(data: &[u8], header_offset: usize) -> Result<XrefTable> {
    let start = find_startxref(data).ok_or_else(|| Error::parse(data.len(), "startxref not found"))?;
    let mut table = XrefTable::default();
    let mut seen_objects = HashSet::new();
    let mut visited = HashSet::new();
    let mut next = Some(start);

    while let Some(offset) = next.take() {
        if !visited.insert(offset) {
            break; // /Prev loop
        }
        let pos = locate_section(data, offset, header_offset)
            .ok_or_else(|| Error::parse(offset, "no xref section at startxref offset"))?;
        let trailer = read_section(data, pos, &mut table, &mut seen_objects)?;

        // Hybrid file: the classic table is complemented by an xref stream
        // whose entries take precedence over the *older* sections only.
        if let Some(stm) = trailer.get(b"XRefStm").and_then(Object::as_i64)
            && let Some(p) = usize::try_from(stm).ok().and_then(|o| locate_section(data, o, header_offset))
                && visited.insert(p) {
                    // Errors here are not fatal; the classic table may suffice.
                    let _ = read_section(data, p, &mut table, &mut seen_objects);
                }

        next = trailer.get(b"Prev").and_then(Object::as_i64).and_then(|p| usize::try_from(p).ok());
        for (k, v) in trailer.iter() {
            if !table.trailer.contains(k) && !matches!(k, b"Prev" | b"XRefStm") {
                table.trailer.set(k.to_vec(), v.clone());
            }
        }
    }
    if !table.trailer.contains(b"Root") {
        return Err(Error::parse(start, "trailer has no /Root"));
    }
    Ok(table)
}

/// Finds the offset given by the last `startxref` in the file.
fn find_startxref(data: &[u8]) -> Option<usize> {
    // Normally in the last ~1 KB, but tolerate trailing garbage.
    let tail_start = data.len().saturating_sub(64 * 1024);
    let at = rfind(&data[tail_start..], b"startxref")? + tail_start;
    let mut lex = Lexer::new(data, at + b"startxref".len());
    match lex.next_token() {
        Some(Token::Integer(n)) => usize::try_from(n).ok(),
        _ => None,
    }
}

/// Offsets are relative to the `%PDF` header; files with junk prepended
/// (e.g. mail headers) are off by `header_offset`. Accept either.
fn locate_section(data: &[u8], offset: usize, header_offset: usize) -> Option<usize> {
    let looks_ok = |p: usize| -> bool {
        let mut lex = Lexer::new(data, p);
        match lex.next_token() {
            Some(Token::Keyword(b"xref")) => true,
            Some(Token::Integer(_)) => matches!(
                (lex.next_token(), lex.next_token()),
                (Some(Token::Integer(_)), Some(Token::Keyword(b"obj")))
            ),
            _ => false,
        }
    };
    let exact = [offset, offset + header_offset].into_iter().filter(|&p| p < data.len()).find(|&p| looks_ok(p));
    if exact.is_some() {
        return exact;
    }
    // Slightly wrong offsets are a common writer bug: look for a nearby
    // `xref` keyword before giving up (and falling back to a full rescan).
    const WINDOW: usize = 32;
    let lo = offset.saturating_sub(WINDOW).min(data.len());
    let hi = offset.saturating_add(WINDOW).min(data.len());
    find(&data[lo..hi], b"xref").map(|i| lo + i).filter(|&p| looks_ok(p))
}

/// Reads one xref section (table or stream) and returns its trailer.
fn read_section(data: &[u8], pos: usize, table: &mut XrefTable, seen: &mut HashSet<u32>) -> Result<Dictionary> {
    let mut lex = Lexer::new(data, pos);
    if lex.peek() == Some(Token::Keyword(b"xref")) {
        lex.next_token();
        read_classic_table(lex, table, seen)
    } else {
        read_xref_stream(data, pos, table, seen)
    }
}

fn read_classic_table(mut lex: Lexer, table: &mut XrefTable, seen: &mut HashSet<u32>) -> Result<Dictionary> {
    loop {
        match lex.next_token() {
            Some(Token::Keyword(b"trailer")) => break,
            Some(Token::Integer(first)) => {
                let Some(Token::Integer(count)) = lex.next_token() else {
                    return Err(Error::parse(lex.pos(), "bad xref subsection header"));
                };
                let first = u64::try_from(first).map_err(|_| Error::parse(lex.pos(), "negative object number"))?;
                if count < 0 || first + count as u64 > MAX_OBJECTS {
                    return Err(Error::parse(lex.pos(), "xref subsection out of range"));
                }
                for i in 0..count as u64 {
                    let (Some(Token::Integer(offset)), Some(Token::Integer(generation)), Some(Token::Keyword(kind))) =
                        (lex.next_token(), lex.next_token(), lex.next_token())
                    else {
                        return Err(Error::parse(lex.pos(), "bad xref entry"));
                    };
                    let num = (first + i) as u32;
                    let entry = match kind {
                        b"n" if offset > 0 => Some(XrefEntry::InFile {
                            offset: offset as usize,
                            generation: u16::try_from(generation).unwrap_or(0),
                        }),
                        b"n" | b"f" => None,
                        _ => return Err(Error::parse(lex.pos(), "bad xref entry type")),
                    };
                    table.add(seen, num, entry);
                }
            }
            _ => return Err(Error::parse(lex.pos(), "expected xref subsection or 'trailer'")),
        }
    }
    let mut parser = Parser { lex };
    match parser.parse_object()? {
        Object::Dictionary(d) => Ok(d),
        _ => Err(Error::parse(parser.lex.pos(), "trailer is not a dictionary")),
    }
}

fn read_xref_stream(data: &[u8], pos: usize, table: &mut XrefTable, seen: &mut HashSet<u32>) -> Result<Dictionary> {
    // /Length of an xref stream must be direct, so no resolver is needed.
    let (_, obj) = Parser::new(data, pos).parse_indirect(&|_| None)?;
    let Object::Stream(stream) = obj else {
        return Err(Error::parse(pos, "xref offset does not point to an xref stream"));
    };
    if stream.dict.type_name() != Some(b"XRef") {
        return Err(Error::parse(pos, "stream is not /Type /XRef"));
    }
    let decoded = filters::decode(&stream.raw, &stream.dict)?.data;
    parse_xref_stream_entries(&decoded, &stream.dict, table, seen)?;
    Ok(stream.dict)
}

fn parse_xref_stream_entries(
    data: &[u8],
    dict: &Dictionary,
    table: &mut XrefTable,
    seen: &mut HashSet<u32>,
) -> Result<()> {
    let bad = |m: &str| Error::parse(0, format!("xref stream: {m}"));
    let widths: Vec<usize> = dict
        .get(b"W")
        .and_then(Object::as_array)
        .ok_or_else(|| bad("missing /W"))?
        .iter()
        .map(|o| o.as_i64().and_then(|w| usize::try_from(w).ok()).filter(|&w| w <= 8))
        .collect::<Option<_>>()
        .ok_or_else(|| bad("invalid /W"))?;
    if widths.len() != 3 {
        return Err(bad("/W must have 3 entries"));
    }
    let size = dict.get(b"Size").and_then(Object::as_i64).unwrap_or(0);
    let index: Vec<i64> = match dict.get(b"Index").and_then(Object::as_array) {
        Some(a) => a.iter().filter_map(Object::as_i64).collect(),
        None => vec![0, size],
    };

    let row_len: usize = widths.iter().sum();
    if row_len == 0 {
        return Err(bad("zero-width rows"));
    }
    let mut rows = data.chunks_exact(row_len);
    for pair in index.chunks_exact(2) {
        let (first, count) = (pair[0], pair[1]);
        if first < 0 || count < 0 || (first + count) as u64 > MAX_OBJECTS {
            return Err(bad("/Index out of range"));
        }
        for num in first..first + count {
            let Some(row) = rows.next() else { return Ok(()) }; // truncated stream
            let field = |i: usize| -> u64 {
                let start: usize = widths[..i].iter().sum();
                row[start..start + widths[i]].iter().fold(0u64, |acc, &b| acc << 8 | b as u64)
            };
            // A zero-width type field defaults to type 1.
            let kind = if widths[0] == 0 { 1 } else { field(0) };
            let entry = match kind {
                1 => Some(XrefEntry::InFile { offset: field(1) as usize, generation: field(2) as u16 }),
                2 => Some(XrefEntry::InStream { stream: field(1) as u32, index: field(2) as u32 }),
                _ => None, // 0 = free; unknown types are treated as null objects
            };
            table.add(seen, num as u32, entry);
        }
    }
    Ok(())
}

/// Rebuilds the cross-reference table by scanning the file for objects.
/// Later definitions of the same object number win, as in an incremental
/// update.
pub fn reconstruct(data: &[u8]) -> Result<XrefTable> {
    let mut table = XrefTable::default();
    let mut object_streams = Vec::new();
    let mut catalog = None;
    let mut page_tree_root = None;

    let mut i = 0;
    while let Some(found) = find(&data[i..], b"obj") {
        let obj_kw = i + found;
        i = obj_kw + 3;
        // Must be a full keyword: `obj` followed by a delimiter/whitespace.
        if data.get(obj_kw + 3).is_some_and(|&c| !crate::object::is_whitespace(c) && !crate::object::is_delimiter(c)) {
            continue;
        }
        let Some((start, num, generation)) = scan_back_for_header(data, obj_kw) else { continue };
        table.entries.insert(num, XrefEntry::InFile { offset: start, generation });

        // Peek at the object to collect trailer info and object streams.
        // Streams' lengths may be indirect; the fallback finds `endstream`.
        if let Ok((_, obj)) = Parser::new(data, start).parse_indirect(&|_| None)
            && let Some(d) = obj.as_dict() {
                match d.type_name() {
                    Some(b"Catalog") => catalog = Some(crate::object::ObjRef::new(num, generation)),
                    Some(b"Pages") if !d.contains(b"Parent") => {
                        page_tree_root = Some(crate::object::ObjRef::new(num, generation));
                    }
                    Some(b"ObjStm") => object_streams.push(num),
                    Some(b"XRef") => merge_missing(&mut table.trailer, d),
                    _ => {}
                }
            }
    }

    // Classic trailers (the last one is the newest).
    let mut j = 0;
    while let Some(found) = find(&data[j..], b"trailer") {
        let at = j + found + b"trailer".len();
        j = at;
        if let Ok(Object::Dictionary(d)) = Parser::new(data, at).parse_object() {
            for (k, v) in d.iter() {
                table.trailer.set(k.to_vec(), v.clone());
            }
        }
    }
    table.trailer.remove(b"Prev");
    table.trailer.remove(b"XRefStm");

    // Objects inside object streams that weren't found directly.
    for stm in object_streams {
        if let Some(n) = object_stream_members(data, &table, stm) {
            for (idx, num) in n.into_iter().enumerate() {
                table.entries.entry(num).or_insert(XrefEntry::InStream { stream: stm, index: idx as u32 });
            }
        }
    }

    if let Some(root) = catalog {
        // Use the catalog found in the body if the trailer's /Root is
        // missing or points to an object that doesn't exist.
        let root_ok = table
            .trailer
            .get(b"Root")
            .and_then(Object::as_reference)
            .is_some_and(|r| table.entries.contains_key(&r.num));
        if !root_ok {
            table.trailer.set(b"Root".to_vec(), Object::Reference(root));
        }
    }
    if !table.trailer.contains(b"Root") {
        // The catalog itself is lost (e.g. the file is truncated), but the
        // page tree survives: synthesize a minimal catalog around it.
        if let Some(pages) = page_tree_root {
            let mut synthetic = Dictionary::new();
            synthetic.set(b"Type".to_vec(), Object::Name(b"Catalog".to_vec()));
            synthetic.set(b"Pages".to_vec(), Object::Reference(pages));
            table.trailer.set(b"Root".to_vec(), Object::Dictionary(synthetic));
        }
    }
    if !table.trailer.contains(b"Root") {
        return Err(Error::parse(0, "could not reconstruct document: no catalog found"));
    }
    Ok(table)
}

fn merge_missing(into: &mut Dictionary, from: &Dictionary) {
    for k in [&b"Root"[..], b"Info", b"ID", b"Encrypt"] {
        if let (false, Some(v)) = (into.contains(k), from.get(k)) {
            into.set(k.to_vec(), v.clone());
        }
    }
}

/// Given the position of an `obj` keyword, walks back over `num gen` and
/// returns (start offset, num, gen).
fn scan_back_for_header(data: &[u8], obj_kw: usize) -> Option<(usize, u32, u16)> {
    let mut p = obj_kw;
    let skip_ws = |p: &mut usize| {
        while *p > 0 && crate::object::is_whitespace(data[*p - 1]) {
            *p -= 1;
        }
    };
    let read_digits = |p: &mut usize| -> Option<u64> {
        let end = *p;
        while *p > 0 && data[*p - 1].is_ascii_digit() && end - *p < 10 {
            *p -= 1;
        }
        std::str::from_utf8(&data[*p..end]).ok()?.parse().ok()
    };
    skip_ws(&mut p);
    let generation = read_digits(&mut p)?;
    if !data.get(p.wrapping_sub(1)).is_some_and(|&c| crate::object::is_whitespace(c)) {
        return None;
    }
    skip_ws(&mut p);
    let num = read_digits(&mut p)?;
    // The object number must start a token.
    if p > 0 && !crate::object::is_whitespace(data[p - 1]) && !crate::object::is_delimiter(data[p - 1]) {
        return None;
    }
    if num == 0 || num > MAX_OBJECTS || generation > u16::MAX as u64 {
        return None;
    }
    Some((p, num as u32, generation as u16))
}

/// Object numbers stored in object stream `stm`, in order.
fn object_stream_members(data: &[u8], table: &XrefTable, stm: u32) -> Option<Vec<u32>> {
    let XrefEntry::InFile { offset, .. } = table.get(stm)? else { return None };
    let (_, obj) = Parser::new(data, offset).parse_indirect(&|_| None).ok()?;
    let stream = obj.as_stream()?;
    let n = stream.dict.get(b"N")?.as_i64()?;
    let decoded = filters::decode(&stream.raw, &stream.dict).ok()?.data;
    let mut lex = Lexer::new(&decoded, 0);
    let mut nums = Vec::new();
    for _ in 0..n {
        match (lex.next_token(), lex.next_token()) {
            (Some(Token::Integer(num)), Some(Token::Integer(_))) => nums.push(u32::try_from(num).ok()?),
            _ => break,
        }
    }
    Some(nums)
}
