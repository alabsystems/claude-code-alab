// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The aterm client boundary: what a contract-3 claim says and what its replies
//! mean.
//!
//! This module holds the pure half of the contract-3 client
//! (`docs/PLAN-stage-1-contract-3-client-2026-11-12.md`, sections 1 and 2): the
//! request texts, the reply parsers and the safe-point judge. It performs no
//! I/O; [`crate::aterm_wire`] carries the lines, and the supervisor's link
//! (`src/supervise_link.rs`) is the one caller: inside an aterm tab it claims
//! the `relaunch` and `upgrade` duties before the first vendor start and judges
//! the safe point by aterm's `status` while that claim stands proven
//! (docs/REFERENCE.md, "aterm supervision"). A [`Restart`] is handed to the
//! supervisor, which makes it only under that claim and a fresh permission read
//! ([`crate::relaunch::permitted`]). The "paired" own-image provider that
//! preceded this client was removed on 2026-11-12 without ever having been
//! activated.
//!
//! The rule a client of this boundary keeps:
//!
//! * **Never a trial claim.** aterm stores a contract-3 claim from a holder that
//!   is not on its roster as an ordinary foreign claim, and a stored foreign
//!   claim takes every duty from aterm's built-in engine
//!   (`aterm/crates/aterm-agent/src/harness/contract.rs`, `Words::Foreign`). So
//!   the harness claims only after a read-only `native-act info` reply names its
//!   row ([`rostered`]).
//! * **Claim only under contract v3.** The claim is [`claim_verb`]'s text, sent
//!   only after `harness policy` advertises contract 3 and reads `effective=on`
//!   ([`Policy`]).
//! * **A claim stands only when `status` proves it.** aterm proves a claim by
//!   kernel peer pid, the claimant leading the terminal's foreground process
//!   group, and its image sitting in a harness package. The harness reads that
//!   back as `supervisor=<holder> supervisor_proven=1` ([`Status::held_by`]) and
//!   releases a claim that does not read so ([`release_verb`]).
//! * **Fail closed.** A field a judgement needs that is absent or malformed
//!   holds; no parser here supplies a default.
//! * **Type only through the fence.** Any `send`/`key`/`paste`/`turn` the
//!   harness ever sends carries `if-supervisor=claude-code-alab@<pid>`
//!   (`crates/aterm-gui/src/control_input.rs:352-415`), so a write after the
//!   claim is lost can never land. Nothing here types.

use std::fmt;
use std::time::Duration;

use crate::aterm_wire::{Fields, percent_decode, session_id};

/// The aterm control contract this harness requires before it claims anything.
pub const REQUIRED_CONTRACT: u32 = 3;

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

/// Whether the host advertised this client's supported contract. An unknown newer
/// contract alone does not imply support for this one. This is a capability check,
/// not activation of the client.
#[must_use]
pub fn may_claim(advertised: Option<u32>) -> bool {
    advertised == Some(REQUIRED_CONTRACT)
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

/// One duty a contract-3 claim takes from aterm's built-in engine. Only the two
/// this harness's roster row may ever allow are named; their order is aterm's
/// canonical one (`contract.rs`, `Scope::ALL`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Scope {
    /// Relaunching the child on the same conversation after a crash.
    Relaunch,
    /// Moving the child onto another vendor build at a safe point.
    Upgrade,
}

impl Scope {
    /// The wire word (`contract.rs`, `Scope::word`).
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Self::Relaunch => "relaunch",
            Self::Upgrade => "upgrade",
        }
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

