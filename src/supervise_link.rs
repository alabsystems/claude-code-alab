// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The supervisor's link to aterm: the contract-3 claim, the reads that judge a
//! safe point and a relaunch by aterm's facts, and the event stream that wakes
//! the look and carries aterm's restart requests.
//!
//! [`crate::aterm`] holds the texts and the judges and performs no I/O;
//! [`crate::aterm_wire`] carries lines. This module is the part between them
//! that the [`Supervisor`] owns: one request connection, one event
//! subscription, and what the harness does when either stops answering.
//!
//! **The claim sequence** ([`claim`]), run once before the first vendor start
//! and again every [`RECLAIM_EVERY_MS`] while the link is [`Link::Lost`]:
//!
//! 1. connect and authenticate;
//! 2. `harness policy`: contract 3 served and `effective=on`;
//! 3. the roster guard: a read-only `native-act info` must name this harness's
//!    row. Nothing is claimed otherwise, not even to find out: aterm stores a
//!    contract-3 claim from a holder off its roster as a foreign claim that
//!    takes every duty ([`crate::aterm::rostered`]);
//! 4. the claim, scope `relaunch,upgrade`: the two duties this harness
//!    performs (bringing a crashed child back, and moving the child onto
//!    another vendor build);
//! 5. `status` must read the claim back as this harness's and proven, or the
//!    claim is released;
//! 6. the event subscription.
//!
//! A step that fails at launch on an answer aterm gave that stands
//! ([`Failure::Refused`], [`Failure::Unsubscribed`]), or a session that cannot
//! be located, leaves [`Link::Absent`]: the session then runs on the harness's
//! own readings ([`crate::quiet`]) for its whole life. Any other failure leaves
//! the session unclaimed and runs the sequence again every
//! [`RECLAIM_EVERY_MS`] on a fresh connection, as for a lost link: an exchange
//! aterm did not answer ([`Failure::Gone`]: the socket could not be reached, a
//! reply did not arrive within its bound, or the connection closed before it),
//! an answer that stands for now only ([`Failure::Later`]: an `ERR …; retry`
//! aterm gives while a dialog is up, its main thread is stalled or its
//! subscription lanes are full, or a policy off for a halt, a suspension or a
//! reason not known here), and a policy switched off by a setting that can be
//! switched on again while the session runs ([`Failure::Off`]).
//!
//! The subscription comes after the claim, so a failed one gives the claim
//! back, and each attempt takes aterm's engine's duties and returns them. One
//! aterm refused with an answer that stands ([`Failure::Unsubscribed`]) is
//! therefore never asked again: it ends the link at launch and on a reclaim
//! alike, rather than flip the duties every [`RECLAIM_EVERY_MS`] for the
//! session's whole life.
//!
//! **Bounds.** Opening at launch may take [`CONNECT_TIMEOUT`]; every later
//! exchange, the reopening of a lost link included, is bounded by
//! [`REQUEST_TIMEOUT`], because it runs in the supervisor's signal loop. An
//! exchange that fails or outlives its bound drops the connection (aterm drops
//! a claim with the connection that made it) and the link is [`Link::Lost`].
//!
//! Both sockets are close-on-exec ([`crate::aterm_wire`]): the vendor child
//! never inherits them, and nothing of the link enters a transfer capsule.

use std::os::fd::{AsRawFd as _, RawFd};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use super::{Supervisor, now_ms};
use crate::aterm::{self, Policy, SafePoint, Scope, Status};
use crate::aterm_wire::{Connection, EventReader, Locator, Unsubscribed};
use crate::cli::VERSION;
use crate::journal::Journal;
use crate::relaunch;

/// How long the launch may spend opening each of the two connections.
pub(super) const CONNECT_TIMEOUT: Duration = Duration::from_secs(2);

/// The bound on every exchange made from the supervisor's loop, and on opening
/// a connection again from it.
pub(super) const REQUEST_TIMEOUT: Duration = Duration::from_millis(500);

// Kept across control reconnections, never reusing a generation after a fork
// attempt. Own-image transfer inside aterm remains unqualified in this release.
static CHILD_GENERATION: AtomicU64 = AtomicU64::new(0);

/// How often a lost link is claimed again, an ended event stream reopened, and
/// at most how long an aterm hold waits before `status` is read again.
pub(super) const RECLAIM_EVERY_MS: u64 = 5_000;

/// How long a crash relaunch waits in all for a lost claim to be proven again,
/// or for aterm to answer a question it could answer only later, before the
/// crash is passed on as an exit: aterm's own grace for a claim whose
/// connection went away (`aterm/crates/aterm-gui/src/harness_host.rs`, 45 s),
/// which the sibling harnesses hold a crash for as well. A restart request
/// waits as long for the same reasons.
pub(super) const RECLAIM_HOLD_MS: u64 = 45_000;

/// How soon a question aterm answered with [`Failure::Later`] is asked again
/// on a link that is still held.
pub(super) const LATER_RETRY_MS: u64 = 1_000;

/// The duties the claim takes: bringing a crashed child back on its
/// conversation, and moving the child onto another vendor build. Nothing this
/// harness does not do.
const SCOPES: [Scope; 2] = [Scope::Relaunch, Scope::Upgrade];

/// [`SCOPES`] as the claim spells them: `relaunch,upgrade`.
fn scope_words() -> String {
    SCOPES.map(Scope::word).join(",")
}

/// Why a step of the link did not succeed. The text is one journal word group.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Failure {
    /// The connection could not be opened or cannot be used again.
    Gone(String),
    /// aterm answered, and the answer admits nothing.
    Refused(String),
    /// aterm answered that the policy is off by a setting a person can switch
    /// on again while the session runs (`why=disabled`, `agent-off` or
    /// `autopilot-off`; [`Policy::switched_off`]), which aterm announces to no
    /// link without a claim: a standing claim is released as for
    /// [`Failure::Refused`], and the policy is read again on the clock.
    Off(String),
    /// aterm answered, and the answer stands for now only, so the question is
    /// asked again: a read it refused with an `ERR` that asks to be retried
    /// (it could not read its own state: `ERR aterm is showing a dialog;
    /// answer it, then retry`, `ERR main thread stalled …; retry`), or a
    /// policy that is off for a reason that passes
    /// ([`Policy::off_by_configuration`] is false). The connection, and a
    /// claim on it, stand.
    Later(String),
    /// aterm answered the event subscription, the last step, and the answer
    /// stands: an `ERR` that does not ask to be retried (`ERR usage: subscribe
    /// …`, `ERR unknown subscribe arg`), or an acknowledgement this harness
    /// cannot parse. The claim the sequence made has been given back. The link
    /// ends ([`Link::Absent`]) whether or not a claim stood before, since every
    /// further attempt would take aterm's engine's duties only to return them.
    Unsubscribed(String),
}

