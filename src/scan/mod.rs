//! Scanning photos of documents.
//!
//! A phone photo of a page is turned into something that looks like it
//! came out of a scanner:
//!
//! 1. [`find_page`] finds the page's four corners in the photo.
//! 2. [`scan`] flattens the page (a perspective transform maps the four
//!    corners to a rectangle), evens out the light (shadows and color casts
//!    are divided away, so the paper turns white), levels slanted text and
//!    makes the ink crisp.
//! 3. [`PdfWriter`] makes a PDF with one scanned image per page.
//!
//! Photos are read from JPEG (turned upright with the camera's Exif
//! orientation), PNG, GIF, BMP, TIFF and WebP. [`PdfWriter::add_image`]
//! also makes pages of images kept as they are.
//!
//! ```no_run
//! use k_pdf::scan::{self, Image, Mode, ScanOptions};
//!
//! let photo = Image::open("page.jpg")?;
//! let quad = scan::find_page(&photo).unwrap_or_else(|| scan::whole(&photo));
//! let page = scan::scan(&photo, &ScanOptions { mode: Mode::Color, ..ScanOptions::new(quad) });
//! let mut pdf = scan::PdfWriter::new(std::fs::File::create("page.pdf")?)?;
//! pdf.add_page(&page)?;
//! pdf.finish()?;
//! # Ok::<(), k_pdf::Error>(())
//! ```

mod clean;
mod detect;
mod formats;
mod png;
mod signature;

use std::io::Write;

use crate::error::{Error, Result};

pub use detect::find_page;
pub use signature::{lift_signature, Ink, Signature};
#[cfg(feature = "render")]
pub use signature::{drawn_signature, typed_signature};

/// The page's corners in a photo, in pixels (x right, y down): top left,
/// top right, bottom right, bottom left.
pub type Quad = [[f32; 2]; 4];

/// Photos larger than this many pixels are refused (a 100-megapixel photo
/// already needs 300 MB).
const MAX_PIXELS: u64 = 120_000_000;

/// An 8-bit image: gray (1 channel) or RGB (3 channels), rows top to bottom.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Image {
    pub width: u32,
    pub height: u32,
    pub channels: u8,
    pub data: Vec<u8>,
}

impl Image {
    /// Reads a JPEG, PNG, GIF, BMP, TIFF or WebP image, turned upright.
    pub fn decode(data: &[u8]) -> Result<Image> {
        match Format::of(data) {
            Some(Format::Jpeg) => decode_jpeg(data),
            Some(Format::Png) => png::decode(data),
            Some(Format::Gif) => formats::decode_gif(data),
            Some(Format::Bmp) => formats::decode_bmp(data),
            Some(Format::Tiff) => formats::decode_tiff(data),
            Some(Format::Webp) => formats::decode_webp(data),
            None if data.get(4..12).is_some_and(|b| b.starts_with(b"ftyphei") || b.starts_with(b"ftypmif")) => {
                Err(Error::Image("HEIC photos can't be read yet; save the photo as JPEG first".into()))
            }
            None => Err(Error::Image("not an image k-pdf can read (JPEG, PNG, GIF, BMP, TIFF or WebP)".into())),
        }
    }

    pub fn open(path: impl AsRef<std::path::Path>) -> Result<Image> {
        Image::decode(&std::fs::read(path)?)
    }

    fn new(width: u32, height: u32, channels: u8) -> Image {
        Image { width, height, channels, data: vec![0; width as usize * height as usize * channels as usize] }
    }

    /// Luminance (Rec. 601), one byte per pixel.
    pub fn luma(&self) -> Vec<u8> {
        match self.channels {
            1 => self.data.clone(),
            _ => self.data.chunks_exact(3).map(|p| luma(p[0], p[1], p[2])).collect(),
        }
    }

    /// The image as RGB.
    pub fn to_rgb(&self) -> Image {
        match self.channels {
            3 => self.clone(),
            _ => Image {
                width: self.width,
                height: self.height,
                channels: 3,
                data: self.data.iter().flat_map(|&v| [v, v, v]).collect(),
            },
        }
    }

