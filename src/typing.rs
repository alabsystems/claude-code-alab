// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The fenced typing client: how this harness types into its session, and only
//! while its claim stands at the revision it read.
//!
//! aterm types for a claim holder only a write that names the claim's holder,
//! its revision and the typing duty it performs, all three judged again at the
//! write itself on the connection that made the claim, together with the
//! person's count the write was judged at ([`TypedFence`]). [`Typist::write`]
//! makes one such write:
//!
//! 1. one fresh `status`, judged by [`judge`]: the claim is this harness's,
//!    proven, takes the duty and states its revision; the policy still stands;
//!    nobody has typed within the policy's `human_grace_s`;
//! 2. for a turn, the composer holds no draft (`status composer=`), and one
//!    fresh `text --json` ([`Screen`]) draws a composer frame, judges the
//!    person's grace again, gives the person's count the turn names, and is the
//!    screen the submit guard is built from with aterm's own readers
//!    ([`composer_guard`]);
//! 3. the write, once ([`send_verb`], [`key_verb`], [`turn_verb`]);
//! 4. aterm's answer, read as a [`Reply`].
//!
//! Two of aterm's answers are acted on here, before the caller sees them:
//!
//! * **A refused authority is read again, never retried.** `ERR denied
//!   supervisor input authority` says the claim moved between the read and the
//!   write (a release, a reclaim, a replacement, another connection). One more
//!   `status` is read and handed back with the refusal ([`Typed::Denied`]); the
//!   write is not sent again, at the new revision or any other, because what was
//!   judged at the old one need not hold at the new.
//! * **A person's key is theirs for the grace.** A write a gesture overtook is
//!   skipped by aterm ([`Reply::Person`]), and the next attempt reads `status`
//!   and sends nothing until the policy's `human_grace_s` has passed since that
//!   key ([`Typed::Held`] with the grace left).
//!
//! Nothing here is a claim: the supervisor's claim takes no typing duty yet
//! (`SCOPES` in `src/supervise_link.rs`), so no part of the harness calls this
//! client, and its tests drive it against a stand-in aterm.

use std::io;
use std::time::Duration;

use alab_policy::guard::composer_guard_at;
use aterm_phase::{ClaudeReader, ScreenReader as _};

use crate::aterm::{self, Hold, Human, Policy, ReplyError, Scope, Status};
use crate::aterm_wire::{
    Connection, TURN_TIMEOUT_MS, TypedFence, key_verb, send_verb, session_id, turn_verb,
};

/// The most rows a turn's reply or a screen read may carry.
const MAX_ROWS: usize = 1024;

/// How much longer than the turn's own `timeout=` its exchange may take: the
/// settle after the submit and the reply's rows.
const TURN_SLACK: Duration = Duration::from_secs(5);

