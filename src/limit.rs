// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Fable's own usage limit: how the harness learns the child stands at it,
//! what it decides, and when the session goes back.
//!
//! **What is decided.** A session whose model is Fable and whose child was
//! refused at Fable's own limit is resumed on [`FALLBACK`], on the same account
//! and the same conversation, and resumed on its launch's own model again once
//! the limit has reset ([`decide`], [`back_at_ms`], [`back_to`]). The supervisor
//! makes both moves through its restart machinery
//! (`Supervisor::limit_look` in [`crate::supervise`]); nothing is typed into
//! the vendor's dialog. The decision reads no aterm switch: it is made the
//! same under every Autopilot word (docs/DECISIONS.md, 44).
//!
//! **How the limit is known.** Through one small interface, [`Source`], so a
//! second witness can be added beside the first. The first, and the only one
//! this build has, is the conversation's transcript ([`Transcript`]): Claude
//! Code writes a refused request as a main-thread `assistant` row that names
//! the limit's bucket and its reset. The row was read off Claude Code 2.1.323
//! (the firing of 2026-11-12 22:39 PST):
//!
//! ```text
//! {"type":"assistant","isSidechain":false,"timestamp":"2026-11-13T06:39:26.429Z",
//!  "message":{"model":"<synthetic>",…,"content":[{"type":"text",
//!    "text":"You've hit your Fable limit · resets Nov 15 at 9am (America/Los_Angeles)"}]},
//!  "apiError":"usage_limit_reached","error":"rate_limit","isApiErrorMessage":true,
//!  "apiErrorStatus":429,
//!  "quotaLimits":{"status":"rejected","resetsAt":1794762000,
//!    "rateLimitType":"seven_day_overage_included",…}}
//! ```
//!
//! The bucket word ([`FABLE_BUCKET`]) says a Fable request was refused, so no
//! other read of the session's model is needed. A subagent's refusal is a
//! sidechain row and counts for nothing: only the conversation's own thread
//! speaks for the session.
//!
//! **What stands.** The newest main-thread limit row stands until a real
//! answer (an `assistant` row whose `message.model` is a model id), a `/model`
//! choice (`Set model to …`) or a `/login` that succeeded follows it
//! ([`Standing`]): aterm's own rule (`StandingScan` in aterm's
//! `crates/aterm-agent/src/harness/upgrade.rs`, ported here because that code
//! is not vendored). Two more rows lift it here: a `/fast on` that moved the
//! model, and a main-thread request sent on a model that is not Fable, since
//! either says the session runs on another model now. Rows stamped before the state's floor ([`State::floor_ms`])
//! are spent: the floor is the session's first child's start, and then the
//! start of each child a model move started, so a limit a move has already
//! answered is never answered twice.
//!
//! **Never a loop.** A session on [`FALLBACK`] is never moved again by a
//! limit row. A return that is refused again before it answers climbs
//! [`RETURN_RUNGS_MS`], and a move that could not be made backs off
//! ([`retry_after_ms`]).
//!
//! **Beyond the process.** A session off Fable is kept beside the journal by
//! its conversation ([`Kept`]), for a new harness process on the same
//! conversation to take up.

use std::path::{Path, PathBuf};

use crate::json::{self, Value};
use crate::model_facts;
use crate::profile;

#[path = "limit_kept.rs"]
mod kept_file;
pub use kept_file::{Kept, forget, keep, kept, kept_path};

/// The `/model` alias of the one model whose own limit the harness moves a
/// session off (aterm's `turn_end::FABLE`).
pub const FABLE: &str = "fable";

/// The model a session at Fable's own limit is resumed on: Claude Opus 5.5, by
/// its full id, on the same account (aterm's `turn_end::FABLE_FALLBACK`).
pub const FALLBACK: &str = "claude-opus-5-5";

/// The `rateLimitType` of Fable's weekly bucket (aterm's `Bucket::Fable`).
pub const FABLE_BUCKET: &str = "seven_day_overage_included";

/// The cause of the harness's own restart off Fable's limit, as the journal
/// and the `model-move` line name it.
pub const CAUSE_LIMIT: &str = "fable-limit";

/// The cause of the harness's own restart back onto the launch's model.
pub const CAUSE_RESET: &str = "fable-reset";

/// How long after the reset a limit row names the session goes back
/// (the default of `--alab-reset-grace-ms`): the reset is the server's clock,
/// and a return made on the second would be refused by a clock a little behind.
pub const RESET_GRACE_MS: u64 = 60_000;

