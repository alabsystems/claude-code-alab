// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The child's model, effort level and top-effort key, as Claude Code states
//! them in its transcript.
//!
//! The rules were measured on Claude Code 2.1.325
//! (`docs/measured/model-effort-key-2026-11-13.md`):
//! 1. **The key is not a level.** Turning the top-effort key
//!    ([`TOP_EFFORT_KEY`]) on or off leaves the effort level where it was, so
//!    the key has a slot of its own.
//! 2. **A choice pins the model.** A `/model` result (`Set model to …`), a
//!    `/fast on` result that moves the model (`model set to …`) and the
//!    launch's `--model` ([`crate::profile::pins_model`]) pin it. A `/resume`
//!    keeps a pinned model. With nothing pinned Claude Code restores the
//!    conversation's model and no row says so: the model is unread until a
//!    row states it.
//! 3. **A `/resume` never carries the effort**: it is unread until a row
//!    states it.
//! 4. **A `/clear` keeps the model, the level and the key.**
//!
//! Each fact is one of three things ([`Said`]): never stated since the process
//! started, stated but unread (the newest statement is one this build cannot
//! read, or a `/resume` left the fact out), or a stated value. Nothing fills an
//! unstated or unread fact: no launch flag, no row from before the process
//! started, no default.
//!
//! The model is the newest of an answer's `message.model`, a main-thread
//! request's `params.model` and a model result. The effort is the newest of a
//! main-thread `assistant`, `user` or `system` row's `effort`, a request's
//! `params.output_config.effort`, an `/effort` result and a model result's
//! `with … effort`. The key is the newest `ultra_effort_enter` or
//! `ultra_effort_exit` attachment, or an `/effort` result that states it.
//!
//! A read is the same whatever looks it is split across: a later read takes
//! only the whole rows appended since, and a `/model` or `/effort` command row
//! waits to be read with the result Claude Code writes under it
//! ([`read_end`]). A fresh read (the first, or one after a rewrite or a growth
//! past [`MAX_APPEND_BYTES`]) takes the transcript's last
//! [`crate::profile::TRANSCRIPT_TAIL_BYTES`]; where that window neither starts
//! at the file's start nor holds a row from before the floor, it may have
//! missed a statement, so each fact it does not state is unread
//! ([`ModelFacts::leave_unread`]), never filled from before what it skipped.
//! Discovery also has a per-look budget ([`MAX_LOOKUP_STEPS`]), shared across
//! conversations. An incomplete, unreadable or ambiguous lookup leaves the three
//! facts unread; a complete lookup that finds no file states nothing new. A known
//! transcript path does not enumerate project directories again.
//!
//! This is a port of the transcript half of aterm's footer reader (`TailFacts`,
//! `TailCache::for_process` and their parsers in
//! `crates/aterm-agent/src/harness/footer.rs` and `claude_model.rs` on aterm's
//! branch `s3607/small-type`, commit `d5ba6086226`). It differs in seven ways:
//! the floor is the record's `startedAt` to the millisecond, not a second; a
//! fresh read takes the window and nothing further back (aterm reads back up to
//! 16 MiB), and a fact it may have missed is unread; a fresh read tells a
//! `/resume` from a `/clear` by any row's stamp, not only by the rows whose
//! words can matter; a command row is never read apart from its result (aterm
//! read a command row alone as unanswered); an `/effort` command whose result
//! this build cannot read leaves the key unread as well as the level; a row of
//! a kind this reader does not know states no level (aterm read a top-level
//! `effort` on any row); and any `--model` pins, alias or id.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use crate::json::{self, Value};
use crate::profile;

/// Claude Code's top-effort key word, as `/effort <key> on` takes it. It is
/// built from two parts and never written whole: the publication guards
/// refuse the word (aterm's `aterm_primer::CLAUDE_TOP_EFFORT_KEY`).
pub const TOP_EFFORT_KEY: &str = concat!("ultra", "code");

/// The least time between two lookups of a transcript that was not found, in
/// milliseconds. A `/clear` begins a conversation with no transcript until
/// its first row, and each lookup reads every project directory
/// ([`crate::profile::transcript_in`]).
pub const LOOKUP_RETRY_MS: u64 = 1_000;

/// The directory-next attempts one look may spend finding transcripts, including the read
/// that proves a scan ended. Cached transcript paths spend none. A conversation switch
/// shares this budget between its left and entered conversations.
pub const MAX_LOOKUP_STEPS: usize = 4096;

/// What the transcript since the process started says of one fact.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum Said<T> {
    /// Nothing since the process started states it.
    #[default]
    Unsaid,
    /// The newest statement is one this build cannot read, or a `/resume`
    /// left the fact out (the model of a process nothing pinned, the level of
    /// any): nothing older stands for it.
    Unread,
    /// The newest statement names it.
    Is(T),
}

impl<T> Said<T> {
    /// Whether a statement decided the fact (stated or unread): no older row
    /// can change it.
    #[must_use]
    pub const fn decided(&self) -> bool {
        !matches!(self, Self::Unsaid)
    }

    /// The value, when the newest statement names one.
    #[must_use]
    pub const fn shown(&self) -> Option<&T> {
        match self {
            Self::Is(value) => Some(value),
            Self::Unsaid | Self::Unread => None,
        }
    }
}

/// The child's model, effort level and top-effort key, each as the transcript
/// since the process started states it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ModelFacts {
    /// The model, as Claude Code shows it (`Opus 5.5`).
    pub model: Said<String>,
    /// The effort level (`high`).
    pub effort: Said<String>,
    /// The top-effort key ([`TOP_EFFORT_KEY`]): on or off.
    pub key: Said<bool>,
    /// A model switch that named no effort while the effort was open, and the
    /// model switched to: the older effort stands only if it was that model's.
    effort_for: Option<String>,
    /// The process chose its model: a `/model` result that set it (readable or
    /// not) or a `/fast on` result that moved it. Either pins it, so a
    /// `/resume` restores no conversation's model over it. Any such row says
    /// so, not only the newest.
    chose: bool,
}

