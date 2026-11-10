//! The PDF object model (ISO 32000 §7.3).

use std::fmt;

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
    pub raw: Vec<u8>,
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
