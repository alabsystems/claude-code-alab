// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! THE WALLS DUTY: what this harness does when its own Claude Code session's
//! turn ended on a wall (a usage limit, an outage or another API error, a full
//! context, a lost login, the critical-memory banner), decided from one
//! certain read of the screen.
//!
//! The claim takes `walls` ([`crate::aterm::Duties`]), so this harness
//! carries its own session's walls under the tab's Autopilot word, by the
//! rules aterm carried them by (`aterm/crates/aterm-agent/src/supervise/
//! policy/turn_end.rs`, `wall_own_act`, ported here, narrower where this
//! harness lacks one of aterm's witnesses). Nothing else carries them: a wall
//! no claim takes is the person's. The wall dialogs (a usage limit's options,
//! the autocompact offer, a model switch's confirmation) are boxes, answered
//! by [`crate::boxes`] under the same duty; this module decides the walls the
//! screen shows as the turn's last words.
//!
//! **What it does**, each act typed under the duty aterm books it by
//! (`aterm/crates/aterm-agent/src/harness/acts.rs`, `act_scope`):
//!
//! * a usage limit (the session's five hours, the week's, a model's own
//!   bucket, spend) is waited out without typing, and the session is resumed
//!   once it has reset (`usage-resume@v1`, a `turn-end` act): at the reset
//!   the conversation's transcript names ([`Limit::reset_at_ms`]) plus
//!   [`TurnEndTiming::reset_grace`] and a jitter of at most
//!   [`TurnEndTiming::reset_jitter`]; where Claude Code's own auto-continue
//!   is armed ([`goes_on_by_itself`]), only [`TurnEndTiming::auto_resume_grace`]
//!   after that reset, so the vendor's own continuation goes first; where no
//!   reset is known, on [`TurnEndTiming::limit_backoff`]'s last rung, or
//!   [`TurnEndTiming::entitlement_backoff`] for spend and a model's bucket. A
//!   resume met by the limit again climbs the ladder;
//! * an outage (an overload, a server error, an unreachable route) is tried
//!   blind (`outage-resume@v1`), at [`TurnEndTiming::outage_first`] from the
//!   wall, doubling to [`TurnEndTiming::outage_cap`], or first at the
//!   `retry-after` a rate limit's words name ([`outage_gap`]);
//! * a refused request is tried again on [`TurnEndTiming::refused_backoff`]
//!   (`api-retry@v1`); a reply cut off, or over its output maximum, at once
//!   and then on [`TurnEndTiming::net_backoff`] or
//!   [`TurnEndTiming::retry_backoff`] (`api-cutoff@v1`);
//! * a full context gets `/compact` (`context-compact@v1`), again on
//!   [`TurnEndTiming::retry_backoff`] if the context is still full;
//! * the critical-memory banner restarts the child on its conversation
//!   (`memory-restart@v1`, a `relaunch` act).
//!
//! **What it never does**, each a rule the tests hold with a negative control:
//!
//! * type `/login`, `/model`, `/rate-limit-options`, or anything that buys,
//!   claims a reset or consents to usage credits: a lost login is the
//!   person's (its sign-in opens their browser), and a limit only usage
//!   credits lift is left while `usage_credits` is off;
//! * act on a usage limit the harness moves the session off
//!   ([`Limit::moving`], [`crate::limit`]), or on the old limit of a session
//!   moved off it, whose resume the turn-end duty owes
//!   ([`Limit::owed_resume`]);
//! * act on a screen it could not read with certainty, or under a box;
//! * act twice on a screen that did not change: after an act nothing more is
//!   typed until the screen moved, but for this harness's own Enter, pressed
//!   once, where its own text stands unsent [`TurnEndTiming::take_within`]
//!   after it was typed ([`Watch::decide`]).
//!
//! The person's grace, the composer and aterm's `if-human=` fence are the
//! typing client's to judge at the write ([`crate::typing::Typist::write`]).
//! Nothing here performs I/O; the supervisor's driver
//! (`src/supervise_wall_duty.rs`) reads, writes, restarts and journals.

use std::time::Duration;

use alab_policy::config::TurnEndTiming;
use alab_policy::ladder::{Ladder, doubling};
use alab_policy::vocab::{ApiCause, Phase, WallKind};
use aterm_phase::{AutoContinue, Program, Reading, Wall};