impl ModelFacts {
    /// The journal's line for these facts: `facts model=<model> effort=<level>
    /// key=<on|off>`, `-` for a fact not stated or unread. The model is written
    /// as Claude Code shows it with each space as `_` (`Opus_5.5`), so every
    /// field stays one word.
    #[must_use]
    pub fn journal_line(&self) -> String {
        let model = self
            .model
            .shown()
            .map_or_else(|| "-".to_string(), |m| m.replace(' ', "_"));
        let effort = self.effort.shown().map_or("-", String::as_str);
        let key = match self.key {
            Said::Is(true) => "on",
            Said::Is(false) => "off",
            Said::Unsaid | Said::Unread => "-",
        };
        format!("facts model={model} effort={effort} key={key}")
    }

    /// These facts (newer) over `older`: each fact the newer rows decided
    /// stands, and the older facts fill the rest. A newer switch that named no
    /// effort keeps the older effort only where the older model is the one
    /// switched to.
    #[must_use]
    fn over(self, older: Self) -> Self {
        let (effort, effort_for) = match self.effort_for {
            None if self.effort.decided() => (self.effort, None),
            None => (older.effort, older.effort_for),
            Some(to) => match &older.model {
                Said::Unsaid => (
                    if self.effort.decided() {
                        self.effort
                    } else {
                        older.effort
                    },
                    Some(to),
                ),
                Said::Is(before) if *before == to => {
                    if self.effort.decided() {
                        (self.effort, None)
                    } else {
                        (older.effort, older.effort_for)
                    }
                }
                Said::Is(_) | Said::Unread => (Said::Unread, None),
            },
        };
        Self {
            model: if self.model.decided() {
                self.model
            } else {
                older.model
            },
            effort,
            effort_for,
            key: if self.key.decided() {
                self.key
            } else {
                older.key
            },
            chose: self.chose || older.chose,
        }
    }

    /// These facts as a conversation taken up (`/resume`) keeps them: the key
    /// and the choice, the model only where the process `pinned` it, and never
    /// the effort. What is not kept is unread, so nothing older fills it.
    #[must_use]
    fn taken_up(self, pinned: bool) -> Self {
        Self {
            model: if pinned { self.model } else { Said::Unread },
            effort: Said::Unread,
            effort_for: None,
            key: self.key,
            chose: self.chose,
        }
    }

    /// Each fact the newest rows left unstated, unread instead: a read that
    /// may have skipped a statement says nothing older stands for it.
    fn leave_unread(&mut self) {
        if !self.model.decided() {
            self.model = Said::Unread;
        }
        if !self.effort.decided() {
            self.decide_effort(Said::Unread);
        }
        if !self.key.decided() {
            self.key = Said::Unread;
        }
    }

    /// The model and the effort are decided: no older row can change either.
    const fn named(&self) -> bool {
        self.model.decided() && self.effort.decided() && self.effort_for.is_none()
    }

    /// No older row can change these facts: [`Self::named`], the key decided,
    /// and a choice of the model found. A process that chose none is read to
    /// its floor, as one that never stated the key is.
    const fn settled(&self) -> bool {
        self.named() && self.key.decided() && self.chose
    }

    fn meet_model(&mut self, said: Said<String>) {
        if let Some(to) = self.effort_for.take()
            && !matches!(&said, Said::Is(model) if *model == to)
        {
            self.effort = Said::Unread;
        }
        if !self.model.decided() {
            self.model = said;
        }
    }

    fn decide_effort(&mut self, said: Said<String>) {
        if matches!(said, Said::Unread) {
            self.effort_for = None;
        }
        self.effort = said;
    }

    /// A word in a level's place ([`level_said`]): the level, and the key
    /// where the word is the key itself.
    fn state_level(&mut self, word: &str, raw: bool) {
        let (level, key) = level_said(word, raw);
        self.decide_effort(level);
        self.state_key(key);
    }

    fn state_key(&mut self, on: Option<bool>) {
        if !self.key.decided()
            && let Some(on) = on
        {
            self.key = Said::Is(on);
        }
    }
}

// Claude Code's own words, each with the build it was first read on.

/// A local command's row (`<command-name>/model</command-name>…`).
const COMMAND_NAME: &str = "<command-name>";
const MODEL_COMMAND: &str = "<command-name>/model</command-name>";
const EFFORT_COMMAND: &str = "<command-name>/effort</command-name>";
/// A local command's result row.
const COMMAND_STDOUT: &str = "<local-command-stdout>";
/// The `subtype` of the `system` row a command typed mid-turn is recorded in (2.1.322).
const LOCAL_COMMAND: &str = "local_command";
/// The `type` of the row Claude Code writes as it sends a request (2.1.321).
const API_REQUEST: &str = "\"type\":\"api-request\"";
/// The `querySource` of a request on the conversation's own thread (2.1.321).
const MAIN_THREAD_QUERY: &str = "repl_main_thread";
/// What every `attachment.type` of the key's own rows starts with (2.1.322):
/// an `ultra_effort_enter` attachment with each prompt while the key is on,
/// and one `ultra_effort_exit` once it stops.
const KEY_ROW: &str = "\"ultra_effort_";
const KEY_ENTER: &str = "ultra_effort_enter";
const KEY_EXIT: &str = "ultra_effort_exit";
/// `/model`'s results (2.1.201, 2.1.283).
const MODEL_SET: &str = "Set model to ";
const MODEL_KEPT: &str = "Kept model as ";
/// `/fast on`'s result that moves the model, after a glyph (2.1.283).
const FAST_ON_MODEL_SET: &str = "Fast mode ON \u{b7} model set to ";
/// How many UTF-16 units may lead [`FAST_ON_MODEL_SET`] (the glyph and a space).
const FAST_ON_LEAD: usize = 4;
/// `/effort`'s results (2.1.283).
const EFFORT_SET: &str = "Set effort level to ";
const EFFORT_CURRENT: &str = "Current effort level: ";
const EFFORT_AUTO: &str = "Effort level set to auto";
/// The key's own results, after the key word (2.1.304).
const KEY_ON_RESULT: &str = " on (this session only): ";
const KEY_OFF_RESULT: &str = " off. Effort stays ";
const EFFORT_STAYS: &str = ". Effort stays ";
const KEY_NEEDS: &str = " needs dynamic workflows enabled";
const KEY_UNAVAILABLE: &str = " isn't available on ";
/// A level result's closing ` · <Key> on|off` (2.1.304).
const KEY_SUFFIX_ON: &str = " on";
const KEY_SUFFIX_OFF: &str = " off";

