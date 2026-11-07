//! Printing on Windows, through GDI: the printers and what they can do
//! come from the spooler (paper sizes, two-sided printing, color); a job
//! sets the printer's options (copies, collating, two-sided, orientation,
//! paper, color) in its DEVMODE, then each sheet is rendered by k-pdf's
//! renderer (at up to 300 dpi) and drawn on the printer's page. The pages
//! per sheet, scaling and black and white are done here.

use std::path::Path;
use std::ptr::{null, null_mut};

use eframe::egui::{Pos2, Rect, Vec2};
use windows_sys::Win32::Foundation::POINT;
use windows_sys::Win32::Graphics::Gdi::{
    CreateDCW, DeleteDC, GetDeviceCaps, SetStretchBltMode, StretchDIBits, BITMAPINFO, BITMAPINFOHEADER, BI_RGB,
    DEVMODEW, DIB_RGB_COLORS, DMCOLLATE_FALSE, DMCOLLATE_TRUE, DMCOLOR_COLOR, DMCOLOR_MONOCHROME, DMDUP_HORIZONTAL,
    DMDUP_SIMPLEX, DMDUP_VERTICAL, DM_COLLATE, DM_COLOR, DM_COPIES, DM_DUPLEX, DM_IN_BUFFER, DM_ORIENTATION,
    DM_OUT_BUFFER, DM_PAPERSIZE, HALFTONE, HDC, HORZRES, LOGPIXELSX, SRCCOPY, VERTRES,
};
use windows_sys::Win32::Graphics::Printing::{
    ClosePrinter, DocumentPropertiesW, EnumPrintersW, GetDefaultPrinterW, OpenPrinterW, PRINTER_ENUM_CONNECTIONS,
    PRINTER_ENUM_LOCAL, PRINTER_HANDLE, PRINTER_INFO_4W,
};
use windows_sys::Win32::Storage::Xps::{
    AbortDoc, DeviceCapabilitiesW, EndDoc, EndPage, StartDocW, StartPage, DC_COLORDEVICE, DC_COPIES, DC_DUPLEX,
    DC_PAPERNAMES, DC_PAPERS, DC_PAPERSIZE, DOCINFOW,
};

use super::{cells, place, sheet_landscape, Job, Paper, Printer, Sides};

/// Most pixels per inch pages are rendered at (more only costs memory).
const MAX_DPI: f32 = 300.0;

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}

/// A NUL-terminated wide string.
unsafe fn from_wide(p: *const u16) -> String {
    if p.is_null() {
        return String::new();
    }
    let mut len = 0;
    // SAFETY: the caller passes a NUL-terminated string.
    unsafe {
        while *p.add(len) != 0 {
            len += 1;
        }
        String::from_utf16_lossy(std::slice::from_raw_parts(p, len))
    }
}

pub fn printers() -> Result<Vec<Printer>, String> {
    let flags = PRINTER_ENUM_LOCAL | PRINTER_ENUM_CONNECTIONS;
    let (mut needed, mut count) = (0u32, 0u32);
    // SAFETY: sizes first, then a buffer of that size (u64s, so the
    // structures in it are aligned).
    let names: Vec<String> = unsafe {
        EnumPrintersW(flags, null(), 4, null_mut(), 0, &mut needed, &mut count);
        if needed == 0 {
            return Ok(Vec::new());
        }
        let mut buf = vec![0u64; needed as usize / 8 + 1];
        if EnumPrintersW(flags, null(), 4, buf.as_mut_ptr().cast(), needed, &mut needed, &mut count) == 0 {
            return Err("Windows couldn't list the printers".into());
        }
        let infos = std::slice::from_raw_parts(buf.as_ptr().cast::<PRINTER_INFO_4W>(), count as usize);
        infos.iter().map(|i| from_wide(i.pPrinterName)).collect()
    };
    let default = default_printer();
    Ok(names
        .into_iter()
        .map(|name| {
            let mut p = capabilities(&name);
            p.is_default = default.as_deref() == Some(name.as_str());
            p
        })
        .collect())
}

