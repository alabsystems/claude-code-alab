// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Whose exit it was, and how long to wait before bringing the child back.
//! The supervisor calls this module when the child ends abnormally under a
//! proven claim, and when aterm asks for a restart ([`crate::supervise`];
//! `docs/PLAN-stage-1-contract-3-client-2026-11-12.md`, section 3 and step 6
//! of its order).
//!
//! Under a proven contract-3 claim that takes `relaunch`, aterm's built-in
//! engine stands down from relaunching a crashed agent and the claim holder
//! does it instead. So the rule must be aterm's own, and this module is that
//! rule ported, with no I/O and no clock of its own. Line numbers are in
//! `aterm/crates/aterm-agent/src/harness/relaunch.rs` at `d29d3887c4d`:
//!
//! * [`OnExit`] and [`on_exit`]: `OnExit` (9553-9566) and `on_exit` (9570-9590),
//!   with `person_present` (9737-9739), `held_by_someone` (9594-9596) and
//!   `driver_hand` (9704-9713) read from a parsed [`Status`] instead of the
//!   reply's text;
//! * [`BACKOFF`], [`HEALTHY`] and [`Ladder`]: `BACKOFF` (9752-9757), `HEALTHY`
//!   (9764), and the crash streak of `Relaunches` (`forgive` 9925-9932, `pause`
//!   9936-9943, and the `Relaunched` and `NotYet` arms of `attempted_as`
//!   9959-10000);
//! * [`EXIT_SETTLE`]: `EXIT_SETTLE` (7270), how long a graceful exit may still
//!   be removing its own session record.
//!
//! Three things differ from aterm's `on_exit`, all on purpose and all stricter:
//!
//! * **Unread facts hold.** aterm relaunches when it could read no `status` at
//!   all, and treats an absent `hold=`, `hand=` or `human_ms=` as nobody there.
//!   Here [`on_exit`] needs a parsed status, and a fact it cannot read is
//!   [`OnExit::Held`].
//! * **Every lease is someone else's.** aterm's `driver_hand` exempts the
//!   deciding host's own `lease:aterm-harness@<pid>`. This harness never takes
//!   a lease, so any `hand=lease:…` is another driver's.
//! * **A halt or a driver holds an asked restart too.** aterm's `on_exit` skips
//!   the hold for an asked exit because aterm's restart act has already refused
//!   one under a halt or a driver's turn or lease
//!   (`aterm/crates/aterm-gui/src/input_stall.rs`, `other_hand`). For a holder
//!   whose proven claim takes `relaunch` the act skips that refusal and only
//!   sends `restart cause=` (`restart_act.rs`, `unavailable_for`), so the
//!   holder must refuse it. Only the person check is skipped for an asked exit.
//!
//! The policy's `relaunch` switch is passed as aterm's host passes it: an asked
//! exit is allowed whatever the switch says, so it limits crashes only
//! (`aterm/crates/aterm-gui/src/harness_host.rs`, `relaunches(job.agent) &&
//! (job.switches.relaunch() || asked.is_some())`). Nor does an asked exit
//! need `effective=on`: aterm's restart act sends `restart cause=` to a proven
//! `relaunch` holder without reading the policy (`restart_act.rs`,
//! `unavailable_for`) and has already told its asker the restart is under
//! way, and its own host decides an asked exit by the same rule whatever the
//! session's Autopilot says.
//!
//! The exit itself is judged by [`abnormal_exit`], which is this harness's own
//! addition: aterm sees no exit status in a tab a harness leads, only the
//! session record, and the plan requires both.
//!
//! [`permitted`] is the whole word of one fresh policy and status pair: the
//! claim, the policy and [`on_exit`] together.

use std::time::Duration;

use crate::aterm::{Human, Policy, Scope, Status, journal_word};
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

/// Whose exit it was, and so what is done about it (aterm `relaunch.rs:9553-9566`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OnExit {
    /// A person typed into the session within the grace before the exit: it is
    /// theirs, and nothing comes back.
    PersonAsked,
    /// The session is held: halted (`hold=1`), under a drive lease, or in a turn
    /// a named driver typed. Also every exit whose status could not say.
    Held,
    /// Nobody owns the exit, but `[harness] relaunch = false`.
    Limited,
    /// Nobody asked for the exit: relaunch.
    Relaunch,
    /// An explicit restart ended it (`aterm ctl restart`, or the stall's
    /// remedy): relaunch, and it is no crash.
    Asked,
    /// The harness's own restart ended the child and its relaunch is already in
    /// flight.
    Restarted,
}

