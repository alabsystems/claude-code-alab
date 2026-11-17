// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Claude's reactor and journal adapter for the shared C3 link.
//!
//! The shared link owns claims, policy reads, events, child generations and
//! reconnects. This module supplies Claude's vendor metadata and already-owned
//! native observations, then drains its notes into the session journal. Input
//! decisions, the crash budget and replacing the vendor remain the supervisor's.

use std::os::fd::RawFd;
use std::time::Duration;

use alab_wire::{Child, Config, Contract, NativeChild, NativeObservation, Scope};

use super::{Supervisor, now_ms};
use crate::cli::VERSION;
use crate::journal::Journal;
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
    .with_contracts(&[Contract::Three])
    .expect("the supported C3 contract")
    .with_stream(false)
    .with_c3_child_tracking()
}

fn metadata(vendor: Option<&str>) -> Child<'_> {
    Child {
        vendor,
        ..Child::default()
    }
}

/// The supervisor's handle to the shared state machine. Hosted own-image
/// restoration starts absent until that adoption is independently qualified.
pub(super) struct Link {
    core: alab_wire::Link,
}

impl Link {
    pub(super) fn absent() -> Self {
        Self {
            core: alab_wire::Link::absent(config(), std::process::id()),
        }
    }

    pub(super) fn launch(journal: &Journal, vendor: Option<&str>) -> Self {
        if !super::image::hosted_by_aterm() {
            return Self::absent();
        }
        let mut link = Self {
            core: alab_wire::Link::launch(config(), std::process::id(), metadata(vendor), now_ms()),
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
                metadata(vendor),
                now,
            ),
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

    pub(super) fn child_ended(&mut self) {
        self.core.child_ended();
    }

    pub(super) fn event_fd(&self) -> Option<RawFd> {
        self.core.event_fd()
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
        self.link
            .core
            .tick_native_with(metadata(self.child.build.version.as_deref()), now, || {
                if no_child {
                    NativeObservation::Unknown
                } else {
                    observe(pid).map_or(NativeObservation::Unknown, NativeObservation::Present)
                }
            });
        self.link.notes(&self.journal);
    }

    pub(super) fn link_events(&mut self, replaced: bool) -> bool {
        let update = self
            .link
            .core
            .events(metadata(self.child.build.version.as_deref()), now_ms());
        self.link.notes(&self.journal);
        if let Some(restart) = update.restart {
            if replaced {
                self.restart_ignored(&restart.cause, "child-replaced");
            } else {
                self.restart_asked(&restart.cause);
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
        let observed = live_child(self.child.pid)
            .map_or(NativeObservation::Unknown, NativeObservation::Present);
        self.link.core.reannounce_native(
            metadata(self.child.build.version.as_deref()),
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
