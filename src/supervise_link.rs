// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Claude's reactor and journal adapter for the shared harness link.
//!
//! The shared link owns claims, policy reads, events, child generations and
//! reconnects. This module supplies Claude's vendor metadata and already-owned
//! native observations, then drains its notes into the session journal. Input
//! decisions, the crash budget and replacing the vendor remain the supervisor's.
//!
//! **What the claim takes.** `relaunch`, `upgrade` and, under contract 4
//! from a launch that reaches aterm's token helper, `accounts` ([`config`]),
//! and nothing that types ([`UNTYPED`]). aterm
//! admits a harness's typed write only on the connection that made the
//! claim, and a typed exchange is a counted reply (`turn`, `text --json`);
//! the shared link reads one reply line per request, and under contract 4
//! its claim connection is the session stream. Until the shared link carries
//! a fenced, counted exchange on the claim's connection, the typing duties
//! (`turn-end`, `approvals`, `questions`, `walls`) are not claimed: aterm's
//! own engine keeps them, and the deciders and drivers of
//! `src/supervise_duty.rs`, `src/supervise_box_duty.rs` and
//! `src/supervise_wall_duty.rs` find no mode to act under
//! ([`Supervisor::link_duty_mode`]). What the harness does of its own accord
//! under `relaunch` (the move off Fable's limit and back) is judged here on
//! one fresh policy and status pair ([`Point`]).
//!
//! **The account moves.** `accounts` types nothing and asks nothing: aterm
//! chooses the account and tells the claim (`restart cause=account to=<key>
//! home=<dir|->`, [`asked`]), and the supervisor ends its child at a point it
//! judges safe ([`Point`], by its own witness) and starts it again on the
//! named account ([`Supervisor::restart_asked`]). The answer is the claim
//! and nothing else: it names the new child once that is observed, and the
//! account (`login=<key>`, [`Link::set_login`]) only once the vendor runs on
//! it ([`Supervisor::link_restate`]). No reply line and no record says a move
//! was made or refused; aterm reads the claim and the child itself.
//!
//! **What the stream is told.** On an admitted contract 4 stream the harness
//! says three things of its own: the child's `model` facts
//! ([`Link::model`]), how its conversation resumes (`session`,
//! [`Link::session`]), and where a move onto a newer vendor build stands
//! (`upgrade`, [`Link::upgrade`]). A `session` record is what aterm's
//! watchdog types after `claude-alab run --` when this harness is gone; an
//! `upgrade` record is what aterm shows of a move this harness makes. Each is
//! put once per change ([`Told`]), only while a stream is open, and put again
//! when a stream opens on an aterm that holds nothing of this claimant
//! ([`alab_wire::Drained::fresh`]).

use std::os::fd::RawFd;
use std::time::Duration;

use alab_wire::claim::Human;
use alab_wire::record::{Dropped, Mode, UpgradePhase};
use alab_wire::{
    Body, Child, ChildProcess, Config, Contract, Facts, NativeChild, NativeObservation, Reading,
    Scope, SendError,
};

use super::{Supervisor, now_ms};
use crate::aterm::{self, Autopilot, Policy, Scope as Duty, Status};
use crate::boxes::BoxPolicy;
use crate::cli::VERSION;
use crate::duty::{Pace, Texts};
use crate::journal::Journal;
use crate::model_facts::ModelFacts;
use crate::procs;
use crate::typing::{Press, Screen, Typed, Write};
use crate::walls::WallPolicy;

pub(super) use alab_wire::link::LATER_RETRY_MS;
pub(super) use alab_wire::{Relaunch, Verdict};

/// Total time one Claude crash or requested restart waits for the host's claim
/// or permission answer, independently of the shared link's per-request clock.
pub(super) const RECLAIM_HOLD_MS: u64 = 45_000;

