// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Own-image handoff for one continuing supervisor and vendor child.
//!
//! A local supervisor can transfer its own continuing child. Within aterm no
//! host provider exists in this crate, so no move is admitted there. A
//! capsule, nonce and anonymous data descriptor authenticate nothing themselves.

use super::{Ending, Supervisor, transfer};
use crate::aterm_wire::{CustodyDeadline, CustodyFd};
use crate::cli::{self, say};
use crate::procs::{self, ChildState, ProcessIdentity};
use crate::self_image::{BuildHold, FirstExecMark, Image};
use crate::sys::{self, SignalPipe};
use crate::transfer_capsule::{self as capsule, AnonymousData, Decoder, Encoder, Header, Phase};
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsStr;
use std::fs::File;
use std::io;
use std::os::fd::{AsFd, AsRawFd, BorrowedFd, FromRawFd, IntoRawFd, OwnedFd, RawFd};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

const ENTRY: &str = "CLAUDE_ALAB_IMAGE_FD";
const PROBE: &str = "CLAUDE_ALAB_IMAGE_PROBE";
const PROBE_ARG: &str = "--alab-internal-image-probe=";
const ADOPT_ARG: &str = "--alab-internal-image-adopt";
const PROBE_HANG: Duration = Duration::from_secs(60);
const TRANSACTION: Duration = Duration::from_secs(10);
const ROLLBACK_RESERVE_NS: u64 = 5_000_000_000;
const POLL: Duration = Duration::from_millis(250);
const OUTPUT_MAX: usize = 16 * 1024;
const ENVELOPE_SCHEMA: u32 = 2;
const RETRY_FIRST: Duration = Duration::from_secs(30);
const RETRY_MAX: Duration = Duration::from_secs(5 * 60);
/// The last step of the retry schedule: the first whose doubled delay reaches
/// [`RETRY_MAX`]. A [`Backoff`] never counts past it.
const RETRY_LAST_STEP: u32 = {
    let mut step = 0;
    while RETRY_FIRST.as_secs() << step < RETRY_MAX.as_secs() {
        step += 1;
    }
    step
};

#[cfg(debug_assertions)]
#[doc(hidden)]
#[path = "supervise_image_fixture.rs"]
pub mod fixture;

#[path = "supervise_image_pending.rs"]
mod pending;

fn invalid(why: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, why)
}

/// Opaque typed-client binding context; it is not authority or authentication.
#[derive(Clone, Debug)]
pub struct HostContext {
    /// Exactly the session the provider must independently revalidate.
    pub sid: String,
    /// Provider-owned bounded binding revision/scope/locator data.
    pub binding: Vec<u8>,
}

/// Fresh local witnesses supplied to the concrete typed host provider.
pub struct Admission<'a> {
    /// The actual continuing process.
    pub process: &'a ProcessIdentity,
    /// The original independently observed vendor child.
    pub child: &'a procs::ChildIdentity,
    /// The exact immutable target being considered.
    pub target: &'a Image,
    /// The host judges this same-child operation separately from a vendor relaunch.
    pub kind: MoveKind,
    /// One original absolute transaction budget, started before prepare.
    pub deadline: CustodyDeadline,
}

/// The concrete operation submitted for typed host admission.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MoveKind {
    /// Preserve vendor PID, PTY, arguments and descendants while replacing its supervisor image.
    OwnImage,
}

/// The authoritative same-operation result after a final commit or recovery query.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CommitOutcome {
    /// The new image is sealed and its fresh binding is acknowledged.
    Committed,
    /// The host proves no commit occurred and an old-image rollback is permitted.
    ProvenNotCommittedRollbackSafe,
    /// Dispatch/ACK or recovery is ambiguous. Retain owners; never infer rollback permission.
    Unknown,
}

/// Typed host integration. Default production code installs no claim or roster
/// capability. A provider must not report ready before its real handler exists.
pub trait Host {
    /// Whether this image has a concrete host implementation, not merely a wire client.
    fn ready(&self) -> bool;
    /// Whether this provider can never admit a move for as long as this process
    /// runs: there is no handler to become ready, as opposed to one that is not
    /// ready yet (which answers `false` here and keeps being asked). The
    /// scheduler then holds before it retains, selects or probes any image,
    /// because a probe of a build that can never be adopted only runs that build
    /// and leaves its first-start mark. The default is `false`: a provider must
    /// say so itself.
    fn never_admits(&self) -> bool {
        false
    }
    /// Fresh admission bound to these exact facts; uncertainty returns false/error.
    /// Pending calls reuse the same operation and deadline, never another grant.
    fn admit(&mut self, facts: &Admission<'_>) -> io::Result<bool>;
    /// Move ALL authoritative/native-operation sockets into custody without a new claim.
    /// An error retains every original owner in this provider for reconciliation.
    fn custody(&mut self, deadline: CustodyDeadline) -> io::Result<(HostContext, Vec<CustodyFd>)>;
    /// Precommit fresh kernel proof and tentative binding only. This MUST NOT
    /// seal the native operation or retire the old rollback authority.
    /// On any lost ACK append its possibly-authoritative socket to `retained`.
    /// `Ok(false)` is an incremental pending proof, not a refusal. The caller
    /// restores ordinary supervision before waiting and authorizes final commit
    /// only after all fallible local restoration. Every connection stays owned.
    fn reprove(
        &mut self,
        context: &HostContext,
        process: &ProcessIdentity,
        child: &procs::ChildIdentity,
        phase: Phase,
        deadline: CustodyDeadline,
        retained: &mut Vec<CustodyFd>,
    ) -> io::Result<bool>;
    /// Dispatch final commit only after all fallible local restoration is done.
    /// Any error after dispatch is Unknown; it never implies rollback permission.
    /// A provider with pending reproof defers dispatch and completes the same
    /// handshake on normal recovery ticks, after ordinary supervision resumes.
    fn commit(
        &mut self,
        context: &HostContext,
        process: &ProcessIdentity,
        child: &procs::ChildIdentity,
        phase: Phase,
        deadline: CustodyDeadline,
        retained: &mut Vec<CustodyFd>,
    ) -> io::Result<CommitOutcome>;
    /// Query/recover only the SAME original operation/capability, including after
    /// its attempt budget expired. Never create a new grant, ticket or deadline.
    /// `None` is only for a still-local prepare whose provider retains its context.
    fn recover(
        &mut self,
        context: Option<&HostContext>,
        process: &ProcessIdentity,
        child: &procs::ChildIdentity,
        phase: Phase,
        original_deadline_ns: u64,
        retained: &mut Vec<CustodyFd>,
    ) -> io::Result<CommitOutcome>;
}

/// Rebuild local typed-provider state only, never an inherited request connection.
/// Construction and `ready` perform no I/O or claim; network acts belong only
/// to admission and reproof after the handler has been installed.
pub type HostFactory = fn() -> Box<dyn Host>;

struct Unavailable;
impl Host for Unavailable {
    fn ready(&self) -> bool {
        false
    }
    fn never_admits(&self) -> bool {
        true
    }
    fn admit(&mut self, _: &Admission<'_>) -> io::Result<bool> {
        Ok(false)
    }
    fn custody(&mut self, _: CustodyDeadline) -> io::Result<(HostContext, Vec<CustodyFd>)> {
        Err(invalid("typed host admission is unavailable"))
    }
    fn reprove(
        &mut self,
        _: &HostContext,
        _: &ProcessIdentity,
        _: &procs::ChildIdentity,
        _: Phase,
        _: CustodyDeadline,
        _: &mut Vec<CustodyFd>,
    ) -> io::Result<bool> {
        Err(invalid("typed host reproof is unavailable"))
    }
    fn commit(
        &mut self,
        _: &HostContext,
        _: &ProcessIdentity,
        _: &procs::ChildIdentity,
        _: Phase,
        _: CustodyDeadline,
        _: &mut Vec<CustodyFd>,
    ) -> io::Result<CommitOutcome> {
        Ok(CommitOutcome::Unknown)
    }
    fn recover(
        &mut self,
        _: Option<&HostContext>,
        _: &ProcessIdentity,
        _: &procs::ChildIdentity,
        _: Phase,
        _: u64,
        _: &mut Vec<CustodyFd>,
    ) -> io::Result<CommitOutcome> {
        Ok(CommitOutcome::Unknown)
    }
}

/// An explicit hold without socket, claim, action or capability advertisement.
pub fn unavailable_host() -> Box<dyn Host> {
    Box::new(Unavailable)
}

/// Whether this process runs in an aterm tab, by the environment aterm gives
/// the programs it starts.
pub(crate) fn hosted_by_aterm() -> bool {
    std::env::var_os("TERM_PROGRAM").as_deref() == Some(OsStr::new("aterm"))
        || std::env::var_os("ATERM_CHILD").as_deref() == Some(OsStr::new("1"))
        || std::env::var_os("ATERM_SESSION_ID").is_some()
}

/// Select local same-child authority only where aterm coordination does not
/// apply. Inside aterm a concrete host adapter is required and this crate has
/// none; no generic request connection, status reply or capability string
/// substitutes for it.
pub fn default_host() -> Box<dyn Host> {
    if hosted_by_aterm() {
        unavailable_host()
    } else {
        Box::new(LocalOwnSupervisor {
            ticket: None,
            prepared: false,
        })
    }
}

#[derive(Clone)]
struct LocalTicket {
    nonce: String,
    deadline_ns: u64,
    process: ProcessIdentity,
    child: procs::ChildIdentity,
    target: procs::Executable,
    old_version: String,
    new_version: String,
}

impl LocalTicket {
    fn context(&self) -> io::Result<HostContext> {
        let mut e = Encoder::new();
        e.u32(1)?;
        e.bytes(self.nonce.as_bytes())?;
        e.u64(self.deadline_ns)?;
        transfer::put_process(&mut e, &self.process)?;
        transfer::put_child(&mut e, &self.child)?;
        transfer::put_executable(&mut e, &self.target)?;
        e.bytes(self.old_version.as_bytes())?;
        e.bytes(self.new_version.as_bytes())?;
        Ok(HostContext {
            sid: format!("local-own-image:{}", self.process.pid),
            binding: e.finish(),
        })
    }
    fn decode(context: &HostContext) -> io::Result<Self> {
        let mut d = Decoder::new(&context.binding)?;
        if d.u32()? != 1 {
            return Err(invalid("local ownership context schema differs"));
        }
        let text = |bytes: &[u8]| {
            String::from_utf8(bytes.to_vec())
                .map_err(|_| invalid("local ownership text is not UTF-8"))
        };
        let value = Self {
            nonce: text(d.bytes(32)?)?,
            deadline_ns: d.u64()?,
            process: transfer::get_process(&mut d)?,
            child: transfer::get_child(&mut d)?,
            target: transfer::get_executable(&mut d)?,
            old_version: text(d.bytes(128)?)?,
            new_version: text(d.bytes(128)?)?,
        };
        d.finish()?;
        if !valid_nonce(&value.nonce)
            || context.sid != format!("local-own-image:{}", value.process.pid)
        {
            return Err(invalid("local ownership context does not bind its actor"));
        }
        Ok(value)
    }
    fn validate(&self, current: &ProcessIdentity, phase: Phase) -> io::Result<()> {
        if hosted_by_aterm()
            || current.pid != self.process.pid
            || current.birth != self.process.birth
            || current.pgid != self.process.pgid
            || current.session != self.process.session
            || current.executable
                != match phase {
                    Phase::Forward => self.target.clone(),
                    Phase::Rollback => self.process.executable.clone(),
                }
            || cli::VERSION
                != match phase {
                    Phase::Forward => self.new_version.as_str(),
                    Phase::Rollback => self.old_version.as_str(),
                }
            || !sys::in_foreground(0)
            || procs::observe_child(&self.child)
                .is_none_or(|seen| !matches!(seen.state, ChildState::Live | ChildState::Stopped))
        {
            return Err(invalid(
                "local same-child/foreground/image authority is not proved",
            ));
        }
        Ok(())
    }
}

struct LocalOwnSupervisor {
    ticket: Option<LocalTicket>,
    prepared: bool,
}

impl Host for LocalOwnSupervisor {
    fn ready(&self) -> bool {
        !hosted_by_aterm()
    }
    fn admit(&mut self, facts: &Admission<'_>) -> io::Result<bool> {
        if hosted_by_aterm()
            || facts.kind != MoveKind::OwnImage
            || !sys::in_foreground(0)
            || procs::capture_process(std::process::id()).as_ref() != Some(facts.process)
            || procs::observe_child(facts.child).is_none_or(|seen| seen.state != ChildState::Live)
            || facts.child.parent != facts.process.pid
        {
            return Ok(false);
        }
        facts.deadline.remaining()?;
        if let Some(ticket) = self.ticket.as_ref() {
            return Ok(ticket.deadline_ns == facts.deadline.monotonic_ns()
                && ticket.child == *facts.child
                && ticket.target == facts.target.executable);
        }
        self.ticket = Some(LocalTicket {
            nonce: nonce(),
            deadline_ns: facts.deadline.monotonic_ns(),
            process: facts.process.clone(),
            child: facts.child.clone(),
            target: facts.target.executable.clone(),
            old_version: cli::VERSION.to_owned(),
            new_version: facts.target.version.clone(),
        });
        Ok(true)
    }
    fn custody(&mut self, deadline: CustodyDeadline) -> io::Result<(HostContext, Vec<CustodyFd>)> {
        let ticket = self
            .ticket
            .as_ref()
            .ok_or_else(|| invalid("no local ownership transaction exists"))?;
        if ticket.deadline_ns != deadline.monotonic_ns() {
            return Err(invalid("local ownership deadline changed"));
        }
        Ok((ticket.context()?, Vec::new()))
    }
    fn reprove(
        &mut self,
        context: &HostContext,
        current: &ProcessIdentity,
        child: &procs::ChildIdentity,
        phase: Phase,
        deadline: CustodyDeadline,
        retained: &mut Vec<CustodyFd>,
    ) -> io::Result<bool> {
        if !retained.is_empty() {
            return Err(invalid("local authority cannot adopt host sockets"));
        }
        let ticket = LocalTicket::decode(context)?;
        if ticket.deadline_ns != deadline.monotonic_ns() || ticket.child != *child {
            return Err(invalid(
                "local ownership child or deadline changed across exec",
            ));
        }
        ticket.validate(current, phase)?;
        self.ticket = Some(ticket);
        self.prepared = true;
        Ok(true)
    }
    fn commit(
        &mut self,
        context: &HostContext,
        current: &ProcessIdentity,
        child: &procs::ChildIdentity,
        phase: Phase,
        deadline: CustodyDeadline,
        retained: &mut Vec<CustodyFd>,
    ) -> io::Result<CommitOutcome> {
        if !self.prepared || !retained.is_empty() {
            return Err(invalid("local ownership is not ready"));
        }
        deadline.remaining()?;
        let ticket = LocalTicket::decode(context)?;
        if ticket.deadline_ns != deadline.monotonic_ns() || ticket.child != *child {
            return Err(invalid("local ownership commit child/deadline changed"));
        }
        ticket.validate(current, phase)?;
        self.ticket = None;
        self.prepared = false;
        Ok(match phase {
            Phase::Forward => CommitOutcome::Committed,
            Phase::Rollback => CommitOutcome::ProvenNotCommittedRollbackSafe,
        })
    }
    fn recover(
        &mut self,
        context: Option<&HostContext>,
        current: &ProcessIdentity,
        child: &procs::ChildIdentity,
        phase: Phase,
        original_deadline_ns: u64,
        retained: &mut Vec<CustodyFd>,
    ) -> io::Result<CommitOutcome> {
        if !retained.is_empty() {
            return Err(invalid("local recovery cannot own host sockets"));
        }
        if context.is_none() && self.ticket.is_none() && !self.prepared {
            // Admission may have refused before recording any local act. There
            // is no external operation or socket to reconcile in this case.
            return if !hosted_by_aterm()
                && phase == Phase::Rollback
                && procs::capture_process(std::process::id()).as_ref() == Some(current)
                && child.parent == current.pid
                && procs::observe_child(child).is_some_and(|seen| {
                    matches!(seen.state, ChildState::Live | ChildState::Stopped)
                }) {
                Ok(CommitOutcome::ProvenNotCommittedRollbackSafe)
            } else {
                Err(invalid("local refused admission cannot yet be reconciled"))
            };
        }
        let ticket = match context {
            Some(context) => LocalTicket::decode(context)?,
            None => self
                .ticket
                .clone()
                .ok_or_else(|| invalid("no local operation to recover"))?,
        };
        if ticket.deadline_ns != original_deadline_ns || ticket.child != *child {
            return Err(invalid("local recovery child/deadline changed"));
        }
        ticket.validate(current, phase)?;
        self.ticket = None;
        self.prepared = false;
        Ok(match phase {
            Phase::Forward => CommitOutcome::Committed,
            Phase::Rollback => CommitOutcome::ProvenNotCommittedRollbackSafe,
        })
    }
}

fn nonce() -> String {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let time = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| duration.as_nanos());
    format!(
        "{:032x}",
        time ^ (u128::from(std::process::id()) << 64)
            ^ u128::from(NEXT.fetch_add(1, Ordering::Relaxed))
    )
}

