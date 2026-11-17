// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! A small, strict JSON reader for the vendor's files this harness reads:
//! Claude Code's `sessions/<pid>.json` ([`crate::profile::SessionRecord`]) and
//! the rows of its transcript ([`crate::profile::permission_mode_of`],
//! [`crate::model_facts`]).
//!
//! It parses RFC 8259 JSON into [`Value`] and refuses anything else: trailing
//! bytes, unpaired surrogates, control bytes in strings, leading zeros, and
//! nesting deeper than [`MAX_DEPTH`]. A refusal is a `None` read, never a
//! guess, so a record the harness cannot read is a record that does not admit
//! a relaunch.

use std::collections::{BTreeMap, btree_map::Entry};

/// Maximum nested arrays and objects; the root container counts as one.
pub const MAX_DEPTH: usize = 32;

/// A parsed JSON value.
#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    /// `null`.
    Null,
    /// `true` or `false`.
    Bool(bool),
    /// A number, kept as its source text so integers never pass through `f64`.
    Number(String),
    /// A string.
    String(String),
    /// An array.
    Array(Vec<Value>),
    /// An object; a repeated key is a refusal ([`parse`]).
    Object(BTreeMap<String, Value>),
}

impl Value {
    /// The member `key` of an object.
    #[must_use]
    pub fn get(&self, key: &str) -> Option<&Value> {
        match self {
            Value::Object(map) => map.get(key),
            _ => None,
        }
    }

    /// A string's text.
    #[must_use]
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Value::String(s) => Some(s),
            _ => None,
        }
    }

    /// A boolean's value.
    #[must_use]
    pub const fn as_bool(&self) -> Option<bool> {
        match self {
            Value::Bool(b) => Some(*b),
            _ => None,
        }
    }

    /// A number that is a non-negative integer fitting `u64` (no fraction, no exponent).
    #[must_use]
    pub fn as_u64(&self) -> Option<u64> {
        match self {
            Value::Number(text) if text.as_bytes().first().is_some_and(u8::is_ascii_digit) => {
                text.parse().ok()
            }
            _ => None,
        }
    }
}

/// Parse `text` as exactly one JSON value (surrounding whitespace allowed).
#[must_use]
pub fn parse(text: &str) -> Option<Value> {
    let mut p = Parser { text, at: 0 };
    let value = p.value(0)?;
    p.ws();
    (p.at == p.text.len()).then_some(value)
}

struct Parser<'a> {
    text: &'a str,
    at: usize,
}