/// The contract-3 claim for session `sid` by harness process `pid`:
///
/// `@<sid> meta set supervisor claude-code-alab@<pid> contract=3 scope=<words> version=<version>[ vendor=<vendor>]`
///
/// `scopes` are spelled once each in aterm's canonical order, comma-joined.
/// `version` is this harness's own ([`crate::cli::VERSION`]); `vendor` is the
/// vendor build the child runs, and is left out when it is unknown or is not a
/// word aterm would accept, because aterm refuses the whole claim over one bad
/// word (`contract.rs`, `read_in`). `login=` is never sent: this harness keeps no
/// account.
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
    let mut verb = format!(
        "meta set supervisor {} contract={REQUIRED_CONTRACT} scope={scope} version={version}",
        holder(pid)
    );
    if let Some(vendor) = vendor_word(vendor) {
        verb.push_str(" vendor=");
        verb.push_str(vendor);
    }
    addressed(sid, &verb)
}

/// A direct vendor child's kernel lifetime, in the host's native birth units.
/// This is identity evidence only: aterm still proves the child's image before
/// allowing any built-in typing duty to use it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ChildProof {
    /// The direct child's process id, checked once when constructed.
    pid: std::num::NonZeroU32,
    /// Epoch microseconds on macOS; boot clock ticks on Linux.
    birth: std::num::NonZeroU64,
}

impl ChildProof {
    /// A native child target in the host's admitted pid and birth ranges.
    #[must_use]
    pub fn new(pid: u32, birth: u64) -> Option<Self> {
        if pid <= 1 || pid > i32::MAX as u32 {
            return None;
        }
        Some(Self {
            pid: std::num::NonZeroU32::new(pid)?,
            birth: std::num::NonZeroU64::new(birth)?,
        })
    }

    /// The claim target for a fresh kernel observation, refusing invalid or
    /// overflowing identities instead of substituting a wall-clock start time.
    #[must_use]
    pub fn from_identity(identity: &crate::procs::ChildIdentity) -> Option<Self> {
        let birth = match identity.birth {
            crate::procs::Birth::MacOs {
                seconds,
                microseconds,
            } => {
                if microseconds >= 1_000_000 {
                    return None;
                }
                seconds
                    .checked_mul(1_000_000)?
                    .checked_add(u64::from(microseconds))?
            }
            crate::procs::Birth::Linux(ticks) => ticks,
        };
        Self::new(identity.pid, birth)
    }
}

/// A contract-3 claim naming its current child, sent only when
/// [`child_proof_supported`] negotiated the optional field. With no child the
/// request remains byte-for-byte the original contract-3 claim.
#[must_use]
pub fn claim_child_verb(
    sid: &str,
    pid: u32,
    scopes: &[Scope],
    version: &str,
    vendor: Option<&str>,
    child: Option<&ChildProof>,
) -> Option<String> {
    let mut verb = claim_verb(sid, pid, scopes, version, vendor)?;
    if let Some(child) = child {
        verb.push_str(&format!(" child={}:{}", child.pid, child.birth));
    }
    Some(verb)
}

/// Whether one unambiguous host capability advertises the optional native
/// child proof. An older, malformed or duplicated capability enables nothing.
#[must_use]
pub fn child_proof_supported(info_reply: &str) -> bool {
    capability(info_reply, "child-proof=")
}

fn capability(info_reply: &str, key: &str) -> bool {
    let mut words = info_reply.split_ascii_whitespace();
    if words.next() != Some("OK") {
        return false;
    }
    let mut advertised = None;
    for word in words {
        if let Some(value) = word.strip_prefix(key)
            && advertised.replace(value).is_some()
        {
            return false;
        }
    }
    advertised == Some("1")
}

/// Whether the host supports a pre-spawn output barrier as well as native child proof.
#[must_use]
pub fn child_start_supported(info_reply: &str) -> bool {
    child_proof_supported(info_reply) && capability(info_reply, "child-start=")
}

/// Begin a new child lifetime before spawning it. This intentionally names no
/// child: only a later native child tuple can finish the pending barrier.
#[must_use]
pub fn claim_child_start_verb(
    sid: &str,
    pid: u32,
    scopes: &[Scope],
    version: &str,
    vendor: Option<&str>,
    generation: u64,
) -> Option<String> {
    if generation == 0 {
        return None;
    }
    let mut verb = claim_verb(sid, pid, scopes, version, vendor)?;
    verb.push_str(&format!(" child-start={generation}"));
    Some(verb)
}