/// How long the session stays off Fable when nothing names a later time: the
/// rungs aterm keeps for a wall that names no reset (`NO_RESET_WALL_S`: half an
/// hour, two hours, six), the last for every further step.
pub const RETURN_RUNGS_MS: [u64; 3] = [30 * 60 * 1_000, 2 * 3_600 * 1_000, 6 * 3_600 * 1_000];

/// The longest a reset is believed: a weekly bucket resets within seven days,
/// so a row that names a later time is held to this.
pub const MAX_AWAY_MS: u64 = 8 * 24 * 3_600 * 1_000;

/// How long work still running at the limit holds the move off Fable (a
/// session record that reads `busy`, background work under the child), from
/// the limit row's own time. Past it the move is made over the work: the
/// conversation's own thread was refused and does nothing until the reset,
/// hours or days away, so what still runs ten minutes in is a subagent on
/// another model, a server, or a job the resumed session can start again
/// (docs/DECISIONS.md, 44).
pub const WALL_PATIENCE_MS: u64 = 10 * 60 * 1_000;

/// The pause after the first move that could not be made; each further one
/// doubles it, up to [`RETRY_MAX_MS`].
pub const RETRY_FIRST_MS: u64 = 60_000;

/// The longest pause between two attempts at a move.
pub const RETRY_MAX_MS: u64 = 30 * 60 * 1_000;

/// The longest `rateLimitType` read (aterm's `LIMIT_TYPE_MAX` is the same order).
const BUCKET_MAX: usize = 64;

/// The most bytes one look reads as an append to the rows read before; a
/// larger growth is read as a fresh tail.
const MAX_APPEND_BYTES: u64 = 4 * 1024 * 1024;

/// `/login`'s result once it succeeded (aterm's `login_succeeded`): another
/// account answers from here, and the limit read before is not its own.
const LOGIN_DONE: &str = "Login successful";

/// The suffix Claude Code puts on a model word for its million-token window
/// (`fable[1m]`, `claude-fable-5-2[1m]`).
const WINDOW_SUFFIX: &str = "[1m]";

/// `model` without a [`WINDOW_SUFFIX`] at its end, in any case.
fn without_window(model: &str) -> &str {
    match model
        .len()
        .checked_sub(WINDOW_SUFFIX.len())
        .and_then(|at| model.split_at_checked(at))
    {
        Some((base, suffix)) if suffix.eq_ignore_ascii_case(WINDOW_SUFFIX) => base,
        _ => model,
    }
}

/// Whether `model` (an alias or an id, as a `--model` names it) is Fable: the
/// alias in any case, or `claude-fable-<major>[-<minor>][-<date>]`, either
/// with a `[1m]` suffix left off (aterm's `turn_end::is_fable` and
/// `claude_model::family_version`). The suffix is the window, not another
/// model: `fable[1m]` is refused at Fable's limit like `fable`.
#[must_use]
pub fn is_fable(model: &str) -> bool {
    let base = without_window(model);
    if base.eq_ignore_ascii_case(FABLE) {
        return true;
    }
    let Some(rest) = base.strip_prefix("claude-") else {
        return false;
    };
    let mut parts = rest.split('-');
    if parts.next() != Some(FABLE) {
        return false;
    }
    // A part of six digits or more is a date; the others are the version.
    let version: Vec<&str> = parts.filter(|part| part.len() < 6).collect();
    (1..=2).contains(&version.len())
        && version
            .iter()
            .all(|part| !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit()))
}

/// Whether `display`, a model as Claude Code shows it (`Fable 5.2`; the
/// model the facts reader states, [`model_facts::ModelFacts::model`]), is
/// Fable: its first word is the alias, in any case.
#[must_use]
pub fn is_fable_display(display: &str) -> bool {
    display
        .split(' ')
        .next()
        .is_some_and(|word| word.eq_ignore_ascii_case(FABLE))
}

/// Whether a session whose model is named `model` (its launch's `--model`,
/// or the model a restart last moved it onto; `None` when nothing names one
/// and Claude Code's own settings choose) may be moved off Fable's limit: one
/// that names no model, or names Fable. A session that names another model is
/// a person's choice and is never moved (aterm's `LaunchModel::admits_fallback`).
#[must_use]
pub fn admits_fallback(model: Option<&str>) -> bool {
    model.is_none_or(is_fable)
}

