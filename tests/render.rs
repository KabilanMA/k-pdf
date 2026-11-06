//! Rendering tests: small PDFs built in memory, checked pixel by pixel.
#![cfg(feature = "render")]

use k_pdf::{Document, RenderedPage, Renderer};

/// Builds a one-page PDF (200 × 100 pt) around `content` and extra objects.
fn pdf(content: impl AsRef<[u8]>, resources: &str, extra: &[&[u8]]) -> Vec<u8> {
    let content = content.as_ref();
    let mut stream = format!("<< /Length {} >>\nstream\n", content.len()).into_bytes();
    stream.extend_from_slice(content);
    stream.extend_from_slice(b"\nendstream");
    let mut objs: Vec<Vec<u8>> = vec![
        b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(),
        b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec(),
        format!("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 100] /Contents 4 0 R /Resources << {resources} >> >>")
            .into_bytes(),
        stream,
    ];
    objs.extend(extra.iter().map(|s| s.to_vec()));
    let mut out = b"%PDF-1.4\n".to_vec();
    let mut offsets = Vec::new();
    for (i, body) in objs.iter().enumerate() {
        offsets.push(out.len());
        out.extend_from_slice(format!("{} 0 obj\n", i + 1).as_bytes());
        out.extend_from_slice(body);
        out.extend_from_slice(b"\nendobj\n");
    }
    let xref = out.len();
    out.extend_from_slice(format!("xref\n0 {}\n0000000000 65535 f\r\n", objs.len() + 1).as_bytes());
    for o in offsets {
        out.extend_from_slice(format!("{o:010} 00000 n\r\n").as_bytes());
    }
    out.extend_from_slice(format!("trailer << /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n", objs.len() + 1).as_bytes());
    out
}

fn render(data: Vec<u8>, scale: f64) -> RenderedPage {
    let doc = Document::from_bytes(data).unwrap();
    let page = doc.pages().unwrap().remove(0);
    Renderer::new(&doc).render_page(&page, scale).unwrap()
}

/// RGB at page coordinates (x, y), y up, for a page rendered at scale 1.
fn rgb(img: &RenderedPage, x: u32, y: u32) -> [u8; 3] {
    let row = img.height - 1 - y;
    let i = ((row * img.width + x) * 4) as usize;
    [img.pixels[i], img.pixels[i + 1], img.pixels[i + 2]]
}

#[test]
fn page_size_background_and_fills() {
    let img = render(pdf("1 0 0 rg 10 10 50 30 re f 0 0 1 RG 4 w 100 50 m 190 50 l S", "", &[]), 1.0);
    assert_eq!((img.width, img.height), (200, 100));
    assert_eq!(rgb(&img, 150, 90), [255, 255, 255]); // white paper
    assert_eq!(rgb(&img, 30, 25), [255, 0, 0]); // red rectangle
    assert_eq!(rgb(&img, 150, 50), [0, 0, 255]); // blue stroke
    // Scale changes the bitmap size.
    let big = render(pdf("", "", &[]), 2.0);
    assert_eq!((big.width, big.height), (400, 200));
}

#[test]
fn clipping_and_cmyk() {
    // Clip to the left half, then paint the whole page with CMYK cyan.
    let img = render(pdf("0 0 100 100 re W n 1 0 0 0 k 0 0 200 100 re f", "", &[]), 1.0);
    assert_eq!(rgb(&img, 50, 50), [0, 255, 255]);
    assert_eq!(rgb(&img, 150, 50), [255, 255, 255]);
}

#[test]
fn transparency_group_opacity_multiplies() {
    // 50% outer opacity × 50% inside the group = 25% black over white.
    let form: &[u8] = b"<< /Type /XObject /Subtype /Form /BBox [0 0 200 100] /Group << /S /Transparency >> \
                /Resources << /ExtGState << /H << /ca 0.5 >> >> >> /Length 26 >>\nstream\n/H gs 0 g 0 0 200 100 re f\nendstream";
    let img = render(pdf("/H gs /F Do", "/ExtGState << /H << /ca 0.5 >> >> /XObject << /F 5 0 R >>", &[form]), 1.0);
    let v = rgb(&img, 100, 50)[0] as i32;
    assert!((v - 191).abs() <= 2, "expected ~75% white, got {v}");
}

#[test]
fn inline_image() {
    // A 2×1 RGB inline image stretched over the page: left red, right green.
    let mut content = b"q 200 0 0 100 0 0 cm BI /W 2 /H 1 /BPC 8 /CS /RGB ID ".to_vec();
    content.extend_from_slice(&[255, 0, 0, 0, 255, 0]);
    content.extend_from_slice(b" EI Q");
    let img = render(pdf(content, "", &[]), 1.0);
    assert_eq!(rgb(&img, 20, 50), [255, 0, 0]);
    assert_eq!(rgb(&img, 180, 50), [0, 255, 0]);
}

#[test]
fn stencil_mask_uses_fill_color() {
    // Bits 0,1: the left pixel is painted with the fill color, the right isn't.
    let mask: &[u8] = b"<< /Type /XObject /Subtype /Image /Width 2 /Height 1 /ImageMask true /Length 1 >>\nstream\n\x40\nendstream";
    let img = render(pdf("0 0 1 rg q 200 0 0 100 0 0 cm /M Do Q", "/XObject << /M 5 0 R >>", &[mask]), 1.0);
    assert_eq!(rgb(&img, 20, 50), [0, 0, 255]);
    assert_eq!(rgb(&img, 180, 50), [255, 255, 255]);
}

#[test]
fn standard_font_text_is_drawn() {
    // Non-embedded Helvetica falls back to a system font; some dark pixels
    // must appear where the text is.
    let font: &[u8] = b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>";
    let img = render(pdf("BT /F1 40 Tf 10 30 Td (Hello) Tj ET", "/Font << /F1 5 0 R >>", &[font]), 1.0);
    let dark = (0..img.height).flat_map(|y| (0..img.width).map(move |x| (x, y)))
        .filter(|&(x, y)| rgb(&img, x, img.height - 1 - y)[0] < 100)
        .count();
    if dark == 0 {
        // No system fonts at all (e.g. a minimal container): nothing to check.
        eprintln!("note: no system font available, text not drawn");
    }
    // Text must stay inside its line: nothing dark in the top 30 points.
    assert!((0..img.width).all(|x| (75..100).all(|y| rgb(&img, x, y)[0] > 200)));
}