fn default_printer() -> Option<String> {
    let mut len = 0u32;
    // SAFETY: length first, then a buffer of that length.
    unsafe {
        GetDefaultPrinterW(null_mut(), &mut len);
        if len == 0 {
            return None;
        }
        let mut buf = vec![0u16; len as usize];
        (GetDefaultPrinterW(buf.as_mut_ptr(), &mut len) != 0).then(|| from_wide(buf.as_ptr()))
    }
}

fn capabilities(name: &str) -> Printer {
    let n = wide(name);
    // SAFETY: each call asks for the count first, then fills a buffer of
    // the documented size (paper names are 64 characters each, sizes are
    // POINTs in tenths of a millimeter).
    let (papers, duplex, color) = unsafe {
        let count = DeviceCapabilitiesW(n.as_ptr(), null(), DC_PAPERS, null_mut(), null());
        let mut papers = Vec::new();
        if count > 0 {
            let count = count as usize;
            let mut ids = vec![0u16; count];
            let mut names = vec![0u16; count * 64];
            let mut sizes = vec![POINT { x: 0, y: 0 }; count];
            DeviceCapabilitiesW(n.as_ptr(), null(), DC_PAPERS, ids.as_mut_ptr(), null());
            DeviceCapabilitiesW(n.as_ptr(), null(), DC_PAPERNAMES, names.as_mut_ptr(), null());
            DeviceCapabilitiesW(n.as_ptr(), null(), DC_PAPERSIZE, sizes.as_mut_ptr().cast(), null());
            for i in 0..count {
                let raw = &names[i * 64..(i + 1) * 64];
                let end = raw.iter().position(|&c| c == 0).unwrap_or(64);
                let label = String::from_utf16_lossy(&raw[..end]);
                let size = (sizes[i].x > 0 && sizes[i].y > 0).then(|| (sizes[i].x as f32 / 10.0, sizes[i].y as f32 / 10.0));
                papers.push(Paper { id: ids[i].to_string(), label, size_mm: size });
            }
        }
        let duplex = DeviceCapabilitiesW(n.as_ptr(), null(), DC_DUPLEX, null_mut(), null()) == 1;
        let color = DeviceCapabilitiesW(n.as_ptr(), null(), DC_COLORDEVICE, null_mut(), null()) == 1;
        (papers, duplex, color)
    };
    let default_paper = DevMode::open(name).ok().map(|d| {
        // SAFETY: the DEVMODE came from the driver.
        unsafe { (*d.devmode()).Anonymous1.Anonymous1.dmPaperSize.to_string() }
    });
    Printer {
        name: name.to_string(),
        label: name.to_string(),
        is_default: false,
        papers,
        default_paper,
        duplex,
        color,
        gray_option: None,
    }
}

/// A printer's settings (DEVMODE) from its driver, with room for the
/// driver's private part.
struct DevMode {
    printer: PRINTER_HANDLE,
    name: Vec<u16>,
    buffer: Vec<u64>,
}

impl DevMode {
    fn open(name: &str) -> Result<DevMode, String> {
        let name = wide(name);
        let mut printer = PRINTER_HANDLE { Value: null_mut() };
        // SAFETY: the size is asked for first, then the buffer is that
        // large (u64s, so it is aligned for DEVMODEW).
        unsafe {
            if OpenPrinterW(name.as_ptr(), &mut printer, null()) == 0 {
                return Err("the printer couldn't be opened".into());
            }
            let size = DocumentPropertiesW(null_mut(), printer, name.as_ptr(), null_mut(), null(), 0);
            if size <= 0 {
                ClosePrinter(printer);
                return Err("the printer's settings couldn't be read".into());
            }
            let mut buffer = vec![0u64; size as usize / 8 + 1];
            let ok = DocumentPropertiesW(null_mut(), printer, name.as_ptr(), buffer.as_mut_ptr().cast(), null(), DM_OUT_BUFFER);
            if ok < 0 {
                ClosePrinter(printer);
                return Err("the printer's settings couldn't be read".into());
            }
            Ok(DevMode { printer, name, buffer })
        }
    }

