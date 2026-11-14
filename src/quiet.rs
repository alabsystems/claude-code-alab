// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The safe point: when the harness may end a running vendor child to move it
//! onto a newer build, and why not otherwise.
//!
//! The harness reads no screen itself. Without a proven aterm claim it decides
//! from two witnesses it can read on its own, and refuses whenever either is
//! missing or ambiguous. Under a proven claim aterm's `status` replaces the
//! second witness ([`crate::aterm::judge_safe_point`]) and the first is kept
//! ([`record_idle`]):
//!
//! 1. **Claude Code's own record** (`sessions/<pid>.json`): `status` is exactly
//!    `idle` and has been for the quiet period. aterm's upgrade gate requires the
//!    same (`upgrade.rs:3311-3352`, `gate`: `status == "idle"` and
//!    `status_age_s >= quiet_s()`, `QUIET_S = 20`).
//! 2. **The terminal's input clock** ([`crate::sys::stdin_input_ms`], the time a
//!    reader last took input from the tty): no input for the quiet period, and
//!    no input that could have left an unsent draft in the composer. aterm's
//!    gate reads the composer from the screen (`composer_empty`); this harness
//!    stands in for it with the rule [`Tracker`] keeps: the composer is known
//!    empty only when the child has read no input since it was spawned, or when
//!    the last input was the submission of the turn whose end produced the
//!    current idle period (read just before that turn's observed start) and
//!    nothing was read since, or when the idle period before that turn was
//!    already known empty and nothing at all was read since (a turn the vendor
//!    started on its own, such as a background task's notice, over an empty
//!    composer leaves it empty).
//!
//! Every other state is "wait". The harness never ends a busy child.

/// The quiet period: aterm's `upgrade::QUIET_S` (20 s, `upgrade.rs:3276`).
pub const QUIET_MS: u64 = 20_000;

/// How long before a turn's observed start its submitting input may be read:
/// Claude Code marks itself busy as it takes the submission, so a last input
/// older than this was not the submission (a turn the vendor started on its
/// own, such as a background-task notice, may sit on an unsent draft).
pub const SUBMIT_WINDOW_MS: u64 = 3_000;

/// An observation gap longer than this many poll intervals means the harness
/// may have missed a status change (it was stopped, or the machine slept), so
/// a turn start seen after it is not trusted.
pub const MAX_GAP_POLLS: u64 = 4;

/// One look at the child.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Look<'a> {
    /// Now, epoch milliseconds.
    pub now_ms: u64,
    /// The record's `status` and `statusUpdatedAt`, when a valid record of this
    /// child was read.
    pub status: Option<(&'a str, u64)>,
    /// The terminal's input clock, when the harness runs on a terminal.
    pub input_ms: Option<u64>,
}

/// Why the harness waits instead of ending the child now.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Wait {
    /// No terminal: no input clock, so no draft witness.
    NoTty,
    /// No valid record of this child.
    NoRecord,
    /// The record says a turn (or a box, or a shell) is running.
    Busy,
    /// Idle, but not yet for the quiet period.
    Settling,
    /// Input was read recently: a person may be typing.
    Typing,
    /// Input was read that may sit in the composer as an unsent draft.
    Draft,
}

impl Wait {
    /// One word for the journal.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Wait::NoTty => "no-tty",
            Wait::NoRecord => "no-record",
            Wait::Busy => "busy",
            Wait::Settling => "settling",
            Wait::Typing => "typing",
            Wait::Draft => "draft",
        }
    }
}

/// The record half of the safe point alone: `status` (the record's `status` and
/// `statusUpdatedAt`, when a valid record of this child was read) is exactly
/// `idle` and has been for `quiet_ms`. This is aterm's own upgrade gate, and it
/// is the gate the harness keeps when aterm's `status` stands in for the input
/// clock and the composer rule ([`Tracker`]): under a proven claim
/// ([`crate::aterm::judge_safe_point`]).
///
/// # Errors
/// [`Wait::NoRecord`], [`Wait::Busy`] or [`Wait::Settling`].
pub fn record_idle(status: Option<(&str, u64)>, now_ms: u64, quiet_ms: u64) -> Result<(), Wait> {
    let Some((status, status_at)) = status else {
        return Err(Wait::NoRecord);
    };
    if status != crate::profile::STATUS_IDLE {
        return Err(Wait::Busy);
    }
    if now_ms.saturating_sub(status_at) < quiet_ms {
        return Err(Wait::Settling);
    }
    Ok(())
}

