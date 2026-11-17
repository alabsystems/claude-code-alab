// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Claude's abnormal-exit classification and crash back-off.
//!
//! The supervisor keeps its record-settle interval and crash ladder here. The
//! shared link judges fresh host policy and status for crash relaunches and
//! requested restarts ([`alab_wire::relaunch::permitted`]).
//!
//! The ladder follows aterm's `Relaunches` at `d29d3887c4d`: `BACKOFF`
//! (9752-9757), `HEALTHY` (9764), `forgive` (9925-9932), `pause` (9936-9943),
//! and the `Relaunched` and `NotYet` arms of `attempted_as` (9959-10000).
//! `EXIT_SETTLE` follows its record-settle interval (7270).

use std::time::Duration;

use crate::sys::ChildEvent;

/// How long after an exit its session record may still be on its way out. A
/// record that still stands this long after the child was reaped was left by a
/// crash (aterm `relaunch.rs:7270`, used at 7340).
pub const EXIT_SETTLE: Duration = Duration::from_millis(250);

/// The pauses before consecutive relaunches of a crash: 1 s, 10 s, 60 s, then
/// 10 minutes for every one after (aterm `relaunch.rs:9752-9757`).
pub const BACKOFF: [Duration; 4] = [
    Duration::from_secs(1),
    Duration::from_secs(10),
    Duration::from_secs(60),
    Duration::from_secs(600),
];

/// A child that ran this long since its last crash relaunch starts the back-off
/// over (aterm `relaunch.rs:9764`).
pub const HEALTHY: Duration = Duration::from_secs(600);

/// Whether the child's end is one a crash relaunch may answer: a non-zero exit
/// other than 129, 130, 131 and 143, or death by a signal other than `SIGINT`,
/// `SIGQUIT`, `SIGTERM` and `SIGHUP`.
///
/// Those four signals, and the exit codes a shell reports for them, are how a
/// person or the session ends a program; bringing it back would fight them. An
/// exit of 0 is the vendor leaving on its own. A stop or a continue is no end.
///
/// This is one of several conditions, never the whole decision: the session
/// record must also still stand [`EXIT_SETTLE`] later, the claim must be proven
/// with `relaunch`, and the shared link must permit the relaunch.
#[must_use]
pub const fn abnormal_exit(event: ChildEvent) -> bool {
    match event {
        ChildEvent::Exited(code) => code != 0 && !matches!(code, 129 | 130 | 131 | 143),
        ChildEvent::Signaled(signal) => !matches!(
            signal,
            libc::SIGINT | libc::SIGQUIT | libc::SIGTERM | libc::SIGHUP
        ),
        ChildEvent::Stopped(_) | ChildEvent::Continued => false,
    }
}

/// The crash back-off: how many relaunches of a crash stand unforgiven, and
/// when the last one was made.
///
/// Times are the supervisor's millisecond clock ([`crate::supervise::now_ms`]);
/// only differences are used, and a clock that steps back reads as no time
/// passed, which never forgives early.
///
/// A relaunch someone asked for (a `restart cause=` event)
/// is no crash and does not touch the ladder.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Ladder {
    streak: u32,
    last_ms: Option<u64>,
}