impl Failure {
    /// The journal text.
    fn why(&self) -> &str {
        match self {
            Self::Gone(why)
            | Self::Refused(why)
            | Self::Off(why)
            | Self::Later(why)
            | Self::Unsubscribed(why) => why,
        }
    }
}

/// A reply to a read that could not be parsed as `step`'s: `Later` when aterm
/// refused it with an `ERR` that asks to be retried, which it does when it
/// cannot read its state now (`aterm/crates/aterm-gui/src/control_media.rs`,
/// `main_thread_refusal`; [`aterm::ReplyError::Retry`]); `Refused` for any
/// other `ERR` (an unknown session, a principal aterm denies, a verb it does
/// not serve), which stands, and for an `OK` reply that does not state what
/// is needed.
fn unreadable(step: &str, error: &aterm::ReplyError) -> Failure {
    match error {
        aterm::ReplyError::Retry(_) => Failure::Later(format!("{step} {error}")),
        _ => Failure::Refused(format!("{step} {error}")),
    }
}

/// Whether `reply` is an `OK` line.
fn ok(reply: &str) -> bool {
    reply == "OK" || reply.starts_with("OK ")
}

/// The first two words of a reply aterm refused with, as `reply=<w>:<w>`.
fn reply_words(reply: &str) -> String {
    let words: Vec<String> = reply
        .split_ascii_whitespace()
        .take(2)
        .map(aterm::journal_word)
        .collect();
    if words.is_empty() {
        "reply=(empty)".to_string()
    } else {
        format!("reply={}", words.join(":"))
    }
}

/// How long after a hold `status` is read again when no event arrives first:
/// the rest of the person's grace plus a second, and at most
/// [`RECLAIM_EVERY_MS`]. An ageing `human_ms` emits no event, so its end has to
/// be read for.
fn recheck_after_ms(grace_left: Option<Duration>) -> u64 {
    grace_left.map_or(RECLAIM_EVERY_MS, |left| {
        u64::try_from(left.as_millis())
            .unwrap_or(u64::MAX)
            .saturating_add(1_000)
            .min(RECLAIM_EVERY_MS)
    })
}

/// Send `request` for `step` and read its one reply line. The line is read
/// leniently: a reply such as `native-act info`'s carries a bare word, which
/// the strict field reader would refuse and poison the connection over.
fn ask(conn: &mut Connection, step: &str, request: Option<String>) -> Result<String, Failure> {
    let request = request.ok_or_else(|| Failure::Refused(format!("{step} request-unsendable")))?;
    conn.request_line(&request)
        .map_err(|error| Failure::Gone(format!("{step} {error}")))
}

fn child_start_ask(
    conn: &mut Connection,
    deadline: Instant,
    step: &str,
    request: Option<String>,
) -> Result<String, Failure> {
    let left = deadline
        .checked_duration_since(Instant::now())
        .filter(|left| !left.is_zero())
        .ok_or_else(|| Failure::Gone("child-start deadline".to_string()))?;
    conn.set_timeout(left);
    ask(conn, step, request)
}

/// One fresh `harness policy`.
fn read_policy(conn: &mut Connection, sid: &str) -> Result<Policy, Failure> {
    let reply = ask(conn, "policy", aterm::policy_verb(sid))?;
    Policy::parse(&reply).map_err(|error| unreadable("policy", &error))
}

/// One fresh `status`.
fn read_status(conn: &mut Connection, sid: &str) -> Result<Status, Failure> {
    let reply = ask(conn, "status", aterm::status_verb(sid))?;
    Status::parse(&reply).map_err(|error| unreadable("status", &error))
}

/// Whether `policy` lets a rostered harness claim: contract 3 served and the
/// policy in effect. A policy off by a setting that can be switched on again
/// is [`Failure::Off`], and one off by a lasting fact (`unhosted`, `headless`)
/// [`Failure::Refused`]. A policy off for a reason that passes (a halt, aterm
/// suspended for its own update, a reason not known here) is
/// [`Failure::Later`]: a standing claim is kept through it and nothing moves
/// meanwhile, because aterm derives a halt's `hold=1` and the policy's
/// `why=hold` from the same fact (`harness_policy_verb.rs`, `effective`) and
/// emits no event when either changes.
fn admits(policy: &Policy) -> Result<(), Failure> {
    if !policy.serves_contract() {
        let contracts = policy
            .contracts
            .iter()
            .map(u32::to_string)
            .collect::<Vec<_>>()
            .join(",");
        return Err(Failure::Refused(format!("policy contract={contracts}")));
    }
    if policy.switched_off() {
        return Err(Failure::Off(format!("policy {}", policy.off_words())));
    }
    if policy.off_by_configuration() {
        return Err(Failure::Refused(format!("policy {}", policy.off_words())));
    }
    if !policy.effective {
        return Err(Failure::Later(format!("policy {}", policy.off_words())));
    }
    Ok(())
}

/// The claim line for the journal.
fn claimed_line(holder: &str, vendor: Option<&str>) -> String {
    format!(
        "aterm claimed holder={holder} scope={} vendor={}",
        scope_words(),
        vendor.unwrap_or("-")
    )
}

/// Steps 2 to 5 of the claim sequence on an open connection: the policy, the
/// roster guard, the claim, and its proof read back from `status`. A claim
/// `status` does not show as this harness's and proven is released before the
/// refusal is returned.
fn guarded_claim(
    conn: &mut Connection,
    sid: &str,
    pid: u32,
    version: &str,
    vendor: Option<&str>,
    child: Option<&aterm::ChildProof>,
    generation: Option<u64>,
) -> Result<(bool, bool, Option<u64>), Failure> {
    admits(&read_policy(conn, sid)?)?;
    // Never a trial claim: without the row, nothing below is sent.
    let info = ask(conn, "roster", aterm::info_verb(sid))?;
    if !aterm::rostered(&info) {
        return Err(Failure::Refused("unrostered".to_string()));
    }
    let child_proof = aterm::child_proof_supported(&info);
    let child_start = aterm::child_start_supported(&info);
    let reply = ask(
        conn,
        "claim",
        aterm::claim_child_generation_verb(
            sid,
            pid,
            &SCOPES,
            version,
            vendor,
            child.filter(|_| child_proof),
            generation.filter(|_| child_start),
        ),
    )?;
    if !ok(&reply) {
        return Err(Failure::Refused(format!("claim {}", reply_words(&reply))));
    }
    let holder = aterm::holder(pid);
    let proof = read_status(conn, sid).and_then(|status| {
        if status.held_by(&holder) {
            Ok(())
        } else {
            Err(Failure::Refused(format!(
                "unproven supervisor={} supervisor_proven={}",
                status
                    .supervisor
                    .as_deref()
                    .map_or_else(|| "-".to_string(), aterm::journal_word),
                u8::from(status.supervisor_proven)
            )))
        }
    });
    if matches!(proof, Err(Failure::Refused(_) | Failure::Later(_))) {
        let _ = ask(conn, "release", aterm::release_verb(sid, pid));
    }
    proof.map(|()| (child_proof, child_start, generation.filter(|_| child_start)))
}

