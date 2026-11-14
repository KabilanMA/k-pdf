//! JPEG decoding (baseline, progressive and lossless; ITU-T T.81).
//!
//! This is jpeg-decoder 0.3.2 (<https://github.com/image-rs/jpeg-decoder>,
//! copyright 2016 The jpeg-decoder Developers, MIT OR Apache-2.0), made
//! part of k-pdf and changed here: progressive JPEGs decoded at a reduced
//! scale keep only the coefficients the scaled IDCT reads (`CoefStore` in
//! `decoder.rs`), which takes about a third of the memory. The rayon,
//! WebAssembly and nightly NEON code paths were removed; x86 uses SSSE3 when
//! the processor has it.

// The parts of jpeg-decoder's interface k-pdf doesn't use yet (metadata,
// settings) are kept.
#![allow(dead_code)]



pub use decoder::{Decoder, PixelFormat};

use std::io;

mod arch;
mod decoder;
mod error;
mod huffman;
mod idct;
mod marker;
mod parser;
mod upsampler;
mod worker;

fn read_u8<R: io::Read>(reader: &mut R) -> io::Result<u8> {
    let mut buf = [0];
    reader.read_exact(&mut buf)?;
    Ok(buf[0])
}

fn read_u16_from_be<R: io::Read>(reader: &mut R) -> io::Result<u16> {
    let mut buf = [0, 0];
    reader.read_exact(&mut buf)?;
    Ok(u16::from_be_bytes(buf))
}
