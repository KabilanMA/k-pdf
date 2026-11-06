//! Tokenizer and object parser (ISO 32000 §7.2–7.3).
//!
//! The parser is deliberately lenient: real-world PDFs, especially old ones,
//! often break the spec in small ways (wrong stream lengths, missing values,
//! stray delimiters). Where a sensible interpretation exists we take it
//! instead of failing.

use crate::error::{Error, Result};
use crate::object::{is_delimiter, is_whitespace, Dictionary, ObjRef, Object, Stream};

/// Nesting limit for arrays/dictionaries, protecting against stack overflow
/// on malicious input.
const MAX_DEPTH: usize = 256;

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Token<'a> {
    Integer(i64),
    Real(f64),
    String(Vec<u8>),
    Name(Vec<u8>),
    ArrayStart,
    ArrayEnd,
    DictStart,
    DictEnd,
    /// Any other run of regular characters: `obj`, `R`, `true`, operators…
    /// Stray delimiters (`)`, `>`, `{`, `}`) are also reported here.
    Keyword(&'a [u8]),
}

pub(crate) struct Lexer<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Lexer<'a> {
    pub fn new(data: &'a [u8], pos: usize) -> Self {
        Lexer { data, pos: pos.min(data.len()) }
    }

    pub fn pos(&self) -> usize {
        self.pos
    }

    pub fn set_pos(&mut self, pos: usize) {
        self.pos = pos.min(self.data.len());
    }

    pub fn data(&self) -> &'a [u8] {
        self.data
    }

    /// Skips whitespace and `%` comments.
    pub fn skip_whitespace(&mut self) {
        while let Some(&b) = self.data.get(self.pos) {
            if is_whitespace(b) {
                self.pos += 1;
            } else if b == b'%' {
                while let Some(&c) = self.data.get(self.pos) {
                    if c == b'\r' || c == b'\n' {
                        break;
                    }
                    self.pos += 1;
                }
            } else {
                break;
            }
        }
    }

    pub fn peek(&mut self) -> Option<Token<'a>> {
        let saved = self.pos;
        let tok = self.next_token();
        self.pos = saved;
        tok
    }

    /// Returns the next token, or `None` at end of input.
    pub fn next_token(&mut self) -> Option<Token<'a>> {
        self.skip_whitespace();
        let &b = self.data.get(self.pos)?;
        let start = self.pos;
        Some(match b {
            b'[' => {
                self.pos += 1;
                Token::ArrayStart
            }
            b']' => {
                self.pos += 1;
                Token::ArrayEnd
            }
            b'<' if self.data.get(self.pos + 1) == Some(&b'<') => {
                self.pos += 2;
                Token::DictStart
            }
            b'>' if self.data.get(self.pos + 1) == Some(&b'>') => {
                self.pos += 2;
                Token::DictEnd
            }
            b'<' => Token::String(self.read_hex_string()),
            b'(' => Token::String(self.read_literal_string()),
            b'/' => Token::Name(self.read_name()),
            b'0'..=b'9' | b'+' | b'-' | b'.' => self.read_number(),
            b')' | b'>' | b'{' | b'}' => {
                self.pos += 1;
                Token::Keyword(&self.data[start..self.pos])
            }
            _ => {
                while let Some(&c) = self.data.get(self.pos) {
                    if is_whitespace(c) || is_delimiter(c) {
                        break;
                    }
                    self.pos += 1;
                }
                Token::Keyword(&self.data[start..self.pos])
            }
        })
    }

    fn read_number(&mut self) -> Token<'a> {
        let start = self.pos;
        while let Some(&c) = self.data.get(self.pos) {
            if c.is_ascii_digit() || matches!(c, b'+' | b'-' | b'.') {
                self.pos += 1;
            } else {
                break;
            }
        }
        parse_number(&self.data[start..self.pos])
    }

    fn read_name(&mut self) -> Vec<u8> {
        self.pos += 1; // '/'
        let mut name = Vec::new();
        while let Some(&c) = self.data.get(self.pos) {
            if is_whitespace(c) || is_delimiter(c) {
                break;
            }
            // `#xx` escapes exist since PDF 1.2; in older files a '#' not
            // followed by two hex digits is just a regular character.
            if c == b'#' {
                let hi = self.data.get(self.pos + 1).and_then(|&h| hex_value(h));
                let lo = self.data.get(self.pos + 2).and_then(|&l| hex_value(l));
                if let (Some(hi), Some(lo)) = (hi, lo) {
                    name.push(hi << 4 | lo);
                    self.pos += 3;
                    continue;
                }
            }
            name.push(c);
            self.pos += 1;
        }
        name
    }

    fn read_hex_string(&mut self) -> Vec<u8> {
        self.pos += 1; // '<'
        let mut out = Vec::new();
        let mut pending: Option<u8> = None;
        while let Some(&c) = self.data.get(self.pos) {
            self.pos += 1;
            if c == b'>' {
                break;
            }
            if let Some(v) = hex_value(c) {
                match pending.take() {
                    Some(hi) => out.push(hi << 4 | v),
                    None => pending = Some(v),
                }
            }
            // Whitespace and invalid characters are ignored.
        }
        // An odd number of digits behaves as if a final 0 followed.
        if let Some(hi) = pending {
            out.push(hi << 4);
        }
        out
    }

    fn read_literal_string(&mut self) -> Vec<u8> {
        self.pos += 1; // '('
        let mut out = Vec::new();
        let mut depth = 1usize;
        while let Some(&c) = self.data.get(self.pos) {
            self.pos += 1;
            match c {
                b'(' => {
                    depth += 1;
                    out.push(c);
                }
                b')' => {
                    depth -= 1;
                    if depth == 0 {
                        break;
                    }
                    out.push(c);
                }
                b'\\' => {
                    let Some(&e) = self.data.get(self.pos) else { break };
                    self.pos += 1;
                    match e {
                        b'n' => out.push(b'\n'),
                        b'r' => out.push(b'\r'),
                        b't' => out.push(b'\t'),
                        b'b' => out.push(0x08),
                        b'f' => out.push(0x0C),
                        b'0'..=b'7' => {
                            let mut v = (e - b'0') as u32;
                            for _ in 0..2 {
                                match self.data.get(self.pos) {
                                    Some(&d @ b'0'..=b'7') => {
                                        v = v * 8 + (d - b'0') as u32;
                                        self.pos += 1;
                                    }
                                    _ => break,
                                }
                            }
                            out.push(v as u8); // high-order overflow is ignored
                        }
                        // Backslash + end-of-line: line continuation.
                        b'\r' => {
                            if self.data.get(self.pos) == Some(&b'\n') {
                                self.pos += 1;
                            }
                        }
                        b'\n' => {}
                        // `\(`, `\)`, `\\`, and unknown escapes: keep the char.
                        other => out.push(other),
                    }
                }
                // Any unescaped end-of-line is read as a single '\n'.
                b'\r' => {
                    if self.data.get(self.pos) == Some(&b'\n') {
                        self.pos += 1;
                    }
                    out.push(b'\n');
                }
                _ => out.push(c),
            }
        }
        out
    }
}

