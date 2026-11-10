//! Integration tests that build small PDFs byte-by-byte, covering file
//! structures from PDF 1.0 through 1.7 and several kinds of damage.

use std::io::Write;

use k_pdf::{Document, Error, ObjRef, Object, TextExtractor};

/// Assembles a PDF from object bodies, computing xref offsets.
struct Builder {
    buf: Vec<u8>,
    offsets: Vec<(u32, usize)>,
}

impl Builder {
    fn new(version: &str) -> Self {
        Builder { buf: format!("%PDF-{version}\n%\u{e2}\u{e3}\n").into_bytes(), offsets: Vec::new() }
    }

    fn obj(&mut self, num: u32, body: &str) -> &mut Self {
        self.obj_bytes(num, body.as_bytes())
    }

    fn obj_bytes(&mut self, num: u32, body: &[u8]) -> &mut Self {
        self.offsets.push((num, self.buf.len()));
        self.buf.extend_from_slice(format!("{num} 0 obj\n").as_bytes());
        self.buf.extend_from_slice(body);
        self.buf.extend_from_slice(b"\nendobj\n");
        self
    }

    fn stream(&mut self, num: u32, dict_extra: &str, data: &[u8]) -> &mut Self {
        let mut body = format!("<< /Length {} {dict_extra} >>\nstream\n", data.len()).into_bytes();
        body.extend_from_slice(data);
        body.extend_from_slice(b"\nendstream");
        self.obj_bytes(num, &body)
    }

    /// Classic xref table + trailer.
    fn finish(&mut self, trailer: &str) -> Vec<u8> {
        let xref_at = self.buf.len();
        let size = self.offsets.iter().map(|(n, _)| *n).max().unwrap_or(0) + 1;
        let mut table = vec![None; size as usize];
        for &(n, off) in &self.offsets {
            table[n as usize] = Some(off);
        }
        let mut x = format!("xref\n0 {size}\n0000000000 65535 f\r\n");
        for entry in &table[1..] {
            match entry {
                Some(off) => x += &format!("{off:010} 00000 n\r\n"),
                None => x += "0000000000 65535 f\r\n",
            }
        }
        x += &format!("trailer\n<< /Size {size} {trailer} >>\nstartxref\n{xref_at}\n%%EOF\n");
        self.buf.extend_from_slice(x.as_bytes());
        self.buf.clone()
    }
}

