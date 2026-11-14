//! Embedded PostScript Type 1 fonts (`/FontFile`), per the Adobe Type 1
//! Font Format specification: eexec decryption, charstring decryption and
//! the Type 1 charstring interpreter.

use crate::hash::HashMap;

use tiny_skia::{Path, PathBuilder};

use crate::encoding::{char_to_glyph_name, BaseEncoding};
use crate::parser::{find, Lexer, Token};

pub(crate) struct Type1Font {
    charstrings: HashMap<Vec<u8>, Vec<u8>>,
    subrs: Vec<Vec<u8>>,
    /// The font's built-in encoding: glyph name per code.
    pub encoding: Vec<Option<String>>,
    /// Glyph space → text space, usually [0.001 0 0 0.001 0 0].
    pub matrix: [f64; 6],
}

impl Type1Font {
    pub fn parse(data: &[u8]) -> Option<Type1Font> {
        let data = strip_pfb(data);
        let eexec = find(&data, b"eexec")?;
        let clear = &data[..eexec];
        let (encoding, matrix) = parse_cleartext(clear);

        // Encrypted portion: binary, or hex in the PFA format.
        let mut start = eexec + b"eexec".len();
        while data.get(start).is_some_and(|b| b.is_ascii_whitespace()) {
            start += 1;
        }
        let cipher = &data[start..];
        let is_hex = cipher.len() >= 4 && cipher[..4].iter().all(u8::is_ascii_hexdigit);
        let bytes = if is_hex { hex_decode(cipher) } else { cipher.to_vec() };
        let private = decrypt(&bytes, 55665, 4);

        let len_iv = find(&private, b"/lenIV")
            .and_then(|p| match Lexer::new(&private, p + 6).next_token() {
                Some(Token::Integer(n)) => Some(n),
                _ => None,
            })
            .unwrap_or(4);
        let (subrs, charstrings) = parse_private(&private, len_iv);
        if charstrings.is_empty() {
            return None;
        }
        Some(Type1Font { charstrings, subrs, encoding, matrix })
    }

    /// Glyph outline in glyph space, or `None` if the glyph is missing.
    pub fn outline(&self, name: &str) -> Option<Path> {
        let mut pb = PathBuilder::new();
        let mut interp = Interpreter::new(self, &mut pb);
        interp.run_glyph(name.as_bytes(), 0.0, 0.0)?;
        pb.finish()
    }

    /// Advance width (from hsbw/sbw) in glyph space.
    pub fn advance(&self, name: &str) -> Option<f64> {
        let mut pb = PathBuilder::new();
        let mut interp = Interpreter::new(self, &mut pb);
        interp.run_glyph(name.as_bytes(), 0.0, 0.0)?;
        interp.width
    }
}

/// PFB files wrap segments in 6-byte headers (0x80, type, u32 length).
fn strip_pfb(data: &[u8]) -> Vec<u8> {
    if data.first() != Some(&0x80) {
        return data.to_vec();
    }
    let mut out = Vec::with_capacity(data.len());
    let mut i = 0;
    while i + 6 <= data.len() && data[i] == 0x80 && data[i + 1] != 3 {
        let len = u32::from_le_bytes([data[i + 2], data[i + 3], data[i + 4], data[i + 5]]) as usize;
        let end = (i + 6 + len).min(data.len());
        out.extend_from_slice(&data[i + 6..end]);
        i = end;
    }
    out
}

fn hex_decode(data: &[u8]) -> Vec<u8> {
    let digits: Vec<u8> = data.iter().filter_map(|&c| crate::parser::hex_value(c)).collect();
    digits.chunks_exact(2).map(|p| p[0] << 4 | p[1]).collect()
}

/// Type 1 decryption; drops the first `skip` plaintext bytes.
fn decrypt(data: &[u8], key: u16, skip: usize) -> Vec<u8> {
    let mut r = key;
    let out: Vec<u8> = data
        .iter()
        .map(|&c| {
            let p = c ^ (r >> 8) as u8;
            r = (c as u16).wrapping_add(r).wrapping_mul(52845).wrapping_add(22719);
            p
        })
        .collect();
    out.get(skip..).map(<[u8]>::to_vec).unwrap_or_default()
}