    fn devmode(&self) -> *const DEVMODEW {
        self.buffer.as_ptr().cast()
    }

    fn devmode_mut(&mut self) -> *mut DEVMODEW {
        self.buffer.as_mut_ptr().cast()
    }

    /// Lets the driver check and complete the changed settings.
    fn merge(&mut self) {
        let dm = self.devmode_mut();
        // SAFETY: input and output are the same valid DEVMODE buffer, as
        // the API allows.
        unsafe {
            DocumentPropertiesW(null_mut(), self.printer, self.name.as_ptr(), dm, dm, DM_IN_BUFFER | DM_OUT_BUFFER);
        }
    }
}

impl Drop for DevMode {
    fn drop(&mut self) {
        // SAFETY: opened in `open`.
        unsafe {
            ClosePrinter(self.printer);
        }
    }
}

pub fn print(file: &Path, job: &Job) -> Result<String, String> {
    let s = &job.settings;
    let doc = k_pdf::Document::open(file).map_err(|e| e.to_string())?;
    let pages = doc.pages().map_err(|e| e.to_string())?;
    if pages.is_empty() {
        return Err("there is nothing to print".into());
    }
    let first = pages[0].display_size(&doc);
    let landscape = sheet_landscape(s, Vec2::new(first.0 as f32, first.1 as f32));

    let name = wide(&job.printer.name);
    let mut settings = DevMode::open(&job.printer.name)?;
    // Copies the driver can't make are printed one after another.
    // SAFETY: a capability query without output.
    let driver_copies = unsafe { DeviceCapabilitiesW(name.as_ptr(), null(), DC_COPIES, null_mut(), null()) }.max(1) as u32;
    let copies = s.copies.max(1);
    let (driver, manual) = if driver_copies >= copies { (copies, 1) } else { (1, copies) };
    // SAFETY: the DEVMODE buffer is valid and large enough.
    unsafe {
        let dm = &mut *settings.devmode_mut();
        let common = &mut dm.Anonymous1.Anonymous1;
        dm.dmFields |= DM_COPIES | DM_COLLATE | DM_ORIENTATION | DM_COLOR;
        common.dmCopies = driver as i16;
        common.dmOrientation = if landscape { 2 } else { 1 };
        dm.dmCollate = if s.collate { DMCOLLATE_TRUE } else { DMCOLLATE_FALSE };
        dm.dmColor = if s.gray { DMCOLOR_MONOCHROME } else { DMCOLOR_COLOR };
        if job.printer.duplex {
            dm.dmFields |= DM_DUPLEX;
            dm.dmDuplex = match s.sides {
                Sides::One => DMDUP_SIMPLEX,
                Sides::LongEdge => DMDUP_VERTICAL,
                Sides::ShortEdge => DMDUP_HORIZONTAL,
            };
        }
        if let Some(paper) = s.paper.as_ref().and_then(|p| p.parse::<i16>().ok()) {
            dm.dmFields |= DM_PAPERSIZE;
            common.dmPaperSize = paper;
        }
    }
    settings.merge();

    // SAFETY: a printer DC for the settings above; closed below.
    let dc = unsafe { CreateDCW(null(), name.as_ptr(), null(), settings.devmode()) };
    if dc.is_null() {
        return Err("the printer couldn't be prepared".into());
    }
    let result = print_sheets(dc, &doc, &pages, job, landscape, manual);
    // SAFETY: created above.
    unsafe {
        DeleteDC(dc);
    }
    result?;
    let n = job.positions.len();
    let what = if n == 1 { "1 page".to_string() } else { format!("{n} pages") };
    Ok(format!("Sent {what} to {}.", job.printer.label))
}