    /// A smaller copy whose longer side is at most `max` pixels (each pixel
    /// the average of a block), and the factor it was shrunk by.
    pub fn shrink(&self, max: u32) -> (Image, u32) {
        let k = self.width.max(self.height).div_ceil(max.max(1)).max(1);
        (self.shrink_by(k), k)
    }

    /// Averages `k`×`k` blocks (leftover pixels at the right and bottom
    /// edges are dropped).
    fn shrink_by(&self, k: u32) -> Image {
        if k <= 1 {
            return self.clone();
        }
        let (w, h) = ((self.width / k).max(1), (self.height / k).max(1));
        let c = self.channels as usize;
        let mut out = Image::new(w, h, self.channels);
        let mut sums = vec![0u32; w as usize * c];
        for y in 0..h {
            sums.fill(0);
            for sy in y * k..((y + 1) * k).min(self.height) {
                let row = &self.data[sy as usize * self.width as usize * c..][..self.width as usize * c];
                for x in 0..w as usize {
                    let start = x * k as usize * c;
                    let end = ((x + 1) * k as usize * c).min(row.len());
                    for (i, &v) in row[start..end].iter().enumerate() {
                        sums[x * c + i % c] += v as u32;
                    }
                }
            }
            let n = k * k;
            let out_row = &mut out.data[y as usize * w as usize * c..][..w as usize * c];
            for (o, s) in out_row.iter_mut().zip(&sums) {
                *o = ((s + n / 2) / n) as u8;
            }
        }
        out
    }

    /// The image turned by Exif `orientation` (1–8): 6 is a quarter turn
    /// clockwise, 3 a half turn, 8 a quarter turn counterclockwise, and
    /// 2, 4, 5, 7 the mirrored versions.
    fn oriented(self, orientation: u16) -> Image {
        if !(2..=8).contains(&orientation) {
            return self;
        }
        let (w, h) = (self.width as usize, self.height as usize);
        let c = self.channels as usize;
        let swap = orientation >= 5;
        let (ow, oh) = if swap { (h, w) } else { (w, h) };
        let mut out = Image::new(ow as u32, oh as u32, self.channels);
        for y in 0..oh {
            for x in 0..ow {
                let (sx, sy) = match orientation {
                    2 => (w - 1 - x, y),
                    3 => (w - 1 - x, h - 1 - y),
                    4 => (x, h - 1 - y),
                    5 => (y, x),
                    6 => (y, h - 1 - x),
                    7 => (w - 1 - y, h - 1 - x),
                    _ => (w - 1 - y, x),
                };
                let s = (sy * w + sx) * c;
                let d = (y * ow + x) * c;
                out.data[d..d + c].copy_from_slice(&self.data[s..s + c]);
            }
        }
        out
    }

    /// Turned clockwise by `turns` quarter turns.
    pub fn turned(self, turns: u8) -> Image {
        match turns % 4 {
            1 => self.oriented(6),
            2 => self.oriented(3),
            3 => self.oriented(8),
            _ => self,
        }
    }
}

/// File extensions of the images that can be read.
pub const IMAGE_EXTENSIONS: &[&str] = &["jpg", "jpeg", "jpe", "jfif", "png", "gif", "bmp", "dib", "tif", "tiff", "webp"];

/// Whether `path` names an image that can be read, by its extension.
pub fn is_image_file(path: &std::path::Path) -> bool {
    path.extension().and_then(|e| e.to_str()).is_some_and(|e| IMAGE_EXTENSIONS.iter().any(|x| x.eq_ignore_ascii_case(e)))
}

/// The image formats that can be read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Format {
    Jpeg,
    Png,
    Gif,
    Bmp,
    Tiff,
    Webp,
}