/// The start of a non-idle run, as first seen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct RunStart {
    /// The record's `statusUpdatedAt` at the first non-idle look.
    status_at: u64,
    /// The input clock at that look; `None` when the start is not trusted.
    input_at: Option<u64>,
    /// The input clock value the idle period just before this run was known
    /// clean at, when nothing had been read since; `None` otherwise.
    clean_before: Option<u64>,
}

/// The lossless state of an observed run, without changing its input trust.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RunSnapshot {
    /// The first observed run's status clock.
    pub status_at: u64,
    /// Input at the run's first look, absent when the observation gap was too long.
    pub input_at: Option<u64>,
    /// The preceding clean clock, when it was unchanged at the run's start.
    pub clean_before: Option<u64>,
}

/// The safe-point history carried with the same child across an image replacement.
///
/// Times remain the original epoch milliseconds; restoring never refreshes an
/// observation or changes the child's spawn time.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Snapshot {
    /// The child's original spawn clock.
    pub spawned_ms: u64,
    /// The normalized polling interval.
    pub poll_ms: u64,
    /// The configured quiet period.
    pub quiet_ms: u64,
    /// The last observation, including any gap since it.
    pub last_look_ms: Option<u64>,
    /// A run whose end has not yet been observed.
    pub run: Option<RunSnapshot>,
    /// The input clock the current idle period is known clean at.
    pub clean_at: Option<u64>,
}

/// The per-child state the safe-point rule needs across looks.
#[derive(Clone, Debug)]
pub struct Tracker {
    spawned_ms: u64,
    poll_ms: u64,
    quiet_ms: u64,
    last_look_ms: Option<u64>,
    run: Option<RunStart>,
    /// The input clock value the current idle period is clean at.
    clean_at: Option<u64>,
}

impl Tracker {
    /// A tracker for a child spawned at `spawned_ms`, looked at every `poll_ms`,
    /// that must be quiet for `quiet_ms` ([`QUIET_MS`]).
    #[must_use]
    pub fn new(spawned_ms: u64, poll_ms: u64, quiet_ms: u64) -> Self {
        Self {
            spawned_ms,
            poll_ms: poll_ms.max(1),
            quiet_ms,
            last_look_ms: None,
            run: None,
            clean_at: None,
        }
    }

    /// Copy every observation and timing field without advancing the tracker.
    #[must_use]
    pub fn snapshot(&self) -> Snapshot {
        Snapshot {
            spawned_ms: self.spawned_ms,
            poll_ms: self.poll_ms,
            quiet_ms: self.quiet_ms,
            last_look_ms: self.last_look_ms,
            run: self.run.map(|run| RunSnapshot {
                status_at: run.status_at,
                input_at: run.input_at,
                clean_before: run.clean_before,
            }),
            clean_at: self.clean_at,
        }
    }

    /// Restore history with the expected spawn clock and settings.
    ///
    /// Returns `None` for different spawn/settings or an impossible combination
    /// of history fields. Wall clocks are not ordered here: clock corrections
    /// are possible, and the next normal look retains the existing gap rule.
    /// The caller must independently establish the child's kernel identity.
    #[must_use]
    pub fn restore(saved: Snapshot, spawned_ms: u64, poll_ms: u64, quiet_ms: u64) -> Option<Self> {
        if saved.spawned_ms != spawned_ms
            || saved.poll_ms != poll_ms.max(1)
            || saved.quiet_ms != quiet_ms
            || (saved.last_look_ms.is_none() && (saved.run.is_some() || saved.clean_at.is_some()))
        {
            return None;
        }
        if let Some(run) = saved.run
            && (saved.clean_at.is_some()
                || run
                    .clean_before
                    .is_some_and(|clean| run.input_at != Some(clean)))
        {
            return None;
        }
        Some(Self {
            spawned_ms: saved.spawned_ms,
            poll_ms: saved.poll_ms,
            quiet_ms: saved.quiet_ms,
            last_look_ms: saved.last_look_ms,
            run: saved.run.map(|run| RunStart {
                status_at: run.status_at,
                input_at: run.input_at,
                clean_before: run.clean_before,
            }),
            clean_at: saved.clean_at,
        })
    }