pub(crate) fn hex_value(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        b'A'..=b'F' => Some(c - b'A' + 10),
        _ => None,
    }
}

/// Parses a number leniently. Handles things like `--5`, `+.5`, `4.` and
/// trailing garbage (`1.2.3` → 1.2) seen in broken files.
fn parse_number<'a>(s: &[u8]) -> Token<'a> {
    let mut i = 0;
    let mut negative = false;
    while i < s.len() && matches!(s[i], b'+' | b'-') {
        negative ^= s[i] == b'-';
        i += 1;
    }
    let mut int_part: i64 = 0;
    let mut overflow = false;
    let mut digits_seen = false;
    while i < s.len() && s[i].is_ascii_digit() {
        digits_seen = true;
        match int_part.checked_mul(10).and_then(|v| v.checked_add((s[i] - b'0') as i64)) {
            Some(v) => int_part = v,
            None => overflow = true,
        }
        i += 1;
    }
    let is_real = i < s.len() && s[i] == b'.';
    if !is_real && !overflow {
        if !digits_seen {
            return Token::Integer(0);
        }
        return Token::Integer(if negative { -int_part } else { int_part });
    }
    // Real number (or an integer too large for i64).
    let mut text = String::new();
    if negative {
        text.push('-');
    }
    text.push('0');
    let start = s.iter().position(|c| c.is_ascii_digit() || *c == b'.').unwrap_or(s.len());
    let mut dot_seen = false;
    for &c in &s[start..] {
        if c == b'.' {
            if dot_seen {
                break;
            }
            dot_seen = true;
        } else if !c.is_ascii_digit() {
            break;
        }
        text.push(c as char);
    }
    Token::Real(text.parse::<f64>().unwrap_or(0.0))
}

