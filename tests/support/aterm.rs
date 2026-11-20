// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! A stand-in for aterm's control socket, and the proof that it and this crate's
//! client agree.
//!
//! [`FakeAterm`] lays what a harness inside an aterm tab finds: the rendezvous
//! directory's `graph/<sid>` entry, a socket in a directory short enough for a
//! Unix socket path, and the token beside it. It answers the five requests a
//! contract-3 client makes (`harness policy`, `native-act info`, `meta set
//! supervisor`, `meta unset supervisor`, `status`) from state a scenario changes
//! as it goes, and streams `subscribe @<sid> events` with aterm's numeric tags
//! (`sub 1 <sid>`, `EVENT 1 …`), which this crate's strict reader requires.
//!
//! It also serves the typed writes a claim holder makes (`send`, `key` and
//! `turn`) and the screen read a turn is built from (`text --json`). A write is
//! judged as aterm judges it: its leading words as `take_leading_options` reads
//! them (`ERR usage: …` for a fence given in part), then its authority as
//! `SupervisorInputFence::authorize` does (the holder, the revision, a typing
//! duty the standing contract-3 claim takes, and the connection that made the
//! claim; `ERR denied supervisor input authority` otherwise, and for a write
//! without the fence from the claim's own connection), then the person's count
//! (`if-human=`; aterm's skip reply), the screen's generation (`if-gen=`, which
//! every write that types moves; `OK skipped reason=changed`) and a row guard
//! (`if=`, on any visible row; `OK skipped`). What it typed is recorded
//! ([`FakeAterm::typed`], with a key's `act=`) and written into its screen at
//! the cursor, and a turn's `submit=guarded:<re>` is judged on the cursor's row
//! after the text; both guards by a matcher for the pattern syntax
//! `alab_policy::guard` writes; any other syntax is `ERR badregex`.
//! [`FakeAterm::before_next_write`] lands one change between a client's read
//! and its write: a person's key, the claim made again, or another screen;
//! [`FakeAterm::after_key`] shows another screen once a key is pressed.
//!
//! It proves nothing itself. A claim is stored when the settable claim reply is
//! `OK` and no other holder stands, and `status` shows it proven while the
//! settable proven bit is set. As in aterm, a claim is bound to the connection
//! that made it and is dropped when that connection closes, and a halt
//! (`status hold=1`) reads in the policy as `effective=off why=hold`, with no
//! generation move and no event (`harness_policy_verb.rs`, `effective`).
//!
//! The wire text is aterm's at `d29d3887c4d`: the policy line of
//! `crates/aterm-gui/src/harness_policy_verb.rs`, the claim replies of
//! `crates/aterm-gui/src/control_session.rs` (`cmd_meta_supervisor_serving`,
//! `claim_reply`), the status words of `crates/aterm-gui/src/session_status.rs`
//! (`native_supervisor_fields`) and `session_timeline.rs` (`wire_fields`), the
//! info line of `crates/aterm-gui/src/native_act.rs` (`Request::Info`), the
//! subscription ACK of `crates/aterm-gui/src/control.rs`, and the restart event
//! of `crates/aterm-gui/src/restart_act.rs` (`record_restart`). The typed writes'
//! text is aterm's at `e7369dce423`: the leading options and replies of
//! `crates/aterm-gui/src/control_input.rs` (`take_leading_options`,
//! `SUPERVISOR_INPUT_USAGE`, `guarded_input_reply`) with `control.rs`'s
//! `stamp_input_seq`, the authority rule of `session_timeline.rs`
//! (`SupervisorInputFence::authorize`, `SCOPED_INPUT_DENIED`,
//! `scoped_untyped_input_refusal`), the turn's options of
//! `crates/aterm-types/src/control_verbs.rs` (`TURN_OPTIONS`) and its replies
//! of `control_session.rs` (`cmd_turn`, `fenced_submit`), and the `text --json`
//! body of `control_query.rs` (`cmd_text_json_read`).
//!
//! The self-tests at the end drive the fake through `Locator`, `Connection` and
//! `EventReader` with no harness process, so a later scenario that fails is the
//! harness's failure and not a disagreement between the fake and the client.

use std::io::{BufRead as _, BufReader, Read as _, Write as _};
use std::net::Shutdown;
use std::os::fd::AsRawFd as _;
use std::os::unix::fs::DirBuilderExt as _;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use alab_policy::guard::KeyAct;
use claude_code_alab::aterm::{
    self, Hold, Human, Policy, ReplyError, SafePoint, Scope, Status, judge_safe_point,
};
use claude_code_alab::aterm_wire::{Connection, EventReader, EventUpdate, Locator};
use claude_code_alab::typing::{Press, Reply, Typed, Typist, Write};

/// The token every connection must present.
const TOKEN: &str = "fake-aterm-token";
/// How long a self-test waits for the fake's threads before it fails.
const HANG: Duration = Duration::from_secs(20);
/// One request's deadline in the self-tests.
const TIMEOUT: Duration = Duration::from_secs(5);

/// A policy body (everything after `OK gen=<n> `) with contract 3 served, the
/// policy in effect and every switch a move or a crash relaunch needs on. The
/// unrelated keys are a sample of the dozens aterm sends.
pub const POLICY_ON: &str = "effective=on why=enabled contract=2,3 enabled=true autopilot=cruise \
    relaunch=true stall_term=true upgrade=true upgrade_ask=false human_grace_s=120 \
    questions=recommended";

/// What `native-act info` answers once aterm's roster carries this harness.
pub const INFO_ROSTERED: &str = "OK native-act state=info epoch=d1a48b482e4c6b391c4715137ce246b6 \
    native-act=1 handoff=blocked clock=monotonic-us \
    own-image=grok-alab:upgrade,muse-alab:upgrade,kimi-alab:upgrade,claude-code-alab:upgrade";

/// A roster response carrying both C3 child capabilities and a synthetic epoch.
pub const INFO_CURRENT: &str = "OK native-act state=info epoch=d1a48b482e4c6b391c4715137ce246b6 \
    native-act=1 handoff=close clock=monotonic-us child-proof=1 child-start=1 \
    own-image=grok-alab:upgrade,muse-alab:upgrade,kimi-alab:upgrade,claude-code-alab:upgrade";

/// The response from an aterm build whose Info parser predates capabilities.
const INFO_CAPABILITIES_UNSUPPORTED: &str =
    "ERR usage: native-act <prepare|precommit|commit|query|signal|resume> <bounded fields>";

/// What `native-act info` answers today (aterm `d29d3887c4d`, read from a
/// running aterm on 2026-11-12): no `claude-code-alab` row. A new fake starts
/// here, so a scenario must opt in to the roster, as a real launch cannot.
pub const INFO_UNROSTERED: &str = "OK native-act state=info epoch=d1a48b482e4c6b391c4715137ce246b6 \
    native-act=1 handoff=blocked clock=monotonic-us \
    own-image=grok-alab:upgrade,muse-alab:upgrade,kimi-alab:upgrade";

/// The `status` fields of a quiet Claude Code session nobody is typing into, as
/// aterm's Claude reader reports them. The claim's own fields are added from
/// the fake's claim state.
pub const QUIET_STATUS: &[(&str, &str)] = &[
    ("hold", "0"),
    ("hand", "-"),
    ("program", "claude"),
    ("agent", "idle"),
    ("composer", "empty"),
    ("input", "clear"),
    ("human_ms", "-"),
    ("need", "-"),
];

/// aterm's answer to a typed write whose fence does not name the standing
/// claim (`session_timeline.rs`, `SCOPED_INPUT_DENIED`).
pub const DENIED: &str = "ERR denied supervisor input authority";

/// aterm's answer to a typed write that names its fence in part, or a part
/// twice (`control_input.rs`, `SUPERVISOR_INPUT_USAGE`).
pub const SUPERVISOR_INPUT_USAGE: &str = "ERR usage: typed supervisor input takes \
    if-supervisor=<holder> if-supervisor-revision=<n> harness-duty=<scope> together, once";

/// The options a `turn` reads after its leading words (`control_verbs.rs`,
/// `TURN_OPTIONS`).
const TURN_OPTIONS: [&str; 16] = [
    "idle",
    "timeout",
    "submit",
    "submit_window",
    "presses",
    "submit_verify",
    "settle",
    "trim",
    "typed",
    "cadence",
    "cap",
    "yield",
    "if-gen",
    "if-rows",
    "if-human",
    "cause",
];

/// A screen: its rows, as `text` prints them, and its cursor.
pub type Shown = (Vec<String>, (usize, usize));

/// The rows of an idle Claude Code whose composer is empty, as `text` prints
/// them (aterm-phase's `claude-2.1.302-idle-compacted` fixture, cut), and the
/// cursor after the composer's caret. A new fake shows this screen.
pub fn composer_screen() -> (Vec<String>, (usize, usize)) {
    let rule = "─".repeat(90);
    let rows = [
        "⏺ Done.",
        "",
        &rule,
        "❯",
        &rule,
        "  ⏵⏵ bypass permissions on (shift+tab to cycle)",
    ]
    .map(str::to_string)
    .to_vec();
    (rows, (3, 2))
}

/// The rows of a Claude Code 2.1.316 permission box (aterm-phase's
/// `claude-2.1.316-box-rm-auto-block-bypass` fixture, cut), which draws no
/// composer, and the cursor on its focused option.
pub fn box_screen() -> (Vec<String>, (usize, usize)) {
    let rows = [
        " Do you want to proceed?",
        " ❯ 1. Yes",
        "   2. No",
        "",
        " Esc to cancel · Tab to amend",
    ]
    .map(str::to_string)
    .to_vec();
    (rows, (1, 3))
}

/// One change [`FakeAterm::before_next_write`] lands when the next typed write
/// arrives, before aterm would judge it.
#[derive(Clone, Debug)]
pub enum NextWrite {
    /// A person makes a gesture: `human_seq` moves and `human_ms` starts again.
    PersonKeys,
    /// The claim is made again by its holder (as after a relaunch's new
    /// `vendor=`): it stands, at the next revision.
    ClaimMoves,
    /// The screen becomes these rows, with the cursor at this row and column.
    Screen(Vec<String>, (usize, usize)),
}

/// A stored claim.
#[derive(Clone, Debug)]
struct Claim {
    holder: String,
    /// The connection it is bound to.
    connection: u64,
    /// Its `contract=` word, when it named one.
    contract: Option<String>,
    /// Its `scope=` words; every duty for a claim that named none.
    scope: String,
    version: Option<String>,
    vendor: Option<String>,
    child: Option<String>,
    child_generation: Option<u64>,
}

#[derive(Debug)]
enum ReplyEffect {
    Rename {
        source: PathBuf,
        target: PathBuf,
        receipt: Option<PathBuf>,
    },
    Signal {
        pid: i32,
        sig: i32,
    },
}

#[derive(Debug)]
struct ReplyHook {
    verb: String,
    remaining_matches: usize,
    effect: ReplyEffect,
}