fn print_sheets(
    dc: HDC,
    doc: &k_pdf::Document,
    pages: &[k_pdf::Page],
    job: &Job,
    landscape: bool,
    manual_copies: u32,
) -> Result<(), String> {
    let s = &job.settings;
    let title = wide(&job.title);
    let info = DOCINFOW {
        cbSize: std::mem::size_of::<DOCINFOW>() as i32,
        lpszDocName: title.as_ptr(),
        lpszOutput: null(),
        lpszDatatype: null(),
        fwType: 0,
    };
    // SAFETY: plain GDI calls on a valid printer DC.
    unsafe {
        if StartDocW(dc, &info) <= 0 {
            return Err("the printer didn't accept the job".into());
        }
        SetStretchBltMode(dc, HALFTONE);
    }
    // SAFETY: device capability queries.
    let (dpi, width, height) =
        unsafe { (GetDeviceCaps(dc, LOGPIXELSX as i32) as f32, GetDeviceCaps(dc, HORZRES as i32), GetDeviceCaps(dc, VERTRES as i32)) };
    let sheet = Rect::from_min_size(Pos2::ZERO, Vec2::new(width as f32, height as f32));
    let per_point = dpi / 72.0;
    let mut renderer = k_pdf::Renderer::new(doc);

    let sheets: Vec<&[k_pdf::Page]> = pages.chunks(s.per_sheet.max(1) as usize).collect();
    // Copies made here: whole copies (collated) or each sheet repeated.
    let order: Vec<usize> = if s.collate {
        (0..manual_copies).flat_map(|_| 0..sheets.len()).collect()
    } else {
        (0..sheets.len()).flat_map(|i| std::iter::repeat_n(i, manual_copies as usize)).collect()
    };
    for i in order {
        // SAFETY: as above.
        unsafe {
            StartPage(dc);
        }
        let gap = if s.per_sheet > 1 { 0.01 * dpi } else { 0.0 };
        for (cell, page) in cells(sheet, s.per_sheet, landscape).into_iter().zip(sheets[i]) {
            let (w, h) = page.display_size(doc);
            let size = Vec2::new(w as f32, h as f32);
            let dest = place(cell.shrink(gap), size, s.scaling, s.per_sheet, per_point);
            let px_per_point = (dest.width() / size.x).min(MAX_DPI / 72.0);
            let rendered = match renderer.render_page(page, px_per_point as f64) {
                Ok(r) => r,
                Err(e) => {
                    // SAFETY: as above.
                    unsafe {
                        AbortDoc(dc);
                    }
                    return Err(e.to_string());
                }
            };
            draw(dc, &rendered, dest, s.gray);
        }
        // SAFETY: as above.
        unsafe {
            EndPage(dc);
        }
    }
    // SAFETY: as above.
    if unsafe { EndDoc(dc) } <= 0 {
        return Err("the printer didn't finish the job".into());
    }
    Ok(())
}

/// Draws a rendered page (RGBA) into `dest` (device pixels).
fn draw(dc: HDC, page: &k_pdf::RenderedPage, dest: Rect, gray: bool) {
    // GDI wants BGR(A), rows top to bottom when the height is negative.
    let mut bits = Vec::with_capacity(page.pixels.len());
    for px in page.pixels.chunks_exact(4) {
        if gray {
            let l = ((px[0] as u32 * 299 + px[1] as u32 * 587 + px[2] as u32 * 114) / 1000) as u8;
            bits.extend_from_slice(&[l, l, l, 0]);
        } else {
            bits.extend_from_slice(&[px[2], px[1], px[0], 0]);
        }
    }
    let info = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: page.width as i32,
            biHeight: -(page.height as i32),
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB,
            ..Default::default()
        },
        bmiColors: Default::default(),
    };
    // SAFETY: `bits` holds width × height 32-bit pixels as described.
    unsafe {
        StretchDIBits(
            dc,
            dest.left().round() as i32,
            dest.top().round() as i32,
            dest.width().round() as i32,
            dest.height().round() as i32,
            0,
            0,
            page.width as i32,
            page.height as i32,
            bits.as_ptr().cast(),
            &info,
            DIB_RGB_COLORS,
            SRCCOPY,
        );
    }
}