/// The claim this launch makes. It asks for the account moves only where
/// the launch reaches aterm's token helper ([`crate::launch::token_helper`]),
/// the one program that may start the vendor on an account aterm names: the
/// claim is the only answer aterm reads, and a launch that could make no
/// move (started by hand in a tab, its host absent) must not say it takes
/// them. aterm would commit a move, tell it, wait out its bounds and roll it
/// back, again and again, where a claim without the word is answered at once
/// with nothing committed. A unit test's own environment says nothing of the
/// launch it stands for, so it claims as a launch with a helper does.
fn config() -> Config {
    #[cfg(test)]
    let helper = true;
    #[cfg(not(test))]
    let helper = crate::launch::token_helper().is_some();
    config_for(helper)
}

/// The claim of a launch that reaches aterm's token helper (`helper`), or
/// of one that does not ([`config`]).
fn config_for(helper: bool) -> Config {
    // `accounts` is a contract 4 word: the shared link leaves it out of a
    // contract 3 claim, which is the claim it always was.
    let scopes: &[Scope] = if helper {
        &[Scope::Relaunch, Scope::Upgrade, Scope::Accounts]
    } else {
        &[Scope::Relaunch, Scope::Upgrade]
    };
    Config::new("claude-code-alab", "claude", scopes, VERSION)
        .expect("compiled Claude claim configuration")
        .with_contracts(&[Contract::Three, Contract::Four])
        .expect("the supported claim contracts")
        .with_c3_child_tracking()
}

/// Why this harness types nothing: the journal's word
/// (`duty untyped shared-link …`). The shared link's claim names no typing
/// duty (the module's own words say why), so each stays aterm's.
pub(super) const UNTYPED: &str = "shared-link";

/// At most how long a hold on the harness's own restart waits before aterm is
/// read again; an ageing `human_ms` emits no event, so its end is read for.
const OWN_RECHECK_MS: u64 = 5_000;

fn metadata<'a>(
    vendor: Option<&'a str>,
    process: Option<ChildProcess>,
    login: Option<&'a str>,
) -> Child<'a> {
    Child {
        vendor,
        process,
        login,
    }
}

/// The moment aterm is asked to judge before the harness ends its child of
/// its own accord.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Point {
    /// A quiet moment, for the duty the restart is made under: `upgrade` for
    /// a move onto another vendor build (the shared link's own safe point),
    /// `relaunch` for the session's return onto its launch's model.
    Quiet(Duty),
    /// A child that stands at a usage limit, the vendor's dialog open.
    Wall,
}

/// What one look of the typing duties read on the claim's connection
/// ([`Supervisor::link_duty_read`]).
pub(super) struct DutyRead {
    /// The policy, which a typed write is judged under.
    pub(super) policy: Policy,
    /// The words the turn-end duty types, from the same reply.
    pub(super) texts: Texts,
    /// The pace of the turn-end duty's acts, from the same reply.
    pub(super) pace: Pace,
    /// What the box duties may press, from the same reply.
    pub(super) boxes: BoxPolicy,
    /// What the walls duty may type, from the same reply.
    pub(super) walls: WallPolicy,
    /// The session's status.
    pub(super) status: Status,
    /// The session's screen.
    pub(super) screen: Screen,
}

