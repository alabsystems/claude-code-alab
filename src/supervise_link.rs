// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Claude's reactor and journal adapter for the shared harness link.
//!
//! The shared link owns claims, policy reads, events, child generations and
//! reconnects. This module supplies Claude's vendor metadata and already-owned
//! native observations, then drains its notes into the session journal. Input
//! decisions, the crash budget and replacing the vendor remain the supervisor's.

use std::os::fd::RawFd;
use std::time::Duration;

use alab_wire::record::Dropped;
use alab_wire::{
    Body, Child, ChildProcess, Config, Contract, NativeChild, NativeObservation, Scope,
};

use super::{Supervisor, now_ms};
use crate::cli::VERSION;
use crate::journal::Journal;
use crate::model_facts::ModelFacts;
use crate::procs;

pub(super) use alab_wire::link::LATER_RETRY_MS;
pub(super) use alab_wire::{Relaunch, Verdict};

/// Total time one Claude crash or requested restart waits for the host's claim
/// or permission answer, independently of the shared link's per-request clock.
pub(super) const RECLAIM_HOLD_MS: u64 = 45_000;

fn config() -> Config {
    Config::new(
        "claude-code-alab",
        "claude",
        &[Scope::Relaunch, Scope::Upgrade],
        VERSION,
    )
    .expect("compiled Claude claim configuration")
    .with_contracts(&[Contract::Three, Contract::Four])
    .expect("the supported claim contracts")
    .with_c3_child_tracking()
}

fn metadata(vendor: Option<&str>, process: Option<ChildProcess>) -> Child<'_> {
    Child {
        vendor,
        process,
        ..Child::default()
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
                metadata(vendor, None),
                now_ms(),
            ),
            process: None,
            model: ModelDelivery::default(),
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
                metadata(vendor, None),
                now,
            ),
            process: None,
            model: ModelDelivery::default(),
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
            self.core.reannounce(metadata(vendor, None), now_ms());
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
        self.core.held_back(now)
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
        let child = metadata(self.child.build.version.as_deref(), self.link.process);
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
            metadata(self.child.build.version.as_deref(), self.link.process),
            now,
        );
        self.link.model.opening(self.link.core.counts().opened);
        for ack in &update.model_acks {
            self.link.model.ack(ack.seq, ack.dropped.as_ref(), now);
        }
        self.link.notes(&self.journal);
        if let Some(restart) = update.restart {
            if replaced {
                self.restart_ignored(&restart.cause, "child-replaced");
            } else {
                self.restart_asked(&restart);
            }
        }
        update.drained
    }

    pub(super) fn link_safe_point(&mut self) -> Verdict {
        let result = self
            .link
            .core
            .safe_point(Duration::from_millis(self.setup.opts.quiet_ms), now_ms());
        self.link.notes(&self.journal);
        result
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
            metadata(self.child.build.version.as_deref(), None),
            observed,
            now_ms(),
        );
        self.link.notes(&self.journal);
    }

    pub(super) fn link_release(&mut self) {
        self.link.release();
    }
}

#[cfg(test)]
#[path = "supervise_link_tests.rs"]
mod tests;
