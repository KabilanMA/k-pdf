//! PDF functions (ISO 32000 §7.10): used by tint transforms and shadings.

use crate::document::Document;
use crate::object::{Dictionary, Object};
use crate::parser::{Lexer, Token};

const MAX_STACK: usize = 100;

#[derive(Debug, Clone)]
pub(crate) enum Function {
    /// Type 0: a table of samples (linear interpolation for 1 input,
    /// nearest sample otherwise).
    Sampled { domain: Vec<f64>, encode: Vec<f64>, decode: Vec<f64>, size: Vec<usize>, outputs: usize, samples: Vec<f64> },
    /// Type 2: `C0 + x^N × (C1 − C0)`.
    Exponential { domain: [f64; 2], c0: Vec<f64>, c1: Vec<f64>, n: f64 },
    /// Type 3: several 1-input functions over sub-domains.
    Stitching { domain: [f64; 2], functions: Vec<Function>, bounds: Vec<f64>, encode: Vec<f64> },
    /// Type 4: a PostScript calculator program.
    PostScript { domain: Vec<f64>, range: Vec<f64>, program: Vec<PsOp> },
    /// An array of 1-output functions, one per output component.
    Array(Vec<Function>),
}

impl Function {
    pub fn parse(doc: &Document, obj: &Object) -> Option<Function> {
        Self::parse_depth(doc, obj, 0)
    }

    fn parse_depth(doc: &Document, obj: &Object, depth: usize) -> Option<Function> {
        if depth > 8 {
            return None;
        }
        let obj = doc.resolve(obj).ok()?;
        if let Object::Array(items) = &obj {
            let fs: Option<Vec<_>> = items.iter().map(|f| Self::parse_depth(doc, f, depth + 1)).collect();
            return fs.map(Function::Array);
        }
        let dict = obj.as_dict()?;
        let nums = |key: &[u8]| -> Vec<f64> {
            doc.get_resolved(dict, key)
                .ok()
                .and_then(|o| o.as_array().map(|a| a.iter().filter_map(Object::as_f64).collect()))
                .unwrap_or_default()
        };
        let domain = nums(b"Domain");
        let domain2 = [domain.first().copied().unwrap_or(0.0), domain.get(1).copied().unwrap_or(1.0)];

        match doc.get_resolved(dict, b"FunctionType").ok()?.as_i64()? {
            0 => {
                let stream = obj.as_stream()?;
                let data = doc.decode_stream(stream).ok()?;
                let size: Vec<usize> = nums(b"Size").iter().map(|&s| s.max(1.0) as usize).collect();
                let range = nums(b"Range");
                let outputs = range.len() / 2;
                let bps = int(doc, dict, b"BitsPerSample").unwrap_or(8).clamp(1, 32) as u32;
                let count: usize = size.iter().product::<usize>().checked_mul(outputs)?;
                if outputs == 0 || count > 1 << 22 {
                    return None;
                }
                let max = ((1u64 << bps) - 1) as f64;
                let samples = (0..count).map(|i| read_bits(&data, i * bps as usize, bps) as f64 / max).collect();
                let mut encode = nums(b"Encode");
                if encode.is_empty() {
                    encode = size.iter().flat_map(|&s| [0.0, (s - 1) as f64]).collect();
                }
                let mut decode = nums(b"Decode");
                if decode.is_empty() {
                    decode = range;
                }
                Some(Function::Sampled { domain, encode, decode, size, outputs, samples })
            }
            2 => {
                let mut c0 = nums(b"C0");
                let mut c1 = nums(b"C1");
                if c0.is_empty() {
                    c0 = vec![0.0];
                }
                if c1.is_empty() {
                    c1 = vec![1.0];
                }
                let n = doc.get_resolved(dict, b"N").ok()?.as_f64()?;
                Some(Function::Exponential { domain: domain2, c0, c1, n })
            }
            3 => {
                let functions = match doc.get_resolved(dict, b"Functions").ok()? {
                    Object::Array(a) => a.iter().map(|f| Self::parse_depth(doc, f, depth + 1)).collect::<Option<Vec<_>>>()?,
                    _ => return None,
                };
                Some(Function::Stitching { domain: domain2, functions, bounds: nums(b"Bounds"), encode: nums(b"Encode") })
            }
            4 => {
                let data = doc.decode_stream(obj.as_stream()?).ok()?;
                let mut lex = Lexer::new(&data, 0);
                // The program is wrapped in one outer pair of braces.
                if lex.next_token() != Some(Token::Keyword(b"{")) {
                    return None;
                }
                let program = parse_ps(&mut lex, 0)?;
                Some(Function::PostScript { domain, range: nums(b"Range"), program })
            }
            _ => None,
        }
    }