    /// Take one look; `Ok(())` when this is a safe point.
    ///
    /// # Errors
    /// [`Wait`] says why not.
    pub fn look(&mut self, look: &Look<'_>) -> Result<(), Wait> {
        let gap_ok = self.last_look_ms.is_some_and(|last| {
            look.now_ms.saturating_sub(last) <= MAX_GAP_POLLS.saturating_mul(self.poll_ms)
        });
        self.last_look_ms = Some(look.now_ms);
        let Some(input) = look.input_ms else {
            self.forget();
            return Err(Wait::NoTty);
        };
        let Some((status, status_at)) = look.status else {
            self.forget();
            return Err(Wait::NoRecord);
        };
        if status != crate::profile::STATUS_IDLE {
            if self.run.is_none() {
                // The first non-idle look of a run. Its start is trusted only when
                // the previous look (idle, or no record yet) was recent enough that
                // no earlier status change could hide between them.
                self.run = Some(RunStart {
                    status_at,
                    input_at: gap_ok.then_some(input),
                    clean_before: self.clean_at.filter(|&c| gap_ok && c == input),
                });
            }
            self.clean_at = None;
            return Err(Wait::Busy);
        }
        if let Some(run) = self.run.take() {
            // A run ended. Its idle period is clean when the input clock at the
            // run's start was the submission (read at or before the start, within
            // the window) and nothing has been read since; or when the composer
            // was known clean as the run started and nothing has been read since
            // (a run the vendor started over an empty composer).
            let submitted = run.input_at.filter(|&at| {
                at <= run.status_at
                    && run.status_at.saturating_sub(at) <= SUBMIT_WINDOW_MS
                    && input == at
            });
            let untouched = run
                .input_at
                .and(run.clean_before)
                .filter(|&clean| input == clean);
            self.clean_at = submitted.or(untouched);
        }
        if self.clean_at.is_none() && input <= self.spawned_ms {
            // The child has read no input at all: its composer was never typed in.
            self.clean_at = Some(input);
        }
        if self.clean_at != Some(input) {
            self.clean_at = None;
            return Err(if look.now_ms.saturating_sub(input) < self.quiet_ms {
                Wait::Typing
            } else {
                Wait::Draft
            });
        }
        if look.now_ms.saturating_sub(status_at) < self.quiet_ms {
            return Err(Wait::Settling);
        }
        if look.now_ms.saturating_sub(input) < self.quiet_ms {
            return Err(Wait::Typing);
        }
        Ok(())
    }

