// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The model reader in an own-image move's capsule (`supervise_transfer.rs`).
//!
//! Only the reader that followed the child saw which conversations it left
//! and when it entered the one it is in. A new image that read the record's
//! conversation afresh from the child's start would take the rows written there
//! before a `/resume` back into it (the child's own earlier time there, or
//! another process's) for the child's current statements, and would lose what
//! the child stated in a conversation it has left. So the reader travels: the
//! facts, the conversation it is reading with its floor and when the child was
//! last seen there, its immutable entry facts and the file's observed stamp.
//! The new image skips an unchanged version and independently replays a
//! changed one. The transcript's path is not carried: it is looked up again.

use std::io;

use super::{ModelFacts, Reader, Said, SessionRead, Stamp};
use crate::profile;
use crate::transfer_capsule::{Decoder, Encoder};

/// The most bytes of one fact's text (a model's name, a level) a capsule
/// carries; every text the reader keeps is at most 64.
const MAX_FACT_BYTES: usize = 256;
/// Version 2 carries the entry baseline instead of the unverifiable suffix carry.
const FORMAT: u8 = 2;

fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

impl Reader {
    /// The most bytes [`Reader::put`] writes, for the capsule's budget.
    pub const TRANSFER_BOUND: usize = 2048;

    /// Write this reader into an own-image move's capsule. The conversation
    /// is written only when its id is one a transcript can have
    /// ([`profile::is_session_id`]): any other is never read, and the new
    /// image takes it up afresh, as this one would.
    ///
    /// # Errors
    /// A fact's text exceeds its bound, or the encoder refuses a value.
    pub fn put(&self, encoder: &mut Encoder) -> io::Result<()> {
        encoder.u8(FORMAT)?;
        encoder.boolean(self.pid.is_some())?;
        if let Some(pid) = self.pid {
            encoder.u32(pid)?;
        }
        put_option_u64(encoder, self.floor_ms)?;
        let session = self
            .session
            .as_ref()
            .filter(|s| profile::is_session_id(&s.id));
        encoder.boolean(session.is_some())?;
        if let Some(session) = session {
            put_session(encoder, session)?;
        }
        put_facts(encoder, &self.facts)
    }

    /// Read a reader [`Reader::put`] wrote, refusing any value it could not
    /// have written.
    ///
    /// # Errors
    /// The format is unsupported, the bytes are truncated, or a tag, id,
    /// text or entry baseline is out of its bounds.
    pub fn get(decoder: &mut Decoder<'_>) -> io::Result<Self> {
        if decoder.u8()? != FORMAT {
            return Err(invalid("unsupported model-reader transfer format"));
        }
        let pid = if decoder.boolean()? {
            Some(decoder.u32()?)
        } else {
            None
        };
        let floor_ms = get_option_u64(decoder)?;
        let session = if decoder.boolean()? {
            Some(get_session(decoder)?)
        } else {
            None
        };
        let facts = get_facts(decoder)?;
        Ok(Self {
            pid,
            floor_ms,
            session,
            line: facts.journal_line(),
            facts,
            #[cfg(test)]
            pin_asks: 0,
        })
    }
}

fn put_option_u64(encoder: &mut Encoder, value: Option<u64>) -> io::Result<()> {
    encoder.boolean(value.is_some())?;
    if let Some(value) = value {
        encoder.u64(value)?;
    }
    Ok(())
}

fn get_option_u64(decoder: &mut Decoder<'_>) -> io::Result<Option<u64>> {
    Ok(if decoder.boolean()? {
        Some(decoder.u64()?)
    } else {
        None
    })
}

/// A signed time field, as its two's-complement bits.
fn put_i64(encoder: &mut Encoder, value: i64) -> io::Result<()> {
    encoder.u64(u64::from_le_bytes(value.to_le_bytes()))
}

fn get_i64(decoder: &mut Decoder<'_>) -> io::Result<i64> {
    Ok(i64::from_le_bytes(decoder.u64()?.to_le_bytes()))
}

fn put_session(encoder: &mut Encoder, session: &SessionRead) -> io::Result<()> {
    encoder.bytes(session.id.as_bytes())?;
    encoder.u64(session.floor_ms)?;
    encoder.u64(session.seen_ms)?;
    encoder.boolean(session.stamp.is_some())?;
    if let Some(stamp) = session.stamp {
        encoder.u64(stamp.device)?;
        encoder.u64(stamp.inode)?;
        encoder.u64(stamp.len)?;
        put_i64(encoder, stamp.modified.0)?;
        put_i64(encoder, stamp.modified.1)?;
        put_i64(encoder, stamp.changed.0)?;
        put_i64(encoder, stamp.changed.1)?;
    }
    encoder.boolean(session.base.is_some())?;
    if let Some(base) = &session.base {
        put_facts(encoder, base)?;
    }
    encoder.boolean(session.resumed)?;
    encoder.u8(match session.pinned {
        None => 0,
        Some(false) => 1,
        Some(true) => 2,
    })?;
    Ok(())
}