/// The `--model` a resume line names to put a session back on its launch's
/// own model, `launch` being the launch's own `--model`: none where the launch
/// named one, whose own line says it, and the alias [`FABLE`] where it named
/// none. A resume line with no `--model` takes up the conversation's newest
/// model (Claude Code restores it onto a process nothing pinned,
/// docs/measured/model-effort-key-2026-11-13.md), which after a move is the one
/// the session was moved onto, [`FALLBACK`] at a limit. The alias stands in for
/// the choice Claude Code's settings made, which this harness does not read; it
/// is named on that line and on every later one until the returned child has
/// answered ([`State::back_on_launch`], `Supervisor::launch_pin`), since until
/// then the conversation's newest model is still the one it was moved off, and
/// on none after. Both the harness's own return and aterm's `restart …
/// model=launch` resume this way (`Supervisor::model_for` in
/// [`crate::supervise`]).
#[must_use]
pub fn back_to(launch: Option<&str>) -> Option<&'static str> {
    launch.is_none().then_some(FABLE)
}

/// A usage limit the child was refused at, as one transcript row states it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Wall {
    /// The row's own time (epoch ms).
    pub at_ms: u64,
    /// `quotaLimits.resetsAt` (epoch seconds), when the row names one.
    pub resets_at_s: Option<u64>,
    /// `quotaLimits.rateLimitType`, when the row's quota was rejected and
    /// names its window.
    pub bucket: Option<String>,
}

impl Wall {
    /// Whether the limit is Fable's own.
    #[must_use]
    pub fn fable(&self) -> bool {
        self.bucket.as_deref() == Some(FABLE_BUCKET)
    }

    /// Whether the reset the row names is over at `now_ms` and came after the
    /// refusal the row records: the limit lifted by itself, and nothing is
    /// gained by leaving Fable. A refusal written at or after the reset it
    /// names was made by a window that reset did not lift (the server's
    /// window rolled on, or a clock is behind), so that reset is no reset
    /// that passed: the session is moved, and goes back on the rungs
    /// ([`back_at_ms`] believes no reset that is already over).
    #[must_use]
    pub fn reset_passed(&self, now_ms: u64) -> bool {
        self.resets_at_s
            .map(|at| at.saturating_mul(1_000))
            .is_some_and(|at| self.at_ms < at && at <= now_ms)
    }

    /// The reset as the journal writes it: epoch seconds, or `-`.
    #[must_use]
    pub fn reset_word(&self) -> String {
        self.resets_at_s
            .map_or_else(|| "-".to_string(), |at| at.to_string())
    }
}

/// The limit one main-thread `assistant` row records, stamped `at_ms`: a row
/// Claude Code wrote for a refused request (`isApiErrorMessage`, `error`
/// `rate_limit`). Its window and reset are read only from a rejected
/// `quotaLimits` (aterm's `is_limit_row` and `wall_of_row`).
fn wall_of_row(row: &Value, at_ms: u64) -> Option<Wall> {
    if row.get("isApiErrorMessage").and_then(Value::as_bool) != Some(true)
        || row.get("error").and_then(Value::as_str) != Some("rate_limit")
    {
        return None;
    }
    let quota = row
        .get("quotaLimits")
        .filter(|quota| quota.get("status").and_then(Value::as_str) == Some("rejected"));
    Some(Wall {
        at_ms,
        resets_at_s: quota
            .and_then(|quota| quota.get("resetsAt"))
            .and_then(Value::as_u64)
            .filter(|at| *at > 0),
        bucket: quota
            .and_then(|quota| quota.get("rateLimitType"))
            .and_then(Value::as_str)
            .filter(|word| {
                (1..=BUCKET_MAX).contains(&word.len())
                    && word
                        .bytes()
                        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
            })
            .map(str::to_owned),
    })
}

/// The tags a `user` row's text may open with and still be words a person
/// typed: a slash command (`<command-name>`, or `<command-message>` where the
/// vendor writes that first) and a `!` shell line (`<bash-input>`). Every
/// other tag is a row the vendor writes itself (a command's output, a
/// background task's notice, a hook's words).
const TYPED_TAGS: [&str; 3] = [
    model_facts::COMMAND_NAME,
    "<command-message>",
    "<bash-input>",
];

