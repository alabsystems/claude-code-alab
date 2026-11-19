// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Order-preserving JSON parser and two-space pretty-printer for vendor settings files.

use std::collections::{HashMap, hash_map::Entry};
use std::fmt::Write as _;

/// Maximum nested arrays and objects; the root container counts as one.
const DEPTH_MAX: usize = 64;

/// One JSON value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Json {
    Null,
    Bool(bool),
    /// The number's own text, validated against the JSON grammar and kept as written.
    Number(String),
    Str(String),
    Array(Vec<Json>),
    /// Members in source order.
    Object(Vec<(String, Json)>),
}

impl Json {
    /// Parse one document.
    ///
    /// # Errors
    /// The byte offset and a word about what was expected.
    pub fn parse(text: &str) -> Result<Self, String> {
        let mut p = Parser { src: text, at: 0 };
        p.ws();
        let value = p.value(0)?;
        p.ws();
        if p.at != p.src.len() {
            return Err(p.err("end of document"));
        }
        Ok(value)
    }

    /// The member `key` of an object, or `None` for a missing key or a non-object.
    #[must_use]
    pub fn get(&self, key: &str) -> Option<&Self> {
        match self {
            Self::Object(members) => members.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }

    /// The mutable member `key`, or `None` for a missing key or a non-object.
    pub fn get_mut(&mut self, key: &str) -> Option<&mut Self> {
        self.as_object_mut()?
            .iter_mut()
            .find(|(name, _)| name == key)
            .map(|(_, value)| value)
    }

    /// Mutable elements of an array, or `None` for a non-array.
    pub const fn as_array_mut(&mut self) -> Option<&mut Vec<Self>> {
        match self {
            Self::Array(items) => Some(items),
            _ => None,
        }
    }

    #[inline]
    #[must_use]
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Self::Str(s) => Some(s),
            _ => None,
        }
    }

    #[inline]
    #[must_use]
    pub fn as_object(&self) -> Option<&[(String, Self)]> {
        match self {
            Self::Object(members) => Some(members),
            _ => None,
        }
    }

    #[inline]
    pub const fn as_object_mut(&mut self) -> Option<&mut Vec<(String, Self)>> {
        match self {
            Self::Object(members) => Some(members),
            _ => None,
        }
    }

    /// The document, pretty-printed with two-space indentation and no trailing newline.
    #[must_use]
    pub fn render(&self) -> String {
        let mut out = String::new();
        self.render_into(&mut out, 0);
        out
    }

    fn render_into(&self, out: &mut String, depth: usize) {
        match self {
            Self::Null => out.push_str("null"),
            Self::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
            Self::Number(n) => out.push_str(n),
            Self::Str(s) => string_into(out, s),
            Self::Array(items) => {
                if items.is_empty() {
                    out.push_str("[]");
                    return;
                }
                out.push('[');
                for (i, item) in items.iter().enumerate() {
                    out.push_str(if i == 0 { "\n" } else { ",\n" });
                    indent(out, depth + 1);
                    item.render_into(out, depth + 1);
                }
                out.push('\n');
                indent(out, depth);
                out.push(']');
            }
            Self::Object(members) => {
                if members.is_empty() {
                    out.push_str("{}");
                    return;
                }
                out.push('{');
                for (i, (k, v)) in members.iter().enumerate() {
                    out.push_str(if i == 0 { "\n" } else { ",\n" });
                    indent(out, depth + 1);
                    string_into(out, k);
                    out.push_str(": ");
                    v.render_into(out, depth + 1);
                }
                out.push('\n');
                indent(out, depth);
                out.push('}');
            }
        }
    }
}

fn indent(out: &mut String, depth: usize) {
    for _ in 0..depth {
        out.push_str("  ");
    }
}

/// One JSON string literal, quoted and escaped.
#[must_use]
pub fn string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    string_into(&mut out, s);
    out
}