/// Whether a `hand=` value names another driver: any drive lease, or a turn a
/// named driver typed (`turn:<id>:<holder>`). A turn with no named driver
/// (`turn:<id>`) and `-` are nobody (aterm `relaunch.rs:9704-9713`).
fn driver_hand(hand: &str) -> bool {
    hand.starts_with("lease:")
        || hand
            .strip_prefix("turn:")
            .is_some_and(|turn| turn.contains(':'))
}

/// Decide whose exit this was, in aterm's order (`relaunch.rs:9570-9590`):
///
/// 1. `restarted`: the harness's own restart is in flight ([`OnExit::Restarted`]);
/// 2. not `asked`, and a person typed within `grace` ([`OnExit::PersonAsked`]);
/// 3. the session is halted or driven, or cannot say ([`OnExit::Held`]);
/// 4. not `allowed` ([`OnExit::Limited`]);
/// 5. `asked` ([`OnExit::Asked`]);
/// 6. otherwise [`OnExit::Relaunch`].
///
/// `allowed` is what aterm's host passes: the policy's `relaunch` switch from a
/// read made after the exit, or'd with `asked` ([`permitted`]). `status` is the
/// `status` reply of that same read; `grace` is the policy's `human_grace_s`.
/// `asked` says an explicit restart named this child. An asked exit skips the
/// person check (whoever asked is the one at the keys) and is still held by a
/// halt or a driver's hand, which aterm's own restart act refuses and leaves a
/// `relaunch` claim holder to refuse. A `human_ms=` that cannot be read holds a
/// crash only: an asked exit does not need it.
///
/// A typed key exactly `grace` old is outside the grace, as in aterm
/// (`HumanInput::within`, `supervise/screen.rs:114-120`).
#[must_use]
pub fn on_exit(
    allowed: bool,
    status: &Status,
    grace: Duration,
    asked: bool,
    restarted: bool,
) -> OnExit {
    if restarted {
        return OnExit::Restarted;
    }
    if !asked && matches!(status.human, Human::Ago(ago) if ago < grace) {
        return OnExit::PersonAsked;
    }
    let free = (asked || status.human != Human::Unknown)
        && status.hold.as_deref() == Some("0")
        && status
            .hand
            .as_deref()
            .is_some_and(|hand| !driver_hand(hand));
    if !free {
        return OnExit::Held;
    }
    if !allowed {
        OnExit::Limited
    } else if asked {
        OnExit::Asked
    } else {
        OnExit::Relaunch
    }
}