/// The whole claim sequence for the session `locator` names, by harness process
/// `pid` of version `version` running vendor build `vendor`. Each connection
/// may take `opening` to open; every exchange after that is bounded by
/// [`REQUEST_TIMEOUT`].
fn claim(
    locator: &Locator,
    pid: u32,
    version: &str,
    vendor: Option<&str>,
    child: Option<aterm::ChildProof>,
    generation: Option<u64>,
    opening: Duration,
) -> Result<Claimed, Failure> {
    let socket = locator
        .socket()
        .map_err(|error| Failure::Gone(format!("graph {error}")))?;
    let mut conn = Connection::connect(&socket, opening)
        .map_err(|error| Failure::Gone(format!("connect {error}")))?;
    conn.set_timeout(REQUEST_TIMEOUT);
    let (child_proof, child_start, generation) = guarded_claim(
        &mut conn,
        locator.sid(),
        pid,
        version,
        vendor,
        child.as_ref(),
        generation,
    )?;
    match EventReader::connect(&socket, locator.sid(), opening) {
        Ok(events) => Ok(Claimed {
            conn,
            locator: locator.clone(),
            pid,
            holder: aterm::holder(pid),
            standing: true,
            child_proof,
            child_start,
            generation,
            child: child.filter(|_| child_proof),
            child_pending: child_proof && child.is_none(),
            events: Some(events),
            events_retry_ms: 0,
            recheck_at_ms: 0,
            policy_due_ms: 0,
            noted: String::new(),
            last_hold: None,
        }),
        Err(error) => {
            let _ = ask(
                &mut conn,
                "release",
                aterm::release_verb(locator.sid(), pid),
            );
            Err(unsubscribed(&error))
        }
    }
}

/// Why the event subscription did not open: [`Failure::Gone`] when aterm gave
/// no answer (the socket, the handshake or a reply failed or outlived its
/// bound); [`Failure::Later`] for an `ERR` that asks to be retried (`ERR
/// subscription capacity busy; retry`, while every subscription lane aterm
/// serves is taken); and [`Failure::Unsubscribed`] for any other answer: an
/// `ERR` that stands, or an acknowledgement that does not parse.
fn unsubscribed(error: &std::io::Error) -> Failure {
    let Some(answer) = Unsubscribed::of(error) else {
        return Failure::Gone(format!("events {error}"));
    };
    if !answer.refused() {
        return Failure::Unsubscribed(format!(
            "events ack-unreadable {}",
            reply_words(answer.header())
        ));
    }
    match aterm::ReplyError::refused(answer.header()) {
        retry @ aterm::ReplyError::Retry(_) => Failure::Later(format!("events {retry}")),
        stands => Failure::Unsubscribed(format!("events {stands}")),
    }
}

/// What one fresh policy and status pair said, and what the claim did about it.
enum Judged {
    /// aterm sees nobody the move would interrupt.
    Safe,
    /// A field holds; the journal line.
    Hold(String),
    /// `status` no longer shows this harness's proven claim; the journal line
    /// of the hold. The connection is of no further use.
    Taken(String),
    /// The policy no longer admits a claim and the claim was released; the
    /// journal line.
    Released(String),
}

/// aterm's word on starting the child again, from one fresh policy and status
/// pair ([`relaunch::permitted`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Relaunch {
    /// The claim is this harness's, proven and takes `relaunch`, the policy
    /// permits it, and the exit is nobody else's.
    Permitted,
    /// aterm answered, or no claim stands, and nothing is started; the reason
    /// as journal words.
    Denied(String),
    /// aterm did not answer, or the link was already lost: the claim may be
    /// proven again within [`RECLAIM_HOLD_MS`]; the reason as journal words.
    Away(String),
    /// aterm answered on a link that is still held, with an answer that stands
    /// for now only ([`Failure::Later`]; a policy off while aterm is suspended
    /// for its own update): ask again after [`LATER_RETRY_MS`], within
    /// [`RECLAIM_HOLD_MS`]; the reason as journal words.
    Later(String),
}

/// What one fresh policy and status pair said about starting the child again.
enum Leave {
    /// Everything [`relaunch::permitted`] asks for holds.
    Granted,
    /// The claim stands and something else does not; the reason.
    Denied(String),
    /// `status` no longer shows this harness's proven claim; the reason. The
    /// connection is of no further use.
    Taken(String),
    /// aterm could not answer now; the reason. The claim stands.
    Later(String),
}

/// An open, authenticated link on which this harness's claim was proven.
pub(super) struct Claimed {
    /// The request connection the claim is bound to.
    conn: Connection,
    /// Where the session is found again.
    locator: Locator,
    /// This harness's pid, as claimed.
    pid: u32,
    /// `claude-code-alab@<pid>`.
    holder: String,
    /// Whether the claim stands. It does not while aterm's policy is off for
    /// the session: the claim was released, the connection and the event
    /// stream are kept, and a policy event that shows it on claims again.
    standing: bool,
    /// Whether this host negotiated the optional contract-3 native child target.
    child_proof: bool,
    /// The host can capture an output barrier before a new child is spawned.
    child_start: bool,
    /// The pending or completed generation this connection acknowledged.
    generation: Option<u64>,
    /// The child lifetime last announced, retained across a policy pause.
    child: Option<aterm::ChildProof>,
    /// A newly spawned child's identity must be announced once observable.
    child_pending: bool,
    /// The event subscription, until it ends.
    events: Option<EventReader>,
    /// When (epoch ms) an ended event stream is opened again.
    events_retry_ms: u64,
    /// Until when (epoch ms) an aterm hold stands without `status` being read
    /// again; an event ends the wait at once.
    recheck_at_ms: u64,
    /// When (epoch ms) the policy is read again without an event, or 0 for
    /// not on a clock: set while the claim does not stand (it was released,
    /// or a claim made again was refused) and after a policy read aterm could
    /// answer only later, and 0 while a standing claim's policy admits it. A
    /// session's Autopilot word and a halt change `effective=` with no
    /// `harness-policy` event and no generation move.
    policy_due_ms: u64,
    /// The last line [`Claimed::follow_policy`] journalled, so a read on the
    /// clock that finds the same is not journalled again.
    noted: String,
    /// The hold aterm's last safe-point answer stated (`wait aterm …`), or
    /// `None` when it called the moment safe or has not been asked. A link
    /// lost after a hold keeps holding until the claim is proven again.
    last_hold: Option<String>,
}

