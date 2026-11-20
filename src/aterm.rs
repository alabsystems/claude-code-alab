// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Pure aterm request/reply policy for local typed clients and protocol fixtures.
//!
//! The supervisor's adapter (`src/supervise_link.rs`) delegates lifecycle claims,
//! recovery and native child generations to [`alab_wire::link::Link`]. This module
//! retains request texts, reply parsers and safe-point interpretation for the
//! typed-client protocol fixtures and for the duties' deciders ([`Duties`],
//! [`Texts`], [`Pace`], [`BoxPolicy`], [`WallPolicy`]), and the judge of the
//! moment a session is moved off Fable's limit ([`judge_wall_point`]). It
//! performs no I/O; [`crate::aterm_wire`] supplies their bounded transport.
//! Runtime lifecycle judgement uses the shared core.
//!
//! The rule a client of this boundary keeps:
//!
//! * **Never a trial claim.** aterm stores a contract-3 claim from a holder that
//!   is not on its roster as an ordinary foreign claim, and a stored foreign
//!   claim takes every duty (`aterm/crates/aterm-agent/src/harness/contract.rs`,
//!   `Words::Foreign`), held by nobody who performs them. So the harness claims
//!   only after a read-only `native-act info` reply names its row
//!   ([`rostered`]).
//! * **Claim only under a contract the host serves.** The claim is
//!   [`claim_verb`]'s text, sent only after `harness policy` advertises
//!   contract 3 and reads `effective=on` ([`Policy`]); where the policy also
//!   serves contract 4 the claim is made under it and takes the account moves
//!   too ([`claim_scopes`], [`Scope::Accounts`]).
//! * **A claim stands only when `status` proves it.** aterm proves a claim by
//!   kernel peer pid, the claimant leading the terminal's foreground process
//!   group, and its image sitting in a harness package. The harness reads that
//!   back as `supervisor=<holder> supervisor_proven=1` ([`Status::held_by`]) and
//!   releases a claim that does not read so ([`release_verb`]).
//! * **Fail closed.** A field a judgement needs that is absent or malformed
//!   holds; no parser here supplies a default.
//! * **Type only through the fence.** Every `send`, `key` and `turn` the
//!   harness sends carries the claim's holder, its `supervisor_revision` and
//!   the typing duty it performs, with the person's count it was judged at
//!   (`if-supervisor=`, `if-supervisor-revision=`, `harness-duty=`,
//!   `if-human=`; [`crate::typing`]), so a write after the claim moved can
//!   never land. Nothing here types; the claim's typing duties, `turn-end`,
//!   `approvals`, `questions` and `walls`, type through that client
//!   ([`Duties::BUILT`]).

use std::fmt;
use std::time::Duration;

pub use alab_policy::config::Autopilot;
pub(crate) use alab_wire::claim::journal_word;
pub use alab_wire::claim::{info_verb, policy_verb, status_verb};

use crate::aterm_wire::{Fields, percent_decode, session_id};

/// The aterm control contract this harness requires before it claims anything.
pub const REQUIRED_CONTRACT: u32 = 3;

/// The contract a claim that takes [`Scope::Accounts`] is made under: aterm
/// knows the word only there (`contract.rs`, `Scope::since`, `read_v4`), and
/// a contract-3 claim naming it is refused whole. A claim names it only where
/// the policy serves this contract too ([`claim_scopes`]).
pub const ACCOUNTS_CONTRACT: u32 = 4;

/// The required contract when an `OK` policy reply advertises it in its contract set
/// (`OK gen=… effective=… contract=2,3 …`). An absent, malformed or duplicate field,
/// or a set without the required member, returns `None`.
#[must_use]
pub fn advertised_contract(policy_reply: &str) -> Option<u32> {
    let mut words = policy_reply.split_ascii_whitespace();
    if words.next() != Some("OK") {
        return None;
    }
    let mut advertised = None;
    for word in words {
        if let Some(value) = word.strip_prefix("contract=") {
            if advertised.is_some() {
                return None;
            }
            advertised = Some(value);
        }
    }
    contract_set(advertised?)?
        .contains(&REQUIRED_CONTRACT)
        .then_some(REQUIRED_CONTRACT)
}

/// This harness's row on aterm's roster: the holder id, the atpkg package and
/// the repository are one word (`aterm/crates/aterm-agent/src/harness/contract.rs`,
/// `Row::id`).
pub const HOLDER_ID: &str = "claude-code-alab";

/// The longest `version=` a claim may carry (`contract.rs`, `VERSION_MAX`).
const VERSION_MAX: usize = 128;
/// The longest `vendor=` a claim may carry (`contract.rs`, `VENDOR_MAX`).
const VENDOR_MAX: usize = 64;
/// The longest value a journal line repeats from a reply.
const JOURNAL_WORD_MAX: usize = 64;

/// One duty a claim takes, in aterm's canonical order (`contract.rs`,
/// `Scope`): the six a contract-3 claim names
/// ([`Scope::ALL`]), and [`Scope::Accounts`], which only a contract-4 claim
/// names ([`ACCOUNTS_CONTRACT`]). A claim names only duties a build performs
/// ([`Duties`], and [`claim_scopes`] for `accounts`); every typing duty is
/// named here so that a typed write can say which one it performs
/// ([`crate::typing`]). aterm's `state` word is not: this harness reports no
/// state stream.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Scope {
    /// Relaunching the child on the same conversation after a crash.
    Relaunch,
    /// Moving the child onto another vendor build at a safe point.
    Upgrade,
    /// Continuations, Cruise, Land, NEEDS YOU, limit and usage resumes.
    TurnEnd,
    /// Permission boxes, folder trust, plan boxes.
    Approvals,
    /// Question dialogs and prose questions.
    Questions,
    /// Usage, context, login and API-retry walls.
    Walls,
    /// Moving the child onto another of the person's accounts when aterm asks
    /// (`restart cause=account to=<key> home=<dir>`). aterm alone chooses the
    /// account and when; this harness asks for none. A contract-4 word that
    /// types nothing; it is never one of [`Duties`].
    Accounts,
}

impl Scope {
    /// Every duty a contract-3 claim may name, in aterm's order
    /// (`contract.rs`, `Scope::ALL` up to `walls`): every one but
    /// [`Scope::Accounts`].
    pub const ALL: [Self; 6] = [
        Self::Relaunch,
        Self::Upgrade,
        Self::TurnEnd,
        Self::Approvals,
        Self::Questions,
        Self::Walls,
    ];

    /// The wire word (`contract.rs`, `Scope::word`).
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Self::Relaunch => "relaunch",
            Self::Upgrade => "upgrade",
            Self::TurnEnd => "turn-end",
            Self::Approvals => "approvals",
            Self::Questions => "questions",
            Self::Walls => "walls",
            Self::Accounts => "accounts",
        }
    }

    /// The duty a wire word names exactly (`contract.rs`, `Scope::from_word`),
    /// [`Scope::Accounts`] included.
    #[must_use]
    pub fn from_word(word: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .chain([Self::Accounts])
            .find(|scope| scope.word() == word)
    }

    /// Whether a typed write may name this duty. aterm admits typed supervisor
    /// input only for `turn-end`, `approvals`, `questions` and `walls`;
    /// `relaunch` and `upgrade` grant no typing
    /// (`aterm/crates/aterm-gui/src/session_timeline.rs`,
    /// `SupervisorInputFence::authorize`).
    #[must_use]
    pub const fn types(self) -> bool {
        matches!(
            self,
            Self::TurnEnd | Self::Approvals | Self::Questions | Self::Walls
        )
    }

    /// This duty's bit in a [`Duties`] set.
    const fn bit(self) -> u8 {
        1 << self as u8
    }
}

/// The duties this harness claims, a set of those it performs
/// ([`Duties::BUILT`]) that always holds the two that type nothing
/// ([`Duties::ALWAYS`]). Each typing duty is switched on or off by itself
/// (`--alab-duties=`, [`crate::cli::Options::duties`]): one left out is
/// performed by nothing in the session, since aterm keeps no Claude Code
/// duty of its own.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Duties(u8);

impl Duties {
    /// Every duty this build performs, the claim a launch makes unless told
    /// otherwise: bringing a crashed child back (`relaunch`), moving it onto
    /// another vendor build (`upgrade`), Autopilot at its turn ends
    /// (`turn-end`, [`crate::duty`]), its permission boxes (`approvals`) and
    /// question dialogs (`questions`, both [`crate::boxes`]), and the walls its
    /// turns end on (`walls`, [`crate::walls`]): every duty aterm names.
    pub const BUILT: Self = Self(
        Scope::Relaunch.bit()
            | Scope::Upgrade.bit()
            | Scope::TurnEnd.bit()
            | Scope::Approvals.bit()
            | Scope::Questions.bit()
            | Scope::Walls.bit(),
    );

    /// The duties every set holds, whatever `--alab-duties=` names: bringing a
    /// crashed child back (`relaunch`, under which the move off Fable's limit
    /// and back is made too, [`judge_wall_point`]) and moving it onto another
    /// vendor build (`upgrade`). Neither types, so every claim names both,
    /// whatever word it is made under ([`Duties::under`]); one left out would
    /// be nobody's. A claim stands only where aterm's policy is in effect,
    /// which is under Cruise and Land: under Off the policy reads
    /// `effective=off why=autopilot-off` and nothing is claimed, so no crash
    /// is brought back (the session ends with its child, as outside aterm),
    /// while the vendor update and the move off Fable's limit go on unclaimed,
    /// on the harness's own readings.
    pub const ALWAYS: Self = Self(Scope::Relaunch.bit() | Scope::Upgrade.bit());

    /// The set `bits` names ([`Duties::bits`]), with [`Duties::ALWAYS`]
    /// added: `None` for an empty set or one that names a duty this build
    /// does not perform.
    #[must_use]
    pub const fn from_bits(bits: u8) -> Option<Self> {
        if bits == 0 || bits & !Self::BUILT.0 != 0 {
            None
        } else {
            Some(Self(bits | Self::ALWAYS.0))
        }
    }

    /// The set as one byte, each duty at [`Scope`]'s place in aterm's order.
    #[must_use]
    pub const fn bits(self) -> u8 {
        self.0
    }

    /// Whether the set holds `scope`.
    #[must_use]
    pub const fn takes(self, scope: Scope) -> bool {
        self.0 & scope.bit() != 0
    }

    /// The duties in aterm's canonical order, as a claim names them.
    #[must_use]
    pub fn scopes(self) -> Vec<Scope> {
        Scope::ALL
            .into_iter()
            .filter(|scope| self.takes(*scope))
            .collect()
    }

    /// Whether the set holds a duty that types ([`Scope::types`]): one a
    /// claim names only while the session's Autopilot types
    /// ([`Duties::under`]).
    #[must_use]
    pub fn types(self) -> bool {
        self.scopes().into_iter().any(Scope::types)
    }