fn string_into(out: &mut String, s: &str) {
    out.push('"');
    let mut start = 0;
    for (at, byte) in s.bytes().enumerate() {
        if !matches!(byte, b'"' | b'\\' | 0..=0x1f) {
            continue;
        }
        // ASCII escape boundaries cannot split a UTF-8 scalar.
        out.push_str(&s[start..at]);
        match byte {
            b'"' => out.push_str("\\\""),
            b'\\' => out.push_str("\\\\"),
            b'\n' => out.push_str("\\n"),
            b'\r' => out.push_str("\\r"),
            b'\t' => out.push_str("\\t"),
            control => {
                let _ = write!(out, "\\u{control:04x}");
            }
        }
        start = at + 1;
    }
    out.push_str(&s[start..]);
    out.push('"');
}

struct Parser<'a> {
    src: &'a str,
    at: usize,
}

impl Parser<'_> {
    fn err(&self, want: impl std::fmt::Display) -> String {
        format!("byte {}: expected {want}", self.at)
    }

    const fn ws(&mut self) {
        while self.at < self.src.len()
            && matches!(self.src.as_bytes()[self.at], b' ' | b'\t' | b'\n' | b'\r')
        {
            self.at += 1;
        }
    }

    fn peek(&self) -> Option<u8> {
        self.src.as_bytes().get(self.at).copied()
    }

    fn eat(&mut self, want: u8) -> Result<(), String> {
        if self.peek() == Some(want) {
            self.at += 1;
            Ok(())
        } else {
            Err(self.err(format_args!("`{}`", want as char)))
        }
    }

    fn value(&mut self, depth: usize) -> Result<Json, String> {
        match self.peek() {
            Some(b'{' | b'[') if depth >= DEPTH_MAX => {
                Err(self.err(format_args!("at most {DEPTH_MAX} nested arrays or objects")))
            }
            Some(b'{') => self.object(depth),
            Some(b'[') => self.array(depth),
            Some(b'"') => self.string().map(Json::Str),
            Some(b't') => self.literal("true", Json::Bool(true)),
            Some(b'f') => self.literal("false", Json::Bool(false)),
            Some(b'n') => self.literal("null", Json::Null),
            Some(b'-' | b'0'..=b'9') => self.number(),
            _ => Err(self.err("a value")),
        }
    }

    fn literal(&mut self, word: &str, value: Json) -> Result<Json, String> {
        if self.src.as_bytes()[self.at..].starts_with(word.as_bytes()) {
            self.at += word.len();
            Ok(value)
        } else {
            Err(self.err(word))
        }
    }

    fn number(&mut self) -> Result<Json, String> {
        let start = self.at;
        if self.peek() == Some(b'-') {
            self.at += 1;
        }
        match self.peek() {
            Some(b'0') => self.at += 1,
            Some(b'1'..=b'9') => self.digits(),
            _ => return Err(self.err("a digit")),
        }
        if self.peek() == Some(b'.') {
            self.at += 1;
            if !matches!(self.peek(), Some(b'0'..=b'9')) {
                return Err(self.err("a digit after `.`"));
            }
            self.digits();
        }
        if matches!(self.peek(), Some(b'e' | b'E')) {
            self.at += 1;
            if matches!(self.peek(), Some(b'+' | b'-')) {
                self.at += 1;
            }
            if !matches!(self.peek(), Some(b'0'..=b'9')) {
                return Err(self.err("an exponent"));
            }
            self.digits();
        }
        Ok(Json::Number(
            self.src
                .get(start..self.at)
                .ok_or_else(|| self.err("a digit"))?
                .to_owned(),
        ))
    }

    fn digits(&mut self) {
        while matches!(self.peek(), Some(b'0'..=b'9')) {
            self.at += 1;
        }
    }

    fn string(&mut self) -> Result<String, String> {
        self.eat(b'"')?;
        let mut out = String::new();
        loop {
            let start = self.at;
            // The input is UTF-8; an ASCII terminator cannot split a scalar.
            while let Some(b) = self.peek() {
                if b == b'"' || b == b'\\' || b < 0x20 {
                    break;
                }
                self.at += 1;
            }
            out.push_str(
                self.src
                    .get(start..self.at)
                    .ok_or_else(|| self.err("UTF-8"))?,
            );
            let Some(b) = self.peek() else {
                return Err(self.err("a closing quote"));
            };
            self.at += 1;
            match b {
                b'"' => return Ok(out),
                b'\\' => {
                    let Some(esc) = self.peek() else {
                        return Err(self.err("an escape"));
                    };
                    self.at += 1;
                    match esc {
                        b'"' => out.push('"'),
                        b'\\' => out.push('\\'),
                        b'/' => out.push('/'),
                        b'b' => out.push('\u{8}'),
                        b'f' => out.push('\u{c}'),
                        b'n' => out.push('\n'),
                        b'r' => out.push('\r'),
                        b't' => out.push('\t'),
                        b'u' => {
                            let hi = self.hex4()?;
                            let ch = if (0xD800..0xDC00).contains(&hi) {
                                // A surrogate pair: `\uD83D\uDE00`.
                                if !self.src.as_bytes()[self.at..].starts_with(b"\\u") {
                                    return Err(self.err("the low half of a surrogate pair"));
                                }
                                self.at += 2;
                                let lo = self.hex4()?;
                                if !(0xDC00..0xE000).contains(&lo) {
                                    return Err(self.err("a low surrogate"));
                                }
                                0x10000 + ((hi - 0xD800) << 10) + (lo - 0xDC00)
                            } else {
                                hi
                            };
                            out.push(char::from_u32(ch).ok_or_else(|| self.err("a scalar value"))?);
                        }
                        _ => return Err(self.err("a JSON escape")),
                    }
                }
                _ => return Err(self.err("no control byte inside a string")),
            }
        }
    }

    fn hex4(&mut self) -> Result<u32, String> {
        let end = self.at + 4;
        let value = self.src.as_bytes().get(self.at..end).and_then(|digits| {
            digits
                .iter()
                .try_fold(0, |v, &b| Some((v << 4) | char::from(b).to_digit(16)?))
        });
        let Some(value) = value else {
            return Err(self.err("four hex digits"));
        };
        self.at = end;
        Ok(value)
    }

    fn array(&mut self, depth: usize) -> Result<Json, String> {
        self.eat(b'[')?;
        let mut items = Vec::new();
        self.ws();
        if self.peek() == Some(b']') {
            self.at += 1;
            return Ok(Json::Array(items));
        }
        loop {
            self.ws();
            items.push(self.value(depth + 1)?);
            self.ws();
            match self.peek() {
                Some(b',') => self.at += 1,
                Some(b']') => {
                    self.at += 1;
                    return Ok(Json::Array(items));
                }
                _ => return Err(self.err("`,` or `]`")),
            }
        }
    }

    fn object(&mut self, depth: usize) -> Result<Json, String> {
        self.eat(b'{')?;
        let mut members: Vec<(String, Json)> = Vec::new();
        let mut slot: HashMap<String, usize> = HashMap::new();
        self.ws();
        if self.peek() == Some(b'}') {
            self.at += 1;
            return Ok(Json::Object(members));
        }
        loop {
            self.ws();
            let key = self.string()?;
            self.ws();
            self.eat(b':')?;
            self.ws();
            let value = self.value(depth + 1)?;
            match slot.entry(key) {
                Entry::Occupied(entry) => members[*entry.get()].1 = value,
                Entry::Vacant(entry) => {
                    let at = members.len();
                    members.push((entry.key().clone(), value));
                    entry.insert(at);
                }
            }
            self.ws();
            match self.peek() {
                Some(b',') => self.at += 1,
                Some(b'}') => {
                    self.at += 1;
                    return Ok(Json::Object(members));
                }
                _ => return Err(self.err("`,` or `}`")),
            }
        }
    }
}

#[cfg(test)]
#[path = "settings_json_tests.rs"]
mod tests;