impl Claimed {
    fn before_spawn(&mut self, vendor: Option<&str>) -> Result<(), Failure> {
        let deadline = Instant::now() + REQUEST_TIMEOUT;
        let sid = self.locator.sid().to_string();
        let status = child_start_ask(
            &mut self.conn,
            deadline,
            "child-start-status",
            aterm::status_verb(&sid),
        )?;
        if let Some(generation) = aterm::child_generation(&status) {
            CHILD_GENERATION.fetch_max(generation, Ordering::SeqCst);
        }
        let generation = CHILD_GENERATION
            .try_update(Ordering::SeqCst, Ordering::SeqCst, |value| {
                value.checked_add(1)
            })
            .map_err(|_| Failure::Refused("child-start generation-exhausted".to_string()))?
            + 1;
        let mut failure = None;
        for _ in 0..2 {
            let result = (|| {
                let reply = child_start_ask(
                    &mut self.conn,
                    deadline,
                    "child-start",
                    aterm::claim_child_start_verb(
                        &sid, self.pid, &SCOPES, VERSION, vendor, generation,
                    ),
                )?;
                if !ok(&reply) {
                    return Err(Failure::Refused(format!(
                        "child-start {}",
                        reply_words(&reply)
                    )));
                }
                let reply = child_start_ask(
                    &mut self.conn,
                    deadline,
                    "child-start-proof",
                    aterm::status_verb(&sid),
                )?;
                if !aterm::child_start_pending(&reply, &self.holder, generation) {
                    return Err(Failure::Refused("child-start unproven-pending".to_string()));
                }
                Ok(())
            })();
            match result {
                Ok(()) => {
                    self.conn.set_timeout(REQUEST_TIMEOUT);
                    self.child = None;
                    self.child_pending = true;
                    self.generation = Some(generation);
                    self.last_hold = None;
                    return Ok(());
                }
                Err(why) => {
                    let gone = matches!(why, Failure::Gone(_));
                    failure = Some(why);
                    if gone || Instant::now() >= deadline {
                        break;
                    }
                }
            }
        }
        Err(failure.unwrap_or_else(|| Failure::Gone("child-start deadline".to_string())))
    }

    /// The negotiated host needs the first vendor child's identity after spawn.
    pub(super) const fn wants_initial_child(&self) -> bool {
        self.child_pending
    }

    /// Give the claim back, keeping the connection. The policy is read again
    /// every [`RECLAIM_EVERY_MS`] from here ([`Supervisor::link_tick`]): aterm
    /// sends no event when a session's own Autopilot word changes, so nothing
    /// else would find the policy on again.
    fn release(&mut self) -> Result<(), Failure> {
        self.standing = false;
        self.policy_due_ms = now_ms().saturating_add(RECLAIM_EVERY_MS);
        let reply = ask(
            &mut self.conn,
            "release",
            aterm::release_verb(self.locator.sid(), self.pid),
        )?;
        if ok(&reply) {
            Ok(())
        } else {
            // A release aterm did not take is made certain by closing the
            // connection the claim is bound to.
            Err(Failure::Gone(format!("release {}", reply_words(&reply))))
        }
    }

    /// Read the policy afresh and make the claim follow it: release a standing
    /// claim the policy no longer admits by configuration, keep it through a
    /// policy that is off for now, and claim again (through the whole guarded
    /// sequence) once the policy admits a claim. While no claim stands, and
    /// after a read aterm could answer only later, the policy is read again
    /// every [`RECLAIM_EVERY_MS`] ([`Supervisor::link_tick`]). The journal
    /// line, when anything changed.
    fn follow_policy(&mut self, vendor: Option<&str>) -> Result<Option<String>, Failure> {
        let sid = self.locator.sid().to_string();
        let again = now_ms().saturating_add(RECLAIM_EVERY_MS);
        let line = match read_policy(&mut self.conn, &sid).and_then(|policy| admits(&policy)) {
            // Only the subscription is ever [`Failure::Unsubscribed`]; this
            // sequence has none.
            Err(unanswered @ (Failure::Gone(_) | Failure::Unsubscribed(_))) => {
                return Err(unanswered);
            }
            Err(Failure::Refused(why) | Failure::Off(why)) if self.standing => {
                self.release()?;
                format!("aterm released {why}")
            }
            Err(Failure::Refused(why) | Failure::Off(why) | Failure::Later(why)) => {
                self.policy_due_ms = again;
                if self.standing {
                    // Off for now (a halt), or not read: the claim stands, and
                    // each look holds on aterm's word meanwhile.
                    return Ok(None);
                }
                format!("aterm unclaimed {why}")
            }
            Ok(()) if self.standing => {
                self.policy_due_ms = 0;
                return Ok(None);
            }
            Ok(()) => match guarded_claim(
                &mut self.conn,
                &sid,
                self.pid,
                VERSION,
                vendor,
                self.child.as_ref(),
                self.generation,
            ) {
                Ok((child_proof, child_start, generation)) => {
                    self.child_proof = child_proof;
                    self.child_start = child_start;
                    self.generation = generation;
                    self.child_pending = child_proof && self.child.is_none();
                    self.standing = true;
                    self.recheck_at_ms = 0;
                    self.policy_due_ms = 0;
                    self.last_hold = None;
                    claimed_line(&self.holder, vendor)
                }
                Err(unanswered @ (Failure::Gone(_) | Failure::Unsubscribed(_))) => {
                    return Err(unanswered);
                }
                Err(Failure::Refused(why) | Failure::Off(why) | Failure::Later(why)) => {
                    self.policy_due_ms = again;
                    format!("aterm unclaimed {why}")
                }
            },
        };
        if line == self.noted {
            return Ok(None);
        }
        self.noted.clone_from(&line);
        Ok(Some(line))
    }

    /// One fresh `harness policy` and one fresh `status`, judged as an upgrade
    /// safe point ([`aterm::judge_safe_point`]). A reply that cannot be read
    /// holds; only a connection that failed is an error.
    fn judge(&mut self, grace: Duration) -> Result<Judged, Failure> {
        let judged = self.judge_pair(grace);
        match &judged {
            Ok(Judged::Hold(hold)) => self.last_hold = Some(hold.clone()),
            // A claim taken by another holder states no hold on the session:
            // the duty is that holder's, and this harness's looks are its own.
            Ok(Judged::Safe | Judged::Taken(_) | Judged::Released(_)) => self.last_hold = None,
            Err(_) => {}
        }
        judged
    }