    fn forget(&mut self) {
        self.run = None;
        self.clean_at = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const POLL: u64 = 250;
    const Q: u64 = QUIET_MS;

    #[test]
    fn the_record_alone_is_idle_only_once_settled() {
        assert_eq!(record_idle(None, 5_000, 100), Err(Wait::NoRecord));
        assert_eq!(
            record_idle(Some(("busy", 1_000)), 5_000, 100),
            Err(Wait::Busy)
        );
        assert_eq!(
            record_idle(Some(("waiting", 1_000)), 5_000, 100),
            Err(Wait::Busy)
        );
        assert_eq!(
            record_idle(Some(("idle", 4_901)), 5_000, 100),
            Err(Wait::Settling)
        );
        assert_eq!(record_idle(Some(("idle", 4_900)), 5_000, 100), Ok(()));
        // A status clock ahead of this one has not settled.
        assert_eq!(
            record_idle(Some(("idle", 6_000)), 5_000, 100),
            Err(Wait::Settling)
        );
    }

    #[test]
    fn restored_history_keeps_the_submitted_turn_and_then_holds_new_input() {
        let mut original = Tracker::new(1_000, 50, 100);
        assert_eq!(
            original.look(&look(1_100, Some(("idle", 1_000)), 900)),
            Ok(())
        );
        assert_eq!(
            original.look(&look(1_150, Some(("busy", 1_140)), 1_140)),
            Err(Wait::Busy)
        );
        let saved = original.snapshot();
        let mut restored = Tracker::restore(saved.clone(), 1_000, 50, 100).expect("same child");
        assert_eq!(restored.snapshot(), saved);
        assert_eq!(
            restored.look(&look(1_400, Some(("idle", 1_300)), 1_140)),
            Ok(())
        );
        let clean = restored.snapshot();
        assert_eq!(clean.clean_at, Some(1_140));
        let mut restored = Tracker::restore(clean.clone(), 1_000, 50, 100).expect("clean history");
        assert_eq!(restored.snapshot(), clean);
        assert_eq!(
            restored.look(&look(1_500, Some(("idle", 1_300)), 1_450)),
            Err(Wait::Typing)
        );
        assert_eq!(
            restored.look(&look(1_600, Some(("idle", 1_300)), 1_450)),
            Err(Wait::Draft)
        );
    }

    #[test]
    fn restoration_does_not_hide_a_gap_or_discard_a_clean_run_start() {
        let mut original = Tracker::new(0, 50, 100);
        assert_eq!(
            original.look(&look(1_100, Some(("idle", 1_000)), 0)),
            Ok(())
        );
        assert_eq!(
            original.look(&look(1_150, Some(("busy", 1_140)), 1_140)),
            Err(Wait::Busy)
        );
        assert_eq!(
            original.look(&look(1_400, Some(("idle", 1_300)), 1_140)),
            Ok(())
        );
        assert_eq!(
            original.look(&look(1_450, Some(("busy", 1_440)), 1_140)),
            Err(Wait::Busy)
        );
        let saved = original.snapshot();
        assert_eq!(saved.run.and_then(|run| run.clean_before), Some(1_140));
        let mut restored = Tracker::restore(saved, 0, 50, 100).expect("pending run");
        assert_eq!(
            restored.look(&look(1_700, Some(("idle", 1_600)), 1_140)),
            Ok(())
        );

        // A subsequent turn after a long exec/stop gap cannot inherit clean input trust.
        let saved = restored.snapshot();
        let mut restored = Tracker::restore(saved, 0, 50, 100).expect("same history");
        assert_eq!(
            restored.look(&look(2_100, Some(("busy", 2_090)), 1_140)),
            Err(Wait::Busy)
        );
        assert_eq!(
            restored.look(&look(2_300, Some(("idle", 2_200)), 1_140)),
            Err(Wait::Draft)
        );
    }

    #[test]
    fn restore_refuses_other_settings_and_impossible_history() {
        let tracker = Tracker::new(1_000, 50, 100);
        let saved = tracker.snapshot();
        assert!(Tracker::restore(saved.clone(), 0, 50, 100).is_none());
        assert!(Tracker::restore(saved.clone(), 1_000, 51, 100).is_none());
        assert!(Tracker::restore(saved.clone(), 1_000, 50, 101).is_none());
        let mut invalid = saved;
        invalid.clean_at = Some(900);
        assert!(Tracker::restore(invalid.clone(), 1_000, 50, 100).is_none());
        invalid.last_look_ms = Some(1_100);
        invalid.run = Some(RunSnapshot {
            status_at: 1_090,
            input_at: Some(900),
            clean_before: None,
        });
        assert!(Tracker::restore(invalid.clone(), 1_000, 50, 100).is_none());
        invalid.clean_at = None;
        invalid.run.as_mut().expect("run").clean_before = Some(800);
        assert!(Tracker::restore(invalid, 1_000, 50, 100).is_none());
    }

    fn look(now: u64, status: Option<(&str, u64)>, input: u64) -> Look<'_> {
        Look {
            now_ms: now,
            status,
            input_ms: Some(input),
        }
    }