/// Whether one fresh `harness policy` and `status` pair lets the claim `holder`
/// start its child again: after a crash (`asked` false), or because aterm asked
/// for a restart (`asked` true).
///
/// `Ok` only when all of these read so, checked in this order; otherwise the
/// first that does not is the error, as journal words:
///
/// 1. `supervisor=<holder>` and `supervisor_proven=1`: the standing claim is
///    this harness's and aterm proved it (`claim supervisor=<word>`, `claim
///    supervisor_proven=0`);
/// 2. `supervisor_scope` names `relaunch`: the claim took this duty (`claim
///    supervisor_scope=<words>`);
/// 3. `contract=` serves 3, and for a crash `effective=on`: the policy still
///    stands (`policy contract=<set>`, `policy effective=off why=<word>`);
/// 4. [`on_exit`], with the policy's `relaunch` switch (or'd with `asked`, as
///    aterm's host passes it) and `human_grace_s`, says [`OnExit::Relaunch`] or
///    [`OnExit::Asked`]. A person at the keys is `person human_ms=<ms>` (a
///    crash only), a hold or an unread fact is `held hold=<word> hand=<word>
///    human_ms=<word>`, and the switch is `relaunch=false` (a crash only).
///
/// Without the claim the relaunch is aterm's own engine's to make, so this
/// harness makes none: two programs would otherwise bring the same child back.
///
/// # Errors
/// The first condition that does not hold, as journal words.
pub fn permitted(
    policy: &Policy,
    status: &Status,
    holder: &str,
    asked: bool,
) -> Result<(), String> {
    if status.supervisor.as_deref() != Some(holder) {
        return Err(format!(
            "claim supervisor={}",
            status
                .supervisor
                .as_deref()
                .map_or_else(|| "-".to_string(), journal_word)
        ));
    }
    if !status.supervisor_proven {
        return Err("claim supervisor_proven=0".to_string());
    }
    if !status.takes(Scope::Relaunch) {
        let scope = if status.supervisor_scope.is_empty() {
            "-".to_string()
        } else {
            journal_word(&status.supervisor_scope.join(","))
        };
        return Err(format!("claim supervisor_scope={scope}"));
    }
    if !policy.serves_contract() {
        let contracts = policy
            .contracts
            .iter()
            .map(u32::to_string)
            .collect::<Vec<_>>()
            .join(",");
        return Err(format!("policy contract={contracts}"));
    }
    if !asked && !policy.effective {
        return Err(format!("policy {}", policy.off_words()));
    }
    let human = match status.human {
        Human::Never => "-".to_string(),
        Human::Ago(ago) => ago.as_millis().to_string(),
        Human::Unknown => "unknown".to_string(),
    };
    match on_exit(
        policy.relaunch || asked,
        status,
        policy.human_grace,
        asked,
        false,
    ) {
        OnExit::Relaunch | OnExit::Asked => Ok(()),
        OnExit::PersonAsked => Err(format!("person human_ms={human}")),
        OnExit::Held => {
            let word =
                |value: Option<&str>| value.map_or_else(|| "absent".to_string(), journal_word);
            Err(format!(
                "held hold={} hand={} human_ms={human}",
                word(status.hold.as_deref()),
                word(status.hand.as_deref())
            ))
        }
        OnExit::Limited => Err("relaunch=false".to_string()),
        // Never returned for `restarted = false`; were it, nothing is started twice.
        OnExit::Restarted => Err("restart-in-flight".to_string()),
    }
}

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
/// with `relaunch`, and [`on_exit`] must say [`OnExit::Relaunch`].
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
/// A relaunch someone asked for ([`OnExit::Asked`], a `restart cause=` event)
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

    const GRACE: Duration = Duration::from_secs(120);

    fn status(fields: &str) -> Status {
        Status::parse(&format!("OK schema=1 {fields}")).expect("status")
    }

    fn quiet() -> Status {
        status("hold=0 hand=- human_ms=-")
    }

    #[test]
    fn a_crash_nobody_owns_is_relaunched() {
        assert_eq!(
            on_exit(true, &quiet(), GRACE, false, false),
            OnExit::Relaunch
        );
        for human in ["120000", "120001", "86400000"] {
            let status = status(&format!("hold=0 hand=- human_ms={human}"));
            assert_eq!(
                on_exit(true, &status, GRACE, false, false),
                OnExit::Relaunch,
                "{human}"
            );
        }
        // A turn with no named driver is nobody's hand.
        let status = status("hold=0 hand=turn:7 human_ms=-");
        assert_eq!(
            on_exit(true, &status, GRACE, false, false),
            OnExit::Relaunch
        );
    }

    #[test]
    fn the_order_is_aterms() {
        let typed = status("hold=1 hand=lease:x human_ms=5");
        let held = status("hold=1 hand=- human_ms=-");
        // 1. The harness's own restart outranks everything.
        for (allowed, asked) in [(true, true), (true, false), (false, true), (false, false)] {
            assert_eq!(
                on_exit(allowed, &typed, GRACE, asked, true),
                OnExit::Restarted
            );
        }
        // 2. A person at the keys outranks a hold and the switch.
        assert_eq!(
            on_exit(true, &typed, GRACE, false, false),
            OnExit::PersonAsked
        );
        assert_eq!(
            on_exit(false, &typed, GRACE, false, false),
            OnExit::PersonAsked
        );
        // 3. A hold outranks the switch.
        assert_eq!(on_exit(true, &held, GRACE, false, false), OnExit::Held);
        assert_eq!(on_exit(false, &held, GRACE, false, false), OnExit::Held);
        // 4. `allowed` outranks an explicit restart; [`permitted`] passes the
        // switch or'd with `asked`, as aterm's host does.
        assert_eq!(
            on_exit(false, &quiet(), GRACE, false, false),
            OnExit::Limited
        );
        assert_eq!(
            on_exit(false, &quiet(), GRACE, true, false),
            OnExit::Limited
        );
        // 5. An asked exit skips the person, and nothing else: a halt or a
        // driver still holds it, as aterm's restart act refuses them.
        let person = status("hold=0 hand=- human_ms=5");
        assert_eq!(on_exit(true, &person, GRACE, true, false), OnExit::Asked);
        assert_eq!(on_exit(true, &typed, GRACE, true, false), OnExit::Held);
        assert_eq!(on_exit(true, &held, GRACE, true, false), OnExit::Held);
        assert_eq!(on_exit(false, &typed, GRACE, true, false), OnExit::Held);
        // An unread `human_ms=` holds a crash only.
        let unread = status("hold=0 hand=-");
        assert_eq!(on_exit(true, &unread, GRACE, false, false), OnExit::Held);
        assert_eq!(on_exit(true, &unread, GRACE, true, false), OnExit::Asked);
    }

    #[test]
    fn a_person_is_present_strictly_inside_the_grace() {
        for (human, expected) in [
            ("0", OnExit::PersonAsked),
            ("119999", OnExit::PersonAsked),
            ("120000", OnExit::Relaunch),
            ("-", OnExit::Relaunch),
        ] {
            let status = status(&format!("hold=0 hand=- human_ms={human}"));
            assert_eq!(
                on_exit(true, &status, GRACE, false, false),
                expected,
                "{human}"
            );
        }
        let now = status("hold=0 hand=- human_ms=0");
        assert_eq!(
            on_exit(true, &now, Duration::ZERO, false, false),
            OnExit::Relaunch
        );
    }

    #[test]
    fn a_hold_a_lease_or_a_driven_turn_holds() {
        for fields in [
            "hold=1 hand=- human_ms=-",
            "hold=0 hand=lease:someone human_ms=-",
            "hold=0 hand=lease:aterm-harness@40450 human_ms=-",
            "hold=0 hand=turn:7:s-12 human_ms=-",
            "hold=0 hand=turn:7:someone%20else human_ms=200000",
        ] {
            assert_eq!(
                on_exit(true, &status(fields), GRACE, false, false),
                OnExit::Held,
                "{fields}"
            );
        }
    }

    #[test]
    fn a_fact_the_status_does_not_state_holds() {
        for fields in [
            "",
            "hand=- human_ms=-",
            "hold=0 human_ms=-",
            "hold=0 hand=-",
            "hold=0 hand=- human_ms=soon",
            "hold=no hand=- human_ms=-",
            "hold= hand=- human_ms=-",
        ] {
            let status = status(fields);
            assert_eq!(
                on_exit(true, &status, GRACE, false, false),
                OnExit::Held,
                "{fields:?}"
            );
            assert_eq!(
                on_exit(false, &status, GRACE, false, false),
                OnExit::Held,
                "{fields:?}"
            );
        }
    }

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

    fn policy(fields: &str) -> Policy {
        Policy::parse(&format!("OK gen=1 {fields}")).expect("policy")
    }

    const POLICY_ON: &str = "effective=on contract=2,3 relaunch=true upgrade=true \
                             upgrade_ask=false human_grace_s=120";
    const HOLDER: &str = "claude-code-alab@77";
    const CLAIM: &str = "supervisor=claude-code-alab@77 supervisor_proven=1 \
                         supervisor_scope=relaunch,upgrade";

    #[test]
    fn a_relaunch_is_permitted_only_under_this_harnesss_proven_relaunch_claim() {
        let on = policy(POLICY_ON);
        let quiet = status(&format!("hold=0 hand=- human_ms=- {CLAIM}"));
        assert_eq!(permitted(&on, &quiet, HOLDER, false), Ok(()));
        assert_eq!(permitted(&on, &quiet, HOLDER, true), Ok(()));
        for (claim, why) in [
            ("", "claim supervisor=-"),
            ("supervisor=- supervisor_proven=0", "claim supervisor=-"),
            (
                "supervisor=someone-else@9 supervisor_proven=1 supervisor_scope=relaunch,upgrade",
                "claim supervisor=someone-else@9",
            ),
            (
                "supervisor=claude-code-alab@78 supervisor_proven=1 supervisor_scope=relaunch",
                "claim supervisor=claude-code-alab@78",
            ),
            (
                "supervisor=claude-code-alab@77 supervisor_proven=0 supervisor_scope=relaunch,upgrade",
                "claim supervisor_proven=0",
            ),
            (
                "supervisor=claude-code-alab@77 supervisor_scope=relaunch,upgrade",
                "claim supervisor_proven=0",
            ),
            (
                "supervisor=claude-code-alab@77 supervisor_proven=1 supervisor_scope=upgrade",
                "claim supervisor_scope=upgrade",
            ),
            (
                "supervisor=claude-code-alab@77 supervisor_proven=1 supervisor_scope=-",
                "claim supervisor_scope=-",
            ),
            (
                "supervisor=claude-code-alab@77 supervisor_proven=1",
                "claim supervisor_scope=-",
            ),
        ] {
            let status = status(&format!("hold=0 hand=- human_ms=- {claim}"));
            for asked in [false, true] {
                assert_eq!(
                    permitted(&on, &status, HOLDER, asked),
                    Err(why.to_string()),
                    "{claim:?} asked={asked}"
                );
            }
        }
    }

    #[test]
    fn a_relaunch_follows_the_policy_and_whose_exit_it_was() {
        let quiet = status(&format!("hold=0 hand=- human_ms=- {CLAIM}"));
        let older = policy(&POLICY_ON.replace("contract=2,3", "contract=2"));
        for asked in [false, true] {
            assert_eq!(
                permitted(&older, &quiet, HOLDER, asked),
                Err("policy contract=2".to_string()),
                "asked={asked}"
            );
        }
        // A policy that is off holds a crash; aterm sent an asked restart to
        // this claim without reading it, and makes one itself whatever the
        // session's Autopilot says.
        for off in ["effective=off", "effective=off why=autopilot-off"] {
            let off_policy = policy(&POLICY_ON.replace("effective=on", off));
            assert_eq!(
                permitted(&off_policy, &quiet, HOLDER, false),
                Err(format!("policy {off}"))
            );
            assert_eq!(permitted(&off_policy, &quiet, HOLDER, true), Ok(()));
        }
        // The switch limits crashes only: aterm relaunches an asked exit
        // whatever it says.
        let limited = policy(&POLICY_ON.replace("relaunch=true", "relaunch=false"));
        assert_eq!(
            permitted(&limited, &quiet, HOLDER, false),
            Err("relaunch=false".to_string())
        );
        assert_eq!(permitted(&limited, &quiet, HOLDER, true), Ok(()));
        let on = policy(POLICY_ON);
        // A crash is a person's or a holder's when aterm says so. A restart
        // someone asked for is the asking person's, and still a holder's or a
        // driver's: the third word is what refuses it, when anything does.
        for (fields, why, asked_why) in [
            (
                "hold=0 hand=- human_ms=119999",
                "person human_ms=119999",
                None,
            ),
            (
                "hold=1 hand=- human_ms=-",
                "held hold=1 hand=- human_ms=-",
                Some("held hold=1 hand=- human_ms=-"),
            ),
            (
                "hold=0 hand=lease:driver@4 human_ms=120000",
                "held hold=0 hand=lease:driver@4 human_ms=120000",
                Some("held hold=0 hand=lease:driver@4 human_ms=120000"),
            ),
            (
                "hold=0 hand=turn:7:s-12 human_ms=-",
                "held hold=0 hand=turn:7:s-12 human_ms=-",
                Some("held hold=0 hand=turn:7:s-12 human_ms=-"),
            ),
            (
                "hand=- human_ms=-",
                "held hold=absent hand=- human_ms=-",
                Some("held hold=absent hand=- human_ms=-"),
            ),
            (
                "hold=0 human_ms=-",
                "held hold=0 hand=absent human_ms=-",
                Some("held hold=0 hand=absent human_ms=-"),
            ),
            ("hold=0 hand=-", "held hold=0 hand=- human_ms=unknown", None),
        ] {
            let status = status(&format!("{fields} {CLAIM}"));
            assert_eq!(
                permitted(&on, &status, HOLDER, false),
                Err(why.to_string()),
                "{fields}"
            );
            assert_eq!(
                permitted(&on, &status, HOLDER, true),
                asked_why.map_or(Ok(()), |why| Err(why.to_string())),
                "{fields}"
            );
        }
        // The policy's own grace is the grace: a key exactly that old is outside it.
        let status = status(&format!("hold=0 hand=- human_ms=120000 {CLAIM}"));
        assert_eq!(permitted(&on, &status, HOLDER, false), Ok(()));
    }

    #[test]
    fn the_constants_are_aterms() {
        assert_eq!(EXIT_SETTLE, Duration::from_millis(250));
        assert_eq!(HEALTHY, Duration::from_secs(600));
        assert_eq!(BACKOFF.map(|pause| pause.as_secs()), [1, 10, 60, 600]);
    }
}