    /// [`Claimed::judge`] without remembering the answer.
    fn judge_pair(&mut self, grace: Duration) -> Result<Judged, Failure> {
        let sid = self.locator.sid().to_string();
        self.recheck_at_ms = now_ms().saturating_add(RECLAIM_EVERY_MS);
        let policy = match read_policy(&mut self.conn, &sid) {
            Ok(policy) => policy,
            Err(Failure::Refused(why) | Failure::Later(why)) => {
                return Ok(Judged::Hold(format!("wait aterm {why}")));
            }
            Err(gone) => return Err(gone),
        };
        match admits(&policy) {
            Err(Failure::Refused(why) | Failure::Off(why)) => {
                self.release()?;
                let line = format!("aterm released {why}");
                // A claim made again later is journalled ([`Claimed::noted`]).
                self.noted.clone_from(&line);
                return Ok(Judged::Released(line));
            }
            // Off for now: the claim stands and the move waits, and the policy
            // is read again on the recheck clock, since nothing announces the
            // end of a halt.
            Err(Failure::Later(why)) => return Ok(Judged::Hold(format!("wait aterm {why}"))),
            Err(unanswered @ (Failure::Gone(_) | Failure::Unsubscribed(_))) => {
                return Err(unanswered);
            }
            Ok(()) => {}
        }
        let status = match read_status(&mut self.conn, &sid) {
            Ok(status) => status,
            Err(Failure::Refused(why) | Failure::Later(why)) => {
                return Ok(Judged::Hold(format!("wait aterm {why}")));
            }
            Err(gone) => return Err(gone),
        };
        match aterm::judge_safe_point(&policy, &status, &self.holder, grace) {
            SafePoint::Safe => {
                self.recheck_at_ms = 0;
                Ok(Judged::Safe)
            }
            SafePoint::Hold(hold) if !status.held_by(&self.holder) => {
                Ok(Judged::Taken(hold.to_string()))
            }
            SafePoint::Hold(hold) => {
                self.recheck_at_ms = now_ms().saturating_add(recheck_after_ms(hold.grace_left));
                Ok(Judged::Hold(hold.to_string()))
            }
        }
    }

    /// One fresh `harness policy` and one fresh `status`, judged as leave to
    /// start the child again ([`relaunch::permitted`]). An `OK` reply that
    /// does not state what is needed grants nothing. A reply aterm could give
    /// only later is [`Leave::Later`]: the question is asked again. For a
    /// crash so is a policy off for a reason that is neither configuration
    /// nor a halt (aterm suspended for its own update, a reason this harness
    /// does not know); a halt denies it, since the exit is the holder's. A
    /// restart aterm asked for (`asked`) is not judged by `effective=`: aterm
    /// routed it to this claim without reading the policy, and its own host
    /// makes an asked restart whatever the switches say. Only a connection
    /// that failed is an error.
    fn relaunch_word(&mut self, asked: bool) -> Result<Leave, Failure> {
        let sid = self.locator.sid().to_string();
        let policy = match read_policy(&mut self.conn, &sid) {
            Ok(policy) => policy,
            Err(Failure::Refused(why)) => return Ok(Leave::Denied(why)),
            Err(Failure::Later(why)) => return Ok(Leave::Later(why)),
            Err(gone) => return Err(gone),
        };
        if !asked
            && !policy.effective
            && !policy.off_by_configuration()
            && policy.why.as_deref() != Some("hold")
        {
            return Ok(Leave::Later(format!("policy {}", policy.off_words())));
        }
        let status = match read_status(&mut self.conn, &sid) {
            Ok(status) => status,
            Err(Failure::Refused(why)) => return Ok(Leave::Denied(why)),
            Err(Failure::Later(why)) => return Ok(Leave::Later(why)),
            Err(gone) => return Err(gone),
        };
        Ok(
            match relaunch::permitted(&policy, &status, &self.holder, asked) {
                Ok(()) => Leave::Granted,
                Err(why) if !status.held_by(&self.holder) => Leave::Taken(why),
                Err(why) => Leave::Denied(why),
            },
        )
    }
}

/// What the supervisor has of aterm.
pub(super) enum Link {
    /// Nothing, for the rest of the session: the harness is outside aterm, the
    /// session cannot be located, aterm refused the launch's claim sequence
    /// with an answer that stands, or it refused the event subscription so
    /// ([`Failure::Unsubscribed`], at launch or on a reclaim). Never retried.
    Absent,
    /// An open link on which the claim was proven.
    Held(Claimed),
    /// No link, and a claim to make through the whole guarded sequence once
    /// `retry_at_ms` has passed: a link that was held and stopped answering,
    /// or a launch that aterm did not answer (a connection that could not be
    /// opened or a read that timed out), could answer only later, or whose
    /// policy was switched off.
    Lost {
        /// Where the session is found again.
        locator: Locator,
        /// When (epoch ms) the claim is next attempted.
        retry_at_ms: u64,
        /// The last reason an attempt failed, so it is journalled once.
        noted: String,
        /// Whether a claim stood before: the link was held. A launch that has
        /// not claimed yet has no duty towards the child.
        claimed: bool,
        /// The hold aterm stated last before the link was lost
        /// ([`Claimed::last_hold`]), as the journal line of the wait it
        /// keeps: nothing moves on the harness's own readings over it. It
        /// stands until aterm answers again: a proven claim, or a refusal,
        /// which is aterm's newer word.
        held: Option<String>,
        /// A generation acknowledged for this exact child, retained on request
        /// reconnect so a live child keeps its original pre-spawn output floor.
        generation: Option<(aterm::ChildProof, u64)>,
    },
}

/// What aterm's word is on ending the child now.
pub(super) enum Verdict {
    /// The policy and status pair is a safe point.
    Safe,
    /// It is not; the journal line (`wait aterm <field>=<value>`).
    Hold(String),
    /// aterm did not answer or no claim stands: the harness's own readings
    /// decide this look.
    Local,
}

impl Link {
    /// A claimed fork needs an acknowledged pre-spawn barrier. An unavailable
    /// barrier definitively drops the bound connection instead of blocking the
    /// vendor indefinitely or claiming the unacknowledged barrier succeeded.
    pub(super) fn before_spawn(&mut self, journal: &Journal, vendor: Option<&str>) {
        let failure = match self {
            Self::Held(claimed) if claimed.standing && claimed.child_start => {
                match claimed.before_spawn(vendor) {
                    Ok(()) => {
                        journal.note(&format!(
                            "aterm child-start generation={}",
                            claimed.generation.unwrap_or(0)
                        ));
                        return;
                    }
                    Err(failure) => failure,
                }
            }
            _ => return,
        };
        let why = failure.why().to_string();
        if let Self::Held(claimed) = std::mem::replace(self, Self::Absent) {
            let locator = claimed.locator.clone();
            // Drop both sockets now. The request connection owns the claim;
            // no stale bound child or ambiguous pending ACK grants input.
            drop(claimed);
            *self = Self::Lost {
                locator,
                retry_at_ms: now_ms().saturating_add(RECLAIM_EVERY_MS),
                noted: why.clone(),
                claimed: true,
                held: None,
                generation: None,
            };
        }
        journal.note(&format!("aterm child-start unclaimed {why}"));
    }

