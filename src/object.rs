//! The PDF object model (ISO 32000 §7.3).

use std::fmt;
use std::ops::{Deref, Range};
use std::sync::Arc;

/// An open file's bytes.
pub(crate) trait FileData: Send + Sync + std::panic::RefUnwindSafe {
    fn bytes(&self) -> &[u8];

    /// Copies the bytes at `offset` into `buf` (as many as there are),
    /// without bringing more of the file into memory than that if it can.
    fn peek(&self, offset: usize, buf: &mut [u8]) -> usize;
}

/// The bytes of an open file, shared by the streams read from it.
pub(crate) type SharedData = Arc<dyn FileData>;

/// A stream's raw bytes: either its own, or a range of the file it was read
/// from. Streams read from a file point into it, so reading an object to
/// look at its dictionary (an image's `/Subtype`, say) doesn't copy (or, for
/// a memory-mapped file, even read) its data, and cloning a stream is cheap.
#[derive(Clone, Default)]
pub struct Bytes(Repr);

#[derive(Clone, Default)]
enum Repr {
    #[default]
    Empty,
    /// Shared, so streams copied around (edits, the overlay of changes)
    /// don't copy their data.
    Owned(Arc<Vec<u8>>),
    /// Boxed so `Bytes`, and with it every `Object`, stays small (16
    /// bytes, not 40): a large document has hundreds of thousands of
    /// objects in memory.
    Shared(Box<(SharedData, Range<usize>)>),
}

impl Bytes {
    pub(crate) fn shared(data: SharedData, range: Range<usize>) -> Bytes {
        Bytes(Repr::Shared(Box::new((data, range))))
    }

    /// The bytes as a vector, copying them only if they are shared.
    pub fn into_vec(self) -> Vec<u8> {
        match self.0 {
            Repr::Empty => Vec::new(),
            Repr::Owned(v) => Arc::try_unwrap(v).unwrap_or_else(|v| (*v).clone()),
            Repr::Shared(shared) => shared.0.bytes()[shared.1.clone()].to_vec(),
        }
    }
}

impl Deref for Bytes {
    type Target = [u8];
    fn deref(&self) -> &[u8] {
        match &self.0 {
            Repr::Empty => &[],
            Repr::Owned(v) => v,
            Repr::Shared(shared) => &shared.0.bytes()[shared.1.clone()],
        }
    }
}

impl From<Vec<u8>> for Bytes {
    fn from(v: Vec<u8>) -> Bytes {
        Bytes(Repr::Owned(Arc::new(v)))
    }
}

impl PartialEq for Bytes {
    fn eq(&self, other: &Bytes) -> bool {
        **self == **other
    }
}

impl PartialEq<[u8]> for Bytes {
    fn eq(&self, other: &[u8]) -> bool {
        **self == *other
    }
}

impl<const N: usize> PartialEq<&[u8; N]> for Bytes {
    fn eq(&self, other: &&[u8; N]) -> bool {
        **self == other[..]
    }
}

impl fmt::Debug for Bytes {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "<{} bytes>", self.len())
    }
}

/// An indirect object reference: `num gen R`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ObjRef {
    pub num: u32,
    pub generation: u16,
}

impl ObjRef {
    pub fn new(num: u32, generation: u16) -> Self {
        ObjRef { num, generation }
    }
}

impl fmt::Display for ObjRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} {} R", self.num, self.generation)
    }
}

/// A PDF dictionary.
///
/// Stored as a small vector of pairs rather than a hash map: real-world
/// dictionaries rarely have more than ~20 keys, so a linear scan is fast and
/// this uses far less memory per dictionary.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Dictionary {
    entries: Vec<(Vec<u8>, Object)>,
}

impl Dictionary {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn get(&self, key: &[u8]) -> Option<&Object> {
        self.entries.iter().find(|(k, _)| k == key).map(|(_, v)| v)
    }