/// One write a typing duty makes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Write<'a> {
    /// `send`: the text as typed.
    Send(&'a str),
    /// `key`: one named key.
    Key(&'a str),
    /// `turn`: the text typed into Claude Code's composer and submitted with an
    /// Enter guarded on the composer's row.
    Turn(&'a str),
}

/// What aterm answered a typed write. Each is read from aterm's own words
/// (`aterm/crates/aterm-gui/src/control_input.rs`, `guarded_input_reply`;
/// `control_session.rs`, `cmd_turn` and `fenced_submit`;
/// `aterm/crates/aterm-types/src/verb_page/turn.rs`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Reply {
    /// `OK seq=<n>`: a `send` or `key` written whole.
    Written {
        /// The content sequence after it, when aterm stamped one.
        seq: Option<u64>,
    },
    /// `OK <n> turn submitted=1 …`: the turn's text went and its submit showed.
    Submitted,
    /// `OK <n> turn submitted=0 … pressed=<k>`: Enter was pressed and no submit
    /// showed, so the program may yet take it.
    Unverified,
    /// The text stands in the composer unsubmitted: a turn skipped after it
    /// typed (`reason=guard`, or any reason with `typed=` or `pasted=` above 0),
    /// one that pressed no Enter, or `ERR key delivery failed sink=busy`. Its
    /// reason word, or `busy`.
    Left(String),
    /// A person's gesture since the read: nothing written
    /// (`OK skipped reason=person seq=<n>`, or a turn's
    /// `OK 0 turn skipped reason=person submitted=0 seq=<n> id=<n>`).
    Person {
        /// The content sequence aterm stamped.
        seq: Option<u64>,
    },
    /// Any other skip that wrote nothing (`OK skipped reason=changed`, a turn
    /// `reason=halted` before its first write): its reason word.
    Skipped(String),
    /// `ERR busy sink`, or `ERR busy supervisor input in flight`: nothing
    /// written, and the same write may be offered again later.
    Busy(String),
    /// `ERR busy supervisor_act=observe …`: aterm has not confirmed the agent's
    /// image and types nothing into it for a harness; never offered again.
    Observed,
    /// `ERR denied supervisor input authority`: the fence no longer names the
    /// claim. [`Typist::write`] answers it with [`Typed::Denied`].
    Denied,
    /// `ERR usage: …`: aterm read the line's words otherwise than this client
    /// wrote them. A defect here, never retried.
    Usage(String),
    /// It may have been partly written: `ERR key delivery failed`, `ERR paste
    /// delivery failed`, `ERR write failed …`.
    InDoubt(String),
    /// Any other `ERR` (`ERR halted`, `ERR busy turn=…`, `ERR busy lease=…`,
    /// `ERR busy supervisor=…`, `ERR busy input-unread …`): nothing written.
    Refused(String),
}

/// `field=` of an `OK` line's words, as canonical decimal.
fn number(words: &[&str], field: &str) -> Option<u64> {
    words
        .iter()
        .find_map(|word| word.strip_prefix(field)?.strip_prefix('='))
        .filter(|value| {
            !value.is_empty()
                && value.bytes().all(|byte| byte.is_ascii_digit())
                && (value.len() == 1 || !value.starts_with('0'))
        })
        .and_then(|value| value.parse().ok())
}

/// `field=` of an `OK` line's words, as one journal word.
fn word(words: &[&str], field: &str) -> Option<String> {
    words
        .iter()
        .find_map(|word| word.strip_prefix(field)?.strip_prefix('='))
        .map(aterm::journal_word)
}

impl Reply {
    /// Read an `ERR` line, which every typed verb answers alike.
    fn refusal(line: &str) -> Self {
        let kept = || aterm::journal_word(line);
        if line == "ERR busy sink" || line == "ERR busy supervisor input in flight" {
            Self::Busy(kept())
        } else if line.starts_with("ERR busy supervisor_act=") {
            Self::Observed
        } else if line == "ERR denied supervisor input authority" {
            Self::Denied
        } else if line.starts_with("ERR usage") {
            Self::Usage(kept())
        } else if line == "ERR key delivery failed sink=busy" {
            Self::Left("busy".to_string())
        } else if line == "ERR key delivery failed"
            || line == "ERR paste delivery failed"
            || line.starts_with("ERR write failed")
        {
            Self::InDoubt(kept())
        } else {
            Self::Refused(kept())
        }
    }

    /// Read the one-line answer to a `send` or `key`.
    #[must_use]
    pub fn of_write(line: &str) -> Self {
        let words: Vec<&str> = line.split_ascii_whitespace().collect();
        match words.first() {
            Some(&"OK") => {}
            _ => return Self::refusal(line),
        }
        let seq = number(&words, "seq");
        if !words.contains(&"skipped") {
            return Self::Written { seq };
        }
        match word(&words, "reason").as_deref() {
            Some("person") => Self::Person { seq },
            Some(reason) => Self::Skipped(reason.to_string()),
            // `OK skipped`: an `if=` guard matched no row.
            None => Self::Skipped("guard".to_string()),
        }
    }

    /// Read the header of a `turn`'s answer.
    #[must_use]
    pub fn of_turn(header: &str) -> Self {
        let words: Vec<&str> = header.split_ascii_whitespace().collect();
        if words.first() != Some(&"OK") {
            return Self::refusal(header);
        }
        if words.get(2) != Some(&"turn") {
            return Self::Refused(aterm::journal_word(header));
        }
        let seq = number(&words, "seq");
        if words.get(3) == Some(&"skipped") {
            let reason = word(&words, "reason").unwrap_or_else(|| "unknown".to_string());
            let stands = number(&words, "typed").is_some_and(|keys| keys > 0)
                || number(&words, "pasted").is_some_and(|graphemes| graphemes > 0);
            return if stands || reason == "guard" {
                Self::Left(reason)
            } else if reason == "person" {
                Self::Person { seq }
            } else {
                Self::Skipped(reason)
            };
        }
        match number(&words, "submitted") {
            Some(1) => Self::Submitted,
            Some(0) if number(&words, "pressed").is_some() => Self::Unverified,
            Some(0) => Self::Left(word(&words, "status").unwrap_or_else(|| "unknown".to_string())),
            _ => Self::Refused(aterm::journal_word(header)),
        }
    }
}

/// One `text --json` read: the rows and the counts a typed write is judged at
/// (`aterm/crates/aterm-gui/src/control_query.rs`, `cmd_text_json_read`:
/// `{"rows":[…],"cursor":{…},"dims":{…},"seq":N,"gen":"E.S","title":"…","human_ms":N|null,"human_seq":N,…}`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Screen {
    /// `"rows"`: the visible rows, as `text` prints them.
    pub rows: Vec<String>,
    /// `"seq"`: the content sequence the rows were read at.
    pub seq: u64,
    /// `"human_ms"`: when a person last made a gesture; `null` is never.
    pub human: Human,
    /// `"human_seq"`: the person's gesture count, read in the same hold as the
    /// rows, so a gesture they miss moves past it.
    pub human_seq: u64,
}