    /// Drive `t` with looks every POLL from `from` to `to` (exclusive) with fixed facts.
    fn hold(
        t: &mut Tracker,
        from: u64,
        to: u64,
        status: (&str, u64),
        input: u64,
    ) -> Result<(), Wait> {
        let mut last = Err(Wait::NoRecord);
        let mut now = from;
        while now < to {
            last = t.look(&look(now, Some(status), input));
            now += POLL;
        }
        last
    }

    #[test]
    fn a_child_that_never_read_input_is_safe_once_idle_is_settled() {
        let mut t = Tracker::new(10_000, POLL, Q);
        // Input clock older than the spawn: nothing was ever typed into this child.
        assert_eq!(t.look(&look(11_000, None, 9_000)), Err(Wait::NoRecord));
        assert_eq!(
            t.look(&look(11_250, Some(("idle", 11_100)), 9_000)),
            Err(Wait::Settling)
        );
        assert_eq!(
            hold(&mut t, 11_500, 11_100 + Q, ("idle", 11_100), 9_000),
            Err(Wait::Settling)
        );
        assert_eq!(
            t.look(&look(11_100 + Q, Some(("idle", 11_100)), 9_000)),
            Ok(())
        );
    }

    #[test]
    fn a_submitted_turn_that_ended_untouched_is_safe() {
        let mut t = Tracker::new(0, POLL, Q);
        // Idle with input at 5_000 (startup replies): not known clean.
        assert_eq!(
            hold(&mut t, 6_000, 50_000, ("idle", 1_000), 5_000),
            Err(Wait::Draft)
        );
        // The person types and submits at 50_000; Claude goes busy at 50_010.
        assert_eq!(
            t.look(&look(50_100, Some(("busy", 50_010)), 50_000)),
            Err(Wait::Busy)
        );
        assert_eq!(
            hold(&mut t, 50_350, 90_000, ("shell", 60_000), 50_000),
            Err(Wait::Busy)
        );
        // The turn ends at 90_000 and nothing is typed after.
        assert_eq!(
            t.look(&look(90_100, Some(("idle", 90_000)), 50_000)),
            Err(Wait::Settling)
        );
        assert_eq!(
            hold(&mut t, 90_350, 90_000 + Q, ("idle", 90_000), 50_000),
            Err(Wait::Settling)
        );
        assert_eq!(
            t.look(&look(90_000 + Q, Some(("idle", 90_000)), 50_000)),
            Ok(())
        );
    }

    #[test]
    fn input_during_the_turn_may_be_a_draft_and_holds() {
        let mut t = Tracker::new(0, POLL, Q);
        assert_eq!(hold(&mut t, 1_000, 50_000, ("idle", 500), 0), Ok(()));
        assert_eq!(
            t.look(&look(50_100, Some(("busy", 50_010)), 50_000)),
            Err(Wait::Busy)
        );
        // Typed while the turn ran, never sent.
        assert_eq!(
            hold(&mut t, 50_350, 90_000, ("busy", 50_010), 59_000),
            Err(Wait::Busy)
        );
        assert_eq!(
            hold(&mut t, 90_100, 200_000, ("idle", 90_000), 59_000),
            Err(Wait::Draft)
        );
    }

    #[test]
    fn input_typed_right_after_the_submission_holds() {
        let mut t = Tracker::new(0, POLL, Q);
        assert_eq!(hold(&mut t, 1_000, 50_000, ("idle", 500), 0), Ok(()));
        // The first busy look already shows input read after the turn began.
        assert_eq!(
            t.look(&look(50_200, Some(("busy", 50_010)), 50_100)),
            Err(Wait::Busy)
        );
        assert_eq!(
            hold(&mut t, 50_450, 90_000, ("busy", 50_010), 50_100),
            Err(Wait::Busy)
        );
        assert_eq!(
            hold(&mut t, 90_100, 200_000, ("idle", 90_000), 50_100),
            Err(Wait::Draft)
        );
    }