/// What a [`FakeAterm`] answers, and what it was asked.
#[derive(Debug)]
struct State {
    generation: u64,
    policy: String,
    /// A whole reply sent in place of the policy line, when set.
    policy_reply: Option<String>,
    info: String,
    info_capabilities: Option<String>,
    claim_reply: String,
    start_reply: Option<String>,
    proven: bool,
    status: Vec<(String, String)>,
    claim: Option<Claim>,
    revision: u64,
    /// Every addressed request, without its `@<sid> `, in arrival order.
    verbs: Vec<String>,
    subscriptions: usize,
    /// A whole reply sent in place of the subscription's acknowledgement, when
    /// set; the connection is closed after it.
    subscribe_reply: Option<String>,
    /// Every `subscribe` request, answered or refused.
    subscribe_asks: usize,
    /// Every event line pushed so far.
    events: Vec<String>,
    connections: u64,
    /// Every accepted connection, so [`FakeAterm::restart`] can drop them.
    streams: Vec<UnixStream>,
    /// How long each addressed request waits before it is answered.
    delay: Duration,
    reply_hook: Option<ReplyHook>,
    /// A path whose existence is recorded as each addressed request arrives.
    witness: Option<PathBuf>,
    /// For each of `verbs`, whether the witness path existed when it arrived.
    witnessed: Vec<bool>,
    body_log: Option<PathBuf>,
    body_counts: Vec<usize>,
    /// The person's gesture count (`status human_seq=`).
    human_seq: u64,
    /// When the person last made a gesture, which `human_ms=` counts from.
    human_at: Option<Instant>,
    /// The content sequence (`seq=`), moved by every write that types.
    content_seq: u64,
    /// The last turn's ledger ID.
    turn_id: u64,
    screen: Vec<String>,
    cursor: (usize, usize),
    /// What reached the session: `send <text>`, `key <name>` (`key act=<word>
    /// <name>` for a key that said what it means for a box), `turn <text>`,
    /// and the submit key a turn pressed.
    typed: Vec<String>,
    /// Changes landed when the next typed write arrives.
    next_write: Vec<NextWrite>,
    /// Screens shown once a key of that name is pressed, the first match
    /// taken ([`FakeAterm::after_key`]).
    after_key: Vec<(String, Shown)>,
}

/// aterm's refusal of a scoped claim that carries a word its contract does not
/// read (`contract.rs`, `read_in`).
pub const CHILD_START_REFUSED: &str = "ERR usage: a scoped claim takes contract= scope= version= \
    vendor= login= (and child= under contract 4)";

/// The leading words of a typed write, as aterm takes them.
#[derive(Debug, Default)]
struct Leading {
    holder: Option<String>,
    revision: Option<u64>,
    duty: Option<Scope>,
    human: Option<u64>,
    /// `if-gen=`: the screen generation the write was decided on.
    generation: Option<String>,
    /// `if=`: a pattern some visible row must match.
    guard: Option<String>,
    /// `act=`: what the input means for the box it dismisses.
    act: Option<String>,
    refusal: Option<String>,
}

/// aterm's refusal of an `act=` word it does not know, or one given twice.
const ACT_USAGE: &str =
    "ERR usage: act=approve|deny|answer says what the input means for the box it dismisses, once";

/// Whether `value` is a screen generation as aterm parses one
/// (`control.rs`, `ScreenGen::parse`): two runs of digits joined by a dot.
fn screen_gen(value: &str) -> bool {
    value.split_once('.').is_some_and(|(epoch, seq)| {
        [epoch, seq]
            .iter()
            .all(|run| !run.is_empty() && run.bytes().all(|byte| byte.is_ascii_digit()))
    })
}

/// Canonical-or-not decimal, as aterm reads `if-supervisor-revision=` and
/// `if-human=`: digits only, any number of them that fits.
fn digits(value: &str) -> Option<u64> {
    (!value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit()))
        .then(|| value.parse().ok())
        .flatten()
}

/// Take a typed write's leading words off `rest` as aterm's
/// `take_leading_options` does: the fence's three words, and where the verb is
/// guarded (`send`, `key`; a turn reads `if-human=` as an option) `if-human=`,
/// `if-gen=`, `if=` and `act=`, up to the first other word or a `--`, which is
/// taken.
fn leading(rest: &str, guarded: bool) -> (Leading, &str) {
    let mut out = Leading::default();
    let mut cur = rest.trim_start();
    loop {
        let (head, tail) = match cur.split_once(char::is_whitespace) {
            Some((head, tail)) => (head, tail.trim_start()),
            None => (cur, ""),
        };
        if head == "--" {
            cur = tail;
            break;
        }
        if let Some(value) = head.strip_prefix("if-supervisor=") {
            if value.is_empty() || value == "-" {
                out.refusal = Some(
                    "ERR usage: if-supervisor=<holder> needs the holder the claim was made under"
                        .to_string(),
                );
            } else if out.holder.replace(value.to_string()).is_some() {
                out.refusal = Some("ERR usage: if-supervisor= given twice".to_string());
            }
        } else if let Some(value) = head.strip_prefix("if-supervisor-revision=") {
            match digits(value) {
                Some(revision) if out.revision.replace(revision).is_none() => {}
                _ => out.refusal = Some(SUPERVISOR_INPUT_USAGE.to_string()),
            }
        } else if let Some(value) = head.strip_prefix("harness-duty=") {
            match Scope::from_word(value) {
                Some(duty) if out.duty.replace(duty).is_none() => {}
                _ => out.refusal = Some(SUPERVISOR_INPUT_USAGE.to_string()),
            }
        } else if guarded && let Some(value) = head.strip_prefix("if-human=") {
            match digits(value) {
                Some(n) if out.human.replace(n).is_some() => {
                    out.refusal = Some("ERR usage: if-human= given twice".to_string());
                }
                Some(_) => {}
                None => {
                    out.refusal = Some(
                        "ERR usage: if-human=<n> needs the person count a read returned \
                         (status human_seq=)"
                            .to_string(),
                    );
                }
            }
        } else if guarded && let Some(value) = head.strip_prefix("if-gen=") {
            if !screen_gen(value) {
                out.refusal = Some(
                    "ERR usage: if-gen=<epoch>.<seq> needs the screen generation a read \
                     returned (status gen=)"
                        .to_string(),
                );
            } else if out.generation.replace(value.to_string()).is_some() {
                out.refusal = Some("ERR usage: if-gen= given twice".to_string());
            }
        } else if guarded && let Some(pattern) = head.strip_prefix("if=") {
            if pattern.is_empty() {
                out.refusal = Some(
                    "ERR usage: if=<re> needs a pattern — ONE token, e.g. \
                     if=Do.you.want.to.proceed"
                        .to_string(),
                );
            } else if out.guard.replace(pattern.to_string()).is_some() {
                out.refusal = Some("ERR usage: if= given twice".to_string());
            }
        } else if guarded && let Some(value) = head.strip_prefix("act=") {
            if !["approve", "deny", "answer"].contains(&value)
                || out.act.replace(value.to_string()).is_some()
            {
                out.refusal = Some(ACT_USAGE.to_string());
            }
        } else {
            break;
        }
        cur = tail;
    }
    if (out.revision.is_some() || out.duty.is_some())
        && (out.holder.is_none() || out.revision.is_none() || out.duty.is_none())
    {
        out.refusal = Some(SUPERVISOR_INPUT_USAGE.to_string());
    }
    (out, cur)
}

/// `text` as a JSON string's contents.
fn json_text(text: &str) -> String {
    let mut out = String::new();
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            c if u32::from(c) < 0x20 => out.push_str(&format!("\\u{:04x}", u32::from(c))),
            c => out.push(c),
        }
    }
    out
}

impl State {
    fn release(&mut self) {
        if self.claim.take().is_some() {
            self.revision += 1;
        }
    }

    /// The policy body as aterm would answer it now: a halt on the session
    /// turns a policy in effect off, `why=hold`, from the same fact.
    fn policy_body(&self) -> String {
        let held = self
            .status
            .iter()
            .any(|(key, value)| key == "hold" && value == "1");
        if !held
            || !self
                .policy
                .split_ascii_whitespace()
                .any(|w| w == "effective=on")
        {
            return self.policy.clone();
        }
        self.policy
            .split_ascii_whitespace()
            .map(|word| match word.split_once('=') {
                Some(("effective", _)) => "effective=off",
                Some(("why", _)) => "why=hold",
                _ => word,
            })
            .collect::<Vec<_>>()
            .join(" ")
    }

    /// `human_ms=` as aterm counts it: from the person's last gesture when one
    /// was made here ([`FakeAterm::set_human_seq`]), else the settable field.
    fn human_ms(&self) -> Option<String> {
        self.human_at.map(|at| at.elapsed().as_millis().to_string())
    }

    fn status_line(&self) -> String {
        let mut line = "OK schema=1 sid=1".to_string();
        let human_ms = self.human_ms();
        for (key, value) in &self.status {
            let value = match (key.as_str(), &human_ms) {
                ("human_ms", Some(ms)) => ms,
                _ => value,
            };
            line.push_str(&format!(" {key}={value}"));
        }
        line.push_str(&format!(" human_seq={}", self.human_seq));
        let word = |value: Option<&String>| value.map_or("-", String::as_str).to_string();
        let claim = self.claim.as_ref();
        line.push_str(&format!(
            " supervisor_contract={} supervisor_scope={} supervisor_revision={} supervisor={} \
             supervisor_proven={} supervisor_version={} supervisor_vendor={} supervisor_child={} supervisor_child_start={}",
            word(claim.and_then(|claim| claim.contract.as_ref())),
            word(claim.map(|claim| &claim.scope)),
            self.revision,
            word(claim.map(|claim| &claim.holder)),
            u8::from(claim.is_some() && self.proven),
            word(claim.and_then(|claim| claim.version.as_ref())),
            word(claim.and_then(|claim| claim.vendor.as_ref())),
            word(claim.and_then(|claim| claim.child.as_ref())),
            claim.and_then(|claim|claim.child_generation).unwrap_or(0),
        ));
        line
    }

    /// `meta set supervisor <holder> <words…>` from connection `id`.
    fn claim(&mut self, id: u64, rest: &str) -> String {
        if self.claim_reply != "OK" {
            return self.claim_reply.clone();
        }
        let mut words = rest.split_ascii_whitespace();
        let Some(holder) = words.next() else {
            return "ERR usage: meta set supervisor <holder> [ttl=<ms>] | meta unset supervisor \
                    [holder=<holder>]"
                .to_string();
        };
        if let Some(other) = self.claim.as_ref().filter(|claim| claim.holder != holder) {
            return format!("ERR busy supervisor={}", other.holder);
        }
        let named = |key: &str| {
            rest.split_ascii_whitespace()
                .find_map(|word| word.strip_prefix(key))
                .map(str::to_string)
        };
        let child = named("child=");
        let child_generation = named("child-start=").and_then(|value| value.parse::<u64>().ok());
        let contract = named("contract=");
        // aterm reads a contract-4 claim's words as `contract= scope=
        // version= vendor= login= child=` and refuses it whole over any
        // other (`contract.rs`, `read_in`).
        if contract.as_deref() == Some("4") && named("child-start=").is_some() {
            return CHILD_START_REFUSED.to_string();
        }
        if child.is_none()
            && child_generation.is_some()
            && let Some(reply) = &self.start_reply
        {
            return reply.clone();
        }
        let scope = named("scope=")
            .unwrap_or_else(|| "relaunch,upgrade,turn-end,approvals,questions,walls".into());
        self.claim = Some(Claim {
            holder: holder.to_string(),
            connection: id,
            contract,
            scope,
            version: named("version="),
            vendor: named("vendor="),
            child,
            child_generation,
        });
        self.revision += 1;
        "OK".to_string()
    }

    /// Land what [`FakeAterm::before_next_write`] queued.
    fn land_next_write(&mut self) {
        for change in std::mem::take(&mut self.next_write) {
            match change {
                NextWrite::PersonKeys => {
                    self.human_seq += 1;
                    self.human_at = Some(Instant::now());
                }
                NextWrite::ClaimMoves => {
                    if self.claim.is_some() {
                        self.revision += 1;
                    }
                }
                NextWrite::Screen(rows, cursor) => {
                    self.screen = rows;
                    self.cursor = cursor;
                }
            }
        }
    }