use crate::aterm::Scope;
use crate::aterm_wire::Fields;
use crate::duty::own_draft;

/// aterm's rule id for the resume past a usage limit's reset.
pub const RULE_LIMIT_RESUME: &str = "usage-resume@v1";
/// aterm's rule id for a refused request tried again.
pub const RULE_API_RETRY: &str = "api-retry@v1";
/// aterm's rule id for a reply cut off, or over its output maximum, tried
/// again.
pub const RULE_API_CUTOFF: &str = "api-cutoff@v1";
/// aterm's rule id for an outage's blind try.
pub const RULE_OUTAGE_RESUME: &str = "outage-resume@v1";
/// aterm's rule id for `/compact` at a full context.
pub const RULE_COMPACT: &str = "context-compact@v1";
/// aterm's rule id for the restart the critical-memory banner calls for.
pub const RULE_MEMORY_RESTART: &str = "memory-restart@v1";

/// The cause the harness's own restart at the critical-memory banner names,
/// in the journal and on the terminal.
pub const CAUSE_MEMORY: &str = "memory";

/// What a full context gets typed.
pub const COMPACT: &str = "/compact";

/// What follows each API wall's report: aterm's `CARRY_ON`.
const CARRY_ON: &str = concat!(
    "Carry on from where you stopped; if the result of your last step is missing, check \
     whether it ran before you repeat it. ",
    alab_policy::needs_hint!()
);

/// The policy's switches for walls, from the `harness policy` reply
/// ([`WallPolicy::of_policy`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct WallPolicy {
    /// `retry_api_errors=true`: an API wall, an outage included, is tried
    /// again.
    pub retry_api_errors: bool,
    /// `resume_limits=true`: a usage limit is resumed once it has reset.
    pub resume_limits: bool,
    /// `usage_credits=true`: the owner lets a limit be passed on usage
    /// credits. Nothing here spends them; with it off, a limit only credits
    /// lift is left to the person.
    pub usage_credits: bool,
    /// `compact_on_context_wall=true`: a full context gets `/compact`.
    pub compact_on_context_wall: bool,
}

impl WallPolicy {
    /// The switches the policy reply `reply` states
    /// (`aterm/crates/aterm-gui/src/harness_policy_verb.rs`, `reply_serving`,
    /// from `SupervisorConfig::texts`). A switch the reply does not state as
    /// `true`, and a reply that is not one, is off: an older or other aterm
    /// never widens what this harness types.
    #[must_use]
    pub fn of_policy(reply: &str) -> Self {
        let Ok(fields) = Fields::parse_lenient(reply) else {
            return Self::default();
        };
        let on = |key: &str| fields.get(key) == Some("true");
        Self {
            retry_api_errors: on("retry_api_errors"),
            resume_limits: on("resume_limits"),
            usage_credits: on("usage_credits"),
            compact_on_context_wall: on("compact_on_context_wall"),
        }
    }
}

/// What the harness knows of a usage limit beyond the screen.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Limit {
    /// When the limit the child stands at resets (epoch ms), as the
    /// conversation's transcript names it (`quotaLimits.resetsAt`,
    /// [`crate::limit::Wall::resets_at_s`]); `None` where no row names one.
    pub reset_at_ms: Option<u64>,
    /// The harness moves the session off Fable's limit
    /// ([`crate::limit::owes_move`]): the limit is the move's, not this
    /// duty's.
    pub moving: bool,
    /// The session was moved off a usage limit, the turn-end duty owes it
    /// its resume ([`crate::duty::Carried::owed_resume`]), and the transcript
    /// names no limit since the child started: a usage wall on the screen is
    /// the old one, which the resume answers. A limit of the model the
    /// session now runs is this duty's, as any other.
    pub owed_resume: bool,
}

/// What one look sees.
#[derive(Clone, Copy, Debug)]
pub struct Look<'a> {
    /// Claude Code's screen, as aterm-phase reads it.
    pub reading: &'a Reading,
    /// The screen's rows.
    pub rows: &'a [String],
    /// The screen's content sequence.
    pub seq: u64,
    /// Now, epoch ms.
    pub now_ms: u64,
    /// What the harness knows of a usage limit.
    pub limit: Limit,
    /// `relaunch=true` in the policy: the child may be brought back.
    pub relaunch: bool,
    /// What spreads this session's resume past a reset from other sessions'
    /// ([`TurnEndTiming::reset_jitter`]): any number fixed for the session.
    pub seed: u64,
}