/// The effort levels `/effort` takes. The key is none of them: Claude Code
/// runs it at whatever level the session has.
#[must_use]
fn effort_level(word: &str) -> Option<&'static str> {
    match word {
        "low" => Some("low"),
        "medium" => Some("medium"),
        "high" => Some("high"),
        "xhigh" => Some("xhigh"),
        "max" => Some("max"),
        _ => None,
    }
}

/// What a word in a level's place says: the level, and the key's state where
/// the word is the key itself. The key there says the key is on and leaves the
/// level unread, never a level of its own. `raw` keeps a word that is no level
/// this build names (a row's own `effort`); without it such a word is unread
/// (a result's wording this build cannot read).
fn level_said(word: &str, raw: bool) -> (Said<String>, Option<bool>) {
    if word.eq_ignore_ascii_case(TOP_EFFORT_KEY) {
        return (Said::Unread, Some(true));
    }
    match effort_level(word) {
        Some(level) => (Said::Is(level.to_owned()), None),
        None if raw => (Said::Is(word.to_owned()), None),
        None => (Said::Unread, None),
    }
}

/// [`level_said`] of the word `text` begins with.
fn level_word(text: &str) -> (Said<String>, Option<bool>) {
    let end = text
        .find(|c: char| !c.is_ascii_alphanumeric())
        .unwrap_or(text.len());
    level_said(&text[..end], false)
}

/// A model as a result spells it, as Claude Code shows it: an id as its
/// display name, a name without its `(default)`, `(1M context)` or
/// `(recommended)`; `None` for anything else.
#[must_use]
fn model_name(display: &str) -> Option<String> {
    let mut name = display.trim();
    loop {
        let before = name;
        for suffix in [" (default)", " (1M context)", " (recommended)"] {
            if let Some(rest) = name.strip_suffix(suffix) {
                name = rest.trim_end();
            }
        }
        if name == before {
            break;
        }
    }
    if name.starts_with("claude-") && is_word(name) {
        return Some(model_display(name));
    }
    let plain = (1..=64).contains(&name.len())
        && !name.contains("  ")
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || " .-_()[]".contains(c));
    plain.then(|| name.to_owned())
}

/// A model id's display name (`claude-opus-5-5` is `Opus 5.5`); anything
/// that is no Claude id is itself.
#[must_use]
fn model_display(id: &str) -> String {
    let base = id.split('[').next().unwrap_or(id);
    let Some(rest) = base.strip_prefix("claude-") else {
        return id.to_owned();
    };
    let mut parts = rest.split('-');
    let Some(family) = parts
        .next()
        .filter(|f| f.chars().all(|c| c.is_ascii_alphabetic()))
    else {
        return id.to_owned();
    };
    let mut name = String::new();
    let mut chars = family.chars();
    if let Some(first) = chars.next() {
        name.extend(first.to_uppercase());
        name.push_str(chars.as_str());
    }
    let mut first = true;
    for part in parts.filter(|p| p.len() < 8 && p.chars().all(|c| c.is_ascii_digit())) {
        name.push(if first { ' ' } else { '.' });
        name.push_str(part);
        first = false;
    }
    name
}

fn is_word(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 64
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '.' | '_' | '[' | ']'))
}

/// `text` without ANSI SGR sequences (`\x1b[1m`, `\x1b[38;2;255;106;0m`), as
/// Claude Code's own result classifier strips them.
fn strip_sgr(text: &str) -> std::borrow::Cow<'_, str> {
    if !text.contains('\u{1b}') {
        return text.into();
    }
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find("\u{1b}[") {
        out.push_str(&rest[..at]);
        let seq = &rest[at + 2..];
        match seq.find(|c: char| !(c.is_ascii_digit() || c == ';')) {
            Some(end) if seq[end..].starts_with('m') => rest = &seq[end + 1..],
            _ => {
                out.push('\u{1b}');
                rest = &rest[at + 1..];
            }
        }
    }
    out.push_str(rest);
    out.into()
}

/// What a `/model` or `/fast` result row says of the model.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ModelResultKind {
    /// `Set model to …`: the model was chosen.
    Set,
    /// `Kept model as …`: a `/model` that changed nothing.
    Kept,
    /// `<glyph> Fast mode ON · model set to …`: `/fast on` moved the model.
    FastOn,
}

/// A model result row, read ([`model_result_of`]).
#[derive(Clone, Debug, PartialEq, Eq)]
struct ModelResult {
    kind: ModelResultKind,
    /// The model as the row spells it, quotes off (`Opus 5.5 (default)`).
    display: Option<String>,
    /// The level a `` with `<level>` effort `` clause after it names, as written.
    effort: Option<String>,
}

/// The model a result quotes (in backticks, or bold), and what follows it.
fn quoted_display(rest: &str) -> Option<(String, &str)> {
    let (display, after) = if let Some(quoted) = rest.strip_prefix('`') {
        let end = quoted.find('`')?;
        (&quoted[..end], &quoted[end + 1..])
    } else {
        let bold = rest.strip_prefix("\u{1b}[1m")?;
        let end = bold.find("\u{1b}[22m")?;
        (&bold[..end], &bold[end + "\u{1b}[22m".len()..])
    };
    Some((display.chars().take(80).collect(), after))
}

fn effort_clause(after: &str) -> Option<String> {
    let (_, rest) = after.split_once(" with `")?;
    let (level, tail) = rest.split_once('`')?;
    tail.starts_with(" effort")
        .then(|| level.chars().take(16).collect())
}

/// What a command result's `content` says of the model, when it is a model result.
fn model_result_of(content: &str) -> Option<ModelResult> {
    let body = content
        .trim_start()
        .strip_prefix(COMMAND_STDOUT)?
        .trim_start();
    for (kind, lead) in [
        (ModelResultKind::Set, MODEL_SET),
        (ModelResultKind::Kept, MODEL_KEPT),
    ] {
        if let Some(rest) = body.strip_prefix(lead) {
            let quoted = quoted_display(rest);
            return Some(ModelResult {
                kind,
                effort: quoted.as_ref().and_then(|(_, after)| effort_clause(after)),
                display: quoted.map(|(display, _)| display),
            });
        }
    }
    let plain = strip_sgr(body);
    let (lead, model) = plain.split_once(FAST_ON_MODEL_SET)?;
    if lead.encode_utf16().count() > FAST_ON_LEAD {
        return None;
    }
    Some(ModelResult {
        kind: ModelResultKind::FastOn,
        display: quoted_display(model).map(|(d, _)| d),
        effort: None,
    })
}