    /// Whether the standing claim is a scoped one, under contract 3 or 4:
    /// aterm's `live_scoped`.
    fn scoped(&self) -> bool {
        self.claim
            .as_ref()
            .is_some_and(|claim| matches!(claim.contract.as_deref(), Some("3" | "4")))
    }

    /// Judge a typed write's authority from connection `id` as aterm does:
    /// the whole fence by `SupervisorInputFence::authorize`, a holder alone by
    /// the legacy fence, and no fence at all as an operator's write unless it
    /// comes on the scoped claim's own connection
    /// (`scoped_untyped_input_refusal`).
    fn authorize(&self, id: u64, fence: &Leading) -> Result<(), String> {
        let claim = self.claim.as_ref();
        match (&fence.holder, fence.revision, fence.duty) {
            (Some(holder), Some(revision), Some(duty)) => {
                let stands = duty.types()
                    && revision == self.revision
                    && claim.is_some_and(|claim| {
                        matches!(claim.contract.as_deref(), Some("3" | "4"))
                            && claim.holder == *holder
                            && claim.scope.split(',').any(|word| word == duty.word())
                            && claim.connection == id
                    })
                    && self.proven;
                if stands {
                    Ok(())
                } else {
                    Err(DENIED.to_string())
                }
            }
            (Some(holder), None, None) => {
                if self.scoped() {
                    return Err(DENIED.to_string());
                }
                match claim {
                    Some(claim) if claim.holder == *holder => Ok(()),
                    Some(claim) => Err(format!("ERR busy supervisor={}", claim.holder)),
                    None => Err("ERR busy supervisor=-".to_string()),
                }
            }
            _ if self.scoped() && claim.is_some_and(|claim| claim.connection == id) => {
                Err(DENIED.to_string())
            }
            _ => Ok(()),
        }
    }

    /// Write `text` into the screen at the cursor, as a terminal echoes it, and
    /// move the cursor past it.
    fn echo(&mut self, text: &str) {
        let (row, col) = self.cursor;
        while self.screen.len() <= row {
            self.screen.push(String::new());
        }
        let mut cells: Vec<char> = self.screen[row].chars().collect();
        while cells.len() < col {
            cells.push(' ');
        }
        let typed: Vec<char> = text.chars().collect();
        for (at, c) in typed.iter().enumerate() {
            match cells.get_mut(col + at) {
                Some(cell) => *cell = *c,
                None => cells.push(*c),
            }
        }
        self.screen[row] = cells.into_iter().collect();
        self.cursor = (row, col + typed.len());
        self.content_seq += 1;
    }

    /// `send` or `key` from connection `id`.
    fn write(&mut self, id: u64, verb: &str, rest: &str) -> String {
        self.land_next_write();
        let (fence, body) = leading(rest, true);
        if let Some(refusal) = fence.refusal {
            return refusal;
        }
        if body.is_empty() {
            return format!("ERR usage: {verb}");
        }
        if let Err(refusal) = self.authorize(id, &fence) {
            return refusal;
        }
        // aterm's order (`input_if_fenced_observed`): the person, the screen's
        // generation, then the guard on any visible row.
        if fence.human.is_some_and(|n| n != self.human_seq) {
            return format!("OK skipped reason=person seq={}", self.content_seq);
        }
        if fence
            .generation
            .as_ref()
            .is_some_and(|generation| *generation != format!("1.{}", self.content_seq))
        {
            return format!("OK skipped reason=changed seq={}", self.content_seq);
        }
        if let Some(pattern) = &fence.guard {
            let Some(matcher) = Guard::compile(pattern) else {
                return "ERR badregex".to_string();
            };
            if !self.screen.iter().any(|row| matcher.matches(row)) {
                return format!("OK skipped seq={}", self.content_seq);
            }
        }
        let act = fence
            .act
            .as_ref()
            .map(|act| format!("act={act} "))
            .unwrap_or_default();
        self.typed.push(format!("{verb} {act}{body}"));
        if verb == "send" {
            self.echo(body);
        } else {
            self.content_seq += 1;
            if let Some(at) = self.after_key.iter().position(|(key, _)| key == body) {
                let (_, (rows, cursor)) = self.after_key.remove(at);
                self.screen = rows;
                self.cursor = cursor;
            }
        }
        format!("OK seq={}", self.content_seq)
    }

    /// `turn` from connection `id`: its leading words, its options, then its
    /// text; typed at the cursor, and its submit key pressed only where the
    /// guard, if any, matches the cursor's row after the text.
    fn turn(&mut self, id: u64, rest: &str) -> String {
        const USAGE: &str = "ERR usage: turn [id=<epoch>:<producer>:<seq>] \
                             [if-supervisor=<holder>] … <text>";
        self.land_next_write();
        let (fence, rest) = leading(rest, false);
        if let Some(refusal) = fence.refusal {
            return refusal;
        }
        let mut submit = Some("enter".to_string());
        let mut guard = None;
        let mut human = None;
        let mut text = rest;
        loop {
            let (word, tail) = text
                .split_once(char::is_whitespace)
                .map_or((text, ""), |(word, tail)| (word, tail.trim_start()));
            if word == "--" {
                text = tail;
                break;
            }
            match word.split_once('=') {
                Some((key, value)) if TURN_OPTIONS.contains(&key) => {
                    match (key, value.strip_prefix("guarded:")) {
                        ("submit", Some("")) => return USAGE.to_string(),
                        ("submit", Some(pattern)) => guard = Some(pattern.to_string()),
                        ("submit", None) if value == "none" => submit = None,
                        ("submit", None) => submit = Some(value.to_string()),
                        ("if-human", _) => match digits(value) {
                            Some(n) => human = Some(n),
                            None => return USAGE.to_string(),
                        },
                        _ => {}
                    }
                    text = tail;
                }
                _ => break,
            }
        }
        if text.trim().is_empty() {
            return USAGE.to_string();
        }
        if let Err(refusal) = self.authorize(id, &fence) {
            return refusal;
        }
        self.turn_id += 1;
        let id = self.turn_id;
        if human.is_some_and(|n| n != self.human_seq) {
            return format!(
                "OK 0 turn skipped reason=person submitted=0 seq={} id={id}",
                self.content_seq
            );
        }
        let matcher = match guard.as_deref().map(Guard::compile) {
            Some(None) => return "ERR badregex".to_string(),
            Some(Some(matcher)) => Some(matcher),
            None => None,
        };
        self.typed.push(format!("turn {text}"));
        self.echo(text);
        let row = self.screen.get(self.cursor.0).cloned().unwrap_or_default();
        if matcher.is_some_and(|matcher| !matcher.matches(&row)) {
            return format!(
                "OK 0 turn skipped reason=guard submitted=0 seq={} id={id}",
                self.content_seq
            );
        }
        let Some(key) = submit else {
            return format!(
                "OK 0 turn submitted=0 status=settled seq={} id={id} dur_ms=0 \
                 hash=0000000000000000",
                self.content_seq
            );
        };
        self.typed.push(key);
        self.content_seq += 1;
        let mut reply = format!(
            "OK {} turn submitted=1 status=settled seq={} id={id} dur_ms=0 \
             hash=0000000000000000",
            self.screen.len(),
            self.content_seq
        );
        for row in &self.screen {
            reply.push('\n');
            reply.push_str(row);
        }
        reply
    }

    /// `text --json`: the screen and the person's stamps, read together.
    fn text_json(&self) -> String {
        let rows: Vec<String> = self
            .screen
            .iter()
            .map(|row| format!("\"{}\"", json_text(row)))
            .collect();
        format!(
            "OK 1\n{{\"rows\":[{}],\"cursor\":{{\"row\":{},\"col\":{},\"visible\":true,\
             \"style\":\"steady_block\"}},\"dims\":{{\"rows\":{},\"cols\":90}},\"seq\":{},\
             \"gen\":\"1.{}\",\"title\":\"\",\"human_ms\":{},\"human_seq\":{},\
             \"launch\":null}}",
            rows.join(","),
            self.cursor.0,
            self.cursor.1,
            self.screen.len(),
            self.content_seq,
            self.content_seq,
            self.human_ms().unwrap_or_else(|| "null".to_string()),
            self.human_seq,
        )
    }

    /// One addressed request from connection `id`.
    fn answer(&mut self, id: u64, verb: &str) -> String {
        self.verbs.push(verb.to_string());
        self.witnessed
            .push(self.witness.as_ref().is_some_and(|path| path.exists()));
        self.body_counts.push(
            self.body_log
                .as_ref()
                .and_then(|path| std::fs::read_to_string(path).ok())
                .map_or(0, |text| {
                    text.lines()
                        .filter(|line| line.starts_with("start\t"))
                        .count()
                }),
        );
        if verb == "harness policy" {
            self.policy_reply
                .clone()
                .unwrap_or_else(|| format!("OK gen={} {}", self.generation, self.policy_body()))
        } else if verb == "native-act info" {
            self.info.clone()
        } else if verb == "native-act info capabilities=1" {
            self.info_capabilities
                .clone()
                .unwrap_or_else(|| INFO_CAPABILITIES_UNSUPPORTED.to_string())
        } else if verb == "status" {
            self.status_line()
        } else if verb == "text --json" {
            self.text_json()
        } else if let Some(rest) = verb.strip_prefix("send ") {
            self.write(id, "send", rest)
        } else if let Some(rest) = verb.strip_prefix("key ") {
            self.write(id, "key", rest)
        } else if let Some(rest) = verb.strip_prefix("turn ") {
            self.turn(id, rest)
        } else if let Some(rest) = verb.strip_prefix("meta set supervisor ") {
            self.claim(id, rest)
        } else if let Some(rest) = verb.strip_prefix("meta unset supervisor") {
            let named = rest.trim().strip_prefix("holder=");
            if self
                .claim
                .as_ref()
                .is_some_and(|claim| named.is_none_or(|holder| holder == claim.holder))
            {
                self.release();
            }
            "OK".to_string()
        } else {
            "ERR unknown verb".to_string()
        }
    }
}

/// A fake aterm serving one session. Dropping it closes every connection and
/// removes its socket directory.
pub struct FakeAterm {
    /// `XDG_RUNTIME_DIR` for the client: it holds `aterm/graph/<sid>`.
    pub xdg: PathBuf,
    /// The short directory holding the socket and its token.
    pub sock_dir: PathBuf,
    /// The session ID.
    pub sid: String,
    state: Arc<Mutex<State>>,
    stop: Arc<AtomicBool>,
}

fn lock(state: &Mutex<State>) -> MutexGuard<'_, State> {
    // A scenario that panicked while holding the state has already failed; the
    // fake keeps answering so its cleanup can run.
    state
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