/// What the duty does at one look.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Decision {
    /// Not this duty's read: no wall, a box, a busy turn, a screen not read
    /// with certainty, or a wall another act answers. The turn-end decider
    /// has it.
    Pass(&'static str),
    /// A wall stands and nothing is done about it now, for the reason named.
    Nothing(&'static str),
    /// The wall is the person's, for this reason.
    Leave(String),
    /// Nothing until `until_ms`, for the reason named.
    Wait {
        /// When to look again, epoch ms.
        until_ms: u64,
        /// Why, as journal words.
        why: String,
    },
    /// Type `text` under the duty `scope` by `rule`, and submit it.
    Type {
        /// The duty aterm books the act under.
        scope: Scope,
        /// The rule.
        rule: &'static str,
        /// What is typed.
        text: String,
    },
    /// Press Enter on this duty's own text, which stands unsent in the
    /// composer.
    Submit {
        /// The duty the text was typed under.
        scope: Scope,
        /// Its rule.
        rule: &'static str,
    },
    /// Restart the child on its conversation, under `relaunch`.
    Restart {
        /// The rule.
        rule: &'static str,
    },
}

impl Decision {
    /// The decision as journal words: `nothing <why>`, `left <why>`, `wait
    /// <why> until=<ms>`, `type rule=<id>`, `enter rule=<id>`, `restart
    /// rule=<id>`; a pass is `pass <why>`, which the driver does not journal.
    #[must_use]
    pub fn words(&self) -> String {
        match self {
            Self::Pass(why) => format!("pass {why}"),
            Self::Nothing(why) => format!("nothing {why}"),
            Self::Leave(why) => format!("left {why}"),
            Self::Wait { until_ms, why } => format!("wait {why} until={until_ms}"),
            Self::Type { rule, .. } => format!("type rule={rule}"),
            Self::Submit { rule, .. } => format!("enter rule={rule}"),
            Self::Restart { rule } => format!("restart rule={rule}"),
        }
    }
}

/// The class of a wall (aterm's `WallClass`): a wall met again in the same
/// class keeps its ladder, and one of another class starts afresh.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Class {
    /// An API error or an overload.
    Retry,
    /// A usage window or spend.
    Usage,
    /// A model's own bucket.
    Model,
    /// A full context.
    Context,
    /// A lost login.
    Auth,
    /// The critical-memory banner.
    Memory,
}

impl Class {
    /// The class of a wall of `kind`.
    #[must_use]
    pub const fn of(kind: WallKind) -> Self {
        match kind {
            WallKind::Overloaded | WallKind::ApiError { .. } => Self::Retry,
            WallKind::UsageSession | WallKind::UsageWeekly | WallKind::Spend => Self::Usage,
            WallKind::ModelBucket { .. } => Self::Model,
            WallKind::Context => Self::Context,
            WallKind::Auth => Self::Auth,
            WallKind::Memory => Self::Memory,
        }
    }
}

/// An outage: an API wall the agent cannot act on, waited out in silence and
/// tried blind (aterm's `Outage`, `outage_of` with no route measured).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outage {
    /// The service is overloaded.
    Overload,
    /// A server error.
    ApiError,
    /// The route to the API failed, or a certificate or proxy refused it.
    Connection,
}

impl Outage {
    /// The outage a wall of `kind` is, if it is one. A reply cut off is no
    /// outage here: aterm calls it one only where it measured the route down,
    /// which this harness does not measure.
    #[must_use]
    pub const fn of(kind: WallKind) -> Option<Self> {
        match kind {
            WallKind::Overloaded => Some(Self::Overload),
            WallKind::ApiError {
                cause: ApiCause::Server,
                ..
            } => Some(Self::ApiError),
            WallKind::ApiError {
                cause: ApiCause::Unreachable | ApiCause::Config,
                ..
            } => Some(Self::Connection),
            _ => None,
        }
    }

