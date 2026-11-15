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

use std::collections::BTreeMap;

/// Nesting bound: the session record is flat; anything this deep is not it.
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
            Value::Number(text) if text.bytes().all(|b| b.is_ascii_digit()) => text.parse().ok(),
            _ => None,
        }
    }
}

/// Parse `text` as exactly one JSON value (surrounding whitespace allowed).
#[must_use]
pub fn parse(text: &str) -> Option<Value> {
    let mut p = Parser {
        bytes: text.as_bytes(),
        at: 0,
    };
    let value = p.value(0)?;
    p.ws();
    (p.at == p.bytes.len()).then_some(value)
}

struct Parser<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl Parser<'_> {
    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.at).copied()
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
        (self.bytes.get(self.at..end)? == word).then(|| self.at = end)
    }

    fn value(&mut self, depth: usize) -> Option<Value> {
        if depth > MAX_DEPTH {
            return None;
        }
        self.ws();
        match self.peek()? {
            b'n' => self.eat(b"null").map(|()| Value::Null),
            b't' => self.eat(b"true").map(|()| Value::Bool(true)),
            b'f' => self.eat(b"false").map(|()| Value::Bool(false)),
            b'"' => self.string().map(Value::String),
            b'[' => self.array(depth),
            b'{' => self.object(depth),
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
            let value = self.value(depth.saturating_add(1))?;
            if map.insert(key, value).is_some() {
                return None;
            }
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
        let text = std::str::from_utf8(self.bytes.get(start..self.at)?).ok()?;
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
            out.push_str(std::str::from_utf8(self.bytes.get(start..self.at)?).ok()?);
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
        let digits = std::str::from_utf8(self.bytes.get(self.at..end)?).ok()?;
        if !digits.bytes().all(|b| b.is_ascii_hexdigit()) {
            return None;
        }
        self.at = end;
        u32::from_str_radix(digits, 16).ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_a_claude_session_record() {
        let text = r#"{"pid":26750,"sessionId":"7c1e2a90-4b3d-4f6e-9a18-2d5c6b7e8f01","cwd":"/h/u/work","startedAt":1792867292005,"procStart":"Sat Oct 24 18:41:29 2026","version":"2.1.310","peerProtocol":1,"peerFeatures":["notify_idle","artifact_yield"],"kind":"interactive","entrypoint":"cli","status":"busy","updatedAt":1792874003933,"statusUpdatedAt":1792874003933}"#;
        let v = parse(text).expect("parses");
        assert_eq!(v.get("pid").and_then(Value::as_u64), Some(26750));
        assert_eq!(v.get("status").and_then(Value::as_str), Some("busy"));
        assert_eq!(
            v.get("statusUpdatedAt").and_then(Value::as_u64),
            Some(1_792_874_003_933)
        );
        assert!(matches!(v.get("peerFeatures"), Some(Value::Array(a)) if a.len() == 2));
    }

    #[test]
    fn decodes_escapes_and_surrogate_pairs() {
        let v = parse(r#""a\"b\\c\n\u00e9\ud83d\ude00""#).expect("parses");
        assert_eq!(v.as_str(), Some("a\"b\\c\né😀"));
    }

    #[test]
    fn refuses_what_is_not_exactly_one_json_value() {
        for bad in [
            "",
            "{",
            "{}x",
            "{\"a\":1,}",
            "[1,]",
            "01",
            "1.",
            "-",
            "\"\\ud800\"",
            "\"\\udc00\"",
            "\"tab\there\"",
            "{\"a\":1,\"a\":2}",
            "nul",
            "{'a':1}",
        ] {
            assert_eq!(parse(bad), None, "{bad:?} must be refused");
        }
    }

    #[test]
    fn refuses_nesting_past_the_bound() {
        let deep = "[".repeat(MAX_DEPTH + 2) + &"]".repeat(MAX_DEPTH + 2);
        assert_eq!(parse(&deep), None);
        let ok = "[".repeat(MAX_DEPTH) + &"]".repeat(MAX_DEPTH);
        assert!(parse(&ok).is_some());
    }

    #[test]
    fn every_prefix_of_a_record_is_answered_without_a_panic() {
        // `number` and `string` take `bytes.get(start..at)`: every proper prefix
        // ends inside a number, a string or an escape and must be refused.
        let text = r#"{"n":-12.5e+3,"s":"a\"b\u00e9","t":[true,null,0]}"#;
        assert!(parse(text).is_some());
        for end in 0..text.len() {
            if let Some(prefix) = text.get(..end) {
                assert_eq!(parse(prefix), None, "{prefix:?} must be refused");
            }
        }
    }

    #[test]
    fn integers_only_through_as_u64() {
        assert_eq!(parse("12").and_then(|v| v.as_u64()), Some(12));
        assert_eq!(parse("-1").and_then(|v| v.as_u64()), None);
        assert_eq!(parse("1.5").and_then(|v| v.as_u64()), None);
        assert_eq!(parse("1e3").and_then(|v| v.as_u64()), None);
        assert_eq!(parse("99999999999999999999").and_then(|v| v.as_u64()), None);
    }
}