/// Used by the parser to resolve an indirect `/Length` of a stream.
pub(crate) type LengthResolver<'r> = &'r dyn Fn(ObjRef) -> Option<i64>;

pub(crate) struct Parser<'a> {
    pub lex: Lexer<'a>,
}

impl<'a> Parser<'a> {
    pub fn new(data: &'a [u8], pos: usize) -> Self {
        Parser { lex: Lexer::new(data, pos) }
    }

    fn err(&self, msg: impl Into<String>) -> Error {
        Error::parse(self.lex.pos(), msg)
    }

    /// Parses one direct object (references allowed, streams not).
    pub fn parse_object(&mut self) -> Result<Object> {
        self.parse_object_depth(0)
    }

    fn parse_object_depth(&mut self, depth: usize) -> Result<Object> {
        if depth > MAX_DEPTH {
            return Err(self.err("objects nested too deeply"));
        }
        let tok = self.lex.next_token().ok_or_else(|| self.err("unexpected end of data"))?;
        self.object_from_token(tok, depth)
    }

    pub fn object_from_token(&mut self, tok: Token<'a>, depth: usize) -> Result<Object> {
        Ok(match tok {
            Token::Integer(i) => {
                // Possibly the start of a reference `num gen R`.
                if let Some(r) = self.try_reference(i) {
                    Object::Reference(r)
                } else {
                    Object::Integer(i)
                }
            }
            Token::Real(r) => Object::Real(r),
            Token::String(s) => Object::String(s),
            Token::Name(n) => Object::Name(n),
            Token::ArrayStart => Object::Array(self.parse_array(depth)?),
            Token::DictStart => Object::Dictionary(self.parse_dict(depth)?),
            Token::Keyword(b"true") => Object::Bool(true),
            Token::Keyword(b"false") => Object::Bool(false),
            Token::Keyword(b"null") => Object::Null,
            Token::Keyword(k) => {
                return Err(self.err(format!("unexpected keyword '{}'", String::from_utf8_lossy(k))));
            }
            Token::ArrayEnd | Token::DictEnd => return Err(self.err("unexpected closing delimiter")),
        })
    }

    fn try_reference(&mut self, num: i64) -> Option<ObjRef> {
        let saved = self.lex.pos();
        if let (Some(Token::Integer(generation)), Some(Token::Keyword(b"R"))) =
            (self.lex.next_token(), self.lex.next_token())
            && let (Ok(num), Ok(generation)) = (u32::try_from(num), u16::try_from(generation)) {
                return Some(ObjRef::new(num, generation));
            }
        self.lex.set_pos(saved);
        None
    }

    fn parse_array(&mut self, depth: usize) -> Result<Vec<Object>> {
        let mut items = Vec::new();
        loop {
            match self.lex.next_token() {
                None => return Err(self.err("unterminated array")),
                Some(Token::ArrayEnd) => return Ok(items),
                // A stray `>>` inside an array: assume the array was not closed.
                Some(Token::DictEnd) => return Ok(items),
                Some(tok) => items.push(self.object_from_token(tok, depth + 1)?),
            }
        }
    }