impl FakeAterm {
    /// A fake aterm whose rendezvous directory is under `root` (a scenario's
    /// private directory). It starts with [`POLICY_ON`] at generation 1,
    /// [`INFO_UNROSTERED`], [`QUIET_STATUS`], a claim reply of `OK`, the proven
    /// bit set and no claim.
    pub fn new(root: &Path) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        let sid = format!("t{n:010x}");
        let xdg = root.join("xdg");
        let graph = xdg.join("aterm").join("graph");
        std::fs::create_dir_all(&graph).expect("graph directory");
        // A Unix socket path holds about a hundred bytes, and a scenario's own
        // directory is far longer than that.
        let parent = Path::new("/tmp/.ubd");
        std::fs::create_dir_all(parent).expect("socket parent");
        let sock_dir = parent.join(format!("fa-{}-{n}.noindex", std::process::id()));
        let _ = std::fs::remove_dir_all(&sock_dir);
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&sock_dir)
            .expect("socket directory");
        let sock = sock_dir.join("c.sock");
        std::fs::write(sock_dir.join("c.sock.token"), format!("{TOKEN}\n")).expect("token");
        std::fs::write(
            graph.join(&sid),
            format!("pid 1\nsock {}\n", sock.display()),
        )
        .expect("graph entry");
        let listener = UnixListener::bind(&sock).expect("bind the fake socket");
        listener.set_nonblocking(true).expect("nonblocking accept");
        let state = Arc::new(Mutex::new(State {
            generation: 1,
            policy: POLICY_ON.to_string(),
            policy_reply: None,
            info: INFO_UNROSTERED.to_string(),
            info_capabilities: None,
            claim_reply: "OK".to_string(),
            start_reply: None,
            proven: true,
            status: QUIET_STATUS
                .iter()
                .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
                .collect(),
            claim: None,
            revision: 0,
            verbs: Vec::new(),
            subscriptions: 0,
            subscribe_reply: None,
            subscribe_asks: 0,
            events: Vec::new(),
            connections: 0,
            streams: Vec::new(),
            delay: Duration::ZERO,
            reply_hook: None,
            witness: None,
            witnessed: Vec::new(),
            body_log: None,
            body_counts: Vec::new(),
            human_seq: 0,
            human_at: None,
            content_seq: 100,
            turn_id: 0,
            screen: composer_screen().0,
            cursor: composer_screen().1,
            typed: Vec::new(),
            next_write: Vec::new(),
            after_key: Vec::new(),
        }));
        let stop = Arc::new(AtomicBool::new(false));
        let (accept_state, accept_stop, accept_sid) =
            (Arc::clone(&state), Arc::clone(&stop), sid.clone());
        std::thread::spawn(move || {
            let mut id = 0_u64;
            while !accept_stop.load(Ordering::Relaxed) {
                let Ok((stream, _)) = listener.accept() else {
                    std::thread::sleep(Duration::from_millis(5));
                    continue;
                };
                id += 1;
                // Darwin hands the listener's nonblocking flag to accepted sockets.
                if stream.set_nonblocking(false).is_err() {
                    continue;
                }
                if let Ok(clone) = stream.try_clone() {
                    lock(&accept_state).streams.push(clone);
                }
                let (state, stop, sid) = (
                    Arc::clone(&accept_state),
                    Arc::clone(&accept_stop),
                    accept_sid.clone(),
                );
                std::thread::spawn(move || serve(stream, id, &state, &stop, &sid));
            }
        });
        Self {
            xdg,
            sock_dir,
            sid,
            state,
            stop,
        }
    }

    /// The environment that puts a harness in this fake's tab.
    pub fn env(&self) -> Vec<(&'static str, String)> {
        vec![
            ("TERM_PROGRAM", "aterm".to_string()),
            ("ATERM_PARENT_SESSION_ID", self.sid.clone()),
            ("XDG_RUNTIME_DIR", self.xdg.display().to_string()),
        ]
    }

    /// The locator a harness started with [`FakeAterm::env`] resolves.
    pub fn locator(&self) -> Locator {
        let env = self.env();
        let value = |key: &str| {
            env.iter()
                .find(|(name, _)| *name == key)
                .map(|(_, value)| value.clone())
                .expect("environment key")
        };
        Locator::from_values(
            &value("ATERM_PARENT_SESSION_ID"),
            Some(std::ffi::OsStr::new(&value("XDG_RUNTIME_DIR"))),
            None,
        )
        .expect("locator")
    }

    /// Answer `harness policy` with `OK gen=<n> <body>` at the next generation,
    /// and say so on every subscription (`EVENT 1 harness-policy gen=<n>`), as
    /// aterm does when its policy cell moves.
    pub fn set_policy(&self, body: &str) {
        let mut state = lock(&self.state);
        state.generation += 1;
        state.policy = body.to_string();
        state.policy_reply = None;
        let line = format!("EVENT 1 harness-policy gen={}", state.generation);
        state.events.push(line);
    }

    /// Answer `harness policy` with `OK gen=<n> <body>` at the same generation,
    /// and say nothing: what aterm does when a session's own Autopilot word,
    /// rather than its policy cell, turns the policy on or off for it.
    pub fn set_policy_quietly(&self, body: &str) {
        let mut state = lock(&self.state);
        state.policy = body.to_string();
        state.policy_reply = None;
    }

    /// Answer `harness policy` with exactly `reply`: a refusal, or a line an
    /// older aterm would send. No event is sent.
    pub fn set_policy_reply(&self, reply: &str) {
        lock(&self.state).policy_reply = Some(reply.to_string());
    }

    /// Answer `harness policy` from the policy body again, after
    /// [`FakeAterm::set_policy_reply`]. No event is sent.
    pub fn clear_policy_reply(&self) {
        lock(&self.state).policy_reply = None;
    }

    /// Answer `native-act info` with exactly `reply`.
    pub fn set_info(&self, reply: &str) {
        lock(&self.state).info = reply.to_string();
    }

    /// Answer the opt-in Info request with exactly `reply`.
    pub fn set_info_capabilities(&self, reply: &str) {
        lock(&self.state).info_capabilities = Some(reply.to_string());
    }

    /// Answer `meta set supervisor …` with exactly `reply`. Only `OK` stores a
    /// claim; `ERR unproven: …` is what aterm answers a rostered holder it could
    /// not prove.
    pub fn set_claim_reply(&self, reply: &str) {
        lock(&self.state).claim_reply = reply.to_string();
    }

    /// Refuse only pre-spawn claims, leaving ordinary claims/reannouncements usable.
    pub fn set_start_reply(&self, reply: Option<&str>) {
        lock(&self.state).start_reply = reply.map(str::to_string);
    }

    /// Count vendor body receipts at each request, independently of the wire client.
    pub fn set_body_log(&self, path: &Path) {
        lock(&self.state).body_log = Some(path.to_path_buf());
    }

    /// Every addressed request paired with the vendor starts already observed then.
    pub fn verbs_with_body_counts(&self) -> Vec<(String, usize)> {
        let state = lock(&self.state);
        state
            .verbs
            .iter()
            .cloned()
            .zip(state.body_counts.iter().copied())
            .collect()
    }

    /// Whether a stored claim reads `supervisor_proven=1`.
    pub fn set_proven(&self, proven: bool) {
        lock(&self.state).proven = proven;
    }

    /// Set one `status` field, or drop it (`None`: an aterm that does not report
    /// it). The claim's own fields come from the claim state and cannot be set
    /// here.
    pub fn set_status(&self, key: &str, value: Option<&str>) {
        let mut state = lock(&self.state);
        state.status.retain(|(known, _)| known != key);
        if let Some(value) = value {
            state.status.push((key.to_string(), value.to_string()));
        }
    }

    /// Push `EVENT 1 <payload>` to every subscription.
    pub fn event(&self, payload: &str) {
        lock(&self.state).events.push(format!("EVENT 1 {payload}"));
    }

    /// Ask the claim holder to restart its child: `EVENT 1 restart cause=<cause>`,
    /// which aterm sends instead of signalling a harness whose proven claim
    /// takes `relaunch`.
    pub fn send_restart(&self, cause: &str) {
        self.event(&format!("restart cause={cause}"));
    }

    /// An aterm restart: every connection is dropped, and the claim with them.
    /// The socket keeps listening, as a restarted aterm's does.
    pub fn restart(&self) {
        let mut state = lock(&self.state);
        state.release();
        for stream in state.streams.drain(..) {
            let _ = stream.shutdown(Shutdown::Both);
        }
    }

    /// Every addressed request so far, without its `@<sid> `, in arrival order.
    pub fn verbs(&self) -> Vec<String> {
        lock(&self.state).verbs.clone()
    }

    /// Wait `delay` before answering each addressed request from now on: an
    /// aterm that is slow, so whatever a client does while it waits has time to
    /// show.
    pub fn set_reply_delay(&self, delay: Duration) {
        lock(&self.state).delay = delay;
    }

    /// Atomically replace a fixture file before replying to the next request
    /// for `verb`. The request has reached aterm while the old file still stands.
    pub fn rename_before_reply(&self, verb: &str, source: &Path, target: &Path) {
        self.rename_before_nth_reply(verb, 1, source, target);
    }

    /// Atomically rename a fixture file before the `nth` matching reply after
    /// this call, counting from one.
    pub fn rename_before_nth_reply(&self, verb: &str, nth: usize, source: &Path, target: &Path) {
        self.before_nth_reply(
            verb,
            nth,
            ReplyEffect::Rename {
                source: source.to_path_buf(),
                target: target.to_path_buf(),
                receipt: None,
            },
        );
    }

    /// Publish a fixture command and wait until its actor supplies the receipt.
    pub fn rename_before_reply_and_wait(
        &self,
        verb: &str,
        source: &Path,
        target: &Path,
        receipt: &Path,
    ) {
        self.before_nth_reply(
            verb,
            1,
            ReplyEffect::Rename {
                source: source.to_path_buf(),
                target: target.to_path_buf(),
                receipt: Some(receipt.to_path_buf()),
            },
        );
    }

    /// Signal this scenario's harness before answering the next matching request.
    pub fn signal_before_reply(&self, verb: &str, pid: i32, sig: i32) {
        assert!(pid > 0, "the hook signals a single process");
        self.before_nth_reply(verb, 1, ReplyEffect::Signal { pid, sig });
    }

    fn before_nth_reply(&self, verb: &str, nth: usize, effect: ReplyEffect) {
        assert_ne!(nth, 0, "reply indices start at one");
        let mut state = lock(&self.state);
        assert!(state.reply_hook.is_none(), "a reply hook is already set");
        state.reply_hook = Some(ReplyHook {
            verb: verb.to_string(),
            remaining_matches: nth,
            effect,
        });
    }

    /// Record, as each addressed request arrives from now on, whether `path`
    /// exists ([`FakeAterm::verbs_with_witness`]): the order of a request and
    /// something a client does outside the socket, such as starting a program
    /// that writes `path`.
    pub fn set_witness(&self, path: &Path) {
        lock(&self.state).witness = Some(path.to_path_buf());
    }

    /// [`FakeAterm::verbs`], each with whether the witness path existed when
    /// the request arrived (`false` while no witness was set).
    pub fn verbs_with_witness(&self) -> Vec<(String, bool)> {
        let state = lock(&self.state);
        state
            .verbs
            .iter()
            .cloned()
            .zip(state.witnessed.iter().copied())
            .collect()
    }

    /// The `status` line as it would be answered now, without recording a
    /// request.
    pub fn status_line(&self) -> String {
        lock(&self.state).status_line()
    }

    /// The standing claim's holder.
    pub fn holder(&self) -> Option<String> {
        lock(&self.state)
            .claim
            .as_ref()
            .map(|claim| claim.holder.clone())
    }

    /// How many connections authenticated.
    pub fn connections(&self) -> u64 {
        lock(&self.state).connections
    }

    /// Answer `subscribe` with exactly `reply` (`None`: the acknowledgement
    /// aterm sends) and close that connection.
    pub fn set_subscribe_reply(&self, reply: Option<&str>) {
        lock(&self.state).subscribe_reply = reply.map(str::to_string);
    }

    /// How many `subscribe` requests arrived, refused ones included.
    pub fn subscribe_asks(&self) -> usize {
        lock(&self.state).subscribe_asks
    }

    /// How many subscriptions were accepted.
    pub fn subscriptions(&self) -> usize {
        lock(&self.state).subscriptions
    }

    /// A person's gesture: `status human_seq=` and `text --json`'s
    /// `"human_seq"` read `n`, and `human_ms=` counts from now.
    pub fn set_human_seq(&self, n: u64) {
        let mut state = lock(&self.state);
        state.human_seq = n;
        state.human_at = Some(Instant::now());
    }

    /// The person's gesture count.
    pub fn human_seq(&self) -> u64 {
        lock(&self.state).human_seq
    }

    /// Show `rows`, with the cursor at `cursor` (row, column).
    pub fn set_screen(&self, (rows, cursor): (Vec<String>, (usize, usize))) {
        let mut state = lock(&self.state);
        state.screen = rows;
        state.cursor = cursor;
    }

    /// The rows shown now.
    pub fn screen(&self) -> Vec<String> {
        lock(&self.state).screen.clone()
    }

    /// Land `change` when the next `send`, `key` or `turn` arrives, before it is
    /// judged: what happens between a client's read and its write.
    pub fn before_next_write(&self, change: NextWrite) {
        lock(&self.state).next_write.push(change);
    }

    /// Show `screen` once a key named `key` is pressed, as the program redraws
    /// when it takes the key: a box it answers goes, a focus it moves moves.
    /// Each is shown once, in the order given for a name.
    pub fn after_key(&self, key: &str, screen: (Vec<String>, (usize, usize))) {
        lock(&self.state).after_key.push((key.to_string(), screen));
    }

    /// What reached the session, in order: `send <text>`, `key <name>`
    /// (`key act=<word> <name>` for a key that said what it means for a box),
    /// `turn <text>`, and the submit key each turn pressed.
    pub fn typed(&self) -> Vec<String> {
        lock(&self.state).typed.clone()
    }

    /// The claim cell's revision (`status supervisor_revision=`).
    pub fn revision(&self) -> u64 {
        lock(&self.state).revision
    }
}