    /// The duties of this set a claim names while `status` states the
    /// session's Autopilot as `autopilot`: every one under Cruise or Land,
    /// whose turn ends, boxes and walls the harness then types into; under
    /// `-` (aterm admits no agent in the session yet) or no word at all (a
    /// launch's first claim, made before its mode is read), only those that
    /// type nothing ([`Duties::ALWAYS`]), so the claim never names a duty the
    /// harness would not perform, and it is made again with the typing duties
    /// once a read states a word that types ([`Duties::types`]). Never empty:
    /// the vendor update, a crash's relaunch and the move off Fable's limit
    /// are named in every claim. Under `off` this is only words: aterm's
    /// policy is not in effect there (`why=autopilot-off`), so no claim is
    /// made, and one that stands is given back.
    #[must_use]
    pub fn under(self, autopilot: Option<Autopilot>) -> Vec<Scope> {
        let typing = matches!(autopilot, Some(Autopilot::Cruise | Autopilot::Land));
        self.scopes()
            .into_iter()
            .filter(|scope| typing || !scope.types())
            .collect()
    }

    /// The duties as a claim spells them: `relaunch,upgrade,turn-end,approvals`.
    #[must_use]
    pub fn words(self) -> String {
        self.scopes()
            .into_iter()
            .map(Scope::word)
            .collect::<Vec<_>>()
            .join(",")
    }

    /// The set a comma-separated list of duty words names, as
    /// `--alab-duties=` takes it, with [`Duties::ALWAYS`] added: each word
    /// aterm's own spelling of a duty this build performs, none empty; a word
    /// given twice is the same duty, and `relaunch` or `upgrade` names one the
    /// set holds anyway. `None` for anything else, and for an empty list.
    #[must_use]
    pub fn parse(words: &str) -> Option<Self> {
        let mut bits = 0;
        for word in words.split(',') {
            let scope = Scope::from_word(word).filter(|scope| Self::BUILT.takes(*scope))?;
            bits |= scope.bit();
        }
        Self::from_bits(bits)
    }
}

impl Default for Duties {
    fn default() -> Self {
        Self::BUILT
    }
}

/// The claim holder for harness process `pid`: `claude-code-alab@<pid>`. aterm
/// reads the pid back as canonical decimal above 1 and proves it against the
/// kernel's peer pid (`contract.rs`, `holder_in`).
#[must_use]
pub fn holder(pid: u32) -> String {
    format!("{HOLDER_ID}@{pid}")
}

/// Whether aterm would read `text` as a claim's `version=`: `MAJOR.MINOR.0`, each
/// part canonical decimal, then optionally `+` and dot-separated identifiers of
/// ASCII alphanumerics and `-` (`contract.rs`, `ClaimVersion::parse`). This is
/// the shape of [`crate::cli::VERSION`], build stamp included.
fn claim_version(text: &str) -> bool {
    if text.len() > VERSION_MAX {
        return false;
    }
    let (core, meta) = match text.split_once('+') {
        Some((core, meta)) => (core, Some(meta)),
        None => (text, None),
    };
    if meta.is_some_and(|meta| {
        meta.split('.').any(|id| {
            id.is_empty()
                || !id
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        })
    }) {
        return false;
    }
    let number = |part: &str| {
        !part.is_empty()
            && part.bytes().all(|byte| byte.is_ascii_digit())
            && (part == "0" || !part.starts_with('0'))
            && part.parse::<u32>().is_ok()
    };
    let mut parts = core.split('.');
    matches!(
        (parts.next(), parts.next(), parts.next(), parts.next()),
        (Some(major), Some(minor), Some("0"), None) if number(major) && number(minor)
    )
}

/// `version` when aterm would read it as a claim's `vendor=`: 1 to 64 bytes of
/// ASCII alphanumerics and `.`, `-`, `+`, `_` (`contract.rs`, `vendor_ok`).
fn vendor_word(version: Option<&str>) -> Option<&str> {
    version.filter(|version| {
        !version.is_empty()
            && version.len() <= VENDOR_MAX
            && version.bytes().all(|byte| {
                byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'+' | b'_')
            })
    })
}

/// `@<sid> <verb>`, when `sid` is a session ID.
fn addressed(sid: &str, verb: &str) -> Option<String> {
    session_id(sid).then(|| format!("@{sid} {verb}"))
}

/// The claim for session `sid` by harness process `pid`:
///
/// `@<sid> meta set supervisor claude-code-alab@<pid> contract=<3|4> scope=<words> version=<version>[ vendor=<vendor>]`
///
/// `scopes` are spelled once each in aterm's canonical order, comma-joined.
/// The contract is 3, or [`ACCOUNTS_CONTRACT`] where `scopes` take
/// [`Scope::Accounts`], a word contract 3 refuses; contract 4 requires the
/// `version=` every claim here carries. `version` is this harness's own
/// ([`crate::cli::VERSION`]); `vendor` is the vendor build the child runs, and
/// is left out when it is unknown or is not a word aterm would accept, because
/// aterm refuses the whole claim over one bad word (`contract.rs`, `read_in`).
/// The account the child runs on, `login=`, is added by [`with_login`].
///
/// `None`, and nothing may be sent, when `sid` is not a session ID, `pid` is not
/// above 1, `scopes` is empty, or `version` is not `MAJOR.MINOR.0[+meta]`.
///
/// The text alone claims nothing, and sending it is permitted only after
/// [`rostered`] and [`Policy::admits_claim`] both hold.
#[must_use]
pub fn claim_verb(
    sid: &str,
    pid: u32,
    scopes: &[Scope],
    version: &str,
    vendor: Option<&str>,
) -> Option<String> {
    if pid <= 1 || scopes.is_empty() || !claim_version(version) {
        return None;
    }
    let mut scopes = scopes.to_vec();
    scopes.sort_unstable();
    scopes.dedup();
    let scope = scopes
        .iter()
        .map(|scope| scope.word())
        .collect::<Vec<_>>()
        .join(",");
    let contract = claim_contract(&scopes);
    let mut verb = format!(
        "meta set supervisor {} contract={contract} scope={scope} version={version}",
        holder(pid)
    );
    if let Some(vendor) = vendor_word(vendor) {
        verb.push_str(" vendor=");
        verb.push_str(vendor);
    }
    addressed(sid, &verb)
}

/// The contract a claim naming `scopes` is made under: [`ACCOUNTS_CONTRACT`]
/// where it takes [`Scope::Accounts`], [`REQUIRED_CONTRACT`] otherwise.
#[must_use]
pub fn claim_contract(scopes: &[Scope]) -> u32 {
    if scopes.contains(&Scope::Accounts) {
        ACCOUNTS_CONTRACT
    } else {
        REQUIRED_CONTRACT
    }
}

/// The duties a claim names under `policy`: the launch's `duties`, and
/// [`Scope::Accounts`] where the policy serves [`ACCOUNTS_CONTRACT`]. A host
/// that serves contract 3 alone gets the contract-3 claim it always had, and
/// a host that serves 4 a contract-4 claim that also takes the account moves
/// (docs/DECISIONS.md 51). Whatever `duties` say of `accounts` is not read:
/// the policy decides it afresh on every claim.
#[must_use]
pub fn claim_scopes(policy: &Policy, duties: &[Scope]) -> Vec<Scope> {
    let mut scopes: Vec<Scope> = duties
        .iter()
        .copied()
        .filter(|scope| *scope != Scope::Accounts)
        .collect();
    if policy.contracts.contains(&ACCOUNTS_CONTRACT) {
        scopes.push(Scope::Accounts);
    }
    scopes.sort_unstable();
    scopes.dedup();
    scopes
}

/// Whether aterm would take `label` as an account label, in a claim's
/// `login=` or a restart's `to=`: 1 to 64 bytes, the first an ASCII lowercase
/// letter or digit, the rest those or `.`, `_`, `-`, and never `help`
/// (`aterm/crates/aterm-agent/src/harness/accounts.rs`, `admissible_label`).
#[must_use]
pub fn account_label(label: &str) -> bool {
    let mut bytes = label.bytes();
    let first = |byte: u8| byte.is_ascii_lowercase() || byte.is_ascii_digit();
    label.len() <= 64
        && label != "help"
        && bytes.next().is_some_and(first)
        && bytes.all(|byte| first(byte) || matches!(byte, b'.' | b'_' | b'-'))
}

/// Whether `word` is an account key, the one identity of an account on the
/// wire (aterm's `aterm_file::account_key::AccountKey`): `ac-` and 16 lower
/// hex digits. A key is an [`account_label`] by its grammar, so it rides a
/// restart's `to=` and a claim's `login=` unchanged; a roster label, a slug
/// or an address is none, and this harness moves a session only onto a key
/// (`Supervisor::restart_asked` in [`crate::supervise`]).
#[must_use]
pub fn account_key(word: &str) -> bool {
    word.strip_prefix("ac-").is_some_and(|hex| {
        hex.len() == 16
            && hex
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    })
}

/// `verb`, a claim, with ` login=<label>` added where the child runs on the
/// account `login` names: aterm reads the word as the account the claim's
/// child last started on, and waits for it after an account move it asked for
/// (`restart cause=account`). `None` where `verb` is, or where `login` is no
/// [`account_label`], since aterm would refuse the whole claim over it.
#[must_use]
pub fn with_login(verb: Option<String>, login: Option<&str>) -> Option<String> {
    let mut verb = verb?;
    if let Some(label) = login {
        if !account_label(label) {
            return None;
        }
        verb.push_str(" login=");
        verb.push_str(label);
    }
    Some(verb)
}

/// What aterm confirmed of the agent image in the session, which decides
/// whether it types a write into it, as `status supervisor_act=` says it
/// (`aterm/crates/aterm-agent/src/harness/agent_image.rs`, `Act`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EngineAct {
    /// `on`: aterm confirmed the agent's running image, and types the writes
    /// fenced on it.
    On,
    /// `observe`: aterm confirmed no agent image, and types no write. In a tab
    /// this harness leads, that is the tab without a kept child: the image
    /// aterm then reads is the harness's own.
    Observe,
    /// `-`: aterm admits no agent in the session, or has read no image yet.
    Unread,
}

/// What aterm confirmed of the agent image in one `status` reply: its one
/// `supervisor_act=` word.
/// `None` for a reply that is not `OK`, or that states the word twice, not at
/// all, or as a word not known here.
#[must_use]
pub fn engine_act(status_reply: &str) -> Option<EngineAct> {
    let mut words = status_reply.split_ascii_whitespace();
    if words.next() != Some("OK") {
        return None;
    }
    let mut values = words.filter_map(|word| word.strip_prefix("supervisor_act="));
    let value = values.next()?;
    if values.next().is_some() {
        return None;
    }
    Some(match value {
        "on" => EngineAct::On,
        "observe" => EngineAct::Observe,
        "-" => EngineAct::Unread,
        _ => return None,
    })
}

#[cfg(test)]
#[path = "aterm_duties_tests.rs"]
mod duties_tests;

#[cfg(test)]
#[path = "aterm_restart_tests.rs"]
mod restart_tests;

#[cfg(test)]
#[path = "aterm_account_tests.rs"]
mod account_tests;

#[cfg(test)]
#[path = "aterm_act_tests.rs"]
mod act_tests;

/// The release of process `pid`'s claim on session `sid`:
/// `@<sid> meta unset supervisor holder=claude-code-alab@<pid>`. Naming the
/// holder makes it a no-op against anyone else's claim
/// (`aterm/crates/aterm-gui/src/control_session.rs`, `cmd_meta_supervisor_serving`).
/// `None` when `sid` is not a session ID or `pid` is not above 1.
#[must_use]
pub fn release_verb(sid: &str, pid: u32) -> Option<String> {
    if pid <= 1 {
        return None;
    }
    addressed(
        sid,
        &format!("meta unset supervisor holder={}", holder(pid)),
    )
}