impl Format {
    /// The format of an image file, from its first bytes.
    pub fn of(data: &[u8]) -> Option<Format> {
        if data.starts_with(&[0xFF, 0xD8]) {
            Some(Format::Jpeg)
        } else if data.starts_with(b"\x89PNG\r\n\x1a\n") {
            Some(Format::Png)
        } else if data.starts_with(b"GIF8") && data.len() > 13 {
            Some(Format::Gif)
        } else if data.starts_with(b"BM") && data.len() > 26 {
            Some(Format::Bmp)
        } else if data.starts_with(b"II*\0") || data.starts_with(b"MM\0*") {
            Some(Format::Tiff)
        } else if data.starts_with(b"RIFF") && data.get(8..12) == Some(b"WEBP") {
            Some(Format::Webp)
        } else {
            None
        }
    }

    /// Whether the format keeps every pixel exactly (so pages made from it
    /// are stored without loss too).
    pub fn lossless(self) -> bool {
        !matches!(self, Format::Jpeg | Format::Webp)
    }
}

fn luma(r: u8, g: u8, b: u8) -> u8 {
    ((r as u32 * 299 + g as u32 * 587 + b as u32 * 114 + 500) / 1000) as u8
}

fn decode_jpeg(data: &[u8]) -> Result<Image> {
    let fail = |e: &dyn std::fmt::Display| Error::Image(format!("the JPEG image is damaged ({e})"));
    let mut decoder = crate::jpeg::Decoder::new(data);
    decoder.read_info().map_err(|e| fail(&e))?;
    let info = decoder.info().ok_or_else(|| fail(&"no image"))?;
    if info.width as u64 * info.height as u64 > MAX_PIXELS {
        return Err(Error::Image("the photo is too large".into()));
    }
    let pixels = decoder.decode().map_err(|e| fail(&e))?;
    let (channels, pixels) = match info.pixel_format {
        crate::jpeg::PixelFormat::L8 => (1, pixels),
        crate::jpeg::PixelFormat::L16 => (1, pixels.chunks_exact(2).map(|p| p[0]).collect()),
        crate::jpeg::PixelFormat::RGB24 => (3, pixels),
        // Adobe CMYK JPEGs store inverted values.
        crate::jpeg::PixelFormat::CMYK32 => (
            3,
            pixels
                .chunks_exact(4)
                .flat_map(|p| {
                    let k = p[3] as u32;
                    [p[0], p[1], p[2]].map(|v| (v as u32 * k / 255) as u8)
                })
                .collect(),
        ),
    };
    let image = Image { width: info.width as u32, height: info.height as u32, channels, data: pixels };
    Ok(image.oriented(exif_orientation(data)))
}

/// A JPEG's size and whether it is gray, when it can be put in a PDF as
/// it is (gray or color, 8 bits; not CMYK).
fn jpeg_info(data: &[u8]) -> Option<(u32, u32, bool)> {
    let mut decoder = crate::jpeg::Decoder::new(data);
    decoder.read_info().ok()?;
    let info = decoder.info()?;
    let gray = match info.pixel_format {
        crate::jpeg::PixelFormat::L8 => true,
        crate::jpeg::PixelFormat::RGB24 => false,
        _ => return None,
    };
    Some((info.width as u32, info.height as u32, gray))
}

/// The Exif orientation tag (0x0112) of a JPEG, or 1 (upright).
fn exif_orientation(data: &[u8]) -> u16 {
    let mut i = 2;
    while i + 4 <= data.len() && data[i] == 0xFF {
        let marker = data[i + 1];
        if marker == 0xFF {
            i += 1; // fill byte
            continue;
        }
        if marker == 0xDA || marker == 0xD9 {
            break; // image data: no more headers
        }
        let len = u16::from_be_bytes([data[i + 2], data[i + 3]]) as usize;
        let Some(segment) = data.get(i + 4..i + 2 + len) else { break };
        if marker == 0xE1
            && let Some(tiff) = segment.strip_prefix(b"Exif\0\0")
        {
            return tiff_orientation(tiff).unwrap_or(1);
        }
        i += 2 + len;
    }
    1
}