    /// The outage as one journal word.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Self::Overload => "overload",
            Self::ApiError => "api-error",
            Self::Connection => "connection",
        }
    }

    /// The outage as its resume names it ([`cleared_text`]).
    #[must_use]
    pub const fn what(self) -> &'static str {
        match self {
            Self::Overload => "API overload",
            Self::ApiError => "API error",
            Self::Connection => "connection error",
        }
    }
}

/// An act typed and not yet taken up.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Acted {
    scope: Scope,
    rule: &'static str,
    /// What it typed, so its own unsent text can be told from anyone else's.
    text: String,
    /// When it was typed, epoch ms.
    at_ms: u64,
    /// The screen's content sequence at the first look after it, which every
    /// later look compares: an unchanged screen is never acted on again.
    seen_seq: Option<u64>,
    /// Whether its Enter was pressed again, which is done once.
    entered: bool,
}

/// The wall being handled.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Track {
    class: Class,
    /// When it was met, or met again after a try, epoch ms: each ladder counts
    /// from here.
    since_ms: u64,
    /// The acts typed at it.
    attempts: u32,
    /// The act typed last, until the screen moved past it.
    acted: Option<Acted>,
    /// The act was taken up (a busy turn, or a screen that moved past it):
    /// the next sight of the wall counts its ladder from there.
    again: bool,
}

/// The duty's memory of the wall the session stands at: rebuilt with each
/// child.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Watch {
    track: Option<Track>,
}

/// `wait` past `at_ms`, saturating.
fn later(at_ms: u64, wait: Duration) -> u64 {
    at_ms.saturating_add(u64::try_from(wait.as_millis()).unwrap_or(u64::MAX))
}

/// `n` as a ladder's step.
fn step(n: u32) -> usize {
    usize::try_from(n).unwrap_or(usize::MAX)
}

impl Watch {
    /// Decide one look under `policy` and `timing`, updating what the duty
    /// remembers of the wall.
    ///
    /// A wall is tracked by its class from the look that first shows it: an
    /// ended turn with no wall (the agent got through) ends the track, and a
    /// busy turn or a box keeps it, so a try the wall meets again climbs its
    /// ladder. After an act nothing is done until the agent took it up (a
    /// busy turn, or a screen that moved past the one first read after it);
    /// the read that then shows the wall again counts its ladder from there.
    #[must_use]
    pub fn decide(
        &mut self,
        look: &Look<'_>,
        policy: &WallPolicy,
        timing: &TurnEndTiming,
    ) -> Decision {
        let reading = look.reading;
        if !matches!(reading.program, Program::Claude) {
            return Decision::Pass("not-claude");
        }
        if !reading.phase_authoritative {
            return Decision::Pass("uncertain");
        }
        if reading.phase == Phase::Prompt || reading.prompt.is_some() {
            return Decision::Pass("box");
        }
        if reading.phase == Phase::Busy
            && let Some(track) = self.track.as_mut()
            && track.acted.take().is_some()
        {
            // The agent took the act up.
            track.again = true;
        }
        let Some(wall) = reading.wall.as_ref() else {
            if reading.phase != Phase::Busy {
                self.track = None;
            }
            return Decision::Pass("no-wall");
        };
        let class = Class::of(wall.kind);
        // Claude Code draws the banner over a live turn too: the restart
        // does not wait for the turn's end.
        if class == Class::Memory {
            return memory(wall, look.relaunch);
        }
        if reading.phase == Phase::Busy {
            return Decision::Pass("busy");
        }
        if matches!(class, Class::Usage | Class::Model) {
            if look.limit.owed_resume {
                return Decision::Pass("owed-resume");
            }
            if look.limit.moving {
                return Decision::Nothing("fable-move");
            }
        }
        if self.track.as_ref().is_none_or(|track| track.class != class) {
            self.track = Some(Track {
                class,
                since_ms: look.now_ms,
                attempts: 0,
                acted: None,
                again: false,
            });
        }
        let Some(track) = self.track.as_mut() else {
            return Decision::Pass("no-track");
        };
        if let Some(acted) = track.acted.as_mut() {
            let seen = *acted.seen_seq.get_or_insert(look.seq);
            if seen == look.seq {
                return awaiting_take(acted, look, timing);
            }
            // The screen moved past the act with no busy turn seen: the agent
            // took it up and answered between two looks.
            track.acted = None;
            track.again = true;
        }
        if std::mem::take(&mut track.again) {
            // The try met the wall again: the next is counted from here.
            track.since_ms = look.now_ms;
        }
        let (since, attempts) = (track.since_ms, track.attempts);
        let message = wall.message.as_str();
        match wall.kind {
            WallKind::Overloaded | WallKind::ApiError { .. } => api(
                wall.kind,
                message,
                since,
                attempts,
                look.now_ms,
                policy,
                timing,
            ),
            WallKind::UsageSession
            | WallKind::UsageWeekly
            | WallKind::Spend
            | WallKind::ModelBucket { .. } => usage(wall, since, attempts, look, policy, timing),
            WallKind::Context => context(message, since, attempts, look.now_ms, policy, timing),
            WallKind::Auth => Decision::Leave(format!(
                "a lost login: {message}; signing in again opens the person's browser, so it is \
                 theirs (/login)"
            )),
            WallKind::Memory => memory(wall, look.relaunch),
        }
    }