    /// Evaluates the function. Output length depends on the function.
    pub fn eval(&self, input: &[f64]) -> Vec<f64> {
        match self {
            Function::Array(fs) => fs.iter().map(|f| f.eval(input).first().copied().unwrap_or(0.0)).collect(),
            Function::Exponential { domain, c0, c1, n } => {
                let x = clamp(input.first().copied().unwrap_or(0.0), domain[0], domain[1]);
                let xn = x.powf(*n);
                c0.iter().zip(c1).map(|(a, b)| a + xn * (b - a)).collect()
            }
            Function::Stitching { domain, functions, bounds, encode } => {
                let x = clamp(input.first().copied().unwrap_or(0.0), domain[0], domain[1]);
                let i = bounds.iter().position(|&b| x < b).unwrap_or(bounds.len()).min(functions.len().saturating_sub(1));
                let lo = if i == 0 { domain[0] } else { bounds[i - 1] };
                let hi = if i < bounds.len() { bounds[i] } else { domain[1] };
                let (e0, e1) = (encode.get(2 * i).copied().unwrap_or(0.0), encode.get(2 * i + 1).copied().unwrap_or(1.0));
                let t = interpolate(x, lo, hi, e0, e1);
                functions.get(i).map(|f| f.eval(&[t])).unwrap_or_default()
            }
            Function::Sampled { domain, encode, decode, size, outputs, samples } => {
                // Index along each input dimension.
                let mut pos = Vec::with_capacity(size.len());
                for (d, &s) in size.iter().enumerate() {
                    let x = input.get(d).copied().unwrap_or(0.0);
                    let (d0, d1) = (domain.get(2 * d).copied().unwrap_or(0.0), domain.get(2 * d + 1).copied().unwrap_or(1.0));
                    let e = interpolate(clamp(x, d0, d1), d0, d1, encode[2 * d], encode[2 * d + 1]);
                    pos.push(clamp(e, 0.0, (s - 1) as f64));
                }
                let index_of = |coords: &[usize]| -> usize {
                    let mut idx = 0;
                    let mut stride = 1;
                    for (c, &s) in coords.iter().zip(size) {
                        idx += c * stride;
                        stride *= s;
                    }
                    idx * outputs
                };
                let decode_out = |j: usize, v: f64| {
                    let (lo, hi) = (decode.get(2 * j).copied().unwrap_or(0.0), decode.get(2 * j + 1).copied().unwrap_or(1.0));
                    lo + v * (hi - lo)
                };
                if size.len() == 1 {
                    let p = pos[0];
                    let (i0, frac) = (p.floor() as usize, p.fract());
                    let i1 = (i0 + 1).min(size[0] - 1);
                    (0..*outputs)
                        .map(|j| {
                            let a = samples.get(i0 * outputs + j).copied().unwrap_or(0.0);
                            let b = samples.get(i1 * outputs + j).copied().unwrap_or(0.0);
                            decode_out(j, a + (b - a) * frac)
                        })
                        .collect()
                } else {
                    let coords: Vec<usize> = pos.iter().map(|p| p.round() as usize).collect();
                    let base = index_of(&coords);
                    (0..*outputs).map(|j| decode_out(j, samples.get(base + j).copied().unwrap_or(0.0))).collect()
                }
            }
            Function::PostScript { domain, range, program } => {
                let mut stack: Vec<f64> = input
                    .iter()
                    .enumerate()
                    .map(|(i, &x)| {
                        clamp(x, domain.get(2 * i).copied().unwrap_or(f64::MIN), domain.get(2 * i + 1).copied().unwrap_or(f64::MAX))
                    })
                    .collect();
                if run_ps(program, &mut stack).is_none() {
                    stack.clear();
                }
                let n = range.len() / 2;
                let start = stack.len().saturating_sub(n);
                let mut out: Vec<f64> = stack[start..].to_vec();
                out.resize(n, 0.0);
                for (i, v) in out.iter_mut().enumerate() {
                    *v = clamp(*v, range[2 * i], range[2 * i + 1]);
                }
                out
            }
        }
    }
}

fn int(doc: &Document, dict: &Dictionary, key: &[u8]) -> Option<i64> {
    doc.get_resolved(dict, key).ok()?.as_i64()
}