fn tiff_orientation(t: &[u8]) -> Option<u16> {
    let little = match t.get(0..2)? {
        b"II" => true,
        b"MM" => false,
        _ => return None,
    };
    let u16_at = |o: usize| {
        let b: [u8; 2] = t.get(o..o + 2)?.try_into().ok()?;
        Some(if little { u16::from_le_bytes(b) } else { u16::from_be_bytes(b) })
    };
    let u32_at = |o: usize| {
        let b: [u8; 4] = t.get(o..o + 4)?.try_into().ok()?;
        Some(if little { u32::from_le_bytes(b) } else { u32::from_be_bytes(b) })
    };
    let ifd = u32_at(4)? as usize;
    let entries = u16_at(ifd)? as usize;
    (0..entries).find_map(|k| {
        let e = ifd + 2 + 12 * k;
        (u16_at(e)? == 0x0112).then(|| u16_at(e + 8)).flatten()
    })
}

/// The longer side, in pixels, that PDF pages are rendered at to be
/// scanned: 300 dpi for Letter, 280 for A4, enough for the ink of a scan
/// (handwriting included).
pub const PDF_PAGE_PIXELS: f32 = 3300.0;

/// Page `page` of `doc` as an RGB image whose longer side is `max` pixels
/// (to scan a page of a PDF: a scan, or a photo, saved as a PDF).
#[cfg(feature = "render")]
pub fn pdf_page_image(doc: &crate::Document, renderer: &mut crate::Renderer, page: &crate::Page, max: f32) -> Result<Image> {
    let (w, h) = page.display_size(doc);
    let scale = max as f64 / w.max(h).max(1.0);
    let r = renderer.render_page(page, scale)?;
    let data = r.pixels.chunks_exact(4).flat_map(|p| [p[0], p[1], p[2]]).collect();
    Ok(Image { width: r.width, height: r.height, channels: 3, data })
}

/// The corners of the whole photo (when no page was found).
pub fn whole(image: &Image) -> Quad {
    let (w, h) = (image.width as f32, image.height as f32);
    [[0.0, 0.0], [w, 0.0], [w, h], [0.0, h]]
}

/// How a scanned page is cleaned up.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Mode {
    /// White paper, crisp ink, colors kept.
    #[default]
    Color,
    /// White paper and crisp gray-scale ink.
    Gray,
    /// Pure black and white: the smallest files, best for text.
    BlackWhite,
    /// The flattened photo as it was (no clean-up).
    Original,
}

impl Mode {
    pub const ALL: [Mode; 4] = [Mode::Color, Mode::Gray, Mode::BlackWhite, Mode::Original];