/// Whether `row`, of type `kind`, records words typed in the composer
/// reaching the vendor: a prompt, a slash command or a `!` shell line sent
/// between turns (a `user` row whose `message.content` is plain text), or a
/// command typed while a turn ran (the `system` row of a local command,
/// [`model_facts::command_text`]). The vendor's own rows of those types are
/// not: a meta row, a compaction's summary, a row whose text opens with any
/// other tag. A prompt sent with an image (whose content is a list of
/// blocks) or one whose text opens with `<` is not counted either; it then
/// dates no input, which holds a move rather than lets one through.
fn typed_row(kind: Option<&str>, row: &Value) -> bool {
    let flag = |key: &str| row.get(key).and_then(Value::as_bool) == Some(true);
    if flag("isMeta") || flag("isCompactSummary") {
        return false;
    }
    model_facts::command_text(kind, row).is_some_and(|text| {
        let text = text.trim_start();
        !text.is_empty()
            && (!text.starts_with('<') || TYPED_TAGS.iter().any(|tag| text.starts_with(tag)))
    })
}

/// A model chosen inside the session: a `/model` result that set it, or a
/// `/fast on` result that moved it ([`Standing::line`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Choice {
    /// When (epoch ms) the result row was stamped.
    pub at_ms: u64,
    /// The model as `--model` takes it, where the rows name one: the `/model`
    /// command's own argument (`/model sonnet`), else the result's model
    /// when it is spelled as one word (`claude-fable-5-1`). `None` for a
    /// choice made in the picker or by `/fast on`, whose result names the
    /// model by its display name (`Sonnet 5`).
    pub word: Option<String>,
}

/// What the rows read so far say: the limit the child stands at, whether it
/// has answered at all, the newest moment typed words reached it, and the
/// newest model chosen inside the session.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Standing {
    wall: Option<Wall>,
    answered: bool,
    typed_ms: Option<u64>,
    chose: Option<Choice>,
    /// The argument of the newest `/model` command row not yet followed by
    /// a result.
    asked: Option<String>,
}

impl Standing {
    /// Fold one transcript line, oldest first. Only a main-thread row stamped
    /// at `floor_ms` or later counts:
    ///
    /// * a limit row ([`wall_of_row`]) is the limit the child stands at, in
    ///   place of any before it;
    /// * an `assistant` row whose `message.model` is a model id is a real
    ///   answer: no limit stands, and the child has answered;
    /// * a `/model` result that set the model, a `/fast on` result that moved
    ///   it ([`model_facts::moved_to`]), and a `/login` that succeeded, lift
    ///   the limit: a person chose another model or another account;
    /// * so does a request the conversation's own thread sends on a model
    ///   that is not Fable (an `api-request` row of the main thread, whose
    ///   `params.model` names the model): the session is on another model,
    ///   however it got there, and a turn on it is no turn the limit refused.
    ///
    /// Beside those, a row of words typed in the composer reaching the vendor
    /// ([`typed_row`]) dates the newest such moment, whatever else it says:
    /// the latest stamp of any, since a command typed mid-turn is written
    /// late, stamped with the moment it was typed. And a result that moved
    /// the model is the newest [`Choice`], naming the word of the `/model`
    /// command row just before it where that row gave one.
    ///
    /// Every other row (a subagent's, a row with no time, one cut short, the
    /// vendor's own `/rate-limit-options` result, a side query's request, a
    /// request on Fable) changes nothing.
    pub fn line(&mut self, line: &[u8], floor_ms: u64) {
        let Some(line) = std::str::from_utf8(line).ok() else {
            return;
        };
        if !line.contains("\"assistant\"")
            && !line.contains("\"user\"")
            && !line.contains(model_facts::LOCAL_COMMAND)
            && !line.contains(model_facts::MODEL_SET)
            && !line.contains(model_facts::FAST_ON_TAIL)
            && !line.contains(model_facts::API_REQUEST)
            && !line.contains(LOGIN_DONE)
        {
            return;
        }
        let Some(row) = json::parse(line) else {
            return;
        };
        if row.get("isSidechain").and_then(Value::as_bool) == Some(true) {
            return;
        }
        let Some(at_ms) = row
            .get("timestamp")
            .and_then(Value::as_str)
            .and_then(model_facts::stamp_ms)
            .filter(|at| *at >= floor_ms)
        else {
            return;
        };
        let kind = row.get("type").and_then(Value::as_str);
        if kind == Some("assistant") {
            if let Some(wall) = wall_of_row(&row, at_ms) {
                self.wall = Some(wall);
            } else if row
                .get("message")
                .and_then(|message| message.get("model"))
                .and_then(Value::as_str)
                .is_some_and(|model| !model.starts_with('<') && model_facts::is_word(model))
            {
                self.wall = None;
                self.answered = true;
            }
            return;
        }
        if kind == Some("api-request") {
            let elsewhere = row.get("querySource").and_then(Value::as_str)
                == Some(model_facts::MAIN_THREAD_QUERY)
                && row
                    .get("params")
                    .and_then(|params| params.get("model"))
                    .and_then(Value::as_str)
                    .is_some_and(|model| model_facts::is_word(model) && !is_fable(model));
            if elsewhere {
                self.wall = None;
            }
            return;
        }
        if typed_row(kind, &row) {
            self.typed_ms = self.typed_ms.max(Some(at_ms));
        }
        let Some(text) = model_facts::command_text(kind, &row).map(str::trim_start) else {
            return;
        };
        if let Some(args) = model_facts::model_command_args(text) {
            self.asked = Some(args.to_string());
            return;
        }
        let Some(body) = text.strip_prefix(model_facts::COMMAND_STDOUT) else {
            return;
        };
        // A result ends what the command row before it asked, whatever it says.
        let asked = self.asked.take();
        if let Some(display) = model_facts::moved_to(text) {
            let word = asked
                .filter(|word| model_facts::is_word(word))
                .or_else(|| display.filter(|word| model_facts::is_word(word)));
            self.chose = Some(Choice { at_ms, word });
            self.wall = None;
        } else if body.trim_start().starts_with(LOGIN_DONE) {
            self.wall = None;
        }
    }