impl Parser<'_> {
    fn peek(&self) -> Option<u8> {
        self.text.as_bytes().get(self.at).copied()
    }

    fn bump(&mut self) -> Option<u8> {
        let b = self.peek()?;
        self.at = self.at.saturating_add(1);
        Some(b)
    }

    fn ws(&mut self) {
        while matches!(self.peek(), Some(b' ' | b'\t' | b'\n' | b'\r')) {
            self.at = self.at.saturating_add(1);
        }
    }

    fn eat(&mut self, word: &[u8]) -> Option<()> {
        let end = self.at.checked_add(word.len())?;
        (self.text.as_bytes().get(self.at..end)? == word).then(|| self.at = end)
    }

    fn value(&mut self, depth: usize) -> Option<Value> {
        self.ws();
        match self.peek()? {
            b'n' => self.eat(b"null").map(|()| Value::Null),
            b't' => self.eat(b"true").map(|()| Value::Bool(true)),
            b'f' => self.eat(b"false").map(|()| Value::Bool(false)),
            b'"' => self.string().map(Value::String),
            b'[' if depth < MAX_DEPTH => self.array(depth),
            b'{' if depth < MAX_DEPTH => self.object(depth),
            b'-' | b'0'..=b'9' => self.number().map(Value::Number),
            _ => None,
        }
    }

    fn array(&mut self, depth: usize) -> Option<Value> {
        self.bump()?;
        let mut items = Vec::new();
        self.ws();
        if self.peek()? == b']' {
            self.bump()?;
            return Some(Value::Array(items));
        }
        loop {
            items.push(self.value(depth.saturating_add(1))?);
            self.ws();
            match self.bump()? {
                b',' => {}
                b']' => return Some(Value::Array(items)),
                _ => return None,
            }
        }
    }

    fn object(&mut self, depth: usize) -> Option<Value> {
        self.bump()?;
        let mut map = BTreeMap::new();
        self.ws();
        if self.peek()? == b'}' {
            self.bump()?;
            return Some(Value::Object(map));
        }
        loop {
            self.ws();
            if self.peek()? != b'"' {
                return None;
            }
            let key = self.string()?;
            self.ws();
            if self.bump()? != b':' {
                return None;
            }
            let Entry::Vacant(entry) = map.entry(key) else {
                return None;
            };
            let value = self.value(depth.saturating_add(1))?;
            entry.insert(value);
            self.ws();
            match self.bump()? {
                b',' => {}
                b'}' => return Some(Value::Object(map)),
                _ => return None,
            }
        }
    }

    fn number(&mut self) -> Option<String> {
        let start = self.at;
        if self.peek() == Some(b'-') {
            self.at = self.at.saturating_add(1);
        }
        match self.bump()? {
            b'0' => {}
            b'1'..=b'9' => {
                while matches!(self.peek(), Some(b'0'..=b'9')) {
                    self.at = self.at.saturating_add(1);
                }
            }
            _ => return None,
        }
        if self.peek() == Some(b'.') {
            self.at = self.at.saturating_add(1);
            self.digits()?;
        }
        if matches!(self.peek(), Some(b'e' | b'E')) {
            self.at = self.at.saturating_add(1);
            if matches!(self.peek(), Some(b'+' | b'-')) {
                self.at = self.at.saturating_add(1);
            }
            self.digits()?;
        }
        let text = self.text.get(start..self.at)?;
        Some(text.to_string())
    }

    fn digits(&mut self) -> Option<()> {
        let start = self.at;
        while matches!(self.peek(), Some(b'0'..=b'9')) {
            self.at = self.at.saturating_add(1);
        }
        (self.at > start).then_some(())
    }

    fn string(&mut self) -> Option<String> {
        self.bump()?;
        let mut out = String::new();
        loop {
            let start = self.at;
            while let Some(b) = self.peek() {
                if b == b'"' || b == b'\\' || b < 0x20 {
                    break;
                }
                self.at = self.at.saturating_add(1);
            }
            out.push_str(self.text.get(start..self.at)?);
            match self.bump()? {
                b'"' => return Some(out),
                b'\\' => out.push(self.escape()?),
                _ => return None,
            }
        }
    }

    fn escape(&mut self) -> Option<char> {
        Some(match self.bump()? {
            b'"' => '"',
            b'\\' => '\\',
            b'/' => '/',
            b'b' => '\u{8}',
            b'f' => '\u{c}',
            b'n' => '\n',
            b'r' => '\r',
            b't' => '\t',
            b'u' => {
                let hi = self.hex4()?;
                match hi {
                    0xD800..=0xDBFF => {
                        self.eat(b"\\u")?;
                        let lo = self.hex4()?;
                        if !(0xDC00..=0xDFFF).contains(&lo) {
                            return None;
                        }
                        let high = hi.checked_sub(0xD800)?;
                        let low = lo.checked_sub(0xDC00)?;
                        char::from_u32(
                            high.checked_mul(0x400)?
                                .checked_add(low)?
                                .checked_add(0x10000)?,
                        )?
                    }
                    0xDC00..=0xDFFF => return None,
                    _ => char::from_u32(hi)?,
                }
            }
            _ => return None,
        })
    }

    fn hex4(&mut self) -> Option<u32> {
        let end = self.at.checked_add(4)?;
        let digits = self.text.get(self.at..end)?;
        if !digits.bytes().all(|b| b.is_ascii_hexdigit()) {
            return None;
        }
        self.at = end;
        u32::from_str_radix(digits, 16).ok()
    }
}

#[cfg(test)]
#[path = "json_tests.rs"]
mod tests;