/// What an `/effort` result says ([`effort_result`]).
#[derive(Clone, Debug, PartialEq, Eq)]
enum EffortSaid {
    /// A level set or shown (`Set effort level to …`, `Current effort level:
    /// …`, `Effort level set to auto`), and the key's state where the result
    /// ends with ` · <Key> on|off`.
    Level {
        said: Said<String>,
        top: Option<bool>,
    },
    /// The key's own `/effort <key> on|off` result: on or off, and the level
    /// that stays.
    Top { on: bool, stays: Said<String> },
    /// The key refused (`<Key> needs dynamic workflows enabled …`, `<Key>
    /// isn't available on <model>. …`): it answers its command and decides
    /// neither the effort nor the key.
    Refused,
}

impl EffortSaid {
    fn effort(self) -> Option<Said<String>> {
        match self {
            Self::Level { said, .. } => Some(said),
            Self::Top { stays, .. } => Some(stays),
            Self::Refused => None,
        }
    }

    const fn top(&self) -> Option<bool> {
        match self {
            Self::Level { top, .. } => *top,
            Self::Top { on, .. } => Some(*on),
            Self::Refused => None,
        }
    }
}

/// `text` after a leading key word, in any case.
fn past_key(text: &str) -> Option<&str> {
    text.get(..TOP_EFFORT_KEY.len())
        .filter(|head| head.eq_ignore_ascii_case(TOP_EFFORT_KEY))
        .map(|_| &text[TOP_EFFORT_KEY.len()..])
}

/// The key's state a level result's last ` · <Key> on|off` states.
fn key_suffix(body: &str) -> Option<bool> {
    let (_, last) = body.rsplit_once(" \u{b7} ")?;
    let rest = past_key(last)?;
    let word = |w: &str| {
        rest.strip_prefix(w)
            .is_some_and(|after| !after.starts_with(|c: char| c.is_ascii_alphanumeric()))
    };
    if word(KEY_SUFFIX_ON) {
        Some(true)
    } else if word(KEY_SUFFIX_OFF) {
        Some(false)
    } else {
        None
    }
}

/// What a command result's `content` says of the effort, when it is an `/effort` result.
fn effort_result(content: &str) -> Option<EffortSaid> {
    let body = content
        .trim_start()
        .strip_prefix(COMMAND_STDOUT)?
        .trim_start();
    let body = strip_sgr(body);
    if let Some(rest) = past_key(&body) {
        if let Some(after) = rest.strip_prefix(KEY_ON_RESULT) {
            let stays = after
                .split_once(EFFORT_STAYS)
                .map_or(Said::Unread, |(_, level)| level_word(level).0);
            return Some(EffortSaid::Top { on: true, stays });
        }
        if let Some(after) = rest.strip_prefix(KEY_OFF_RESULT) {
            return Some(EffortSaid::Top {
                on: false,
                stays: level_word(after).0,
            });
        }
        if rest.starts_with(KEY_NEEDS) || rest.starts_with(KEY_UNAVAILABLE) {
            return Some(EffortSaid::Refused);
        }
    }
    let top = key_suffix(&body);
    if body.starts_with(EFFORT_AUTO) {
        return Some(EffortSaid::Level {
            said: Said::Unread,
            top,
        });
    }
    [EFFORT_SET, EFFORT_CURRENT].into_iter().find_map(|lead| {
        let (said, key) = level_word(body.strip_prefix(lead)?);
        Some(EffortSaid::Level {
            said,
            top: top.or(key),
        })
    })
}

/// Which facts a local command's result row answered ([`fold_result`]), so
/// [`scan`] can pair a command row with its own result.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Answered {
    /// A readable model was named.
    model: bool,
    /// It is an `/effort` result, a refused key included.
    effort: bool,
}

/// A local command's result row folded into `facts`, newest first. A model
/// result names the model ([`ModelFacts::meet_model`]) and any `with …
/// effort`; without that clause the earlier effort stands only for the same
/// model. One that set or moved the model is a choice; `Kept model as …` is
/// none.
fn fold_result(content: &str, facts: &mut ModelFacts) -> Answered {
    if let Some(result) = model_result_of(content) {
        facts.chose |= result.kind != ModelResultKind::Kept;
        let model = result
            .display
            .as_deref()
            .and_then(model_name)
            .map_or(Said::Unread, Said::Is);
        let readable = matches!(model, Said::Is(_));
        facts.meet_model(model.clone());
        if !facts.effort.decided() {
            match (result.effort.as_deref(), model) {
                (Some(word), _) => facts.state_level(word, false),
                (None, Said::Is(to)) => facts.effort_for = Some(to),
                (None, _) => facts.decide_effort(Said::Unread),
            }
        }
        return Answered {
            model: readable,
            effort: false,
        };
    }
    let Some(said) = effort_result(content) else {
        return Answered::default();
    };
    facts.state_key(said.top());
    if !facts.effort.decided()
        && let Some(effort) = said.effort()
    {
        facts.decide_effort(effort);
    }
    Answered {
        model: false,
        effort: true,
    }
}

/// The text of a row that records a local command or its result: a `user`
/// row's plain-string `message.content` (a command run between turns), or a
/// `system` row of subtype [`LOCAL_COMMAND`]'s own `content` (one typed while
/// a turn ran, which Claude Code 2.1.322 writes once that turn's reply lands).
fn command_text<'a>(kind: Option<&str>, row: &'a Value) -> Option<&'a str> {
    match kind {
        Some("user") => row.get("message")?.get("content")?.as_str(),
        Some("system") if row.get("subtype").and_then(Value::as_str) == Some(LOCAL_COMMAND) => {
            row.get("content")?.as_str()
        }
        _ => None,
    }
}

/// What the rows of a transcript tail say about the model, the effort and the
/// key, counting only rows stamped at `since_ms` or later (every row when
/// `None`): newest first, stopping once nothing older can change them.
#[must_use]
pub fn tail_facts(tail: &str, since_ms: Option<u64>) -> ModelFacts {
    scan(tail.as_bytes(), since_ms, false, true).0
}