    /// What a look reports of these rows.
    #[must_use]
    pub fn sight(&self) -> Sight {
        Sight {
            wall: self.wall.clone(),
            answered: self.answered,
            typed_ms: self.typed_ms,
            chose: self.chose.clone(),
        }
    }
}

/// What a [`Source`] sees at one look.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Sight {
    /// The usage limit the child stands at, whichever window it is of.
    pub wall: Option<Wall>,
    /// Whether the child has answered a request since the floor.
    pub answered: bool,
    /// The newest moment (epoch ms) since the floor that words typed in the
    /// composer reached the vendor as a prompt or a command, when one has
    /// ([`Standing::line`]): input read by then was taken, not left as a
    /// draft.
    pub typed_ms: Option<u64>,
    /// The newest model chosen inside the session since the floor, when one
    /// was.
    pub chose: Option<Choice>,
}

impl Sight {
    /// The limit the child stands at, when it is Fable's own.
    #[must_use]
    pub fn fable(&self) -> Option<&Wall> {
        self.wall.as_ref().filter(|wall| wall.fable())
    }
}

/// What a [`Source`] may read at one look.
#[derive(Clone, Copy, Debug)]
pub struct At<'a> {
    /// The conversation's transcript, once the supervisor's model reader has
    /// found it ([`model_facts::Reader::transcript`]).
    pub transcript: Option<&'a Path>,
    /// Rows stamped before this (epoch ms) are spent ([`State::floor_ms`]).
    pub floor_ms: u64,
}

/// A witness of the usage limit the child stands at. The supervisor asks one
/// on every look and decides on what it reports; a second witness (aterm's
/// reading of the screen, say) is another implementation of this.
pub trait Source {
    /// One look: what stands now.
    fn look(&mut self, at: &At<'_>) -> Sight;
}

/// The transcript as a [`Source`]: the rows since the floor, folded as they
/// are appended ([`Standing`]). The first read of a file takes its last
/// [`profile::TRANSCRIPT_TAIL_BYTES`]; each later one takes only the whole
/// rows appended since, so a standing limit is kept however much is written
/// after it. Another file, another floor, a file that shrank or one that grew
/// by more than [`MAX_APPEND_BYTES`] at once is read afresh. Nothing of this
/// crosses an own-image move: the new image reads the tail again.
#[derive(Debug, Default)]
pub struct Transcript {
    path: Option<PathBuf>,
    floor_ms: u64,
    /// The file's device, inode and length as the last read found them.
    seen: Option<(u64, u64, u64)>,
    /// Just past the last whole row folded.
    read_to: u64,
    standing: Standing,
}

impl Transcript {
    fn forget(&mut self) {
        self.seen = None;
        self.read_to = 0;
        self.standing = Standing::default();
    }