fn hello_world_objects(b: &mut Builder, content: &[u8], content_extra: &str) {
    b.obj(1, "<< /Type /Catalog /Pages 2 0 R >>")
        .obj(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 /MediaBox [0 0 612 792] >>")
        .obj(3, "<< /Type /Page /Parent 2 0 R /Contents 4 0 R /Resources << /Font << /F1 5 0 R >> >> >>")
        .stream(4, content_extra, content)
        .obj(5, "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>");
}

const HELLO: &[u8] = b"BT /F1 12 Tf 72 720 Td (Hello World) Tj 0 -14 Td (Second line) Tj ET";

fn text_of(doc: &Document) -> String {
    let mut ex = TextExtractor::new(doc);
    doc.pages().unwrap().iter().map(|p| ex.extract_page(p).unwrap()).collect()
}

#[test]
fn pdf_1_0_uncompressed() {
    let mut b = Builder::new("1.0");
    hello_world_objects(&mut b, HELLO, "");
    let doc = Document::from_bytes(b.finish("/Root 1 0 R")).unwrap();
    assert_eq!(doc.version(), "1.0");
    assert_eq!(doc.page_count().unwrap(), 1);
    assert!(!doc.was_repaired());
    assert_eq!(text_of(&doc), "Hello World\nSecond line\n");
    let page = &doc.pages().unwrap()[0];
    assert_eq!(page.media_box(&doc), [0.0, 0.0, 612.0, 792.0]); // inherited from /Pages
}

#[test]
fn lzw_and_ascii85_content() {
    // Old PDFs (1.0/1.1) typically used LZW, often wrapped in ASCII85.
    let lzw = lzw_encode(HELLO);
    let mut a85 = ascii85_encode(&lzw);
    a85.extend_from_slice(b"~>");
    let mut b = Builder::new("1.1");
    hello_world_objects(&mut b, &a85, "/Filter [/ASCII85Decode /LZWDecode]");
    let doc = Document::from_bytes(b.finish("/Root 1 0 R")).unwrap();
    assert_eq!(text_of(&doc), "Hello World\nSecond line\n");
}

#[test]
fn flate_with_indirect_length() {
    let compressed = zlib(HELLO);
    let mut b = Builder::new("1.4");
    b.obj(1, "<< /Type /Catalog /Pages 2 0 R >>")
        .obj(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>")
        .obj(3, "<< /Type /Page /Parent 2 0 R /Contents 4 0 R /Resources << /Font << /F1 5 0 R >> >> >>");
    let mut body = b"<< /Length 6 0 R /Filter /FlateDecode >>\nstream\n".to_vec();
    body.extend_from_slice(&compressed);
    body.extend_from_slice(b"\nendstream");
    b.obj_bytes(4, &body)
        .obj(5, "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>")
        .obj(6, &compressed.len().to_string());
    let doc = Document::from_bytes(b.finish("/Root 1 0 R")).unwrap();
    assert_eq!(text_of(&doc), "Hello World\nSecond line\n");
}

#[test]
fn info_dictionary_encodings() {
    let mut b = Builder::new("1.3");
    hello_world_objects(&mut b, HELLO, "");
    b.obj(6, "<< /Title <FEFF00480069> /Author (Jos\\351 \\(test\\)) /Producer /SomeName >>");
    let doc = Document::from_bytes(b.finish("/Root 1 0 R /Info 6 0 R")).unwrap();
    let m = doc.metadata().unwrap();
    assert_eq!(m.title.as_deref(), Some("Hi"));
    assert_eq!(m.author.as_deref(), Some("José (test)"));
    assert_eq!(m.producer.as_deref(), Some("SomeName"));
}

#[test]
fn incremental_update_overrides_object() {
    let mut b = Builder::new("1.4");
    hello_world_objects(&mut b, HELLO, "");
    let mut data = b.finish("/Root 1 0 R");
    let first_xref = find_last(&data, b"startxref").map(|p| {
        let s = std::str::from_utf8(&data[p + 10..]).unwrap();
        s.split_whitespace().next().unwrap().parse::<usize>().unwrap()
    });

    // Append an update replacing the content stream (object 4).
    let new_content = b"BT /F1 12 Tf 72 720 Td (Updated) Tj ET";
    let off = data.len();
    write!(data, "4 0 obj\n<< /Length {} >>\nstream\n", new_content.len()).unwrap();
    data.extend_from_slice(new_content);
    data.extend_from_slice(b"\nendstream\nendobj\n");
    let xref_at = data.len();
    write!(
        data,
        "xref\n4 1\n{off:010} 00000 n\r\ntrailer\n<< /Size 6 /Root 1 0 R /Prev {} >>\nstartxref\n{xref_at}\n%%EOF\n",
        first_xref.unwrap()
    )
    .unwrap();

    let doc = Document::from_bytes(data).unwrap();
    assert!(!doc.was_repaired());
    assert_eq!(text_of(&doc), "Updated\n");
}

#[test]
fn xref_stream_and_object_streams() {
    // PDF 1.5 layout: page objects compressed inside an object stream,
    // cross-reference given as a stream with /W [1 2 1].
    let mut buf = b"%PDF-1.5\n".to_vec();
    let content_off = buf.len();
    write!(buf, "4 0 obj\n<< /Length {} >>\nstream\n", HELLO.len()).unwrap();
    buf.extend_from_slice(HELLO);
    buf.extend_from_slice(b"\nendstream\nendobj\n");

    let objs = [
        (1, "<< /Type /Catalog /Pages 2 0 R >>"),
        (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
        (3, "<< /Type /Page /Parent 2 0 R /Contents 4 0 R /Resources << /Font << /F1 5 0 R >> >> >>"),
        (5, "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>"),
    ];
    let mut header = String::new();
    let mut body = String::new();
    for (num, src) in objs {
        header += &format!("{num} {} ", body.len());
        body += src;
        body += "\n";
    }
    let objstm_data = zlib(format!("{header}{body}").as_bytes());
    let objstm_off = buf.len();
    write!(
        buf,
        "6 0 obj\n<< /Type /ObjStm /N 4 /First {} /Filter /FlateDecode /Length {} >>\nstream\n",
        header.len(),
        objstm_data.len()
    )
    .unwrap();
    buf.extend_from_slice(&objstm_data);
    buf.extend_from_slice(b"\nendstream\nendobj\n");

    let xref_off = buf.len();
    let mut rows = Vec::new();
    let mut row = |t: u8, f2: usize, f3: u8| rows.extend_from_slice(&[t, (f2 >> 8) as u8, f2 as u8, f3]);
    row(0, 0, 255); // 0: free
    row(2, 6, 0); // 1: in objstm 6, index 0
    row(2, 6, 1); // 2
    row(2, 6, 2); // 3
    row(1, content_off, 0); // 4
    row(2, 6, 3); // 5
    row(1, objstm_off, 0); // 6
    row(1, xref_off, 0); // 7: the xref stream itself
    let xref_data = zlib(&rows);
    write!(
        buf,
        "7 0 obj\n<< /Type /XRef /Size 8 /W [1 2 1] /Root 1 0 R /Filter /FlateDecode /Length {} >>\nstream\n",
        xref_data.len()
    )
    .unwrap();
    buf.extend_from_slice(&xref_data);
    write!(buf, "\nendstream\nendobj\nstartxref\n{xref_off}\n%%EOF\n").unwrap();

    let doc = Document::from_bytes(buf).unwrap();
    assert!(!doc.was_repaired());
    assert_eq!(doc.version(), "1.5");
    assert_eq!(text_of(&doc), "Hello World\nSecond line\n");
}

#[test]
fn wrong_xref_offsets_are_repaired() {
    let mut b = Builder::new("1.2");
    hello_world_objects(&mut b, HELLO, "");
    let data = b.finish("/Root 1 0 R");
    // Insert junk in the middle, shifting every later object.
    let at = find_first(&data, b"3 0 obj").unwrap();
    let mut broken = data[..at].to_vec();
    broken.extend_from_slice(b"% some garbage inserted by a broken tool\n");
    broken.extend_from_slice(&data[at..]);

    let doc = Document::from_bytes(broken).unwrap();
    assert_eq!(text_of(&doc), "Hello World\nSecond line\n");
    assert!(doc.was_repaired());
}

#[test]
fn missing_xref_and_trailer_are_reconstructed() {
    let mut b = Builder::new("1.3");
    hello_world_objects(&mut b, HELLO, "");
    let data = b.finish("/Root 1 0 R");
    let cut = find_first(&data, b"xref").unwrap();
    let doc = Document::from_bytes(data[..cut].to_vec()).unwrap(); // truncated file
    assert!(doc.was_repaired());
    assert_eq!(doc.page_count().unwrap(), 1);
    assert_eq!(text_of(&doc), "Hello World\nSecond line\n");
}

#[test]
fn junk_before_header() {
    let mut b = Builder::new("1.4");
    hello_world_objects(&mut b, HELLO, "");
    let mut data = b"From: someone@example.com\r\nSubject: pdf\r\n\r\n".to_vec();
    data.extend_from_slice(&b.finish("/Root 1 0 R"));
    let doc = Document::from_bytes(data).unwrap();
    assert_eq!(text_of(&doc), "Hello World\nSecond line\n");
}

#[test]
fn wrong_stream_length_is_tolerated() {
    let mut b = Builder::new("1.3");
    b.obj(1, "<< /Type /Catalog /Pages 2 0 R >>")
        .obj(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>")
        .obj(3, "<< /Type /Page /Parent 2 0 R /Contents 4 0 R /Resources << /Font << /F1 5 0 R >> >> >>");
    let mut body = b"<< /Length 5 >>\nstream\n".to_vec(); // far too short
    body.extend_from_slice(HELLO);
    body.extend_from_slice(b"\nendstream");
    b.obj_bytes(4, &body).obj(5, "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>");
    let doc = Document::from_bytes(b.finish("/Root 1 0 R")).unwrap();
    assert_eq!(text_of(&doc), "Hello World\nSecond line\n");
}

#[test]
fn nested_page_tree_with_inheritance_and_cycle() {
    let mut b = Builder::new("1.4");
    b.obj(1, "<< /Type /Catalog /Pages 2 0 R >>")
        .obj(2, "<< /Type /Pages /Kids [3 0 R 6 0 R] /Count 3 /Rotate 90 >>")
        // Intermediate node with a back-reference to the root (a cycle).
        .obj(3, "<< /Type /Pages /Parent 2 0 R /Kids [4 0 R 5 0 R 2 0 R] /Count 2 /MediaBox [0 0 100 200] >>")
        .obj(4, "<< /Type /Page /Parent 3 0 R >>")
        .obj(5, "<< /Type /Page /Parent 3 0 R /Rotate 0 >>")
        .obj(6, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 300 400] >>");
    let doc = Document::from_bytes(b.finish("/Root 1 0 R")).unwrap();
    let pages = doc.pages().unwrap();
    assert_eq!(pages.len(), 3);
    assert_eq!(pages[0].id.num, 4);
    assert_eq!(pages[0].media_box(&doc), [0.0, 0.0, 100.0, 200.0]);
    assert_eq!(pages[0].rotation(), 90);
    assert_eq!(pages[1].rotation(), 0);
    assert_eq!(pages[2].media_box(&doc), [0.0, 0.0, 300.0, 400.0]);
}

#[test]
fn tounicode_identity_h_and_tj_spacing() {
    let cmap = b"/CIDInit /ProcSet findresource begin 12 dict begin begincmap
1 begincodespacerange <0000> <FFFF> endcodespacerange
3 beginbfchar <0001> <0048> <0002> <0069> <0003> <00660069> endbfchar
endcmap end end";
    let content = b"BT /F1 10 Tf 0 0 Td [<00010002> -600 <0003>] TJ ET";
    let mut b = Builder::new("1.4");
    b.obj(1, "<< /Type /Catalog /Pages 2 0 R >>")
        .obj(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>")
        .obj(3, "<< /Type /Page /Parent 2 0 R /Contents 4 0 R /Resources << /Font << /F1 5 0 R >> >> >>")
        .stream(4, "", content)
        .obj(5, "<< /Type /Font /Subtype /Type0 /BaseFont /X /Encoding /Identity-H /DescendantFonts [6 0 R] /ToUnicode 7 0 R >>")
        .obj(6, "<< /Type /Font /Subtype /CIDFontType2 /DW 500 >>")
        .stream(7, "", cmap);
    let doc = Document::from_bytes(b.finish("/Root 1 0 R")).unwrap();
    assert_eq!(text_of(&doc), "Hi fi\n");
}

#[test]
fn differences_encoding() {
    let content = b"BT /F1 10 Tf 0 0 Td <0102> Tj ET";
    let mut b = Builder::new("1.2");
    b.obj(1, "<< /Type /Catalog /Pages 2 0 R >>")
        .obj(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>")
        .obj(3, "<< /Type /Page /Parent 2 0 R /Contents 4 0 R /Resources << /Font << /F1 5 0 R >> >> >>")
        .stream(4, "", content)
        .obj(5, "<< /Type /Font /Subtype /Type1 /BaseFont /X /FirstChar 1 /Widths [500 500] /Encoding << /Differences [1 /eacute /germandbls] >> >>");
    let doc = Document::from_bytes(b.finish("/Root 1 0 R")).unwrap();
    assert_eq!(text_of(&doc), "éß\n");
}

#[test]
fn form_xobject_text_and_inline_image() {
    let content = b"q 1 0 0 1 0 0 cm BI /W 2 /H 1 /BPC 8 /CS /G ID \x00EI\xff EI Q /Fm1 Do";
    let mut b = Builder::new("1.4");
    b.obj(1, "<< /Type /Catalog /Pages 2 0 R >>")
        .obj(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>")
        .obj(3, "<< /Type /Page /Parent 2 0 R /Contents 4 0 R /Resources << /XObject << /Fm1 6 0 R >> /Font << /F1 5 0 R >> >> >>")
        .stream(4, "", content)
        .obj(5, "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>")
        .stream(6, "/Type /XObject /Subtype /Form /BBox [0 0 100 100]", b"BT /F1 9 Tf (In a form) Tj ET");
    let doc = Document::from_bytes(b.finish("/Root 1 0 R")).unwrap();
    assert_eq!(text_of(&doc), "In a form\n");
}

#[test]
fn a_password_is_asked_for() {
    // The check values match no password (not even the empty one).
    let mut b = Builder::new("1.4");
    hello_world_objects(&mut b, HELLO, "");
    b.obj(6, "<< /Filter /Standard /V 1 /R 2 /O <00> /U <00> /P -4 >>");
    let data = b.finish("/Root 1 0 R /Encrypt 6 0 R");
    assert!(matches!(Document::from_bytes(data.clone()), Err(Error::PasswordNeeded)));
    assert!(matches!(Document::from_bytes_with_password(data, "guess"), Err(Error::WrongPassword)));

    // Encrypted for certain people (with certificates), not with a password.
    let mut b = Builder::new("1.4");
    hello_world_objects(&mut b, HELLO, "");
    b.obj(6, "<< /Filter /Adobe.PubSec /V 4 /R 4 >>");
    assert!(matches!(Document::from_bytes(b.finish("/Root 1 0 R /Encrypt 6 0 R")), Err(Error::Encrypted(_))));
}

#[test]
fn not_a_pdf() {
    assert!(Document::from_bytes(b"hello, I am not a PDF".to_vec()).is_err());
    assert!(Document::from_bytes(b"%PDF-1.4\n garbage only".to_vec()).is_err());
}

#[test]
fn reference_cycles_do_not_hang() {
    let mut b = Builder::new("1.4");
    b.obj(1, "<< /Type /Catalog /Pages 2 0 R >>")
        .obj(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>")
        .obj(3, "<< /Type /Page /Parent 2 0 R /Contents 4 0 R >>")
        .obj(4, "5 0 R")
        .obj(5, "4 0 R");
    let doc = Document::from_bytes(b.finish("/Root 1 0 R")).unwrap();
    let page = &doc.pages().unwrap()[0];
    assert!(doc.page_content(page).is_err());
    assert!(doc.resolve(&Object::Reference(k_pdf::ObjRef::new(4, 0))).is_err());
}

// ----------------------------------------------------------------------
// Helpers
// ----------------------------------------------------------------------

fn find_first(h: &[u8], n: &[u8]) -> Option<usize> {
    h.windows(n.len()).position(|w| w == n)
}

fn find_last(h: &[u8], n: &[u8]) -> Option<usize> {
    h.windows(n.len()).rposition(|w| w == n)
}

fn zlib(data: &[u8]) -> Vec<u8> {
    let mut e = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
    e.write_all(data).unwrap();
    e.finish().unwrap()
}

/// Minimal LZW encoder (early change = 1), for producing test input.
fn lzw_encode(data: &[u8]) -> Vec<u8> {
    use std::collections::HashMap;
    let mut out = Vec::new();
    let (mut acc, mut nbits) = (0u32, 0u32);
    let mut emit = |code: u32, width: u32, out: &mut Vec<u8>| {
        acc = (acc << width) | code;
        nbits += width;
        while nbits >= 8 {
            out.push((acc >> (nbits - 8)) as u8);
            nbits -= 8;
        }
    };
    let mut dict: HashMap<Vec<u8>, u32> = (0..=255u32).map(|i| (vec![i as u8], i)).collect();
    let mut next = 258u32;
    let width = |next: u32| if next + 1 > 4095 { 12 } else { 32 - (next).leading_zeros() }.max(9);
    emit(256, 9, &mut out);
    let mut w: Vec<u8> = Vec::new();
    for &c in data {
        let mut wc = w.clone();
        wc.push(c);
        if dict.contains_key(&wc) {
            w = wc;
        } else {
            emit(dict[&w], width(next), &mut out);
            dict.insert(wc, next);
            next += 1;
            w = vec![c];
        }
    }
    if !w.is_empty() {
        emit(dict[&w], width(next), &mut out);
        next += 1;
    }
    emit(257, width(next), &mut out);
    emit(0, 7, &mut out); // flush
    out
}

fn ascii85_encode(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    for chunk in data.chunks(4) {
        let mut b = [0u8; 4];
        b[..chunk.len()].copy_from_slice(chunk);
        let mut v = u32::from_be_bytes(b);
        let mut digits = [0u8; 5];
        for d in digits.iter_mut().rev() {
            *d = (v % 85) as u8 + b'!';
            v /= 85;
        }
        out.extend_from_slice(&digits[..chunk.len() + 1]);
    }
    out
}

#[test]
fn character_positions_in_display_space() {
    let mut b = Builder::new("1.4");
    hello_world_objects(&mut b, HELLO, "");
    let doc = Document::from_bytes(b.finish("/Root 1 0 R")).unwrap();
    let page = &doc.pages().unwrap()[0];
    let chars = TextExtractor::new(&doc).extract_chars(page).unwrap();
    assert_eq!(k_pdf::chars_to_string(&chars), "Hello World\nSecond line\n");
    // "H" at x = 72, baseline y = 720 (page space) = 792 − 720 = 72 from the top.
    let h = chars[0].rect.unwrap();
    assert_eq!(chars[0].text, "H");
    assert!((h[0] - 72.0).abs() < 0.01 && (h[1] - (72.0 - 9.6)).abs() < 0.01 && (h[3] - (72.0 + 2.4)).abs() < 0.01);
    // Helvetica has no /Widths here, so its standard metrics apply: H = 0.722 em.
    assert!((h[2] - (72.0 + 12.0 * 0.722)).abs() < 0.01);
    // Inserted separators have no rectangle.
    let newline = chars.iter().find(|c| c.text == "\n").unwrap();
    assert!(newline.rect.is_none());
}

#[test]
fn links_and_destinations() {
    use k_pdf::{page_links, LinkTarget};
    let mut b = Builder::new("1.7");
    b.obj(
        1,
        "<< /Type /Catalog /Pages 2 0 R /Names << /Dests << /Names [(sec1) [4 0 R /XYZ 0 692 null]] >> >> \
         /Dests << /old [4 0 R /FitH 592] >> >>",
    )
    .obj(2, "<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 /MediaBox [0 0 612 792] >>")
    .obj(3, "<< /Type /Page /Parent 2 0 R /Annots [5 0 R 6 0 R 7 0 R 8 0 R 9 0 R 10 0 R] >>")
    .obj(4, "<< /Type /Page /Parent 2 0 R >>")
    // A web link, 100 pt from the top of the page.
    .obj(5, "<< /Type /Annot /Subtype /Link /Rect [72 682 172 692] /A << /S /URI /URI (https://example.com) >> >>")
    .obj(6, "<< /Subtype /Link /Rect [0 0 10 10] /Dest (sec1) >>")
    .obj(7, "<< /Subtype /Link /Rect [0 0 10 10] /Dest /old >>")
    .obj(8, "<< /Subtype /Link /Rect [0 0 10 10] /A << /S /Named /N /NextPage >> >>")
    // Launching programs is never offered; hidden links are skipped.
    .obj(9, "<< /Subtype /Link /Rect [0 0 10 10] /A << /S /Launch /F (calc.exe) >> >>")
    .obj(10, "<< /Subtype /Link /F 2 /Rect [0 0 10 10] /A << /S /URI /URI (https://hidden) >> >>");
    let doc = Document::from_bytes(b.finish("/Root 1 0 R")).unwrap();
    let pages = doc.pages().unwrap();
    let links = page_links(&doc, &pages, 0);
    let targets: Vec<_> = links.iter().map(|l| l.target.clone()).collect();
    assert_eq!(
        targets,
        [
            LinkTarget::Uri("https://example.com".into()),
            LinkTarget::Page { page: 1, top: Some(100.0) },
            LinkTarget::Page { page: 1, top: Some(200.0) },
            LinkTarget::Page { page: 1, top: None },
        ]
    );
    assert_eq!(links[0].rect, [72.0, 100.0, 172.0, 110.0]);
}

/// Three pages "A", "B", "C" in a nested page tree: A and B sit under an
/// inner node that supplies their resources, media box and rotation.
fn three_page_doc(version: &str) -> Vec<u8> {
    let mut b = Builder::new(version);
    b.obj(1, "<< /Type /Catalog /Pages 2 0 R >>")
        .obj(2, "<< /Type /Pages /Kids [3 0 R 6 0 R] /Count 3 >>")
        .obj(
            3,
            "<< /Type /Pages /Parent 2 0 R /Kids [4 0 R 5 0 R] /Count 2 /MediaBox [0 0 300 400] /Rotate 90 \
             /Resources << /Font << /F1 10 0 R >> >> >>",
        )
        .obj(4, "<< /Type /Page /Parent 3 0 R /Contents 7 0 R >>")
        .obj(5, "<< /Type /Page /Parent 3 0 R /Contents 8 0 R >>")
        .obj(
            6,
            "<< /Type /Page /Parent 2 0 R /Contents 9 0 R /MediaBox [0 0 612 792] \
             /Resources << /Font << /F1 10 0 R >> >> >>",
        )
        .stream(7, "", b"BT /F1 12 Tf 10 10 Td (Page A) Tj ET")
        .stream(8, "", b"BT /F1 12 Tf 10 10 Td (Page B) Tj ET")
        .stream(9, "", b"BT /F1 12 Tf 10 10 Td (Page C) Tj ET")
        .obj(10, "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>")
        .obj(11, "<< /Title (Three pages) >>");
    b.finish("/Root 1 0 R /Info 11 0 R")
}

fn page_texts(doc: &Document) -> Vec<String> {
    let mut ex = TextExtractor::new(doc);
    doc.pages().unwrap().iter().map(|p| ex.extract_page(p).unwrap().trim().to_string()).collect()
}

#[test]
fn reordering_pages_flattens_the_tree_and_keeps_inherited_attributes() {
    // 1.4 is written with a classic xref table, 1.7 with object streams.
    for version in ["1.4", "1.7"] {
        let doc = Document::from_bytes(three_page_doc(version)).unwrap();
        let mut out = Vec::new();
        k_pdf::write_with_page_order(&doc, &[2, 0, 1], &mut out).unwrap();
        assert_eq!(out.windows(7).any(|w| w == b"/ObjStm"), version == "1.7");

        let saved = Document::from_bytes(out).unwrap();
        assert!(!saved.was_repaired(), "{version}: the saved file's xref should be valid");
        assert_eq!(page_texts(&saved), ["Page C", "Page A", "Page B"]);
        let pages = saved.pages().unwrap();
        assert_eq!(pages[1].media_box(&saved), [0.0, 0.0, 300.0, 400.0]);
        assert_eq!(pages[1].rotation(), 90);
        assert_eq!(pages[0].rotation(), 0);
        assert_eq!(saved.metadata().unwrap().title.as_deref(), Some("Three pages"));
        // The old inner node is gone: the tree is one flat /Pages node.
        assert!(saved.get_object(ObjRef::new(3, 0)).unwrap().is_null());
    }
}

#[test]
fn pages_left_out_of_the_order_are_dropped() {
    let doc = Document::from_bytes(three_page_doc("1.4")).unwrap();
    let mut out = Vec::new();
    k_pdf::write_with_page_order(&doc, &[1], &mut out).unwrap();
    let saved = Document::from_bytes(out).unwrap();
    assert_eq!(page_texts(&saved), ["Page B"]);
    assert!(k_pdf::write_with_page_order(&doc, &[3], &mut Vec::new()).is_err());
}

#[test]
fn pages_from_another_document_are_merged_in() {
    let base = Document::from_bytes(three_page_doc("1.4")).unwrap();
    // The second document's pages say "Page A/B/C" too; tell them apart by
    // their page sizes (A and B inherit 300 × 400 from the inner node).
    let other = Document::from_bytes(three_page_doc("1.6")).unwrap();
    let mut out = Vec::new();
    k_pdf::write_document(&[&base, &other], &[(0, 0), (1, 2), (1, 0), (0, 2)], &mut out).unwrap();
    let saved = Document::from_bytes(out).unwrap();
    assert!(!saved.was_repaired());
    assert_eq!(saved.version(), "1.6", "the newer version wins");
    assert_eq!(page_texts(&saved), ["Page A", "Page C", "Page A", "Page C"]);
    let pages = saved.pages().unwrap();
    assert_eq!(pages[2].media_box(&saved), [0.0, 0.0, 300.0, 400.0]);
    assert_eq!(pages[2].rotation(), 90);
    // Only the base document's metadata is kept.
    assert_eq!(saved.metadata().unwrap().title.as_deref(), Some("Three pages"));
    assert!(k_pdf::write_document(&[&base, &other], &[(2, 0)], &mut Vec::new()).is_err());
}

/// A one-page form: a text field, a multi-line text field, a check box, a
/// radio group with two buttons, and a combo box. The radio group's buttons
/// are kids of a parent field, and the name field is nested ("person.name").
fn form_doc() -> Vec<u8> {
    let check_ap = "/AP << /N << /Yes 20 0 R /Off 21 0 R >> >>";
    let mut b = Builder::new("1.7");
    b.obj(1, "<< /Type /Catalog /Pages 2 0 R /AcroForm 4 0 R >>")
        .obj(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 /MediaBox [0 0 600 800] >>")
        .obj(3, "<< /Type /Page /Parent 2 0 R /Annots [11 0 R 12 0 R 13 0 R 15 0 R 16 0 R 17 0 R] >>")
        .obj(
            4,
            "<< /Fields [10 0 R 12 0 R 13 0 R 14 0 R 17 0 R] /DA (/Helv 0 Tf 0 g) \
             /DR << /Font << /Helv 5 0 R >> >> /XFA 30 0 R >>",
        )
        .obj(5, "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>")
        .obj(10, "<< /T (person) /Kids [11 0 R] >>")
        .obj(
            11,
            "<< /Type /Annot /Subtype /Widget /Parent 10 0 R /T (name) /FT /Tx /V (Ada) \
             /Rect [100 700 300 720] /MK << /BG [1 1 1] /BC [0 0 0] >> >>",
        )
        .obj(12, "<< /Subtype /Widget /T (notes) /FT /Tx /Ff 4096 /Rect [100 600 300 680] /Q 1 /TU (Anything else) >>")
        .obj(13, &format!("<< /Subtype /Widget /T (agree) /FT /Btn /V /Off /AS /Off /Rect [100 560 115 575] {check_ap} >>"))
        .obj(14, "<< /T (size) /FT /Btn /Ff 49152 /V /M /Kids [15 0 R 16 0 R] >>")
        .obj(15, "<< /Subtype /Widget /Parent 14 0 R /AS /Off /Rect [100 520 115 535] /AP << /N << /S 20 0 R /Off 21 0 R >> >> >>")
        .obj(16, "<< /Subtype /Widget /Parent 14 0 R /AS /M /Rect [130 520 145 535] /AP << /N << /M 20 0 R /Off 21 0 R >> >> >>")
        .obj(
            17,
            "<< /Subtype /Widget /T (city) /FT /Ch /Ff 131072 /Opt [(Paris) [(NYC) (New York)]] /V (Paris) \
             /DA (/Helv 11 Tf 1 0 0 rg) /Rect [100 480 300 500] >>",
        )
        .stream(20, "/Type /XObject /Subtype /Form /BBox [0 0 15 15]", b"0 0 15 15 re f")
        .stream(21, "/Type /XObject /Subtype /Form /BBox [0 0 15 15]", b"")
        .stream(30, "", b"<xdp/>");
    b.finish("/Root 1 0 R")
}

#[test]
fn form_fields_are_read() {
    use k_pdf::{FieldKind, FieldValue};
    let doc = Document::from_bytes(form_doc()).unwrap();
    let fields = k_pdf::form_fields(&doc).unwrap();
    let names: Vec<&str> = fields.iter().map(|f| f.name.as_str()).collect();
    assert_eq!(names, ["person.name", "notes", "agree", "size", "city"]);

    let name = &fields[0];
    assert_eq!(name.value, FieldValue::Text("Ada".into()));
    assert!(matches!(name.kind, FieldKind::Text { multiline: false, .. }));
    // Display coordinates: 800 - 720 = 80 from the top.
    assert_eq!(name.widgets[0].rect, [100.0, 80.0, 300.0, 100.0]);
    assert_eq!(name.widgets[0].background, Some([1.0, 1.0, 1.0]));

    assert!(matches!(fields[1].kind, FieldKind::Text { multiline: true, .. }));
    assert_eq!((fields[1].alignment, fields[1].tooltip.as_deref()), (1, Some("Anything else")));
    assert_eq!((&fields[2].kind, &fields[2].value), (&FieldKind::CheckBox, &FieldValue::Button(None)));
    assert_eq!(fields[2].widgets[0].on_state.as_deref(), Some("Yes"));

    let size = &fields[3];
    assert_eq!((&size.kind, &size.value), (&FieldKind::Radio, &FieldValue::Button(Some("M".into()))));
    let states: Vec<_> = size.widgets.iter().map(|w| w.on_state.as_deref()).collect();
    assert_eq!(states, [Some("S"), Some("M")]);

    let FieldKind::ComboBox { options, .. } = &fields[4].kind else { panic!("expected a combo box") };
    assert_eq!((options[1].value.as_str(), options[1].label.as_str()), ("NYC", "New York"));
    assert_eq!((fields[4].font_size, fields[4].text_color), (11.0, [1.0, 0.0, 0.0]));
}

#[test]
fn filling_a_form_saves_values_and_appearances() {
    use k_pdf::FieldValue;
    let doc = Document::from_bytes(form_doc()).unwrap();
    let fields = k_pdf::form_fields(&doc).unwrap();
    let values = [
        (fields[0].id, FieldValue::Text("Grace Hopper (née Murray)".into())),
        (fields[1].id, FieldValue::Text("line one\nline two".into())),
        (fields[2].id, FieldValue::Button(Some("Yes".into()))),
        (fields[3].id, FieldValue::Button(Some("S".into()))),
        (fields[4].id, FieldValue::Choice(vec!["NYC".into()])),
    ];
    let changes = k_pdf::fill_form(&doc, &fields, &values).unwrap();
    let mut out = Vec::new();
    k_pdf::write_document_with_changes(&[&doc], &[(0, 0)], &changes, &mut out).unwrap();

    let saved = Document::from_bytes(out).unwrap();
    assert!(!saved.was_repaired());
    let after = k_pdf::form_fields(&saved).unwrap();
    let got: Vec<FieldValue> = after.iter().map(|f| f.value.clone()).collect();
    let want: Vec<FieldValue> = values.iter().map(|(_, v)| v.clone()).collect();
    assert_eq!(got, want);

    // Check boxes and radio buttons switch their widgets' states.
    let state = |r: ObjRef| saved.get_object(r).unwrap().as_dict().unwrap().get(b"AS").cloned();
    assert_eq!(state(after[2].widgets[0].id), Some(Object::Name(b"Yes".to_vec())));
    assert_eq!(state(after[3].widgets[0].id), Some(Object::Name(b"S".to_vec())));
    assert_eq!(state(after[3].widgets[1].id), Some(Object::Name(b"Off".to_vec())));

    // Text and choice fields get appearance streams showing the new text.
    let appearance = |r: ObjRef| {
        let w = saved.get_object(r).unwrap();
        let ap = saved.get_dict(w.as_dict().unwrap(), b"AP").unwrap();
        let Object::Stream(s) = saved.get_resolved(&ap, b"N").unwrap() else { panic!("no appearance") };
        String::from_utf8_lossy(&saved.decode_stream(&s).unwrap()).into_owned()
    };
    let name_ap = appearance(after[0].widgets[0].id);
    assert!(name_ap.contains("(Grace Hopper \\(n\\351e Murray\\))") || name_ap.contains("<4772616365"), "{name_ap}");
    assert!(name_ap.contains("re f"), "the white background is kept: {name_ap}");
    let notes_ap = appearance(after[1].widgets[0].id);
    assert!(notes_ap.contains("(line one)") && notes_ap.contains("(line two)"), "{notes_ap}");
    assert!(appearance(after[4].widgets[0].id).contains("(New York)"));

    // XFA data is dropped so it can't override the values.
    let catalog = saved.catalog().unwrap();
    assert!(!saved.get_dict(&catalog, b"AcroForm").unwrap().contains(b"XFA"));
}

/// A page with a two-line paragraph (drawn in two text objects, the second
/// line split over two operations) and a footer.
fn text_page() -> Vec<u8> {
    let content: &[u8] = b"BT /F1 12 Tf 72 700 Td (Hello world, this is) Tj ET \
        BT /F1 12 Tf 72 685.6 Td [(a te) -10 (st)] TJ ( paragraph.) Tj ET \
        BT /F1 8 Tf 0.5 0 0 rg 72 100 Td (Footer) Tj ET";
    let mut b = Builder::new("1.4");
    b.obj(1, "<< /Type /Catalog /Pages 2 0 R >>")
        .obj(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 /MediaBox [0 0 612 792] >>")
        .obj(3, "<< /Type /Page /Parent 2 0 R /Contents 4 0 R /Resources << /Font << /F1 5 0 R >> >> >>")
        .stream(4, "", content)
        .obj(5, "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>");
    b.finish("/Root 1 0 R")
}

#[test]
fn text_blocks_are_found() {
    let doc = Document::from_bytes(text_page()).unwrap();
    let text = k_pdf::edit::page_text(&doc, 0).unwrap();
    assert_eq!(text.blocks.len(), 2);
    let para = &text.blocks[0];
    assert_eq!(para.lines.len(), 2);
    assert!(para.editable);
    let s: String = text.block_text(para).into_iter().map(|(c, _)| c).collect();
    assert_eq!(s, "Hello world, this is a test paragraph.");
    let footer = &text.blocks[1];
    let style = text.style_of(&text.glyphs[footer.glyphs[0]]);
    assert_eq!((style.family.as_str(), style.size, style.color), ("Helvetica", 8.0, [0.5, 0.0, 0.0]));
    // Baselines in display space (792 - 700 = 92).
    assert!((para.lines[0].baseline - 92.0).abs() < 0.01);
}

#[test]
fn editing_text_replaces_it_in_place() {
    use k_pdf::edit::{Align, BlockEdit, TextStyle};
    let doc = Document::from_bytes(text_page()).unwrap();
    let text = k_pdf::edit::page_text(&doc, 0).unwrap();
    let para = &text.blocks[0];
    let style = text.style_of(&text.glyphs[para.glyphs[0]]);
    let red = TextStyle { color: [1.0, 0.0, 0.0], bold: true, ..style.clone() };
    let edit = BlockEdit {
        page: 0,
        remove: para.glyphs.clone(),
        runs: vec![("Goodbye ".into(), style.clone()), ("cruel".into(), red), (" world, rewritten here.".into(), style)],
        left: 72.0,
        width: 200.0,
        first_baseline: para.lines[0].baseline,
        line_spacing: 1.2,
        align: Align::Left,
        indent: 0.0,
        wrap: true,
    };
    let mut changes = k_pdf::Changes::default();
    k_pdf::edit::edit_text(&doc, &edit, &mut changes).unwrap();

    // The document shows the edit right away (overlay)…
    let now: String = k_pdf::TextExtractor::new(&doc).extract_page(&doc.pages().unwrap()[0]).unwrap();
    assert!(now.contains("Goodbye cruel world"), "{now}");
    assert!(!now.contains("Hello"), "{now}");
    // …and after saving.
    let mut out = Vec::new();
    k_pdf::write_document_with_changes(&[&doc], &[(0, 0)], &changes, &mut out).unwrap();
    let saved = Document::from_bytes(out).unwrap();
    let after = k_pdf::edit::page_text(&saved, 0).unwrap();
    let all: Vec<String> =
        after.blocks.iter().map(|b| after.block_text(b).into_iter().map(|(c, _)| c).collect()).collect();
    assert!(all.iter().any(|t| t.contains("Goodbye cruel world, rewritten here.")), "{all:?}");
    assert!(all.iter().any(|t| t == "Footer"), "{all:?}");
    assert!(!all.iter().any(|t| t.contains("Hello")), "{all:?}");
    // The bold red word kept its style.
    let cruel = after.glyphs.iter().find(|g| g.text == "c" && g.color == [1.0, 0.0, 0.0]).expect("red text");
    assert!(after.style_of(cruel).bold);
}

#[test]
fn removing_text_and_adding_a_new_box() {
    use k_pdf::edit::{Align, BlockEdit, TextStyle};
    let doc = Document::from_bytes(text_page()).unwrap();
    let text = k_pdf::edit::page_text(&doc, 0).unwrap();
    let mut changes = k_pdf::Changes::default();
    // Delete the footer…
    let footer = BlockEdit {
        page: 0,
        remove: text.blocks[1].glyphs.clone(),
        runs: Vec::new(),
        left: 0.0,
        width: 100.0,
        first_baseline: 0.0,
        line_spacing: 1.2,
        align: Align::Left,
        indent: 0.0,
        wrap: true,
    };
    k_pdf::edit::edit_text(&doc, &footer, &mut changes).unwrap();
    // …and add a new line of text elsewhere.
    let new = BlockEdit {
        remove: Vec::new(),
        runs: vec![("Added later".into(), TextStyle { family: "Helvetica".into(), size: 10.0, ..TextStyle::default() })],
        left: 300.0,
        first_baseline: 400.0,
        ..footer
    };
    k_pdf::edit::edit_text(&doc, &new, &mut changes).unwrap();
    let now = k_pdf::edit::page_text(&doc, 0).unwrap();
    let texts: Vec<String> = now.blocks.iter().map(|b| now.block_text(b).into_iter().map(|(c, _)| c).collect()).collect();
    assert_eq!(texts.len(), 2, "{texts:?}");
    assert!(texts.contains(&"Added later".to_string()) && !texts.iter().any(|t| t == "Footer"), "{texts:?}");
    let added = now.blocks.iter().find(|b| now.block_text(b).first().is_some_and(|(c, _)| *c == 'A')).unwrap();
    assert!((added.rect[0] - 300.0).abs() < 0.5 && (added.lines[0].baseline - 400.0).abs() < 0.5);
}

#[test]
fn cropping_pages() {
    let doc = Document::from_bytes(three_page_doc("1.4")).unwrap();
    let mut changes = k_pdf::Changes::default();
    // Page C: 612 × 792, upright. Keep 100 pt from the left, 50 from the top.
    k_pdf::edit::crop_page(&doc, 2, Some([100.0, 50.0, 400.0, 350.0]), &mut changes).unwrap();
    let pages = doc.pages().unwrap();
    assert_eq!(pages[2].crop_box(&doc), [100.0, 442.0, 400.0, 742.0]);
    assert_eq!(pages[2].display_size(&doc), (300.0, 300.0));

    // Page A: 300 × 400 rotated 90°, shown 400 wide and 300 tall. The crop
    // is given as shown and lands on the right part of the unrotated page.
    k_pdf::edit::crop_page(&doc, 0, Some([0.0, 0.0, 200.0, 100.0]), &mut changes).unwrap();
    let pages = doc.pages().unwrap();
    assert_eq!(pages[0].display_size(&doc), (200.0, 100.0));
    assert_eq!(pages[0].crop_box(&doc), [0.0, 0.0, 100.0, 200.0]);

    // Areas outside the page are refused; None shows the whole page again.
    assert!(k_pdf::edit::crop_page(&doc, 1, Some([500.0, 500.0, 600.0, 600.0]), &mut changes).is_err());
    k_pdf::edit::crop_page(&doc, 2, None, &mut changes).unwrap();
    assert_eq!(doc.pages().unwrap()[2].display_size(&doc), (612.0, 792.0));

    // Saved files keep the crop.
    let mut out = Vec::new();
    k_pdf::write_document_with_changes(&[&doc], &[(0, 0), (0, 1), (0, 2)], &changes, &mut out).unwrap();
    let saved = Document::from_bytes(out).unwrap();
    assert_eq!(saved.pages().unwrap()[0].display_size(&saved), (200.0, 100.0));
}

#[test]
#[cfg(feature = "render")]
fn redaction_removes_what_is_under_the_areas() {
    let content: &[u8] = b"BT /F1 12 Tf 72 720 Td (Secret code 1234 here) Tj ET \
        q 1 0 0 1 72 600 cm /Fm1 Do Q \
        q 50 0 0 50 300 300 cm /Im1 Do Q q 50 0 0 50 300 100 cm /Im1 Do Q \
        0 0 1 rg 400 100 20 20 re f 100 100 300 10 re f";
    let mut b = Builder::new("1.4");
    b.obj(1, "<< /Type /Catalog /Pages 2 0 R >>")
        .obj(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 /MediaBox [0 0 612 792] >>")
        .obj(
            3,
            "<< /Type /Page /Parent 2 0 R /Contents 4 0 R /Annots [8 0 R] \
             /Resources << /Font << /F1 5 0 R >> /XObject << /Fm1 6 0 R /Im1 7 0 R >> >> >>",
        )
        .stream(4, "", content)
        .obj(5, "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>")
        .stream(
            6,
            "/Type /XObject /Subtype /Form /BBox [0 0 200 50] /Resources << /Font << /F1 5 0 R >> >>",
            b"BT /F1 10 Tf 0 10 Td (Secret stamp) Tj ET",
        )
        .stream(7, "/Type /XObject /Subtype /Image /Width 2 /Height 2 /ColorSpace /DeviceGray /BitsPerComponent 8", &[200; 4])
        .obj(8, "<< /Type /Annot /Subtype /Link /Rect [72 715 120 730] /A << /S /URI /URI (http://secret.example) >> >>");
    let doc = Document::from_bytes(b.finish("/Root 1 0 R")).unwrap();

    // "Secret" twice: on the page and in the form XObject.
    let found = k_pdf::edit::find_text(&doc, 0, "SECRET").unwrap();
    assert_eq!(found.len(), 2, "{found:?}");
    let mut areas = found.clone();
    // The left half of the image (display y = 792 - 350 … 792 - 300), and
    // the square at (400, 100) but only part of the long bar.
    areas.push([290.0, 430.0, 325.0, 500.0]);
    areas.push([390.0, 660.0, 430.0, 700.0]);
    let mut changes = k_pdf::Changes::default();
    let done = k_pdf::edit::redact_page(&doc, 0, &areas, &mut changes).unwrap();
    assert_eq!(done, k_pdf::edit::Redacted { chars: 12, images: 1, images_removed: 0, shapes: 1, annotations: 1 });

    let mut out = Vec::new();
    k_pdf::write_document_with_changes(&[&doc], &[(0, 0)], &changes, &mut out).unwrap();
    // Nothing of the secret is left in the file (the old content stream
    // and the link were stored uncompressed).
    assert!(find_first(&out.to_ascii_lowercase(), b"secret").is_none());
    let saved = Document::from_bytes(out).unwrap();
    let text = text_of(&saved);
    assert!(text.contains("code 1234 here") && text.contains("stamp"), "{text}");
    assert!(!text.to_lowercase().contains("secret"), "{text}");
    let page = &saved.pages().unwrap()[0];
    assert!(matches!(saved.get_resolved(&page.dict, b"Annots"), Ok(Object::Array(a)) if a.is_empty()));

    // The image: a copy with its left column blacked out; the original is
    // still drawn below (outside the areas).
    let resources = saved.get_dict(&page.dict, b"Resources").unwrap();
    let xobjects = saved.get_dict(&resources, b"XObject").unwrap();
    let images: Vec<Vec<u8>> = xobjects
        .iter()
        .filter_map(|(_, x)| match saved.resolve(x).ok()? {
            Object::Stream(s) if s.dict.get(b"Subtype").and_then(Object::as_name) == Some(b"Image") => {
                Some(saved.decode_stream(&s).unwrap())
            }
            _ => None,
        })
        .collect();
    assert!(images.contains(&vec![0, 200, 0, 200]) && images.contains(&vec![200; 4]), "{images:?}");
}

/// A page with an uncompressed content stream, the same image stored twice,
/// and an LZW-compressed form XObject.
#[cfg(feature = "compress")]
fn compressible_doc() -> Vec<u8> {
    let content: &[u8] = b"BT /F1 12 Tf 72 720 Td (Compress me, compress me, compress me) Tj ET \
        q 64 0 0 64 72 500 cm /Im1 Do Q q 64 0 0 64 200 500 cm /Im2 Do Q /Fm1 Do";
    // A 64 x 64 gray gradient: compresses far better with row prediction.
    let image: Vec<u8> = (0..64 * 64).map(|i| ((i % 64) * 4 + i / 64) as u8).collect();
    let form = lzw_encode(b"BT /F1 9 Tf 72 400 Td (In a form) Tj ET");
    let mut b = Builder::new("1.4");
    b.obj(1, "<< /Type /Catalog /Pages 2 0 R >>")
        .obj(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 /MediaBox [0 0 612 792] >>")
        .obj(
            3,
            "<< /Type /Page /Parent 2 0 R /Contents 4 0 R /Resources << /Font << /F1 5 0 R >> \
             /XObject << /Im1 6 0 R /Im2 7 0 R /Fm1 8 0 R >> >> >>",
        )
        .stream(4, "", content)
        .obj(5, "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>");
    for n in [6, 7] {
        b.stream(n, "/Type /XObject /Subtype /Image /Width 64 /Height 64 /ColorSpace /DeviceGray /BitsPerComponent 8", &image);
    }
    b.stream(8, "/Type /XObject /Subtype /Form /BBox [0 0 612 792] /Filter /LZWDecode", &form);
    b.finish("/Root 1 0 R")
}

#[test]
#[cfg(feature = "compress")]
fn compression_loses_nothing() {
    use k_pdf::compress::{compress, CompressOptions};
    let original = compressible_doc();
    let doc = Document::from_bytes(original.clone()).unwrap();
    let mut out = Vec::new();
    let report = compress(&doc, &CompressOptions::default(), &mut out).unwrap();
    assert!(out.len() < original.len() / 2, "{} → {}", original.len(), out.len());
    assert_eq!(report.duplicates, 1);
    assert_eq!(report.brotli_streams, 0);
    assert!(!out.windows(12).any(|w| w == b"BrotliDecode"));

    let small = Document::from_bytes(out).unwrap();
    assert_eq!(text_of(&small), text_of(&doc));
    // Both images now use one object, with the same samples.
    let page = &small.pages().unwrap()[0];
    let res = small.get_dict(&page.dict, b"Resources").unwrap();
    let xobjects = small.get_dict(&res, b"XObject").unwrap();
    assert_eq!(xobjects.get(b"Im1"), xobjects.get(b"Im2"));
    let image = |d: &Document, xo: &k_pdf::Dictionary| match d.resolve(xo.get(b"Im1").unwrap()).unwrap() {
        Object::Stream(s) => d.decode_stream(&s).unwrap(),
        _ => panic!("not a stream"),
    };
    let orig_page = &doc.pages().unwrap()[0];
    let orig_xo = doc.get_dict(&doc.get_dict(&orig_page.dict, b"Resources").unwrap(), b"XObject").unwrap();
    assert_eq!(image(&small, &xobjects), image(&doc, &orig_xo));
}

#[test]
#[cfg(feature = "compress")]
fn brotli_compression_is_marked_and_readable() {
    use k_pdf::compress::{compress, CompressOptions};
    let doc = Document::from_bytes(compressible_doc()).unwrap();
    let mut out = Vec::new();
    let report = compress(&doc, &CompressOptions { jpeg: true, brotli: true }, &mut out).unwrap();
    assert!(report.brotli_streams > 0);
    assert!(out.starts_with(b"%PDF-2.0"));
    let small = Document::from_bytes(out).unwrap();
    assert_eq!(text_of(&small), text_of(&doc));
    // The registered extension (PDF Association, prefix PDFa).
    let catalog = small.catalog().unwrap();
    let ext = small.get_dict(&catalog, b"Extensions").unwrap();
    let Some(Object::Array(list)) = ext.get(b"PDFa") else { panic!("no PDFa extension") };
    let entry = small.resolve(&list[0]).unwrap();
    let entry = entry.as_dict().unwrap();
    assert_eq!(entry.get(b"ExtensionLevel"), Some(&Object::Integer(1)));
    assert_eq!(entry.get(b"BaseVersion"), Some(&Object::Name(b"2.0".to_vec())));
}

#[test]
fn comments_are_added_changed_saved_and_deleted() {
    use k_pdf::edit::annotate::{self, CommentKind};
    let content: &[u8] = b"BT /F1 12 Tf 72 720 Td (Read this sentence) Tj ET";
    let mut b = Builder::new("1.4");
    b.obj(1, "<< /Type /Catalog /Pages 2 0 R >>")
        .obj(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 /MediaBox [0 0 612 792] >>")
        .obj(3, "<< /Type /Page /Parent 2 0 R /Contents 4 0 R /Annots 6 0 R /Resources << /Font << /F1 5 0 R >> >> >>")
        .stream(4, "", content)
        .obj(5, "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>")
        .obj(6, "[]");
    let doc = Document::from_bytes(b.finish("/Root 1 0 R")).unwrap();
    let mut changes = k_pdf::Changes::default();

    let area = k_pdf::edit::find_text(&doc, 0, "this").unwrap()[0];
    let yellow = annotate::HIGHLIGHT_YELLOW;
    let hl = annotate::add_highlight(&doc, 0, &[area], yellow, "", "me", &mut changes).unwrap();
    let note = annotate::add_note(&doc, 0, [300.0, 100.0], yellow, "A note", "me", &mut changes).unwrap();
    // A comment typed on the new highlight (an object added this session).
    annotate::set_comment_text(&doc, hl, "Why “this”?", &mut changes).unwrap();

    let list = annotate::page_comments(&doc, 0).unwrap();
    assert_eq!(list.len(), 2, "{list:?}");
    assert_eq!((&list[0].kind, list[0].text.as_str()), (&CommentKind::Highlight, "Why “this”?"));
    assert_eq!((&list[1].kind, list[1].text.as_str()), (&CommentKind::Note, "A note"));
    // The highlight covers the word, in display points.
    let a = list[0].areas[0];
    assert!((a[0] - area[0]).abs() < 0.01 && (a[3] - area[3]).abs() < 0.01, "{a:?} vs {area:?}");

    // Saved and read back.
    let mut out = Vec::new();
    k_pdf::write_document_with_changes(&[&doc], &[(0, 0)], &changes, &mut out).unwrap();
    let saved = Document::from_bytes(out).unwrap();
    let list = annotate::comments(&saved).unwrap();
    assert_eq!(list.iter().map(|c| c.text.as_str()).collect::<Vec<_>>(), ["Why “this”?", "A note"]);
    assert_eq!(list[0].author, "me");
    // Each object written once.
    let data = saved.data();
    let header = format!("\n{} 0 obj", list[0].id.num);
    assert!(data.windows(header.len()).filter(|w| *w == header.as_bytes()).count() <= 1);
    // The text is still there, and the highlight is drawn.
    assert!(text_of(&saved).contains("Read this sentence"));

    // Deleting the note.
    let mut changes = k_pdf::Changes::default();
    annotate::delete_comment(&saved, 0, list[1].id, &mut changes).unwrap();
    let left = annotate::comments(&saved).unwrap();
    assert_eq!(left.len(), 1);
    assert_eq!(left[0].id, list[0].id);
    let _ = note;
}

#[test]
fn rotating_pages() {
    // Page 1 inherits /Rotate 90 from the page tree; page 2 has none.
    let mut b = Builder::new("1.4");
    b.obj(1, "<< /Type /Catalog /Pages 2 0 R >>")
        .obj(2, "<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 /MediaBox [0 0 600 800] /Rotate 90 >>")
        .obj(3, "<< /Type /Page /Parent 2 0 R >>")
        .obj(4, "<< /Type /Page /Parent 2 0 R /Rotate 0 >>");
    let doc = Document::from_bytes(b.finish("/Root 1 0 R")).unwrap();
    let mut changes = k_pdf::Changes::default();
    k_pdf::edit::rotate_page(&doc, 0, -90, &mut changes).unwrap();
    k_pdf::edit::rotate_page(&doc, 1, 90, &mut changes).unwrap();
    k_pdf::edit::rotate_page(&doc, 1, 180, &mut changes).unwrap();
    assert!(k_pdf::edit::rotate_page(&doc, 1, 45, &mut changes).is_err());

    let mut out = Vec::new();
    k_pdf::write_document_with_changes(&[&doc], &[(0, 0), (0, 1)], &changes, &mut out).unwrap();
    let saved = Document::from_bytes(out).unwrap();
    let pages = saved.pages().unwrap();
    // The inherited 90 was turned back to 0 (set on the page itself).
    assert_eq!(pages[0].rotation(), 0);
    assert_eq!(pages[0].display_size(&saved), (600.0, 800.0));
    assert_eq!(pages[1].rotation(), 270);
    assert_eq!(pages[1].display_size(&saved), (800.0, 600.0));
}

#[test]
fn deleted_pages_take_their_form_fields() {
    // Page 1 has field A; page 2 has field B and a widget of field C,
    // which also has a widget on page 1.
    let mut b = Builder::new("1.4");
    b.obj(1, "<< /Type /Catalog /Pages 2 0 R /AcroForm << /Fields [5 0 R 6 0 R 7 0 R] >> >>")
        .obj(2, "<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 /MediaBox [0 0 600 800] >>")
        .obj(3, "<< /Type /Page /Parent 2 0 R /Annots [5 0 R 8 0 R] >>")
        .obj(4, "<< /Type /Page /Parent 2 0 R /Annots [6 0 R 9 0 R] >>")
        .obj(5, "<< /FT /Tx /T (A) /Subtype /Widget /Rect [0 0 10 10] /P 3 0 R >>")
        .obj(6, "<< /FT /Tx /T (B) /V (secret) /Subtype /Widget /Rect [0 0 10 10] /P 4 0 R >>")
        .obj(7, "<< /FT /Tx /T (C) /Kids [8 0 R 9 0 R] >>")
        .obj(8, "<< /Parent 7 0 R /Subtype /Widget /Rect [0 0 10 10] /P 3 0 R >>")
        .obj(9, "<< /Parent 7 0 R /Subtype /Widget /Rect [0 0 10 10] /P 4 0 R >>");
    let doc = Document::from_bytes(b.finish("/Root 1 0 R")).unwrap();
    let mut out = Vec::new();
    k_pdf::write_with_page_order(&doc, &[0], &mut out).unwrap();
    let text = String::from_utf8_lossy(&out).into_owned();
    let saved = Document::from_bytes(out).unwrap();
    let names: Vec<String> = k_pdf::form_fields(&saved).unwrap().iter().map(|f| f.name.clone()).collect();
    assert_eq!(names, ["A", "C"]);
    let c = k_pdf::form_fields(&saved).unwrap().into_iter().find(|f| f.name == "C").unwrap();
    assert_eq!(c.widgets.len(), 1);
    assert!(!text.contains("secret"));
}