impl Screen {
    /// The read `body`, the one line after `OK 1`. `None` when it is not JSON,
    /// or `rows`, `seq`, `human_ms` or `human_seq` is absent or outside its
    /// grammar: a screen this client cannot read is a screen it does not type
    /// into.
    #[must_use]
    pub fn parse(body: &str) -> Option<Self> {
        let value = crate::json::parse(body)?;
        let rows = match value.get("rows")? {
            crate::json::Value::Array(rows) => rows
                .iter()
                .map(|row| row.as_str().map(str::to_string))
                .collect::<Option<Vec<_>>>()?,
            _ => return None,
        };
        let human = match value.get("human_ms")? {
            crate::json::Value::Null => Human::Never,
            ms => Human::Ago(Duration::from_millis(ms.as_u64()?)),
        };
        Some(Self {
            rows,
            seq: value.get("seq")?.as_u64()?,
            human,
            human_seq: value.get("human_seq")?.as_u64()?,
        })
    }
}

/// The submit guard for `text` typed into Claude Code's composer on `rows`, as
/// aterm's own engine builds it (`aterm/crates/aterm-agent/src/supervise/turn_end_loop.rs`,
/// `type_guarded`): the caret Claude's reader finds on `rows`, the column it is
/// drawn at, and the chip the composer folds a pasted `text` into where it
/// folds one, else `text`, through [`composer_guard_at`]. `None` when `rows`
/// draw no composer frame for the reader, so there is no row a typed text could
/// land on.
#[must_use]
pub fn composer_guard(rows: &[String], text: &str) -> Option<String> {
    let reader = ClaudeReader;
    reader.composer(rows)?;
    let caret = reader.caret_on(rows)?;
    let drawn = reader.folded_paste(text);
    Some(composer_guard_at(
        caret,
        reader.caret_column(),
        drawn.as_deref().unwrap_or(text),
    ))
}

/// What a typed write carries beyond the holder: the claim's revision and the
/// person's count, as [`judge`] read them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Judged {
    /// `supervisor_revision=`.
    pub revision: u64,
    /// `human_seq=`.
    pub human_seq: u64,
}

/// A hold on `field`, with `value` as one journal word.
fn hold(field: &'static str, value: &str) -> Hold {
    Hold {
        field,
        value: aterm::journal_word(value),
        grace_left: None,
    }
}