    /// Fold what the file at `path` holds beyond the rows already folded.
    fn read(&mut self, path: &Path) {
        use std::io::{Read as _, Seek as _, SeekFrom};
        use std::os::unix::fs::MetadataExt as _;
        let Some(stamp) = std::fs::metadata(path)
            .ok()
            .filter(std::fs::Metadata::is_file)
            .map(|meta| (meta.dev(), meta.ino(), meta.len()))
        else {
            // Gone, or no longer a regular file: nothing stands.
            return self.forget();
        };
        if self.seen == Some(stamp) {
            return;
        }
        let Some((mut file, meta)) = profile::open_transcript(path) else {
            return self.forget();
        };
        let (device, inode, len) = (meta.dev(), meta.ino(), meta.len());
        let same_file = self
            .seen
            .is_some_and(|(dev, ino, _)| dev == device && ino == inode);
        let fresh = !same_file || len < self.read_to || len - self.read_to > MAX_APPEND_BYTES;
        if fresh {
            self.standing = Standing::default();
            self.read_to = len.saturating_sub(profile::TRANSCRIPT_TAIL_BYTES);
        }
        let from = self.read_to;
        let mut bytes = Vec::new();
        if file.seek(SeekFrom::Start(from)).is_err()
            || file
                .take(len.saturating_sub(from))
                .read_to_end(&mut bytes)
                .is_err()
        {
            return self.forget();
        }
        // Only whole rows are folded; one still being written is read with
        // the look that finds its newline.
        let end = bytes
            .iter()
            .rposition(|&b| b == b'\n')
            .map_or(0, |newline| newline.saturating_add(1));
        // A fresh window that starts past the file's start begins inside a
        // row: that row is dropped.
        let start = if fresh && from > 0 {
            bytes[..end]
                .iter()
                .position(|&b| b == b'\n')
                .map_or(end, |newline| newline.saturating_add(1))
        } else {
            0
        };
        for line in bytes[start..end].split(|&b| b == b'\n') {
            self.standing.line(line, self.floor_ms);
        }
        self.read_to = from.saturating_add(end as u64);
        self.seen = Some((device, inode, len));
    }
}

impl Source for Transcript {
    fn look(&mut self, at: &At<'_>) -> Sight {
        if at.transcript != self.path.as_deref() || at.floor_ms != self.floor_ms {
            self.path = at.transcript.map(Path::to_path_buf);
            self.floor_ms = at.floor_ms;
            self.forget();
        }
        let Some(path) = self.path.clone() else {
            return Sight::default();
        };
        self.read(&path);
        self.standing.sight()
    }
}

/// Where the session is with respect to Fable's limit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    /// On the model its launch, or a restart aterm asked for, chose.
    /// `returned` says the running child was started by a return onto the
    /// launch's own model (from [`FALLBACK`], or from another model a restart
    /// had moved the session onto) and has not answered since.
    OnLaunch {
        /// Whether this is a return that has yet to answer.
        returned: bool,
    },
    /// Moved onto [`FALLBACK`] at Fable's limit, until `back_at_ms`.
    OffFable {
        /// When (epoch ms) the session goes back.
        back_at_ms: u64,
    },
}

/// What the supervisor keeps of the limit between looks. It crosses an
/// own-image move whole.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct State {
    /// Rows stamped before this (epoch ms) are spent: the first child's
    /// start, then the start of each child a model move started.
    pub floor_ms: u64,
    /// Where the session is.
    pub phase: Phase,
    /// How many returns in a row were refused at the limit before answering.
    pub walled_returns: u32,
    /// How many moves in a row could not be made.
    pub attempts: u32,
    /// No move is tried before this (epoch ms).
    pub retry_at_ms: u64,
}

impl State {
    /// The state of a session whose first child started at `floor_ms`.
    #[must_use]
    pub const fn new(floor_ms: u64) -> Self {
        Self {
            floor_ms,
            phase: Phase::OnLaunch { returned: false },
            walled_returns: 0,
            attempts: 0,
            retry_at_ms: 0,
        }
    }

    /// The child has answered a request: a return, if it was one, held, and
    /// the next limit is a new one.
    pub fn answered(&mut self) {
        if matches!(self.phase, Phase::OnLaunch { returned: true }) {
            self.phase = Phase::OnLaunch { returned: false };
            self.walled_returns = 0;
        }
    }