/// [`tail_facts`] of the rows in `rows`, and whether the scan stopped at a row
/// from before `since_ms` (the conversation was there before: one the process
/// took up). `chose` is a choice of the model already known, which a scan
/// then does not read back to the floor for. `fresh` is a read of a tail, not
/// of rows just appended: there every stamped row counts for the floor, not
/// only one whose words can matter, so a conversation taken up whose newest
/// rows say nothing of the model (a run of queue rows) is still told from a
/// new one. An appended row is new, so there only the rows that are parsed
/// anyway are asked. A row that is not UTF-8 is skipped.
fn scan(rows: &[u8], since_ms: Option<u64>, chose: bool, fresh: bool) -> (ModelFacts, bool) {
    let mut facts = ModelFacts {
        chose,
        ..ModelFacts::default()
    };
    let mut under = Answered::default();
    for raw in rows.rsplit(|&b| b == b'\n') {
        if facts.settled() {
            break;
        }
        let Some(line) = std::str::from_utf8(raw).ok().filter(|l| !l.is_empty()) else {
            continue;
        };
        let command = line.contains(COMMAND_STDOUT) || line.contains(COMMAND_NAME);
        let model_open = !facts.model.decided() || facts.effort_for.is_some();
        let answer = line.contains("\"assistant\"");
        let wants_model = model_open && (command || answer || line.contains(API_REQUEST));
        let wants_effort = !facts.effort.decided() && (command || line.contains("\"effort\""));
        // The key's own rows and an `/effort` result state the key. A session
        // that never used the key states nothing, so an answer is read for its
        // stamp alone: the first one from before the floor ends the scan. A
        // choice of the model is looked for the same way.
        let wants_key = !facts.key.decided() && (command || answer || line.contains(KEY_ROW));
        let wants_choice = !facts.chose && (command || answer);
        let wanted = wants_model || wants_effort || wants_key || wants_choice;
        let floor_only = fresh && since_ms.is_some() && line.contains("\"timestamp\"");
        if !(wanted || floor_only) {
            continue;
        }
        // A row cut short (the tail's first, or one still being written) does
        // not parse, and changes nothing.
        let Some(row) = json::parse(line) else {
            continue;
        };
        if row.get("isSidechain").and_then(Value::as_bool) == Some(true) {
            continue;
        }
        if let Some(since) = since_ms {
            match row
                .get("timestamp")
                .and_then(Value::as_str)
                .and_then(stamp_ms)
            {
                Some(at) if at < since => return (facts, true),
                Some(_) => {}
                None => continue,
            }
        }
        if !wanted {
            continue;
        }
        let kind = row.get("type").and_then(Value::as_str);
        if command && let Some(content) = command_text(kind, &row) {
            let content = content.trim_start();
            if content.starts_with(COMMAND_STDOUT) {
                under = fold_result(content, &mut facts);
                continue;
            }
            if content.starts_with(COMMAND_NAME) {
                let answered = std::mem::take(&mut under);
                if content.starts_with(MODEL_COMMAND) && !answered.model {
                    facts.meet_model(Said::Unread);
                    if !facts.effort.decided() {
                        facts.decide_effort(Said::Unread);
                    }
                } else if content.starts_with(EFFORT_COMMAND) && !answered.effort {
                    // `/effort <key> on|off` may have been what was asked.
                    if !facts.effort.decided() {
                        facts.decide_effort(Said::Unread);
                    }
                    if !facts.key.decided() {
                        facts.key = Said::Unread;
                    }
                }
                continue;
            }
        }
        if kind == Some("attachment") {
            match row
                .get("attachment")
                .and_then(|a| a.get("type"))
                .and_then(Value::as_str)
            {
                Some(KEY_ENTER) => facts.state_key(Some(true)),
                Some(KEY_EXIT) => facts.state_key(Some(false)),
                _ => {}
            }
            continue;
        }
        // A main-thread request names the model and effort Claude Code sent it
        // with, the moment it is sent, so the newest one stands until a newer
        // row says otherwise. A side query (a title, a compaction) may run on
        // another model, so only the main thread counts.
        if kind == Some("api-request") {
            if row.get("querySource").and_then(Value::as_str) != Some(MAIN_THREAD_QUERY) {
                continue;
            }
            let params = row.get("params");
            if model_open
                && let Some(model) = params
                    .and_then(|p| p.get("model"))
                    .and_then(Value::as_str)
                    .filter(|m| is_word(m))
            {
                facts.meet_model(Said::Is(model_display(model)));
            }
            if !facts.effort.decided()
                && let Some(effort) = params
                    .and_then(|p| p.get("output_config"))
                    .and_then(|c| c.get("effort"))
                    .and_then(Value::as_str)
                    .filter(|e| is_word(e))
            {
                facts.state_level(effort, true);
            }
            continue;
        }
        if model_open
            && kind == Some("assistant")
            && let Some(model) = row
                .get("message")
                .and_then(|m| m.get("model"))
                .and_then(Value::as_str)
                .filter(|m| !m.starts_with('<') && is_word(m))
        {
            facts.meet_model(Said::Is(model_display(model)));
        }
        // Only the conversation's own rows state the level: a row of a kind
        // this reader does not know (a `progress` row of a subagent, say)
        // changes nothing.
        if !facts.effort.decided()
            && matches!(kind, Some("assistant" | "user" | "system"))
            && let Some(effort) = row
                .get("effort")
                .and_then(Value::as_str)
                .filter(|e| is_word(e))
        {
            facts.state_level(effort, true);
        }
    }
    (facts, false)
}