    /// Insert or replace a key. Later duplicates override earlier ones,
    /// matching the behaviour of most PDF readers.
    pub fn set(&mut self, key: Vec<u8>, value: Object) {
        match self.entries.iter_mut().find(|(k, _)| *k == key) {
            Some(slot) => slot.1 = value,
            None => self.entries.push((key, value)),
        }
    }

    pub fn remove(&mut self, key: &[u8]) -> Option<Object> {
        let i = self.entries.iter().position(|(k, _)| k == key)?;
        Some(self.entries.remove(i).1)
    }

    pub fn contains(&self, key: &[u8]) -> bool {
        self.get(key).is_some()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn iter(&self) -> impl Iterator<Item = (&[u8], &Object)> {
        self.entries.iter().map(|(k, v)| (k.as_slice(), v))
    }

    pub fn iter_mut(&mut self) -> impl Iterator<Item = (&[u8], &mut Object)> {
        self.entries.iter_mut().map(|(k, v)| (k.as_slice(), v))
    }

    /// The value of `/Type`, if it is a name.
    pub fn type_name(&self) -> Option<&[u8]> {
        self.get(b"Type").and_then(Object::as_name)
    }
}

/// A stream: a dictionary followed by raw (still encoded) bytes.
#[derive(Debug, Clone, PartialEq)]
pub struct Stream {
    pub dict: Dictionary,
    /// Raw bytes exactly as stored in the file (filters not yet applied).
    pub raw: Bytes,
}

/// Any PDF object.
#[derive(Debug, Clone, PartialEq, Default)]
pub enum Object {
    #[default]
    Null,
    Bool(bool),
    Integer(i64),
    Real(f64),
    /// Literal `( )` or hex `< >` string; bytes are stored undecoded.
    String(Vec<u8>),
    /// A name, without the leading `/` and with `#xx` escapes resolved.
    Name(Vec<u8>),
    Array(Vec<Object>),
    Dictionary(Dictionary),
    Stream(Stream),
    Reference(ObjRef),
}

impl Object {
    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Object::Bool(b) => Some(*b),
            _ => None,
        }
    }

    pub fn as_i64(&self) -> Option<i64> {
        match self {
            Object::Integer(i) => Some(*i),
            // Some writers emit integers as reals (e.g. `/Length 120.0`).
            Object::Real(r) if r.fract() == 0.0 => Some(*r as i64),
            _ => None,
        }
    }

    /// Numeric value; accepts both integers and reals.
    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Object::Integer(i) => Some(*i as f64),
            Object::Real(r) => Some(*r),
            _ => None,
        }
    }

    pub fn as_name(&self) -> Option<&[u8]> {
        match self {
            Object::Name(n) => Some(n),
            _ => None,
        }
    }

    pub fn as_string(&self) -> Option<&[u8]> {
        match self {
            Object::String(s) => Some(s),
            _ => None,
        }
    }

    pub fn as_array(&self) -> Option<&[Object]> {
        match self {
            Object::Array(a) => Some(a),
            _ => None,
        }
    }

    /// The dictionary of a dictionary *or* of a stream.
    pub fn as_dict(&self) -> Option<&Dictionary> {
        match self {
            Object::Dictionary(d) => Some(d),
            Object::Stream(s) => Some(&s.dict),
            _ => None,
        }
    }

    pub fn as_stream(&self) -> Option<&Stream> {
        match self {
            Object::Stream(s) => Some(s),
            _ => None,
        }
    }

    pub fn as_reference(&self) -> Option<ObjRef> {
        match self {
            Object::Reference(r) => Some(*r),
            _ => None,
        }
    }

    pub fn is_null(&self) -> bool {
        matches!(self, Object::Null)
    }

    pub fn type_name(&self) -> &'static str {
        match self {
            Object::Null => "null",
            Object::Bool(_) => "boolean",
            Object::Integer(_) => "integer",
            Object::Real(_) => "real",
            Object::String(_) => "string",
            Object::Name(_) => "name",
            Object::Array(_) => "array",
            Object::Dictionary(_) => "dictionary",
            Object::Stream(_) => "stream",
            Object::Reference(_) => "reference",
        }
    }
}