fn valid_nonce(value: &str) -> bool {
    value.len() == 32
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
}

fn signal_abi() -> String {
    sys::HANDLED
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(",")
}

#[derive(Debug)]
struct Capability {
    host: bool,
}

impl Capability {
    fn reply(nonce: &str, factory: HostFactory) -> io::Result<String> {
        Ok(format!(
            "claude-image-probe-v1 nonce={nonce} pid={} version={} capsule={} payload={} envelope={} signals={} terminal={} host={}\n",
            std::process::id(),
            cli::VERSION,
            capsule::ABI_VERSION,
            transfer::SCHEMA,
            ENVELOPE_SCHEMA,
            signal_abi(),
            sys::terminal_snapshot_abi()?,
            u8::from(factory().ready())
        ))
    }
    fn parse(bytes: &[u8], nonce: &str, pid: i32, image: &Image) -> io::Result<Self> {
        let line = std::str::from_utf8(bytes).map_err(|_| invalid("probe reply is not UTF-8"))?;
        if !line.ends_with('\n') || line[..line.len().saturating_sub(1)].contains(['\n', '\r']) {
            return Err(invalid("probe reply is not exactly one line"));
        }
        let mut words = line.split_ascii_whitespace();
        if words.next() != Some("claude-image-probe-v1") {
            return Err(invalid("probe protocol differs"));
        }
        let mut fields = BTreeMap::new();
        for word in words {
            let (name, value) = word
                .split_once('=')
                .ok_or_else(|| invalid("malformed probe field"))?;
            if fields.insert(name, value).is_some() {
                return Err(invalid("duplicate probe field"));
            }
        }
        let expected = [
            ("nonce", nonce.to_owned()),
            ("pid", pid.to_string()),
            ("version", image.version.clone()),
            ("capsule", capsule::ABI_VERSION.to_string()),
            ("payload", transfer::SCHEMA.to_string()),
            ("envelope", ENVELOPE_SCHEMA.to_string()),
            ("signals", signal_abi()),
            ("terminal", sys::terminal_snapshot_abi()?),
        ];
        if fields.len() != expected.len() + 1
            || expected
                .iter()
                .any(|(key, value)| fields.get(key).copied() != Some(value.as_str()))
        {
            return Err(invalid("probe image, nonce or transfer ABI differs"));
        }
        let host = match fields.get("host").copied() {
            Some("0") => false,
            Some("1") => true,
            _ => return Err(invalid("probe host capability is invalid")),
        };
        Ok(Self { host })
    }
}

struct Probe {
    image: Image,
    hold: BuildHold,
    mark: Option<FirstExecMark>,
    child: Option<sys::ProbeChild>,
    pid: Option<i32>,
    signals: Option<sys::AllSignals>,
    nonce: String,
    until: Instant,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    cancelled: bool,
    failed: Option<String>,
    ended: Option<std::process::ExitStatus>,
}

impl Probe {
    fn start(image: Image) -> io::Result<Self> {
        let hold = image.retain()?;
        let mark = FirstExecMark::before_probe(&image)?;
        let nonce = nonce();
        let changes = [
            (PROBE.into(), nonce.clone().into()),
            (
                cli::PARENT_VAR.into(),
                std::process::id().to_string().into(),
            ),
            (cli::DEPTH_VAR.into(), cli::MAX_DEPTH.to_string().into()),
        ];
        // Old harnesses reject both the parent/depth guards and this reserved
        // leading option before any vendor spawn. New code routes it first.
        let (child, signals, failed) = match sys::ProbeChild::spawn(
            &image.executable.path,
            &[format!("{PROBE_ARG}{nonce}").into()],
            &changes,
        ) {
            Ok(child) => (Some(child), None, None),
            Err(refusal) if refusal.child.is_some() || refusal.signals.is_some() => (
                refusal.child.map(|child| *child),
                refusal.signals.map(|block| *block),
                Some(refusal.error.to_string()),
            ),
            Err(refusal) => return Err(refusal.error),
        };
        let pid = child.as_ref().and_then(sys::ProbeChild::pid);
        Ok(Self {
            image,
            hold,
            mark: Some(mark),
            child,
            pid,
            signals,
            nonce,
            until: Instant::now() + PROBE_HANG,
            stdout: Vec::new(),
            stderr: Vec::new(),
            cancelled: false,
            failed,
            ended: None,
        })
    }
    fn poll(&mut self) -> io::Result<Option<Capability>> {
        if let Some(block) = self.signals.as_mut() {
            match block.restore() {
                Ok(()) => self.signals = None,
                Err(error) => {
                    self.failed.get_or_insert(error.to_string());
                }
            }
        }
        let Some(child) = self.child.as_mut() else {
            return if self.signals.is_some() {
                Ok(None)
            } else {
                Err(invalid(
                    self.failed.as_deref().unwrap_or("probe did not start"),
                ))
            };
        };
        if self.failed.is_none()
            && let Err(error) = child.drain(&mut self.stdout, &mut self.stderr, OUTPUT_MAX)
        {
            self.failed = Some(error.to_string());
        }
        // Even output/cancellation failures must reach the exact child's wait.
        // Never drop a still-owned PID merely because its reader failed.
        if self.ended.is_none() {
            self.ended = match child.try_wait() {
                Ok(status) => status,
                Err(error) => {
                    self.failed.get_or_insert(error.to_string());
                    return Ok(None);
                }
            };
        }
        // Observe an already completed proof before cancellation: a suspended
        // parent must not reject output merely because its next poll was late.
        if self.ended.is_none()
            && (self.failed.is_some() || Instant::now() >= self.until)
            && !self.cancelled
        {
            match child.cancel() {
                Ok(()) => self.cancelled = true,
                Err(error) => {
                    self.failed.get_or_insert(error.to_string());
                }
            }
        }
        let Some(status) = self.ended else {
            return Ok(None);
        };
        if self.signals.is_some() {
            return Ok(None);
        }
        if self.cancelled || self.failed.is_some() || !status.success() {
            return Err(invalid(
                self.failed
                    .as_deref()
                    .unwrap_or("probe ended without execution proof"),
            ));
        }
        child.drain(&mut self.stdout, &mut self.stderr, OUTPUT_MAX)?;
        self.hold.revalidate()?;
        let capability = Capability::parse(
            &self.stdout,
            &self.nonce,
            self.pid.ok_or_else(|| invalid("probe pid absent"))?,
            &self.image,
        )?;
        // Exact target code produced the nonce-bound reply. Busy cleanup keeps
        // the persistent mark rather than weakening retention.
        if let Some(mark) = self.mark.take() {
            let _ = mark.ran();
        }
        Ok(Some(capability))
    }
    fn pending(&self) -> bool {
        self.signals.is_some() || self.child.as_ref().is_some_and(sys::ProbeChild::pending)
    }
    fn cancel(&mut self) {
        self.failed
            .get_or_insert_with(|| "probe cancelled by continuing supervisor".into());
        if let Some(child) = &mut self.child
            && child.cancel().is_ok()
        {
            self.cancelled = true;
        }
    }
}

#[derive(Clone)]
struct Envelope {
    prefix: PathBuf,
    old_build: u64,
    new_build: u64,
    deadline_ns: u64,
    revisions: [[u64; 5]; 2],
    /// The step the attempt [`Backoff`] stood at when this transaction was
    /// dispatched, at most [`RETRY_LAST_STEP`]. An old image entered again by
    /// rollback continues its schedule from here, so a target that keeps
    /// failing after the exec is retried at a growing interval.
    retry_step: u32,
    context: HostContext,
    // This byte vector is NEVER recaptured or replaced for rollback.
    original: Vec<u8>,
}

