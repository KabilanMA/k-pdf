//! Lossless JPEG optimization with MozJPEG (as `jpegtran -optimize` does).
//!
//! The image's DCT coefficients are copied unchanged into a new file; only
//! their entropy coding changes (optimal Huffman tables, and progressive
//! scans when that is smaller). Every marker of the original (JFIF, Exif,
//! ICC profile, Adobe color transform, comments) is copied as it was. The
//! result decodes to exactly the same pixels; this is checked by decoding
//! both before the new file is used.

use std::ffi::{c_ulong, c_void};
use std::panic::{self, AssertUnwindSafe};

use mozjpeg_sys::*;

unsafe extern "C" {
    fn free(p: *mut c_void);
}

/// libjpeg reports fatal errors through `error_exit`, which must not
/// return: unwind back to `catch_unwind` in [`optimize`].
unsafe extern "C-unwind" fn error_exit(_: &mut jpeg_common_struct) {
    panic::resume_unwind(Box::new("libjpeg error"));
}

/// Warnings and messages: stay quiet.
unsafe extern "C-unwind" fn emit_message(_: &mut jpeg_common_struct, _: std::ffi::c_int) {}
unsafe extern "C-unwind" fn output_message(_: &mut jpeg_common_struct) {}

/// The smallest lossless re-encoding of `data` (progressive or baseline
/// with optimized tables), if it is smaller and decodes to the same pixels.
pub(crate) fn optimize(data: &[u8]) -> Option<Vec<u8>> {
    let original = pixels(data)?;
    let mut best: Option<Vec<u8>> = None;
    for progressive in [true, false] {
        let Some(out) = transcode(data, progressive) else { continue };
        if out.len() < best.as_ref().map_or(data.len(), Vec::len) && pixels(&out).as_ref() == Some(&original) {
            best = Some(out);
        }
    }
    best
}

/// Decoded pixels (and their layout), for comparing.
fn pixels(data: &[u8]) -> Option<(Vec<u8>, u16, u16)> {
    let mut d = jpeg_decoder::Decoder::new(data);
    let px = d.decode().ok()?;
    let info = d.info()?;
    Some((px, info.width, info.height))
}

fn transcode(data: &[u8], progressive: bool) -> Option<Vec<u8>> {
    // Boxed, so they stay put while libjpeg points into them, and can be
    // destroyed after an error.
    let mut src_err: Box<jpeg_error_mgr> = Box::new(unsafe { std::mem::zeroed() });
    let mut dst_err: Box<jpeg_error_mgr> = Box::new(unsafe { std::mem::zeroed() });
    let mut src: Box<jpeg_decompress_struct> = Box::new(unsafe { std::mem::zeroed() });
    let mut dst: Box<jpeg_compress_struct> = Box::new(unsafe { std::mem::zeroed() });
    let mut buffer: *mut u8 = std::ptr::null_mut();
    let mut size: c_ulong = 0;

    let result = panic::catch_unwind(AssertUnwindSafe(|| unsafe {
        for err in [&mut *src_err, &mut *dst_err] {
            jpeg_std_error(err);
            err.error_exit = Some(error_exit);
            err.emit_message = Some(emit_message);
            err.output_message = Some(output_message);
        }
        src.common.err = &mut *src_err;
        jpeg_create_decompress(&mut *src);
        dst.common.err = &mut *dst_err;
        jpeg_create_compress(&mut *dst);

        jpeg_mem_src(&mut src, data.as_ptr(), data.len() as c_ulong);
        // Keep every APPn and COM marker, to copy them.
        for m in 0..16 {
            jpeg_save_markers(&mut src, jpeg_marker::APP0 as i32 + m, 0xFFFF);
        }
        jpeg_save_markers(&mut src, jpeg_marker::COM as i32, 0xFFFF);
        jpeg_read_header(&mut src, 1);
        let coefficients = jpeg_read_coefficients(&mut src);
        if coefficients.is_null() {
            return false;
        }

        jpeg_mem_dest(&mut dst, &mut buffer, &mut size);
        if !progressive {
            // libjpeg defaults: baseline (sequential) scans.
            jpeg_c_set_int_param(&mut dst, J_INT_PARAM::JINT_COMPRESS_PROFILE, JCP_FASTEST as i32);
        }
        jpeg_copy_critical_parameters(&src, &mut dst);
        dst.optimize_coding = 1;
        if progressive {
            jpeg_simple_progression(&mut dst);
        }
        // The original's own markers are copied instead.
        dst.write_JFIF_header = 0;
        dst.write_Adobe_marker = 0;
        jpeg_write_coefficients(&mut dst, coefficients);
        let mut marker = src.marker_list;
        while !marker.is_null() {
            let m = &*marker;
            jpeg_write_marker(&mut dst, m.marker as i32, m.data, m.data_length);
            marker = m.next;
        }
        jpeg_finish_compress(&mut dst);
        jpeg_finish_decompress(&mut src);
        true
    }));

    let out = match result {
        Ok(true) if !buffer.is_null() => Some(unsafe { std::slice::from_raw_parts(buffer, size as usize) }.to_vec()),
        _ => None,
    };
    unsafe {
        // Safe after errors too (libjpeg's documented recovery).
        jpeg_destroy_compress(&mut dst);
        jpeg_destroy_decompress(&mut src);
        if !buffer.is_null() {
            free(buffer.cast());
        }
    }
    out
}