impl Ladder {
    /// A ladder with nothing on it: the first crash waits [`BACKOFF`]`[0]`.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            streak: 0,
            last_ms: None,
        }
    }

    /// The child crashed at `now_ms`: forgive the streak if it had run
    /// [`HEALTHY`] since the last crash relaunch, then return the pause before
    /// the next attempt (aterm `Relaunches::exited_as`, `forgive` and `pause`).
    pub fn crashed(&mut self, now_ms: u64) -> Duration {
        let healthy = u64::try_from(HEALTHY.as_millis()).unwrap_or(u64::MAX);
        if self
            .last_ms
            .is_some_and(|at| now_ms.saturating_sub(at) >= healthy)
        {
            self.streak = 0;
        }
        self.pause()
    }

    /// The pause the next attempt waits: [`BACKOFF`] at the streak, its last
    /// step for every streak past the table.
    #[must_use]
    pub fn pause(&self) -> Duration {
        let step = usize::try_from(self.streak)
            .unwrap_or(usize::MAX)
            .min(BACKOFF.len() - 1);
        BACKOFF[step]
    }

    /// A crash relaunch started the child at `now_ms`: the streak climbs and the
    /// healthy run that forgives it starts (aterm `attempted_as`, `Relaunched`).
    pub fn relaunched(&mut self, now_ms: u64) {
        self.streak = self.streak.saturating_add(1);
        self.last_ms = Some(now_ms);
    }

    /// A crash relaunch was tried and did not start the child: the streak
    /// climbs, and no healthy run starts (aterm `attempted_as`, `NotYet`).
    pub fn missed(&mut self) {
        self.streak = self.streak.saturating_add(1);
    }

    /// How many crash relaunches stand unforgiven.
    #[must_use]
    pub const fn streak(&self) -> u32 {
        self.streak
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_an_end_nobody_sent_is_abnormal() {
        for code in [1, 2, 126, 127, 128, 132, 134, 137, 139, 142, 144, 255, -1] {
            assert!(abnormal_exit(ChildEvent::Exited(code)), "exit {code}");
        }
        for code in [0, 129, 130, 131, 143] {
            assert!(!abnormal_exit(ChildEvent::Exited(code)), "exit {code}");
        }
        for signal in [
            libc::SIGKILL,
            libc::SIGSEGV,
            libc::SIGABRT,
            libc::SIGBUS,
            libc::SIGILL,
            libc::SIGFPE,
            libc::SIGPIPE,
            libc::SIGALRM,
            libc::SIGUSR1,
            libc::SIGUSR2,
        ] {
            assert!(
                abnormal_exit(ChildEvent::Signaled(signal)),
                "signal {signal}"
            );
        }
        for signal in [libc::SIGINT, libc::SIGQUIT, libc::SIGTERM, libc::SIGHUP] {
            assert!(
                !abnormal_exit(ChildEvent::Signaled(signal)),
                "signal {signal}"
            );
        }
        assert!(!abnormal_exit(ChildEvent::Stopped(libc::SIGTSTP)));
        assert!(!abnormal_exit(ChildEvent::Stopped(libc::SIGSEGV)));
        assert!(!abnormal_exit(ChildEvent::Continued));
    }

    #[test]
    fn the_ladder_climbs_one_ten_sixty_then_ten_minutes() {
        let mut ladder = Ladder::new();
        assert_eq!(ladder, Ladder::default());
        let mut now = 1_000_000;
        for (streak, seconds) in [(0, 1), (1, 10), (2, 60), (3, 600), (4, 600), (5, 600)] {
            assert_eq!(ladder.streak(), streak);
            let pause = ladder.crashed(now);
            assert_eq!(pause, Duration::from_secs(seconds), "streak {streak}");
            assert_eq!(pause, ladder.pause());
            now += u64::try_from(pause.as_millis()).unwrap();
            ladder.relaunched(now);
            // It crashes again one second into its run.
            now += 1_000;
        }
    }

    #[test]
    fn a_healthy_run_starts_the_ladder_over() {
        let mut ladder = Ladder::new();
        assert_eq!(ladder.crashed(0), Duration::from_secs(1));
        ladder.relaunched(1_000);
        assert_eq!(ladder.crashed(2_000), Duration::from_secs(10));
        ladder.relaunched(12_000);
        // One millisecond short of healthy: the streak stands.
        assert_eq!(ladder.crashed(12_000 + 599_999), Duration::from_secs(60));
        assert_eq!(ladder.streak(), 2);
        // Healthy, measured from the relaunch and not from the crash before it.
        assert_eq!(ladder.crashed(12_000 + 600_000), Duration::from_secs(1));
        assert_eq!(ladder.streak(), 0);
        // Forgiving is not forgetting the clock: the next relaunch starts a new run.
        ladder.relaunched(700_000);
        assert_eq!(ladder.crashed(700_500), Duration::from_secs(10));
    }

    #[test]
    fn a_miss_climbs_without_starting_a_healthy_run() {
        let mut ladder = Ladder::new();
        assert_eq!(ladder.crashed(0), Duration::from_secs(1));
        ladder.missed();
        assert_eq!(ladder.pause(), Duration::from_secs(10));
        // No relaunch ever landed, so no amount of time forgives the miss.
        assert_eq!(ladder.crashed(10_000_000), Duration::from_secs(10));
        ladder.relaunched(10_010_000);
        ladder.missed();
        assert_eq!(ladder.streak(), 3);
        // The healthy run is measured from the relaunch that landed.
        assert_eq!(
            ladder.crashed(10_010_000 + 599_999),
            Duration::from_secs(600)
        );
        assert_eq!(ladder.crashed(10_010_000 + 600_000), Duration::from_secs(1));
    }

    #[test]
    fn a_clock_that_steps_back_or_saturates_never_forgives_early() {
        let mut ladder = Ladder::new();
        ladder.relaunched(5_000_000);
        assert_eq!(ladder.crashed(1_000), Duration::from_secs(10));
        assert_eq!(ladder.streak(), 1);
        let mut high = Ladder::new();
        for _ in 0..10 {
            high.missed();
        }
        high.streak = u32::MAX;
        high.missed();
        high.relaunched(u64::MAX);
        assert_eq!(high.streak(), u32::MAX);
        assert_eq!(high.crashed(u64::MAX), Duration::from_secs(600));
    }

    #[test]
    fn the_constants_are_aterms() {
        assert_eq!(EXIT_SETTLE, Duration::from_millis(250));
        assert_eq!(HEALTHY, Duration::from_secs(600));
        assert_eq!(BACKOFF.map(|pause| pause.as_secs()), [1, 10, 60, 600]);
    }
}