/// The first of aterm's facts that holds a restart the harness makes of its
/// own accord under `relaunch`, as the journal's wait line with the rest of
/// the person's grace where that is what holds; `None` when none does. The
/// pair was read on a standing claim `status` shows as this harness's and
/// proven, under a policy in effect ([`Reading::Fresh`]). Checked in order:
///
/// 1. `supervisor_scope` names `relaunch`;
/// 2. away from a wall, `agent=idle`;
/// 3. `composer=empty` or `placeholder`, and at a `wall` also `-` (the
///    vendor's limit dialog draws no composer);
/// 4. `human_ms=-`, or at least the larger of the policy's `human_grace_s`
///    and `grace`;
/// 5. `hand=-`, `hold=0`, `input=clear`, and away from a wall `need=-` (a
///    session at a limit may read as needing a person).
///
/// A field that is absent or unreadable holds; nothing is assumed.
fn own_hold(facts: &Facts, grace: Duration, wall: bool) -> Option<(String, Option<Duration>)> {
    let Facts { policy, status } = facts;
    let hold = |field: &str, value: Option<&str>| {
        Some((
            format!(
                "wait aterm {field}={}",
                value.map_or_else(|| "absent".to_string(), aterm::journal_word)
            ),
            None,
        ))
    };
    if !status.takes(Scope::Relaunch) {
        let scope = status.supervisor_scope.join(",");
        return hold(
            "supervisor_scope",
            Some(if scope.is_empty() { "-" } else { &scope }),
        );
    }
    if !wall && status.agent.as_deref() != Some("idle") {
        return hold("agent", status.agent.as_deref());
    }
    let composer = status.composer.as_deref();
    if !(matches!(composer, Some("empty" | "placeholder")) || (wall && composer == Some("-"))) {
        return hold("composer", composer);
    }
    let grace = policy.human_grace.max(grace);
    match status.human {
        Human::Never => {}
        Human::Ago(ago) if ago >= grace => {}
        Human::Ago(ago) => {
            return Some((
                format!("wait aterm human_ms={}", ago.as_millis()),
                Some(grace.saturating_sub(ago)),
            ));
        }
        Human::Unknown => return hold("human_ms", Some("unknown")),
    }
    let need = (!wall).then_some(("need", status.need.as_deref(), "-"));
    [
        ("hand", status.hand.as_deref(), "-"),
        ("hold", status.hold.as_deref(), "0"),
        ("input", status.input.as_deref(), "clear"),
    ]
    .into_iter()
    .chain(need)
    .find(|(_, value, wanted)| *value != Some(*wanted))
    .and_then(|(field, value, _)| hold(field, value))
}

/// aterm's restart request as the supervisor reads it. The shared link
/// names an account's `to=` word its `label`; aterm sends the account's key
/// there, and the supervisor moves a session only onto a word that is one
/// ([`aterm::account_key`]).
fn asked(restart: alab_wire::claim::Restart) -> aterm::Restart {
    aterm::Restart {
        cause: restart.cause,
        model: restart.model,
        account: restart.account.map(|account| aterm::Account {
            key: account.label,
            home: account.home,
        }),
    }
}

/// The supervisor's handle to the shared state machine. Hosted own-image
/// restoration starts absent until that adoption is independently qualified.
pub(super) struct Link {
    core: alab_wire::Link,
    /// The kernel lifetime of our current child, once observed for contract 4.
    /// We hold its original wait handle until `child_ended` clears this fact.
    process: Option<ChildProcess>,
    model: ModelDelivery,
    /// How the child's conversation resumes ([`Link::session`]).
    session: Told,
    /// Where the move onto a newer vendor build stands ([`Link::upgrade`]).
    upgrade: Told,
    /// Until when a hold on the harness's own restart stands without a fresh
    /// read ([`Link::own_point`]); `0` for none.
    own_recheck_at_ms: u64,
    /// The key of the account the child runs on, which every claim made
    /// from here names (`login=`); `None` on the launch's own login.
    login: Option<String>,
    /// A test of the account moves stands in for a claim granted `accounts`.
    #[cfg(test)]
    pub(super) accounts_granted: bool,
    /// A test of the account moves stands in for a launch that reaches
    /// aterm's token helper ([`Link::token_helper`]).
    #[cfg(test)]
    pub(super) token_helper: bool,
}

/// A dropped Model is not delivery. Keep at most one model in flight and
/// read the latest facts again when it is acknowledged or the stream opens.
#[derive(Default)]
struct ModelDelivery {
    opened: u64,
    pending: Option<(u64, Body)>,
    settled: Option<Body>,
    retry_at: u64,
}

const MODEL_RETRY_MS: u64 = 1_000;

/// One record the harness tells aterm of its own state: the newest it wants
/// aterm to hold, and the one last put on the stream. Only a change is put;
/// a record the wire cannot write is not tried again until it changes.
#[derive(Debug, Default)]
struct Told {
    wanted: Option<Body>,
    put: Option<Body>,
}

impl Told {
    /// What is still to be put.
    fn due(&self) -> Option<&Body> {
        self.wanted
            .as_ref()
            .filter(|wanted| self.put.as_ref() != Some(wanted))
    }
}

