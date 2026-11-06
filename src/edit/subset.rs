//! TrueType subsetting for embedding: keeps the glyphs used (and the
//! components of composite glyphs), empties all others while keeping glyph
//! ids unchanged, and drops tables a PDF reader doesn't need. Works on a
//! single font or one face of a collection (`.ttc`).

use std::collections::BTreeSet;

/// Tables kept in the subset (the ones PDF readers use for TrueType).
const KEEP: [&[u8; 4]; 11] = [b"head", b"hhea", b"hmtx", b"maxp", b"loca", b"glyf", b"cvt ", b"fpgm", b"prep", b"cmap", b"OS/2"];

fn u16_at(d: &[u8], o: usize) -> Option<u16> {
    Some(u16::from_be_bytes(d.get(o..o + 2)?.try_into().ok()?))
}

fn u32_at(d: &[u8], o: usize) -> Option<u32> {
    Some(u32::from_be_bytes(d.get(o..o + 4)?.try_into().ok()?))
}

/// Returns the subset font file, or `None` if the data isn't a TrueType
/// font with glyph outlines.
pub(crate) fn subset_truetype(data: &[u8], index: u32, gids: &[u16]) -> Option<Vec<u8>> {
    // Table directory of the face.
    let face_offset = if data.get(..4)? == b"ttcf" { u32_at(data, 12 + 4 * index as usize)? as usize } else { 0 };
    let num_tables = u16_at(data, face_offset + 4)? as usize;
    let mut tables: Vec<([u8; 4], &[u8])> = Vec::new();
    for i in 0..num_tables {
        let rec = face_offset + 12 + 16 * i;
        let tag: [u8; 4] = data.get(rec..rec + 4)?.try_into().ok()?;
        let off = u32_at(data, rec + 8)? as usize;
        let len = u32_at(data, rec + 12)? as usize;
        tables.push((tag, data.get(off..off.checked_add(len)?)?));
    }
    let table = |tag: &[u8; 4]| tables.iter().find(|(t, _)| t == tag).map(|(_, d)| *d);
    let head = table(b"head")?;
    let glyf = table(b"glyf")?;
    let loca = table(b"loca")?;
    let num_glyphs = u16_at(table(b"maxp")?, 4)? as usize;
    let long_loca = u16_at(head, 50)? == 1;
    let glyph_range = |g: usize| -> Option<(usize, usize)> {
        let (a, b) = if long_loca {
            (u32_at(loca, 4 * g)? as usize, u32_at(loca, 4 * g + 4)? as usize)
        } else {
            (u16_at(loca, 2 * g)? as usize * 2, u16_at(loca, 2 * g + 2)? as usize * 2)
        };
        (a <= b && b <= glyf.len()).then_some((a, b))
    };

    // The glyphs to keep: those used, .notdef, and composite components.
    let mut keep: BTreeSet<usize> = gids.iter().map(|&g| g as usize).filter(|&g| g < num_glyphs).collect();
    keep.insert(0);
    let mut todo: Vec<usize> = keep.iter().copied().collect();
    while let Some(g) = todo.pop() {
        let Some((a, b)) = glyph_range(g) else { continue };
        let glyph = &glyf[a..b];
        if glyph.len() < 10 || (u16_at(glyph, 0)? as i16) >= 0 {
            continue;
        }
        // Composite: walk its components.
        let mut p = 10;
        while let (Some(flags), Some(component)) = (u16_at(glyph, p), u16_at(glyph, p + 2)) {
            let component = component as usize;
            if component < num_glyphs && keep.insert(component) {
                todo.push(component);
            }
            p += 4 + if flags & 1 != 0 { 4 } else { 2 };
            p += if flags & 0x8 != 0 {
                2
            } else if flags & 0x40 != 0 {
                4
            } else if flags & 0x80 != 0 {
                8
            } else {
                0
            };
            if flags & 0x20 == 0 {
                break;
            }
        }
    }

    // New glyf and (long) loca.
    let mut new_glyf = Vec::new();
    let mut new_loca = Vec::with_capacity(4 * (num_glyphs + 1));
    for g in 0..num_glyphs {
        new_loca.extend_from_slice(&(new_glyf.len() as u32).to_be_bytes());
        if keep.contains(&g)
            && let Some((a, b)) = glyph_range(g)
        {
            new_glyf.extend_from_slice(&glyf[a..b]);
            while new_glyf.len() % 4 != 0 {
                new_glyf.push(0);
            }
        }
    }
    new_loca.extend_from_slice(&(new_glyf.len() as u32).to_be_bytes());
    let mut new_head = head.to_vec();
    new_head.get_mut(50..52)?.copy_from_slice(&1u16.to_be_bytes());
    new_head.get_mut(8..12)?.copy_from_slice(&[0; 4]); // checkSumAdjustment

    let mut out_tables: Vec<([u8; 4], Vec<u8>)> = Vec::new();
    for tag in KEEP {
        let data = match tag {
            b"glyf" => new_glyf.clone(),
            b"loca" => new_loca.clone(),
            b"head" => new_head.clone(),
            _ => match table(tag) {
                Some(d) => d.to_vec(),
                None => continue,
            },
        };
        out_tables.push((*tag, data));
    }
    out_tables.sort_by_key(|(t, _)| *t);
    Some(write_sfnt(&out_tables))
}

fn write_sfnt(tables: &[([u8; 4], Vec<u8>)]) -> Vec<u8> {
    let n = tables.len() as u16;
    let entry_selector = 15 - n.max(1).leading_zeros() as u16;
    let search_range = (1u16 << entry_selector) * 16;
    let mut out = Vec::new();
    out.extend_from_slice(&0x0001_0000u32.to_be_bytes());
    out.extend_from_slice(&n.to_be_bytes());
    out.extend_from_slice(&search_range.to_be_bytes());
    out.extend_from_slice(&entry_selector.to_be_bytes());
    out.extend_from_slice(&(n * 16 - search_range).to_be_bytes());
    let mut offset = 12 + 16 * tables.len();
    let mut body = Vec::new();
    for (tag, data) in tables {
        let checksum = data
            .chunks(4)
            .map(|c| {
                let mut w = [0u8; 4];
                w[..c.len()].copy_from_slice(c);
                u32::from_be_bytes(w)
            })
            .fold(0u32, u32::wrapping_add);
        out.extend_from_slice(tag);
        out.extend_from_slice(&checksum.to_be_bytes());
        out.extend_from_slice(&(offset as u32).to_be_bytes());
        out.extend_from_slice(&(data.len() as u32).to_be_bytes());
        body.extend_from_slice(data);
        while body.len() % 4 != 0 {
            body.push(0);
        }
        offset = 12 + 16 * tables.len() + body.len();
    }
    out.extend_from_slice(&body);
    out
}