    #[test]
    fn input_during_idle_holds_until_a_clean_turn() {
        let mut t = Tracker::new(10_000, POLL, Q);
        assert_eq!(
            hold(&mut t, 11_000, 40_000, ("idle", 10_500), 9_000),
            Ok(())
        );
        // A keystroke while idle (a draft, a slash command, a focus report).
        assert_eq!(
            t.look(&look(40_000, Some(("idle", 10_500)), 39_900)),
            Err(Wait::Typing)
        );
        assert_eq!(
            hold(&mut t, 40_250, 200_000, ("idle", 10_500), 39_900),
            Err(Wait::Draft)
        );
        // A submitted turn that ends untouched makes it clean again.
        assert_eq!(
            t.look(&look(200_000, Some(("busy", 199_990)), 199_980)),
            Err(Wait::Busy)
        );
        assert_eq!(
            hold(&mut t, 200_250, 230_000 + Q, ("idle", 230_000), 199_980),
            Err(Wait::Settling)
        );
        assert_eq!(
            t.look(&look(230_000 + Q, Some(("idle", 230_000)), 199_980)),
            Ok(())
        );
    }

    #[test]
    fn a_turn_the_vendor_started_itself_does_not_clear_an_old_draft() {
        let mut t = Tracker::new(0, POLL, Q);
        assert_eq!(hold(&mut t, 1_000, 30_000, ("idle", 500), 0), Ok(()));
        // Typed at 30_000 (a draft), then the vendor starts a turn at 60_000 by itself.
        assert_eq!(
            hold(&mut t, 30_000, 60_000, ("idle", 500), 30_000),
            Err(Wait::Draft)
        );
        assert_eq!(
            t.look(&look(60_100, Some(("busy", 60_000)), 30_000)),
            Err(Wait::Busy)
        );
        assert_eq!(
            hold(&mut t, 60_350, 69_000, ("busy", 60_000), 30_000),
            Err(Wait::Busy)
        );
        assert_eq!(
            hold(&mut t, 69_100, 200_000, ("idle", 69_000), 30_000),
            Err(Wait::Draft)
        );
    }

    #[test]
    fn a_turn_the_vendor_started_over_a_clean_composer_stays_clean() {
        let mut t = Tracker::new(0, POLL, Q);
        // Idle, then a submitted turn (input read at 1_000) that ended untouched: clean.
        assert_eq!(
            hold(&mut t, 250, 1_100, ("idle", 100), 0),
            Err(Wait::Settling)
        );
        assert_eq!(
            t.look(&look(1_100, Some(("busy", 1_010)), 1_000)),
            Err(Wait::Busy)
        );
        assert_eq!(
            hold(&mut t, 1_350, 5_000 + Q, ("idle", 5_000), 1_000),
            Err(Wait::Settling)
        );
        assert_eq!(
            t.look(&look(5_000 + Q, Some(("idle", 5_000)), 1_000)),
            Ok(())
        );
        // A background task's notice starts a turn on its own; nothing is read.
        assert_eq!(
            t.look(&look(5_250 + Q, Some(("busy", 5_200 + Q)), 1_000)),
            Err(Wait::Busy)
        );
        let end = 40_000 + Q;
        assert_eq!(
            hold(&mut t, 5_500 + Q, end, ("busy", 5_200 + Q), 1_000),
            Err(Wait::Busy)
        );
        assert_eq!(
            hold(&mut t, end, end + Q, ("idle", end), 1_000),
            Err(Wait::Settling)
        );
        assert_eq!(t.look(&look(end + Q, Some(("idle", end)), 1_000)), Ok(()));
    }

    #[test]
    fn input_during_a_vendor_started_turn_still_holds() {
        let mut t = Tracker::new(0, POLL, Q);
        assert_eq!(hold(&mut t, 1_000, 30_000, ("idle", 500), 0), Ok(()));
        assert_eq!(
            t.look(&look(30_000, Some(("busy", 29_900)), 0)),
            Err(Wait::Busy)
        );
        // A keystroke is read while the vendor's own turn runs.
        assert_eq!(
            hold(&mut t, 30_250, 50_000, ("busy", 29_900), 40_000),
            Err(Wait::Busy)
        );
        assert_eq!(
            hold(&mut t, 50_100, 200_000, ("idle", 50_000), 40_000),
            Err(Wait::Draft)
        );
    }