/// The roster guard: whether a `native-act info` reply names this harness's row
/// with the `upgrade` duty.
///
/// The reply is `OK native-act state=info … own-image=<id>:upgrade[,<id>:upgrade…]`,
/// or `own-image=-` when no row allows it
/// (`aterm/crates/aterm-gui/src/native_act.rs`, `own_image_rows` and
/// `Request::Info`). It carries the bare word `native-act`, so it is read
/// leniently; the one field that matters must be there exactly once.
///
/// `false` for anything else: an `ERR`, a host that does not know the verb, a
/// missing or repeated `own-image=`, or a list without the exact member
/// `claude-code-alab:upgrade`. A harness that is not rostered must never send a
/// claim, not even to find out: aterm would store it as a foreign claim that
/// takes every duty until it is released.
#[must_use]
pub fn rostered(info_reply: &str) -> bool {
    let mut words = info_reply.split_ascii_whitespace();
    if words.next() != Some("OK") {
        return false;
    }
    let mut rows = None;
    for word in words {
        if let Some(value) = word.strip_prefix("own-image=")
            && rows.replace(value).is_some()
        {
            return false;
        }
    }
    let wanted = (HOLDER_ID, Scope::Upgrade.word());
    rows.is_some_and(|rows| {
        rows.split(',')
            .any(|row| row.split_once(':') == Some(wanted))
    })
}

/// Why a reply could not be read as what was asked for. Its text is one journal
/// word group, as in `aterm unclaimed <why>`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ReplyError {
    /// The reply is not an `OK` line; its first two words, made journal-safe.
    Refused(String),
    /// The reply is not an `OK` line, and it asks to be retried: aterm could
    /// not read its own state now (`ERR aterm is showing a dialog; answer it,
    /// then retry`, `ERR main thread stalled 6s; retry`;
    /// `aterm/crates/aterm-gui/src/control_media.rs`, `main_thread_refusal`).
    /// Its first two words, made journal-safe.
    Retry(String),
    /// A field the reader needs is absent.
    Missing(&'static str),
    /// A field the reader needs is not in its grammar.
    Malformed(&'static str),
    /// A field the reader needs appears more than once, so it has no one value.
    Repeated(&'static str),
}

impl ReplyError {
    /// A reply that is not an `OK` line: [`ReplyError::Retry`] when its last
    /// word is `retry`, which is how aterm words a refusal that stands for now
    /// only, else [`ReplyError::Refused`].
    #[must_use]
    pub fn refused(reply: &str) -> Self {
        let words: Vec<String> = reply
            .split_ascii_whitespace()
            .take(2)
            .map(journal_word)
            .collect();
        let words = if words.is_empty() {
            "(empty)".to_string()
        } else {
            words.join(":")
        };
        if reply.split_ascii_whitespace().next_back() == Some("retry") {
            Self::Retry(words)
        } else {
            Self::Refused(words)
        }
    }
}

impl fmt::Display for ReplyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Refused(words) | Self::Retry(words) => write!(f, "reply={words}"),
            Self::Missing(key) => write!(f, "{key}=absent"),
            Self::Malformed(key) => write!(f, "{key}=malformed"),
            Self::Repeated(key) => write!(f, "{key}=repeated"),
        }
    }
}

impl std::error::Error for ReplyError {}

/// The members of a `contract=` value: canonical nonzero decimals, comma-joined.
fn contract_set(value: &str) -> Option<Vec<u32>> {
    value
        .split(',')
        .map(|member| {
            if member.is_empty()
                || member.starts_with('0')
                || !member.bytes().all(|byte| byte.is_ascii_digit())
            {
                return None;
            }
            member.parse::<u32>().ok()
        })
        .collect()
}

fn canonical_u64(value: &str) -> Option<u64> {
    if value.is_empty()
        || (value.len() > 1 && value.starts_with('0'))
        || !value.bytes().all(|byte| byte.is_ascii_digit())
    {
        return None;
    }
    value.parse().ok()
}

/// The parts of a `harness policy` reply a claim holder acts on
/// (`aterm/crates/aterm-gui/src/harness_policy_verb.rs`):
///
/// `OK gen=<n> effective=<on|off> why=<word> contract=2,3 … relaunch=<bool> … upgrade=<bool> upgrade_ask=<bool> human_grace_s=<n> …`
///
/// The reader is strict about these seven fields, reads `why=` when it is there,
/// and is blind to every other word: the reply carries dozens of keys this
/// harness has no use for and aterm adds more.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Policy {
    /// `gen=`: the policy cell's generation, which a `harness-policy` event moves.
    pub generation: u64,
    /// `contract=`: every contract the host serves.
    pub contracts: Vec<u32>,
    /// `effective=on`: aterm's harness policy is in effect for this session.
    pub effective: bool,
    /// `why=`: the first reason the policy is off for the session (`enabled`
    /// while it is on), as one journal word; `None` when aterm did not say.
    pub why: Option<String>,
    /// `[harness] relaunch`: a crashed agent may be brought back.
    pub relaunch: bool,
    /// `[harness] upgrade`: an agent may be moved onto another build.
    pub upgrade: bool,
    /// `[harness] upgrade_ask`: a person says when; nothing moves by itself.
    pub upgrade_ask: bool,
    /// `[harness] human_grace_s`: how long after a person's last key the session
    /// is still theirs.
    pub human_grace: Duration,
}

impl Policy {
    /// Parse one `harness policy` reply.
    ///
    /// # Errors
    /// The reply is not an `OK` line, or one of `gen=`, `contract=`,
    /// `effective=`, `relaunch=`, `upgrade=`, `upgrade_ask=` and `human_grace_s=`
    /// is absent, repeated or outside its grammar. Nothing is defaulted: a host
    /// that does not say whether it permits a move has not permitted it. The
    /// short `OK effective=<on|off>` reply aterm gives a lesser principal is
    /// such an error. `why=` may be absent; repeated, it is an error too.
    pub fn parse<'a>(reply: &'a str) -> Result<Self, ReplyError> {
        const KEYS: [&str; 7] = [
            "gen",
            "contract",
            "effective",
            "relaunch",
            "upgrade",
            "upgrade_ask",
            "human_grace_s",
        ];
        let mut words = reply.split_ascii_whitespace();
        if words.next() != Some("OK") {
            return Err(ReplyError::refused(reply));
        }
        let mut values: [Option<&str>; 7] = [None; 7];
        let mut why = None;
        for word in words {
            let Some((key, value)) = word.split_once('=') else {
                continue;
            };
            if key == "why" && why.replace(value).is_some() {
                return Err(ReplyError::Repeated("why"));
            }
            if let Some((known, slot)) = KEYS
                .iter()
                .zip(values.iter_mut())
                .find(|(known, _)| **known == key)
                && slot.replace(value).is_some()
            {
                return Err(ReplyError::Repeated(known));
            }
        }
        let value =
            |found: Option<&'a str>, key: &'static str| found.ok_or(ReplyError::Missing(key));
        let switch = |found: Option<&'a str>, key: &'static str| match value(found, key)? {
            "true" => Ok(true),
            "false" => Ok(false),
            _ => Err(ReplyError::Malformed(key)),
        };
        let [
            generation,
            contracts,
            effective,
            relaunch,
            upgrade,
            upgrade_ask,
            human_grace,
        ] = values;
        let [
            generation_key,
            contracts_key,
            effective_key,
            relaunch_key,
            upgrade_key,
            upgrade_ask_key,
            human_grace_key,
        ] = KEYS;
        Ok(Self {
            generation: canonical_u64(value(generation, generation_key)?)
                .ok_or(ReplyError::Malformed(generation_key))?,
            contracts: contract_set(value(contracts, contracts_key)?)
                .ok_or(ReplyError::Malformed(contracts_key))?,
            effective: match value(effective, effective_key)? {
                "on" => true,
                "off" => false,
                _ => return Err(ReplyError::Malformed(effective_key)),
            },
            why: why.map(journal_word),
            relaunch: switch(relaunch, relaunch_key)?,
            upgrade: switch(upgrade, upgrade_key)?,
            upgrade_ask: switch(upgrade_ask, upgrade_ask_key)?,
            human_grace: canonical_u64(value(human_grace, human_grace_key)?)
                .map(Duration::from_secs)
                .ok_or(ReplyError::Malformed(human_grace_key))?,
        })
    }

    /// Whether the host serves the contract this harness claims under.
    #[must_use]
    pub fn serves_contract(&self) -> bool {
        self.contracts.contains(&REQUIRED_CONTRACT)
    }

    /// Whether this policy lets a rostered harness claim at all: contract 3 is
    /// served and the policy is in effect for the session. It says nothing about
    /// any one duty; [`judge_safe_point`] and [`Policy::relaunch`] do.
    #[must_use]
    pub fn admits_claim(&self) -> bool {
        self.serves_contract() && self.effective
    }

    /// Whether the policy is off for the session by configuration: aterm has
    /// no harness host (`unhosted`), its supervisor is off (`disabled`), off
    /// for a headless instance (`headless`) or for this agent (`agent-off`),
    /// or the session's own Autopilot is off (`autopilot-off`)
    /// (`aterm/crates/aterm-gui/src/harness_policy_verb.rs`, `effective`).
    /// The other reasons pass on their own: a halt on the session (`hold`)
    /// and aterm suspended for its own update (`suspended`); so does a reason
    /// this harness does not know, or none given.
    #[must_use]
    pub fn off_by_configuration(&self) -> bool {
        !self.effective
            && matches!(
                self.why.as_deref(),
                Some("unhosted" | "disabled" | "headless" | "agent-off" | "autopilot-off")
            )
    }

    /// `effective=off`, with `why=<word>` when aterm said why: the journal
    /// words of a policy that is off.
    #[must_use]
    pub fn off_words(&self) -> String {
        self.why.as_deref().map_or_else(
            || "effective=off".to_string(),
            |why| format!("effective=off why={why}"),
        )
    }
}

/// When a person last typed, as `status human_ms=` reports it
/// (`aterm/crates/aterm-agent/src/supervise/screen.rs`, `HumanInput::of_status`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Human {
    /// `human_ms=-`: nobody has typed since aterm began serving the session.
    Never,
    /// The last key was this long before the read.
    Ago(Duration),
    /// The field is absent or is not a number: nothing is known, which holds.
    Unknown,
}