    /// The act `rule` decided last was typed under `scope`, or may have
    /// been, at `now_ms`: it counts as a try, and nothing more is typed until
    /// the screen moved past it.
    pub fn typed(&mut self, scope: Scope, rule: &'static str, text: &str, now_ms: u64) {
        if let Some(track) = self.track.as_mut() {
            track.attempts = track.attempts.saturating_add(1);
            track.acted = Some(Acted {
                scope,
                rule,
                text: text.to_string(),
                at_ms: now_ms,
                seen_seq: None,
                entered: false,
            });
        }
    }
}

/// An act stands untaken on a screen that has not moved: nothing, unless its
/// own text still stands unsent [`TurnEndTiming::take_within`] after it was
/// typed, when its Enter is pressed once more (the turn-end duty's own rule,
/// [`crate::duty`]).
fn awaiting_take(acted: &mut Acted, look: &Look<'_>, timing: &TurnEndTiming) -> Decision {
    if acted.entered || !own_draft(look.reading, &acted.text) {
        return Decision::Nothing("awaiting-take");
    }
    let due = later(acted.at_ms, timing.take_within);
    if look.now_ms < due {
        return Decision::Wait {
            until_ms: due,
            why: "awaiting-take".to_string(),
        };
    }
    acted.entered = true;
    Decision::Submit {
        scope: acted.scope,
        rule: acted.rule,
    }
}

/// The critical-memory banner (aterm's `memory_restart`): the child is
/// restarted on its conversation while the policy lets it be brought back.
fn memory(wall: &Wall, relaunch: bool) -> Decision {
    if !relaunch {
        return Decision::Leave(format!(
            "memory critical: restart it: {} (relaunch is off)",
            aterm_phase::memory_banner_head(&wall.message)
        ));
    }
    Decision::Restart {
        rule: RULE_MEMORY_RESTART,
    }
}

/// An API wall of `kind`, met at `since` and tried `attempts` times (aterm's
/// `wall_own_act` for `Overloaded` and `ApiError`, with `outage_wait`'s blind
/// ladder): an outage waits its gap and is tried blind; a refused request
/// waits its own ladder's rung; a reply cut off, or over its maximum, is
/// tried at once and then on its ladder.
fn api(
    kind: WallKind,
    message: &str,
    since: u64,
    attempts: u32,
    now: u64,
    policy: &WallPolicy,
    timing: &TurnEndTiming,
) -> Decision {
    if !policy.retry_api_errors {
        return Decision::Leave(format!(
            "{}: {message} (retry_api_errors is off)",
            kind.name()
        ));
    }
    if let Some(outage) = Outage::of(kind) {
        let due = later(
            since,
            outage_gap(timing, attempts, retry_after_of(kind, message)),
        );
        if now < due {
            return Decision::Wait {
                until_ms: due,
                why: format!(
                    "outage-{} try={}",
                    outage.word(),
                    attempts.saturating_add(1)
                ),
            };
        }
        return Decision::Type {
            scope: Scope::Walls,
            rule: RULE_OUTAGE_RESUME,
            text: cleared_text(outage.what(), false),
        };
    }
    let rung =
        |ladder: &[Duration], n: usize| Ladder(ladder).at(n).map_or(now, |wait| later(since, wait));
    let at_once_then = |ladder: &[Duration]| {
        step(attempts)
            .checked_sub(1)
            .map_or(since, |n| rung(ladder, n))
    };
    let (due, rule, why) = match kind {
        WallKind::ApiError {
            cause: ApiCause::Request,
            ..
        } => (
            rung(timing.refused_backoff.as_slice(), step(attempts)),
            RULE_API_RETRY,
            "api-refused",
        ),
        WallKind::ApiError {
            cause: ApiCause::Overlong,
            ..
        } => (
            at_once_then(timing.retry_backoff.as_slice()),
            RULE_API_CUTOFF,
            "api-overlong",
        ),
        _ => (
            at_once_then(timing.net_backoff.as_slice()),
            RULE_API_CUTOFF,
            "api-cut-off",
        ),
    };
    if now < due {
        return Decision::Wait {
            until_ms: due,
            why: format!("{why} try={}", attempts.saturating_add(1)),
        };
    }
    Decision::Type {
        scope: Scope::Walls,
        rule,
        text: wall_retry_text(kind),
    }
}