/// Reads /Encoding and /FontMatrix from the clear-text part.
fn parse_cleartext(clear: &[u8]) -> (Vec<Option<String>>, [f64; 6]) {
    let mut encoding: Vec<Option<String>> = vec![None; 256];
    let mut matrix = [0.001, 0.0, 0.0, 0.001, 0.0, 0.0];
    let mut lex = Lexer::new(clear, 0);
    while let Some(tok) = lex.next_token() {
        match tok {
            Token::Name(n) if n == b"FontMatrix" => {
                // `[a b c d e f]` or `{a b c d e f}`
                let mut vals = Vec::new();
                while let Some(t) = lex.next_token() {
                    match t {
                        Token::Integer(i) => vals.push(i as f64),
                        Token::Real(r) => vals.push(r),
                        Token::ArrayStart | Token::Keyword(b"{") => {}
                        _ => break,
                    }
                }
                if vals.len() == 6 && vals[0] != 0.0 {
                    matrix.copy_from_slice(&vals);
                }
            }
            Token::Name(n) if n == b"Encoding" => match lex.next_token() {
                Some(Token::Keyword(b"StandardEncoding")) => {
                    for (code, slot) in encoding.iter_mut().enumerate() {
                        *slot = BaseEncoding::Standard.decode(code as u8).and_then(char_to_glyph_name);
                    }
                }
                _ => {
                    // `dup <code> /<name> put` … until `def`/`readonly`.
                    let mut prev: [Option<Token>; 2] = [None, None];
                    while let Some(t) = lex.next_token() {
                        match &t {
                            Token::Keyword(b"def" | b"readonly") => break,
                            Token::Keyword(b"put") => {
                                if let (Some(Token::Integer(code)), Some(Token::Name(name))) = (&prev[0], &prev[1])
                                    && (0..256).contains(code)
                                {
                                    encoding[*code as usize] = Some(String::from_utf8_lossy(name).into_owned());
                                }
                            }
                            _ => {}
                        }
                        prev = [prev[1].take(), Some(t)];
                    }
                }
            },
            _ => {}
        }
    }
    (encoding, matrix)
}

/// Extracts /Subrs and /CharStrings from the decrypted private part.
/// Binary entries look like `dup 5 23 RD <23 bytes> NP` (Subrs) and
/// `/A 120 RD <120 bytes> ND` (CharStrings); `RD` may be spelled `-|`.
type PrivateParts = (Vec<Vec<u8>>, HashMap<Vec<u8>, Vec<u8>>);

fn parse_private(private: &[u8], len_iv: i64) -> PrivateParts {
    let mut subrs: Vec<Vec<u8>> = Vec::new();
    let mut charstrings = HashMap::default();
    let mut lex = Lexer::new(private, 0);
    // The last few tokens; only the trailing 3 matter.
    let mut hist: Vec<Token> = Vec::new();
    let decrypt_cs = |bin: &[u8]| -> Vec<u8> {
        if len_iv < 0 { bin.to_vec() } else { decrypt(bin, 4330, len_iv as usize) }
    };
    while let Some(tok) = lex.next_token() {
        // `RD` or `-|`. The PDF lexer reads `-|` as the number `-` (0)
        // followed by `|`, so recognise that spelling from the raw bytes.
        let is_rd = match tok {
            Token::Keyword(b"RD") => true,
            Token::Keyword(b"|") if lex.pos() >= 2 && private[lex.pos() - 2] == b'-' => {
                hist.pop(); // the bogus 0 produced by '-'
                true
            }
            _ => false,
        };
        if is_rd && let Some(Token::Integer(len)) = hist.last().cloned() {
            let start = lex.pos() + 1; // exactly one space before the binary data
            let end = start.saturating_add(len.max(0) as usize).min(private.len());
            let bin = decrypt_cs(&private[start.min(end)..end]);
            match hist.len().checked_sub(2).map(|i| &hist[i]) {
                Some(Token::Name(name)) => {
                    charstrings.insert(name.clone(), bin);
                }
                Some(Token::Integer(idx)) if (0..65536).contains(idx) => {
                    let idx = *idx as usize;
                    if subrs.len() <= idx {
                        subrs.resize(idx + 1, Vec::new());
                    }
                    subrs[idx] = bin;
                }
                _ => {}
            }
            lex.set_pos(end);
            hist.clear();
            continue;
        }
        if hist.len() == 3 {
            hist.remove(0);
        }
        hist.push(tok);
    }
    (subrs, charstrings)
}