/// Epoch milliseconds of an RFC 3339 UTC stamp as Claude Code writes a row's
/// `timestamp` (`2026-11-13T04:18:02.796Z`): `Z` only, with an optional
/// fraction of 1 to 9 digits, cut to the millisecond. `None` for any other
/// text and for a time before 1970.
#[must_use]
fn stamp_ms(text: &str) -> Option<u64> {
    let (&last, head) = text.as_bytes().split_last()?;
    let (stamp, fraction) = head.split_first_chunk::<19>()?;
    if last != b'Z' {
        return None;
    }
    for (at, separator) in [(4, b'-'), (7, b'-'), (10, b'T'), (13, b':'), (16, b':')] {
        if stamp.get(at) != Some(&separator) {
            return None;
        }
    }
    let field = |from: usize, to: usize| stamp.get(from..to)?.iter().try_fold(0, push_digit);
    let (year, month, day) = (field(0, 4)?, field(5, 7)?, field(8, 10)?);
    let (hour, minute, second) = (field(11, 13)?, field(14, 16)?, field(17, 19)?);
    if !(1..=12).contains(&month)
        || !(1..=31).contains(&day)
        || hour > 23
        || minute > 59
        || second > 60
    {
        return None;
    }
    let millis = match fraction {
        [] => 0,
        [b'.', digits @ ..]
            if (1..=9).contains(&digits.len()) && digits.iter().all(u8::is_ascii_digit) =>
        {
            digits
                .iter()
                .chain(std::iter::repeat(&b'0'))
                .take(3)
                .try_fold(0, push_digit)?
        }
        _ => return None,
    };
    days_from_civil(year, month, day)?
        .checked_mul(86_400)?
        .checked_add(u64::from(hour) * 3600 + u64::from(minute) * 60 + u64::from(second))?
        .checked_mul(1000)?
        .checked_add(u64::from(millis))
}

/// `value` with the ASCII digit `digit` written after it; `None` for a byte
/// that is no digit, or past `u16::MAX` (a stamp's fields have at most four
/// digits).
fn push_digit(value: u16, &digit: &u8) -> Option<u16> {
    match digit {
        b'0'..=b'9' => value.checked_mul(10)?.checked_add(u16::from(digit - b'0')),
        _ => None,
    }
}

/// Days from 1970-01-01 to a civil date (Howard Hinnant's `days_from_civil`);
/// `None` before 1970 and for a day 0.
fn days_from_civil(year: u16, month: u16, day: u16) -> Option<u64> {
    let (year, month, day) = (u64::from(year), u64::from(month), u64::from(day));
    // Year 0 is before 1970 in any month, and day 0 is no day.
    if year == 0 || day == 0 {
        return None;
    }
    let year = if month <= 2 { year - 1 } else { year };
    let era = year / 400;
    let year_of_era = year % 400;
    let day_of_year = (153 * ((month + 9) % 12) + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    (era * 146_097 + day_of_era).checked_sub(719_468)
}

/// What the supervisor knows of its child at one look ([`Reader::look`]).
#[derive(Clone, Copy)]
pub struct Process<'a> {
    /// The child's pid.
    pub pid: u32,
    /// Its own session record's conversation and `startedAt` (epoch ms), once
    /// it has written one. The `startedAt` is the floor: only rows stamped at
    /// or after it are the process's statements.
    pub record: Option<(&'a str, u64)>,
    /// Its command line's arguments, `argv[0]` left out. Whether they name
    /// `--model` ([`pins_model`]) is asked only when the record names another
    /// conversation, so a look parses no command line.
    pub args: &'a [OsString],
}

/// Whether the arguments `args` (`argv[0]` left out) name `--model`
/// ([`crate::profile::pins_model`]); arguments that are not all UTF-8 pin
/// nothing.
fn pins_model(args: &[OsString]) -> bool {
    crate::supervise::launch_argv(args).is_some_and(|argv| profile::pins_model(&argv))
}

/// The most bytes one look reads as an append to the rows read before; a
/// larger growth is read as a fresh tail, and each fact that tail does not
/// state is unread, since what it skips may have stated it.
pub const MAX_APPEND_BYTES: u64 = 4 * 1024 * 1024;

/// How many bytes before the end of the last read must be unchanged for an
/// append to be read on top of it.
const GUARD_BYTES: usize = 64;

/// One transcript's file as a read found it: re-read only when one of these
/// changed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Stamp {
    device: u64,
    inode: u64,
    len: u64,
    modified: (i64, i64),
    changed: (i64, i64),
}

impl Stamp {
    /// The stamp of the regular file at `path` (a link followed, as
    /// [`crate::profile::transcript_in`] follows it).
    fn of(path: &Path) -> Option<Self> {
        let meta = std::fs::metadata(path).ok()?;
        meta.is_file().then(|| Self::from(&meta))
    }
}

impl From<&std::fs::Metadata> for Stamp {
    fn from(meta: &std::fs::Metadata) -> Self {
        use std::os::unix::fs::MetadataExt as _;
        Self {
            device: meta.dev(),
            inode: meta.ino(),
            len: meta.len(),
            modified: (meta.mtime(), meta.mtime_nsec()),
            changed: (meta.ctime(), meta.ctime_nsec()),
        }
    }
}

/// Where the last read of a transcript ended, so the next reads only what was
/// appended since.
#[derive(Clone, Copy, Debug)]
struct Carry {
    device: u64,
    inode: u64,
    /// Just past the last whole row read.
    read_to: u64,
    /// The [`GUARD_BYTES`] (fewer at the file's start) before `read_to`.
    guard: [u8; GUARD_BYTES],
    guard_len: usize,
}

impl Carry {
    /// The carry of a read of `meta`'s file whose bytes `read` end at `read_to`.
    fn at(meta: &std::fs::Metadata, read_to: u64, read: &[u8]) -> Self {
        use std::os::unix::fs::MetadataExt as _;
        let guard_len = read.len().min(GUARD_BYTES);
        let mut guard = [0; GUARD_BYTES];
        guard[..guard_len].copy_from_slice(&read[read.len() - guard_len..]);
        Self {
            device: meta.dev(),
            inode: meta.ino(),
            read_to,
            guard,
            guard_len,
        }
    }
}

/// Bytes `[from, to)` of `file` (fewer where it ends sooner).
fn read_range(file: &mut std::fs::File, from: u64, to: u64) -> Option<Vec<u8>> {
    use std::io::{Read as _, Seek as _, SeekFrom};
    file.seek(SeekFrom::Start(from)).ok()?;
    let mut bytes = Vec::new();
    file.take(to.saturating_sub(from))
        .read_to_end(&mut bytes)
        .ok()?;
    Some(bytes)
}

/// The length of `bytes` up to and with its last newline: its whole rows.
fn whole_rows(bytes: &[u8]) -> usize {
    bytes
        .iter()
        .rposition(|&b| b == b'\n')
        .map_or(0, |nl| nl.saturating_add(1))
}