/// Encodes 8-bit gray (`channels` 1) or RGB (3) pixels as a JPEG of the
/// given quality (1–100), with optimized Huffman tables. Used for scanned
/// pages (see `crate::scan`).
pub(crate) fn encode(pixels: &[u8], width: u32, height: u32, channels: u8, quality: u8) -> Option<Vec<u8>> {
    let stride = width as usize * channels as usize;
    if width == 0 || height == 0 || !matches!(channels, 1 | 3) || pixels.len() < stride * height as usize {
        return None;
    }
    let mut err: Box<jpeg_error_mgr> = Box::new(unsafe { std::mem::zeroed() });
    let mut c: Box<jpeg_compress_struct> = Box::new(unsafe { std::mem::zeroed() });
    let mut buffer: *mut u8 = std::ptr::null_mut();
    let mut size: c_ulong = 0;

    let result = panic::catch_unwind(AssertUnwindSafe(|| unsafe {
        jpeg_std_error(&mut err);
        err.error_exit = Some(error_exit);
        err.emit_message = Some(emit_message);
        err.output_message = Some(output_message);
        c.common.err = &mut *err;
        jpeg_create_compress(&mut *c);
        jpeg_mem_dest(&mut c, &mut buffer, &mut size);
        c.image_width = width;
        c.image_height = height;
        c.input_components = channels as i32;
        c.in_color_space = if channels == 1 { J_COLOR_SPACE::JCS_GRAYSCALE } else { J_COLOR_SPACE::JCS_RGB };
        // The libjpeg-turbo profile: MozJPEG's default trellis search is
        // several times slower for little gain on scanned pages.
        jpeg_c_set_int_param(&mut c, J_INT_PARAM::JINT_COMPRESS_PROFILE, JCP_FASTEST as i32);
        jpeg_set_defaults(&mut c);
        jpeg_set_quality(&mut c, quality.clamp(1, 100) as i32, 1);
        c.optimize_coding = 1;
        jpeg_start_compress(&mut c, 1);
        while c.next_scanline < c.image_height {
            let row = pixels[c.next_scanline as usize * stride..].as_ptr();
            let rows = [row];
            jpeg_write_scanlines(&mut c, rows.as_ptr(), 1);
        }
        jpeg_finish_compress(&mut c);
    }));

    let out = match result {
        Ok(()) if !buffer.is_null() => Some(unsafe { std::slice::from_raw_parts(buffer, size as usize) }.to_vec()),
        _ => None,
    };
    unsafe {
        jpeg_destroy_compress(&mut c);
        if !buffer.is_null() {
            free(buffer.cast());
        }
    }
    out
}