/// What the interpreter does after an operator.
enum Flow {
    Continue,
    /// `return` from a subroutine.
    Return,
    /// `endchar` (or the end of a `seac` composite).
    End,
}

/// Type 1 charstring interpreter producing a path.
struct Interpreter<'f, 'p> {
    font: &'f Type1Font,
    pb: &'p mut PathBuilder,
    stack: Vec<f64>,
    ps_stack: Vec<f64>,
    x: f64,
    y: f64,
    /// Offset applied to all points (for seac accents).
    dx: f64,
    dy: f64,
    flex: Option<Vec<(f64, f64)>>,
    open: bool,
    width: Option<f64>,
}

/// Nesting limit for subroutine calls and seac components.
const MAX_DEPTH: usize = 10;

impl<'f, 'p> Interpreter<'f, 'p> {
    fn new(font: &'f Type1Font, pb: &'p mut PathBuilder) -> Self {
        Interpreter {
            font,
            pb,
            stack: Vec::new(),
            ps_stack: Vec::new(),
            x: 0.0,
            y: 0.0,
            dx: 0.0,
            dy: 0.0,
            flex: None,
            open: false,
            width: None,
        }
    }

    fn run_glyph(&mut self, name: &[u8], dx: f64, dy: f64) -> Option<()> {
        self.run_glyph_depth(name, dx, dy, 0)
    }

    fn run_glyph_depth(&mut self, name: &[u8], dx: f64, dy: f64, depth: usize) -> Option<()> {
        let font = self.font;
        let cs = font.charstrings.get(name)?;
        self.dx = dx;
        self.dy = dy;
        self.stack.clear();
        self.run(cs, depth)?;
        Some(())
    }

    fn move_to(&mut self) {
        if let Some(points) = &mut self.flex {
            points.push((self.x, self.y));
            return;
        }
        if self.open {
            self.pb.close();
        }
        self.pb.move_to((self.x + self.dx) as f32, (self.y + self.dy) as f32);
        self.open = true;
    }

    fn line_to(&mut self) {
        self.pb.line_to((self.x + self.dx) as f32, (self.y + self.dy) as f32);
    }

    fn cubic(&mut self, p1: (f64, f64), p2: (f64, f64), p3: (f64, f64)) {
        let (ox, oy) = (self.dx, self.dy);
        self.pb.cubic_to(
            (p1.0 + ox) as f32,
            (p1.1 + oy) as f32,
            (p2.0 + ox) as f32,
            (p2.1 + oy) as f32,
            (p3.0 + ox) as f32,
            (p3.1 + oy) as f32,
        );
    }

    /// Relative curve from the current point.
    fn rel_curve(&mut self, d: [f64; 6]) {
        let p1 = (self.x + d[0], self.y + d[1]);
        let p2 = (p1.0 + d[2], p1.1 + d[3]);
        let p3 = (p2.0 + d[4], p2.1 + d[5]);
        self.cubic(p1, p2, p3);
        (self.x, self.y) = p3;
    }

    fn close(&mut self) {
        if self.open {
            self.pb.close();
            self.open = false;
        }
    }