/// A usage limit (aterm's `wall_own_act` for the usage windows, spend and a
/// model's bucket, with `resume_at_reset`). Fable's own limit, which the
/// harness moves the session off, never reaches here ([`Limit::moving`]).
fn usage(
    wall: &Wall,
    since: u64,
    attempts: u32,
    look: &Look<'_>,
    policy: &WallPolicy,
    timing: &TurnEndTiming,
) -> Decision {
    let (kind, message, now) = (wall.kind, wall.message.as_str(), look.now_ms);
    let reset = look.limit.reset_at_ms;
    if goes_on_by_itself(message, look.rows) {
        if !policy.resume_limits {
            return Decision::Nothing("goes-on-by-itself");
        }
        let due = match reset {
            Some(at) => later(at, timing.auto_resume_grace),
            None => later(
                since,
                timing.limit_backoff.last().copied().unwrap_or_default(),
            ),
        };
        if now < due {
            return Decision::Wait {
                until_ms: due,
                why: format!("{}-goes-on-by-itself", kind.name()),
            };
        }
        return resume(reset.is_some());
    }
    let credits_only = !policy.usage_credits
        && reset.is_none()
        && match kind {
            WallKind::Spend => true,
            WallKind::ModelBucket { .. } => names_usage_credits(message),
            _ => false,
        };
    if credits_only {
        return Decision::Leave(format!(
            "{}: {message} (only usage credits lift it, and usage_credits is off: nothing is \
             typed)",
            kind.name()
        ));
    }
    if !policy.resume_limits {
        return match kind {
            WallKind::UsageSession | WallKind::UsageWeekly => {
                Decision::Nothing("resume-limits-off")
            }
            _ => Decision::Leave(format!("{}: {message} (resume_limits is off)", kind.name())),
        };
    }
    resume_at_reset(kind, reset, since, attempts, look, timing)
}

/// When a usage limit with no auto-continue armed is resumed (aterm's
/// `resume_at_reset`): spend or a model's bucket that names no reset climbs
/// [`TurnEndTiming::entitlement_backoff`]; any other at its reset plus the
/// grace and the jitter, never sooner than [`TurnEndTiming::limit_backoff`]'s
/// rung after a try the limit met again unless a new reset was named since;
/// with no reset known, the ladder's last rung from the wall.
fn resume_at_reset(
    kind: WallKind,
    reset: Option<u64>,
    since: u64,
    attempts: u32,
    look: &Look<'_>,
    timing: &TurnEndTiming,
) -> Decision {
    let now = look.now_ms;
    if reset.is_none() && matches!(kind, WallKind::Spend | WallKind::ModelBucket { .. }) {
        let Some(wait) = Ladder(timing.entitlement_backoff.as_slice()).at(step(attempts)) else {
            return Decision::Nothing("no-ladder");
        };
        let due = later(since, wait);
        if now < due {
            return Decision::Wait {
                until_ms: due,
                why: format!("{}-no-reset-named", kind.name()),
            };
        }
        return resume(false);
    }
    let back = Ladder(timing.limit_backoff.as_slice());
    let at_reset = reset.map(|at| {
        later(
            later(at, timing.reset_grace),
            jitter(look.seed, timing.reset_jitter),
        )
    });
    let named_past_meet = reset.is_some_and(|at| at > since);
    let after_retry = if attempts == 0 || named_past_meet {
        None
    } else {
        back.at(step(attempts) - 1).map(|wait| later(since, wait))
    };
    let due = match (at_reset, after_retry) {
        (Some(at), Some(floor)) => at.max(floor),
        (Some(at), None) => at,
        (None, Some(floor)) => floor,
        (None, None) => match timing.limit_backoff.last() {
            Some(&wait) => later(since, wait),
            None => return Decision::Nothing("no-ladder"),
        },
    };
    if now < due {
        return Decision::Wait {
            until_ms: due,
            why: format!("{}-resets", kind.name()),
        };
    }
    resume(at_reset.is_some())
}