/// How much of the whole rows `rows` a read takes now: all of them, unless the
/// last is a `/model` or `/effort` command row. Claude Code writes its result
/// as the row under it, and the two are read together: a look that fell
/// between them would read the command as unanswered (its facts unread), and
/// the result read alone by the next look could not undo that. Once any row is
/// under the command it is read, answered or not.
fn read_end(rows: &[u8]) -> usize {
    let Some(body) = rows.strip_suffix(b"\n") else {
        return rows.len();
    };
    let last = whole_rows(body);
    if body.get(last..).is_some_and(awaits_result) {
        last
    } else {
        rows.len()
    }
}

/// Whether `line` is a main-thread `/model` or `/effort` command row (one
/// [`scan`] reads as unanswered when no result is under it).
fn awaits_result(line: &[u8]) -> bool {
    let Some(line) = std::str::from_utf8(line)
        .ok()
        .filter(|l| l.contains(COMMAND_NAME))
    else {
        return false;
    };
    let Some(row) = json::parse(line) else {
        return false;
    };
    row.get("isSidechain").and_then(Value::as_bool) != Some(true)
        && command_text(row.get("type").and_then(Value::as_str), &row)
            .map(str::trim_start)
            .is_some_and(|text| text.starts_with(MODEL_COMMAND) || text.starts_with(EFFORT_COMMAND))
}

/// One conversation the process was read in.
#[derive(Clone, Debug, Default)]
struct SessionRead {
    id: String,
    /// The floor under the rows that speak for the process here (epoch ms):
    /// its own start, or for a conversation it switched into, when it was
    /// last seen in the one it left.
    floor_ms: u64,
    /// When the process was last seen in this conversation (epoch ms).
    seen_ms: u64,
    /// The transcript, once found.
    path: Option<PathBuf>,
    /// No lookup before this (epoch ms), after one that found nothing.
    lookup_after_ms: u64,
    /// The file as the last read found it.
    stamp: Option<Stamp>,
    /// Where the last read ended.
    carry: Option<Carry>,
    #[cfg(test)]
    lookups: usize,
    #[cfg(test)]
    reads: usize,
    #[cfg(test)]
    appends: usize,
}

impl SessionRead {
    fn new(id: &str, floor_ms: u64) -> Self {
        Self {
            id: id.to_string(),
            floor_ms,
            ..Self::default()
        }
    }

    /// What the rows since this conversation's floor that this read is the
    /// first to see say (the rows appended since the last read, or a fresh
    /// tail), and whether they met a row from before the floor; `None` when
    /// the transcript is not found or has not changed since the last read.
    fn read(
        &mut self,
        config: &Path,
        chose: bool,
        now_ms: u64,
        lookup_steps: &mut usize,
    ) -> Option<(ModelFacts, bool)> {
        if self.path.is_none() {
            if now_ms < self.lookup_after_ms {
                return None;
            }
            #[cfg(test)]
            {
                self.lookups += 1;
            }
            match profile::transcript_in_bounded(config, &self.id, lookup_steps) {
                Ok(path) => self.path = path,
                Err(_) => {
                    self.lookup_after_ms = now_ms.saturating_add(LOOKUP_RETRY_MS);
                    return Some((
                        ModelFacts {
                            model: Said::Unread,
                            effort: Said::Unread,
                            key: Said::Unread,
                            ..ModelFacts::default()
                        },
                        false,
                    ));
                }
            }
            if self.path.is_none() {
                self.lookup_after_ms = now_ms.saturating_add(LOOKUP_RETRY_MS);
                return None;
            }
        }
        let path = self.path.as_deref()?;
        let Some(stamp) = Stamp::of(path) else {
            // Gone, or no longer a regular file: looked up again.
            self.path = None;
            self.stamp = None;
            self.carry = None;
            return None;
        };
        if self.stamp == Some(stamp) {
            return None;
        }
        let (mut file, meta) = profile::open_transcript(path)?;
        let read = match self.read_append(&mut file, &meta, chose) {
            Some(read) => read,
            None => self.read_fresh(&mut file, &meta, chose)?,
        };
        self.stamp = Some(Stamp::from(&meta));
        #[cfg(test)]
        {
            self.reads += 1;
        }
        Some(read)
    }

    /// The whole rows appended since the carried read (up to [`read_end`]),
    /// or `None` where this is not that file grown by at most
    /// [`MAX_APPEND_BYTES`] with the bytes before the carried end unchanged
    /// (it is then read afresh).
    fn read_append(
        &mut self,
        file: &mut std::fs::File,
        meta: &std::fs::Metadata,
        chose: bool,
    ) -> Option<(ModelFacts, bool)> {
        use std::os::unix::fs::MetadataExt as _;
        let carry = self.carry?;
        let len = meta.len();
        if carry.device != meta.dev()
            || carry.inode != meta.ino()
            || len <= carry.read_to
            || len - carry.read_to > MAX_APPEND_BYTES
        {
            return None;
        }
        let guard = carry.guard_len as u64;
        let bytes = read_range(file, carry.read_to.checked_sub(guard)?, len)?;
        if bytes.get(..carry.guard_len)? != &carry.guard[..carry.guard_len] {
            return None;
        }
        let rows = &bytes[carry.guard_len..];
        let end = carry.guard_len + read_end(&rows[..whole_rows(rows)]);
        self.carry = Some(Carry::at(
            meta,
            carry.read_to + (end - carry.guard_len) as u64,
            &bytes[..end],
        ));
        #[cfg(test)]
        {
            self.appends += 1;
        }
        Some(scan(
            &bytes[carry.guard_len..end],
            Some(self.floor_ms),
            chose,
            false,
        ))
    }

