//! Validated borrowed JSON with a caller-owned token tape.
//!
//! Duplicate decoded keys and unpaired UTF-16 surrogates are rejected. Numeric
//! lexemes are preserved; conversion to an integer is checked, never truncated.
use crate::{Error, ErrorKind};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Kind {
    Object,
    Array,
    String,
    Number,
    Bool,
    Null,
}

/// A compact index into the input; no pointers and no owned strings.
#[derive(Clone, Copy, Debug)]
pub struct Token {
    start: u32,
    end: u32,
    next: u32,
    kind: Kind,
}
impl Token {
    pub const EMPTY: Self = Self {
        start: 0,
        end: 0,
        next: 0,
        kind: Kind::Null,
    };
}

#[derive(Clone, Copy, Debug)]
pub struct Limits {
    pub max_bytes: usize,
    /// Hard implementation ceiling: 64 container levels, regardless of this value.
    pub max_depth: usize,
    /// Maximum members per object or elements per array.
    pub max_members: usize,
    /// Maximum decoded UTF-8 bytes per string, including object keys.
    pub max_string_bytes: usize,
}
impl Default for Limits {
    fn default() -> Self {
        Self {
            max_bytes: 1 << 20,
            max_depth: 32,
            max_members: 256,
            max_string_bytes: 1 << 16,
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Document<'a> {
    input: &'a str,
    tokens: &'a [Token],
}
impl<'a> Document<'a> {
    pub fn parse(input: &'a [u8], storage: &'a mut [Token], limits: Limits) -> Result<Self, Error> {
        if input.len() > limits.max_bytes || input.len() > u32::MAX as usize {
            return Err(Error::new(
                ErrorKind::InputLimit,
                limits.max_bytes.min(input.len()),
            ));
        }
        let input = core::str::from_utf8(input)
            .map_err(|e| Error::new(ErrorKind::Utf8, e.valid_up_to()))?;
        let used = {
            let mut p = Parser {
                input,
                tokens: storage,
                at: 0,
                used: 0,
                limits,
            };
            p.value(0)?;
            p.ws();
            if p.at != input.len() {
                return Err(p.error(ErrorKind::Json));
            }
            p.used
        };
        Ok(Self {
            input,
            tokens: &storage[..used],
        })
    }
    pub fn root(self) -> Value<'a> {
        Value {
            doc: self,
            index: 0,
        }
    }
    pub fn tokens_used(self) -> usize {
        self.tokens.len()
    }
    pub fn token_bytes(self) -> usize {
        core::mem::size_of_val(self.tokens)
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Value<'a> {
    doc: Document<'a>,
    index: usize,
}
impl<'a> Value<'a> {
    fn token(self) -> Token {
        self.doc.tokens[self.index]
    }
    pub fn kind(self) -> Kind {
        self.token().kind
    }
    pub fn offset(self) -> usize {
        self.token().start as usize
    }
    pub fn raw(self) -> &'a str {
        let t = self.token();
        &self.doc.input[t.start as usize..t.end as usize]
    }
    pub fn string(self) -> Option<JsonString<'a>> {
        (self.kind() == Kind::String).then(|| JsonString {
            raw: &self.raw()[1..self.raw().len() - 1],
        })
    }
    pub fn integer(self) -> Result<i64, Error> {
        if self.kind() != Kind::Number {
            return Err(Error::new(ErrorKind::Type, self.offset()));
        }
        self.raw()
            .parse()
            .map_err(|_| Error::new(ErrorKind::Integer, self.offset()))
    }
    pub fn boolean(self) -> Option<bool> {
        (self.kind() == Kind::Bool).then(|| self.raw() == "true")
    }
    pub fn members(self) -> Members<'a> {
        Members {
            value: self,
            at: self.index + 1,
            end: if self.kind() == Kind::Object {
                self.token().next as usize
            } else {
                self.index + 1
            },
        }
    }
    pub fn elements(self) -> Elements<'a> {
        Elements {
            value: self,
            at: self.index + 1,
            end: if self.kind() == Kind::Array {
                self.token().next as usize
            } else {
                self.index + 1
            },
        }
    }
    pub fn get(self, key: &str) -> Option<Self> {
        self.members()
            .find_map(|(k, v)| k.matches(key).then_some(v))
    }
    pub fn require(self, key: &str) -> Result<Self, Error> {
        self.get(key)
            .ok_or(Error::new(ErrorKind::InvalidParams, self.offset()))
    }
}