    /// The launch's link, decided before the first vendor start. Outside aterm
    /// nothing is attempted and nothing is journalled. Inside, a session that
    /// can be located runs the claim sequence for vendor build `vendor`;
    /// anything short of a proven claim is one journal line, `aterm unclaimed
    /// <why>`, and [`Link::Absent`] when aterm's answer stands
    /// ([`Failure::Refused`], [`Failure::Unsubscribed`]) or the session cannot
    /// be located. A launch aterm
    /// did not answer ([`Failure::Gone`]: the connection failed or a read
    /// timed out, which says nothing of what aterm would answer next), an
    /// answer that stands for now only ([`Failure::Later`]) or a policy
    /// switched off by a setting that can be switched on again
    /// ([`Failure::Off`]) is [`Link::Lost`] with no claim made instead, and
    /// the sequence is tried again on a fresh connection every
    /// [`RECLAIM_EVERY_MS`], as for a lost link. Every supervised session is
    /// one that can be moved (a launch that cannot runs the vendor alone and
    /// never comes here), so every one claims the duties it performs.
    pub(super) fn launch(journal: &Journal, vendor: Option<&str>) -> Self {
        if !super::image::hosted_by_aterm() {
            return Self::Absent;
        }
        let (locator, claimed) =
            match Locator::from_env().map_err(|error| Failure::Gone(format!("locator {error}"))) {
                Ok(locator) => {
                    let claimed = claim(
                        &locator,
                        std::process::id(),
                        VERSION,
                        vendor,
                        None,
                        None,
                        CONNECT_TIMEOUT,
                    );
                    (Some(locator), claimed)
                }
                Err(failure) => (None, Err(failure)),
            };
        match (claimed, locator) {
            (Ok(claimed), _) => {
                journal.note(&claimed_line(&claimed.holder, vendor));
                Self::Held(claimed)
            }
            (Err(Failure::Gone(why) | Failure::Later(why) | Failure::Off(why)), Some(locator)) => {
                let noted = format!("aterm unclaimed {why}");
                journal.note(&noted);
                Self::Lost {
                    locator,
                    retry_at_ms: now_ms().saturating_add(RECLAIM_EVERY_MS),
                    noted: why,
                    claimed: false,
                    held: None,
                    generation: None,
                }
            }
            (Err(failure), _) => {
                journal.note(&format!("aterm unclaimed {}", failure.why()));
                Self::Absent
            }
        }
    }

    /// The event stream's descriptor, for the supervisor's wait.
    pub(super) fn event_fd(&self) -> Option<RawFd> {
        match self {
            Self::Held(claimed) => claimed.events.as_ref().map(|events| events.as_raw_fd()),
            Self::Absent | Self::Lost { .. } => None,
        }
    }

    /// Whether aterm's word replaces the harness's own input readings: the
    /// link is held and the claim stands.
    pub(super) fn judges(&self) -> bool {
        matches!(self, Self::Held(claimed) if claimed.standing)
    }

    /// Whether an aterm hold still stands at `now_ms` without a fresh read.
    pub(super) fn held_back(&self, now_ms: u64) -> bool {
        matches!(self, Self::Held(claimed) if claimed.standing && now_ms < claimed.recheck_at_ms)
    }

    /// Whether this session ever had a claim: the link is held or is being
    /// claimed again. Without one the harness has no duty towards a crashed
    /// child, and its end is the session's own.
    pub(super) const fn claims(&self) -> bool {
        matches!(self, Self::Held(_) | Self::Lost { claimed: true, .. })
    }

    /// Whether the link is lost and waiting for its next claim attempt.
    pub(super) const fn lost(&self) -> bool {
        matches!(self, Self::Lost { .. })
    }

    /// The wait a lost link keeps because aterm's last safe-point answer was a
    /// hold: `wait link-lost <that hold>`.
    pub(super) fn lost_hold(&self) -> Option<String> {
        match self {
            Self::Lost {
                held: Some(hold), ..
            } => Some(format!(
                "wait link-lost {}",
                hold.strip_prefix("wait ").unwrap_or(hold)
            )),
            _ => None,
        }
    }

    /// Give a standing claim back (one bounded exchange, its result ignored)
    /// and close the link: the harness is about to exit.
    pub(super) fn release(&mut self) {
        if let Self::Held(claimed) = self
            && claimed.standing
        {
            let _ = claimed.release();
        }
        *self = Self::Absent;
    }
}

/// Capture the direct live child's native birth. The host checks its current
/// image separately: a token-route helper may exec the vendor with the same
/// kernel lifetime, so no executable path is sent as authority here.
fn live_child_proof(pid: libc::pid_t) -> Option<aterm::ChildProof> {
    let pid = u32::try_from(pid).ok()?;
    let identity = crate::procs::capture_child(pid)?;
    aterm::ChildProof::from_identity(&identity)
}

impl Supervisor {
    /// The held link stopped answering (or its claim is no longer this
    /// harness's): drop both connections, which also drops the claim at aterm,
    /// and claim again on the reclaim clock.
    fn link_lost(&mut self, why: &str) {
        let Link::Held(claimed) = std::mem::replace(&mut self.link, Link::Absent) else {
            return;
        };
        self.journal.note(&format!("aterm lost {why}"));
        self.link = Link::Lost {
            locator: claimed.locator.clone(),
            retry_at_ms: now_ms().saturating_add(RECLAIM_EVERY_MS),
            noted: String::new(),
            claimed: true,
            // Only a standing claim's answer holds: a released claim's looks
            // were already the harness's own.
            held: claimed.last_hold.clone().filter(|_| claimed.standing),
            generation: claimed.child.zip(claimed.generation),
        };
    }