/// The resume past a usage limit, a `turn-end` act: *has cleared* where its
/// reset was known, *may have cleared* where it was tried blind.
fn resume(measured: bool) -> Decision {
    Decision::Type {
        scope: Scope::TurnEnd,
        rule: RULE_LIMIT_RESUME,
        text: cleared_text("usage limit", measured),
    }
}

/// A full context (aterm's `wall_own_act` for `Context`): `/compact`, and
/// again on [`TurnEndTiming::retry_backoff`] while it stays full.
fn context(
    message: &str,
    since: u64,
    attempts: u32,
    now: u64,
    policy: &WallPolicy,
    timing: &TurnEndTiming,
) -> Decision {
    if !policy.compact_on_context_wall {
        return Decision::Leave(format!(
            "context full: {message} (compact_on_context_wall is off)"
        ));
    }
    if let Some(wait) = step(attempts)
        .checked_sub(1)
        .and_then(|n| Ladder(timing.retry_backoff.as_slice()).at(n))
    {
        let due = later(since, wait);
        if now < due {
            return Decision::Wait {
                until_ms: due,
                why: format!(
                    "context-full-after-compact try={}",
                    attempts.saturating_add(1)
                ),
            };
        }
    }
    Decision::Type {
        scope: Scope::Walls,
        rule: RULE_COMPACT,
        text: COMPACT.to_string(),
    }
}

/// The share of `spread` this session waits past a reset: `seed` modulo it.
fn jitter(seed: u64, spread: Duration) -> Duration {
    let spread = u64::try_from(spread.as_millis()).unwrap_or(u64::MAX);
    if spread == 0 {
        return Duration::ZERO;
    }
    Duration::from_millis(seed % spread)
}

/// Whether Claude Code goes on by itself past this limit: its own
/// auto-continue is armed (`Continuing automatically at 3:15pm`, `Continuing
/// shortly`, the footer's countdown) and was not cancelled (aterm's
/// `limit::resumes_by_itself` on the wall's `message`, and aterm-phase's
/// [`aterm_phase::auto_continue_end`] on `rows`).
#[must_use]
pub fn goes_on_by_itself(message: &str, rows: &[String]) -> bool {
    match aterm_phase::auto_continue_end(rows) {
        Some(AutoContinue::Armed) => return true,
        Some(AutoContinue::Unkept | AutoContinue::Fired) => return false,
        None => {}
    }
    let lower = message.to_lowercase();
    lower.contains(aterm_phase::anchor!("wall.auto_continue"))
        || lower.contains("continuing shortly")
        || lower
            .replace('\u{2219}', "\u{b7}")
            .contains("available again \u{b7} continuing now")
        || lower.contains(&aterm_phase::anchor!("wall.goal_auto").to_lowercase())
}

/// Whether a limit's `message` names usage credits as what lifts it (aterm's
/// `names_usage_credits`).
fn names_usage_credits(message: &str) -> bool {
    let lower = message.to_lowercase();
    lower.contains("usage credits") || lower.contains("extra usage")
}