/// What a `session` record says of the child's conversation
/// ([`Link::session`]); the link adds the agent, the versions and the
/// account.
pub(super) struct Resumes<'a> {
    /// The vendor build's version, as the claim's `vendor=` says it.
    pub(super) vendor: Option<&'a str>,
    /// The conversation the child's session record names.
    pub(super) conversation: &'a str,
    /// The directory the conversation runs in, as its record states it.
    pub(super) cwd: &'a str,
    /// The words after `claude-alab run --` that resume it; none where it
    /// cannot be resumed from here.
    pub(super) resume: Vec<String>,
    /// A child started since the last record was made: it is put even
    /// where it says what the last one said.
    pub(super) started: bool,
}

/// `body`'s journal words after `aterm told <kind>`, its `seq` included.
fn told_words(body: &Body, seq: u64) -> String {
    match body {
        Body::Session {
            account,
            conversation,
            vendor,
            resume,
            ..
        } => format!(
            "session seq={seq} conversation={} account={} vendor={} argv={}",
            conversation.as_deref().unwrap_or("-"),
            account.as_deref().unwrap_or("-"),
            vendor.as_deref().unwrap_or("-"),
            if resume.is_empty() {
                "-".to_string()
            } else {
                resume.join("\u{1f}")
            }
        ),
        Body::Upgrade { phase, to } => format!("upgrade seq={seq} phase={} to={to}", phase.word()),
        // Only the two above are told.
        _ => format!("record seq={seq}"),
    }
}

impl ModelDelivery {
    fn opening(&mut self, opened: u64) {
        if self.opened != opened {
            *self = Self {
                opened,
                ..Self::default()
            };
        }
    }

    fn ack(&mut self, seq: u64, dropped: Option<&Dropped>, now: u64) {
        let Some((_, body)) = self.pending.take_if(|(sent, _)| seq == *sent) else {
            return;
        };
        let retry = matches!(dropped, Some(Dropped::Rate | Dropped::Applied))
            || matches!(dropped, Some(Dropped::Refused(reason)) if reason == "child");
        if retry {
            self.retry_at = now.saturating_add(MODEL_RETRY_MS);
        } else {
            // A success or a permanent refusal settles the snapshot. The
            // link already records the reason for a refusal.
            self.settled = Some(body);
            self.retry_at = 0;
        }
    }
}

impl Link {
    pub(super) fn absent() -> Self {
        Self {
            core: alab_wire::Link::absent(config(), std::process::id()),
            process: None,
            model: ModelDelivery::default(),
            session: Told::default(),
            upgrade: Told::default(),
            own_recheck_at_ms: 0,
            login: None,
            #[cfg(test)]
            accounts_granted: false,
            #[cfg(test)]
            token_helper: false,
        }
    }

    pub(super) fn launch(journal: &Journal, vendor: Option<&str>) -> Self {
        if !super::image::hosted_by_aterm() {
            return Self::absent();
        }
        let mut link = Self {
            core: alab_wire::Link::launch(
                config(),
                std::process::id(),
                metadata(vendor, None, None),
                now_ms(),
            ),
            process: None,
            model: ModelDelivery::default(),
            session: Told::default(),
            upgrade: Told::default(),
            own_recheck_at_ms: 0,
            login: None,
            #[cfg(test)]
            accounts_granted: false,
            #[cfg(test)]
            token_helper: false,
        };
        link.notes(journal);
        link
    }

    #[cfg(test)]
    pub(super) fn launch_at(
        locator: &alab_wire::wire::Locator,
        vendor: Option<&str>,
        now: u64,
    ) -> Self {
        Self {
            core: alab_wire::Link::launch_at(
                config(),
                std::process::id(),
                locator,
                metadata(vendor, None, None),
                now,
            ),
            process: None,
            model: ModelDelivery::default(),
            session: Told::default(),
            upgrade: Told::default(),
            own_recheck_at_ms: 0,
            login: None,
            #[cfg(test)]
            accounts_granted: false,
            #[cfg(test)]
            token_helper: false,
        }
    }

    fn notes(&mut self, journal: &Journal) {
        for line in self.core.take_notes() {
            journal.note(&line);
        }
    }