    /// Runs a charstring. Returns `Some(true)` once `endchar` is reached,
    /// `Some(false)` on `return` or end of data, `None` on error.
    fn run(&mut self, cs: &[u8], depth: usize) -> Option<bool> {
        if depth > MAX_DEPTH {
            return None;
        }
        let mut i = 0;
        while i < cs.len() {
            let v = cs[i];
            i += 1;
            let flow = match v {
                32..=246 => {
                    self.stack.push(v as f64 - 139.0);
                    Flow::Continue
                }
                247..=250 => {
                    let w = *cs.get(i)? as f64;
                    i += 1;
                    self.stack.push((v as f64 - 247.0) * 256.0 + w + 108.0);
                    Flow::Continue
                }
                251..=254 => {
                    let w = *cs.get(i)? as f64;
                    i += 1;
                    self.stack.push(-(v as f64 - 251.0) * 256.0 - w - 108.0);
                    Flow::Continue
                }
                255 => {
                    let b = cs.get(i..i + 4)?;
                    i += 4;
                    self.stack.push(i32::from_be_bytes([b[0], b[1], b[2], b[3]]) as f64);
                    Flow::Continue
                }
                12 => {
                    let op = *cs.get(i)?;
                    i += 1;
                    self.escape(op, depth)?
                }
                _ => self.command(v, depth)?,
            };
            match flow {
                Flow::Continue => {}
                Flow::Return => return Some(false),
                Flow::End => return Some(true),
            }
            if self.stack.len() > 64 {
                return None;
            }
        }
        Some(false)
    }

    fn arg(&self, i: usize) -> f64 {
        self.stack.get(i).copied().unwrap_or(0.0)
    }

    fn command(&mut self, op: u8, depth: usize) -> Option<Flow> {
        match op {
            1 | 3 => {} // hstem, vstem: hints are ignored
            4 => {
                self.y += self.arg(0);
                self.move_to();
            }
            5 => {
                self.x += self.arg(0);
                self.y += self.arg(1);
                self.line_to();
            }
            6 => {
                self.x += self.arg(0);
                self.line_to();
            }
            7 => {
                self.y += self.arg(0);
                self.line_to();
            }
            8 => self.rel_curve([self.arg(0), self.arg(1), self.arg(2), self.arg(3), self.arg(4), self.arg(5)]),
            9 => self.close(),
            10 => {
                // callsubr: the remaining operands are the subroutine's input.
                let n = self.stack.pop()? as usize;
                let font = self.font;
                let subr = font.subrs.get(n)?;
                let ended = self.run(subr, depth + 1)?;
                return Some(if ended { Flow::End } else { Flow::Continue });
            }
            11 => return Some(Flow::Return),
            13 => {
                // hsbw: sbx wx
                self.x = self.arg(0);
                self.y = 0.0;
                self.width = Some(self.arg(1));
            }
            14 => {
                self.close();
                return Some(Flow::End);
            }
            21 => {
                self.x += self.arg(0);
                self.y += self.arg(1);
                self.move_to();
            }
            22 => {
                self.x += self.arg(0);
                self.move_to();
            }
            30 => self.rel_curve([0.0, self.arg(0), self.arg(1), self.arg(2), self.arg(3), 0.0]),
            31 => self.rel_curve([self.arg(0), 0.0, self.arg(1), self.arg(2), 0.0, self.arg(3)]),
            _ => {}
        }
        self.stack.clear();
        Some(Flow::Continue)
    }