impl Envelope {
    fn encode(&self) -> io::Result<Vec<u8>> {
        if self.context.sid.is_empty()
            || self.context.sid.len() > 256
            || self.context.sid.contains(['\0', '\n', '\r'])
            || self.context.binding.len() > OUTPUT_MAX
        {
            return Err(invalid("invalid typed host context"));
        }
        if self.retry_step > RETRY_LAST_STEP {
            return Err(invalid("image envelope retry step is out of range"));
        }
        let mut e = Encoder::new();
        e.u32(ENVELOPE_SCHEMA)?;
        e.os_string(self.prefix.as_os_str())?;
        e.u64(self.old_build)?;
        e.u64(self.new_build)?;
        e.u64(self.deadline_ns)?;
        for revision in self.revisions {
            for field in revision {
                e.u64(field)?;
            }
        }
        e.u32(self.retry_step)?;
        e.bytes(self.context.sid.as_bytes())?;
        e.bytes(&self.context.binding)?;
        e.bytes(&self.original)?;
        Ok(e.finish())
    }
    fn decode(bytes: &[u8]) -> io::Result<Self> {
        let mut d = Decoder::new(bytes)?;
        if d.u32()? != ENVELOPE_SCHEMA {
            return Err(invalid("image envelope schema differs"));
        }
        let value = Self {
            prefix: d.os_string(16 * 1024)?.into(),
            old_build: d.u64()?,
            new_build: d.u64()?,
            deadline_ns: d.u64()?,
            revisions: [
                [d.u64()?, d.u64()?, d.u64()?, d.u64()?, d.u64()?],
                [d.u64()?, d.u64()?, d.u64()?, d.u64()?, d.u64()?],
            ],
            retry_step: d.u32()?,
            context: HostContext {
                sid: String::from_utf8(d.bytes(256)?.to_vec())
                    .map_err(|_| invalid("host sid is not UTF-8"))?,
                binding: d.bytes(OUTPUT_MAX)?.to_vec(),
            },
            original: d.bytes(capsule::MAX_PAYLOAD_BYTES)?.to_vec(),
        };
        d.finish()?;
        if !value.prefix.is_absolute()
            || value.old_build == 0
            || value.new_build == 0
            || value.old_build == value.new_build
        {
            return Err(invalid("image envelope build pair is invalid"));
        }
        // Encoding repeats every bound, the retry step's among them.
        value.encode()?;
        Ok(value)
    }
    fn recorded_deadline(&self) -> io::Result<CustodyDeadline> {
        let deadline = CustodyDeadline::from_recorded_monotonic_ns(self.deadline_ns)?;
        match deadline.remaining() {
            Ok(left) if left > TRANSACTION => {
                return Err(invalid(
                    "handoff deadline exceeds its original transaction bound",
                ));
            }
            Err(error) if error.kind() != io::ErrorKind::TimedOut => return Err(error),
            _ => {}
        }
        Ok(deadline)
    }
    fn deadline(&self) -> io::Result<CustodyDeadline> {
        let deadline = self.recorded_deadline()?;
        deadline.remaining()?;
        Ok(deadline)
    }
    fn forward_remaining(&self) -> io::Result<Duration> {
        let remaining = self.deadline()?.remaining()?;
        remaining
            .checked_sub(Duration::from_nanos(ROLLBACK_RESERVE_NS))
            .filter(|left| !left.is_zero())
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::TimedOut,
                    "forward budget reached rollback reserve",
                )
            })
    }
}

struct Compatible {
    image: Image,
    hold: BuildHold,
}

#[derive(Clone)]
struct PendingAdmission {
    target: Image,
    child: procs::ChildIdentity,
    deadline: CustodyDeadline,
}

struct Recovery {
    envelope: Envelope,
    phase: Phase,
    other_gate: Option<BuildHold>,
    rollback_attempt: RollbackAttempt,
}

impl Recovery {
    fn from_entry(
        envelope: Envelope,
        phase: Phase,
        other_gate: Option<BuildHold>,
        rollback_attempt: RollbackAttempt,
    ) -> Self {
        Self {
            envelope,
            phase,
            other_gate,
            rollback_attempt,
        }
    }

    fn may_rollback(&self, decision: CommitOutcome) -> bool {
        self.phase == Phase::Forward
            && decision == CommitOutcome::ProvenNotCommittedRollbackSafe
            && !self.rollback_attempt.consumed
    }

    fn dispatch_rollback(
        &mut self,
        decision: CommitOutcome,
        dispatch: impl FnOnce(&Envelope, Option<&BuildHold>) -> io::Error,
    ) -> Option<io::Error> {
        if !self.may_rollback(decision) {
            return None;
        }
        if !self.rollback_attempt.consume() {
            return None;
        }
        Some(dispatch(&self.envelope, self.other_gate.as_ref()))
    }
}

/// One original transaction permits at most one rollback dispatch. A returned
/// exec error consumes it just as an entry into the old image does.
#[derive(Default)]
struct RollbackAttempt {
    consumed: bool,
}

impl RollbackAttempt {
    fn for_entry(phase: Phase) -> Self {
        Self {
            consumed: phase == Phase::Rollback,
        }
    }

    /// Take the one dispatch: true the first time, false once consumed.
    const fn consume(&mut self) -> bool {
        if self.consumed {
            return false;
        }
        self.consumed = true;
        true
    }
}

/// One retry schedule: [`RETRY_FIRST`], doubling with every deferral up to
/// [`RETRY_MAX`].
#[derive(Default)]
struct Backoff {
    /// When the next try may run; `None` when it may run now.
    after: Option<Instant>,
    /// How many deferrals lie behind, at most [`RETRY_LAST_STEP`]: the next
    /// one waits [`Backoff::delay`].
    step: u32,
}
impl Backoff {
    /// A schedule that is ready now and standing at `step`: `None` past
    /// [`RETRY_LAST_STEP`], which no schedule reaches.
    fn at_step(step: u32) -> Option<Self> {
        (step <= RETRY_LAST_STEP).then_some(Self { after: None, step })
    }
    /// How long the next deferral waits.
    fn delay(&self) -> Duration {
        RETRY_FIRST
            .saturating_mul(1u32.checked_shl(self.step).unwrap_or(u32::MAX))
            .min(RETRY_MAX)
    }
    fn ready(&self, now: Instant) -> bool {
        self.after.is_none_or(|after| now >= after)
    }
    fn defer(&mut self, now: Instant) {
        self.after = now.checked_add(self.delay());
        self.step = self.step.saturating_add(1).min(RETRY_LAST_STEP);
    }
}

/// The probe and attempt schedules an image entered in `phase` resumes
/// ordinary supervision with. A forward entry runs the target, so both start
/// afresh. A rollback entry is the old image again after its target failed
/// past the exec: its attempt schedule continues from `carried_step`, the step
/// the transaction was dispatched at ([`Envelope::retry_step`]), not from the
/// start, or a target that passes its probe and fails after every exec would
/// be retried at the first interval for ever. When the entry is `ready` (the
/// rollback is proved) both are deferred now, as [`defer_rollback_retries`]
/// does later for one whose proof is still pending.
fn entry_retries(phase: Phase, carried_step: u32, ready: bool, now: Instant) -> (Backoff, Backoff) {
    let mut probe_retry = Backoff::default();
    let mut attempt_retry = match phase {
        Phase::Forward => Backoff::default(),
        // Decoding bounded the step; one out of range is the longest wait.
        Phase::Rollback => Backoff::at_step(carried_step).unwrap_or(Backoff {
            after: None,
            step: RETRY_LAST_STEP,
        }),
    };
    if ready {
        defer_rollback_retries(phase, now, &mut probe_retry, &mut attempt_retry);
    }
    (probe_retry, attempt_retry)
}

fn defer_rollback_retries(
    phase: Phase,
    now: Instant,
    probe_retry: &mut Backoff,
    attempt_retry: &mut Backoff,
) {
    if phase == Phase::Rollback {
        probe_retry.defer(now);
        attempt_retry.defer(now);
    }
}

/// One single-threaded supervisor's scheduler. Unknown required host witnesses
/// hold the move; local probing never grants authority or advertises a claim.
pub struct Runtime {
    host: Box<dyn Host>,
    running: Option<BuildHold>,
    probe: Option<Probe>,
    compatible: Option<Compatible>,
    seen: Option<Image>,
    probe_retry: Backoff,
    probe_rejected: bool,
    attempt_retry: Backoff,
    retained: Vec<CustodyFd>,
    held: Option<String>,
    attempted: Option<Image>,
    admission: Option<PendingAdmission>,
    recovery: Option<Recovery>,
}

impl Default for Runtime {
    fn default() -> Self {
        Self::new(unavailable_host)
    }
}

impl Runtime {
    /// Construct local state only; this opens no host socket and publishes no capability.
    pub fn new(factory: HostFactory) -> Self {
        Self {
            host: factory(),
            running: None,
            probe: None,
            compatible: None,
            seen: None,
            probe_retry: Backoff::default(),
            probe_rejected: false,
            attempt_retry: Backoff::default(),
            retained: Vec::new(),
            held: None,
            attempted: None,
            admission: None,
            recovery: None,
        }
    }
    fn note(&mut self, supervisor: &Supervisor, why: String) {
        if self.held.as_ref() != Some(&why) {
            supervisor.journal.note(&format!("harness-image {why}"));
            self.held = Some(why);
        }
    }
    pub(super) fn cancel_probe(&mut self) {
        if let Some(probe) = &mut self.probe {
            probe.cancel();
        }
    }
    /// The vendor has already ended. Keep the auxiliary child's wait ownership
    /// until its terminal event is consumed, including a slow OS assessment.
    pub(super) fn finish_probe(&mut self) {
        self.cancel_probe();
        let detector = Instant::now() + PROBE_HANG;
        let mut warned = false;
        while let Some(probe) = self.probe.as_mut() {
            let result = probe.poll();
            if !probe.pending() {
                self.probe = None;
                break;
            }
            if !warned && Instant::now() >= detector {
                say(
                    "claude-alab: waiting for the owned capability probe to exit; its image remains retained",
                );
                warned = true;
            }
            if result.is_err() {
                probe.cancel();
            }
            std::thread::sleep(POLL);
        }
    }
    pub(super) fn tick(&mut self, supervisor: &mut Supervisor, pipe: &mut SignalPipe) {
        let moves_allowed = sys::stdin_is_terminal()
            && supervisor.ending == Ending::None
            && supervisor.restart_deferred.is_none()
            && supervisor.moved.is_none()
            && !supervisor.child.stopped;
        if self.recovery.is_some() {
            self.recover(supervisor, pipe);
            return;
        }
        if let Some(original) = self.admission.as_ref() {
            let facts = pending::Facts {
                selected: Image::selected(&supervisor.setup.home),
                moves_allowed,
                child_unchanged: supervisor.child.identity.as_ref() == Some(&original.child)
                    && procs::observe_child(&original.child)
                        .is_some_and(|seen| seen.state == ChildState::Live),
                usable_budget: original
                    .deadline
                    .remaining()
                    .is_ok_and(|left| left > Duration::from_nanos(ROLLBACK_RESERVE_NS)),
            };
            self.service_pending(supervisor, facts, |runtime, supervisor, target| {
                runtime.attempt(supervisor, pipe, target)
            });
            return;
        }
        if !self.retained.is_empty() {
            self.note(supervisor, "held unresolved-authority-custody".into());
            return;
        }
        if self.probe.is_some() {
            self.poll_probe(supervisor);
            if self.probe.is_some() {
                return;
            }
        }
        if !moves_allowed || self.hold_without_host(supervisor) {
            return;
        }
        if self.running.is_none() {
            let running = procs::capture_process(std::process::id())
                .ok_or_else(|| invalid("own identity unreadable"))
                .and_then(|process| Image::running(&supervisor.setup.home, &process, cli::VERSION))
                .and_then(|image| image.retain());
            match running {
                Ok(hold) => self.running = Some(hold),
                Err(error) => {
                    self.note(supervisor, format!("held running-image {error}"));
                    return;
                }
            }
        }
        let target = match Image::selected(&supervisor.setup.home) {
            Ok(image) => image,
            Err(error) => {
                self.note(supervisor, format!("held selection {error}"));
                return;
            }
        };
        if self
            .running
            .as_ref()
            .is_some_and(|hold| hold.image() == &target)
        {
            return;
        }
        self.select(&target);
        if self.compatible.is_none() {
            if self.probe_rejected || !self.probe_retry.ready(Instant::now()) {
                return;
            }
            match Probe::start(target) {
                Ok(probe) => {
                    self.probe = Some(probe);
                    self.note(supervisor, "probing".into());
                }
                Err(error) => {
                    // Probe::start returns Err only with no child or mask
                    // custody. A postfork refusal remains an owned Probe.
                    self.probe_retry.defer(Instant::now());
                    self.note(supervisor, format!("held probe-start {error}"));
                }
            }
            return;
        }
        let Some(compatible) = self.compatible.as_ref() else {
            return;
        };
        if self.attempted.as_ref() == Some(&compatible.image)
            || !self.attempt_retry.ready(Instant::now())
        {
            return;
        }
        if !self.host.ready() {
            self.note(supervisor, "held host-admission-unknown".into());
            return;
        }
        if let Err(error) = self.attempt(supervisor, pipe, compatible.image.clone()) {
            self.attempt_failed(supervisor, &error, Instant::now());
        }
    }