    /// Each fresh attempt, including a failed target's fallback, gets its own
    /// pre-spawn transaction. The caller retires a prior child at its wait event.
    pub(super) fn before_spawn(&mut self, journal: &Journal, vendor: Option<&str>) {
        let _ = self.core.before_spawn(vendor, now_ms());
        self.notes(journal);
    }

    pub(super) fn child_ended(&mut self, journal: &Journal, vendor: Option<&str>) {
        let retired = self.process.take();
        self.model = ModelDelivery::default();
        self.core.child_ended();
        if retired.is_some() && self.contract_four() {
            // The host re-proves its standing child on a claim record. Its
            // original wait event is already consumed, so it cannot be kept.
            self.core
                .reannounce(metadata(vendor, None, self.login.as_deref()), now_ms());
            self.notes(journal);
        }
    }

    /// Report the latest complete snapshot for the observed child. A missing
    /// fact is an explicit absence in this snapshot. Wait for the stream's
    /// child announcement to be acknowledged before numbering model records.
    /// The host still checks the child on every record it applies.
    pub(super) fn model(&mut self, facts: &ModelFacts, now: u64) {
        let Some(child) = self.process else {
            return;
        };
        if !self.core.streaming() || self.core.claim_in_flight() {
            return;
        }
        self.model.opening(self.core.counts().opened);
        if self.model.pending.is_some() || now < self.model.retry_at {
            return;
        }
        let body = Body::Model {
            child: Some(child),
            model: facts.model.shown().cloned(),
            effort: facts.effort.shown().cloned(),
            key: facts.key.shown().copied(),
            offers: Vec::new(),
            permission: None,
        };
        if self.model.settled.as_ref() == Some(&body) {
            return;
        }
        if let Ok(seq) = self.core.send(body.clone(), now, now) {
            self.model.pending = Some((seq, body));
        }
    }

    /// Tell aterm how the child's conversation resumes: a `session` record
    /// whose `resume` words, typed after `claude-alab run --` in its `cwd`,
    /// start the same conversation on the same model, on the account every
    /// claim names (`login=`). It names no child: aterm keeps the newest from
    /// this claimant and types it once this harness is gone. Put now, or at
    /// the first [`Link::tell`] with a stream open; one that says what the
    /// last said is put again only for a child that started since.
    pub(super) fn session(&mut self, said: Resumes<'_>, journal: &Journal, now: u64) {
        if said.started {
            self.session.put = None;
        }
        let config = self.core.config();
        self.session.wanted = Some(Body::Session {
            agent: config.vendor().to_string(),
            version: config.version().to_string(),
            vendor: said.vendor.map(str::to_string),
            account: self.login.clone(),
            conversation: Some(said.conversation.to_string()),
            cwd: Some(said.cwd.to_string()),
            resume: said.resume,
            mode: Mode::Tui,
            // None of aterm's own records is taken here.
            takes: Vec::new(),
        });
        self.tell(journal, now);
    }

    /// Tell aterm that the move onto the vendor version `to` (`-` where its
    /// build states none) is at `phase`: an `upgrade` record, put now or at
    /// the first [`Link::tell`] with a stream open. Phases told while no
    /// stream is open are not kept: the newest stands for them.
    pub(super) fn upgrade(&mut self, phase: UpgradePhase, to: &str, journal: &Journal, now: u64) {
        self.upgrade.wanted = Some(Body::Upgrade {
            phase,
            to: to.to_string(),
        });
        self.tell(journal, now);
    }