    fn parse_dict(&mut self, depth: usize) -> Result<Dictionary> {
        let mut dict = Dictionary::new();
        loop {
            match self.lex.next_token() {
                None => return Err(self.err("unterminated dictionary")),
                Some(Token::DictEnd) => return Ok(dict),
                Some(Token::Name(key)) => {
                    // A key with no value (`/Key >>`) is treated as null.
                    if matches!(self.lex.peek(), Some(Token::DictEnd)) {
                        dict.set(key, Object::Null);
                        continue;
                    }
                    let value = self.parse_object_depth(depth + 1)?;
                    // Per spec, a null value is equivalent to the key being absent.
                    if !value.is_null() {
                        dict.set(key, value);
                    }
                }
                // Garbage where a key should be: skip it.
                Some(_) => continue,
            }
        }
    }

    /// Parses `num gen obj <object> [stream…endstream] [endobj]` at the
    /// current position.
    pub fn parse_indirect(&mut self, resolve_length: LengthResolver) -> Result<(ObjRef, Object)> {
        let num = match self.lex.next_token() {
            Some(Token::Integer(n)) => u32::try_from(n).map_err(|_| self.err("bad object number"))?,
            _ => return Err(self.err("expected object number")),
        };
        let generation = match self.lex.next_token() {
            Some(Token::Integer(g)) => u16::try_from(g).map_err(|_| self.err("bad generation"))?,
            _ => return Err(self.err("expected generation number")),
        };
        if self.lex.next_token() != Some(Token::Keyword(b"obj")) {
            return Err(self.err("expected 'obj'"));
        }
        let id = ObjRef::new(num, generation);

        // An empty object (`1 0 obj endobj`) is null.
        if matches!(self.lex.peek(), Some(Token::Keyword(b"endobj"))) {
            return Ok((id, Object::Null));
        }
        let obj = self.parse_object()?;
        if let Object::Dictionary(dict) = obj {
            if matches!(self.lex.peek(), Some(Token::Keyword(b"stream"))) {
                self.lex.next_token();
                let raw = self.read_stream_data(&dict, resolve_length);
                return Ok((id, Object::Stream(Stream { dict, raw })));
            }
            return Ok((id, Object::Dictionary(dict)));
        }
        Ok((id, obj))
    }

    /// Reads stream bytes; the lexer is positioned just after `stream`.
    fn read_stream_data(&mut self, dict: &Dictionary, resolve_length: LengthResolver) -> Vec<u8> {
        let data = self.lex.data();
        let mut start = self.lex.pos();
        // Spec: `stream` is followed by CRLF or LF. Tolerate spaces and a
        // lone CR as well.
        while matches!(data.get(start), Some(b' ' | b'\t')) {
            start += 1;
        }
        match data.get(start) {
            Some(b'\r') if data.get(start + 1) == Some(&b'\n') => start += 2,
            Some(b'\r' | b'\n') => start += 1,
            _ => {}
        }

        let declared = match dict.get(b"Length") {
            Some(Object::Reference(r)) => resolve_length(*r),
            Some(o) => o.as_i64(),
            None => None,
        };

        // Trust /Length only if `endstream` really follows it.
        if let Some(len) = declared.and_then(|l| usize::try_from(l).ok())
            && let Some(end) = start.checked_add(len).filter(|&e| e <= data.len()) {
                let mut p = end;
                while data.get(p).is_some_and(|&b| is_whitespace(b)) {
                    p += 1;
                }
                if data[p..].starts_with(b"endstream") {
                    self.lex.set_pos(p + b"endstream".len());
                    return data[start..end].to_vec();
                }
            }

        // Fallback: scan for `endstream` and drop the EOL before it.
        let (mut end, after) = match find(&data[start..], b"endstream") {
            Some(i) => (start + i, start + i + b"endstream".len()),
            None => (data.len(), data.len()),
        };
        if end > start && data[end - 1] == b'\n' {
            end -= 1;
        }
        if end > start && data[end - 1] == b'\r' {
            end -= 1;
        }
        self.lex.set_pos(after);
        data[start..end].to_vec()
    }
}