/// The parts of a `status` reply that witness a claim and a safe point.
///
/// `status` is a line of some two hundred fields that aterm extends freely, so
/// it is read leniently ([`Fields::parse_lenient`]): the last word for a key
/// wins and a bare word is skipped. Leniency ends there. Every field is kept as
/// aterm sent it, or `None` when it did not, and [`judge_safe_point`] holds on
/// each one it cannot read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Status {
    /// `supervisor=`: the standing claim's holder, percent-decoded. `None` for
    /// `-` (no claim), an absent field, or a value that does not decode.
    pub supervisor: Option<String>,
    /// `supervisor_proven=1`: aterm proved the standing claim. Anything else,
    /// absence included, is unproven.
    pub supervisor_proven: bool,
    /// `supervisor_scope=`: the duty words the standing claim takes, as sent.
    /// Empty for `-` or an absent field. A claim without `contract=` lists every
    /// duty (`aterm/crates/aterm-gui/src/session_timeline.rs`,
    /// `SupervisorAuthority::wire_fields`).
    pub supervisor_scope: Vec<String>,
    /// `supervisor_revision=`: the claim cell's revision, which moves with every
    /// claim, release and replacement. A typed write names it
    /// (`if-supervisor-revision=`), and aterm refuses one it no longer is.
    pub supervisor_revision: Option<u64>,
    /// `agent=`: the agent's phase as aterm's reader sees it (`idle` at rest).
    pub agent: Option<String>,
    /// `composer=`: what the agent's input box holds (`empty`, `placeholder`, or
    /// a draft).
    pub composer: Option<String>,
    /// `human_ms=`.
    pub human: Human,
    /// `human_seq=`: how many gestures a person has made in the session. A typed
    /// write names the count it was judged at (`if-human=`), and aterm skips one
    /// a gesture has moved past (`aterm/crates/aterm-gui/src/control_input.rs`,
    /// `LeadingInputOptions::if_human`). `None` when absent or not canonical
    /// decimal.
    pub human_seq: Option<u64>,
    /// `hand=`: who drives the session (`-` for nobody).
    pub hand: Option<String>,
    /// `hold=`: `1` while a halt stands on the session.
    pub hold: Option<String>,
    /// `input=`: `clear` once the session's program has read everything typed.
    pub input: Option<String>,
    /// `need=`: what the session waits on a person for (`-` for nothing).
    pub need: Option<String>,
    /// `autopilot=`: how far Autopilot carries this session's turn ends, the
    /// session's own word over the policy's default, and `off` wherever the
    /// policy is off for it (`aterm/crates/aterm-gui/src/autopilot.rs`,
    /// `wire_field`). `None` for `-` (aterm admits no agent in the session), an
    /// absent field, or a word not known here: the turn-end duty then types
    /// nothing.
    pub autopilot: Option<Autopilot>,
    /// Whether the reply carried `autopilot=` at all, `-` included: a host
    /// that states the field names a mode once it admits an agent in the
    /// session, and one that never states it (an older aterm) never will.
    pub autopilot_stated: bool,
}

impl Status {
    /// Parse one schema-1 `status` reply.
    ///
    /// # Errors
    /// The reply is not an `OK` line, or its `schema=` is absent or its major
    /// version is not 1. Every other field may be absent here and is judged
    /// where it is needed.
    pub fn parse(reply: &str) -> Result<Self, ReplyError> {
        let fields = Fields::parse_lenient(reply).map_err(|_| ReplyError::refused(reply))?;
        let schema = fields.get("schema").ok_or(ReplyError::Missing("schema"))?;
        if schema.split('.').next() != Some("1") {
            return Err(ReplyError::Malformed("schema"));
        }
        let word = |key: &str| fields.get(key).map(str::to_string);
        Ok(Self {
            supervisor: fields
                .get("supervisor")
                .filter(|value| *value != "-" && !value.is_empty())
                .and_then(|value| percent_decode(value).ok()),
            supervisor_proven: fields.get("supervisor_proven") == Some("1"),
            supervisor_scope: fields
                .get("supervisor_scope")
                .filter(|value| *value != "-")
                .map(|value| {
                    value
                        .split(',')
                        .filter(|word| !word.is_empty())
                        .map(str::to_string)
                        .collect()
                })
                .unwrap_or_default(),
            supervisor_revision: fields.get("supervisor_revision").and_then(canonical_u64),
            agent: word("agent"),
            composer: word("composer"),
            human: match fields.get("human_ms") {
                Some("-") => Human::Never,
                Some(value) => canonical_u64(value)
                    .map_or(Human::Unknown, |ms| Human::Ago(Duration::from_millis(ms))),
                None => Human::Unknown,
            },
            human_seq: fields.get("human_seq").and_then(canonical_u64),
            hand: word("hand"),
            hold: word("hold"),
            input: word("input"),
            need: word("need"),
            // Exactly the three wire words: `parse`'s other names for a mode
            // are a person's, never aterm's.
            autopilot: fields
                .get("autopilot")
                .and_then(|word| Autopilot::ALL.into_iter().find(|mode| mode.word() == word)),
            autopilot_stated: fields.get("autopilot").is_some(),
        })
    }

    /// Whether this status shows `holder`'s claim standing and proven.
    #[must_use]
    pub fn held_by(&self, holder: &str) -> bool {
        self.supervisor_proven && self.supervisor.as_deref() == Some(holder)
    }

    /// Whether the standing claim takes `scope`.
    #[must_use]
    pub fn takes(&self, scope: Scope) -> bool {
        self.supervisor_scope
            .iter()
            .any(|word| word == scope.word())
    }
}

/// A restart aterm asks the claim holder to make, from an
/// `EVENT <local> restart cause=<why>[ to=<key> home=<path>][ model=<word>]`
/// line (`aterm/crates/aterm-gui/src/restart_act.rs`, `record_restart` and
/// `record_scoped_restart`). aterm sends it instead of signalling a harness
/// whose proven claim takes `relaunch`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Restart {
    /// Why: `asked` (a person's `aterm ctl restart`), `stall` (the stall's
    /// remedy), `model` (aterm moves the child onto another model), one of
    /// [`ACCOUNT_CAUSES`] (aterm moves the child onto another account, which
    /// [`Restart::account`] names), or a bare cause, which names neither a
    /// model nor an account: `memory` (aterm's memory restart) and
    /// `input-lost` (a line it wrote that the child never drew), the two of
    /// its own loop (`restart_act.rs`, `BARE_CAUSES`), and `route`, the word
    /// aterm's HX-58 tells a session whose route must change (the vault's
    /// token for a stored login or back; aterm's token helper judges the
    /// route when it starts the child again). The word is free
    /// ([`Restart::parse`]): a cause not named here is read, and made as a
    /// restart of the child as it is, which is how `route` is obeyed whether
    /// or not the aterm beside the harness sends it yet.
    pub cause: String,
    /// `model=<word>`: the model aterm asks the child to be resumed on
    /// ([`model_word`]), or [`MODEL_LAUNCH`], which names none and asks for
    /// the launch's own. `None` for a restart that names none: the child
    /// comes back on the model it had.
    pub model: Option<String>,
    /// `to=<key> home=<dir|->`: the account aterm moves the child onto, for
    /// a restart whose cause is one of [`ACCOUNT_CAUSES`]; `None` for every
    /// other cause, whose `to=` and `home=` are passed over.
    pub account: Option<Account>,
}

/// The causes of a restart that moves the child onto another account
/// (`aterm/crates/aterm-gui/src/harness_host.rs`, the scoped account move):
/// `account`, at a limit, and `account-ahead`, before one.
pub const ACCOUNT_CAUSES: [&str; 2] = ["account", "account-ahead"];

/// An account aterm keeps for the person, as a restart names it: its key
/// (`to=`, [`account_key`]), the one identity it has on the wire, and the
/// login directory the vendor reads its login from
/// (`CLAUDE_SECURESTORAGE_CONFIG_DIR`; aterm's `accounts::LOGIN_ENV`), or
/// `None` for the machine's default login (`home=-`). No credential is ever
/// here: the vendor is started through aterm's token helper, which serves the
/// account's token itself ([`crate::launch::vendor_command_as`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Account {
    /// The restart's `to=` word. aterm sends the account's key; the word is
    /// read by the grammar aterm holds a `to=` to ([`account_label`]), and
    /// the supervisor makes no move onto one that is no [`account_key`].
    pub key: String,
    /// The login directory, absolute, byte for byte what aterm sent; `None`
    /// for the default login.
    pub home: Option<std::path::PathBuf>,
}

impl Account {
    /// The account a restart's `to=` and percent-encoded `home=` name: `None`
    /// for a word aterm would not take as a `to=`, or a home that is not `-`
    /// and does not decode to an absolute path free of NUL. The decoded path
    /// is kept as sent, never canonicalised.
    #[must_use]
    pub fn of_words(to: &str, home: &str) -> Option<Self> {
        if !account_label(to) {
            return None;
        }
        let home = if home == "-" {
            None
        } else {
            let decoded = percent_decode(home).ok()?;
            let path = std::path::PathBuf::from(decoded);
            if !path.is_absolute() || path.as_os_str().as_encoded_bytes().contains(&0) {
                return None;
            }
            Some(path)
        };
        Some(Self {
            key: to.to_string(),
            home,
        })
    }
}

/// The reserved `model=` word of a restart: not a model, but "the model this
/// session was launched on". The holder drops any model an earlier restart
/// moved the child onto and resumes on the launch line's own model words: its
/// `--model`, or none at all where the launch named none and Claude Code's
/// settings chose. aterm sends it for the move back at a limit's reset, where
/// naming a model would pin an alias the launch never asked for.
pub const MODEL_LAUNCH: &str = "launch";

/// Whether `word` may be a restart's `model=` and so a `--model` value on the
/// vendor's command line: aterm's own rule for the event
/// (`restart_act.rs`, `model_word`), 1 to 256 bytes of ASCII alphanumerics and
/// `._-:@/[]`, never starting with `-`, which the vendor would read as an
/// option.
#[must_use]
pub fn model_word(word: &str) -> bool {
    (1..=256).contains(&word.len())
        && !word.starts_with('-')
        && word
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"._-:@/[]".contains(&byte))
}

/// Why a restart event is not acted on ([`Restart::parse`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Unacted {
    /// No `cause=<word>` first, a cause that is no such word, or a cause
    /// named again: a line nobody can name a restart from, which the event
    /// stream treats as a fault of the stream.
    Cause,
    /// A restart that names its `cause` but whose other words cannot be acted
    /// on. `why` is `model` (a `model=` that is no [`model_word`], or one
    /// given twice), `to` or `home` (given twice), or `account` (a move of one
    /// of [`ACCOUNT_CAUSES`] whose `to=` and `home=` name no [`Account`]). The
    /// holder journals it and passes it over; the stream stands.
    Words {
        /// The restart's cause, a word of the [`Restart::parse`] rule.
        cause: String,
        /// Which of its words could not be read.
        why: &'static str,
    },
}

impl Restart {
    /// Read the words after `restart` in an event line. The first must be
    /// `cause=<word>`, a word of 1 to 64 ASCII alphanumerics, `-` and `_`, and no
    /// later word may name a cause again. A later `model=<word>` names the model
    /// to resume on ([`MODEL_LAUNCH`] for the launch's own); it must be a
    /// [`model_word`] and may be given once. `to=` and `home=` may each be
    /// given once; for one of [`ACCOUNT_CAUSES`] both must be there and name
    /// an [`Account`], and for any other cause they are passed over. Any other
    /// trailing word (a key not known here) is passed over.
    ///
    /// # Errors
    /// [`Unacted::Cause`] for a restart nobody can name, and
    /// [`Unacted::Words`] for one whose model could not be put on a command
    /// line or an account move whose account cannot be read. Neither is acted
    /// on at all, so a malformed move never ends the child only to resume it
    /// unmoved.
    pub fn parse<'a>(words: impl Iterator<Item = &'a str>) -> Result<Self, Unacted> {
        let words: Vec<&'a str> = words.collect();
        let (first, rest) = words.split_first().ok_or(Unacted::Cause)?;
        let cause = first.strip_prefix("cause=").ok_or(Unacted::Cause)?;
        if cause.is_empty()
            || cause.len() > JOURNAL_WORD_MAX
            || !cause
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
            || rest.iter().any(|word| word.starts_with("cause="))
        {
            return Err(Unacted::Cause);
        }
        let unacted = |why: &'static str| Unacted::Words {
            cause: cause.to_string(),
            why,
        };
        let mut model = None;
        let mut to = None;
        let mut home = None;
        for &word in rest {
            if let Some(value) = word.strip_prefix("model=")
                && (!model_word(value) || model.replace(value).is_some())
            {
                return Err(unacted("model"));
            }
            if let Some(value) = word.strip_prefix("to=")
                && to.replace(value).is_some()
            {
                return Err(unacted("to"));
            }
            if let Some(value) = word.strip_prefix("home=")
                && home.replace(value).is_some()
            {
                return Err(unacted("home"));
            }
        }
        let account = if ACCOUNT_CAUSES.contains(&cause) {
            let read = to
                .zip(home)
                .and_then(|(to, home)| Account::of_words(to, home));
            Some(read.ok_or_else(|| unacted("account"))?)
        } else {
            None
        };
        Ok(Self {
            cause: cause.to_string(),
            model: model.map(str::to_string),
            account,
        })
    }
}