    /// The session was just resumed on [`FALLBACK`] at the limit `wall` (or
    /// at one no row names any more), by a child that started at `floor_ms`:
    /// when it goes back ([`back_at_ms`], with `grace_ms` after a named
    /// reset). A return that was refused before it answered counts one more
    /// walled return; any other move starts the count again.
    pub fn left_fable(
        &mut self,
        wall: Option<&Wall>,
        now_ms: u64,
        floor_ms: u64,
        grace_ms: u64,
    ) -> u64 {
        self.walled_returns = if matches!(self.phase, Phase::OnLaunch { returned: true }) {
            self.walled_returns.saturating_add(1)
        } else {
            0
        };
        let back = back_at_ms(wall, self.walled_returns, now_ms, grace_ms);
        self.phase = Phase::OffFable { back_at_ms: back };
        self.settled(floor_ms);
        back
    }

    /// The session was just resumed on a model that is not the harness's own
    /// fallback, by a child that started at `floor_ms`. Coming from
    /// [`Phase::OffFable`] it is a return, whoever asked for it.
    pub fn on_launch(&mut self, floor_ms: u64) {
        self.phase = Phase::OnLaunch {
            returned: matches!(self.phase, Phase::OffFable { .. }),
        };
        self.settled(floor_ms);
    }

    /// The session was just resumed on its launch's own model from another
    /// one a restart had moved it onto, by a child that started at
    /// `floor_ms`: a return that has yet to answer, as one from [`FALLBACK`]
    /// is, whoever asked for it. Until it answers ([`State::answered`]) the
    /// supervisor keeps naming the launch's model on every resume line
    /// (`Supervisor::launch_pin` in [`crate::supervise`]), and a limit that
    /// refuses it counts as a refused return.
    pub fn back_on_launch(&mut self, floor_ms: u64) {
        self.phase = Phase::OnLaunch { returned: true };
        self.settled(floor_ms);
    }

    /// The session was just started on another account at `now_ms`, after
    /// any model move the same start made: Fable's limit is an account's, so
    /// a time to go back read off the old account's wall says nothing of the
    /// new one's, and the returns the old account refused count no more.
    /// Off Fable, it goes back at `now_ms` or sooner, at the next point a
    /// return may be made, where the new account's Fable answers for itself:
    /// a limit there is a move of its own ([`State::left_fable`]). The time
    /// it goes back, `None` where it is not off Fable.
    pub fn account_moved(&mut self, now_ms: u64) -> Option<u64> {
        self.walled_returns = 0;
        let Phase::OffFable { back_at_ms } = self.phase else {
            return None;
        };
        let back = back_at_ms.min(now_ms);
        self.phase = Phase::OffFable { back_at_ms: back };
        Some(back)
    }

    /// A harness before this process left the session off Fable until
    /// `kept.back_at_ms` ([`Kept`]): it is off Fable here too, with the same
    /// count of refused returns. The floor stays: this process's first child
    /// started it.
    pub fn adopted(&mut self, kept: &Kept) {
        self.phase = Phase::OffFable {
            back_at_ms: kept.back_at_ms,
        };
        self.walled_returns = kept.walled_returns;
    }

    /// A person chose a model inside the session while it was off Fable: no
    /// return is owed, nothing is tried again, and the next Fable limit, if
    /// the choice was Fable, is a new one. The floor stays: nothing was
    /// started.
    pub fn given_up(&mut self) {
        self.phase = Phase::OnLaunch { returned: false };
        self.walled_returns = 0;
        self.attempts = 0;
        self.retry_at_ms = 0;
    }

    /// A move was completed: the rows before it are spent and no attempt is
    /// owed.
    const fn settled(&mut self, floor_ms: u64) {
        self.floor_ms = floor_ms;
        self.attempts = 0;
        self.retry_at_ms = 0;
    }

    /// A move could not be made at `now_ms`: the pause before the next
    /// attempt ([`retry_after_ms`]), in milliseconds.
    pub fn failed(&mut self, now_ms: u64) -> u64 {
        let pause = retry_after_ms(self.attempts);
        self.attempts = self.attempts.saturating_add(1);
        self.retry_at_ms = now_ms.saturating_add(pause);
        pause
    }
}