    /// The link's own clock, run on every poll: claim a lost link again,
    /// reopen an ended event stream, and read the policy again when it is due
    /// ([`Claimed::policy_due_ms`]). Each is bounded by [`REQUEST_TIMEOUT`] per
    /// step and stops at the first step that fails.
    pub(super) fn link_tick(&mut self) {
        let now = now_ms();
        let no_child = self.no_child();
        match &mut self.link {
            Link::Absent => {}
            Link::Lost {
                locator,
                retry_at_ms,
                noted,
                claimed,
                held,
                generation,
            } => {
                if now < *retry_at_ms {
                    return;
                }
                let vendor = self.child.build.version.as_deref();
                let child = (!no_child)
                    .then(|| live_child_proof(self.child.pid))
                    .flatten();
                match claim(
                    locator,
                    std::process::id(),
                    VERSION,
                    vendor,
                    child,
                    generation
                        .filter(|(saved, _)| Some(*saved) == child)
                        .map(|(_, generation)| generation),
                    REQUEST_TIMEOUT,
                ) {
                    Ok(claimed) => {
                        self.journal.note(&claimed_line(&claimed.holder, vendor));
                        self.link = Link::Held(claimed);
                    }
                    Err(failure) => {
                        *retry_at_ms = now_ms().saturating_add(RECLAIM_EVERY_MS);
                        if noted != failure.why() {
                            *noted = failure.why().to_string();
                            self.journal
                                .note(&format!("aterm unclaimed {}", failure.why()));
                        }
                        // aterm answered: its earlier hold is no longer its
                        // last word, and the harness's own readings decide.
                        if matches!(
                            failure,
                            Failure::Refused(_) | Failure::Off(_) | Failure::Unsubscribed(_)
                        ) {
                            *held = None;
                        }
                        // A launch that never claimed and is now refused for
                        // good is what the launch would have made it. A
                        // subscription refused for good ends any link: each
                        // attempt would claim and give the claim back.
                        if matches!(failure, Failure::Unsubscribed(_))
                            || (!*claimed && matches!(failure, Failure::Refused(_)))
                        {
                            self.link = Link::Absent;
                        }
                    }
                }
            }
            Link::Held(claimed) => {
                let mut policy_due = claimed.policy_due_ms != 0 && now >= claimed.policy_due_ms;
                if claimed.events.is_none() && now >= claimed.events_retry_ms {
                    let reopened = claimed.locator.socket().and_then(|socket| {
                        EventReader::connect(&socket, claimed.locator.sid(), REQUEST_TIMEOUT)
                    });
                    match reopened {
                        Ok(events) => {
                            claimed.events = Some(events);
                            self.journal.note("aterm events-reopened");
                            // What the stream said while it was down was not heard.
                            policy_due = true;
                        }
                        Err(_) => {
                            claimed.events_retry_ms = now_ms().saturating_add(RECLAIM_EVERY_MS);
                        }
                    }
                }
                if policy_due {
                    self.link_policy();
                }
            }
        }
        if !self.no_child() && matches!(&self.link, Link::Held(claimed) if claimed.child_pending) {
            self.link_reannounce();
        }
    }

    /// The event descriptor is readable: drain it without waiting. `true` when
    /// something was drained, so the next look is due now. A policy event (or
    /// any loss, which may hide one) is answered by a fresh policy read; a
    /// restart request is handed to [`Supervisor::restart_asked`] after it,
    /// unless the child that ended is about to be `replaced`: aterm sent it
    /// for that child, and the one started next is no stall and nobody's
    /// request (`ignored child-replaced`).
    pub(super) fn link_events(&mut self, replaced: bool) -> bool {
        let Link::Held(claimed) = &mut self.link else {
            return false;
        };
        let Some(events) = claimed.events.as_mut() else {
            return false;
        };
        let update = events.drain();
        claimed.recheck_at_ms = 0;
        if update.ended {
            claimed.events = None;
            claimed.events_retry_ms = now_ms().saturating_add(RECLAIM_EVERY_MS);
            self.journal.note(&format!(
                "aterm events-lost {}",
                update
                    .fault
                    .as_ref()
                    .map_or_else(|| "closed".to_string(), ToString::to_string)
            ));
        }
        if update.requires_refresh() {
            self.link_policy();
        }
        if let Some(restart) = &update.restart {
            if replaced {
                self.restart_ignored(&restart.cause, "child-replaced");
            } else {
                self.restart_asked(&restart.cause);
            }
        }
        true
    }

    /// Make the claim follow a fresh policy read ([`Claimed::follow_policy`]).
    fn link_policy(&mut self) {
        let Link::Held(claimed) = &mut self.link else {
            return;
        };
        match claimed.follow_policy(self.child.build.version.as_deref()) {
            Ok(Some(line)) => self.journal.note(&line),
            Ok(None) => {}
            Err(failure) => self.link_lost(failure.why()),
        }
    }

    /// aterm's word on ending the child now: one fresh policy read and one
    /// fresh status read ([`Claimed::judge`]). A link that fails here is lost,
    /// and the caller decides this look on its own readings.
    pub(super) fn link_safe_point(&mut self) -> Verdict {
        let grace = Duration::from_millis(self.setup.opts.quiet_ms);
        let Link::Held(claimed) = &mut self.link else {
            return Verdict::Local;
        };
        if !claimed.standing {
            return Verdict::Local;
        }
        match claimed.judge(grace) {
            Ok(Judged::Safe) => Verdict::Safe,
            Ok(Judged::Hold(why)) => Verdict::Hold(why),
            Ok(Judged::Taken(why)) => {
                self.link_lost(&format!("claim {why}"));
                Verdict::Hold(why)
            }
            Ok(Judged::Released(line)) => {
                self.journal.note(&line);
                Verdict::Local
            }
            Err(failure) => {
                // aterm's last word stands over one that never came: after a
                // hold the look holds, and only after a safe answer, or none,
                // do the harness's own readings decide.
                self.link_lost(failure.why());
                self.link.lost_hold().map_or(Verdict::Local, Verdict::Hold)
            }
        }
    }

    /// aterm's word on starting the child again: one fresh policy read and one
    /// fresh status read ([`Claimed::relaunch_word`]), for a crash (`asked`
    /// false) or a restart aterm asked for (`asked` true). With no link, or a
    /// claim given back because the policy went off, nothing is asked and
    /// nothing is permitted. A link that fails here is lost, and the caller may
    /// wait for it to be claimed again. A `status` that no longer shows the
    /// claim as this harness's loses the link too, as at a safe point, and
    /// permits nothing: aterm answered, and the duty is not this harness's.
    pub(super) fn link_relaunch_word(&mut self, asked: bool) -> Relaunch {
        let claimed = match &mut self.link {
            Link::Held(claimed) if claimed.standing => claimed,
            Link::Held(_) | Link::Absent => return Relaunch::Denied("no-claim".to_string()),
            Link::Lost { .. } => return Relaunch::Away("link-lost".to_string()),
        };
        match claimed.relaunch_word(asked) {
            Ok(Leave::Granted) => Relaunch::Permitted,
            Ok(Leave::Denied(why)) => Relaunch::Denied(why),
            Ok(Leave::Later(why)) => Relaunch::Later(why),
            Ok(Leave::Taken(why)) => {
                self.link_lost(&why);
                Relaunch::Denied(why)
            }
            Err(failure) => {
                self.link_lost(failure.why());
                Relaunch::Away(failure.why().to_string())
            }
        }
    }