    /// The whole rows of the file's last
    /// [`crate::profile::TRANSCRIPT_TAIL_BYTES`] (up to [`read_end`]); a first
    /// row the window cuts is dropped. A window that starts past the file's
    /// start and meets no row from before the floor may have cut off a
    /// statement since the floor: each fact it does not state is unread.
    fn read_fresh(
        &mut self,
        file: &mut std::fs::File,
        meta: &std::fs::Metadata,
        chose: bool,
    ) -> Option<(ModelFacts, bool)> {
        let len = meta.len();
        let start = len.saturating_sub(profile::TRANSCRIPT_TAIL_BYTES);
        let bytes = read_range(file, start, len)?;
        let end = read_end(&bytes[..whole_rows(&bytes)]);
        let from = if start == 0 {
            0
        } else {
            bytes[..end]
                .iter()
                .position(|&b| b == b'\n')
                .map_or(end, |nl| nl.saturating_add(1))
        };
        // A window with no row end in it starts mid-row: the next read is fresh too.
        let read_to = start.saturating_add(end as u64);
        self.carry = (start == 0 || end > 0).then(|| Carry::at(meta, read_to, &bytes[..end]));
        let (mut facts, floored) = scan(&bytes[from..end], Some(self.floor_ms), chose, true);
        if start > 0 && !floored {
            facts.leave_unread();
        }
        Some((facts, floored))
    }
}

/// What one child process has stated of its model, effort and key, across
/// the conversations it was read in. An own-image move carries it in the
/// capsule ([`Reader::put`], [`Reader::get`]): only this reader saw which
/// conversations the child left and when it entered the one it is in.
#[derive(Clone, Debug)]
pub struct Reader {
    /// The process these facts are of.
    pid: Option<u32>,
    /// Its record's `startedAt` (epoch ms), once read.
    floor_ms: Option<u64>,
    /// The conversation it was last read in.
    session: Option<SessionRead>,
    facts: ModelFacts,
    /// [`ModelFacts::journal_line`] as [`Reader::look`] last returned it.
    line: String,
    /// How many times [`pins_model`] was asked.
    #[cfg(test)]
    pin_asks: usize,
}

impl Default for Reader {
    fn default() -> Self {
        Self {
            pid: None,
            floor_ms: None,
            session: None,
            facts: ModelFacts::default(),
            line: ModelFacts::default().journal_line(),
            #[cfg(test)]
            pin_asks: 0,
        }
    }
}

impl Reader {
    /// The facts as the last look left them.
    #[must_use]
    pub const fn facts(&self) -> &ModelFacts {
        &self.facts
    }

    /// The child these facts are of, once a look has seen it.
    #[must_use]
    pub const fn pid(&self) -> Option<u32> {
        self.pid
    }

    /// Follow `process` one look further, at `now_ms`; the facts' journal line
    /// when it changed. Claude Code's config directory is `config`.
    ///
    /// The transcript is read again only when its length, modification time,
    /// change time or identity moved, and then only the rows appended since
    /// the last read where it grew. Another process (a pid, or a record
    /// `startedAt`, other than the last) starts from nothing. A record that
    /// names another conversation is a `/clear` (a new conversation: every
    /// fact is kept) or a `/resume` (one with rows from before the switch:
    /// the key and a pinned model are kept, the rest is unread); first the
    /// conversation left is read once more, so a statement made there since
    /// the last look counts. A first read that cannot tell the two apart (its
    /// window reaches neither the file's start nor a row from before the
    /// switch) leaves each fact it does not state unread.
    /// Discovery consumes at most [`MAX_LOOKUP_STEPS`] directory-next attempts,
    /// including the attempt that proves a scan ended. A refused lookup marks
    /// every fact unread and keeps the existing retry interval.
    pub fn look(&mut self, config: &Path, process: &Process<'_>, now_ms: u64) -> Option<&str> {
        self.look_with_budget(config, process, now_ms, MAX_LOOKUP_STEPS)
    }

    fn look_with_budget(
        &mut self,
        config: &Path,
        process: &Process<'_>,
        now_ms: u64,
        mut lookup_steps: usize,
    ) -> Option<&str> {
        let mut moved = false;
        let started = process.record.map(|(_, started)| started);
        if self.pid != Some(process.pid)
            || started.is_some_and(|s| self.floor_ms.is_some_and(|f| f != s))
        {
            let line = std::mem::take(&mut self.line);
            *self = Self {
                pid: Some(process.pid),
                line,
                ..Self::default()
            };
            moved = true;
        }
        if let Some(record) = process.record {
            self.floor_ms = Some(record.1);
            moved |= self.follow(config, record, process.args, now_ms, &mut lookup_steps);
        }
        if !moved {
            return None;
        }
        let line = self.facts.journal_line();
        if line == self.line {
            return None;
        }
        self.line = line;
        Some(&self.line)
    }

    /// One look at the conversation `id` the record names; whether the facts
    /// were folded again.
    fn follow(
        &mut self,
        config: &Path,
        (id, started): (&str, u64),
        args: &[OsString],
        now_ms: u64,
        lookup_steps: &mut usize,
    ) -> bool {
        let mut session = match self.session.take() {
            Some(same) if same.id == id => same,
            Some(left) => {
                self.switch(config, left, (id, started), args, now_ms, lookup_steps);
                return true;
            }
            None => SessionRead::new(id, started),
        };
        let read = session.read(config, self.facts.chose, now_ms, lookup_steps);
        session.seen_ms = now_ms;
        self.session = Some(session);
        let Some((tail, _)) = read else {
            return false;
        };
        self.facts = tail.over(std::mem::take(&mut self.facts));
        true
    }

    /// The process moved from the conversation `left` into `id`.
    fn switch(
        &mut self,
        config: &Path,
        mut left: SessionRead,
        (id, started): (&str, u64),
        args: &[OsString],
        now_ms: u64,
        lookup_steps: &mut usize,
    ) {
        let before = std::mem::take(&mut self.facts);
        let carried = match left.read(config, before.chose, now_ms, lookup_steps) {
            Some((said, _)) => said.over(before),
            None => before,
        };
        let mut entered = SessionRead::new(id, left.seen_ms.max(started));
        let (tail, resumed) = entered
            .read(config, carried.chose, now_ms, lookup_steps)
            .unwrap_or_default();
        self.facts = tail.over(if resumed {
            let pinned = carried.chose || {
                #[cfg(test)]
                {
                    self.pin_asks += 1;
                }
                pins_model(args)
            };
            carried.taken_up(pinned)
        } else {
            carried
        });
        entered.seen_ms = now_ms;
        self.session = Some(entered);
    }
}

#[path = "model_facts_transfer.rs"]
mod transfer;

#[cfg(test)]
#[path = "model_facts_tests.rs"]
mod tests;