    /// `target` is the selected image now. A change of target forgets what was
    /// learned of the last one, and where there was a last one both retry
    /// schedules start afresh: a wait earned by one target is not served by
    /// another. An unchanged target keeps its schedules, also across a rollback
    /// into this image ([`entry_retries`]).
    fn select(&mut self, target: &Image) {
        if self.seen.as_ref() != Some(target) {
            self.compatible = None;
            if self.seen.is_some() {
                self.probe_retry = Backoff::default();
                self.attempt_retry = Backoff::default();
            }
            self.probe_rejected = false;
            self.attempted = None;
            self.seen = Some(target.clone());
        }
    }

    /// Hold every own-image move when the host can never admit one
    /// ([`Host::never_admits`]): one journal line per change, and no image is
    /// retained, selected or probed. `true` when the tick ends here.
    fn hold_without_host(&mut self, supervisor: &Supervisor) -> bool {
        let never = self.host.never_admits();
        if never {
            self.note(supervisor, "held host-never-admits".into());
        }
        never
    }

    /// A fresh attempt returned `error`. One that ended before any admission
    /// or recovery existed (not foreground, an unreadable identity, a child
    /// that is not live) left nothing for a later tick to service, so its
    /// retry is deferred like every other; without that the same witnesses
    /// would be read again on every tick. A pending admission or a recovery is
    /// serviced first by the next tick and keeps its own clock.
    fn attempt_failed(&mut self, supervisor: &Supervisor, error: &io::Error, now: Instant) {
        if self.admission.is_none() && self.recovery.is_none() {
            self.attempt_retry.defer(now);
        }
        self.note(supervisor, format!("held transfer {error}"));
    }

    fn poll_probe(&mut self, supervisor: &Supervisor) {
        if let Some(mut probe) = self.probe.take() {
            match probe.poll() {
                Ok(None) => {
                    self.probe = Some(probe);
                }
                Ok(Some(capability)) if capability.host => {
                    self.compatible = Some(Compatible {
                        image: probe.image,
                        hold: probe.hold,
                    });
                }
                Ok(Some(_)) => {
                    self.probe_rejected = true;
                    self.note(supervisor, "held target-host-unavailable".into());
                }
                Err(error) => {
                    probe.cancel();
                    if probe.pending() {
                        self.probe = Some(probe);
                    } else {
                        // Only an observed terminal child and restored mask
                        // allow a later probe, never cancellation alone.
                        self.probe_retry.defer(Instant::now());
                    }
                    self.note(supervisor, format!("held probe {error}"));
                }
            }
        }
    }

    pub(super) fn blocks_vendor_move(&self) -> bool {
        self.admission.is_some() || self.recovery.is_some() || !self.retained.is_empty()
    }

    fn service_pending(
        &mut self,
        supervisor: &mut Supervisor,
        facts: pending::Facts,
        dispatch: impl FnOnce(&mut Self, &mut Supervisor, Image) -> io::Result<()>,
    ) {
        // The caller services only a pending operation; without one there is nothing to do.
        let original = match &self.admission {
            Some(admission) => admission.clone(),
            None => return,
        };
        match pending::decide(
            &original,
            &facts,
            self.attempted.is_some(),
            !self.retained.is_empty(),
        ) {
            pending::Decision::ContinueOriginal => {
                // The unchanged queued prepare must progress on its original
                // provider operation and deadline, without applying new-target
                // retry/selection skips or minting a fresh transaction.
                if let Err(error) = dispatch(self, supervisor, original.target) {
                    self.note(supervisor, format!("held pending transfer {error}"));
                }
            }
            pending::Decision::ReconcileOriginal => self.recover_pending(supervisor),
        }
    }

    fn recover_pending(&mut self, supervisor: &Supervisor) {
        let Some(pending) = self.admission.as_ref() else {
            return;
        };
        let Some(process) = procs::capture_process(std::process::id()) else {
            return;
        };
        match self.host.recover(
            None,
            &process,
            &pending.child,
            Phase::Rollback,
            pending.deadline.monotonic_ns(),
            &mut self.retained,
        ) {
            Ok(CommitOutcome::ProvenNotCommittedRollbackSafe) => {
                self.attempted = None;
                self.attempt_retry.defer(Instant::now());
                self.admission = None;
                self.retained.clear();
                self.note(supervisor, "prepare-aborted".into());
            }
            _ => self.note(supervisor, "held pending-native-operation".into()),
        }
    }

    fn recover(&mut self, supervisor: &mut Supervisor, pipe: &mut SignalPipe) {
        // The caller recovers only a retained transaction; without one there is nothing to do.
        let Some(mut recovery) = self.recovery.take() else {
            return;
        };
        let Some(process) = procs::capture_process(std::process::id()) else {
            self.recovery = Some(recovery);
            return;
        };
        let Some(child) = supervisor.child.identity.as_ref() else {
            self.recovery = Some(recovery);
            return;
        };
        let decision = self
            .host
            .recover(
                Some(&recovery.envelope.context),
                &process,
                child,
                recovery.phase,
                recovery.envelope.deadline_ns,
                &mut self.retained,
            )
            .unwrap_or(CommitOutcome::Unknown);
        let ready = matches!(
            (recovery.phase, decision),
            (Phase::Forward, CommitOutcome::Committed)
                | (
                    Phase::Rollback,
                    CommitOutcome::ProvenNotCommittedRollbackSafe
                )
        );
        if ready {
            self.retained.clear();
            self.admission = None;
            self.attempted = None;
            defer_rollback_retries(
                recovery.phase,
                Instant::now(),
                &mut self.probe_retry,
                &mut self.attempt_retry,
            );
            self.note(supervisor, "recovered-ready".into());
            return;
        }
        if recovery.may_rollback(decision) {
            let new = self.running.as_ref();
            if let (true, Some(new), Ok(deadline)) = (
                recovery.other_gate.is_some(),
                new,
                recovery.envelope.deadline(),
            ) {
                match self.host.custody(deadline) {
                    Ok((_, mut owners))
                        if owners
                            .len()
                            .checked_add(self.retained.len())
                            .is_some_and(|held| held <= capsule::MAX_AUTHORITIES)
                            && owners.iter().all(|owner| owner.deadline() == deadline) =>
                    {
                        self.retained.append(&mut owners)
                    }
                    Ok((_, owners)) => {
                        let _owners = owners;
                        park(
                            "recovery custody exceeded budget/roles",
                            supervisor.child.identity.as_ref(),
                            supervisor.termios.as_ref(),
                            Some(HeldSignals::of_pipe(pipe)),
                        );
                    }
                    Err(error) => {
                        self.note(supervisor, format!("held recovery custody {error}"));
                        self.recovery = Some(recovery);
                        return;
                    }
                }
                if let Some(error) = recovery.dispatch_rollback(decision, |envelope, old| {
                    let Some(old) = old else {
                        return invalid("old image retention missing during rollback");
                    };
                    rollback_exec(
                        envelope,
                        Some(pipe),
                        &[None, None],
                        &self.retained,
                        &[old, new],
                        supervisor.child.identity.as_ref(),
                    )
                }) {
                    self.note(supervisor, format!("held recovered rollback {error}"));
                }
            }
        }
        self.recovery = Some(recovery);
        self.note(supervisor, "held native-commit-outcome-unknown".into());
    }

    fn attempt(
        &mut self,
        supervisor: &mut Supervisor,
        pipe: &mut SignalPipe,
        target: Image,
    ) -> io::Result<()> {
        if !sys::in_foreground(0) {
            return Err(invalid("own image is not foreground"));
        }
        let process = procs::capture_process(std::process::id())
            .ok_or_else(|| invalid("own kernel identity unavailable"))?;
        let child = supervisor
            .child
            .identity
            .clone()
            .ok_or_else(|| invalid("original child identity unavailable"))?;
        // A copy: a hold below restores these modes without borrowing the supervisor.
        let terminal = supervisor.termios;
        if procs::observe_child(&child).is_none_or(|seen| seen.state != ChildState::Live) {
            return Err(invalid("same child is not live"));
        }
        if let Some(pending) = self.admission.as_ref()
            && (pending.target != target
                || pending.child != child
                || pending.deadline.remaining().map_or(true, |remaining| {
                    remaining <= Duration::from_nanos(ROLLBACK_RESERVE_NS)
                }))
        {
            self.recover_pending(supervisor);
            return Err(invalid(
                "pending admission changed or reached its rollback reserve",
            ));
        }
        let deadline = match self.admission.as_ref() {
            Some(pending) => pending.deadline,
            None => {
                // Keep the original nanoseconds verbatim. The typed wire
                // adapter alone floors ns/1000 when encoding deadline-us.
                let deadline = CustodyDeadline::after(TRANSACTION)?;
                self.admission = Some(PendingAdmission {
                    target: target.clone(),
                    child: child.clone(),
                    deadline,
                });
                deadline
            }
        };
        if !self.host.admit(&Admission {
            process: &process,
            child: &child,
            target: &target,
            kind: MoveKind::OwnImage,
            deadline,
        })? {
            return Err(invalid("same-child host act admission is unknown"));
        }
        // Keep the ordinary signal/terminal event path authoritative until the
        // exact instant signals are blocked. Nothing is silently discarded.
        for sig in pipe.wait(0) {
            match sig {
                libc::SIGHUP | libc::SIGTERM => supervisor.pass_on(sig),
                libc::SIGTSTP => supervisor.on_tstp(),
                libc::SIGCONT if supervisor.child.stopped => {
                    let _ = supervisor.signal_child(libc::SIGCONT);
                }
                _ => {}
            }
        }
        supervisor.reap();
        if supervisor.ending != Ending::None
            || supervisor.restart_deferred.is_some()
            || supervisor.moved.is_some()
            || supervisor.child.stopped
        {
            return Err(invalid("child stopped or is ending"));
        }
        if !sys::in_foreground(0)
            || procs::observe_child(&child).is_none_or(|seen| seen.state != ChildState::Live)
            || Image::selected(&supervisor.setup.home)? != target
        {
            return Err(invalid(
                "foreground, child identity or selection changed while deciding",
            ));
        }
        let old = self
            .running
            .as_ref()
            .ok_or_else(|| invalid("old image retention missing"))?;
        let new = self
            .compatible
            .as_ref()
            .ok_or_else(|| invalid("compatible target retention missing"))?;
        if new.image != target
            || old.image().executable != process.executable
            || old.image().version != cli::VERSION
        {
            return Err(invalid("running image or target changed"));
        }
        old.revalidate()?;
        new.hold.revalidate()?;
        self.attempted = Some(target.clone());
        let (context, authorities) = self.host.custody(deadline)?;
        let mut envelope = Envelope {
            prefix: old.image().prefix.clone(),
            old_build: old.image().build,
            new_build: target.build,
            deadline_ns: deadline.monotonic_ns(),
            revisions: [old.image().file_revision, target.file_revision],
            retry_step: self.attempt_retry.step,
            context,
            original: Vec::new(),
        };
        self.recovery = Some(Recovery {
            envelope: envelope.clone(),
            phase: Phase::Rollback,
            other_gate: None,
            rollback_attempt: RollbackAttempt::default(),
        });
        self.admission = None;
        if authorities.len() > capsule::MAX_AUTHORITIES
            || authorities.iter().any(|owner| owner.deadline() != deadline)
        {
            self.retained = authorities;
            return Err(invalid(
                "authority custody does not preserve the transaction budget",
            ));
        }
        let mut signal_exec = match pipe.prepare_exec() {
            Ok(guard) => guard,
            Err(error) => {
                self.retained = authorities;
                return Err(error);
            }
        };
        let snapshot = signal_exec.snapshot();
        let original = match transfer::encode(supervisor, &snapshot) {
            Ok(bytes) => bytes,
            Err(error) => {
                self.retained = authorities;
                rollback_pipe(&mut signal_exec, Some(&child), terminal.as_ref());
                return Err(error);
            }
        };
        envelope.original = original;
        // Set above, before any exit from this attempt: the transaction is retained.
        let Some(recovery) = self.recovery.as_mut() else {
            self.retained = authorities;
            rollback_pipe(&mut signal_exec, Some(&child), terminal.as_ref());
            return Err(invalid("the transaction is not retained"));
        };
        recovery.envelope = envelope.clone();
        let data = match AnonymousData::create().and_then(|mut data| {
            let header = header_for(
                &envelope,
                Phase::Forward,
                data.as_raw_fd(),
                snapshot,
                &authorities,
                [old.as_raw_fd(), new.hold.as_raw_fd()],
            )?;
            data.write_capsule(&header, &envelope.encode()?)?;
            Ok(data)
        }) {
            Ok(data) => data,
            Err(error) => {
                self.retained = authorities;
                rollback_pipe(&mut signal_exec, Some(&child), terminal.as_ref());
                return Err(error);
            }
        };
        let mut guards = Vec::new();
        let descriptors: Vec<_> = std::iter::once(data.as_fd())
            .chain(authorities.iter().map(AsFd::as_fd))
            .chain([old.as_fd(), new.hold.as_fd()])
            .collect();
        let mut inheritance_error = None;
        for fd in descriptors {
            match sys::inherit_for_one_exec(fd) {
                Ok(guard) => guards.push(guard),
                Err(error) => {
                    inheritance_error = Some(error);
                    break;
                }
            }
        }
        if let Some(error) = inheritance_error {
            restore_all(
                &mut guards,
                Some(&child),
                terminal.as_ref(),
                Some(HeldSignals::of(&snapshot)),
            );
            drop(guards);
            self.retained = authorities;
            rollback_pipe(&mut signal_exec, Some(&child), terminal.as_ref());
            return Err(error);
        }
        if envelope.forward_remaining().is_err()
            || !sys::in_foreground(0)
            || procs::observe_child(&child).is_none_or(|seen| seen.state != ChildState::Live)
            || !Image::selected(&supervisor.setup.home).is_ok_and(|selected| selected == target)
            || old.revalidate().is_err()
            || new.hold.revalidate().is_err()
        {
            restore_all(
                &mut guards,
                Some(&child),
                terminal.as_ref(),
                Some(HeldSignals::of(&snapshot)),
            );
            drop(guards);
            self.retained = authorities;
            rollback_pipe(&mut signal_exec, Some(&child), terminal.as_ref());
            return Err(invalid(
                "forward budget or final ownership/admission witnesses changed",
            ));
        }
        supervisor.journal.note(&format!(
            "harness-image exec old={} new={} child={} deadline={}",
            old.image().build,
            target.build,
            child.pid,
            envelope.deadline_ns
        ));
        #[cfg(debug_assertions)]
        let original_flags: Vec<_> = guards
            .iter()
            .map(sys::DescriptorExecGuard::fixture_original_flags)
            .chain(signal_exec.fixture_original_flags())
            .collect();
        #[cfg(debug_assertions)]
        let fixture_ready = fixture_observe(
            &fixture::Event::BeforeExec(
                fixture_facts(&envelope, Phase::Forward, snapshot),
                &original_flags,
            ),
            Some(&child),
        );
        #[cfg(not(debug_assertions))]
        let fixture_ready = true;
        let error = if fixture_ready {
            exec_image(&target.executable.path, Phase::Forward, data.as_raw_fd())
        } else {
            io::Error::other("fixture observer refused the exec boundary")
        };
        restore_all(
            &mut guards,
            Some(&child),
            terminal.as_ref(),
            Some(HeldSignals::of(&snapshot)),
        );
        #[cfg(debug_assertions)]
        fixture_observe(
            &fixture::Event::DescriptorsRestored(
                fixture_facts(&envelope, Phase::Forward, snapshot),
                &original_flags,
            ),
            Some(&child),
        );
        drop(guards);
        self.retained = authorities;
        rollback_pipe(&mut signal_exec, Some(&child), terminal.as_ref());
        #[cfg(debug_assertions)]
        fixture_observe(
            &fixture::Event::SignalsRestored(
                fixture_facts(&envelope, Phase::Forward, snapshot),
                &original_flags,
            ),
            Some(&child),
        );
        Err(error)
    }
}