/// Acknowledged pending authority, read back before a claimed fork. Duplicate
/// barrier fields, a named child or a missing proof never establish the barrier.
#[must_use]
pub fn child_start_pending(reply: &str, holder: &str, generation: u64) -> bool {
    if generation == 0 {
        return false;
    }
    let Ok(status) = Status::parse(reply) else {
        return false;
    };
    let one = |key: &str, expected: &str| {
        let mut values = reply
            .split_ascii_whitespace()
            .filter_map(|word| word.strip_prefix(key));
        values.next() == Some(expected) && values.next().is_none()
    };
    status.held_by(holder)
        && status.takes(Scope::Relaunch)
        && status.takes(Scope::Upgrade)
        && status
            .supervisor_revision
            .is_some_and(|revision| revision > 0)
        && one("supervisor_child_start=", &generation.to_string())
        && one("supervisor_child=", "-")
}

/// The host's unique canonical child generation, including a completed bound
/// child. Zero or an absent/malformed field carries no generation evidence.
#[must_use]
pub fn child_generation(reply: &str) -> Option<u64> {
    let mut values = reply
        .split_ascii_whitespace()
        .filter_map(|word| word.strip_prefix("supervisor_child_start="));
    let value = values.next()?;
    if values.next().is_some() {
        return None;
    }
    canonical_u64(value).filter(|value| *value > 0)
}

/// Finish or renew a particular child generation; old hosts receive the
/// original child claim unchanged, and no guessed generation is added.
#[must_use]
pub fn claim_child_generation_verb(
    sid: &str,
    pid: u32,
    scopes: &[Scope],
    version: &str,
    vendor: Option<&str>,
    child: Option<&ChildProof>,
    generation: Option<u64>,
) -> Option<String> {
    let mut verb = claim_child_verb(sid, pid, scopes, version, vendor, child)?;
    if let Some(generation) = generation {
        if generation == 0 {
            return None;
        }
        verb.push_str(&format!(" child-start={generation}"));
    }
    Some(verb)
}

#[cfg(test)]
#[path = "aterm_start_tests.rs"]
mod start_tests;

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

/// The read of session `sid`'s policy, `@<sid> harness policy`, answered by a
/// [`Policy`]. `None` when `sid` is not a session ID.
#[must_use]
pub fn policy_verb(sid: &str) -> Option<String> {
    addressed(sid, "harness policy")
}

/// The read of session `sid`'s status, `@<sid> status`, answered by a
/// [`Status`]. `None` when `sid` is not a session ID.
#[must_use]
pub fn status_verb(sid: &str) -> Option<String> {
    addressed(sid, "status")
}