/// Whether `human` leaves the session to this harness under `grace`: nobody has
/// typed, or the last gesture is at least `grace` old. A hold on `human_ms` with
/// the grace left otherwise; an unreadable `human_ms` holds with none.
fn persons_grace(human: Human, grace: Duration) -> Result<(), Hold> {
    match human {
        Human::Never => Ok(()),
        Human::Ago(ago) if ago >= grace => Ok(()),
        Human::Ago(ago) => Err(Hold {
            field: "human_ms",
            value: ago.as_millis().to_string(),
            grace_left: Some(grace.saturating_sub(ago)),
        }),
        Human::Unknown => Err(hold("human_ms", "unknown")),
    }
}

/// Judge one fresh `harness policy` and `status` pair for a write of `duty` by
/// the claim `holder`. It is the fence's two numbers only when all of these
/// read exactly so, checked in this order, and otherwise the first that does
/// not is the [`Hold`]:
///
/// 1. `duty` grants typing ([`Scope::types`]): aterm admits typed input for
///    `turn-end`, `approvals`, `questions` and `walls` only;
/// 2. `supervisor=<holder>`, `supervisor_proven=1` and `supervisor_scope`
///    naming `duty`;
/// 3. `supervisor_revision=` stated;
/// 4. `contract=` serves 3 and `effective=on`;
/// 5. `human_ms=-`, or at least the policy's `human_grace_s`;
/// 6. `human_seq=` stated.
///
/// aterm judges the first three again at the write, and the person's count
/// under the terminal lock of the write; this judgement keeps the harness from
/// sending what aterm would refuse and from typing into a person's grace,
/// which aterm's fence does not measure.
///
/// # Errors
/// The first of those facts that does not hold, as a [`Hold`] on its field;
/// a `human_ms` hold names in `grace_left` how long the grace still runs.
pub fn judge(policy: &Policy, status: &Status, holder: &str, duty: Scope) -> Result<Judged, Hold> {
    if !duty.types() {
        return Err(hold("harness-duty", duty.word()));
    }
    if status.supervisor.as_deref() != Some(holder) {
        return Err(hold(
            "supervisor",
            status.supervisor.as_deref().unwrap_or("-"),
        ));
    }
    if !status.supervisor_proven {
        return Err(hold("supervisor_proven", "0"));
    }
    if !status.takes(duty) {
        let scope = status.supervisor_scope.join(",");
        return Err(hold(
            "supervisor_scope",
            if scope.is_empty() { "-" } else { &scope },
        ));
    }
    let revision = status
        .supervisor_revision
        .ok_or_else(|| hold("supervisor_revision", "absent"))?;
    if !policy.serves_contract() {
        return Err(hold("contract", "unserved"));
    }
    if !policy.effective {
        return Err(hold("effective", "off"));
    }
    persons_grace(status.human, policy.human_grace)?;
    let human_seq = status
        .human_seq
        .ok_or_else(|| hold("human_seq", "absent"))?;
    Ok(Judged {
        revision,
        human_seq,
    })
}

/// How one typed write ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Typed {
    /// Nothing was sent: the first fact that holds the write, by [`judge`]; for
    /// a turn `composer=` (a draft is a person's), `text` (no composer frame
    /// drawn, or a screen read this client cannot parse) or `human_ms` on the
    /// screen read; or `write=unsendable`, a text or key no verb here spells
    /// ([`send_verb`], [`key_verb`], [`turn_verb`]). A `human_ms` hold names in
    /// `grace_left` how long the person's grace still runs.
    Held(Hold),
    /// A read before the write was answered with something other than its
    /// reply; nothing was sent.
    Unread(ReplyError),
    /// The write was sent, and this is aterm's answer (never [`Reply::Denied`]).
    Answered(Reply),
    /// The write was sent and aterm refused its authority
    /// (`ERR denied supervisor input authority`). `status` is the one `status`
    /// read after it (`None` where that read was answered with something other
    /// than a status). The write was not sent again.
    Denied {
        /// The session's status after the refusal.
        status: Option<Box<Status>>,
    },
}

/// The fenced typing client of one claim: the session and the holder it types
/// as. It keeps no state between writes; each write is judged afresh.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Typist {
    sid: String,
    holder: String,
}