/// Whether the newest usage-limit notice on the `rows` above row `above`
/// (a usage limit's dialog's first row) is Fable's own (`You've hit your
/// Fable limit · …`): the limit the harness moves a Fable session off, which
/// its transcript may not name yet.
#[must_use]
pub fn fable_limit_above(rows: &[String], above: usize) -> bool {
    rows.iter()
        .take(above)
        .rev()
        // A prompt row (`❯ /rate-limit-options`, which Claude Code types
        // itself to open the dialog) is no notice.
        .filter(|row| !row.trim_start().starts_with('❯'))
        .find_map(|row| {
            // The reader skips the gutter's marks (`⎿`) itself.
            aterm_phase::classify_wall(row)
                .filter(WallKind::reads_limited)
                .map(|_| row.to_lowercase().contains("fable limit"))
        })
        .unwrap_or(false)
}

/// The wait a rate limit's own words name for its next try (aterm's
/// `retry_after_of`): a 429's `retry-after`, none for any other wall.
fn retry_after_of(kind: WallKind, message: &str) -> Option<Duration> {
    matches!(
        kind,
        WallKind::ApiError {
            code: Some(429),
            ..
        }
    )
    .then(|| aterm_phase::retry_after(message))
    .flatten()
}

/// The gap from an outage wall, or its last try, to try `n` (counted from 0;
/// aterm's `outage_gap`): [`TurnEndTiming::outage_first`] doubling to
/// [`TurnEndTiming::outage_cap`]; where the wall names its `retry-after`
/// (`after`, bounded by the cap), the first try comes then and no later one
/// sooner.
#[must_use]
pub fn outage_gap(timing: &TurnEndTiming, n: u32, after: Option<Duration>) -> Duration {
    let ladder = doubling(timing.outage_first, n, timing.outage_cap);
    match after.map(|after| after.min(timing.outage_cap)) {
        Some(after) if n == 0 => after,
        Some(after) => ladder.max(after),
        None => ladder,
    }
}

/// The one line a resume types past a condition that ended the agent's turn,
/// naming `what` cleared (aterm's `cleared_text`): *has cleared* where the
/// clearing was `measured`, *may have cleared* where it is a try.
#[must_use]
pub fn cleared_text(what: &str, measured: bool) -> String {
    let cleared = if measured {
        "has cleared"
    } else {
        "may have cleared"
    };
    format!(
        "{} The {what} {cleared}; continue where you stopped.",
        alab_policy::mark!()
    )
}

/// What a try at an API wall of `kind` types (aterm's `wall_retry_text`).
#[must_use]
pub fn wall_retry_text(kind: WallKind) -> String {
    let who = Program::Claude.product();
    let what = api_wall_words(kind);
    let cause = match kind {
        WallKind::ApiError { cause, .. } => cause,
        _ => ApiCause::Server,
    };
    match cause {
        ApiCause::CutOff => {
            format!("{who} reported {what}, so your last reply may be incomplete. {CARRY_ON}")
        }
        ApiCause::Unreachable | ApiCause::Config => {
            format!("{who} reported {what}; trying again. {CARRY_ON}")
        }
        ApiCause::Overlong => format!(
            "{who} reported {what}, so your last reply was cut off. Write long files and outputs \
             across several smaller replies. {CARRY_ON}"
        ),
        ApiCause::Server | ApiCause::Request => format!("{who} reported {what}. {CARRY_ON}"),
    }
}

/// An API wall in aterm's words, from its kind alone (aterm's
/// `api_wall_words`).
#[must_use]
pub fn api_wall_words(kind: WallKind) -> String {
    let (code, why) = match kind {
        WallKind::Overloaded => (None, Some("overloaded")),
        WallKind::ApiError { code, cause, .. } => (
            code,
            match cause {
                ApiCause::Server => None,
                ApiCause::Unreachable => Some("the API could not be reached"),
                ApiCause::CutOff => Some("the reply was cut off"),
                ApiCause::Config => Some("a certificate or proxy refusal"),
                ApiCause::Request => Some("the request was refused"),
                ApiCause::Overlong => Some("the reply was over its output token maximum"),
            },
        ),
        _ => (None, None),
    };
    match (code, why) {
        (Some(code), Some(why)) => format!("an API error ({code}, {why})"),
        (Some(code), None) => format!("an API error ({code})"),
        (None, Some(why)) => format!("an API error ({why})"),
        (None, None) => "an API error".to_string(),
    }
}

#[cfg(test)]
#[path = "walls_tests.rs"]
mod tests;