/// The read-only roster question, `@<sid> native-act info`, whose reply
/// [`rostered`] judges. `None` when `sid` is not a session ID.
#[must_use]
pub fn info_verb(sid: &str) -> Option<String> {
    addressed(sid, "native-act info")
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

/// `value` as one journal word: ASCII graphic bytes kept, any other byte
/// percent-encoded, cut at 64 bytes. An empty value reads `(empty)`, which no
/// wire value spells.
pub(crate) fn journal_word(value: &str) -> String {
    if value.is_empty() {
        return "(empty)".to_string();
    }
    let mut word = String::new();
    for byte in value.bytes() {
        let piece = if byte.is_ascii_graphic() && byte != b'%' {
            char::from(byte).to_string()
        } else {
            format!("%{byte:02X}")
        };
        if word.len() + piece.len() > JOURNAL_WORD_MAX {
            break;
        }
        word.push_str(&piece);
    }
    word
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

    /// Whether the policy is off by a setting a person can switch on again
    /// while the session runs: `[harness] enabled` (`disabled`), the agent's
    /// own switch (`agent-off`), or the session's Autopilot (`autopilot-off`).
    /// Each is [`Policy::off_by_configuration`]; `unhosted` and `headless`
    /// are facts of the window and stand.
    #[must_use]
    pub fn switched_off(&self) -> bool {
        !self.effective
            && matches!(
                self.why.as_deref(),
                Some("disabled" | "agent-off" | "autopilot-off")
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
    /// claim, release and replacement.
    pub supervisor_revision: Option<u64>,
    /// `agent=`: the agent's phase as aterm's reader sees it (`idle` at rest).
    pub agent: Option<String>,
    /// `composer=`: what the agent's input box holds (`empty`, `placeholder`, or
    /// a draft).
    pub composer: Option<String>,
    /// `human_ms=`.
    pub human: Human,
    /// `hand=`: who drives the session (`-` for nobody).
    pub hand: Option<String>,
    /// `hold=`: `1` while a halt stands on the session.
    pub hold: Option<String>,
    /// `input=`: `clear` once the session's program has read everything typed.
    pub input: Option<String>,
    /// `need=`: what the session waits on a person for (`-` for nothing).
    pub need: Option<String>,
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
            hand: word("hand"),
            hold: word("hold"),
            input: word("input"),
            need: word("need"),
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
/// `EVENT <local> restart cause=<why>[ to=<label> home=<path>]` line
/// (`aterm/crates/aterm-gui/src/restart_act.rs`, `record_restart` and
/// `record_scoped_account`). aterm sends it instead of signalling a harness
/// whose proven claim takes `relaunch`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Restart {
    /// Why: `asked` (a person's `aterm ctl restart`), `stall` (the stall's
    /// remedy), or an account cause. Only the cause is read; this harness keeps
    /// no account, so `to=` and `home=` are passed over.
    pub cause: String,
}

impl Restart {
    /// Read the words after `restart` in an event line. The first must be
    /// `cause=<word>`, a word of 1 to 64 ASCII alphanumerics, `-` and `_`, and no
    /// later word may name a cause again; any other trailing word is passed
    /// over. `None` otherwise: a restart nobody can name is not acted on.
    #[must_use]
    pub fn parse<'a>(mut words: impl Iterator<Item = &'a str>) -> Option<Self> {
        let cause = words.next()?.strip_prefix("cause=")?;
        if cause.is_empty()
            || cause.len() > JOURNAL_WORD_MAX
            || !cause
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
            || words.any(|word| word.starts_with("cause="))
        {
            return None;
        }
        Some(Self {
            cause: cause.to_string(),
        })
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

/// Judge one fresh `harness policy` and `status` pair as an upgrade safe point
/// for the claim `holder`, by aterm's own facts.
///
/// It is [`SafePoint::Safe`] only when all of these read exactly so, checked in
/// this order, and otherwise the first that does not is the [`Hold`]:
///
/// 1. `supervisor=<holder>`: the standing claim is this harness's;
/// 2. `supervisor_proven=1`: aterm proved it;
/// 3. `supervisor_scope` names `upgrade`: the claim took this duty;
/// 4. `contract=` serves 3 and `effective=on`: the policy still stands;
/// 5. `upgrade=true`;
/// 6. `upgrade_ask=false`: nobody has to say when;
/// 7. `agent=idle`;
/// 8. `composer=empty` or `composer=placeholder`: no draft;
/// 9. `human_ms=-`, or at least the grace: the larger of the policy's
///    `human_grace_s` and the caller's own `grace`;
/// 10. `hand=-`, `hold=0`, `input=clear`, `need=-`.
///
/// A field that is absent or unreadable holds; nothing is assumed. `grace` can
/// lengthen aterm's grace and never shorten it.
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
) -> SafePoint {
    if status.supervisor.as_deref() != Some(holder) {
        return Hold::on(
            "supervisor",
            Some(status.supervisor.as_deref().unwrap_or("-")),
        );
    }
    if !status.supervisor_proven {
        return Hold::on("supervisor_proven", Some("0"));
    }
    if !status.takes(Scope::Upgrade) {
        let scope = status.supervisor_scope.join(",");
        return Hold::on(
            "supervisor_scope",
            Some(if scope.is_empty() { "-" } else { &scope }),
        );
    }
    if !policy.serves_contract() {
        let contracts = policy
            .contracts
            .iter()
            .map(u32::to_string)
            .collect::<Vec<_>>()
            .join(",");
        return Hold::on("contract", Some(&contracts));
    }
    if !policy.effective {
        return Hold::on("effective", Some("off"));
    }
    if !policy.upgrade {
        return Hold::on("upgrade", Some("false"));
    }
    if policy.upgrade_ask {
        return Hold::on("upgrade_ask", Some("true"));
    }
    if status.agent.as_deref() != Some("idle") {
        return Hold::on("agent", status.agent.as_deref());
    }
    if !matches!(status.composer.as_deref(), Some("empty" | "placeholder")) {
        return Hold::on("composer", status.composer.as_deref());
    }
    let grace = policy.human_grace.max(grace);
    match status.human {
        Human::Never => {}
        Human::Ago(ago) if ago >= grace => {}
        Human::Ago(ago) => {
            return SafePoint::Hold(Hold {
                field: "human_ms",
                value: ago.as_millis().to_string(),
                grace_left: Some(grace.saturating_sub(ago)),
            });
        }
        Human::Unknown => return Hold::on("human_ms", Some("unknown")),
    }
    for (field, value, wanted) in [
        ("hand", status.hand.as_deref(), "-"),
        ("hold", status.hold.as_deref(), "0"),
        ("input", status.input.as_deref(), "clear"),
        ("need", status.need.as_deref(), "-"),
    ] {
        if value != Some(wanted) {
            return Hold::on(field, value);
        }
    }
    SafePoint::Safe
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_child_proof_is_negotiated_once_and_invalid_capabilities_enable_nothing() {
        assert!(child_proof_supported(
            "OK native-act child-proof=1 own-image=claude-code-alab:upgrade"
        ));
        for reply in [
            "OK own-image=claude-code-alab:upgrade",
            "ERR child-proof=1",
            "OKAY child-proof=1",
            "OK child-proof=0",
            "OK child-proof=01",
            "OK child-proof=2",
            "OK child-proof=1 child-proof=1",
            "OK child-proof=1 child-proof=0",
        ] {
            assert!(!child_proof_supported(reply), "{reply}");
        }
        let child = ChildProof::new(123, 42_000_005).unwrap();
        let old = claim_verb(SID, 4242, &[Scope::Upgrade], "0.2.0", Some("2.1.325")).unwrap();
        assert_eq!(
            claim_child_verb(SID, 4242, &[Scope::Upgrade], "0.2.0", Some("2.1.325"), None),
            Some(old.clone())
        );
        assert_eq!(
            claim_child_verb(
                SID,
                4242,
                &[Scope::Upgrade],
                "0.2.0",
                Some("2.1.325"),
                Some(&child)
            ),
            Some(format!("{old} child=123:42000005"))
        );
        for (pid, birth) in [(1, 1), (u32::MAX, 1), (123, 0)] {
            assert!(ChildProof::new(pid, birth).is_none());
        }
    }

    #[test]
    fn child_claim_birth_keeps_native_units_and_refuses_overflow() {
        let identity = |birth| crate::procs::ChildIdentity {
            pid: 123,
            parent: 42,
            pgid: 42,
            session: 42,
            birth,
            executable: crate::procs::Executable {
                path: "/vendor/claude".into(),
                device: 1,
                inode: 2,
            },
        };
        assert_eq!(
            ChildProof::from_identity(&identity(crate::procs::Birth::MacOs {
                seconds: 42,
                microseconds: 5
            })),
            Some(ChildProof::new(123, 42_000_005).unwrap())
        );
        assert_eq!(
            ChildProof::from_identity(&identity(crate::procs::Birth::Linux(42))),
            Some(ChildProof::new(123, 42).unwrap())
        );
        for birth in [
            crate::procs::Birth::MacOs {
                seconds: u64::MAX,
                microseconds: 0,
            },
            crate::procs::Birth::MacOs {
                seconds: 42,
                microseconds: 1_000_000,
            },
            crate::procs::Birth::Linux(0),
        ] {
            assert_eq!(ChildProof::from_identity(&identity(birth)), None);
        }
    }

    #[test]
    fn an_older_policy_without_contracts_admits_no_claim() {
        let older = "OK gen=12 effective=on why=config continue=on land=off questions=on";
        assert_eq!(advertised_contract(older), None);
        assert!(!may_claim(advertised_contract(older)));
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
        assert!(may_claim(Some(3)));
        assert!(!may_claim(Some(4)));
        assert!(!may_claim(Some(2)));
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
        assert!(may_claim(advertised_contract("OK contract=2,3")));
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
                hand: Some("-".to_string()),
                hold: Some("0".to_string()),
                input: Some("clear".to_string()),
                need: Some("-".to_string()),
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
                hand: None,
                hold: None,
                input: None,
                need: None,
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
        }
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
        let parse = |tail: &str| Restart::parse(tail.split_ascii_whitespace());
        for (tail, cause) in [
            ("cause=asked", "asked"),
            ("cause=stall", "stall"),
            ("cause=account to=work home=%2FUsers%2Fa", "account"),
            ("cause=account-ahead to=work home=-", "account-ahead"),
            ("cause=asked a_later_key=1 bare", "asked"),
        ] {
            assert_eq!(
                parse(tail),
                Some(Restart {
                    cause: cause.to_string()
                }),
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
        match judge_safe_point(policy, status, ME, Duration::ZERO) {
            SafePoint::Safe => "safe".to_string(),
            SafePoint::Hold(hold) => hold.to_string(),
        }
    }

    #[test]
    fn a_quiet_proven_session_is_a_safe_point() {
        let verdict = judge_safe_point(&policy(), &status(), ME, Duration::ZERO);
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
        let SafePoint::Hold(hold) = judge_safe_point(&policy(), &typed, ME, Duration::ZERO) else {
            panic!("typing 90 s ago is inside a 120 s grace");
        };
        assert_eq!((hold.field, hold.value.as_str()), ("human_ms", "90000"));
        assert_eq!(hold.grace_left, Some(Duration::from_secs(30)));
        // The caller's own grace lengthens aterm's and never shortens it.
        let aged = Status::parse(&with(LIVE_STATUS, "human_ms", Some("150000"))).unwrap();
        assert!(judge_safe_point(&policy(), &aged, ME, Duration::from_secs(1)).is_safe());
        let SafePoint::Hold(hold) =
            judge_safe_point(&policy(), &aged, ME, Duration::from_secs(200))
        else {
            panic!("150 s is inside the caller's 200 s grace");
        };
        assert_eq!(hold.grace_left, Some(Duration::from_secs(50)));
        // A grace of zero is a policy aterm may state: only `-` and any age pass.
        let none = Policy::parse(&with(LIVE_POLICY, "human_grace_s", Some("0"))).unwrap();
        let now = Status::parse(&with(LIVE_STATUS, "human_ms", Some("0"))).unwrap();
        assert!(judge_safe_point(&none, &now, ME, Duration::ZERO).is_safe());
        // Any other hold carries no grace.
        let busy = Status::parse(&with(LIVE_STATUS, "agent", Some("working"))).unwrap();
        let SafePoint::Hold(hold) = judge_safe_point(&policy(), &busy, ME, Duration::ZERO) else {
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