/// What Land's check was answered with, as the act this harness reports to
/// aterm's task record ([`act_verb`]), the act codex-alab reports for the
/// same rest: aterm's `acts::RULE_LANDED` and `acts::RULE_LEFT`
/// (`aterm/crates/aterm-agent/src/harness/acts.rs`), each filed under the
/// `turn-end` duty (`act_scope`), so a claim that takes `turn-end` may
/// report it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LandAct {
    /// The check was answered `LANDED` over a clean repository: `landed@v1`.
    Landed,
    /// The check was answered with a `NEEDS YOU:` line: the rest is the
    /// person's, `left-to-person@v1` ([`alab_policy::words::RULE_LEFT`]).
    Left,
}

impl LandAct {
    /// The act's word on the wire.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Self::Landed => "landed@v1",
            Self::Left => alab_policy::words::RULE_LEFT,
        }
    }
}

/// The lease a task-face act is reported under:
/// `@<sid> meta prepare harness_act`, answered `OK until_us=<n> lease_ms=<ms>`
/// ([`Lease::parse`]). aterm answers it only for the proven holder of a claim
/// that takes `turn-end` (`aterm/crates/aterm-gui/src/control_session.rs`,
/// `cmd_meta_harness_act_with_clock`). `None` when `sid` is not a session ID.
#[must_use]
pub fn act_prepare_verb(sid: &str) -> Option<String> {
    addressed(sid, "meta prepare harness_act")
}

/// The report of `act` inside the lease that ends at `until_us`:
/// `@<sid> meta set harness_act <act> until_us=<n>`. aterm refuses it once
/// the lease has run (`ERR expired harness_act`) and while its own reading
/// of the session is not at rest (`ERR busy harness_act`), so a report
/// delayed past the next turn never lands on it. `None` when `sid` is not a
/// session ID.
#[must_use]
pub fn act_verb(sid: &str, act: LandAct, until_us: u64) -> Option<String> {
    addressed(
        sid,
        &format!("meta set harness_act {} until_us={until_us}", act.word()),
    )
}

/// The longest lease aterm grants (its `HARNESS_ACT_LEASE_MS`); a reply that
/// names a longer one, or none, is not read.
pub const ACT_LEASE_MAX_MS: u64 = 5_000;

/// What one exchange of a task-face report heard ([`Lease::parse`],
/// [`Reported::parse`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Reported {
    /// aterm recorded the act.
    Recorded,
    /// aterm cannot take it now (not at rest, the lease ran out, the claim
    /// does not take `turn-end` at this moment, or another refusal that may
    /// pass): report again later; the journal words.
    Later(String),
    /// This aterm takes no such report, or answered what is not read here:
    /// nothing is reported to it; the journal words.
    Unserved(String),
}

impl Reported {
    /// A `meta set harness_act` reply: `OK` is recorded; aterm's usage, or
    /// a field or verb it does not know, is unserved; any other refusal is
    /// for later.
    #[must_use]
    pub fn parse(reply: &str) -> Self {
        if reply.split_ascii_whitespace().next() == Some("OK") {
            return Self::Recorded;
        }
        Self::refusal("act", reply)
    }

    /// A refusal of the `step` exchange: unserved where this aterm does not
    /// know the request (codex-alab's `prepare_unknown`: its usage, an
    /// unknown meta field or an unknown verb), else for later.
    fn refusal(step: &str, reply: &str) -> Self {
        let words = format!("{step} {}", ReplyError::refused(reply));
        if reply.starts_with("ERR usage")
            || reply.starts_with("ERR unknown meta field")
            || reply.starts_with("ERR unknown verb")
        {
            Self::Unserved(words)
        } else {
            Self::Later(words)
        }
    }
}

/// The lease a [`act_prepare_verb`] reply grants.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Lease {
    /// When it ends, on aterm's clock (epoch µs): sent back with the act.
    pub until_us: u64,
}

impl Lease {
    /// Parse a `meta prepare harness_act` reply: `OK until_us=<n>
    /// lease_ms=<ms>`, exactly those two words in that order, `<n>` a
    /// canonical number above 0 and `<ms>` one from 1 to
    /// [`ACT_LEASE_MAX_MS`].
    ///
    /// # Errors
    /// A refusal ([`Reported::Later`], or [`Reported::Unserved`] from an
    /// aterm with no such request), or an `OK` not in that grammar
    /// ([`Reported::Unserved`], `prepare lease=malformed`).
    pub fn parse(reply: &str) -> Result<Self, Reported> {
        let mut words = reply.split_ascii_whitespace();
        if words.next() != Some("OK") {
            return Err(Reported::refusal("prepare", reply));
        }
        let until_us = words
            .next()
            .and_then(|word| word.strip_prefix("until_us="))
            .and_then(canonical_u64)
            .filter(|until| *until > 0);
        let lease_ms = words
            .next()
            .and_then(|word| word.strip_prefix("lease_ms="))
            .and_then(canonical_u64)
            .filter(|ms| (1..=ACT_LEASE_MAX_MS).contains(ms));
        match (until_us, lease_ms, words.next()) {
            (Some(until_us), Some(_), None) => Ok(Self { until_us }),
            _ => Err(Reported::Unserved("prepare lease=malformed".to_string())),
        }
    }
}

/// The one field that keeps a moment from being a safe point.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hold {
    /// The `status` or `harness policy` key that holds.
    pub field: &'static str,
    /// What aterm said for it, as one journal word; `absent` when it said
    /// nothing, `unknown` for a `human_ms` that is not a number.
    pub value: String,
    /// For a `human_ms` hold, how much of the person's grace is still to run.
    /// Nothing announces its end (an ageing key emits no event), so the holder
    /// reads again after it.
    pub grace_left: Option<Duration>,
}

impl Hold {
    fn on(field: &'static str, value: Option<&str>) -> SafePoint {
        SafePoint::Hold(Self {
            field,
            value: value.map_or_else(|| "absent".to_string(), journal_word),
            grace_left: None,
        })
    }
}

impl fmt::Display for Hold {
    /// The journal line: `wait aterm <field>=<value>`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "wait aterm {}={}", self.field, self.value)
    }
}

/// What one fresh policy and status pair says about moving the child now.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SafePoint {
    /// Every condition holds: aterm sees nobody the move would interrupt.
    Safe,
    /// The first condition that does not.
    Hold(Hold),
}

impl SafePoint {
    /// Whether the move may proceed.
    #[must_use]
    pub const fn is_safe(&self) -> bool {
        matches!(self, Self::Safe)
    }
}

/// The first of the claim's own conditions that does not hold for `duty`,
/// when one does not ([`judge_safe_point`], items 1 to 4).
fn claim_hold(policy: &Policy, status: &Status, holder: &str, duty: Scope) -> Option<SafePoint> {
    if status.supervisor.as_deref() != Some(holder) {
        return Some(Hold::on(
            "supervisor",
            Some(status.supervisor.as_deref().unwrap_or("-")),
        ));
    }
    if !status.supervisor_proven {
        return Some(Hold::on("supervisor_proven", Some("0")));
    }
    if !status.takes(duty) {
        let scope = status.supervisor_scope.join(",");
        return Some(Hold::on(
            "supervisor_scope",
            Some(if scope.is_empty() { "-" } else { &scope }),
        ));
    }
    if !policy.serves_contract() {
        let contracts = policy
            .contracts
            .iter()
            .map(u32::to_string)
            .collect::<Vec<_>>()
            .join(",");
        return Some(Hold::on("contract", Some(&contracts)));
    }
    if !policy.effective {
        return Some(Hold::on("effective", Some("off")));
    }
    None
}

/// The hold of a person who typed within the grace (the larger of the
/// policy's `human_grace_s` and `grace`), or whose last key cannot be read,
/// when there is one ([`judge_safe_point`], item 9).
fn person_hold(policy: &Policy, status: &Status, grace: Duration) -> Option<SafePoint> {
    let grace = policy.human_grace.max(grace);
    match status.human {
        Human::Never => None,
        Human::Ago(ago) if ago >= grace => None,
        Human::Ago(ago) => Some(SafePoint::Hold(Hold {
            field: "human_ms",
            value: ago.as_millis().to_string(),
            grace_left: Some(grace.saturating_sub(ago)),
        })),
        Human::Unknown => Some(Hold::on("human_ms", Some("unknown"))),
    }
}