    pub fn label(self) -> &'static str {
        match self {
            Mode::Color => "Color",
            Mode::Gray => "Grayscale",
            Mode::BlackWhite => "Black & white",
            Mode::Original => "Original photo",
        }
    }

    /// The name used on the command line.
    pub fn name(self) -> &'static str {
        match self {
            Mode::Color => "color",
            Mode::Gray => "gray",
            Mode::BlackWhite => "bw",
            Mode::Original => "original",
        }
    }

    pub fn from_name(name: &str) -> Option<Mode> {
        Mode::ALL.into_iter().find(|m| m.name() == name)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct ScanOptions {
    /// The page's corners in the photo.
    pub quad: Quad,
    pub mode: Mode,
    /// Level text that is slightly slanted (up to 8°).
    pub straighten: bool,
    /// Quarter turns clockwise, applied last.
    pub turns: u8,
    /// Clean up for handwriting: faint ink and pencil are darkened where
    /// they are (so light strokes come out like the others), the paper's
    /// ruled lines are kept light (and left out of black and white), and
    /// black and white keeps faint strokes whole. Printed text would come
    /// out bolder, so this is off by default.
    pub handwriting: bool,
    /// The longer side of the result, at most (pixels). Pages are never
    /// enlarged beyond the photo's own resolution.
    pub max_size: u32,
}

impl ScanOptions {
    pub fn new(quad: Quad) -> ScanOptions {
        // 3300 pixels is A4 at 280 dpi.
        ScanOptions { quad, mode: Mode::Color, straighten: true, turns: 0, handwriting: false, max_size: 3300 }
    }
}

/// A scanned page.
#[derive(Clone, Debug)]
pub struct Scanned {
    pub image: Image,
    /// Only black (0) and white (255) pixels: stored as 1 bit per pixel.
    pub bilevel: bool,
}

/// Scans the page at `options.quad` in `photo`.
pub fn scan(photo: &Image, options: &ScanOptions) -> Scanned {
    clean::scan(photo, options)
}

/// Writes scanned pages to a PDF, one page at a time (so a long scan
/// doesn't need all its pages in memory).
pub struct PdfWriter<W: Write> {
    out: W,
    /// Bytes written so far.
    pos: u64,
    /// Offset of each object, by number (object 1 is the catalog and 2 the
    /// page tree; they are written last).
    offsets: Vec<u64>,
    pages: Vec<u32>,
}

impl<W: Write> PdfWriter<W> {
    pub fn new(out: W) -> Result<PdfWriter<W>> {
        let mut w = PdfWriter { out, pos: 0, offsets: vec![0, 0, 0], pages: Vec::new() };
        w.put(b"%PDF-1.4\n%\xE2\xE3\xCF\xD3\n")?;
        Ok(w)
    }

    fn put(&mut self, bytes: &[u8]) -> Result<()> {
        self.out.write_all(bytes)?;
        self.pos += bytes.len() as u64;
        Ok(())
    }

    fn object(&mut self, number: u32, dict: &str, stream: Option<&[u8]>) -> Result<()> {
        self.offsets[number as usize] = self.pos;
        self.put(format!("{number} 0 obj\n{dict}").as_bytes())?;
        if let Some(data) = stream {
            self.put(b"\nstream\n")?;
            self.put(data)?;
            self.put(b"\nendstream")?;
        }
        self.put(b"\nendobj\n")
    }

    fn reserve(&mut self) -> u32 {
        self.offsets.push(0);
        self.offsets.len() as u32 - 1
    }

    /// Adds a page showing `page`, sized like the paper it most likely is
    /// (A4, Letter or Legal when the proportions match, otherwise fitting
    /// on A4).
    pub fn add_page(&mut self, page: &Scanned) -> Result<()> {
        let img = &page.image;
        if page.bilevel {
            let data = deflate(&pack_bits(img));
            return self.image_page(img.width, img.height, "/DeviceGray", 1, "/FlateDecode", &data);
        }
        self.add_pixels(img, false)
    }

    /// Adds a page showing an image file (`data`) as it is, turned
    /// clockwise by `turns` quarter turns. A JPEG that needs no turning is
    /// copied in unchanged; images from formats that keep every pixel
    /// (PNG, GIF, BMP, TIFF) are stored without loss, others as JPEG.
    pub fn add_image(&mut self, data: &[u8], turns: u8) -> Result<()> {
        let format = Format::of(data).ok_or_else(|| Error::Image("not an image k-pdf can read".into()))?;
        if format == Format::Jpeg
            && turns.is_multiple_of(4)
            && exif_orientation(data) == 1
            && let Some((w, h, gray)) = jpeg_info(data)
        {
            let color = if gray { "/DeviceGray" } else { "/DeviceRGB" };
            return self.image_page(w, h, color, 8, "/DCTDecode", data);
        }
        let image = Image::decode(data)?.turned(turns);
        self.add_pixels(&image, format.lossless())
    }

    /// Adds a page showing `img`: without loss (Deflate, or 1 bit per
    /// pixel when it is only black and white) or as a JPEG. Pictures that
    /// are photos in all but name (a photo saved as PNG) compress badly
    /// without loss, so they become JPEGs (quality 90) when that is
    /// several times smaller.
    fn add_pixels(&mut self, img: &Image, lossless: bool) -> Result<()> {
        // Color images without color are stored as gray: a third smaller.
        let gray;
        let (pixels, channels) = if img.channels == 3 && is_gray(img) {
            gray = img.luma();
            (&gray[..], 1)
        } else {
            (&img.data[..], img.channels)
        };
        let color = if channels == 1 { "/DeviceGray" } else { "/DeviceRGB" };
        if lossless {
            if channels == 1 && pixels.iter().all(|&v| v == 0 || v == 255) {
                let bilevel = Image { width: img.width, height: img.height, channels: 1, data: pixels.to_vec() };
                return self.image_page(img.width, img.height, color, 1, "/FlateDecode", &deflate(&pack_bits(&bilevel)));
            }
            let flate = deflate(pixels);
            match crate::compress::jpeg::encode(pixels, img.width, img.height, channels, 90) {
                Some(jpeg) if jpeg.len() * 3 < flate.len() => {
                    return self.image_page(img.width, img.height, color, 8, "/DCTDecode", &jpeg);
                }
                _ => return self.image_page(img.width, img.height, color, 8, "/FlateDecode", &flate),
            }
        }
        let jpeg = crate::compress::jpeg::encode(pixels, img.width, img.height, channels, 85)
            .ok_or_else(|| Error::Image("could not encode the page as JPEG".into()))?;
        self.image_page(img.width, img.height, color, 8, "/DCTDecode", &jpeg)
    }

    /// Writes a page that shows one image, `width`×`height` pixels, encoded
    /// as `filter`.
    fn image_page(&mut self, width: u32, height: u32, color: &str, bits: u8, filter: &str, data: &[u8]) -> Result<()> {
        let (w, h) = page_size(width, height);
        let page_ref = self.reserve();
        let contents = self.reserve();
        let image = self.reserve();
        self.pages.push(page_ref);
        self.object(
            page_ref,
            &format!(
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 {} {}] /Resources << /XObject << /Im0 {image} 0 R >> >> /Contents {contents} 0 R >>",
                num(w),
                num(h)
            ),
            None,
        )?;
        let draw = format!("q {} 0 0 {} 0 0 cm /Im0 Do Q", num(w), num(h));
        self.object(contents, &format!("<< /Length {} >>", draw.len()), Some(draw.as_bytes()))?;
        self.object(
            image,
            &format!(
                "<< /Type /XObject /Subtype /Image /Width {width} /Height {height} /ColorSpace {color} /BitsPerComponent {bits} /Filter {filter} /Length {} >>",
                data.len()
            ),
            Some(data),
        )
    }

    /// Writes the page tree, catalog and cross-reference table.
    pub fn finish(mut self) -> Result<W> {
        let kids: Vec<String> = self.pages.iter().map(|p| format!("{p} 0 R")).collect();
        self.object(2, &format!("<< /Type /Pages /Kids [{}] /Count {} >>", kids.join(" "), kids.len()), None)?;
        self.object(1, "<< /Type /Catalog /Pages 2 0 R >>", None)?;
        let info = self.reserve();
        self.object(info, "<< /Producer (k-pdf) >>", None)?;
        let xref = self.pos;
        let mut table = format!("xref\n0 {}\n0000000000 65535 f \n", self.offsets.len());
        for &offset in &self.offsets[1..] {
            table.push_str(&format!("{offset:010} 00000 n \n"));
        }
        table.push_str(&format!(
            "trailer\n<< /Size {} /Root 1 0 R /Info {info} 0 R >>\nstartxref\n{xref}\n%%EOF\n",
            self.offsets.len()
        ));
        self.put(table.as_bytes())?;
        self.out.flush()?;
        Ok(self.out)
    }
}