/// Writes objects in PDF syntax (stream data is summarised, not dumped).
impl fmt::Display for Object {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Object::Null => f.write_str("null"),
            Object::Bool(b) => write!(f, "{b}"),
            Object::Integer(i) => write!(f, "{i}"),
            // Rust never uses exponent notation here, which PDF doesn't allow.
            Object::Real(r) if r.is_finite() => write!(f, "{r}"),
            Object::Real(_) => f.write_str("0"),
            Object::String(s) => {
                if s.iter().all(|&b| (0x20..0x7f).contains(&b)) {
                    f.write_str("(")?;
                    for &b in s {
                        if matches!(b, b'(' | b')' | b'\\') {
                            f.write_str("\\")?;
                        }
                        write!(f, "{}", b as char)?;
                    }
                    f.write_str(")")
                } else {
                    f.write_str("<")?;
                    for b in s {
                        write!(f, "{b:02X}")?;
                    }
                    f.write_str(">")
                }
            }
            Object::Name(n) => {
                f.write_str("/")?;
                for &b in n {
                    if b.is_ascii_graphic() && !is_delimiter(b) && b != b'#' {
                        write!(f, "{}", b as char)?;
                    } else {
                        write!(f, "#{b:02X}")?;
                    }
                }
                Ok(())
            }
            Object::Array(a) => {
                f.write_str("[")?;
                for (i, o) in a.iter().enumerate() {
                    if i > 0 {
                        f.write_str(" ")?;
                    }
                    write!(f, "{o}")?;
                }
                f.write_str("]")
            }
            Object::Dictionary(d) => write_dict(f, d),
            Object::Stream(s) => {
                write_dict(f, &s.dict)?;
                write!(f, " stream <{} bytes> endstream", s.raw.len())
            }
            Object::Reference(r) => write!(f, "{r}"),
        }
    }
}

impl Object {
    /// The object's PDF syntax, as `to_string()` gives it, appended to
    /// `out`, without going through `fmt` (several times faster, which
    /// matters when saving hundreds of thousands of objects).
    pub fn write_to(&self, out: &mut Vec<u8>) {
        const HEX: &[u8; 16] = b"0123456789ABCDEF";
        match self {
            Object::Null => out.extend_from_slice(b"null"),
            Object::Bool(b) => out.extend_from_slice(if *b { b"true" } else { b"false" }),
            Object::Integer(i) => write_int(out, *i),
            Object::Real(r) if r.is_finite() => {
                use std::io::Write;
                let _ = write!(out, "{r}");
            }
            Object::Real(_) => out.push(b'0'),
            Object::String(s) => {
                if s.iter().all(|&b| (0x20..0x7f).contains(&b)) {
                    out.push(b'(');
                    for &b in s {
                        if matches!(b, b'(' | b')' | b'\\') {
                            out.push(b'\\');
                        }
                        out.push(b);
                    }
                    out.push(b')');
                } else {
                    out.push(b'<');
                    for &b in s {
                        out.extend_from_slice(&[HEX[(b >> 4) as usize], HEX[(b & 15) as usize]]);
                    }
                    out.push(b'>');
                }
            }
            Object::Name(n) => write_name(out, n),
            Object::Array(a) => {
                out.push(b'[');
                for (i, o) in a.iter().enumerate() {
                    if i > 0 {
                        out.push(b' ');
                    }
                    o.write_to(out);
                }
                out.push(b']');
            }
            Object::Dictionary(d) => write_dict_to(out, d),
            Object::Stream(s) => {
                write_dict_to(out, &s.dict);
                out.extend_from_slice(b" stream <");
                write_int(out, s.raw.len() as i64);
                out.extend_from_slice(b" bytes> endstream");
            }
            Object::Reference(r) => {
                write_int(out, r.num as i64);
                out.push(b' ');
                write_int(out, r.generation as i64);
                out.extend_from_slice(b" R");
            }
        }
    }