fn header_for(
    envelope: &Envelope,
    phase: Phase,
    data: RawFd,
    signals: sys::SignalSnapshot,
    authorities: &[CustodyFd],
    gates: [RawFd; 2],
) -> io::Result<Header> {
    let old = Image::build_at(&envelope.prefix, envelope.old_build)?;
    let new = Image::build_at(&envelope.prefix, envelope.new_build)?;
    if [old.file_revision, new.file_revision] != envelope.revisions {
        return Err(invalid("retained image revisions changed"));
    }
    let original = transfer::decode(&envelope.original)?;
    Ok(Header {
        phase,
        forward_attempt: 1,
        rollback_attempt: u8::from(phase == Phase::Rollback),
        self_pid: original.process().pid,
        self_birth: original.process().birth.clone(),
        old_image: capsule::Image {
            executable: old.executable,
            version: old.version,
        },
        new_image: capsule::Image {
            executable: new.executable,
            version: new.version,
        },
        fds: capsule::FdRoles {
            capsule: data,
            signal_read: signals.read_fd,
            signal_write: signals.write_fd,
            authorities: authorities.iter().map(AsRawFd::as_raw_fd).collect(),
            retention_gates: gates.into(),
        },
    })
}

fn restore_all(
    guards: &mut [sys::DescriptorExecGuard<'_>],
    child: Option<&procs::ChildIdentity>,
    terminal: Option<&sys::Termios>,
    signals: Option<HeldSignals>,
) {
    let mut failed = None;
    for guard in guards {
        if let Err(error) = guard.restore() {
            failed.get_or_insert(error);
        }
    }
    if let Some(error) = failed {
        park(
            &format!("descriptor restoration failed: {error}"),
            child,
            terminal,
            signals,
        );
    }
}

fn rollback_pipe(
    guard: &mut sys::SignalExecGuard<'_>,
    child: Option<&procs::ChildIdentity>,
    terminal: Option<&sys::Termios>,
) {
    if let Err(error) = guard.rollback() {
        park(
            &format!("pipe rollback failed: {error}"),
            child,
            terminal,
            Some(HeldSignals::of(&guard.snapshot())),
        );
    }
}

fn exec_image(path: &std::path::Path, phase: Phase, data: RawFd) -> io::Error {
    #[cfg(not(debug_assertions))]
    let _ = phase;
    #[cfg(debug_assertions)]
    let fixture_path = match fixture::exec_path(phase, path) {
        Ok(path) => path,
        Err(error) => return error,
    };
    #[cfg(debug_assertions)]
    let path = fixture_path.as_path();
    let error = sys::exec_exact(
        path,
        &[ADOPT_ARG.into()],
        &[(ENTRY.into(), data.to_string().into())],
    );
    #[cfg(debug_assertions)]
    {
        fixture_observe(
            &fixture::Event::ExecReturned(phase, error.raw_os_error()),
            None,
        );
    }
    error
}

#[cfg(debug_assertions)]
fn fixture_facts(
    envelope: &Envelope,
    phase: Phase,
    signals: sys::SignalSnapshot,
) -> fixture::Facts<'_> {
    fixture::Facts {
        phase,
        version: cli::VERSION,
        deadline_ns: envelope.deadline_ns,
        original: &envelope.original,
        signals,
    }
}

#[cfg(debug_assertions)]
fn fixture_observe(event: &fixture::Event<'_>, child: Option<&procs::ChildIdentity>) -> bool {
    if let Err(error) = fixture::observe(event) {
        say(&format!(
            "{}: debug image fixture observer failed child={}: {error}",
            crate::profile::TOOL,
            child.map_or(0, |child| child.pid)
        ));
        return false;
    }
    true
}

struct OwnedRoles {
    data: Option<OwnedFd>,
    signals: [Option<OwnedFd>; 2],
    authorities: Vec<Option<OwnedFd>>,
    gates: [Option<OwnedFd>; 2],
}

impl OwnedRoles {
    fn take(header: &Header) -> io::Result<Self> {
        let &[old_gate, new_gate] = header.fds.retention_gates.as_slice() else {
            return Err(invalid("handoff must retain exactly its old/new gate pair"));
        };
        let all: Vec<_> = [
            header.fds.capsule,
            header.fds.signal_read,
            header.fds.signal_write,
        ]
        .into_iter()
        .chain(header.fds.authorities.iter().copied())
        .chain(header.fds.retention_gates.iter().copied())
        .collect();
        if all.iter().any(|fd| *fd < 3)
            || all.iter().copied().collect::<BTreeSet<_>>().len() != all.len()
        {
            return Err(invalid(
                "inherited roles alias or name standard descriptors",
            ));
        }
        for &fd in &all {
            sys::descriptor_flags(fd)?;
        }
        let take = |fd| {
            // SAFETY: every role is open, globally unique and nonstandard; this
            // fresh single-threaded image has no Rust owner for inherited roles.
            unsafe { OwnedFd::from_raw_fd(fd) }
        };
        Ok(Self {
            data: Some(take(header.fds.capsule)),
            signals: [
                Some(take(header.fds.signal_read)),
                Some(take(header.fds.signal_write)),
            ],
            authorities: header
                .fds
                .authorities
                .iter()
                .map(|fd| Some(take(*fd)))
                .collect(),
            gates: [Some(take(old_gate)), Some(take(new_gate))],
        })
    }
    fn restore_cloexec(&self) -> io::Result<()> {
        for fd in self
            .data
            .iter()
            .chain(self.signals.iter().flatten())
            .chain(self.authorities.iter().flatten())
            .chain(self.gates.iter().flatten())
        {
            sys::restore_descriptor_cloexec(fd.as_fd())?;
        }
        Ok(())
    }
}

struct Frame {
    header: Header,
    envelope: Envelope,
    saved: transfer::Saved,
    /// `saved`'s terminal modes bound to fd 0, when that is still the same terminal.
    terminal: Option<sys::Termios>,
    _data: OwnedFd,
    raw_signals: [Option<OwnedFd>; 2],
    pipe: Option<SignalPipe>,
    block: Option<sys::SignalBlock>,
    authorities: Vec<CustodyFd>,
    /// The retained old and new image holds, in that order.
    gates: [BuildHold; 2],
    host: Box<dyn Host>,
    bound: bool,
    rollback_attempt: RollbackAttempt,
    restored: Option<Supervisor>,
    #[cfg(debug_assertions)]
    observer_failed: bool,
}

impl Frame {
    fn load(raw: RawFd, factory: HostFactory) -> io::Result<Self> {
        sys::ensure_single_threaded()?;
        if raw < 3 || !sys::managed_signals_blocked()? {
            return Err(invalid(
                "adoption requires inherited blocked signals and a nonstandard capsule",
            ));
        }
        sys::descriptor_flags(raw)?;
        // SAFETY: fcntl established this inherited fd is live. One native thread
        // neither closes nor transfers it while capsule reading borrows it.
        let decoded = capsule::read_inherited(unsafe { BorrowedFd::borrow_raw(raw) })?;
        let envelope = Envelope::decode(&decoded.payload)?;
        let saved = transfer::decode(&envelope.original)?;
        let current = procs::capture_process(std::process::id())
            .ok_or_else(|| invalid("current image identity is unavailable"))?;
        let expected = match decoded.header.phase {
            Phase::Forward => &decoded.header.new_image,
            Phase::Rollback => &decoded.header.old_image,
        };
        if current.pid != decoded.header.self_pid
            || current.birth != decoded.header.self_birth
            || current.pid != saved.process().pid
            || current.birth != saved.process().birth
            || current.pgid != saved.process().pgid
            || current.session != saved.process().session
            || current.executable != expected.executable
            || expected.version != cli::VERSION
            || decoded.header.old_image.executable != saved.process().executable
            || decoded.header.fds.signal_read != saved.signals().read_fd
            || decoded.header.fds.signal_write != saved.signals().write_fd
            || envelope.prefix != crate::store::default_prefix(saved.home()).canonicalize()?
        {
            return Err(invalid(
                "inherited entry does not bind this same process, image and original state",
            ));
        }
        // The saved state is now proved to be this process's own, so a hold
        // from here follows its child. The original harness's saved modes,
        // bound to this same terminal, are restored if the child then dies by
        // a signal.
        let terminal = sys::Termios::from_snapshot(0, saved.termios()).ok();
        let mut roles = match OwnedRoles::take(&decoded.header) {
            Ok(roles) => roles,
            // No role is owned, so the hold drains the self-pipe only once it
            // proves to be the recorded read end.
            Err(error) => park(
                &format!("inherited roles: {error}"),
                Some(saved.child()),
                terminal.as_ref(),
                Some(HeldSignals {
                    read_fd: sys::inherited_read_end(saved.signals()).ok(),
                    launch_blocked: saved.signals().prior_mask,
                }),
            ),
        };
        // The inherited self-pipe read end holds signals the old image caught last.
        let signals = roles.signals[0].as_ref().map(|read| HeldSignals {
            read_fd: Some(read.as_raw_fd()),
            launch_blocked: saved.signals().prior_mask,
        });
        // From here every refusal keeps the verified role owners on this
        // stack. A budget or metadata error never closes authoritative FDs.
        let deadline = match envelope.recorded_deadline() {
            Ok(deadline) => deadline,
            Err(error) => park(
                &format!("recorded transaction clock: {error}"),
                Some(saved.child()),
                terminal.as_ref(),
                signals,
            ),
        };
        let old = match Image::build_at(&envelope.prefix, envelope.old_build) {
            Ok(image) => image,
            Err(error) => park(
                &format!("retained old image: {error}"),
                Some(saved.child()),
                terminal.as_ref(),
                signals,
            ),
        };
        let new = match Image::build_at(&envelope.prefix, envelope.new_build) {
            Ok(image) => image,
            Err(error) => park(
                &format!("retained new image: {error}"),
                Some(saved.child()),
                terminal.as_ref(),
                signals,
            ),
        };
        if [old.file_revision, new.file_revision] != envelope.revisions
            || old.executable != decoded.header.old_image.executable
            || old.version != decoded.header.old_image.version
            || new.executable != decoded.header.new_image.executable
            || new.version != decoded.header.new_image.version
        {
            park(
                "retained image pair changed",
                Some(saved.child()),
                terminal.as_ref(),
                signals,
            );
        }
        if let Err(error) = roles.restore_cloexec() {
            park(
                &format!("inherited flags: {error}"),
                Some(saved.child()),
                terminal.as_ref(),
                signals,
            );
        }
        // `OwnedRoles::take` validated every role, so each slot holds its descriptor.
        let hold_gate = |role: &mut Option<OwnedFd>, image: &Image| -> BuildHold {
            let Some(fd) = role.take() else {
                park(
                    "inherited gate role is not owned",
                    Some(saved.child()),
                    terminal.as_ref(),
                    signals,
                );
            };
            match BuildHold::from_inherited(image, File::from(fd)) {
                Ok(hold) => hold,
                Err(refusal) => {
                    *role = Some(refusal.file.into());
                    park(
                        &format!("inherited gate: {}", refusal.error),
                        Some(saved.child()),
                        terminal.as_ref(),
                        signals,
                    );
                }
            }
        };
        // Old first, then new, exactly as the header orders the retention gates.
        let [old_role, new_role] = &mut roles.gates;
        let gates = [hold_gate(old_role, &old), hold_gate(new_role, &new)];
        let mut authorities = Vec::new();
        for role in &mut roles.authorities {
            let Some(fd) = role.take() else {
                park(
                    "inherited authority role is not owned",
                    Some(saved.child()),
                    terminal.as_ref(),
                    signals,
                );
            };
            match CustodyFd::from_inherited_for_recovery(fd, deadline.monotonic_ns()) {
                Ok(owner) => authorities.push(owner),
                Err(refusal) => {
                    *role = Some(refusal.fd);
                    park(
                        &format!("inherited authority: {}", refusal.error),
                        Some(saved.child()),
                        terminal.as_ref(),
                        signals,
                    );
                }
            }
        }
        let rollback_attempt = RollbackAttempt::for_entry(decoded.header.phase);
        let Some(data) = roles.data.take() else {
            park(
                "inherited data role is not owned",
                Some(saved.child()),
                terminal.as_ref(),
                signals,
            );
        };
        let frame = Self {
            header: decoded.header,
            envelope,
            saved,
            terminal,
            _data: data,
            raw_signals: roles.signals,
            pipe: None,
            block: None,
            authorities,
            gates,
            host: factory(),
            bound: false,
            rollback_attempt,
            restored: None,
            #[cfg(debug_assertions)]
            observer_failed: false,
        };
        #[cfg(debug_assertions)]
        let mut frame = frame;
        #[cfg(debug_assertions)]
        {
            frame.observer_failed = !fixture_observe(
                &fixture::Event::Entry(fixture_facts(
                    &frame.envelope,
                    frame.header.phase,
                    *frame.saved.signals(),
                )),
                Some(frame.saved.child()),
            );
        }
        Ok(frame)
    }