/// Paper sizes in points, portrait.
const PAPERS: [(f32, f32); 3] = [(595.28, 841.89), (612.0, 792.0), (612.0, 1008.0)];

/// The page size in points for an image of `w`×`h` pixels.
fn page_size(w: u32, h: u32) -> (f32, f32) {
    let (w, h) = (w as f32, h as f32);
    let landscape = w > h;
    let aspect = w.max(h) / w.min(h);
    for (short, long) in PAPERS {
        if (aspect / (long / short) - 1.0).abs() < 0.01 {
            return if landscape { (long, short) } else { (short, long) };
        }
    }
    let (short, long) = PAPERS[0];
    let (bw, bh) = if landscape { (long, short) } else { (short, long) };
    let scale = (bw / w).min(bh / h);
    (w * scale, h * scale)
}

fn num(v: f32) -> String {
    let s = format!("{v:.2}");
    s.trim_end_matches('0').trim_end_matches('.').to_string()
}

/// Whether an RGB image has (practically) no color.
fn is_gray(img: &Image) -> bool {
    let colored = img
        .data
        .chunks_exact(3)
        .filter(|p| p[0].abs_diff(p[1]) > 14 || p[1].abs_diff(p[2]) > 14 || p[0].abs_diff(p[2]) > 14)
        .count();
    colored * 1000 < img.data.len() / 3
}