    /// The child was relaunched: announce the claim again with the build it now
    /// runs, so `supervisor_vendor=` stays true. A refusal loses the link.
    pub(super) fn link_reannounce(&mut self) {
        if self.no_child() {
            return;
        }
        let child = live_child_proof(self.child.pid);
        let Link::Held(claimed) = &mut self.link else {
            return;
        };
        if claimed.child_proof {
            claimed.child_pending = true;
            let Some(child) = child else {
                // The child's native image facts may not yet be observable
                // immediately after spawn. Keep the target unannounced and
                // retry on the next poll; never guess a birth from wall time.
                return;
            };
            claimed.child = Some(child);
            claimed.child_pending = false;
        }
        // A hold aterm stated was on the child that ended.
        claimed.last_hold = None;
        if !claimed.standing {
            return;
        }
        claimed.recheck_at_ms = 0;
        let vendor = self.child.build.version.as_deref();
        let announced = ask(
            &mut claimed.conn,
            "reannounce",
            aterm::claim_child_generation_verb(
                claimed.locator.sid(),
                claimed.pid,
                &SCOPES,
                VERSION,
                vendor,
                claimed.child.as_ref().filter(|_| claimed.child_proof),
                claimed.generation.filter(|_| claimed.child_start),
            ),
        )
        .and_then(|reply| {
            if ok(&reply) {
                Ok(())
            } else {
                Err(Failure::Refused(format!(
                    "reannounce {}",
                    reply_words(&reply)
                )))
            }
        });
        match announced {
            Ok(()) => self.journal.note(&format!(
                "aterm reannounced vendor={}",
                vendor.unwrap_or("-")
            )),
            Err(failure) => self.link_lost(failure.why()),
        }
    }

    /// The harness is about to exit: give a standing claim back and close the
    /// link ([`Link::release`]).
    pub(super) fn link_release(&mut self) {
        self.link.release();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_hold_is_read_again_after_the_grace_and_within_five_seconds() {
        assert_eq!(recheck_after_ms(None), RECLAIM_EVERY_MS);
        assert_eq!(recheck_after_ms(Some(Duration::ZERO)), 1_000);
        assert_eq!(recheck_after_ms(Some(Duration::from_millis(1_500))), 2_500);
        assert_eq!(recheck_after_ms(Some(Duration::from_secs(4))), 5_000);
        assert_eq!(recheck_after_ms(Some(Duration::from_secs(119))), 5_000);
        assert_eq!(recheck_after_ms(Some(Duration::MAX)), 5_000);
    }

    #[test]
    fn a_refused_reply_is_two_journal_words() {
        assert!(ok("OK") && ok("OK gen=1") && !ok("OKAY") && !ok("ERR busy") && !ok(""));
        assert_eq!(
            reply_words("ERR busy supervisor=someone-else@77"),
            "reply=ERR:busy"
        );
        assert_eq!(reply_words("ERR"), "reply=ERR");
        assert_eq!(reply_words(""), "reply=(empty)");
    }

    #[test]
    fn a_policy_admits_a_claim_only_with_contract_three_in_effect() {
        let policy = |contract: &str, effective: &str| {
            Policy::parse(&format!(
                "OK gen=1 effective={effective} contract={contract} relaunch=true upgrade=true \
                 upgrade_ask=false human_grace_s=120"
            ))
            .expect("a policy")
        };
        assert_eq!(admits(&policy("2,3", "on")), Ok(()));
        assert_eq!(
            admits(&policy("2", "on")),
            Err(Failure::Refused("policy contract=2".to_string()))
        );
        // Off by a setting a person can switch on again while the session
        // runs, or by a fact of the window that stands.
        for why in ["disabled", "agent-off", "autopilot-off"] {
            assert_eq!(
                admits(&policy("2,3", &format!("off why={why}"))),
                Err(Failure::Off(format!("policy effective=off why={why}")))
            );
        }
        for why in ["unhosted", "headless"] {
            assert_eq!(
                admits(&policy("2,3", &format!("off why={why}"))),
                Err(Failure::Refused(format!("policy effective=off why={why}")))
            );
        }
        // A halt, aterm's own update and a reason not known here pass: the
        // answer stands for now only.
        for why in ["hold", "suspended", "something-new"] {
            assert_eq!(
                admits(&policy("2,3", &format!("off why={why}"))),
                Err(Failure::Later(format!("policy effective=off why={why}")))
            );
        }
        assert_eq!(
            admits(&policy("2,3", "off")),
            Err(Failure::Later("policy effective=off".to_string()))
        );
        // The contract is named first: a host that does not serve it has no
        // policy this harness reads.
        assert_eq!(
            admits(&policy("4", "off")).unwrap_err().why(),
            "policy contract=4"
        );
    }

    #[test]
    fn a_reply_aterm_could_not_give_now_is_asked_again_and_a_bad_ok_is_refused() {
        assert_eq!(
            unreadable(
                "policy",
                &Policy::parse("ERR aterm is showing a dialog; answer it, then retry").unwrap_err()
            ),
            Failure::Later("policy reply=ERR:aterm".to_string())
        );
        assert_eq!(
            unreadable(
                "status",
                &Status::parse("ERR main thread stalled 6s; retry").unwrap_err()
            ),
            Failure::Later("status reply=ERR:main".to_string())
        );
        // Any other refusal stands: an unknown session, a principal aterm
        // denies, a verb it does not serve.
        for reply in [
            "ERR denied",
            "ERR no such session 3",
            "ERR usage: status",
            "",
        ] {
            assert!(
                matches!(
                    unreadable("status", &Status::parse(reply).unwrap_err()),
                    Failure::Refused(_)
                ),
                "{reply:?}"
            );
            assert!(
                matches!(
                    unreadable("policy", &Policy::parse(reply).unwrap_err()),
                    Failure::Refused(_)
                ),
                "{reply:?}"
            );
        }
        assert_eq!(
            unreadable(
                "policy",
                &Policy::parse("ERR no such session 3").unwrap_err()
            ),
            Failure::Refused("policy reply=ERR:no".to_string())
        );
        assert_eq!(
            unreadable("policy", &Policy::parse("OK effective=on").unwrap_err()),
            Failure::Refused("policy gen=absent".to_string())
        );
        assert_eq!(
            unreadable("status", &Status::parse("OK hold=0").unwrap_err()),
            Failure::Refused("status schema=absent".to_string())
        );
    }

    #[test]
    fn the_claim_line_names_the_holder_both_duties_and_the_vendor_build() {
        assert_eq!(
            claimed_line("claude-code-alab@77", Some("2.1.311")),
            "aterm claimed holder=claude-code-alab@77 scope=relaunch,upgrade vendor=2.1.311"
        );
        assert_eq!(
            claimed_line("claude-code-alab@77", None),
            "aterm claimed holder=claude-code-alab@77 scope=relaunch,upgrade vendor=-"
        );
    }

    #[test]
    fn the_claim_takes_relaunch_and_upgrade_and_nothing_else() {
        assert_eq!(scope_words(), "relaunch,upgrade");
        assert_eq!(
            aterm::claim_verb(
                "s-0000000000000000001a",
                77,
                &SCOPES,
                "0.2.0",
                Some("2.1.311")
            )
            .as_deref(),
            Some(
                "@s-0000000000000000001a meta set supervisor claude-code-alab@77 contract=3 scope=relaunch,upgrade \
                 version=0.2.0 vendor=2.1.311"
            )
        );
    }
}