fn clamp(x: f64, lo: f64, hi: f64) -> f64 {
    if x < lo { lo } else if x > hi { hi } else { x }
}

fn interpolate(x: f64, x0: f64, x1: f64, y0: f64, y1: f64) -> f64 {
    if x1 == x0 { y0 } else { y0 + (x - x0) * (y1 - y0) / (x1 - x0) }
}

/// Reads `bits` bits (MSB first) starting at bit offset `pos`.
pub(crate) fn read_bits(data: &[u8], pos: usize, bits: u32) -> u32 {
    if bits == 8 {
        return data.get(pos / 8).copied().unwrap_or(0) as u32;
    }
    let mut v: u64 = 0;
    for i in 0..bits as usize {
        let bit = pos + i;
        let byte = data.get(bit / 8).copied().unwrap_or(0);
        v = (v << 1) | ((byte >> (7 - bit % 8)) & 1) as u64;
    }
    v as u32
}

// ---------------------------------------------------------------------------
// PostScript calculator (Type 4)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub(crate) enum PsOp {
    Num(f64),
    Op(Vec<u8>),
    If(Vec<PsOp>),
    IfElse(Vec<PsOp>, Vec<PsOp>),
}

/// Parses until the matching `}`; the opening `{` was already consumed.
fn parse_ps(lex: &mut Lexer, depth: usize) -> Option<Vec<PsOp>> {
    if depth > 16 {
        return None;
    }
    let mut ops = Vec::new();
    let mut pending: Vec<Vec<PsOp>> = Vec::new(); // procedures awaiting if/ifelse
    loop {
        match lex.next_token()? {
            Token::Integer(i) => ops.push(PsOp::Num(i as f64)),
            Token::Real(r) => ops.push(PsOp::Num(r)),
            Token::Keyword(b"{") => pending.push(parse_ps(lex, depth + 1)?),
            Token::Keyword(b"}") => return Some(ops),
            Token::Keyword(b"if") => ops.push(PsOp::If(pending.pop()?)),
            Token::Keyword(b"ifelse") => {
                let else_branch = pending.pop()?;
                ops.push(PsOp::IfElse(pending.pop()?, else_branch));
            }
            Token::Keyword(b"true") => ops.push(PsOp::Num(1.0)),
            Token::Keyword(b"false") => ops.push(PsOp::Num(0.0)),
            Token::Keyword(k) => ops.push(PsOp::Op(k.to_vec())),
            _ => return None,
        }
    }
}

fn run_ps(program: &[PsOp], st: &mut Vec<f64>) -> Option<()> {
    for op in program {
        if st.len() > MAX_STACK {
            return None;
        }
        match op {
            PsOp::Num(n) => st.push(*n),
            PsOp::If(body) => {
                if st.pop()? != 0.0 {
                    run_ps(body, st)?;
                }
            }
            PsOp::IfElse(a, b) => {
                if st.pop()? != 0.0 {
                    run_ps(a, st)?
                } else {
                    run_ps(b, st)?
                }
            }
            PsOp::Op(name) => ps_operator(name, st)?,
        }
    }
    Some(())
}