/// Naive forward byte-substring search.
pub(crate) fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    let first = needle[0];
    let mut i = 0;
    while i + needle.len() <= haystack.len() {
        match haystack[i..=haystack.len() - needle.len()].iter().position(|&b| b == first) {
            Some(off) => i += off,
            None => return None,
        }
        if &haystack[i..i + needle.len()] == needle {
            return Some(i);
        }
        i += 1;
    }
    None
}

/// Backward byte-substring search: last occurrence of `needle`.
pub(crate) fn rfind(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    (0..=haystack.len() - needle.len()).rev().find(|&i| &haystack[i..i + needle.len()] == needle)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(s: &str) -> Object {
        Parser::new(s.as_bytes(), 0).parse_object().unwrap()
    }

    #[test]
    fn numbers() {
        assert_eq!(parse("42"), Object::Integer(42));
        assert_eq!(parse("-17"), Object::Integer(-17));
        assert_eq!(parse("+3"), Object::Integer(3));
        assert_eq!(parse("3.5"), Object::Real(3.5));
        assert_eq!(parse("-.5"), Object::Real(-0.5));
        assert_eq!(parse("4."), Object::Real(4.0));
        assert_eq!(parse("--5"), Object::Integer(5)); // lenient
    }

    #[test]
    fn strings() {
        assert_eq!(parse("(a(b)c)"), Object::String(b"a(b)c".to_vec()));
        assert_eq!(parse(r"(\(\)\\\n\101)"), Object::String(b"()\\\nA".to_vec()));
        assert_eq!(parse("(line\\\r\ncont)"), Object::String(b"linecont".to_vec()));
        assert_eq!(parse("(a\r\nb)"), Object::String(b"a\nb".to_vec()));
        assert_eq!(parse("<48 65 6C6C 6F>"), Object::String(b"Hello".to_vec()));
        assert_eq!(parse("<901FA>"), Object::String(vec![0x90, 0x1F, 0xA0]));
    }

    #[test]
    fn names() {
        assert_eq!(parse("/Type"), Object::Name(b"Type".to_vec()));
        assert_eq!(parse("/A#20B"), Object::Name(b"A B".to_vec()));
        assert_eq!(parse("/C#"), Object::Name(b"C#".to_vec())); // pre-1.2 style
    }

    #[test]
    fn containers_and_refs() {
        let o = parse("<< /Kids [1 0 R 2 0 R] /Count 2 /Missing null /Odd >>");
        let d = o.as_dict().unwrap();
        assert_eq!(
            d.get(b"Kids").unwrap().as_array().unwrap(),
            &[Object::Reference(ObjRef::new(1, 0)), Object::Reference(ObjRef::new(2, 0))]
        );
        assert_eq!(d.get(b"Count"), Some(&Object::Integer(2)));
        assert!(!d.contains(b"Missing"));
        assert_eq!(parse("[1 2 3]").as_array().unwrap().len(), 3);
    }

    #[test]
    fn indirect_stream_with_bad_length() {
        let src = b"7 0 obj << /Length 999 >> stream\r\nhello\nendstream endobj";
        let (id, obj) = Parser::new(src, 0).parse_indirect(&|_| None).unwrap();
        assert_eq!(id, ObjRef::new(7, 0));
        assert_eq!(obj.as_stream().unwrap().raw, b"hello");
    }

    #[test]
    fn indirect_stream_with_ref_length() {
        let src = b"1 0 obj << /Length 2 0 R >> stream\nab>>cd\nendstream endobj";
        let (_, obj) = Parser::new(src, 0).parse_indirect(&|_| Some(6)).unwrap();
        assert_eq!(obj.as_stream().unwrap().raw, b"ab>>cd");
    }

    #[test]
    fn comments_skipped() {
        assert_eq!(parse("% comment\n 5"), Object::Integer(5));
    }
}