fn get_session(decoder: &mut Decoder<'_>) -> io::Result<SessionRead> {
    let id = std::str::from_utf8(decoder.bytes(64)?)
        .ok()
        .filter(|id| profile::is_session_id(id))
        .ok_or_else(|| invalid("carried conversation id is not a session id"))?
        .to_owned();
    let floor_ms = decoder.u64()?;
    let seen_ms = decoder.u64()?;
    let stamp = if decoder.boolean()? {
        Some(Stamp {
            device: decoder.u64()?,
            inode: decoder.u64()?,
            len: decoder.u64()?,
            modified: (get_i64(decoder)?, get_i64(decoder)?),
            changed: (get_i64(decoder)?, get_i64(decoder)?),
        })
    } else {
        None
    };
    let base = if decoder.boolean()? {
        Some(get_facts(decoder)?)
    } else {
        None
    };
    let resumed = decoder.boolean()?;
    let pinned = match decoder.u8()? {
        0 => None,
        1 => Some(false),
        2 => Some(true),
        _ => return Err(invalid("a carried launch pin has no such state")),
    };
    if (resumed && base.is_none()) || (pinned.is_some() && !resumed) {
        return Err(invalid(
            "a carried resume or launch pin has no entry evidence",
        ));
    }
    Ok(SessionRead {
        floor_ms,
        seen_ms,
        stamp,
        base,
        resumed,
        pinned,
        ..SessionRead::new(&id, floor_ms)
    })
}

fn put_text(encoder: &mut Encoder, text: &str) -> io::Result<()> {
    if text.len() > MAX_FACT_BYTES {
        return Err(invalid("a fact's text exceeds its bound"));
    }
    encoder.bytes(text.as_bytes())
}

fn get_text(decoder: &mut Decoder<'_>) -> io::Result<String> {
    String::from_utf8(decoder.bytes(MAX_FACT_BYTES)?.to_vec())
        .map_err(|_| invalid("a carried fact is not UTF-8"))
}

/// A fact: 0 unstated, 1 unread, 2 a value and its text.
fn put_said(encoder: &mut Encoder, said: &Said<String>) -> io::Result<()> {
    match said {
        Said::Unsaid => encoder.u8(0),
        Said::Unread => encoder.u8(1),
        Said::Is(text) => {
            encoder.u8(2)?;
            put_text(encoder, text)
        }
    }
}

fn get_said(decoder: &mut Decoder<'_>) -> io::Result<Said<String>> {
    match decoder.u8()? {
        0 => Ok(Said::Unsaid),
        1 => Ok(Said::Unread),
        2 => Ok(Said::Is(get_text(decoder)?)),
        _ => Err(invalid("a carried fact has no such state")),
    }
}

/// The key: 0 unstated, 1 unread, 2 off, 3 on.
fn put_facts(encoder: &mut Encoder, facts: &ModelFacts) -> io::Result<()> {
    put_said(encoder, &facts.model)?;
    put_said(encoder, &facts.effort)?;
    encoder.u8(match facts.key {
        Said::Unsaid => 0,
        Said::Unread => 1,
        Said::Is(false) => 2,
        Said::Is(true) => 3,
    })?;
    encoder.boolean(facts.effort_for.is_some())?;
    if let Some(to) = &facts.effort_for {
        put_text(encoder, to)?;
    }
    encoder.boolean(facts.chose)
}

fn get_facts(decoder: &mut Decoder<'_>) -> io::Result<ModelFacts> {
    let model = get_said(decoder)?;
    let effort = get_said(decoder)?;
    let key = match decoder.u8()? {
        0 => Said::Unsaid,
        1 => Said::Unread,
        2 => Said::Is(false),
        3 => Said::Is(true),
        _ => return Err(invalid("the carried key has no such state")),
    };
    let effort_for = if decoder.boolean()? {
        Some(get_text(decoder)?)
    } else {
        None
    };
    Ok(ModelFacts {
        model,
        effort,
        key,
        effort_for,
        chose: decoder.boolean()?,
    })
}