fn ps_operator(name: &[u8], st: &mut Vec<f64>) -> Option<()> {
    let b = |v: bool| if v { 1.0 } else { 0.0 };
    macro_rules! unary {
        ($f:expr) => {{
            let a = st.pop()?;
            st.push($f(a));
        }};
    }
    macro_rules! binary {
        ($f:expr) => {{
            let y = st.pop()?;
            let x = st.pop()?;
            st.push($f(x, y));
        }};
    }
    match name {
        b"add" => binary!(|x, y| x + y),
        b"sub" => binary!(|x, y| x - y),
        b"mul" => binary!(|x, y| x * y),
        b"div" => binary!(|x, y| if y == 0.0 { 0.0 } else { x / y }),
        b"idiv" => binary!(|x: f64, y: f64| if y == 0.0 { 0.0 } else { (x / y).trunc() }),
        b"mod" => binary!(|x: f64, y: f64| if y == 0.0 { 0.0 } else { x % y }),
        b"exp" => binary!(f64::powf),
        b"atan" => binary!(|x: f64, y: f64| {
            let d = x.atan2(y).to_degrees();
            if d < 0.0 { d + 360.0 } else { d }
        }),
        b"neg" => unary!(|x: f64| -x),
        b"abs" => unary!(f64::abs),
        b"ceiling" => unary!(f64::ceil),
        b"floor" => unary!(f64::floor),
        b"round" => unary!(|x: f64| (x + 0.5).floor()),
        b"truncate" | b"cvi" => unary!(f64::trunc),
        b"cvr" => {}
        b"sqrt" => unary!(|x: f64| x.max(0.0).sqrt()),
        b"sin" => unary!(|x: f64| x.to_radians().sin()),
        b"cos" => unary!(|x: f64| x.to_radians().cos()),
        b"ln" => unary!(f64::ln),
        b"log" => unary!(f64::log10),
        b"eq" => binary!(|x, y| b(x == y)),
        b"ne" => binary!(|x, y| b(x != y)),
        b"gt" => binary!(|x, y| b(x > y)),
        b"ge" => binary!(|x, y| b(x >= y)),
        b"lt" => binary!(|x, y| b(x < y)),
        b"le" => binary!(|x, y| b(x <= y)),
        b"and" => binary!(|x: f64, y: f64| ((x as i64) & (y as i64)) as f64),
        b"or" => binary!(|x: f64, y: f64| ((x as i64) | (y as i64)) as f64),
        b"xor" => binary!(|x: f64, y: f64| ((x as i64) ^ (y as i64)) as f64),
        b"not" => unary!(|x: f64| if x == 0.0 { 1.0 } else if x == 1.0 { 0.0 } else { !(x as i64) as f64 }),
        b"bitshift" => binary!(|x: f64, y: f64| {
            let (x, s) = (x as i64, y as i64);
            (if s >= 0 { x.wrapping_shl(s as u32) } else { x.wrapping_shr((-s) as u32) }) as f64
        }),
        b"pop" => {
            st.pop()?;
        }
        b"dup" => st.push(*st.last()?),
        b"exch" => {
            let n = st.len();
            if n < 2 {
                return None;
            }
            st.swap(n - 1, n - 2);
        }
        b"copy" => {
            let n = st.pop()? as usize;
            let len = st.len();
            if n > len {
                return None;
            }
            st.extend_from_within(len - n..);
        }
        b"index" => {
            let n = st.pop()? as usize;
            let v = *st.get(st.len().checked_sub(n + 1)?)?;
            st.push(v);
        }
        b"roll" => {
            let j = st.pop()? as i64;
            let n = st.pop()? as usize;
            let len = st.len();
            if n > len || n == 0 {
                return Some(());
            }
            let slice = &mut st[len - n..];
            let j = j.rem_euclid(n as i64) as usize;
            slice.rotate_right(j);
        }
        _ => return None,
    }
    Some(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ps(src: &str, input: &[f64], outputs: usize) -> Vec<f64> {
        let mut lex = Lexer::new(src.as_bytes(), 0);
        assert_eq!(lex.next_token(), Some(Token::Keyword(b"{")));
        let program = parse_ps(&mut lex, 0).unwrap();
        let f = Function::PostScript { domain: vec![0.0, 1.0], range: [0.0, 1.0].repeat(outputs), program };
        f.eval(input)
    }

    #[test]
    fn postscript_tint_transform() {
        // Typical spot-color tint: tint → CMYK.
        assert_eq!(ps("{ dup 0.5 mul exch 0 exch 0 }", &[1.0], 4), vec![0.5, 0.0, 1.0, 0.0]);
        assert_eq!(ps("{ 0.5 gt { 1 } { 0 } ifelse }", &[0.7], 1), vec![1.0]);
        assert_eq!(ps("{ 1 2 3 3 1 roll }", &[0.0], 3), vec![1.0, 1.0, 1.0]); // clamped to range
    }

    #[test]
    fn exponential_and_stitching() {
        let e = Function::Exponential { domain: [0.0, 1.0], c0: vec![0.0], c1: vec![1.0], n: 2.0 };
        assert_eq!(e.eval(&[0.5]), vec![0.25]);
        let s = Function::Stitching {
            domain: [0.0, 1.0],
            functions: vec![
                Function::Exponential { domain: [0.0, 1.0], c0: vec![0.0], c1: vec![1.0], n: 1.0 },
                Function::Exponential { domain: [0.0, 1.0], c0: vec![1.0], c1: vec![0.0], n: 1.0 },
            ],
            bounds: vec![0.5],
            encode: vec![0.0, 1.0, 0.0, 1.0],
        };
        assert_eq!(s.eval(&[0.25]), vec![0.5]);
        assert_eq!(s.eval(&[0.75]), vec![0.5]);
    }
}