    /// Put what is still to be told, while a stream is open: one journal line
    /// each, `aterm told session seq=<n> conversation=<id> account=<key|->
    /// vendor=<version|-> argv=<resume words|->` or `aterm told upgrade
    /// seq=<n> phase=<phase> to=<version|->`. A record the wire cannot write
    /// is journalled `aterm told <kind> unwritable <why>` and not tried again
    /// until it changes; one the stream has no room for now is tried again at
    /// the next look.
    pub(super) fn tell(&mut self, journal: &Journal, now: u64) {
        if !self.core.streaming() {
            return;
        }
        for (kind, told) in [
            ("session", &mut self.session),
            ("upgrade", &mut self.upgrade),
        ] {
            let Some(body) = told.due().cloned() else {
                continue;
            };
            match self.core.send(body.clone(), now, now) {
                Ok(seq) => journal.note(&format!("aterm told {}", told_words(&body, seq))),
                Err(SendError::Line(why)) => {
                    journal.note(&format!("aterm told {kind} unwritable {why}"));
                }
                Err(_) => continue,
            }
            told.put = Some(body);
        }
    }

    /// A stream opened on an aterm that holds nothing this claimant sent
    /// ([`alab_wire::Drained::fresh`]): the conversation's `session` record is
    /// to be put again, and so is the move's `upgrade` record while the move
    /// is still to finish. They are put at the next look
    /// ([`Supervisor::session_look`]), which first describes a child that
    /// started while no stream was open, so nothing stale is put.
    fn reopened(&mut self) {
        self.session.put = None;
        if matches!(
            self.upgrade.wanted,
            Some(Body::Upgrade {
                phase: UpgradePhase::Seen | UpgradePhase::Waiting | UpgradePhase::Moving,
                ..
            })
        ) {
            self.upgrade.put = None;
        }
    }

    pub(super) fn fds(&self) -> [Option<RawFd>; 2] {
        [self.core.event_fd(), self.core.stream_fd()]
    }

    pub(super) fn due_ms(&self) -> Option<u64> {
        self.core.due_ms()
    }

    pub(super) fn streaming(&self) -> bool {
        self.core.streaming()
    }

    fn contract_four(&self) -> bool {
        self.core
            .grant()
            .is_some_and(|grant| grant.contract == Contract::Four)
    }

    pub(super) fn judges(&self) -> bool {
        self.core.judges()
    }

    pub(super) fn held_back(&self, now: u64) -> bool {
        self.core.held_back(now) || (self.core.judges() && now < self.own_recheck_at_ms)
    }

    /// Whether the link is absent for the rest of the session: the harness
    /// is outside aterm, or its session cannot be located.
    pub(super) const fn absent_for_good(&self) -> bool {
        self.core.is_absent()
    }

    /// Whether aterm's account moves are this harness's: the standing claim
    /// was granted `accounts`, which it asks for under contract 4 where the
    /// launch reaches aterm's token helper ([`config`]). Under contract 3,
    /// from a launch with no helper, on a development link, under a person's
    /// cap that leaves the word out, and on an aterm that does not know it,
    /// the grant is absent: an account move aterm sends is then not made
    /// here.
    pub(super) fn moves_accounts(&self) -> bool {
        #[cfg(test)]
        if self.accounts_granted {
            return true;
        }
        self.core.takes(Scope::Accounts)
    }

    /// Whether this launch reaches aterm's token helper
    /// ([`crate::launch::token_helper`]), the one program that may start the
    /// vendor on an account aterm names. A test states it, since the test
    /// process's own environment says nothing of the launch it stands for.
    pub(super) fn token_helper(&self) -> bool {
        #[cfg(test)]
        {
            self.token_helper
        }
        #[cfg(not(test))]
        {
            // The launch's environment decides, the same for every start.
            let Self { .. } = self;
            crate::launch::token_helper().is_some()
        }
    }

    /// The child now runs on the account `key` names: every claim made from
    /// here says `login=<key>`. `None` is the launch's own login, which a
    /// claim names no `login=` for.
    pub(super) fn set_login(&mut self, key: Option<&str>) {
        self.login = key.map(str::to_string);
    }