impl Typist {
    /// The typist of harness process `pid`'s claim on session `sid`. `None`
    /// when `sid` is not a session ID or `pid` is not above 1.
    #[must_use]
    pub fn new(sid: &str, pid: u32) -> Option<Self> {
        (session_id(sid) && pid > 1).then(|| Self {
            sid: sid.to_string(),
            holder: aterm::holder(pid),
        })
    }

    /// The holder every write names.
    #[must_use]
    pub fn holder(&self) -> &str {
        &self.holder
    }

    fn status(&self, conn: &mut Connection) -> io::Result<Result<Status, ReplyError>> {
        let request = format!("@{} status", self.sid);
        Ok(Status::parse(&conn.request_line(&request)?))
    }

    /// Make one typed write of `duty` on `conn`, the connection the claim was
    /// made on, under `policy`, the policy read last: judge a fresh `status`,
    /// for a turn read the screen, send the write once and read aterm's answer.
    ///
    /// A turn's exchange is bounded by its own `timeout=` and a settle beyond
    /// it, in place of `conn`'s bound, which is set back after it.
    ///
    /// # Errors
    /// An exchange failed: the connection is poisoned, and a write that was sent
    /// may have been made. Nothing is sent again, and on that connection nothing
    /// can be.
    pub fn write(
        &self,
        conn: &mut Connection,
        policy: &Policy,
        duty: Scope,
        write: Write<'_>,
    ) -> io::Result<Typed> {
        let status = match self.status(conn)? {
            Ok(status) => status,
            Err(error) => return Ok(Typed::Unread(error)),
        };
        let judged = match judge(policy, &status, &self.holder, duty) {
            Ok(judged) => judged,
            Err(hold) => return Ok(Typed::Held(hold)),
        };
        let fence = |human_seq| TypedFence {
            holder: &self.holder,
            revision: judged.revision,
            duty,
            human_seq,
        };
        let reply = match write {
            Write::Send(text) => {
                let Some(request) = send_verb(&self.sid, &fence(judged.human_seq), text) else {
                    return Ok(Typed::Held(hold("write", "unsendable")));
                };
                Reply::of_write(&conn.request_line(&request)?)
            }
            Write::Key(key) => {
                let Some(request) = key_verb(&self.sid, &fence(judged.human_seq), key) else {
                    return Ok(Typed::Held(hold("write", "unsendable")));
                };
                Reply::of_write(&conn.request_line(&request)?)
            }
            Write::Turn(text) => {
                if !matches!(status.composer.as_deref(), Some("empty" | "placeholder")) {
                    return Ok(Typed::Held(hold(
                        "composer",
                        status.composer.as_deref().unwrap_or("absent"),
                    )));
                }
                let read = format!("@{} text --json", self.sid);
                let (header, body) = conn.request_counted(&read, 1)?;
                if header != "OK 1" {
                    return Ok(Typed::Unread(ReplyError::refused(&header)));
                }
                let Some(screen) = body.first().and_then(|body| Screen::parse(body)) else {
                    return Ok(Typed::Held(hold("text", "unreadable")));
                };
                if let Err(hold) = persons_grace(screen.human, policy.human_grace) {
                    return Ok(Typed::Held(hold));
                }
                let Some(guard) = composer_guard(&screen.rows, text) else {
                    return Ok(Typed::Held(hold("text", "no-composer")));
                };
                let Some(request) = turn_verb(&self.sid, &fence(screen.human_seq), &guard, text)
                else {
                    return Ok(Typed::Held(hold("write", "unsendable")));
                };
                let bound = conn.timeout();
                conn.set_timeout(Duration::from_millis(TURN_TIMEOUT_MS) + TURN_SLACK);
                let answered = conn.request_counted(&request, MAX_ROWS);
                conn.set_timeout(bound);
                Reply::of_turn(&answered?.0)
            }
        };
        if reply != Reply::Denied {
            return Ok(Typed::Answered(reply));
        }
        let status = self.status(conn)?.ok().map(Box::new);
        Ok(Typed::Denied { status })
    }
}

#[cfg(test)]
#[path = "typing_tests.rs"]
mod tests;