pub struct Members<'a> {
    value: Value<'a>,
    at: usize,
    end: usize,
}
impl<'a> Iterator for Members<'a> {
    type Item = (JsonString<'a>, Value<'a>);
    fn next(&mut self) -> Option<Self::Item> {
        if self.at >= self.end {
            return None;
        }
        let key = Value {
            doc: self.value.doc,
            index: self.at,
        };
        let value = Value {
            doc: self.value.doc,
            index: key.token().next as usize,
        };
        self.at = value.token().next as usize;
        Some((key.string()?, value))
    }
}
pub struct Elements<'a> {
    value: Value<'a>,
    at: usize,
    end: usize,
}
impl<'a> Iterator for Elements<'a> {
    type Item = Value<'a>;
    fn next(&mut self) -> Option<Self::Item> {
        if self.at >= self.end {
            return None;
        }
        let value = Value {
            doc: self.value.doc,
            index: self.at,
        };
        self.at = value.token().next as usize;
        Some(value)
    }
}

/// Validated JSON string contents. Escaped and unescaped spellings compare equally.
#[derive(Clone, Copy, Debug)]
pub struct JsonString<'a> {
    raw: &'a str,
}
impl<'a> JsonString<'a> {
    pub fn as_str(self) -> Option<&'a str> {
        (!self.raw.as_bytes().contains(&b'\\')).then_some(self.raw)
    }
    pub fn chars(self) -> DecodedChars<'a> {
        DecodedChars {
            raw: self.raw,
            at: 0,
        }
    }
    pub fn matches(self, value: &str) -> bool {
        if let Some(raw) = self.as_str() {
            raw == value
        } else {
            self.chars().eq(value.chars())
        }
    }
    pub fn equals(self, other: Self) -> bool {
        self.raw == other.raw || self.chars().eq(other.chars())
    }
    pub fn decoded_len(self) -> usize {
        self.chars().map(char::len_utf8).sum()
    }
    pub fn decode_into(self, output: &mut [u8]) -> Result<&str, Error> {
        let needed = self.decoded_len();
        if output.len() < needed {
            return Err(Error::capacity(ErrorKind::Buffer, 0, needed));
        }
        let mut at = 0;
        for c in self.chars() {
            let n = c.len_utf8();
            c.encode_utf8(&mut output[at..at + n]);
            at += n;
        }
        core::str::from_utf8(&output[..at]).map_err(|_| Error::new(ErrorKind::Utf8, 0))
    }
}

pub struct DecodedChars<'a> {
    raw: &'a str,
    at: usize,
}
impl Iterator for DecodedChars<'_> {
    type Item = char;
    fn next(&mut self) -> Option<char> {
        if self.at == self.raw.len() {
            return None;
        }
        decode_char(self.raw, &mut self.at).ok()
    }
}

fn hex(input: &[u8], at: &mut usize) -> Result<u32, Error> {
    let mut n = 0;
    for _ in 0..4 {
        let d = match input.get(*at).copied() {
            Some(b'0'..=b'9') => input[*at] - b'0',
            Some(b'a'..=b'f') => input[*at] - b'a' + 10,
            Some(b'A'..=b'F') => input[*at] - b'A' + 10,
            _ => return Err(Error::new(ErrorKind::Json, *at)),
        };
        n = (n << 4) | u32::from(d);
        *at += 1;
    }
    Ok(n)
}
fn decode_char(input: &str, at: &mut usize) -> Result<char, Error> {
    let bytes = input.as_bytes();
    if bytes.get(*at) != Some(&b'\\') {
        let c = input
            .get(*at..)
            .and_then(|s| s.chars().next())
            .ok_or(Error::new(ErrorKind::Json, *at))?;
        if c <= '\u{1f}' {
            return Err(Error::new(ErrorKind::Json, *at));
        }
        *at += c.len_utf8();
        return Ok(c);
    }
    *at += 1;
    let escape = bytes
        .get(*at)
        .copied()
        .ok_or(Error::new(ErrorKind::Json, *at))?;
    *at += 1;
    match escape {
        b'"' => Ok('"'),
        b'\\' => Ok('\\'),
        b'/' => Ok('/'),
        b'b' => Ok('\u{8}'),
        b'f' => Ok('\u{c}'),
        b'n' => Ok('\n'),
        b'r' => Ok('\r'),
        b't' => Ok('\t'),
        b'u' => {
            let mut n = hex(bytes, at)?;
            if (0xd800..=0xdbff).contains(&n) {
                if bytes.get(*at..*at + 2) != Some(b"\\u") {
                    return Err(Error::new(ErrorKind::Json, *at));
                }
                *at += 2;
                let low = hex(bytes, at)?;
                if !(0xdc00..=0xdfff).contains(&low) {
                    return Err(Error::new(ErrorKind::Json, *at - 4));
                }
                n = 0x10000 + ((n - 0xd800) << 10) + low - 0xdc00;
            }
            char::from_u32(n).ok_or(Error::new(ErrorKind::Json, *at - 4))
        }
        _ => Err(Error::new(ErrorKind::Json, *at - 1)),
    }
}