    /// aterm's word on a restart the harness makes of its own accord under
    /// `relaunch`, at a quiet moment or at a `wall`: one fresh policy and
    /// status pair ([`alab_wire::Link::read`]) judged by [`own_hold`]. A hold
    /// stands for the rest of the person's grace and a second, and at most
    /// [`OWN_RECHECK_MS`], before aterm is read again
    /// ([`Link::held_back`]). With no standing claim, or where aterm did not
    /// answer and stated no hold before, the harness's own readings decide.
    fn own_point(&mut self, grace: Duration, wall: bool, now: u64) -> Verdict {
        self.own_recheck_at_ms = 0;
        match self.core.read(now) {
            Reading::Fresh(facts) => match own_hold(&facts, grace, wall) {
                None => Verdict::Safe,
                Some((why, grace_left)) => {
                    let wait = grace_left.map_or(OWN_RECHECK_MS, |left| {
                        u64::try_from(left.as_millis())
                            .unwrap_or(u64::MAX)
                            .saturating_add(1_000)
                            .min(OWN_RECHECK_MS)
                    });
                    self.own_recheck_at_ms = now.saturating_add(wait);
                    Verdict::Hold(why)
                }
            },
            Reading::Hold(why) => Verdict::Hold(why),
            Reading::Local => Verdict::Local,
        }
    }

    pub(super) const fn claims(&self) -> bool {
        self.core.claims()
    }

    pub(super) const fn lost(&self) -> bool {
        self.core.is_lost()
    }

    pub(super) fn lost_hold(&self) -> Option<String> {
        self.core.lost_hold()
    }

    pub(super) fn release(&mut self) {
        self.core.release(now_ms());
    }
}