    /// The signal state a hold here inherits; the self-pipe read end is
    /// attached or still only inherited.
    fn held_signals(&self) -> Option<HeldSignals> {
        self.pipe
            .as_ref()
            .map(SignalPipe::read_fd)
            .or_else(|| self.raw_signals[0].as_ref().map(AsRawFd::as_raw_fd))
            .map(|read_fd| HeldSignals {
                read_fd: Some(read_fd),
                launch_blocked: self.saved.signals().prior_mask,
            })
    }

    /// Restore the attached signal block's prior mask; [`Frame::attach`] sets
    /// the block, and a frame without one cannot restore it.
    fn restore_block(&mut self) -> io::Result<()> {
        self.block.as_mut().map_or_else(
            || Err(invalid("signal block is not attached")),
            sys::SignalBlock::restore,
        )
    }

    fn attach(&mut self) -> io::Result<()> {
        let (read, write) = match std::mem::take(&mut self.raw_signals) {
            [Some(read), Some(write)] => (read.into_raw_fd(), write.into_raw_fd()),
            roles => {
                self.raw_signals = roles;
                return Err(invalid("inherited signal roles are not owned"));
            }
        };
        // SAFETY: this independently validated same-process capsule transfers
        // unique endpoints; every error leaves both raw descriptors caller-owned.
        match unsafe { SignalPipe::attach_inherited(self.saved.signals()) } {
            Ok((pipe, block)) => {
                self.pipe = Some(pipe);
                self.block = Some(block);
                Ok(())
            }
            Err(error) => {
                // SAFETY: failed attachment explicitly retained caller ownership.
                self.raw_signals[0] = Some(unsafe { OwnedFd::from_raw_fd(read) });
                // SAFETY: independently retained, distinct write endpoint.
                self.raw_signals[1] = Some(unsafe { OwnedFd::from_raw_fd(write) });
                Err(error)
            }
        }
    }

    fn resume(mut self) -> ! {
        let restored = match self.saved.clone().restore(&self.header) {
            Ok(restored) => restored,
            Err(error) => self.rollback(&format!("runtime reconstruction: {error}")),
        };
        let child_state = restored.child_state;
        self.restored = Some(restored.supervisor);
        if let Err(error) = self.attach() {
            self.rollback(&format!("signal attachment: {error}"));
        }
        if child_state == ChildState::Unreaped {
            if let Err(error) = self.restore_block() {
                park(
                    &format!("terminal dispatch mask: {error}"),
                    Some(self.saved.child()),
                    self.terminal.as_ref(),
                    self.held_signals(),
                );
            }
            // The same owned terminal event is dispatched normally. No claim,
            // seal, live-adoption permission or replacement child is created.
            if let Some(supervisor) = self.restored.as_mut() {
                supervisor.reap();
            }
            park(
                "unreaped terminal event was not consumed",
                Some(self.saved.child()),
                self.terminal.as_ref(),
                self.held_signals(),
            );
        }
        #[cfg(debug_assertions)]
        if self.observer_failed {
            self.rollback("entry fixture observer failed after validated ownership");
        }
        let current = match procs::capture_process(std::process::id()) {
            Some(current) => current,
            None => self.rollback("current identity lost before reproof"),
        };
        let deadline = match self.envelope.deadline() {
            Ok(deadline) => deadline,
            Err(error) => self.rollback(&format!("budget: {error}")),
        };
        if self.header.phase == Phase::Forward && self.envelope.forward_remaining().is_err() {
            self.rollback("forward budget reached rollback reserve");
        }
        #[cfg(debug_assertions)]
        if let Err(error) = fixture::precommit(self.header.phase) {
            self.rollback(&format!("debug fixture precommit refusal: {error}"));
        }
        let proof_ready = match self.host.reprove(
            &self.envelope.context,
            &current,
            self.saved.child(),
            self.header.phase,
            deadline,
            &mut self.authorities,
        ) {
            Ok(ready) => ready,
            Err(error) => self.rollback(&format!("fresh host reproof: {error}")),
        };
        self.bound = true;
        if self.authorities.len() > capsule::MAX_AUTHORITIES {
            park(
                "host exceeded bounded retained-authority roles",
                Some(self.saved.child()),
                self.terminal.as_ref(),
                self.held_signals(),
            );
        }
        if let Err(error) = self.gates.iter().try_for_each(BuildHold::revalidate) {
            self.rollback(&format!("ready gate changed: {error}"));
        }
        if self.envelope.deadline().is_err() {
            self.rollback("budget expired before ready unmask");
        }
        // Every fallible local restore finishes BEFORE the irreversible native
        // commit can be dispatched. Precommit still permits the sole rollback.
        if let Err(error) = self.restore_block() {
            self.rollback(&format!("ready mask: {error}"));
        }
        let decision = self
            .host
            .commit(
                &self.envelope.context,
                &current,
                self.saved.child(),
                self.header.phase,
                deadline,
                &mut self.authorities,
            )
            .unwrap_or(CommitOutcome::Unknown);
        if self.header.phase == Phase::Forward
            && decision == CommitOutcome::ProvenNotCommittedRollbackSafe
        {
            self.rollback("host proves final commit did not occur");
        }
        // After commit DISPATCH, any unknown result stays in this image with
        // normal signal forwarding and same-op recovery. Never infer rollback.
        self.continue_supervising(
            decision,
            if proof_ready {
                "image adoption outcome"
            } else {
                "typed host proof pending; ordinary supervision restored"
            },
        )
    }

    fn continue_supervising(mut self, decision: CommitOutcome, why: &str) -> ! {
        if self.pipe.is_none() || self.restored.is_none() {
            park(
                why,
                Some(self.saved.child()),
                self.terminal.as_ref(),
                self.held_signals(),
            );
        }
        if let Err(error) = self.restore_block() {
            park(
                &format!("normal supervision mask: {error}; {why}"),
                Some(self.saved.child()),
                self.terminal.as_ref(),
                self.held_signals(),
            );
        }
        let ready = ready_outcome(self.header.phase, decision);
        #[cfg(debug_assertions)]
        fixture_observe(
            &fixture::Event::Resumed(
                fixture_facts(&self.envelope, self.header.phase, *self.saved.signals()),
                ready,
            ),
            Some(self.saved.child()),
        );
        // Both were checked at entry; a frame without them holds instead.
        let Some(mut supervisor) = self.restored.take() else {
            park(
                why,
                Some(self.saved.child()),
                self.terminal.as_ref(),
                self.held_signals(),
            );
        };
        supervisor
            .journal
            .note(&format!("harness-image resumed ready={ready} reason={why}"));
        let Some(mut pipe) = self.pipe.take() else {
            park(
                why,
                Some(self.saved.child()),
                self.terminal.as_ref(),
                self.held_signals(),
            );
        };
        // The image now running keeps its hold; the other one is kept only for recovery.
        // After a rollback the target that failed is the one already seen, so
        // its carried retry schedule holds until another target is selected.
        let [old_hold, new_hold] = self.gates;
        let (current_hold, other_gate, seen) = if self.header.phase == Phase::Forward {
            (new_hold, Some(old_hold), None)
        } else {
            let failed = new_hold.image().clone();
            (old_hold, Some(new_hold), Some(failed))
        };
        let envelope = self.envelope;
        let carried_step = envelope.retry_step;
        let recovery = if ready {
            self.authorities.clear();
            drop(other_gate);
            drop(envelope);
            None
        } else {
            Some(Recovery::from_entry(
                envelope,
                self.header.phase,
                other_gate,
                self.rollback_attempt,
            ))
        };
        drop(self._data);
        drop(self.block.take());
        let (probe_retry, attempt_retry) =
            entry_retries(self.header.phase, carried_step, ready, Instant::now());
        supervisor.image = Runtime {
            host: self.host,
            running: Some(current_hold),
            probe: None,
            compatible: None,
            seen,
            probe_retry,
            probe_rejected: false,
            attempt_retry,
            retained: self.authorities,
            held: None,
            attempted: None,
            admission: None,
            recovery,
        };
        drop(self.saved);
        drop(self.header);
        // A mid-exec stop stays stopped; ordinary job-control dispatch follows it.
        supervisor.run(&mut pipe, false)
    }

    fn rollback(mut self, why: &str) -> ! {
        if self.header.phase == Phase::Rollback {
            self.continue_supervising(
                CommitOutcome::Unknown,
                &format!("rollback entry held: {why}"),
            );
        }
        let deadline = match self.envelope.deadline() {
            Ok(deadline) => deadline,
            Err(_) => self.continue_supervising(
                CommitOutcome::Unknown,
                &format!("expired original transaction: {why}"),
            ),
        };
        if self.bound {
            match self.host.custody(deadline) {
                Ok((_, mut owners))
                    if owners.iter().all(|owner| owner.deadline() == deadline)
                        && owners
                            .len()
                            .checked_add(self.authorities.len())
                            .is_some_and(|total| total <= capsule::MAX_AUTHORITIES) =>
                {
                    self.authorities.append(&mut owners)
                }
                Ok((_, mut owners)) => {
                    self.authorities.append(&mut owners);
                    self.continue_supervising(
                        CommitOutcome::Unknown,
                        "ready binding custody could not preserve its budget/roles",
                    );
                }
                Err(error) => self.continue_supervising(
                    CommitOutcome::Unknown,
                    &format!("ready binding retained by provider: {error}"),
                ),
            }
        }
        let error = self.exec_rollback();
        self.continue_supervising(
            CommitOutcome::Unknown,
            &format!("rollback exec returned: {error}; original failure: {why}"),
        )
    }