    /// [`Object::write_to`] into a new vector.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = Vec::new();
        self.write_to(&mut out);
        out
    }
}

fn write_int(out: &mut Vec<u8>, i: i64) {
    let mut buf = [0u8; 20];
    let mut n = i.unsigned_abs();
    let mut at = buf.len();
    loop {
        at -= 1;
        buf[at] = b'0' + (n % 10) as u8;
        n /= 10;
        if n == 0 {
            break;
        }
    }
    if i < 0 {
        out.push(b'-');
    }
    out.extend_from_slice(&buf[at..]);
}

fn write_name(out: &mut Vec<u8>, n: &[u8]) {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    out.push(b'/');
    for &b in n {
        if b.is_ascii_graphic() && !is_delimiter(b) && b != b'#' {
            out.push(b);
        } else {
            out.extend_from_slice(&[b'#', HEX[(b >> 4) as usize], HEX[(b & 15) as usize]]);
        }
    }
}

fn write_dict_to(out: &mut Vec<u8>, d: &Dictionary) {
    out.extend_from_slice(b"<<");
    for (k, v) in d.iter() {
        out.push(b' ');
        write_name(out, k);
        out.push(b' ');
        v.write_to(out);
    }
    out.extend_from_slice(b" >>");
}

fn write_dict(f: &mut fmt::Formatter<'_>, d: &Dictionary) -> fmt::Result {
    f.write_str("<<")?;
    for (k, v) in d.iter() {
        write!(f, " {} {}", Object::Name(k.to_vec()), v)?;
    }
    f.write_str(" >>")
}

pub(crate) fn is_whitespace(b: u8) -> bool {
    matches!(b, b'\0' | b'\t' | b'\n' | b'\x0C' | b'\r' | b' ')
}

pub(crate) fn is_delimiter(b: u8) -> bool {
    matches!(b, b'(' | b')' | b'<' | b'>' | b'[' | b']' | b'{' | b'}' | b'/' | b'%')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn objects_stay_small() {
        // Objects are the bulk of a large document's memory.
        assert_eq!(std::mem::size_of::<Bytes>(), 16);
        assert!(std::mem::size_of::<Object>() <= 48, "{}", std::mem::size_of::<Object>());
    }

    #[test]
    fn bytes_match_display() {
        let mut d = Dictionary::new();
        d.set(b"Type".to_vec(), Object::Name(b"Font".to_vec()));
        d.set(b"A B#(".to_vec(), Object::Array(vec![Object::Real(0.1), Object::Real(-12.5), Object::Real(1e-7), Object::Real(3e20)]));
        d.set(b"S".to_vec(), Object::String(b"a(b)c\\d".to_vec()));
        d.set(b"H".to_vec(), Object::String(vec![0, 0xff, 0x41]));
        d.set(b"R".to_vec(), Object::Reference(ObjRef::new(12, 3)));
        let objects = [
            Object::Null,
            Object::Bool(true),
            Object::Bool(false),
            Object::Integer(0),
            Object::Integer(-42),
            Object::Integer(i64::MIN),
            Object::Integer(i64::MAX),
            Object::Real(f64::NAN),
            Object::Real(2.0),
            Object::Name(Vec::new()),
            Object::Array(Vec::new()),
            Object::Dictionary(Dictionary::new()),
            Object::Dictionary(d.clone()),
            Object::Stream(Stream { dict: d, raw: vec![1, 2, 3].into() }),
        ];
        for o in &objects {
            assert_eq!(String::from_utf8(o.to_bytes()).unwrap(), o.to_string());
        }
    }
}