/// Convert the existing native observation without changing its kernel units.
/// C4's epoch metadata is a separate type and is never populated here.
fn native_child(identity: &procs::ChildIdentity) -> Option<NativeChild> {
    let birth = match identity.birth {
        procs::Birth::MacOs {
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
        procs::Birth::Linux(ticks) => ticks,
    };
    NativeChild::from_native(identity.pid, birth)
}

fn live_child(pid: libc::pid_t) -> Option<NativeChild> {
    let pid = u32::try_from(pid).ok()?;
    native_child(&procs::capture_child(pid)?)
}

impl Supervisor {
    pub(super) fn link_tick(&mut self) {
        self.link_tick_with(now_ms(), live_child);
    }

    fn link_tick_with(
        &mut self,
        now: u64,
        mut observe: impl FnMut(libc::pid_t) -> Option<NativeChild>,
    ) {
        let no_child = self.no_child();
        let pid = self.child.pid;
        let four = self.link.contract_four();
        // A held C3 link keeps its existing lazy native observation. If the
        // host restarts, reclaim can negotiate either contract for this child.
        let new_child = if !no_child
            && (four || self.link.core.is_lost())
            && self.link.process.is_none()
            && !self.child.identity_pending
            && self.child.identity.is_some()
        {
            let observed = u32::try_from(pid)
                .ok()
                .and_then(|pid| ChildProcess::of(pid).ok());
            self.link.process = observed;
            observed.is_some()
        } else {
            false
        };
        let child = metadata(
            self.child.build.version.as_deref(),
            self.link.process,
            self.link.login.as_deref(),
        );
        self.link.core.tick_native_with(child, now, || {
            if no_child {
                NativeObservation::Unknown
            } else {
                observe(pid).map_or(NativeObservation::Unknown, NativeObservation::Present)
            }
        });
        if four && new_child {
            self.link.core.reannounce(child, now);
        }
        self.link.notes(&self.journal);
    }

    pub(super) fn link_events(&mut self, replaced: bool) -> bool {
        let now = now_ms();
        let update = self.link.core.events(
            metadata(
                self.child.build.version.as_deref(),
                self.link.process,
                self.link.login.as_deref(),
            ),
            now,
        );
        self.link.model.opening(self.link.core.counts().opened);
        for ack in &update.model_acks {
            self.link.model.ack(ack.seq, ack.dropped.as_ref(), now);
        }
        self.link.notes(&self.journal);
        if update.fresh {
            self.link.reopened();
        }
        if update.drained {
            // An event may end a hold: the next look reads aterm afresh.
            self.link.own_recheck_at_ms = 0;
            self.turn_end.wake();
        }
        if let Some(restart) = update.restart {
            let restart = asked(restart);
            if replaced {
                self.restart_for_replaced(&restart);
            } else {
                self.restart_asked(&restart);
            }
        }
        update.drained
    }

    /// aterm's word on ending the child now, at the moment `point`: the
    /// shared link's own safe point for a vendor update, and one fresh pair
    /// judged here for what the harness does under `relaunch`
    /// ([`Link::own_point`]).
    pub(super) fn link_safe_point(&mut self, point: Point) -> Verdict {
        let grace = Duration::from_millis(self.setup.opts.quiet_ms);
        let now = now_ms();
        let result = match point {
            Point::Quiet(Duty::Upgrade) => self.link.core.safe_point(grace, now),
            Point::Quiet(_) => self.link.own_point(grace, false, now),
            Point::Wall => self.link.own_point(grace, true, now),
        };
        self.link.notes(&self.journal);
        result
    }

    /// The session's Autopilot mode as the typing duties may rely on it.
    /// The shared link's claim takes no typing duty ([`UNTYPED`]), so none is
    /// stated, and no typing duty reads, types or reports anything.
    pub(super) fn link_duty_mode(&mut self) -> Option<Autopilot> {
        None
    }

    /// One look's reads for the typing duties on the claim's own connection.
    /// `None`: the shared link carries no counted reply ([`UNTYPED`]).
    pub(super) fn link_duty_read(&mut self) -> Option<DutyRead> {
        None
    }

    /// One typed write of the typing duty `duty` on the claim's own
    /// connection. `None`: nothing is typed ([`UNTYPED`]).
    pub(super) fn link_duty_write(
        &mut self,
        _policy: &Policy,
        _duty: Duty,
        _write: Write<'_>,
    ) -> Option<Typed> {
        None
    }

    /// One key of the typing duty `duty` on a box or a question dialog.
    /// `None`: nothing is pressed ([`UNTYPED`]).
    pub(super) fn link_duty_press(
        &mut self,
        _policy: &Policy,
        _duty: Duty,
        _press: &Press<'_>,
    ) -> Option<Typed> {
        None
    }

    /// Report `act`, the rest of Land's check, to aterm's task record.
    /// `None`: Land's check is not this harness's to answer ([`UNTYPED`]).
    pub(super) fn link_report(&mut self, _act: aterm::LandAct) -> Option<aterm::Reported> {
        None
    }

    pub(super) fn link_relaunch_word(&mut self, asked: bool) -> Relaunch {
        let result = self.link.core.relaunch_word(asked, now_ms());
        self.link.notes(&self.journal);
        result
    }

    pub(super) fn link_reannounce(&mut self) {
        if self.no_child() || self.link.core.is_absent() || self.link.core.is_lost() {
            return;
        }
        if self.link.contract_four() {
            self.link_tick();
            return;
        }
        let observed = live_child(self.child.pid)
            .map_or(NativeObservation::Unknown, NativeObservation::Present);
        self.link.core.reannounce_native(
            metadata(
                self.child.build.version.as_deref(),
                None,
                self.link.login.as_deref(),
            ),
            observed,
            now_ms(),
        );
        self.link.notes(&self.journal);
    }

    /// Say the standing claim again as it stands now, though it names a
    /// child aterm was already told of: what the claim says of that child
    /// changed (the account it runs on, `login=`), or aterm asked for what
    /// the session already runs on and is answered by the claim alone.
    /// [`Supervisor::link_reannounce`] names a child once, when it is first
    /// observed under contract 4; this says the same child again.
    pub(super) fn link_restate(&mut self) {
        if self.no_child() || self.link.core.is_absent() || self.link.core.is_lost() {
            return;
        }
        if !self.link.contract_four() {
            return self.link_reannounce();
        }
        let named = self.link.process;
        self.link_tick();
        // A child the tick observed just now was announced by it.
        if named.is_some() && named == self.link.process {
            self.link.core.reannounce(
                metadata(
                    self.child.build.version.as_deref(),
                    named,
                    self.link.login.as_deref(),
                ),
                now_ms(),
            );
            self.link.notes(&self.journal);
        }
    }

    pub(super) fn link_release(&mut self) {
        self.link.release();
    }
}

#[cfg(test)]
#[path = "supervise_link_tests.rs"]
pub(super) mod tests;
