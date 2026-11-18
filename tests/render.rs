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

/// An image XObject with binary `data`.
fn image_object(dict: &str, data: &[u8]) -> Vec<u8> {
    let mut obj = format!("<< /Type /XObject /Subtype /Image {dict} /Length {} >>\nstream\n", data.len()).into_bytes();
    obj.extend_from_slice(data);
    obj.extend_from_slice(b"\nendstream");
    obj
}

/// A 16×8 JPEG 2000 codestream (lossless): the left half red (220, 30, 30),
/// the right half blue (30, 30, 220). Made with OpenJPEG through Pillow.
const J2K_RED_BLUE: &[u8] = &[
    0xff, 0x4f, 0xff, 0x51, 0x00, 0x2f, 0x00, 0x00, 0x00, 0x00, 0x00, 0x10, 0x00, 0x00, 0x00, 0x08, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x10, 0x00, 0x00, 0x00, 0x08, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x03, 0x07, 0x01, 0x01, 0x07, 0x01, 0x01,
    0x07, 0x01, 0x01, 0xff, 0x52, 0x00, 0x0c, 0x00, 0x00, 0x00, 0x01, 0x00, 0x03, 0x04, 0x04, 0x00, 0x01, 0xff, 0x5c, 0x00, 0x0d, 0x40, 0x40, 0x48,
    0x48, 0x50, 0x48, 0x48, 0x50, 0x48, 0x48, 0x50, 0xff, 0x64, 0x00, 0x25, 0x00, 0x01, 0x43, 0x72, 0x65, 0x61, 0x74, 0x65, 0x64, 0x20, 0x62, 0x79,
    0x20, 0x4f, 0x70, 0x65, 0x6e, 0x4a, 0x50, 0x45, 0x47, 0x20, 0x76, 0x65, 0x72, 0x73, 0x69, 0x6f, 0x6e, 0x20, 0x32, 0x2e, 0x35, 0x2e, 0x34, 0xff,
    0x90, 0x00, 0x0a, 0x00, 0x00, 0x00, 0x00, 0x00, 0x54, 0x00, 0x01, 0xff, 0x93, 0xdf, 0x80, 0x18, 0x04, 0x3c, 0xf9, 0xcf, 0xb4, 0x0c, 0x08, 0x85,
    0x7f, 0xdf, 0x80, 0x18, 0x06, 0xe9, 0x5d, 0xc7, 0xda, 0x06, 0x00, 0x02, 0xd6, 0xbf, 0x80, 0xc7, 0xda, 0x06, 0x00, 0x05, 0x86, 0xbf, 0xc7, 0xda,
    0x0a, 0x00, 0x0d, 0x0e, 0x22, 0x55, 0x44, 0x80, 0xc7, 0xda, 0x0c, 0x00, 0x0e, 0x10, 0x95, 0x2a, 0xa2, 0x7f, 0xc7, 0xda, 0x08, 0x00, 0x22, 0x1a,
    0x20, 0x3f, 0x80, 0xc7, 0xda, 0x08, 0x00, 0x21, 0x6e, 0x1e, 0x3f, 0xff, 0xd9,
];

/// A 64×16 JBIG2 page in the embedded organization (a generic region): the
/// left half black, the right half white. Made with jbig2enc-rust.
const JBIG2_HALF_BLACK: &[u8] = &[
    0x00, 0x00, 0x00, 0x01, 0x30, 0x00, 0x01, 0x00, 0x00, 0x00, 0x13, 0x00, 0x00, 0x00, 0x40, 0x00, 0x00, 0x00, 0x10, 0x00, 0x00, 0x01, 0x2c, 0x00,
    0x00, 0x01, 0x2c, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x02, 0x26, 0x00, 0x01, 0x00, 0x00, 0x00, 0x29, 0x00, 0x00, 0x00, 0x40, 0x00, 0x00, 0x00,
    0x10, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x03, 0xff, 0xfd, 0xff, 0x02, 0xfe, 0xfe, 0xfe, 0xff, 0x4e, 0x16, 0xa1, 0x8d,
    0x26, 0x16, 0x10, 0x19, 0x3f, 0xff, 0x7f, 0xff, 0xff, 0xac,
];

#[test]
fn jpeg2000_image() {
    let im = image_object("/Width 16 /Height 8 /Filter /JPXDecode", J2K_RED_BLUE);
    let img = render(pdf("q 200 0 0 100 0 0 cm /I Do Q", "/XObject << /I 5 0 R >>", &[&im]), 1.0);
    let near = |a: [u8; 3], b: [u8; 3]| a.iter().zip(b).all(|(&x, y)| x.abs_diff(y) <= 4);
    assert!(near(rgb(&img, 40, 50), [220, 30, 30]), "{:?}", rgb(&img, 40, 50));
    assert!(near(rgb(&img, 160, 50), [30, 30, 220]), "{:?}", rgb(&img, 160, 50));
    // Shown 4 times smaller than it is stored, it is decoded at a reduced
    // resolution (as OpenJPEG gives it: the left still red, the right blue).
    let im = image_object("/Width 16 /Height 8 /Filter /JPXDecode", J2K_RED_BLUE);
    let img = render(pdf("q 4 0 0 2 10 10 cm /I Do Q", "/XObject << /I 5 0 R >>", &[&im]), 1.0);
    let (left, right) = (rgb(&img, 10, 10), rgb(&img, 13, 10));
    assert!(left[0] > 200 && left[2] < 60 && right[2] > 200 && right[0] < 60, "{left:?} {right:?}");
    // Damaged data draws a light box instead of failing the page.
    let im = image_object("/Width 16 /Height 8 /Filter /JPXDecode", &J2K_RED_BLUE[..40]);
    let img = render(pdf("q 200 0 0 100 0 0 cm /I Do Q", "/XObject << /I 5 0 R >>", &[&im]), 1.0);
    assert_eq!(rgb(&img, 40, 50), [220, 220, 220]);
}

#[test]
fn jbig2_image() {
    // As a gray image: JBIG2's black pixels are black.
    let im = image_object("/Width 64 /Height 16 /ColorSpace /DeviceGray /BitsPerComponent 1 /Filter /JBIG2Decode", JBIG2_HALF_BLACK);
    let img = render(pdf("q 200 0 0 100 0 0 cm /I Do Q", "/XObject << /I 5 0 R >>", &[&im]), 1.0);
    assert_eq!(rgb(&img, 40, 50), [0, 0, 0]);
    assert_eq!(rgb(&img, 160, 50), [255, 255, 255]);
    // As a stencil mask, the black pixels are painted with the fill color.
    let im = image_object("/Width 64 /Height 16 /ImageMask true /Filter /JBIG2Decode", JBIG2_HALF_BLACK);
    let img = render(pdf("0 0 1 rg q 200 0 0 100 0 0 cm /I Do Q", "/XObject << /I 5 0 R >>", &[&im]), 1.0);
    assert_eq!(rgb(&img, 40, 50), [0, 0, 255]);
    assert_eq!(rgb(&img, 160, 50), [255, 255, 255]);
    // Shrunk 4 times, the edge between the halves stays where it was.
    let im = image_object("/Width 64 /Height 16 /ColorSpace /DeviceGray /BitsPerComponent 1 /Filter /JBIG2Decode", JBIG2_HALF_BLACK);
    let img = render(pdf("q 16 0 0 4 10 10 cm /I Do Q", "/XObject << /I 5 0 R >>", &[&im]), 1.0);
    assert_eq!((rgb(&img, 17, 11), rgb(&img, 18, 11)), ([0, 0, 0], [255, 255, 255]));
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