/// When (epoch ms) a session moved off Fable at `wall` goes back, decided at
/// `now_ms`:
///
/// * the reset the row names, plus `grace_ms`, where it names one that is
///   still ahead, held to [`MAX_AWAY_MS`] from now;
/// * otherwise [`RETURN_RUNGS_MS`] from now, by how many returns were refused
///   (`walled_returns`): half an hour, two hours, six;
/// * and once a return has been refused, never sooner than the rung of that
///   count, whatever reset is named: a reset that did not lift the limit is
///   not believed twice at the same pace.
#[must_use]
pub fn back_at_ms(wall: Option<&Wall>, walled_returns: u32, now_ms: u64, grace_ms: u64) -> u64 {
    let rung = |n: u32| {
        let at = usize::try_from(n).map_or(RETURN_RUNGS_MS.len() - 1, |n| {
            n.min(RETURN_RUNGS_MS.len() - 1)
        });
        now_ms.saturating_add(RETURN_RUNGS_MS[at])
    };
    let named = wall
        .and_then(|wall| wall.resets_at_s)
        .map(|at| at.saturating_mul(1_000))
        .filter(|at| *at > now_ms)
        .map(|at| {
            at.saturating_add(grace_ms)
                .min(now_ms.saturating_add(MAX_AWAY_MS))
        });
    match (named, walled_returns) {
        (Some(at), 0) => at,
        (Some(at), refused) => at.max(rung(refused - 1)),
        (None, refused) => rung(refused),
    }
}

/// The pause (ms) after a move that could not be made, `attempts` having
/// failed before it: [`RETRY_FIRST_MS`], doubling, up to [`RETRY_MAX_MS`].
#[must_use]
pub fn retry_after_ms(attempts: u32) -> u64 {
    // A shift only loses bits past the type's width; one this small never
    // does, and any larger is past the longest pause already.
    if attempts < 16 {
        (RETRY_FIRST_MS << attempts).min(RETRY_MAX_MS)
    } else {
        RETRY_MAX_MS
    }
}

/// What the harness does about a Fable limit the child stands at.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Decision {
    /// Resume the conversation on [`FALLBACK`].
    Move,
    /// Leave the child where it is; why, as journal words.
    Stay(String),
}

/// Decide what to do about the Fable limit `wall`, at `now_ms`, for a session
/// in `state` whose launch named the model `launch` and which now runs on
/// `current` (the model a restart last moved it onto, else `launch`; each
/// `None` where nothing names one). The first that holds:
///
/// * the session is on [`FALLBACK`] already (`on-fallback`): it is never
///   moved again by a limit row;
/// * `launch` names a model that is not Fable (`model=<word>`): a launch that
///   asked for another model is never moved, whatever it was moved onto since;
/// * `current` names a model that is not Fable (`model=<word>`);
/// * the reset the row names is over, and the row was written before it
///   (`reset-passed`, [`Wall::reset_passed`]);
/// * otherwise the session is moved.
#[must_use]
pub fn decide(
    state: &State,
    wall: &Wall,
    launch: Option<&str>,
    current: Option<&str>,
    now_ms: u64,
) -> Decision {
    if matches!(state.phase, Phase::OffFable { .. }) || current == Some(FALLBACK) {
        return Decision::Stay("on-fallback".to_string());
    }
    if let Some(other) = [launch, current]
        .into_iter()
        .flatten()
        .find(|model| !is_fable(model))
    {
        return Decision::Stay(format!("model={other}"));
    }
    if wall.reset_passed(now_ms) {
        return Decision::Stay("reset-passed".to_string());
    }
    Decision::Move
}

/// The journal's one line for a Fable limit and what was decided about it:
/// `limit-seen bucket=<type> at=<s> resets-at=<s|-> decision=move to=<model>`
/// or `… decision=stay <why>`.
#[must_use]
pub fn seen_line(wall: &Wall, decision: &Decision) -> String {
    let decided = match decision {
        Decision::Move => format!("move to={FALLBACK}"),
        Decision::Stay(why) => format!("stay {why}"),
    };
    format!(
        "limit-seen bucket={} at={} resets-at={} decision={decided}",
        wall.bucket.as_deref().unwrap_or("-"),
        wall.at_ms / 1_000,
        wall.reset_word(),
    )
}

/// Whether the harness owes the session a move off the Fable limit `sight`
/// reports, at `now_ms`, on its launch's terms ([`decide`] says
/// [`Decision::Move`] in [`Phase::OnLaunch`]), whether or not a back-off
/// still holds the move: the limit is then the move's, and the walls duty
/// ([`crate::walls`]) neither resumes it nor waits it out at the dialog.
#[must_use]
pub fn owes_move(
    state: &State,
    sight: &Sight,
    launch: Option<&str>,
    current: Option<&str>,
    now_ms: u64,
) -> bool {
    matches!(state.phase, Phase::OnLaunch { .. })
        && sight
            .fable()
            .is_some_and(|wall| decide(state, wall, launch, current, now_ms) == Decision::Move)
}

#[cfg(test)]
#[path = "limit_tests.rs"]
mod tests;