/// One piece of a guard: what one character may be, and how many.
#[derive(Debug)]
enum Atom {
    Char(char),
    Space,
    Any,
    Class(Vec<Option<char>>),
}

#[derive(Debug)]
enum Count {
    One,
    Star,
    Exactly(usize),
}

/// A `submit=guarded:<re>` or `if=<re>` pattern in the syntax
/// `alab_policy::guard` writes (`^`, `$`, `.`, `\s`, `\xHH`, `\x{H…}`, an
/// escaped metacharacter, a class of characters and `\s`, `*` and `{n}`),
/// matched as a regex is against a row. Anything else does not compile, as a
/// pattern aterm's matcher would read otherwise than this one must not be
/// guessed at.
#[derive(Debug)]
struct Guard {
    start: bool,
    pieces: Vec<(Atom, Count)>,
    end: bool,
}

impl Guard {
    fn escape(chars: &mut std::iter::Peekable<std::str::Chars<'_>>) -> Option<Atom> {
        match chars.next()? {
            's' => Some(Atom::Space),
            'x' => {
                let hex: String = if chars.peek() == Some(&'{') {
                    chars.next();
                    let hex: String = chars.by_ref().take_while(|c| *c != '}').collect();
                    hex
                } else {
                    chars.by_ref().take(2).collect()
                };
                let code = u32::from_str_radix(&hex, 16).ok()?;
                char::from_u32(code).map(Atom::Char)
            }
            c if "\\.+*?()|[]{}^$".contains(c) => Some(Atom::Char(c)),
            _ => None,
        }
    }

    fn compile(pattern: &str) -> Option<Self> {
        let mut chars = pattern.chars().peekable();
        let start = chars.next_if_eq(&'^').is_some();
        let mut pieces = Vec::new();
        let mut end = false;
        while let Some(c) = chars.next() {
            let atom = match c {
                '\\' => Self::escape(&mut chars)?,
                '.' => Atom::Any,
                '$' if chars.peek().is_none() => {
                    end = true;
                    break;
                }
                '[' => {
                    let mut class = Vec::new();
                    loop {
                        match chars.next()? {
                            ']' => break,
                            '\\' => match Self::escape(&mut chars)? {
                                Atom::Char(c) => class.push(Some(c)),
                                Atom::Space => class.push(None),
                                _ => return None,
                            },
                            c => class.push(Some(c)),
                        }
                    }
                    Atom::Class(class)
                }
                '$' | '(' | ')' | '|' | '+' | '?' | '*' | '{' | '}' | ']' | '^' => return None,
                c => Atom::Char(c),
            };
            let count = if chars.next_if_eq(&'*').is_some() {
                Count::Star
            } else if chars.next_if_eq(&'{').is_some() {
                let n: String = chars.by_ref().take_while(|c| *c != '}').collect();
                Count::Exactly(n.parse().ok()?)
            } else {
                Count::One
            };
            pieces.push((atom, count));
        }
        Some(Self { start, pieces, end })
    }

    fn admits(atom: &Atom, c: char) -> bool {
        match atom {
            Atom::Char(want) => c == *want,
            Atom::Space => c.is_whitespace(),
            Atom::Any => c != '\n',
            Atom::Class(class) => class
                .iter()
                .any(|item| item.map_or_else(|| c.is_whitespace(), |want| c == want)),
        }
    }

    fn at(&self, piece: usize, text: &[char], i: usize) -> bool {
        let Some((atom, count)) = self.pieces.get(piece) else {
            return !self.end || i == text.len();
        };
        match count {
            Count::One => {
                i < text.len() && Self::admits(atom, text[i]) && self.at(piece + 1, text, i + 1)
            }
            Count::Exactly(n) => {
                (0..*n).all(|k| text.get(i + k).is_some_and(|c| Self::admits(atom, *c)))
                    && self.at(piece + 1, text, i + n)
            }
            Count::Star => {
                let mut j = i;
                loop {
                    if self.at(piece + 1, text, j) {
                        return true;
                    }
                    if j < text.len() && Self::admits(atom, text[j]) {
                        j += 1;
                    } else {
                        return false;
                    }
                }
            }
        }
    }

    fn matches(&self, row: &str) -> bool {
        let text: Vec<char> = row.chars().collect();
        if self.start {
            self.at(0, &text, 0)
        } else {
            (0..=text.len()).any(|i| self.at(0, &text, i))
        }
    }
}

impl Drop for FakeAterm {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        for stream in lock(&self.state).streams.drain(..) {
            let _ = stream.shutdown(Shutdown::Both);
        }
        let _ = std::fs::remove_dir_all(&self.sock_dir);
    }
}

/// Stream the events pushed after this subscription began, until the client
/// goes, the fake stops, or a restart drops the connection.
fn stream_events(mut out: UnixStream, state: &Mutex<State>, stop: &AtomicBool, mut sent: usize) {
    // The read below is the pause between looks and the detector of a closed peer.
    if out
        .set_read_timeout(Some(Duration::from_millis(5)))
        .is_err()
    {
        return;
    }
    while !stop.load(Ordering::Relaxed) {
        let fresh: Vec<String> = {
            let state = lock(state);
            state.events.get(sent..).unwrap_or_default().to_vec()
        };
        for line in &fresh {
            if out.write_all(format!("{line}\n").as_bytes()).is_err() {
                return;
            }
        }
        sent += fresh.len();
        let mut probe = [0_u8; 1];
        match out.read(&mut probe) {
            Ok(0) => return,
            Ok(_) => {}
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock
                        | std::io::ErrorKind::TimedOut
                        | std::io::ErrorKind::Interrupted
                ) => {}
            Err(_) => return,
        }
    }
}

fn serve(stream: UnixStream, id: u64, state: &Mutex<State>, stop: &AtomicBool, sid: &str) {
    let Ok(mut out) = stream.try_clone() else {
        return;
    };
    let mut authenticated = false;
    for line in BufReader::new(stream).lines() {
        let Ok(line) = line else { break };
        if !authenticated {
            // `AUTH <token>` has no reply of its own; the next request's reply is
            // the first line a client reads.
            if line == format!("AUTH {TOKEN}") {
                authenticated = true;
                lock(state).connections += 1;
                continue;
            }
            let _ = out.write_all(b"ERR auth\n");
            break;
        }
        if line.starts_with("subscribe ") {
            if line != format!("subscribe @{sid} events") {
                let _ = out.write_all(b"ERR usage: subscribe\n");
                break;
            }
            let refusal = {
                let mut state = lock(state);
                state.subscribe_asks += 1;
                state.subscribe_reply.clone()
            };
            if let Some(reply) = refusal {
                let _ = out.write_all(format!("{reply}\n").as_bytes());
                break;
            }
            // The client may publish its first event as soon as it reads the acknowledgment.
            let start = {
                let mut state = lock(state);
                state.subscriptions += 1;
                state.events.len()
            };
            if out
                .write_all(
                    format!("OK subscribe 1\nsub 1 {sid} role=- attention=- user_title=-\n")
                        .as_bytes(),
                )
                .is_ok()
            {
                stream_events(out, state, stop, start);
            }
            break;
        }
        let reply = if line == "version" {
            "OK version=fake-aterm".to_string()
        } else if let Some(verb) = line.strip_prefix(&format!("@{sid} ")) {
            // The request is recorded as it arrives and answered after the delay.
            let (reply, delay, hook) = {
                let mut state = lock(state);
                let ready = state.reply_hook.as_mut().is_some_and(|hook| {
                    if hook.verb != verb {
                        return false;
                    }
                    hook.remaining_matches -= 1;
                    hook.remaining_matches == 0
                });
                let hook = if ready { state.reply_hook.take() } else { None };
                (state.answer(id, verb), state.delay, hook)
            };
            if let Some(hook) = hook {
                match hook.effect {
                    ReplyEffect::Rename {
                        source,
                        target,
                        receipt,
                    } => {
                        std::fs::rename(source, target).expect("replace the fixture file");
                        if let Some(receipt) = receipt {
                            let until = Instant::now() + HANG;
                            while !receipt.is_file() && !stop.load(Ordering::Relaxed) {
                                assert!(Instant::now() < until, "fixture receipt at {receipt:?}");
                                std::thread::sleep(Duration::from_millis(5));
                            }
                        }
                    }
                    ReplyEffect::Signal { pid, sig } => {
                        assert!(claude_code_alab::sys::kill(pid, sig), "kill({pid}, {sig})");
                    }
                }
            }
            if !delay.is_zero() {
                std::thread::sleep(delay);
            }
            reply
        } else {
            "ERR no such session".to_string()
        };
        if out.write_all(format!("{reply}\n").as_bytes()).is_err() {
            break;
        }
    }
    // The connection is gone, and a claim bound to it goes with it.
    let mut state = lock(state);
    if state
        .claim
        .as_ref()
        .is_some_and(|claim| claim.connection == id)
    {
        state.release();
    }
}

// ───────────────────────── the fake and the client agree ─────────────────────────

