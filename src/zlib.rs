//! Zlib compression split across threads, as pigz does: the data is cut
//! into pieces that are deflated at the same time and joined into one
//! valid zlib stream. Each piece but the last ends with a sync flush (an
//! empty stored block), so the pieces are byte-aligned and can simply be
//! concatenated; their Adler-32 checksums are combined. The result is a
//! few hundredths of a percent larger than compressing in one go (each
//! piece starts without the previous one as history).

use flate2::{Compress, Compression, FlushCompress};

/// Pieces smaller than this aren't worth a thread.
const MIN_PIECE: usize = 256 * 1024;
/// More threads than this add memory (a deflate state each) for little
/// extra speed.
const MAX_THREADS: usize = 8;

/// How many pieces to cut `len` bytes into.
pub fn pieces_for(len: usize) -> usize {
    let threads = std::thread::available_parallelism().map_or(1, |n| n.get());
    (len / MIN_PIECE).clamp(1, threads.clamp(1, MAX_THREADS))
}

/// Zlib-compresses `data` at `level` (0–9), using several cores.
pub fn compress(data: &[u8], level: u32) -> Vec<u8> {
    let pieces = pieces_for(data.len());
    let size = data.len().div_ceil(pieces).max(1);
    compress_pieces(pieces, level, |i, feed| {
        let start = (i * size).min(data.len());
        feed(&data[start..(start + size).min(data.len())]);
    })
}

/// Zlib-compresses data produced in `pieces` pieces: `produce(i, feed)`
/// passes piece `i`'s bytes to `feed`, in as many calls as it likes (so a
/// piece never has to be in memory at once). Pieces are produced and
/// deflated on separate threads.
pub fn compress_pieces<F>(pieces: usize, level: u32, produce: F) -> Vec<u8>
where
    F: Fn(usize, &mut dyn FnMut(&[u8])) + Sync,
{
    let pieces = pieces.max(1);
    let run = |i: usize| deflate_piece(level, i + 1 == pieces, |feed| produce(i, feed));
    let parts: Vec<(Vec<u8>, u32, u64)> = if pieces == 1 {
        vec![run(0)]
    } else {
        std::thread::scope(|s| {
            let handles: Vec<_> = (0..pieces).map(|i| s.spawn(move || run(i))).collect();
            handles.into_iter().map(|h| h.join().expect("deflate thread panicked")).collect()
        })
    };
    let mut out = Vec::with_capacity(parts.iter().map(|p| p.0.len()).sum::<usize>() + 6);
    // Header: deflate with a 32 KB window; FLEVEL from the level; check bits.
    let flevel: u16 = match level {
        0..=1 => 0,
        2..=5 => 1,
        6 => 2,
        _ => 3,
    };
    let mut header: u16 = 0x7800 | (flevel << 6);
    header += 31 - header % 31;
    out.extend_from_slice(&header.to_be_bytes());
    let mut adler = 1;
    for (bytes, piece_adler, len) in &parts {
        out.extend_from_slice(bytes);
        adler = adler32_combine(adler, *piece_adler, *len);
    }
    out.extend_from_slice(&adler.to_be_bytes());
    out
}

/// Raw deflate of one piece (sync-flushed, or finished if it is the last),
/// with its Adler-32 and length.
fn deflate_piece(level: u32, last: bool, produce: impl FnOnce(&mut dyn FnMut(&[u8]))) -> (Vec<u8>, u32, u64) {
    let mut c = Compress::new(Compression::new(level), false);
    let mut out: Vec<u8> = Vec::with_capacity(64 * 1024);
    let mut adler = Adler::default();
    let mut len = 0u64;
    let push = |c: &mut Compress, out: &mut Vec<u8>, mut input: &[u8], flush: FlushCompress| loop {
        if out.capacity() - out.len() < 4096 {
            out.reserve(out.capacity().max(64 * 1024));
        }
        let before = c.total_in();
        let status = c.compress_vec(input, out, flush).expect("deflate cannot fail");
        input = &input[(c.total_in() - before) as usize..];
        let flushed = out.capacity() > out.len(); // room left: nothing pending
        match (status, flush) {
            (flate2::Status::StreamEnd, _) => break,
            (_, FlushCompress::None) if input.is_empty() => break,
            (_, FlushCompress::Sync) if input.is_empty() && flushed => break,
            _ => {}
        }
    };
    produce(&mut |bytes: &[u8]| {
        adler.update(bytes);
        len += bytes.len() as u64;
        push(&mut c, &mut out, bytes, FlushCompress::None);
    });
    push(&mut c, &mut out, &[], if last { FlushCompress::Finish } else { FlushCompress::Sync });
    (out, adler.value(), len)
}

const MOD: u32 = 65521;

struct Adler {
    a: u32,
    b: u32,
}

impl Default for Adler {
    fn default() -> Self {
        Adler { a: 1, b: 0 }
    }
}

impl Adler {
    fn update(&mut self, data: &[u8]) {
        // 5552 bytes is the most that can be summed before b overflows.
        for chunk in data.chunks(5552) {
            for &x in chunk {
                self.a += x as u32;
                self.b += self.a;
            }
            self.a %= MOD;
            self.b %= MOD;
        }
    }

    fn value(&self) -> u32 {
        (self.b << 16) | self.a
    }
}

/// The Adler-32 of two pieces joined, from theirs (as zlib's
/// `adler32_combine`).
fn adler32_combine(adler1: u32, adler2: u32, len2: u64) -> u32 {
    let rem = (len2 % MOD as u64) as u32;
    let mut sum1 = adler1 & 0xffff;
    let mut sum2 = ((rem as u64 * sum1 as u64) % MOD as u64) as u32;
    sum1 += (adler2 & 0xffff) + MOD - 1;
    sum2 += ((adler1 >> 16) & 0xffff) + ((adler2 >> 16) & 0xffff) + MOD - rem;
    if sum1 >= MOD {
        sum1 -= MOD;
    }
    if sum1 >= MOD {
        sum1 -= MOD;
    }
    if sum2 >= MOD << 1 {
        sum2 -= MOD << 1;
    }
    if sum2 >= MOD {
        sum2 -= MOD;
    }
    sum1 | (sum2 << 16)
}

#[cfg(test)]
mod tests {
    use std::io::Read;

    fn inflate(z: &[u8]) -> Vec<u8> {
        let mut back = Vec::new();
        flate2::read::ZlibDecoder::new(z).read_to_end(&mut back).unwrap();
        back
    }

    #[test]
    fn round_trips() {
        let data: Vec<u8> = (0..3_000_000u32).map(|i| ((i % 251) ^ (i / 7919)) as u8).collect();
        for level in [1, 6] {
            assert_eq!(inflate(&super::compress(&data, level)), data);
        }
        // Several pieces fed in small slices.
        let z = super::compress_pieces(5, 2, |i, feed| {
            for chunk in data[i * 600_000..(i + 1) * 600_000].chunks(1000) {
                feed(chunk);
            }
        });
        assert_eq!(inflate(&z), data);
        assert!(inflate(&super::compress(b"", 6)).is_empty());
    }
}