    #[test]
    fn a_vendor_started_turn_seen_after_a_gap_does_not_keep_clean() {
        let mut t = Tracker::new(0, POLL, Q);
        // A submitted turn that ended untouched: clean at input 1_000.
        assert_eq!(
            hold(&mut t, 250, 1_100, ("idle", 100), 0),
            Err(Wait::Settling)
        );
        assert_eq!(
            t.look(&look(1_100, Some(("busy", 1_010)), 1_000)),
            Err(Wait::Busy)
        );
        assert_eq!(
            hold(&mut t, 1_350, 5_000 + Q + 250, ("idle", 5_000), 1_000),
            Ok(())
        );
        // The harness was stopped; a turn is first seen after the gap, so what
        // happened in between (a keystroke read and its echo cleared) is unknown.
        assert_eq!(
            t.look(&look(90_000, Some(("busy", 89_900)), 1_000)),
            Err(Wait::Busy)
        );
        assert_eq!(
            hold(&mut t, 90_250, 99_000, ("busy", 89_900), 1_000),
            Err(Wait::Busy)
        );
        assert_eq!(
            hold(&mut t, 99_100, 200_000, ("idle", 99_000), 1_000),
            Err(Wait::Draft)
        );
    }

    #[test]
    fn a_run_seen_after_a_gap_is_not_trusted() {
        let mut t = Tracker::new(0, POLL, Q);
        assert_eq!(hold(&mut t, 1_000, 30_000, ("idle", 500), 0), Ok(()));
        // The harness was stopped for half a minute; a status change may hide in the gap.
        assert_eq!(
            t.look(&look(61_000, Some(("shell", 60_900)), 60_000)),
            Err(Wait::Busy)
        );
        assert_eq!(
            hold(&mut t, 61_250, 69_000, ("shell", 60_900), 60_000),
            Err(Wait::Busy)
        );
        assert_eq!(
            hold(&mut t, 69_100, 200_000, ("idle", 69_000), 60_000),
            Err(Wait::Draft)
        );
    }

    #[test]
    fn the_same_run_without_the_gap_is_clean() {
        let mut t = Tracker::new(0, POLL, Q);
        assert_eq!(hold(&mut t, 1_000, 61_000, ("idle", 500), 0), Ok(()));
        assert_eq!(
            t.look(&look(61_000, Some(("shell", 60_900)), 60_000)),
            Err(Wait::Busy)
        );
        assert_eq!(
            hold(&mut t, 61_250, 69_000, ("shell", 60_900), 60_000),
            Err(Wait::Busy)
        );
        assert_eq!(
            hold(&mut t, 69_100, 69_000 + Q, ("idle", 69_000), 60_000),
            Err(Wait::Settling)
        );
        assert_eq!(
            t.look(&look(69_000 + Q, Some(("idle", 69_000)), 60_000)),
            Ok(())
        );
    }

    #[test]
    fn every_non_idle_status_is_busy() {
        for status in ["busy", "shell", "waiting", "question", ""] {
            let mut t = Tracker::new(10_000, POLL, Q);
            assert_eq!(
                t.look(&look(100_000, Some((status, 1)), 0)),
                Err(Wait::Busy),
                "{status:?}"
            );
        }
    }

    #[test]
    fn missing_witnesses_wait() {
        let mut t = Tracker::new(10_000, POLL, Q);
        assert_eq!(
            t.look(&Look {
                now_ms: 100_000,
                status: Some(("idle", 1)),
                input_ms: None
            }),
            Err(Wait::NoTty)
        );
        assert_eq!(t.look(&look(100_250, None, 0)), Err(Wait::NoRecord));
        // A record that disappears mid-run forgets the run.
        assert_eq!(
            t.look(&look(100_500, Some(("busy", 100_400)), 0)),
            Err(Wait::Busy)
        );
        assert_eq!(t.look(&look(100_750, None, 0)), Err(Wait::NoRecord));
    }
}