/// A private root for one self-test's rendezvous directory.
fn root(name: &str) -> PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let root = std::env::temp_dir().join(format!(
        ".ubd/fake-aterm.noindex/{name}-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).expect("self-test root");
    root
}

fn connect(fake: &FakeAterm) -> Connection {
    let socket = fake.locator().socket().expect("the graph names the socket");
    Connection::connect(&socket, TIMEOUT).expect("connect and authenticate")
}

fn eventually(what: &str, mut holds: impl FnMut() -> bool) {
    let until = Instant::now() + HANG;
    while !holds() {
        assert!(Instant::now() < until, "hang detector: {what}");
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// Drain `events` until `wanted` says an update is the one awaited, waiting on
/// the reader's own descriptor as a supervisor's loop will.
fn await_update(
    what: &str,
    events: &mut EventReader,
    wanted: impl Fn(&EventUpdate) -> bool,
) -> EventUpdate {
    let until = Instant::now() + HANG;
    loop {
        let mut probe = libc::pollfd {
            fd: events.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: probe is one initialized poll record for the reader's open descriptor.
        let _ = unsafe { libc::poll(&raw mut probe, 1, 50) };
        let update = events.drain();
        if wanted(&update) {
            return update;
        }
        assert!(
            !update.ended,
            "the event stream ended awaiting {what}: {update:?}"
        );
        assert!(Instant::now() < until, "hang detector: {what}");
    }
}

fn ask(connection: &mut Connection, request: Option<String>) -> String {
    connection
        .request_line(&request.expect("a session ID the client accepts"))
        .expect("a reply")
}

fn me() -> (u32, String) {
    let pid = std::process::id();
    (pid, aterm::holder(pid))
}

fn claim(fake: &FakeAterm, connection: &mut Connection, scopes: &[Scope]) -> String {
    let (pid, _) = me();
    ask(
        connection,
        aterm::claim_verb(&fake.sid, pid, scopes, "0.1.0", Some("2.1.311")),
    )
}

fn status(fake: &FakeAterm, connection: &mut Connection) -> Status {
    Status::parse(&ask(connection, aterm::status_verb(&fake.sid))).expect("a schema-1 status")
}

fn policy(fake: &FakeAterm, connection: &mut Connection) -> Policy {
    Policy::parse(&ask(connection, aterm::policy_verb(&fake.sid))).expect("a policy")
}

/// The client finds the socket through the graph, reads the token beside it and
/// authenticates; a wrong token and an unknown session are refused.
pub fn the_fake_aterm_is_found_and_authenticates_this_crates_client() {
    let fake = FakeAterm::new(&root("auth"));
    assert_eq!(fake.locator().sid(), fake.sid);
    assert_eq!(
        fake.locator().socket().expect("socket"),
        fake.sock_dir.join("c.sock")
    );
    let mut connection = connect(&fake);
    assert_eq!(connection.version(), "version=fake-aterm");
    assert_eq!(fake.connections(), 1);
    // A request must name this session, and an unknown verb is an ordinary error.
    assert_eq!(
        connection
            .request_line("@tzzzzzzzzzz status")
            .expect("reply"),
        "ERR no such session"
    );
    assert_eq!(
        connection
            .request_line(&format!("@{} look", fake.sid))
            .expect("reply"),
        "ERR unknown verb"
    );
    assert!(!connection.is_poisoned());
    assert_eq!(fake.verbs(), ["look"]);

    let token = fake.sock_dir.join("c.sock.token");
    std::fs::write(&token, "another-token\n").expect("rewrite the token");
    let socket = fake.locator().socket().expect("socket");
    let refused = Connection::connect(&socket, TIMEOUT).expect_err("a wrong token");
    assert_eq!(refused.kind(), std::io::ErrorKind::PermissionDenied);
    assert_eq!(fake.connections(), 1);
    std::fs::write(&token, format!("{TOKEN}\n")).expect("restore the token");
    drop(connect(&fake));
    assert_eq!(fake.connections(), 2);
}

/// `harness policy` parses, moves with its generation, and a refusal or an older
/// aterm's line is an error and never a policy.
pub fn the_fake_aterms_policy_reads_as_a_policy() {
    let fake = FakeAterm::new(&root("policy"));
    let mut connection = connect(&fake);
    let on = policy(&fake, &mut connection);
    assert_eq!(on.generation, 1);
    assert!(on.admits_claim() && on.relaunch && on.upgrade && !on.upgrade_ask);
    assert_eq!(on.human_grace, Duration::from_secs(120));

    fake.set_policy(&POLICY_ON.replace("effective=on why=enabled", "effective=off why=hold"));
    let off = policy(&fake, &mut connection);
    assert_eq!(off.generation, 2);
    assert!(off.serves_contract() && !off.effective && !off.admits_claim());

    // A halt is the policy off, `why=hold`, at the same generation.
    fake.set_policy(POLICY_ON);
    fake.set_status("hold", Some("1"));
    let held = policy(&fake, &mut connection);
    assert_eq!(held.generation, 3);
    assert!(!held.effective && held.why.as_deref() == Some("hold"));
    assert!(!held.off_by_configuration());
    fake.set_status("hold", Some("0"));
    assert!(policy(&fake, &mut connection).effective);

    // An aterm that predates contract 3 says nothing of contracts.
    fake.set_policy_reply("OK gen=12 effective=on why=config continue=on land=off questions=on");
    let older = ask(&mut connection, aterm::policy_verb(&fake.sid));
    assert_eq!(Policy::parse(&older), Err(ReplyError::Missing("contract")));
    assert_eq!(aterm::advertised_contract(&older), None);
    fake.set_policy_reply("ERR denied");
    let denied = ask(&mut connection, aterm::policy_verb(&fake.sid));
    assert_eq!(
        Policy::parse(&denied).unwrap_err().to_string(),
        "reply=ERR:denied"
    );
    // An explicit ERR is a reply: the connection stays usable.
    assert!(!connection.is_poisoned());
    assert_eq!(fake.verbs(), ["harness policy"; 6]);
}

/// `native-act info` carries a bare word, reads through the roster guard, and
/// today's reply names no row for this harness.
pub fn the_fake_aterms_info_reply_reads_through_the_roster_guard() {
    let fake = FakeAterm::new(&root("info"));
    let mut connection = connect(&fake);
    let today = ask(&mut connection, aterm::info_verb(&fake.sid));
    assert_eq!(today, INFO_UNROSTERED);
    assert!(!aterm::rostered(&today));
    // The strict reader refuses the bare `native-act`, so this reply must never
    // go through `request_fields`, which would poison the connection.
    assert!(claude_code_alab::aterm_wire::Fields::parse(&today).is_err());

    fake.set_info(INFO_ROSTERED);
    assert!(aterm::rostered(&ask(
        &mut connection,
        aterm::info_verb(&fake.sid)
    )));
    fake.set_info(INFO_CURRENT);
    let current = ask(&mut connection, aterm::info_verb(&fake.sid));
    assert_eq!(current, INFO_CURRENT);
    assert!(aterm::rostered(&current));
    fake.set_info("ERR usage: native-act");
    assert!(!aterm::rostered(&ask(
        &mut connection,
        aterm::info_verb(&fake.sid)
    )));
    assert!(!connection.is_poisoned());
    // The guard is a read: nothing was claimed by asking.
    assert_eq!(fake.holder(), None);
    assert_eq!(fake.verbs(), ["native-act info"; 4]);
}

/// A claim is stored, `status` reads it back proven with its scope, the judge
/// calls a quiet session safe, and a release clears it.
pub fn a_claim_on_the_fake_aterm_reads_back_proven_in_status() {
    let fake = FakeAterm::new(&root("claim"));
    let mut connection = connect(&fake);
    let (pid, holder) = me();
    let before = status(&fake, &mut connection);
    assert_eq!(before.supervisor, None);
    assert!(!before.held_by(&holder) && before.supervisor_scope.is_empty());
    assert_eq!(before.supervisor_revision, Some(0));

    assert_eq!(claim(&fake, &mut connection, &[Scope::Upgrade]), "OK");
    assert_eq!(fake.holder().as_deref(), Some(holder.as_str()));
    let held = status(&fake, &mut connection);
    assert!(held.held_by(&holder));
    assert!(held.takes(Scope::Upgrade) && !held.takes(Scope::Relaunch));
    assert_eq!(held.supervisor_revision, Some(1));
    assert_eq!(held.human, Human::Never);
    let on = policy(&fake, &mut connection);
    assert_eq!(
        judge_safe_point(&on, &held, &holder, Duration::ZERO, Scope::Upgrade),
        SafePoint::Safe
    );

    // One field at a time holds, by name.
    fake.set_status("composer", Some("draft"));
    let drafting = status(&fake, &mut connection);
    assert_eq!(
        judge_safe_point(&on, &drafting, &holder, Duration::ZERO, Scope::Upgrade),
        SafePoint::Hold(Hold {
            field: "composer",
            value: "draft".to_string(),
            grace_left: None,
        })
    );
    fake.set_status("composer", None);
    let SafePoint::Hold(hold) = judge_safe_point(
        &on,
        &status(&fake, &mut connection),
        &holder,
        Duration::ZERO,
        Scope::Upgrade,
    ) else {
        panic!("a status without composer= holds");
    };
    assert_eq!(hold.to_string(), "wait aterm composer=absent");
    fake.set_status("composer", Some("empty"));

    // A stored claim aterm did not prove is not this harness's to act under.
    fake.set_proven(false);
    let unproven = status(&fake, &mut connection);
    assert!(!unproven.held_by(&holder));
    assert_eq!(unproven.supervisor.as_deref(), Some(holder.as_str()));
    let SafePoint::Hold(hold) =
        judge_safe_point(&on, &unproven, &holder, Duration::ZERO, Scope::Upgrade)
    else {
        panic!("an unproven claim holds");
    };
    assert_eq!(hold.to_string(), "wait aterm supervisor_proven=0");
    fake.set_proven(true);

    // Claiming again on the same connection widens the scope in place.
    assert_eq!(
        claim(&fake, &mut connection, &[Scope::Upgrade, Scope::Relaunch]),
        "OK"
    );
    let wide = status(&fake, &mut connection);
    assert!(wide.held_by(&holder) && wide.takes(Scope::Relaunch) && wide.takes(Scope::Upgrade));

    assert_eq!(
        ask(&mut connection, aterm::release_verb(&fake.sid, pid)),
        "OK"
    );
    assert_eq!(fake.holder(), None);
    assert_eq!(status(&fake, &mut connection).supervisor, None);

    let verbs = fake.verbs();
    assert_eq!(verbs.first().map(String::as_str), Some("status"));
    assert_eq!(
        verbs.get(1).map(String::as_str),
        Some(
            format!(
                "meta set supervisor {holder} contract=3 scope=upgrade version=0.1.0 \
                 vendor=2.1.311"
            )
            .as_str()
        )
    );
    assert!(verbs.contains(&format!(
        "meta set supervisor {holder} contract=3 scope=relaunch,upgrade version=0.1.0 \
         vendor=2.1.311"
    )));
    assert_eq!(
        verbs.iter().rev().nth(1).map(String::as_str),
        Some(format!("meta unset supervisor holder={holder}").as_str())
    );
}

/// The settable claim reply refuses without storing, another holder's standing
/// claim answers `ERR busy`, and a release naming someone else changes nothing.
pub fn a_refused_or_contended_claim_on_the_fake_aterm_stores_nothing() {
    let fake = FakeAterm::new(&root("refused"));
    let mut connection = connect(&fake);
    let (pid, holder) = me();
    let unproven = "ERR unproven: a contract 3 claim needs its own direct connection, the \
                    session's foreground group and a build of its program from atpkg";
    fake.set_claim_reply(unproven);
    assert_eq!(claim(&fake, &mut connection, &[Scope::Upgrade]), unproven);
    assert_eq!(fake.holder(), None);
    assert_eq!(status(&fake, &mut connection).supervisor, None);
    fake.set_claim_reply("OK");

    // Another program holds the session from its own connection.
    let mut other = connect(&fake);
    assert_eq!(
        other
            .request_line(&format!(
                "@{} meta set supervisor someone-else@77",
                fake.sid
            ))
            .expect("reply"),
        "OK"
    );
    let foreign = status(&fake, &mut connection);
    assert_eq!(foreign.supervisor.as_deref(), Some("someone-else@77"));
    // A claim without `contract=` takes every duty.
    assert!(foreign.takes(Scope::Upgrade) && foreign.takes(Scope::Relaunch));
    assert!(!foreign.held_by(&holder));
    assert_eq!(
        claim(&fake, &mut connection, &[Scope::Upgrade]),
        "ERR busy supervisor=someone-else@77"
    );
    // Releasing by name leaves the other holder's claim alone.
    assert_eq!(
        ask(&mut connection, aterm::release_verb(&fake.sid, pid)),
        "OK"
    );
    assert_eq!(fake.holder().as_deref(), Some("someone-else@77"));
    drop(other);
    eventually("the other holder's connection closes", || {
        fake.holder().is_none()
    });
    assert_eq!(claim(&fake, &mut connection, &[Scope::Upgrade]), "OK");
    assert!(status(&fake, &mut connection).held_by(&holder));
}

/// A claim is bound to its connection: closing it releases the claim, and
/// another connection reads the session unclaimed.
pub fn closing_the_claims_connection_releases_it_on_the_fake_aterm() {
    let fake = FakeAterm::new(&root("drop"));
    let (_, holder) = me();
    let mut claimant = connect(&fake);
    let mut observer = connect(&fake);
    assert_eq!(claim(&fake, &mut claimant, &[Scope::Upgrade]), "OK");
    let seen = status(&fake, &mut observer);
    assert!(seen.held_by(&holder));
    let revision = seen.supervisor_revision.expect("revision");

    // The observer going away releases nothing: the claim is the claimant's
    // connection's, not the last one's.
    drop(observer);
    assert_eq!(fake.connections(), 2);
    assert!(status(&fake, &mut claimant).held_by(&holder));

    drop(claimant);
    eventually("the claim goes with its connection", || {
        fake.holder().is_none()
    });
    let mut later = connect(&fake);
    let after = status(&fake, &mut later);
    assert_eq!(after.supervisor, None);
    assert!(!after.supervisor_proven && after.supervisor_scope.is_empty());
    assert!(after.supervisor_revision.expect("revision") > revision);
}

/// A subscription is acknowledged with a numeric tag this crate's strict reader
/// accepts, and events arrive on a descriptor that polls readable.
pub fn events_from_the_fake_aterm_reach_this_crates_reader() {
    let fake = FakeAterm::new(&root("events"));
    let socket = fake.locator().socket().expect("socket");
    // An event from before the subscription is not replayed.
    fake.send_restart("before-subscribe");
    let mut events = EventReader::connect(&socket, &fake.sid, TIMEOUT).expect("subscribe");
    fake.event("status");
    let update = await_update("a status event", &mut events, |update| {
        update.activity || update.restart.is_some()
    });
    assert!(update.activity);
    assert!(!update.policy_changed && update.restart.is_none() && update.fault.is_none());
    assert_eq!(fake.subscriptions(), 1);

    fake.set_policy(POLICY_ON);
    let update = await_update("a policy event", &mut events, |update| {
        update.policy_changed
    });
    assert!(update.requires_refresh() && !update.ended);
    // The event says only that the policy moved; the policy is read again.
    let mut connection = connect(&fake);
    assert_eq!(policy(&fake, &mut connection).generation, 2);

    // A kind the reader has no use for is read past.
    fake.event("meta field=title value=-");
    fake.event("turn 7 submitted=1 status=done dur_ms=12 by=-");
    let update = await_update("a turn event", &mut events, |update| update.activity);
    assert!(update.fault.is_none() && !update.ended);

    // A subscription for another session is refused outright.
    assert!(EventReader::connect(&socket, "tzzzzzzzzzz", TIMEOUT).is_err());
    assert_eq!(fake.subscriptions(), 1);
}

/// The restart sender's line parses as a restart with its cause.
pub fn a_restart_event_from_the_fake_aterm_parses() {
    let fake = FakeAterm::new(&root("restart-event"));
    let socket = fake.locator().socket().expect("socket");
    let mut events = EventReader::connect(&socket, &fake.sid, TIMEOUT).expect("subscribe");
    fake.send_restart("asked");
    let update = await_update("a restart event", &mut events, |update| {
        update.restart.is_some()
    });
    assert_eq!(
        update.restart.map(|restart| restart.cause).as_deref(),
        Some("asked")
    );
    assert!(update.fault.is_none() && !update.ended && !update.policy_changed);
    fake.send_restart("stall");
    let update = await_update("a second restart event", &mut events, |update| {
        update.restart.is_some()
    });
    assert_eq!(
        update.restart.map(|restart| restart.cause).as_deref(),
        Some("stall")
    );
}

/// An aterm restart drops every connection and the claim; the client sees its
/// request connection fail and its event stream end, finds the socket again
/// through the graph, and claims again.
pub fn restarting_the_fake_aterm_drops_every_connection_and_the_claim() {
    let fake = FakeAterm::new(&root("restart"));
    let (_, holder) = me();
    let socket = fake.locator().socket().expect("socket");
    let mut connection = connect(&fake);
    let mut events = EventReader::connect(&socket, &fake.sid, TIMEOUT).expect("subscribe");
    assert_eq!(claim(&fake, &mut connection, &[Scope::Upgrade]), "OK");
    assert!(status(&fake, &mut connection).held_by(&holder));

    fake.restart();
    assert_eq!(fake.holder(), None);
    let request = aterm::status_verb(&fake.sid).expect("status verb");
    assert!(connection.request_line(&request).is_err());
    // A failed exchange poisons the connection: nothing more is sent on it.
    assert!(connection.is_poisoned());
    assert!(connection.request_line(&request).is_err());
    let update = await_update("the event stream ends", &mut events, |update| update.ended);
    assert!(update.requires_refresh());
    assert!(events.drain().ended);

    let mut again = connect(&fake);
    assert_eq!(status(&fake, &mut again).supervisor, None);
    assert_eq!(claim(&fake, &mut again, &[Scope::Upgrade]), "OK");
    assert!(status(&fake, &mut again).held_by(&holder));
    let mut events = EventReader::connect(&socket, &fake.sid, TIMEOUT).expect("subscribe again");
    fake.event("agent");
    assert!(
        await_update("an event after the restart", &mut events, |update| update
            .activity)
        .fault
        .is_none()
    );
    assert_eq!(fake.subscriptions(), 2);
}

// ──────────────────────── the typing client and the fake aterm ────────────────────────

/// Claim `scopes` on `connection` and read the policy: a typist's world.
fn typing_claim(fake: &FakeAterm, connection: &mut Connection, scopes: &[Scope]) -> Policy {
    assert_eq!(claim(fake, connection, scopes), "OK");
    policy(fake, connection)
}

/// The fake's addressed requests from `from` on.
fn verbs_since(fake: &FakeAterm, from: usize) -> Vec<String> {
    fake.verbs().split_off(from)
}

/// The fence a write by this process names at `revision` and person count
/// `human`, for `duty`: the words between the verb and its text.
fn fence_words(revision: u64, duty: Scope, human: u64) -> String {
    let (_, holder) = me();
    format!(
        "if-supervisor={holder} if-supervisor-revision={revision} harness-duty={} \
         if-human={human}",
        duty.word()
    )
}

/// A typed write carries the claim's holder, its revision, its duty and the
/// person's count, and lands; aterm's fence refuses a write that names it in
/// part, names a duty that types nothing, comes from another connection or
/// omits it on the claim's own; a duty the claim does not take is held before
/// anything is sent.
pub fn aterm_typed_write_carries_the_fence() {
    let fake = FakeAterm::new(&root("typed-fence"));
    let (pid, holder) = me();
    let mut connection = connect(&fake);
    let on = typing_claim(
        &fake,
        &mut connection,
        &[
            Scope::Relaunch,
            Scope::Upgrade,
            Scope::TurnEnd,
            Scope::Approvals,
        ],
    );
    let revision = fake.revision();
    let typist = Typist::new(&fake.sid, pid).expect("typist");
    let from = fake.verbs().len();
    assert_eq!(
        typist
            .write(
                &mut connection,
                &on,
                Scope::TurnEnd,
                Write::Send("continue")
            )
            .expect("send"),
        Typed::Answered(Reply::Written { seq: Some(101) })
    );
    assert_eq!(
        typist
            .write(&mut connection, &on, Scope::Approvals, Write::Key("enter"))
            .expect("key"),
        Typed::Answered(Reply::Written { seq: Some(102) })
    );
    assert_eq!(
        verbs_since(&fake, from),
        [
            "status".to_string(),
            format!(
                "send {} -- continue",
                fence_words(revision, Scope::TurnEnd, 0)
            ),
            "status".to_string(),
            format!("key {} enter", fence_words(revision, Scope::Approvals, 0)),
        ]
    );
    assert_eq!(fake.typed(), ["send continue", "key enter"]);

    // aterm types nothing that does not carry the whole fence for this claim.
    let sid = &fake.sid;
    let raw = |connection: &mut Connection, words: &str| {
        connection
            .request_line(&format!("@{sid} {words}"))
            .expect("a reply")
    };
    assert_eq!(raw(&mut connection, "send -- x"), DENIED);
    assert_eq!(
        raw(
            &mut connection,
            &format!("send if-supervisor={holder} -- x")
        ),
        DENIED
    );
    assert_eq!(
        raw(
            &mut connection,
            &format!("send if-supervisor={holder} if-supervisor-revision={revision} -- x")
        ),
        SUPERVISOR_INPUT_USAGE
    );
    assert_eq!(
        raw(
            &mut connection,
            &format!(
                "send if-supervisor={holder} if-supervisor-revision={revision} \
                 harness-duty=relaunch -- x"
            )
        ),
        DENIED
    );
    let mut other = connect(&fake);
    assert_eq!(
        raw(
            &mut other,
            &format!("send {} -- x", fence_words(revision, Scope::TurnEnd, 0))
        ),
        DENIED
    );
    assert_eq!(fake.typed(), ["send continue", "key enter"]);

    // A duty the claim does not take is the client's hold: nothing is sent.
    let from = fake.verbs().len();
    let Typed::Held(hold) = typist
        .write(&mut connection, &on, Scope::Questions, Write::Send("yes"))
        .expect("questions")
    else {
        panic!("a duty the claim does not take is held");
    };
    assert_eq!(
        hold.to_string(),
        "wait aterm supervisor_scope=relaunch,upgrade,turn-end,approvals"
    );
    assert_eq!(verbs_since(&fake, from), ["status"]);
    assert!(!connection.is_poisoned());
}

/// A write whose claim was made again between the read and the write is
/// refused by aterm's fence; the client reads `status` once more, hands it
/// back with the refusal, and sends the write no second time. The next write,
/// judged afresh, names the new revision and lands.
pub fn aterm_typed_write_at_a_stale_revision_rereads_status_and_does_not_retry_blind() {
    let fake = FakeAterm::new(&root("typed-stale"));
    let (pid, holder) = me();
    let mut connection = connect(&fake);
    let on = typing_claim(
        &fake,
        &mut connection,
        &[Scope::Relaunch, Scope::Upgrade, Scope::TurnEnd],
    );
    let revision = fake.revision();
    let typist = Typist::new(&fake.sid, pid).expect("typist");
    let from = fake.verbs().len();
    fake.before_next_write(NextWrite::ClaimMoves);
    let Typed::Denied {
        status: Some(after),
    } = typist
        .write(
            &mut connection,
            &on,
            Scope::TurnEnd,
            Write::Send("continue"),
        )
        .expect("send")
    else {
        panic!("a stale revision is refused and status is read again");
    };
    assert!(after.held_by(&holder));
    assert_eq!(after.supervisor_revision, Some(revision + 1));
    assert_eq!(
        verbs_since(&fake, from),
        [
            "status".to_string(),
            format!(
                "send {} -- continue",
                fence_words(revision, Scope::TurnEnd, 0)
            ),
            "status".to_string(),
        ]
    );
    assert!(fake.typed().is_empty());

    let from = fake.verbs().len();
    assert_eq!(
        typist
            .write(
                &mut connection,
                &on,
                Scope::TurnEnd,
                Write::Send("continue")
            )
            .expect("send again"),
        Typed::Answered(Reply::Written { seq: Some(101) })
    );
    assert_eq!(
        verbs_since(&fake, from),
        [
            "status".to_string(),
            format!(
                "send {} -- continue",
                fence_words(revision + 1, Scope::TurnEnd, 0)
            ),
        ]
    );
    assert_eq!(fake.typed(), ["send continue"]);
}

/// A person's key between the read and the write skips the write (aterm's
/// `if-human=` fence), and the next attempt sends nothing until the policy's
/// grace has run since that key; then it names the person's new count and
/// lands. A turn a person's key overtakes types nothing either.
pub fn aterm_typed_write_after_a_persons_key_is_skipped_and_waits_the_grace() {
    let fake = FakeAterm::new(&root("typed-person"));
    let (pid, _) = me();
    fake.set_policy(&POLICY_ON.replace("human_grace_s=120", "human_grace_s=1"));
    let mut connection = connect(&fake);
    let on = typing_claim(
        &fake,
        &mut connection,
        &[Scope::Relaunch, Scope::Upgrade, Scope::TurnEnd],
    );
    assert_eq!(on.human_grace, Duration::from_secs(1));
    let revision = fake.revision();
    let typist = Typist::new(&fake.sid, pid).expect("typist");
    let from = fake.verbs().len();
    fake.before_next_write(NextWrite::PersonKeys);
    let Typed::Answered(Reply::Person { .. }) = typist
        .write(
            &mut connection,
            &on,
            Scope::TurnEnd,
            Write::Send("continue"),
        )
        .expect("send")
    else {
        panic!("a person's key since the read skips the write");
    };
    assert_eq!(fake.human_seq(), 1);
    assert!(fake.typed().is_empty());
    assert_eq!(
        verbs_since(&fake, from),
        [
            "status".to_string(),
            format!(
                "send {} -- continue",
                fence_words(revision, Scope::TurnEnd, 0)
            ),
        ]
    );

    // At once again: aterm's status says a person typed within the grace, so
    // nothing is sent, and the hold says how much of it is left. Nothing in
    // the fake announces the grace's end; the client reads it from status.
    let from = fake.verbs().len();
    let Typed::Held(hold) = typist
        .write(
            &mut connection,
            &on,
            Scope::TurnEnd,
            Write::Send("continue"),
        )
        .expect("held")
    else {
        panic!("a write within the person's grace is held");
    };
    assert_eq!(hold.field, "human_ms");
    let left = hold.grace_left.expect("the grace left");
    assert!(left <= Duration::from_secs(1), "{left:?}");
    assert_eq!(verbs_since(&fake, from), ["status"]);
    assert!(fake.typed().is_empty());

    // Once the grace has run, the write names the person's new count.
    let landed = after_the_grace(&typist, &mut connection, &on, Write::Send("continue"));
    assert_eq!(landed, Typed::Answered(Reply::Written { seq: Some(101) }));
    let sends: Vec<String> = fake
        .verbs()
        .into_iter()
        .filter(|verb| verb.starts_with("send "))
        .collect();
    assert_eq!(
        sends.last().map(String::as_str),
        Some(
            format!(
                "send {} -- continue",
                fence_words(revision, Scope::TurnEnd, 1)
            )
            .as_str()
        )
    );
    assert_eq!(fake.typed(), ["send continue"]);

    // A key a person makes before the read holds the next write as well.
    fake.set_human_seq(fake.human_seq() + 1);
    let Typed::Held(hold) = typist
        .write(
            &mut connection,
            &on,
            Scope::TurnEnd,
            Write::Turn("carry on"),
        )
        .expect("held turn")
    else {
        panic!("a turn within the person's grace is held");
    };
    assert_eq!(hold.field, "human_ms");

    // A turn is fenced on the person too: once the grace has run, a key
    // between the screen read and the turn types nothing of it.
    fake.before_next_write(NextWrite::PersonKeys);
    let Typed::Answered(Reply::Person { .. }) =
        after_the_grace(&typist, &mut connection, &on, Write::Turn("carry on"))
    else {
        panic!("a person's key since the screen read skips the turn");
    };
    assert_eq!(fake.human_seq(), 3);
    assert_eq!(fake.typed(), ["send continue"]);
    assert!(!connection.is_poisoned());
}

/// `write` of a turn-end duty, made once the person's grace has run: each
/// attempt held for it waits out the grace it names, under the hang detector.
fn after_the_grace(
    typist: &Typist,
    connection: &mut Connection,
    on: &Policy,
    write: Write<'_>,
) -> Typed {
    let until = Instant::now() + HANG;
    loop {
        match typist
            .write(connection, on, Scope::TurnEnd, write)
            .expect("a write after the grace")
        {
            Typed::Held(hold) if hold.field == "human_ms" => {
                assert!(Instant::now() < until, "hang detector: the grace never ran");
                std::thread::sleep(hold.grace_left.unwrap_or_default() + Duration::from_millis(5));
            }
            other => return other,
        }
    }
}

/// A turn's Enter is guarded on Claude Code's composer row, built from the
/// screen read with aterm's own readers: on the composer the text is submitted;
/// where a box opened between the read and the turn, the text lands where the
/// cursor is and no Enter is pressed into the box; a screen with no composer,
/// or a person's draft in it, is held before anything is sent.
pub fn aterm_typed_text_is_guarded_on_the_composer_row() {
    let fake = FakeAterm::new(&root("typed-guard"));
    let (pid, _) = me();
    let mut connection = connect(&fake);
    let on = typing_claim(
        &fake,
        &mut connection,
        &[Scope::Relaunch, Scope::Upgrade, Scope::TurnEnd],
    );
    let revision = fake.revision();
    let typist = Typist::new(&fake.sid, pid).expect("typist");
    let from = fake.verbs().len();
    assert_eq!(
        typist
            .write(
                &mut connection,
                &on,
                Scope::TurnEnd,
                Write::Turn("carry on")
            )
            .expect("turn"),
        Typed::Answered(Reply::Submitted)
    );
    let guard = "^❯\\scarry\\x20on\\s*$";
    assert_eq!(
        verbs_since(&fake, from),
        [
            "status".to_string(),
            "text --json".to_string(),
            format!(
                "turn if-supervisor={} if-supervisor-revision={revision} harness-duty=turn-end \
                 submit=guarded:{guard} if-human=0 idle=600 timeout=2500 -- carry on",
                aterm::holder(pid)
            ),
        ]
    );
    assert_eq!(fake.typed(), ["turn carry on", "enter"]);
    assert_eq!(fake.screen()[3], "❯ carry on");

    // A box opens after the read: the text goes where the cursor stands and
    // the guard keeps the Enter out of the box.
    fake.set_screen(composer_screen());
    fake.before_next_write(NextWrite::Screen(box_screen().0, box_screen().1));
    assert_eq!(
        typist
            .write(
                &mut connection,
                &on,
                Scope::TurnEnd,
                Write::Turn("carry on")
            )
            .expect("turn into a box"),
        Typed::Answered(Reply::Left("guard".to_string()))
    );
    assert_eq!(fake.typed(), ["turn carry on", "enter", "turn carry on"]);

    // No composer on the read: nothing is sent.
    fake.set_screen(box_screen());
    let from = fake.verbs().len();
    let Typed::Held(hold) = typist
        .write(
            &mut connection,
            &on,
            Scope::TurnEnd,
            Write::Turn("carry on"),
        )
        .expect("no composer")
    else {
        panic!("a screen with no composer is held");
    };
    assert_eq!(hold.to_string(), "wait aterm text=no-composer");
    assert_eq!(verbs_since(&fake, from), ["status", "text --json"]);

    // A draft in the composer is the person's: nothing is read or sent.
    fake.set_screen(composer_screen());
    fake.set_status("composer", Some("draft"));
    let from = fake.verbs().len();
    let Typed::Held(hold) = typist
        .write(
            &mut connection,
            &on,
            Scope::TurnEnd,
            Write::Turn("carry on"),
        )
        .expect("a draft")
    else {
        panic!("a draft is held");
    };
    assert_eq!(hold.to_string(), "wait aterm composer=draft");
    assert_eq!(verbs_since(&fake, from), ["status"]);
    assert_eq!(fake.typed(), ["turn carry on", "enter", "turn carry on"]);
    assert!(!connection.is_poisoned());
}

/// A key on a box names the read it was decided on, as the box duties press
/// it: aterm presses it while the screen is that read's and a row matches its
/// guard, and records what it means for the box; a screen that moved since,
/// or a guard no row matches, presses nothing. A key that only moves a focus
/// says nothing of the box. A key the fake was told the program takes shows
/// its next screen.
pub fn aterm_a_key_on_a_box_is_fenced_on_its_read() {
    let fake = FakeAterm::new(&root("typed-press"));
    let (pid, _) = me();
    let mut connection = connect(&fake);
    let on = typing_claim(
        &fake,
        &mut connection,
        &[Scope::Relaunch, Scope::Upgrade, Scope::Approvals],
    );
    let revision = fake.revision();
    let typist = Typist::new(&fake.sid, pid).expect("typist");
    fake.set_screen(box_screen());
    fake.after_key("1", composer_screen());
    let screen = typist
        .read(&mut connection)
        .expect("text")
        .expect("an OK text")
        .expect("a screen");
    assert_eq!(screen.generation.as_deref(), Some("1.100"));
    let guard = r"^\x20{1}Do\x20you\x20want\x20to\x20proceed\?\s*$";
    let press = |key, generation, guard, act| Press {
        key,
        generation,
        guard,
        act,
        human_seq: 0,
        grace: Duration::from_millis(2_500),
    };
    let yes = press("1", "1.100", guard, Some(KeyAct::Approve));
    let from = fake.verbs().len();
    assert_eq!(
        typist
            .press(&mut connection, &on, Scope::Approvals, &yes)
            .expect("press"),
        Typed::Answered(Reply::Written { seq: Some(101) })
    );
    assert_eq!(
        verbs_since(&fake, from),
        [
            "status".to_string(),
            format!(
                "key {} if-gen=1.100 if={guard} act=approve 1",
                fence_words(revision, Scope::Approvals, 0)
            ),
        ]
    );
    assert_eq!(fake.typed(), ["key act=approve 1"]);
    assert_eq!(fake.screen(), composer_screen().0);

    // The same key on the read it was decided on, after the screen moved:
    // nothing.
    assert_eq!(
        typist
            .press(&mut connection, &on, Scope::Approvals, &yes)
            .expect("stale"),
        Typed::Answered(Reply::Skipped("changed".to_string()))
    );
    // A fresh generation, and a guard no row of the screen matches: nothing.
    fake.set_screen(box_screen());
    let elsewhere = press("1", "1.101", r"^\x20Allow\?\s*$", Some(KeyAct::Approve));
    assert_eq!(
        typist
            .press(&mut connection, &on, Scope::Approvals, &elsewhere)
            .expect("unguarded"),
        Typed::Answered(Reply::Skipped("guard".to_string()))
    );
    assert_eq!(fake.typed(), ["key act=approve 1"]);

    // A focus key carries no act; the guard matches the box's row.
    let down = press("down", "1.101", guard, None);
    assert_eq!(
        typist
            .press(&mut connection, &on, Scope::Approvals, &down)
            .expect("down"),
        Typed::Answered(Reply::Written { seq: Some(102) })
    );
    assert_eq!(fake.typed(), ["key act=approve 1", "key down"]);

    // A duty the claim does not take is held before anything is sent.
    let from = fake.verbs().len();
    let Typed::Held(hold) = typist
        .press(&mut connection, &on, Scope::Questions, &down)
        .expect("questions")
    else {
        panic!("a duty the claim does not take is held");
    };
    assert_eq!(
        hold.to_string(),
        "wait aterm supervisor_scope=relaunch,upgrade,approvals"
    );
    assert_eq!(verbs_since(&fake, from), ["status"]);
    assert!(!connection.is_poisoned());
}