    /// Exec the old image; a successful exec never returns, so this returns
    /// only the error of an attempt that did not leave this image.
    fn exec_rollback(&mut self) -> io::Error {
        if !self.rollback_attempt.consume() {
            return invalid("original rollback attempt already consumed");
        }
        let [old_gate, new_gate] = &self.gates;
        let child = self.saved.child();
        rollback_exec(
            &self.envelope,
            self.pipe.as_mut(),
            &self.raw_signals,
            &self.authorities,
            &[old_gate, new_gate],
            Some(child),
        )
    }
}

fn ready_outcome(phase: Phase, decision: CommitOutcome) -> bool {
    matches!(
        (phase, decision),
        (Phase::Forward, CommitOutcome::Committed)
            | (
                Phase::Rollback,
                CommitOutcome::ProvenNotCommittedRollbackSafe
            )
    )
}

fn rollback_exec(
    envelope: &Envelope,
    pipe: Option<&mut SignalPipe>,
    raw: &[Option<OwnedFd>; 2],
    authorities: &[CustodyFd],
    gates: &[&BuildHold; 2],
    child: Option<&procs::ChildIdentity>,
) -> io::Error {
    let result = (|| -> io::Result<()> {
        envelope.deadline()?.remaining()?;
        for gate in gates {
            gate.revalidate()?;
        }
        let original = transfer::decode(&envelope.original)?;
        let terminal = sys::Termios::from_snapshot(0, original.termios()).ok();
        let signals = pipe
            .as_deref()
            .map(SignalPipe::read_fd)
            .or_else(|| raw[0].as_ref().map(AsRawFd::as_raw_fd))
            .map(|read_fd| HeldSignals {
                read_fd: Some(read_fd),
                launch_blocked: original.signals().prior_mask,
            });
        let mut data = AnonymousData::create()?;
        let header = header_for(
            envelope,
            Phase::Rollback,
            data.as_raw_fd(),
            *original.signals(),
            authorities,
            [gates[0].as_raw_fd(), gates[1].as_raw_fd()],
        )?;
        data.write_capsule(&header, &envelope.encode()?)?;
        let mut pipe_exec = match pipe {
            Some(pipe) => Some(pipe.prepare_exec()?),
            None => None,
        };
        // A guard prepared while blocked may report ALL HANDLED. This does
        // not replace a single byte of the original saved runtime payload.
        let mut guards = Vec::new();
        let mut descriptors: Vec<_> = std::iter::once(data.as_fd())
            .chain(authorities.iter().map(AsFd::as_fd))
            .chain(gates.iter().map(|gate| gate.as_fd()))
            .collect();
        if pipe_exec.is_none() {
            descriptors.extend(raw.iter().flatten().map(AsFd::as_fd));
        }
        for fd in descriptors {
            match sys::inherit_for_one_exec(fd) {
                Ok(guard) => guards.push(guard),
                Err(error) => {
                    restore_all(&mut guards, child, terminal.as_ref(), signals);
                    if let Some(guard) = pipe_exec.as_mut() {
                        rollback_pipe(guard, child, terminal.as_ref());
                    }
                    return Err(error);
                }
            }
        }
        #[cfg(debug_assertions)]
        let original_flags: Vec<_> = guards
            .iter()
            .map(sys::DescriptorExecGuard::fixture_original_flags)
            .chain(
                pipe_exec
                    .as_ref()
                    .into_iter()
                    .flat_map(sys::SignalExecGuard::fixture_original_flags),
            )
            .collect();
        #[cfg(debug_assertions)]
        let fixture_ready = fixture_observe(
            &fixture::Event::BeforeExec(
                fixture_facts(envelope, Phase::Rollback, *original.signals()),
                &original_flags,
            ),
            child,
        );
        #[cfg(not(debug_assertions))]
        let fixture_ready = true;
        let error = match envelope.deadline().and_then(CustodyDeadline::remaining) {
            Ok(_) if fixture_ready => exec_image(
                &header.old_image.executable.path,
                Phase::Rollback,
                data.as_raw_fd(),
            ),
            Ok(_) => io::Error::other("fixture observer refused the rollback exec boundary"),
            Err(error) => error,
        };
        restore_all(&mut guards, child, terminal.as_ref(), signals);
        #[cfg(debug_assertions)]
        fixture_observe(
            &fixture::Event::DescriptorsRestored(
                fixture_facts(envelope, Phase::Rollback, *original.signals()),
                &original_flags,
            ),
            child,
        );
        if let Some(guard) = pipe_exec.as_mut() {
            rollback_pipe(guard, child, terminal.as_ref());
            #[cfg(debug_assertions)]
            fixture_observe(
                &fixture::Event::SignalsRestored(
                    fixture_facts(envelope, Phase::Rollback, *original.signals()),
                    &original_flags,
                ),
                child,
            );
        }
        Err(error)
    })();
    result
        .err()
        .unwrap_or_else(|| invalid("rollback exec unexpectedly returned success"))
}

/// Whether the harness's arguments (`args`, without the program name) open with
/// a private image word: `(probe, adopt)`. A harness starts a private entry
/// with that word as its only argument, so only the first argument counts. A
/// leading `--alab-` word is the harness's own namespace (docs/DECISIONS.md, 6);
/// the same word after any vendor word is the vendor's argument, as every
/// other argument there is, and selects nothing here.
fn private_words(args: &[std::ffi::OsString]) -> (bool, bool) {
    let first = args.first();
    (
        first.is_some_and(|arg| arg.to_str().is_some_and(|arg| arg.starts_with(PROBE_ARG))),
        first.is_some_and(|arg| arg == OsStr::new(ADOPT_ARG)),
    )
}

/// Route private probe/adoption entries before any normal CLI launch processing.
/// Malformed markers never become vendor arguments or a fresh vendor spawn.
pub fn early_entry(factory: HostFactory) {
    let entry = std::env::var_os(ENTRY);
    let probe = std::env::var_os(PROBE);
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    let (probe_arg, adopt_arg) = private_words(&args);
    if entry.is_none() && probe.is_none() && !probe_arg && !adopt_arg {
        return;
    }
    let result = (|| -> io::Result<()> {
        sys::ensure_single_threaded()?;
        match (entry, probe) {
            (None, Some(nonce)) => {
                let nonce = nonce
                    .to_str()
                    .filter(|nonce| valid_nonce(nonce))
                    .ok_or_else(|| invalid("malformed probe marker"))?;
                if args.len() != 1 || args[0] != OsStr::new(&format!("{PROBE_ARG}{nonce}")) {
                    return Err(invalid("probe marker and arguments differ"));
                }
                sys::clear_exec_markers(&[ENTRY, PROBE])?;
                let reply = Capability::reply(nonce, factory)?;
                use std::io::Write as _;
                let mut stdout = std::io::stdout();
                stdout.write_all(reply.as_bytes())?;
                stdout.flush()?;
                std::process::exit(0);
            }
            (Some(raw), None)
                if !probe_arg && args.len() == 1 && args[0] == OsStr::new(ADOPT_ARG) =>
            {
                let text = raw
                    .to_str()
                    .ok_or_else(|| invalid("non-UTF8 capsule descriptor marker"))?;
                let fd = text
                    .parse::<i32>()
                    .ok()
                    .filter(|fd| *fd >= 3 && fd.to_string() == text)
                    .ok_or_else(|| invalid("malformed capsule descriptor marker"))?;
                let frame = Frame::load(fd, factory)?;
                if let Err(error) = sys::clear_exec_markers(&[ENTRY, PROBE]) {
                    frame.rollback(&format!("marker cleanup: {error}"));
                }
                frame.resume();
            }
            _ => Err(invalid("malformed or overlapping private image markers")),
        }
    })();
    let why = result
        .err()
        .unwrap_or_else(|| invalid("private image entry unexpectedly returned"));
    if sys::managed_signals_blocked().unwrap_or(true) {
        park(&format!("malformed adoption: {why}"), None, None, None);
    }
    say(&format!(
        "{}: refusing private image entry: {why}",
        crate::profile::TOOL
    ));
    cli::journal_refusal(&format!("private-image-entry {why}"));
    std::process::exit(cli::EXIT_USAGE)
}

/// Hold the handoff: supervise `child` by the section 2 signal table until it
/// ends, then end the same way. HUP and TERM are passed on. A child stop is
/// followed. CONT continues a stopped child. A TSTP waits briefly for the child
/// to stop with it, and is otherwise passed on. A child killed by a signal
/// cannot restore the terminal, so `terminal` (the modes the original harness
/// saved before its first child started) is put back before dying the same
/// way. The managed signals are blocked on entry and taken from the pending
/// set, because a hold can start under either image's handlers or an exec
/// guard; signals caught before that wait as bytes on the self-pipe and are
/// taken first. A signal the launch mask blocks stays pending, as in ordinary
/// supervision. A malformed adoption has no recorded child, so it holds for
/// the one child the kernel proves this process owns ([`own_child`]).
fn park(
    why: &str,
    child: Option<&procs::ChildIdentity>,
    terminal: Option<&sys::Termios>,
    signals: Option<HeldSignals>,
) -> ! {
    // Block first: a signal caught before this is a byte on the self-pipe,
    // and every later one stays pending for the hold to take. If the mask
    // cannot be set, the hold still follows the child's stops and its end.
    let _ = sys::block_managed();
    // SAFETY: the read end is owned by a frame a hold never returns to, or is
    // the proved inherited read end no owner holds; nothing closes it, so the
    // descriptor stays open for the rest of the process.
    let mut caught = signals
        .and_then(|held| held.read_fd)
        .map(|fd| sys::queued_signals(unsafe { BorrowedFd::borrow_raw(fd) }))
        .unwrap_or_default();
    let launch_blocked = signals.map_or(0, |held| held.launch_blocked);
    say(&format!(
        "{}: image handoff held: {why}",
        crate::profile::TOOL
    ));
    let found;
    let child = match child {
        Some(child) => child,
        None => {
            found = own_child();
            &found
        }
    };
    let mut hold = Hold {
        child,
        terminal,
        stopped: false,
    };
    loop {
        let mut tstp = false;
        caught.extend(sys::take_pending(launch_blocked).unwrap_or_default());
        for sig in caught.drain(..) {
            match sig {
                libc::SIGHUP | libc::SIGTERM => hold.pass_on(sig),
                libc::SIGTSTP => tstp = true,
                libc::SIGCONT if hold.stopped => hold.signal(libc::SIGCONT),
                // INT and QUIT belong to the child; CHLD is read below.
                _ => {}
            }
        }
        hold.reap();
        if hold.stopped {
            hold.follow_stop();
            continue;
        }
        if tstp {
            hold.on_tstp();
            continue;
        }
        std::thread::sleep(POLL);
    }
}

/// The child a malformed adoption holds for. Such an entry proved no saved
/// state, so it has no recorded child, no saved terminal modes and no proved
/// self-pipe; but the kernel still proves this process's own children
/// ([`procs::own_children`]), and an exec keeps them. The one child there is
/// is followed by the section 2 table like any other hold, and one that
/// already exited ends the hold the same way at once. With no child there is
/// nothing a hold protects, so the entry is refused like any other malformed
/// entry. With several, none is followed yet: HUP and TERM are passed on to
/// each, and the ones that end are reaped until one is left, which is then
/// followed. A list that cannot be read is looked at again, with the managed
/// signals left pending.
fn own_child() -> procs::ChildIdentity {
    loop {
        match procs::own_children().as_deref() {
            Some([]) => {
                say(&format!(
                    "{}: refusing private image entry: no child to hold",
                    crate::profile::TOOL
                ));
                cli::journal_refusal("private-image-entry no child to hold");
                std::process::exit(cli::EXIT_USAGE)
            }
            Some(&[pid]) => {
                if let Some(child) = procs::capture_child(pid) {
                    return child;
                }
                // Not live: it exited and waits unreaped. Only an end is read
                // here, so a stop stays pending for the hold.
                if let Some(event) = libc::pid_t::try_from(pid)
                    .ok()
                    .and_then(sys::wait_child_end)
                {
                    sys::exit_like(event);
                }
            }
            Some(pids) => several_children(pids),
            None => {}
        }
        std::thread::sleep(POLL);
    }
}

/// One look at several children of a malformed adoption. Every listed pid is
/// this process's unreaped child until this process reaps it, so it is
/// signalled before any is reaped. HUP and TERM are passed on to each (with a
/// CONT, as a stopped child acts on them only once continued); the other
/// managed signals have no followed child and are dropped. Then the children
/// that ended are reaped, always leaving one listed for the one-child look.
fn several_children(pids: &[u32]) {
    let pids: Vec<libc::pid_t> = pids
        .iter()
        .filter_map(|pid| libc::pid_t::try_from(*pid).ok())
        .collect();
    for sig in sys::take_pending(0).unwrap_or_default() {
        if matches!(sig, libc::SIGHUP | libc::SIGTERM) {
            for &pid in &pids {
                let _ = sys::kill(pid, sig);
                let _ = sys::kill(pid, libc::SIGCONT);
            }
        }
    }
    let mut left = pids.len();
    for &pid in &pids {
        if left <= 1 {
            break;
        }
        if sys::wait_child_end(pid).is_some() {
            left = left.saturating_sub(1);
        }
    }
}