struct Parser<'a, 't> {
    input: &'a str,
    tokens: &'t mut [Token],
    at: usize,
    used: usize,
    limits: Limits,
}
impl Parser<'_, '_> {
    fn error(&self, kind: ErrorKind) -> Error {
        Error::new(kind, self.at)
    }
    fn byte(&self) -> Option<u8> {
        self.input.as_bytes().get(self.at).copied()
    }
    fn ws(&mut self) {
        while matches!(self.byte(), Some(b' ' | b'\t' | b'\n' | b'\r')) {
            self.at += 1;
        }
    }
    fn string(&mut self) -> Result<(), Error> {
        if self.byte() != Some(b'"') {
            return Err(self.error(ErrorKind::Json));
        }
        self.at += 1;
        let mut len = 0;
        while self.byte() != Some(b'"') {
            let c = decode_char(self.input, &mut self.at)?;
            len += c.len_utf8();
            if len > self.limits.max_string_bytes {
                return Err(self.error(ErrorKind::StringLimit));
            }
        }
        self.at += 1;
        Ok(())
    }
    fn digits(&mut self) -> Result<(), Error> {
        let start = self.at;
        while self.byte().is_some_and(|b| b.is_ascii_digit()) {
            self.at += 1;
        }
        if self.at == start {
            Err(self.error(ErrorKind::Json))
        } else {
            Ok(())
        }
    }
    fn number(&mut self) -> Result<(), Error> {
        if self.byte() == Some(b'-') {
            self.at += 1;
        }
        match self.byte() {
            Some(b'0') => self.at += 1,
            Some(b'1'..=b'9') => self.digits()?,
            _ => return Err(self.error(ErrorKind::Json)),
        }
        if self.byte() == Some(b'.') {
            self.at += 1;
            self.digits()?;
        }
        if matches!(self.byte(), Some(b'e' | b'E')) {
            self.at += 1;
            if matches!(self.byte(), Some(b'+' | b'-')) {
                self.at += 1;
            }
            self.digits()?;
        }
        Ok(())
    }
    fn value(&mut self, depth: usize) -> Result<usize, Error> {
        self.ws();
        let start = self.at;
        let index = self.used;
        if index == self.tokens.len() || index >= u32::MAX as usize {
            return Err(Error::capacity(ErrorKind::Tokens, start, index + 1));
        }
        self.used += 1;
        let kind = match self.byte() {
            Some(b'{') | Some(b'[') => {
                if depth >= self.limits.max_depth.min(64) {
                    return Err(self.error(ErrorKind::Depth));
                }
                let object = self.byte() == Some(b'{');
                let close = if object { b'}' } else { b']' };
                self.at += 1;
                self.ws();
                let mut count = 0;
                if self.byte() != Some(close) {
                    loop {
                        if count >= self.limits.max_members {
                            return Err(self.error(ErrorKind::Members));
                        }
                        count += 1;
                        if object {
                            if self.byte() != Some(b'"') {
                                return Err(self.error(ErrorKind::Json));
                            }
                            let key_index = self.value(depth + 1)?;
                            let key = self.tokens[key_index];
                            let raw = &self.input[key.start as usize + 1..key.end as usize - 1];
                            let mut prior = index + 1;
                            while prior < key_index {
                                let t = self.tokens[prior];
                                let old = &self.input[t.start as usize + 1..t.end as usize - 1];
                                if (JsonString { raw }).equals(JsonString { raw: old }) {
                                    return Err(Error::new(
                                        ErrorKind::DuplicateKey,
                                        key.start as usize,
                                    ));
                                }
                                prior = self.tokens[t.next as usize].next as usize;
                            }
                            self.ws();
                            if self.byte() != Some(b':') {
                                return Err(self.error(ErrorKind::Json));
                            }
                            self.at += 1;
                        }
                        self.value(depth + 1)?;
                        self.ws();
                        if self.byte() == Some(close) {
                            break;
                        }
                        if self.byte() != Some(b',') {
                            return Err(self.error(ErrorKind::Json));
                        }
                        self.at += 1;
                        self.ws();
                    }
                }
                self.at += 1;
                if object { Kind::Object } else { Kind::Array }
            }
            Some(b'"') => {
                self.string()?;
                Kind::String
            }
            Some(b'-' | b'0'..=b'9') => {
                self.number()?;
                Kind::Number
            }
            Some(b't') if self.input.as_bytes().get(self.at..self.at + 4) == Some(b"true") => {
                self.at += 4;
                Kind::Bool
            }
            Some(b'f') if self.input.as_bytes().get(self.at..self.at + 5) == Some(b"false") => {
                self.at += 5;
                Kind::Bool
            }
            Some(b'n') if self.input.as_bytes().get(self.at..self.at + 4) == Some(b"null") => {
                self.at += 4;
                Kind::Null
            }
            _ => return Err(self.error(ErrorKind::Json)),
        };
        self.tokens[index] = Token {
            start: start as u32,
            end: self.at as u32,
            next: self.used as u32,
            kind,
        };
        Ok(index)
    }
}