/// 1 bit per pixel, rows padded to bytes; 1 is white.
fn pack_bits(img: &Image) -> Vec<u8> {
    let (w, h) = (img.width as usize, img.height as usize);
    let row_bytes = w.div_ceil(8);
    let mut out = vec![0u8; row_bytes * h];
    for y in 0..h {
        for x in 0..w {
            if img.data[(y * w + x) * img.channels as usize] >= 128 {
                out[y * row_bytes + x / 8] |= 0x80 >> (x % 8);
            }
        }
    }
    out
}

fn deflate(data: &[u8]) -> Vec<u8> {
    let mut e = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::best());
    e.write_all(data).expect("writing to memory");
    e.finish().expect("writing to memory")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn numbered(w: u32, h: u32) -> Image {
        Image { width: w, height: h, channels: 1, data: (0..w * h).map(|i| i as u8).collect() }
    }

    #[test]
    fn orientations_turn_the_image_upright() {
        // 3×2:  0 1 2
        //       3 4 5
        let img = numbered(3, 2);
        let cw = img.clone().oriented(6);
        assert_eq!((cw.width, cw.height), (2, 3));
        assert_eq!(cw.data, [3, 0, 4, 1, 5, 2]);
        assert_eq!(img.clone().oriented(8).data, [2, 5, 1, 4, 0, 3]);
        assert_eq!(img.clone().oriented(3).data, [5, 4, 3, 2, 1, 0]);
        assert_eq!(img.clone().turned(1).turned(3), img);
    }

    #[test]
    fn exif_orientation_is_found() {
        // APP1 with a big-endian TIFF header and one IFD entry.
        let mut tiff = b"MM\0\x2A\0\0\0\x08\0\x01".to_vec();
        tiff.extend([0x01, 0x12, 0, 3, 0, 0, 0, 1, 0, 6, 0, 0]);
        let mut app1 = b"Exif\0\0".to_vec();
        app1.extend(&tiff);
        let mut jpeg = vec![0xFF, 0xD8, 0xFF, 0xE1];
        jpeg.extend(((app1.len() + 2) as u16).to_be_bytes());
        jpeg.extend(&app1);
        jpeg.extend([0xFF, 0xDA]);
        assert_eq!(exif_orientation(&jpeg), 6);
        assert_eq!(exif_orientation(&[0xFF, 0xD8, 0xFF, 0xDA]), 1);
    }

    #[test]
    fn paper_sizes_are_recognized() {
        assert_eq!(page_size(2480, 3508), (595.28, 841.89));
        assert_eq!(page_size(3300, 2550), (792.0, 612.0));
        let (w, h) = page_size(800, 3000); // a receipt
        assert!((h - 841.89).abs() < 0.01 && (w - 224.5).abs() < 0.1);
    }

    #[test]
    fn written_pdfs_open() {
        let mut gray = numbered(64, 90);
        gray.data.iter_mut().for_each(|v| *v = if *v > 128 { 255 } else { 0 });
        let mut out = PdfWriter::new(Vec::new()).unwrap();
        out.add_page(&Scanned { image: gray.clone(), bilevel: true }).unwrap();
        out.add_page(&Scanned { image: gray.to_rgb(), bilevel: false }).unwrap();
        let bytes = out.finish().unwrap();
        let path = std::env::temp_dir().join(format!("k-pdf-scan-test-{}.pdf", std::process::id()));
        std::fs::write(&path, &bytes).unwrap();
        let doc = crate::Document::open(&path).unwrap();
        assert_eq!(doc.page_count().unwrap(), 2);
        std::fs::remove_file(&path).ok();
    }
}