/// The signal state a hold inherits from the supervision it interrupts.
#[derive(Clone, Copy)]
struct HeldSignals {
    /// The signal self-pipe's read end, owned by a frame a hold never leaves
    /// or checked as the recorded inherited read end; absent when an inherited
    /// read end fails that check.
    read_fd: Option<RawFd>,
    /// The managed signals the launch mask blocks, indexed by [`sys::HANDLED`].
    launch_blocked: u8,
}

impl HeldSignals {
    const fn of(snapshot: &sys::SignalSnapshot) -> Self {
        Self {
            read_fd: Some(snapshot.read_fd),
            launch_blocked: snapshot.prior_mask,
        }
    }

    fn of_pipe(pipe: &SignalPipe) -> Self {
        Self {
            read_fd: Some(pipe.read_fd()),
            launch_blocked: pipe.prior_mask(),
        }
    }
}

/// The child a held handoff supervises.
struct Hold<'a> {
    child: &'a procs::ChildIdentity,
    terminal: Option<&'a sys::Termios>,
    stopped: bool,
}

impl Hold<'_> {
    fn pid(&self) -> libc::pid_t {
        libc::pid_t::try_from(self.child.pid).unwrap_or(0)
    }

    /// Signal the child only while it is still this same unreaped child.
    fn signal(&self, sig: libc::c_int) {
        if procs::observe_child(self.child).is_some() {
            let _ = sys::kill(self.pid(), sig);
        }
    }

    fn pass_on(&self, sig: libc::c_int) {
        self.signal(sig);
        if self.stopped {
            // A stopped child acts on HUP or TERM only once continued.
            self.signal(libc::SIGCONT);
        }
    }

    /// Read every pending child event; an end exits here.
    fn reap(&mut self) {
        let pid = self.pid();
        // A non-positive pid would make waitpid wait for some other child.
        if pid <= 0 || procs::observe_child(self.child).is_none() {
            return;
        }
        while let Some(event) = sys::wait_child(pid) {
            match event {
                sys::ChildEvent::Stopped(_) => self.stopped = true,
                sys::ChildEvent::Continued => self.stopped = false,
                sys::ChildEvent::Exited(_) | sys::ChildEvent::Signaled(_) => {
                    if matches!(event, sys::ChildEvent::Signaled(_))
                        && let Some(terminal) = self.terminal
                    {
                        terminal.restore();
                    }
                    sys::exit_like(event);
                }
            }
        }
    }

    /// The child stopped: stop too, then continue the child once continued.
    fn follow_stop(&mut self) {
        sys::stop_self();
        self.reap();
        if self.stopped
            && procs::observe_child(self.child)
                .is_some_and(|seen| seen.state == ChildState::Stopped)
        {
            self.signal(libc::SIGCONT);
            self.stopped = false;
        }
    }

    /// A TSTP reached the hold: follow the child if it stops with it, else pass it on.
    fn on_tstp(&mut self) {
        for _ in 0..super::TSTP_WAIT_MS / 10 {
            self.reap();
            if self.stopped {
                self.follow_stop();
                return;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        self.signal(libc::SIGTSTP);
    }
}

#[cfg(test)]
#[path = "supervise_image_retry_tests.rs"]
mod retry_budget_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsString;
    use std::os::unix::ffi::OsStringExt as _;

    fn image() -> Image {
        Image {
            prefix: "/private/prefix".into(),
            build: 22,
            directory: "/private/prefix/store/claude-code-alab/22".into(),
            executable: procs::Executable {
                path: "/private/prefix/store/claude-code-alab/22/bin/claude-alab".into(),
                device: 2,
                inode: 222,
            },
            version: cli::VERSION.into(),
            file_revision: [100, 2, 3, 4, 5],
        }
    }

    #[test]
    fn a_private_word_counts_only_as_the_first_argument() {
        let os = |words: &[&str]| -> Vec<std::ffi::OsString> {
            words.iter().map(std::ffi::OsString::from).collect()
        };
        let probe = format!("{PROBE_ARG}0123456789abcdef0123456789abcdef");
        assert_eq!(private_words(&os(&[])), (false, false));
        assert_eq!(private_words(&os(&[ADOPT_ARG])), (false, true));
        assert_eq!(private_words(&os(&[&probe])), (true, false));
        assert_eq!(private_words(&os(&[ADOPT_ARG, "-p"])), (false, true));
        assert_eq!(private_words(&os(&[&probe, ADOPT_ARG])), (true, false));
        // After any vendor word, both are the vendor's arguments.
        for vendor in [
            &["-p", ADOPT_ARG][..],
            &["-p", &probe],
            &["--model", "x", ADOPT_ARG, &probe],
            &["", ADOPT_ARG],
            &["--alab-no-live-update", ADOPT_ARG],
        ] {
            assert_eq!(private_words(&os(vendor)), (false, false), "{vendor:?}");
        }
    }

    #[test]
    fn probe_reply_binds_nonce_actor_version_and_every_transfer_abi() {
        let nonce = "1234567890abcdef1234567890abcdef";
        let reply = Capability::reply(nonce, unavailable_host).unwrap();
        let pid = i32::try_from(std::process::id()).unwrap();
        assert!(
            !Capability::parse(reply.as_bytes(), nonce, pid, &image())
                .unwrap()
                .host
        );
        assert!(
            Capability::parse(
                reply.as_bytes(),
                "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                pid,
                &image()
            )
            .is_err()
        );
        assert!(Capability::parse(reply.as_bytes(), nonce, pid + 1, &image()).is_err());
        let mut wrong_image = image();
        wrong_image.version = format!("{}.wrong", cli::VERSION);
        for wrong in [
            reply.replace("capsule=1", "capsule=2"),
            reply.replace(
                &format!("payload={}", transfer::SCHEMA),
                &format!("payload={}", transfer::SCHEMA + 1),
            ),
            reply.replace(
                &format!("envelope={ENVELOPE_SCHEMA}"),
                &format!("envelope={}", ENVELOPE_SCHEMA + 1),
            ),
            reply.replace("host=0", "host=2"),
            reply.replace("host=0", "host=0 host=1"),
            format!("{reply}unrelated late output\n"),
            reply.trim_end().to_owned(),
        ] {
            assert!(
                Capability::parse(wrong.as_bytes(), nonce, pid, &image()).is_err(),
                "accepted {wrong:?}"
            );
        }
    }

    #[test]
    fn original_state_and_absolute_budget_are_lossless_and_not_reanchored() {
        let original = vec![0, 255, 10, 0, 13, 128];
        let envelope = Envelope {
            prefix: "/private/prefix".into(),
            old_build: 11,
            new_build: 22,
            deadline_ns: 987_654_321_123,
            revisions: [[1, 2, 3, 4, 5], [6, 7, 8, 9, 10]],
            retry_step: 3,
            context: HostContext {
                sid: "s-abcdef".into(),
                binding: vec![0, 255, 1],
            },
            original: original.clone(),
        };
        let encoded = envelope.encode().unwrap();
        let decoded = Envelope::decode(&encoded).unwrap();
        assert_eq!(decoded.deadline_ns, 987_654_321_123);
        assert_eq!(decoded.original, original);
        assert_eq!(decoded.revisions, envelope.revisions);
        assert_eq!(decoded.retry_step, 3);
        assert_eq!(decoded.encode().unwrap(), encoded);
        // The carried retry step is bounded where it is written and where it
        // is read: bytes naming a step past the schedule's last are refused.
        for step in 0..=RETRY_LAST_STEP {
            let mut stepped = envelope.clone();
            stepped.retry_step = step;
            let decoded = Envelope::decode(&stepped.encode().unwrap()).unwrap();
            assert_eq!(decoded.retry_step, step);
        }
        let mut over = envelope.clone();
        over.retry_step = RETRY_LAST_STEP + 1;
        assert!(over.encode().is_err());
        let mut other = envelope.clone();
        other.retry_step = 0;
        let at = other
            .encode()
            .unwrap()
            .iter()
            .zip(&encoded)
            .position(|(a, b)| a != b)
            .expect("the encoded retry step");
        for bad in [RETRY_LAST_STEP + 1, u32::MAX] {
            let mut forged = encoded.clone();
            forged[at..at + 4].copy_from_slice(&bad.to_le_bytes());
            assert!(Envelope::decode(&forged).is_err(), "{bad}");
        }
        let mut last = encoded.clone();
        last[at..at + 4].copy_from_slice(&RETRY_LAST_STEP.to_le_bytes());
        assert_eq!(
            Envelope::decode(&last).unwrap().retry_step,
            RETRY_LAST_STEP,
            "the forged word is the retry step"
        );
        let mut trailing = encoded.clone();
        trailing.push(0);
        assert!(Envelope::decode(&trailing).is_err());
        assert!(Envelope::decode(&encoded[..encoded.len() - 1]).is_err());
        let mut wrong = envelope.clone();
        wrong.new_build = wrong.old_build;
        assert!(Envelope::decode(&wrong.encode().unwrap()).is_err());
        wrong = envelope;
        wrong.context.binding.resize(OUTPUT_MAX + 1, 0);
        assert!(wrong.encode().is_err());
    }

    #[test]
    fn expired_recorded_entry_budget_survives_without_authorizing_an_exec() {
        let envelope = Envelope {
            prefix: "/private/prefix".into(),
            old_build: 11,
            new_build: 22,
            deadline_ns: 1,
            revisions: [[1; 5], [2; 5]],
            retry_step: 0,
            context: HostContext {
                sid: "s-abcdef".into(),
                binding: vec![1, 2],
            },
            original: vec![0, 255, 0],
        };
        let decoded = Envelope::decode(&envelope.encode().unwrap()).unwrap();
        assert_eq!(decoded.recorded_deadline().unwrap().monotonic_ns(), 1);
        assert_eq!(
            decoded.deadline().unwrap_err().kind(),
            io::ErrorKind::TimedOut
        );
        assert_eq!(
            decoded.forward_remaining().unwrap_err().kind(),
            io::ErrorKind::TimedOut
        );
        assert_eq!(decoded.original, envelope.original);
    }

    #[test]
    fn retry_wait_grows_monotonically_and_stays_bounded_without_sleeping() {
        let mut retry = Backoff::default();
        let mut now = Instant::now();
        assert!(retry.ready(now));
        for wait in [30, 60, 120, 240, 300, 300] {
            retry.defer(now);
            assert!(!retry.ready(now));
            assert!(!retry.ready(now + Duration::from_secs(wait - 1)));
            now += Duration::from_secs(wait);
            assert!(retry.ready(now));
        }
    }

    #[test]
    fn ambiguous_or_wrong_phase_commit_cannot_release_retained_owners() {
        for phase in [Phase::Forward, Phase::Rollback] {
            assert!(!ready_outcome(phase, CommitOutcome::Unknown));
        }
        assert!(ready_outcome(Phase::Forward, CommitOutcome::Committed));
        assert!(!ready_outcome(Phase::Rollback, CommitOutcome::Committed));
        assert!(!ready_outcome(
            Phase::Forward,
            CommitOutcome::ProvenNotCommittedRollbackSafe
        ));
        assert!(ready_outcome(
            Phase::Rollback,
            CommitOutcome::ProvenNotCommittedRollbackSafe
        ));
    }

    #[test]
    fn local_context_preserves_distinct_child_session_and_unix_image_bytes() {
        let mut executable = image().executable;
        executable.path = OsString::from_vec(b"/private/non-utf8-\xff".to_vec()).into();
        let process = ProcessIdentity {
            pid: 100,
            parent: 99,
            pgid: 100,
            session: 99,
            birth: procs::Birth::Linux(123),
            executable: executable.clone(),
        };
        let child = procs::ChildIdentity {
            pid: 101,
            parent: 100,
            pgid: 101,
            session: 101,
            birth: procs::Birth::Linux(124),
            executable: executable.clone(),
        };
        let ticket = LocalTicket {
            nonce: "1234567890abcdef1234567890abcdef".into(),
            deadline_ns: 987_654_321_123,
            process: process.clone(),
            child: child.clone(),
            target: executable,
            old_version: cli::VERSION.into(),
            new_version: cli::VERSION.into(),
        };
        let mut context = ticket.context().unwrap();
        let decoded = LocalTicket::decode(&context).unwrap();
        assert_eq!(decoded.process, process);
        assert_eq!(decoded.child, child);
        assert_eq!(decoded.deadline_ns, ticket.deadline_ns);
        context.sid = "local-own-image:101".into();
        assert!(LocalTicket::decode(&context).is_err());
        context = ticket.context().unwrap();
        context.binding.push(0);
        assert!(LocalTicket::decode(&context).is_err());
    }
}