    fn escape(&mut self, op: u8, depth: usize) -> Option<Flow> {
        match op {
            0..=2 => {} // dotsection, vstem3, hstem3
            6 => {
                // seac: asb adx ady bchar achar — base glyph plus an accent.
                let (asb, adx, ady) = (self.arg(0), self.arg(1), self.arg(2));
                let std_name = |c: f64| BaseEncoding::Standard.decode(c as u8).and_then(char_to_glyph_name);
                let (base, accent) = (std_name(self.arg(3))?, std_name(self.arg(4))?);
                let (ox, oy, width) = (self.dx, self.dy, self.width);
                self.close();
                self.run_glyph_depth(base.as_bytes(), ox, oy, depth + 1)?;
                self.close();
                // The accent's hsbw puts it at x = asb; shift it to adx.
                self.run_glyph_depth(accent.as_bytes(), ox + adx - asb, oy + ady, depth + 1)?;
                self.close();
                self.width = width;
                return Some(Flow::End);
            }
            7 => {
                // sbw: sbx sby wx wy
                self.x = self.arg(0);
                self.y = self.arg(1);
                self.width = Some(self.arg(2));
            }
            12 => {
                let b = self.stack.pop()?;
                let a = self.stack.pop()?;
                self.stack.push(if b == 0.0 { 0.0 } else { a / b });
                return Some(Flow::Continue);
            }
            16 => {
                // callothersubr: arg1 … argn n othersubr#
                let othersubr = self.stack.pop()? as i64;
                let n = (self.stack.pop()?.max(0.0) as usize).min(self.stack.len());
                let args = self.stack.split_off(self.stack.len() - n);
                match othersubr {
                    0 => {
                        // End of flex: a reference point + 6 points = two curves.
                        if let Some(p) = self.flex.take()
                            && p.len() >= 7
                        {
                            self.cubic(p[1], p[2], p[3]);
                            self.cubic(p[4], p[5], p[6]);
                            (self.x, self.y) = p[6];
                        }
                        // `pop pop setcurrentpoint` follows: pops yield x, then y.
                        self.ps_stack = vec![self.y, self.x];
                    }
                    1 => {
                        self.flex = Some(Vec::new());
                        self.ps_stack.clear();
                    }
                    2 => self.ps_stack.clear(),
                    // 3 = hint replacement: `pop` returns the subr to call.
                    // Unknown othersubrs: hand the arguments back unchanged.
                    _ => self.ps_stack = args.into_iter().rev().collect(),
                }
                return Some(Flow::Continue);
            }
            17 => {
                // pop: move a value from the PostScript stack.
                let v = self.ps_stack.pop().unwrap_or(0.0);
                self.stack.push(v);
                return Some(Flow::Continue);
            }
            33 => {
                // setcurrentpoint
                self.x = self.arg(0);
                self.y = self.arg(1);
            }
            _ => {}
        }
        self.stack.clear();
        Some(Flow::Continue)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn encrypt(plain: &[u8], key: u16) -> Vec<u8> {
        let mut r = key;
        plain
            .iter()
            .map(|&p| {
                let c = p ^ (r >> 8) as u8;
                r = (c as u16).wrapping_add(r).wrapping_mul(52845).wrapping_add(22719);
                c
            })
            .collect()
    }

    /// Encodes a charstring number (Type 1 format).
    fn num(v: i32) -> Vec<u8> {
        match v {
            -107..=107 => vec![(v + 139) as u8],
            108..=1131 => {
                let v = v - 108;
                vec![(v / 256 + 247) as u8, (v % 256) as u8]
            }
            _ => {
                let mut b = vec![255];
                b.extend_from_slice(&v.to_be_bytes());
                b
            }
        }
    }

    #[test]
    fn parses_and_outlines_a_square() {
        // Glyph "square": hsbw 50 600; rmoveto 0 0; 500 0 rlineto;
        // 0 500 rlineto; -500 0 rlineto; closepath; endchar
        let mut cs = Vec::new();
        for (args, op) in [
            (vec![50, 600], 13u8),
            (vec![0, 0], 21),
            (vec![500, 0], 5),
            (vec![0, 500], 5),
            (vec![-500, 0], 5),
            (vec![], 9),
            (vec![], 14),
        ] {
            for a in args {
                cs.extend(num(a));
            }
            cs.push(op);
        }
        let mut cs_plain = vec![0u8; 4];
        cs_plain.extend(&cs);
        let cs_enc = encrypt(&cs_plain, 4330);

        let mut private = b"    /lenIV 4 def /CharStrings 1 dict dup begin\n/square ".to_vec();
        private.extend_from_slice(format!("{} -| ", cs_enc.len()).as_bytes());
        private.extend_from_slice(&cs_enc);
        private.extend_from_slice(b" ND\nend\n");
        let mut font = b"%!PS-AdobeFont-1.0\n/FontMatrix [0.001 0 0 0.001 0 0] readonly def\n\
/Encoding 256 array 0 1 255 {1 index exch /.notdef put} for dup 65 /square put readonly def\n\
currentfile eexec\n"
            .to_vec();
        font.extend(encrypt(&private, 55665));

        let t1 = Type1Font::parse(&font).expect("font parses");
        assert_eq!(t1.encoding[65].as_deref(), Some("square"));
        assert_eq!(t1.advance("square"), Some(600.0));
        let path = t1.outline("square").unwrap();
        let b = path.bounds();
        assert_eq!((b.left(), b.top(), b.right(), b.bottom()), (50.0, 0.0, 550.0, 500.0));
    }
}