/// The first of `fields` (a `status` key, what aterm said for it, and the one
/// value that does not hold) that reads otherwise, when one does.
fn field_hold(fields: &[(&'static str, Option<&str>, &str)]) -> Option<SafePoint> {
    fields
        .iter()
        .find(|(_, value, wanted)| *value != Some(*wanted))
        .map(|(field, value, _)| Hold::on(field, *value))
}

/// Judge one fresh `harness policy` and `status` pair as a safe point for
/// ending the child of the claim `holder` under its duty `duty`, by aterm's
/// own facts: `upgrade` for a move onto another vendor build, `relaunch` for
/// a restart the harness makes of its own accord at a quiet moment (the
/// session's return onto its launch's model, [`crate::limit`]).
///
/// It is [`SafePoint::Safe`] only when all of these read exactly so, checked in
/// this order, and otherwise the first that does not is the [`Hold`]:
///
/// 1. `supervisor=<holder>`: the standing claim is this harness's;
/// 2. `supervisor_proven=1`: aterm proved it;
/// 3. `supervisor_scope` names `duty`: the claim took this duty;
/// 4. `contract=` serves 3 and `effective=on`: the policy still stands;
/// 5. for `upgrade`, `upgrade=true`;
/// 6. for `upgrade`, `upgrade_ask=false`: nobody has to say when;
/// 7. `agent=idle`;
/// 8. `composer=empty` or `composer=placeholder`: no draft;
/// 9. `human_ms=-`, or at least the grace: the larger of the policy's
///    `human_grace_s` and the caller's own `grace`;
/// 10. `hand=-`, `hold=0`, `input=clear`, `need=-`.
///
/// A field that is absent or unreadable holds; nothing is assumed. `grace` can
/// lengthen aterm's grace and never shorten it. The two upgrade switches are
/// a person's word on vendor updates and are read for that duty alone.
///
/// This judges only what aterm sees. The harness's own kernel checks (the
/// foreground group, the session record, background work, the re-validated
/// plan) stand beside it and are not replaced by it.
#[must_use]
pub fn judge_safe_point(
    policy: &Policy,
    status: &Status,
    holder: &str,
    grace: Duration,
    duty: Scope,
) -> SafePoint {
    if let Some(hold) = claim_hold(policy, status, holder, duty) {
        return hold;
    }
    if duty == Scope::Upgrade {
        if !policy.upgrade {
            return Hold::on("upgrade", Some("false"));
        }
        if policy.upgrade_ask {
            return Hold::on("upgrade_ask", Some("true"));
        }
    }
    if status.agent.as_deref() != Some("idle") {
        return Hold::on("agent", status.agent.as_deref());
    }
    if !matches!(status.composer.as_deref(), Some("empty" | "placeholder")) {
        return Hold::on("composer", status.composer.as_deref());
    }
    if let Some(hold) = person_hold(policy, status, grace) {
        return hold;
    }
    field_hold(&[
        ("hand", status.hand.as_deref(), "-"),
        ("hold", status.hold.as_deref(), "0"),
        ("input", status.input.as_deref(), "clear"),
        ("need", status.need.as_deref(), "-"),
    ])
    .unwrap_or(SafePoint::Safe)
}

/// Judge one fresh `harness policy` and `status` pair as the moment to end a
/// child that stands at a usage limit, for the claim `holder`
/// ([`crate::limit`]). The vendor's limit dialog may be open then: the agent
/// is not `idle`, no composer is drawn (`composer=-`), and the session may
/// read as needing a person, so neither the agent nor the need is asked. What
/// is asked is whether a person or a driver would be interrupted, or a draft
/// lost:
///
/// 1. `supervisor=<holder>` and `supervisor_proven=1`;
/// 2. `supervisor_scope` names `relaunch`: the restart is made under that duty;
/// 3. `contract=` serves 3 and `effective=on`;
/// 4. `composer=empty`, `placeholder` or `-`: no draft aterm can see. Once the
///    dialog is closed (pressed, or never drawn under the vendor's own
///    countdown) the composer is back, and a draft in it reads `draft`;
/// 5. `human_ms=-`, or at least the grace (the larger of the policy's
///    `human_grace_s` and `grace`);
/// 6. `hand=-`, `hold=0`, `input=clear`.
///
/// A field that is absent or unreadable holds. The first that does not read so
/// is the [`Hold`]. aterm's `-` also stands for a composer whose placeholder
/// cannot be told from a draft; that one is let through here, where the
/// dialog's own `-` must be, and the person's grace (5) is what guards it.
#[must_use]
pub fn judge_wall_point(
    policy: &Policy,
    status: &Status,
    holder: &str,
    grace: Duration,
) -> SafePoint {
    claim_hold(policy, status, holder, Scope::Relaunch)
        .or_else(|| {
            (!matches!(
                status.composer.as_deref(),
                Some("empty" | "placeholder" | "-")
            ))
            .then(|| Hold::on("composer", status.composer.as_deref()))
        })
        .or_else(|| person_hold(policy, status, grace))
        .or_else(|| {
            field_hold(&[
                ("hand", status.hand.as_deref(), "-"),
                ("hold", status.hold.as_deref(), "0"),
                ("input", status.input.as_deref(), "clear"),
            ])
        })
        .unwrap_or(SafePoint::Safe)
}

#[cfg(test)]
#[path = "aterm_wall_tests.rs"]
mod wall_tests;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_older_policy_without_contracts_admits_no_claim() {
        let older = "OK gen=12 effective=on why=config continue=on land=off questions=on";
        assert_eq!(advertised_contract(older), None);
    }

    #[test]
    fn the_supported_contract_must_be_a_member_of_a_valid_set() {
        for reply in [
            "OK gen=1 contract=3",
            "OK gen=1 effective=on why=enabled contract=2,3 continue=on",
            "OK contract=3,2",
            "OK contract=2,3,4",
        ] {
            assert_eq!(advertised_contract(reply), Some(3), "{reply}");
        }
        for reply in [
            "",
            "ERR contract=3",
            "OKAY contract=3",
            "OK contract=2",
            "OK contract=4",
            "OK contract=",
            "OK contract=x",
            "OK contract=03",
            "OK contract=+3",
            "OK contract=2,,3",
            "OK contract=3,",
            "OK contract=,3",
            "OK contract=3,x",
            "OK contract=3,0",
            "OK contract=3,4294967296",
            "OK contract=3 contract=2,3",
        ] {
            assert_eq!(advertised_contract(reply), None, "{reply:?}");
        }
    }

    #[test]
    fn recognizing_a_host_contract_alone_admits_no_claim() {
        assert_eq!(advertised_contract("OK contract=2,3"), Some(3));
        // The contract set is one of seven facts a policy must state, and the
        // policy one of three gates (with the roster and the proof read-back).
        assert_eq!(
            Policy::parse("OK contract=2,3"),
            Err(ReplyError::Missing("gen"))
        );
        assert!(!rostered("OK contract=2,3"));
    }

    const SID: &str = "tqn5r6e0148";
    const ME: &str = "claude-code-alab@4242";

    /// A reply cut from a running aterm (2026-11-12), text keys shortened.
    const LIVE_POLICY: &str = "OK gen=1 effective=on why=enabled contract=2,3 codex_rotation=1 \
        enabled=true headless=true autopilot=cruise land_at=- approve=all trust_roots=- \
        answer_text=Nobody%20is%20here. continue=true continue_per_hour=0 relaunch=true \
        stall_term_after_s=600 stall_term=true memory_ceiling=auto upgrade=true \
        upgrade_ask=false human_grace_s=120 occupancy=observe facts=true questions=recommended";

    /// A reply cut from a running aterm (2026-11-12) with this harness's proven
    /// claim written in where its built-in holder stood.
    const LIVE_STATUS: &str = "OK schema=1 sid=16 subject=%E2%97%90%20overview observed=true \
        phase=running hold=0 hold_advisory=- fabric=connected hand=- hand_pid=- level=quiet \
        needs=- needs_n=- hold_by=- typed_by=aterm program=claude agent=idle agent_detail=- \
        agent_rev=30 composer=empty program_state=- integration=on input=clear input_bytes=0 \
        input_wait_ms=0 supervisor_contract=3 supervisor_scope=relaunch,upgrade \
        supervisor_revision=136 supervisor=claude-code-alab@4242 supervisor_proven=1 \
        supervisor_version=0.1.0 supervisor_vendor=2.1.311 supervisor_act=on autopilot=cruise \
        human_ms=2154931 human_typed_ms=2154931 human_seq=1474 upgrade=- need=- \
        need_since_ms=- gen=1.90262 seq=90262 hash=5633b312ee9f9b16";

    fn policy() -> Policy {
        Policy::parse(LIVE_POLICY).expect("live policy")
    }

    fn status() -> Status {
        Status::parse(LIVE_STATUS).expect("live status")
    }

    /// `reply` with the word for `key` replaced (`Some`) or removed (`None`).
    fn with(reply: &str, key: &str, value: Option<&str>) -> String {
        let prefix = format!("{key}=");
        let mut found = false;
        let mut words: Vec<String> = Vec::new();
        for word in reply.split_ascii_whitespace() {
            if word.starts_with(&prefix) {
                found = true;
                if let Some(value) = value {
                    words.push(format!("{prefix}{value}"));
                }
            } else {
                words.push(word.to_string());
            }
        }
        assert!(found, "{key} is not in the reply");
        words.join(" ")
    }

    #[test]
    fn a_live_policy_reply_parses_and_unknown_keys_are_passed_over() {
        let policy = policy();
        assert_eq!(
            policy,
            Policy {
                generation: 1,
                contracts: vec![2, 3],
                effective: true,
                why: Some("enabled".to_string()),
                relaunch: true,
                upgrade: true,
                upgrade_ask: false,
                human_grace: Duration::from_secs(120),
            }
        );
        assert!(policy.serves_contract() && policy.admits_claim());
        let more = format!("{LIVE_POLICY} bare-word a_new_key=1 =x");
        assert_eq!(Policy::parse(&more), Ok(policy));
    }

    #[test]
    fn a_policy_off_by_configuration_is_told_from_one_off_for_now() {
        for why in [
            "unhosted",
            "disabled",
            "headless",
            "agent-off",
            "autopilot-off",
        ] {
            let reply = with(
                &with(LIVE_POLICY, "effective", Some("off")),
                "why",
                Some(why),
            );
            let off = Policy::parse(&reply).unwrap();
            assert!(off.off_by_configuration(), "{why}");
            assert_eq!(off.off_words(), format!("effective=off why={why}"));
        }
        for why in [Some("hold"), Some("suspended"), Some("a-new-reason"), None] {
            let reply = with(&with(LIVE_POLICY, "effective", Some("off")), "why", why);
            let off = Policy::parse(&reply).unwrap();
            assert!(
                !off.off_by_configuration() && !off.admits_claim(),
                "{why:?}"
            );
        }
        assert_eq!(
            Policy::parse(&with(
                &with(LIVE_POLICY, "effective", Some("off")),
                "why",
                None
            ))
            .unwrap()
            .off_words(),
            "effective=off"
        );
        // On is never off by configuration, whatever `why=` says.
        let on = Policy::parse(&with(LIVE_POLICY, "why", Some("disabled"))).unwrap();
        assert!(on.effective && !on.off_by_configuration());
        assert_eq!(
            Policy::parse(&format!("{LIVE_POLICY} why=hold")),
            Err(ReplyError::Repeated("why"))
        );
    }

    #[test]
    fn a_policy_that_is_off_or_older_parses_but_admits_no_claim() {
        let off = Policy::parse(&with(LIVE_POLICY, "effective", Some("off"))).unwrap();
        assert!(off.serves_contract() && !off.effective && !off.admits_claim());
        let older = Policy::parse(&with(LIVE_POLICY, "contract", Some("2"))).unwrap();
        assert!(!older.serves_contract() && older.effective && !older.admits_claim());
    }

    #[test]
    fn a_policy_missing_repeating_or_garbling_a_needed_field_is_refused() {
        for key in [
            "gen",
            "contract",
            "effective",
            "relaunch",
            "upgrade",
            "upgrade_ask",
            "human_grace_s",
        ] {
            assert_eq!(
                Policy::parse(&with(LIVE_POLICY, key, None)),
                Err(ReplyError::Missing(key)),
                "{key} absent"
            );
            assert_eq!(
                Policy::parse(&format!("{LIVE_POLICY} {key}=1")),
                Err(ReplyError::Repeated(key)),
                "{key} repeated"
            );
            for bad in ["", "x", "01", "-1", "TRUE", "On", "yes", "3,", "1.5"] {
                assert_eq!(
                    Policy::parse(&with(LIVE_POLICY, key, Some(bad))),
                    Err(ReplyError::Malformed(key)),
                    "{key}={bad}"
                );
            }
        }
        for (reply, why) in [
            ("", "reply=(empty)"),
            ("ERR denied", "reply=ERR:denied"),
            ("ERR no such session: s-1", "reply=ERR:no"),
            ("OKAY gen=1", "reply=OKAY:gen=1"),
            ("OK effective=on", "gen=absent"),
            ("OK\u{7}x effective=on", "reply=OK%07x:effective=on"),
        ] {
            assert_eq!(
                Policy::parse(reply).unwrap_err().to_string(),
                why,
                "{reply:?}"
            );
        }
        assert_eq!(ReplyError::Malformed("gen").to_string(), "gen=malformed");
        assert_eq!(ReplyError::Repeated("gen").to_string(), "gen=repeated");
    }

    #[test]
    fn a_live_status_reply_parses_leniently() {
        let status = status();
        assert_eq!(
            status,
            Status {
                supervisor: Some(ME.to_string()),
                supervisor_proven: true,
                supervisor_scope: vec!["relaunch".to_string(), "upgrade".to_string()],
                supervisor_revision: Some(136),
                agent: Some("idle".to_string()),
                composer: Some("empty".to_string()),
                human: Human::Ago(Duration::from_millis(2_154_931)),
                human_seq: Some(1474),
                hand: Some("-".to_string()),
                hold: Some("0".to_string()),
                input: Some("clear".to_string()),
                need: Some("-".to_string()),
                autopilot: Some(Autopilot::Cruise),
                autopilot_stated: true,
            }
        );
        assert!(status.held_by(ME) && !status.held_by("claude-code-alab@4243"));
        assert!(status.takes(Scope::Upgrade) && status.takes(Scope::Relaunch));
        // The last word for a key wins, and bare words and empty keys are skipped.
        let later = format!("{LIVE_STATUS} bare =x agent=working hold=1");
        let later = Status::parse(&later).unwrap();
        assert_eq!(later.agent.as_deref(), Some("working"));
        assert_eq!(later.hold.as_deref(), Some("1"));
        // A holder aterm percent-encodes reads the same.
        let encoded = with(LIVE_STATUS, "supervisor", Some("claude-code-alab%404242"));
        assert!(Status::parse(&encoded).unwrap().held_by(ME));
        // Autopilot is one of aterm's three wire words; `-`, another name for
        // a mode and an absent field state none.
        for (word, mode) in [
            (Some("land"), Some(Autopilot::Land)),
            (Some("off"), Some(Autopilot::Off)),
            (Some("cruise"), Some(Autopilot::Cruise)),
            (Some("-"), None),
            (Some("forever"), None),
            (Some("Land"), None),
            (None, None),
        ] {
            let reply = with(LIVE_STATUS, "autopilot", word);
            let parsed = Status::parse(&reply).unwrap();
            assert_eq!(parsed.autopilot, mode, "{word:?}");
            assert_eq!(parsed.autopilot_stated, word.is_some(), "{word:?}");
        }
    }

    #[test]
    fn every_duty_is_spelled_as_aterm_spells_it_and_only_the_four_typing_duties_type() {
        let words: Vec<&str> = Scope::ALL.into_iter().map(Scope::word).collect();
        assert_eq!(
            words,
            [
                "relaunch",
                "upgrade",
                "turn-end",
                "approvals",
                "questions",
                "walls"
            ]
        );
        for scope in Scope::ALL {
            assert_eq!(Scope::from_word(scope.word()), Some(scope));
        }
        for word in ["", "turn_end", "Walls", "relaunch,upgrade", "turn-end "] {
            assert_eq!(Scope::from_word(word), None, "{word:?}");
        }
        let typing: Vec<Scope> = Scope::ALL.into_iter().filter(|s| s.types()).collect();
        assert_eq!(
            typing,
            [
                Scope::TurnEnd,
                Scope::Approvals,
                Scope::Questions,
                Scope::Walls
            ]
        );
        // The canonical order is the derived one, so a sorted scope list is
        // spelled in aterm's order.
        let mut sorted = Scope::ALL;
        sorted.reverse();
        sorted.sort();
        assert_eq!(sorted, Scope::ALL);
    }

    #[test]
    fn a_status_reply_states_no_claim_and_no_fact_it_does_not_carry() {
        let bare = Status::parse("OK schema=1").unwrap();
        assert_eq!(
            bare,
            Status {
                supervisor: None,
                supervisor_proven: false,
                supervisor_scope: Vec::new(),
                supervisor_revision: None,
                agent: None,
                composer: None,
                human: Human::Unknown,
                human_seq: None,
                hand: None,
                hold: None,
                input: None,
                need: None,
                autopilot: None,
                autopilot_stated: false,
            }
        );
        assert!(!bare.held_by(ME) && !bare.takes(Scope::Upgrade));
        let unclaimed = Status::parse(
            "OK schema=1.2 supervisor=- supervisor_proven=0 supervisor_scope=- \
             supervisor_revision=7 human_ms=-",
        )
        .unwrap();
        assert_eq!(unclaimed.supervisor, None);
        assert!(unclaimed.supervisor_scope.is_empty());
        assert_eq!(unclaimed.supervisor_revision, Some(7));
        assert_eq!(unclaimed.human, Human::Never);
        // A value outside its grammar is no claim, no proof and no reading.
        let garbled =
            |key: &str, value: &str| Status::parse(&with(LIVE_STATUS, key, Some(value))).unwrap();
        assert_eq!(garbled("supervisor", "%zz").supervisor, None);
        assert_eq!(garbled("supervisor", "").supervisor, None);
        assert!(!garbled("supervisor_proven", "true").held_by(ME));
        assert!(!garbled("supervisor_proven", "01").held_by(ME));
        assert_eq!(
            garbled("supervisor_revision", "x").supervisor_revision,
            None
        );
        for value in ["soon", "-5", "007", ""] {
            assert_eq!(
                garbled("human_ms", value).human,
                Human::Unknown,
                "{value:?}"
            );
            assert_eq!(garbled("human_seq", value).human_seq, None, "{value:?}");
        }
        assert_eq!(garbled("human_seq", "0").human_seq, Some(0));
        for (reply, why) in [
            ("ERR no such session", "reply=ERR:no"),
            ("", "reply=(empty)"),
            ("OK sid=1", "schema=absent"),
            ("OK schema=2", "schema=malformed"),
            ("OK schema=", "schema=malformed"),
        ] {
            assert_eq!(
                Status::parse(reply).unwrap_err().to_string(),
                why,
                "{reply:?}"
            );
        }
    }

    #[test]
    fn the_holder_and_the_claim_text_are_exactly_contract_3s() {
        assert_eq!(holder(4242), ME);
        assert_eq!(
            claim_verb(SID, 4242, &[Scope::Upgrade], "0.1.0", Some("2.1.311")).as_deref(),
            Some(
                "@tqn5r6e0148 meta set supervisor claude-code-alab@4242 contract=3 scope=upgrade \
                 version=0.1.0 vendor=2.1.311"
            )
        );
        // Scopes are spelled once each, in aterm's order, however they are given.
        assert_eq!(
            claim_verb(
                SID,
                4242,
                &[Scope::Upgrade, Scope::Relaunch, Scope::Upgrade],
                "0.1.0+g1a2b3c4",
                None
            )
            .as_deref(),
            Some(
                "@tqn5r6e0148 meta set supervisor claude-code-alab@4242 contract=3 \
                 scope=relaunch,upgrade version=0.1.0+g1a2b3c4"
            )
        );
        // A vendor word aterm would refuse the whole claim over is left out.
        for vendor in ["", "2.1 311", "2.1.311\n", "caf\u{e9}", &"9".repeat(65)] {
            assert_eq!(
                claim_verb(SID, 4242, &[Scope::Upgrade], "0.1.0", Some(vendor)).as_deref(),
                Some(
                    "@tqn5r6e0148 meta set supervisor claude-code-alab@4242 contract=3 \
                     scope=upgrade version=0.1.0"
                ),
                "{vendor:?}"
            );
        }
        assert!(
            claim_verb(
                SID,
                4242,
                &[Scope::Upgrade],
                "0.1.0",
                Some("2.1.311-beta_1+x")
            )
            .unwrap()
            .ends_with(" vendor=2.1.311-beta_1+x")
        );
        // This build's own version is one aterm reads.
        assert!(claim_verb(SID, 4242, &[Scope::Upgrade], crate::cli::VERSION, None).is_some());
    }

    #[test]
    fn nothing_is_sent_for_a_claim_aterm_could_not_read() {
        for version in [
            "",
            "0.1",
            "0.1.1",
            "0.1.0-rc1",
            "0.1.0+",
            "0.1.0+a..b",
            "0.1.0+a_b",
            "01.1.0",
            "0.01.0",
            "0.1.0.0",
            "v0.1.0",
            "0.1.0 x",
            "4294967296.0.0",
        ] {
            assert_eq!(
                claim_verb(SID, 4242, &[Scope::Upgrade], version, None),
                None,
                "{version:?}"
            );
        }
        assert!(
            claim_verb(
                SID,
                4242,
                &[Scope::Upgrade],
                &format!("1.2.0+{}", "a".repeat(122)),
                None
            )
            .is_some()
        );
        assert!(
            claim_verb(
                SID,
                4242,
                &[Scope::Upgrade],
                &format!("1.2.0+{}", "a".repeat(123)),
                None
            )
            .is_none()
        );
        assert_eq!(claim_verb(SID, 4242, &[], "0.1.0", None), None);
        for pid in [0, 1] {
            assert_eq!(claim_verb(SID, pid, &[Scope::Upgrade], "0.1.0", None), None);
            assert_eq!(release_verb(SID, pid), None);
        }
        for sid in [
            "",
            "s-",
            "16",
            "s-xyz",
            "s-1 status",
            "s-1\n@s-2",
            "*",
            "../s-1",
        ] {
            assert_eq!(
                claim_verb(sid, 4242, &[Scope::Upgrade], "0.1.0", None),
                None
            );
            assert_eq!(release_verb(sid, 4242), None);
            assert_eq!(policy_verb(sid), None);
            assert_eq!(status_verb(sid), None);
            assert_eq!(info_verb(sid), None);
        }
    }

    #[test]
    fn the_release_and_the_reads_are_addressed_to_the_session() {
        assert_eq!(
            release_verb(SID, 4242).as_deref(),
            Some("@tqn5r6e0148 meta unset supervisor holder=claude-code-alab@4242")
        );
        assert_eq!(
            policy_verb(SID).as_deref(),
            Some("@tqn5r6e0148 harness policy")
        );
        assert_eq!(status_verb(SID).as_deref(), Some("@tqn5r6e0148 status"));
        assert_eq!(
            info_verb(SID).as_deref(),
            Some("@tqn5r6e0148 native-act info")
        );
    }

    #[test]
    fn the_roster_guard_needs_this_harnesss_own_upgrade_row() {
        const INFO: &str = "OK native-act state=info epoch=d1a48b482e4c6b391c4715137ce246b6 \
            native-act=1 handoff=blocked clock=monotonic-us own-image=";
        for rows in [
            "claude-code-alab:upgrade",
            "grok-alab:upgrade,muse-alab:upgrade,kimi-alab:upgrade,claude-code-alab:upgrade",
            "claude-code-alab:upgrade,grok-alab:upgrade",
        ] {
            assert!(rostered(&format!("{INFO}{rows}")), "{rows}");
            assert!(rostered(&format!("{INFO}{rows} a_later_key=1")), "{rows}");
        }
        for rows in [
            // What aterm answers today (2026-11-12): the row is not shipped.
            "grok-alab:upgrade,muse-alab:upgrade,kimi-alab:upgrade",
            "-",
            "",
            "claude-code-alab",
            "claude-code-alab:relaunch",
            "claude-code-alab:upgrade:x",
            "claude-code-alabs:upgrade",
            "xclaude-code-alab:upgrade",
            "Claude-Code-Alab:upgrade",
            "claude-code-alab:upgrade;grok-alab:upgrade",
        ] {
            assert!(!rostered(&format!("{INFO}{rows}")), "{rows:?}");
        }
        for reply in [
            "",
            "OK",
            "OK native-act state=info",
            "ERR usage: native-act",
            "ERR own-image=claude-code-alab:upgrade",
            "OKAY own-image=claude-code-alab:upgrade",
            "own-image=claude-code-alab:upgrade",
            "OK own-image=claude-code-alab:upgrade own-image=-",
            "OK own-image=- own-image=claude-code-alab:upgrade",
            "OK own_image=claude-code-alab:upgrade",
        ] {
            assert!(!rostered(reply), "{reply:?}");
        }
    }

    #[test]
    fn a_restart_event_names_exactly_one_cause() {
        let parse = |tail: &str| Restart::parse(tail.split_ascii_whitespace()).ok();
        for (tail, cause) in [
            ("cause=asked", "asked"),
            ("cause=stall", "stall"),
            ("cause=account to=work home=%2FUsers%2Fa", "account"),
            ("cause=account-ahead to=work home=-", "account-ahead"),
            ("cause=asked a_later_key=1 bare", "asked"),
        ] {
            assert_eq!(
                parse(tail).map(|restart| (restart.cause, restart.model)),
                Some((cause.to_string(), None)),
                "{tail}"
            );
        }
        for tail in [
            "",
            "cause=",
            "asked",
            "to=work cause=account",
            "cause=asked cause=stall",
            "cause=a/b",
            "cause=a%20b",
            "Cause=asked",
        ] {
            assert_eq!(parse(tail), None, "{tail:?}");
        }
        assert_eq!(
            parse(&format!("cause={}", "a".repeat(64))).map(|r| r.cause.len()),
            Some(64)
        );
        assert_eq!(parse(&format!("cause={}", "a".repeat(65))), None);
    }

    fn held(policy: &Policy, status: &Status) -> String {
        match judge_safe_point(policy, status, ME, Duration::ZERO, Scope::Upgrade) {
            SafePoint::Safe => "safe".to_string(),
            SafePoint::Hold(hold) => hold.to_string(),
        }
    }

    #[test]
    fn a_quiet_proven_session_is_a_safe_point() {
        let verdict = judge_safe_point(&policy(), &status(), ME, Duration::ZERO, Scope::Upgrade);
        assert_eq!(verdict, SafePoint::Safe);
        assert!(verdict.is_safe());
        for (key, value) in [
            ("composer", "placeholder"),
            ("human_ms", "-"),
            ("human_ms", "120000"),
        ] {
            let status = Status::parse(&with(LIVE_STATUS, key, Some(value))).unwrap();
            assert_eq!(held(&policy(), &status), "safe", "{key}={value}");
        }
        // A claim that also took relaunch, or only upgrade, is still ours to move under.
        let narrow =
            Status::parse(&with(LIVE_STATUS, "supervisor_scope", Some("upgrade"))).unwrap();
        assert_eq!(held(&policy(), &narrow), "safe");
    }

    #[test]
    fn each_status_field_holds_alone_and_is_journalled_by_name() {
        for (key, value, line) in [
            ("supervisor", Some("-"), "wait aterm supervisor=-"),
            ("supervisor", None, "wait aterm supervisor=-"),
            (
                "supervisor",
                Some("claude-code-alab@4243"),
                "wait aterm supervisor=claude-code-alab@4243",
            ),
            (
                "supervisor",
                Some("aterm-harness@40450"),
                "wait aterm supervisor=aterm-harness@40450",
            ),
            (
                "supervisor",
                Some("a%20b%0A"),
                "wait aterm supervisor=a%20b%0A",
            ),
            (
                "supervisor_proven",
                Some("0"),
                "wait aterm supervisor_proven=0",
            ),
            ("supervisor_proven", None, "wait aterm supervisor_proven=0"),
            (
                "supervisor_scope",
                Some("-"),
                "wait aterm supervisor_scope=-",
            ),
            ("supervisor_scope", None, "wait aterm supervisor_scope=-"),
            (
                "supervisor_scope",
                Some("relaunch"),
                "wait aterm supervisor_scope=relaunch",
            ),
            ("agent", Some("working"), "wait aterm agent=working"),
            ("agent", Some("unknown"), "wait aterm agent=unknown"),
            ("agent", Some(""), "wait aterm agent=(empty)"),
            ("agent", None, "wait aterm agent=absent"),
            ("composer", Some("draft"), "wait aterm composer=draft"),
            ("composer", None, "wait aterm composer=absent"),
            ("human_ms", Some("119999"), "wait aterm human_ms=119999"),
            ("human_ms", Some("0"), "wait aterm human_ms=0"),
            ("human_ms", Some("soon"), "wait aterm human_ms=unknown"),
            ("human_ms", None, "wait aterm human_ms=unknown"),
            (
                "hand",
                Some("lease:someone"),
                "wait aterm hand=lease:someone",
            ),
            ("hand", Some("turn:7:s-12"), "wait aterm hand=turn:7:s-12"),
            ("hand", None, "wait aterm hand=absent"),
            ("hold", Some("1"), "wait aterm hold=1"),
            ("hold", None, "wait aterm hold=absent"),
            ("input", Some("unread"), "wait aterm input=unread"),
            ("input", None, "wait aterm input=absent"),
            ("need", Some("question"), "wait aterm need=question"),
            ("need", None, "wait aterm need=absent"),
        ] {
            let status = Status::parse(&with(LIVE_STATUS, key, value)).unwrap();
            assert_eq!(held(&policy(), &status), line, "{key}={value:?}");
        }
        // Nothing but the schema: the first unmet condition is the claim.
        let bare = Status::parse("OK schema=1").unwrap();
        assert_eq!(held(&policy(), &bare), "wait aterm supervisor=-");
    }

    #[test]
    fn each_policy_field_holds_alone() {
        for (key, value, line) in [
            ("contract", "2", "wait aterm contract=2"),
            ("contract", "2,4", "wait aterm contract=2,4"),
            ("effective", "off", "wait aterm effective=off"),
            ("upgrade", "false", "wait aterm upgrade=false"),
            ("upgrade_ask", "true", "wait aterm upgrade_ask=true"),
        ] {
            let policy = Policy::parse(&with(LIVE_POLICY, key, Some(value))).unwrap();
            assert_eq!(held(&policy, &status()), line, "{key}={value}");
        }
        // `relaunch=false` limits the crash relaunch, never a move.
        let policy = Policy::parse(&with(LIVE_POLICY, "relaunch", Some("false"))).unwrap();
        assert_eq!(held(&policy, &status()), "safe");
    }

    #[test]
    fn the_first_unmet_condition_is_the_one_reported() {
        // Everything is wrong at once; each repair uncovers the next condition.
        let mut policy_reply = with(
            &with(LIVE_POLICY, "upgrade", Some("false")),
            "upgrade_ask",
            Some("true"),
        );
        let mut reply = LIVE_STATUS.to_string();
        for (key, value) in [
            ("supervisor_proven", "0"),
            ("supervisor_scope", "relaunch"),
            ("agent", "working"),
            ("composer", "draft"),
            ("human_ms", "5"),
            ("hand", "lease:x"),
            ("hold", "1"),
            ("input", "unread"),
            ("need", "question"),
        ] {
            reply = with(&reply, key, Some(value));
        }
        let wrong = Policy::parse(&policy_reply).unwrap();
        for (key, good, line) in [
            ("supervisor_proven", "1", "wait aterm supervisor_proven=0"),
            (
                "supervisor_scope",
                "relaunch,upgrade",
                "wait aterm supervisor_scope=relaunch",
            ),
        ] {
            assert_eq!(held(&wrong, &Status::parse(&reply).unwrap()), line);
            reply = with(&reply, key, Some(good));
        }
        let status = Status::parse(&reply).unwrap();
        for (key, good, line) in [
            ("upgrade", "true", "wait aterm upgrade=false"),
            ("upgrade_ask", "false", "wait aterm upgrade_ask=true"),
        ] {
            assert_eq!(held(&Policy::parse(&policy_reply).unwrap(), &status), line);
            policy_reply = with(&policy_reply, key, Some(good));
        }
        let policy = Policy::parse(&policy_reply).unwrap();
        for (key, good, line) in [
            ("agent", "idle", "wait aterm agent=working"),
            ("composer", "empty", "wait aterm composer=draft"),
            ("human_ms", "-", "wait aterm human_ms=5"),
            ("hand", "-", "wait aterm hand=lease:x"),
            ("hold", "0", "wait aterm hold=1"),
            ("input", "clear", "wait aterm input=unread"),
            ("need", "-", "wait aterm need=question"),
        ] {
            assert_eq!(held(&policy, &Status::parse(&reply).unwrap()), line);
            reply = with(&reply, key, Some(good));
        }
        assert_eq!(held(&policy, &Status::parse(&reply).unwrap()), "safe");
    }

    #[test]
    fn the_persons_grace_is_aterms_or_longer_and_says_how_long_is_left() {
        let typed = Status::parse(&with(LIVE_STATUS, "human_ms", Some("90000"))).unwrap();
        let SafePoint::Hold(hold) =
            judge_safe_point(&policy(), &typed, ME, Duration::ZERO, Scope::Upgrade)
        else {
            panic!("typing 90 s ago is inside a 120 s grace");
        };
        assert_eq!((hold.field, hold.value.as_str()), ("human_ms", "90000"));
        assert_eq!(hold.grace_left, Some(Duration::from_secs(30)));
        // The caller's own grace lengthens aterm's and never shortens it.
        let aged = Status::parse(&with(LIVE_STATUS, "human_ms", Some("150000"))).unwrap();
        assert!(
            judge_safe_point(&policy(), &aged, ME, Duration::from_secs(1), Scope::Upgrade)
                .is_safe()
        );
        let SafePoint::Hold(hold) = judge_safe_point(
            &policy(),
            &aged,
            ME,
            Duration::from_secs(200),
            Scope::Upgrade,
        ) else {
            panic!("150 s is inside the caller's 200 s grace");
        };
        assert_eq!(hold.grace_left, Some(Duration::from_secs(50)));
        // A grace of zero is a policy aterm may state: only `-` and any age pass.
        let none = Policy::parse(&with(LIVE_POLICY, "human_grace_s", Some("0"))).unwrap();
        let now = Status::parse(&with(LIVE_STATUS, "human_ms", Some("0"))).unwrap();
        assert!(judge_safe_point(&none, &now, ME, Duration::ZERO, Scope::Upgrade).is_safe());
        // Any other hold carries no grace.
        let busy = Status::parse(&with(LIVE_STATUS, "agent", Some("working"))).unwrap();
        let SafePoint::Hold(hold) =
            judge_safe_point(&policy(), &busy, ME, Duration::ZERO, Scope::Upgrade)
        else {
            panic!("a working agent holds");
        };
        assert_eq!(hold.grace_left, None);
    }

    #[test]
    fn a_long_or_hostile_value_is_one_bounded_journal_word() {
        let long = "x".repeat(300);
        let status = Status::parse(&with(LIVE_STATUS, "agent", Some(&long))).unwrap();
        let line = held(&policy(), &status);
        assert_eq!(line, format!("wait aterm agent={}", "x".repeat(64)));
        let odd = Status::parse(&with(LIVE_STATUS, "need", Some("caf\u{e9}%"))).unwrap();
        assert_eq!(held(&policy(), &odd), "wait aterm need=caf%C3%A9%25");
        assert!(!held(&policy(), &odd).contains(char::is_control));
    }
}
