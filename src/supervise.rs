// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The supervisor: run the vendor child, follow its signals and its end, and
//! move it onto a newer vendor build underneath the running harness.
//!
//! **Which launches.** Only a launch that can be moved onto another build is
//! supervised: stdin is a terminal, the command line is not a one-shot, and it
//! rewrites into a resume command line ([`live_update_refusal`]). Every other
//! launch never reaches this module: [`crate::cli`] runs the vendor in the
//! harness's own place. So everything below may assume a session that can be
//! resumed.
//!
//! **Process shape.** The child is spawned in the harness's own process group
//! on the inherited terminal (no `setpgid`, no proxying), so the harness stays
//! the job's leader and the child reads the terminal directly.
//!
//! **Signals.** INT and QUIT belong to the child (the terminal sends them to
//! the whole group; the harness catches and drops them). HUP and TERM sent to
//! the harness are passed on to the child, and the session then ends with it.
//! TSTP is followed: when the child stops, the harness stops itself, so the
//! shell sees the job stopped; when the job is continued, the harness
//! continues the child. A TSTP sent to the harness alone is passed on if the
//! child did not stop with it.
//!
//! **The end.** When the child ends on its own (or after a passed-on HUP or
//! TERM), the harness ends the same way: the same exit code, or death by the
//! same signal. The one exception is a crash under a proven aterm claim, below.
//!
//! **Live update.** On every look the harness reads atpkg's `current` link.
//! When it names a different usable build than the child runs, the harness
//! waits for a safe point ([`crate::quiet`]) with no background work under the
//! child ([`crate::procs`]), sends the child SIGTERM (what
//! aterm's own Claude upgrade lane sends at its idle gate:
//! `upgrade_drive.rs:3186-3236` `terminate`, journalled there as "SIGTERM
//! (idle: nothing asked or typed; resumed as it was)"), and when the child has
//! exited, spawns the new build with the resume argv
//! ([`crate::profile::resume_argv`], in the conversation's own permission mode,
//! [`crate::profile::with_permission_mode`]) in the same process, on the same
//! terminal. A child still running when the grace ends keeps running, and that
//! target build is never tried again (a later build re-arms the update): the
//! harness sends one SIGTERM per target, never a second. The harness process
//! never exits for an update.
//!
//! **aterm.** Inside an aterm tab the harness claims the session's `relaunch`
//! and `upgrade` duties before the first child starts ([`link`]). While that
//! claim stands proven, aterm's `status` replaces the harness's own reading of
//! the terminal's input (the input clock, the composer rule and the
//! unread-input check) at the safe point; every check the harness makes against
//! the kernel and the store stays. Without a proven claim, and whenever aterm
//! stops answering, the harness decides on its own readings exactly as it does
//! outside aterm.
//!
//! **Crash relaunch.** aterm's own engine stands down from bringing a crashed
//! agent back while a proven claim takes `relaunch`, so the harness does it,
//! by aterm's rule ([`crate::relaunch`]). A child that
//! ends abnormally ([`crate::relaunch::abnormal_exit`]) and leaves Claude
//! Code's session record standing [`crate::relaunch::EXIT_SETTLE`] later is a
//! crash. If one fresh policy and status read then shows this harness's proven
//! claim taking `relaunch`, the policy's `relaunch` switch on, and nobody the
//! exit belongs to (a person who typed within the grace, a hold, a driver),
//! the harness waits the back-off's pause (1 s, 10 s, 60 s, then 10 minutes;
//! over again after 10 healthy minutes), reads aterm once more, and starts the
//! same build with `--resume <conversation>`. During all of that there is no
//! child ([`Ending::Crashed`]): nothing is signalled or waited for by pid, no
//! update and no own-image move is made, the terminal is back in the modes the
//! harness found it in (keyboard reporting included), and a HUP, TERM, INT or
//! QUIT ends the harness with the crashed child's status. Anything short of
//! every condition ends the harness the way the child ended, and what was
//! typed at the dead screen is discarded. A link that is lost, or an answer
//! aterm can give only later, is waited for, up to [`link::RECLAIM_HOLD_MS`]
//! in all for one crash.
//!
//! **A restart aterm asks for.** aterm sends a claim holder `restart
//! cause=<why>` with an optional `model=` instead of signalling its child.
//! Under the same claim and one fresh permission read the harness ends the
//! child with SIGTERM at once
//! (the request is aterm's or a person's through aterm, so no safe point is
//! waited for), and SIGKILL after [`RESTART_GRACE_MS`] if it is still running,
//! as aterm's own restart act does, then starts the same build on the same
//! conversation. That is no crash and does not move the back-off. With no
//! session record yet there is no conversation to resume, and the request
//! waits for one, including its requested model.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use alab_wire::claim::Restart;

use crate::cli::{EXIT_VENDOR_MISSING, Options, say};
use crate::journal::Journal;
use crate::model_facts;
use crate::procs;
use crate::profile::{self, COMMAND, SessionRecord, TOOL};
use crate::quiet::{Look, Tracker, Wait};
use crate::relaunch::{EXIT_SETTLE, Ladder, abnormal_exit};
use crate::store::{Build, Store};
use crate::sys::{self, ChildEvent, SignalPipe, Termios};

#[doc(hidden)]
#[path = "supervise_image.rs"]
pub mod image;
#[path = "supervise_link.rs"]
mod link;
#[path = "supervise_transfer.rs"]
mod transfer;
#[path = "supervise_vendor.rs"]
mod vendor;

/// How long after the harness's own SIGTERM the child's exit still counts as
/// the end it asked for (and is relaunched); a later exit is the session's own.
/// The default of `--alab-term-grace-ms`.
pub const TERM_GRACE_MS: u64 = 30_000;

/// How long a child sent SIGTERM for a restart aterm asked for may take to end
/// before it is sent SIGKILL, and then how long that may take before the
/// restart is abandoned: aterm's own restart act's grace
/// (`aterm/crates/aterm-gui/src/input_stall.rs`, `RESTART_GRACE`). A shorter
/// `--alab-term-grace-ms` shortens it too.
pub const RESTART_GRACE_MS: u64 = 5_000;

/// How long a look that found background work under the child (or could not
/// read the process table) holds before the table is read again.
pub const BACKGROUND_RECHECK_MS: u64 = 2_000;

/// How long a TSTP sent to the harness waits for the child to stop with it
/// before the harness passes it on.
pub const TSTP_WAIT_MS: u64 = 250;

/// How much older than the spawn a record's `startedAt` may be and still be
/// this child's (clock granularity), not a stale record of an earlier pid.
pub const RECORD_SLACK_MS: u64 = 5_000;

/// How long after a vendor relaunch a non-clean exit counts as a failed start
/// and falls back to the previous build, while the new child has neither written
/// its session record nor taken the terminal out of its saved canonical mode
/// (docs/DECISIONS.md, 28).
pub const STARTUP_GRACE_MS: u64 = 15_000;

/// The most target builds one session remembers as abandoned ([`Abandoned`]).
pub const MAX_ABANDONED: usize = 64;

/// The target builds whose update this session abandoned (the child outlived
/// its SIGTERM, or the build failed to start): an ordered set of build numbers,
/// at most [`MAX_ABANDONED`] of them. A build in it is never sent SIGTERM for
/// again, whichever builds `current` names in between. A full set forgets
/// nothing: [`Abandoned::refusal`] then refuses every further target, because a
/// target that could not be remembered if it were abandoned too could be
/// signalled twice. That ends live updates for the rest of the session (the
/// child keeps running on its build), which is the fail-closed side of "at most
/// one SIGTERM per target build".
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct Abandoned {
    /// The remembered build numbers, ascending.
    builds: std::collections::BTreeSet<u64>,
}

/// Why [`Abandoned`] refuses a target build.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AbandonedRefusal {
    /// This very build was abandoned before.
    Build,
    /// The set is full: no further target can be remembered, so none is tried.
    Full,
}

impl Abandoned {
    /// The set holding `builds`: `None` unless they are strictly ascending and
    /// at most [`MAX_ABANDONED`] (the one spelling a transfer may carry).
    fn from_ascending(builds: &[u64]) -> Option<Self> {
        (builds.len() <= MAX_ABANDONED && builds.windows(2).all(|pair| pair[0] < pair[1])).then(
            || Self {
                builds: builds.iter().copied().collect(),
            },
        )
    }

    /// The remembered build numbers, ascending.
    fn builds(&self) -> impl ExactSizeIterator<Item = u64> + '_ {
        self.builds.iter().copied()
    }

    /// Why `build` must not be sent SIGTERM for, when it must not.
    fn refusal(&self, build: u64) -> Option<AbandonedRefusal> {
        if self.builds.contains(&build) {
            Some(AbandonedRefusal::Build)
        } else if self.builds.len() >= MAX_ABANDONED {
            Some(AbandonedRefusal::Full)
        } else {
            None
        }
    }

    /// Remember `build`. A target is signalled only while [`Abandoned::refusal`]
    /// allows it, so there is room for it; were there none, the set stays full
    /// and keeps refusing every target, this one included.
    fn remember(&mut self, build: u64) {
        if self.builds.len() < MAX_ABANDONED {
            self.builds.insert(build);
        }
    }
}

/// Whether `event` is a startup failure rather than a clean exit or a user/session signal.
const fn refused_start(event: ChildEvent) -> bool {
    match event {
        ChildEvent::Exited(code) => code != 0 && !matches!(code, 129 | 130 | 131 | 143),
        ChildEvent::Signaled(sig) => !matches!(
            sig,
            libc::SIGINT | libc::SIGQUIT | libc::SIGTERM | libc::SIGHUP
        ),
        ChildEvent::Stopped(_) | ChildEvent::Continued => false,
    }
}

/// The one line the terminal gets when the session has been moved from the
/// build of version `old` onto the started build of version `new`.
fn relaunch_notice(old: Option<&str>, new: Option<&str>, session_id: &str) -> String {
    format!(
        "{TOOL}: {COMMAND} {} -> {}: resuming conversation {session_id} on the new build",
        old.unwrap_or("?"),
        new.unwrap_or("?"),
    )
}

/// `event` as the terminal is told of it: `exit <code>` or `signal <number>`.
fn end_words(event: ChildEvent) -> String {
    match event {
        ChildEvent::Exited(code) => format!("exit {code}"),
        ChildEvent::Signaled(signal) => format!("signal {signal}"),
        ChildEvent::Stopped(_) | ChildEvent::Continued => "no end".to_string(),
    }
}

/// The one line the terminal gets when a crashed session (the build of version
/// `version`, ended as `event`) has been started again on its conversation.
fn crash_notice(version: Option<&str>, event: ChildEvent, session_id: &str) -> String {
    format!(
        "{TOOL}: {COMMAND} {} ended abnormally ({}); resuming conversation {session_id}",
        version.unwrap_or("?"),
        end_words(event),
    )
}

/// The one line the terminal gets when a crash of the build of version
/// `version` (ended as `event`) is to be answered with a relaunch `pause_ms`
/// from now. Until then no program reads the terminal: what is typed is
/// discarded, and Ctrl-C ends the session.
fn pause_notice(version: Option<&str>, event: ChildEvent, pause_ms: u64) -> String {
    format!(
        "{TOOL}: {COMMAND} {} ended abnormally ({}); restarting in {} s, Ctrl-C ends the session",
        version.unwrap_or("?"),
        end_words(event),
        pause_ms.div_ceil(1_000),
    )
}

/// The one line the terminal gets when the build of version `version` has been
/// started again on its conversation because aterm asked (`cause`).
fn restart_notice(version: Option<&str>, cause: &str, session_id: &str) -> String {
    format!(
        "{TOOL}: {COMMAND} {} restarted at aterm's request ({cause}); resuming conversation {session_id}",
        version.unwrap_or("?"),
    )
}

/// The one line the terminal gets when a moved build (version `new`) failed
/// to start. With `refused` absent the previous build (version `old`) has been
/// started and the line says the conversation resumes on it; with the reason
/// the previous build could not be restarted, the line says so and claims no
/// resume.
fn fallback_notice(
    new: Option<&str>,
    old: Option<&str>,
    session_id: &str,
    refused: Option<&std::io::Error>,
) -> String {
    let (new, old) = (new.unwrap_or("?"), old.unwrap_or("?"));
    match refused {
        None => format!(
            "{TOOL}: {COMMAND} {new} failed to start; resuming conversation {session_id} on {old}"
        ),
        Some(why) => format!(
            "{TOOL}: {COMMAND} {new} failed to start and {old} could not be restarted \
             (conversation {session_id}): {why}"
        ),
    }
}

/// Now, epoch milliseconds.
#[must_use]
pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

/// Everything [`run`] starts from: a launch that can be moved
/// ([`live_update_refusal`] finds nothing against `args`, on a terminal).
#[derive(Clone, Debug)]
pub struct Setup {
    /// The user's home.
    pub home: PathBuf,
    /// The vendor's atpkg store.
    pub store: Store,
    /// This harness's own executable, which the store must never resolve to.
    pub own_exe: Option<PathBuf>,
    /// The build to start on.
    pub build: Build,
    /// The harness's settings.
    pub opts: Options,
    /// The vendor arguments, unchanged.
    pub args: Vec<OsString>,
    /// This harness's nesting depth ([`crate::cli::nesting_depth`]).
    pub depth: u32,
}

/// Why this launch can never be moved onto another build, in the journal's
/// words, or `None` when it can. Decided once from the launch
/// ([`crate::cli::main`]), before anything is started. The first that holds:
///
/// * `no-live-update`: `--alab-no-live-update` asked for it (`live_update` is
///   false);
/// * `no-tty`: stdin is not a terminal (`on_tty` is false), so there is no
///   interactive session to resume;
/// * `argv-not-utf8`: an argument is not UTF-8, and the resume grammar reads
///   text;
/// * `one-shot`: `-p`, `--print`, `-h`, `--help`, `-v` or `--version` is among
///   the arguments ([`profile::one_shot`]);
/// * `unresumable <why>`: the arguments do not rewrite into a resume command
///   line ([`profile::resume_argv`]: `--worktree`, an unknown flag, …).
///
/// A launch with a reason is not supervised: the vendor runs in the harness's
/// own place. The supervisor would claim nothing from aterm for it, never
/// update it and never relaunch it, and by standing in front of the vendor it
/// would keep aterm from proving the tab's foreground leader.
#[must_use]
pub fn live_update_refusal(live_update: bool, on_tty: bool, args: &[OsString]) -> Option<String> {
    if !live_update {
        return Some("no-live-update".to_string());
    }
    if !on_tty {
        return Some("no-tty".to_string());
    }
    let Some(argv) = launch_argv(args) else {
        return Some("argv-not-utf8".to_string());
    };
    if profile::one_shot(&argv) {
        return Some("one-shot".to_string());
    }
    if let Err(refusal) = profile::resume_argv(&argv, "00000000") {
        return Some(format!("unresumable {refusal}"));
    }
    None
}

/// `argv` as the resume grammar reads it: the vendor command first, then the
/// arguments; `None` when an argument is not UTF-8.
#[must_use]
pub fn launch_argv(args: &[OsString]) -> Option<Vec<String>> {
    std::iter::once(Some(COMMAND.to_string()))
        .chain(args.iter().map(|a| a.to_str().map(str::to_string)))
        .collect()
}

/// The child the harness runs now.
struct Child {
    pid: libc::pid_t,
    build: Build,
    /// The current child's arguments, which can differ from the original launch after a resume.
    args: Vec<OsString>,
    /// Missing identity forbids a future image transfer, never ordinary child supervision.
    identity: Option<crate::procs::ChildIdentity>,
    /// A launch helper still has to exec the exact expected vendor image.
    identity_pending: bool,
    tracker: Tracker,
    spawned_ms: u64,
    stopped: bool,
}

/// What the harness has asked of the child's end.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Ending {
    /// Nothing: an exit is the session's own.
    None,
    /// The harness sent SIGTERM to move onto `target`, resuming `session_id`.
    /// `sent` is when, on a clock that stops while the machine sleeps (a
    /// child cannot act on a signal then); time the child spends stopped
    /// does not count either ([`Supervisor::follow_stop`]).
    ForUpdate { sent: Instant, plan: vendor::Plan },
    /// The old child has been reaped. Retain the automatic replacement and
    /// its leases until the job has the foreground. Its synchronous launch
    /// attempt borrows this state and temporarily takes the plan.
    AwaitingUpdate {
        launch: UpdateLaunch,
        plan: Option<vendor::Plan>,
    },
    /// HUP or TERM was passed on: the session ends with the child.
    Passed(libc::c_int),
    /// The harness sent SIGTERM because aterm asked for a restart (`cause`),
    /// and SIGKILL once the child outlived [`RESTART_GRACE_MS`] (`killed`):
    /// the same build is started again from `plan`, whose target is its old,
    /// whenever the child ends while this stands. `sent` is when the last
    /// signal was sent, on a clock that stops while the machine sleeps; time
    /// the child spends stopped does not count.
    ForRestart {
        sent: Instant,
        plan: vendor::Plan,
        cause: String,
        /// The updated model basis, installed when the replacement starts.
        selected_args: Option<Vec<OsString>>,
        killed: bool,
    },
    /// The child crashed and was reaped: **no child runs**, and
    /// [`Child::pid`] names no process of this harness. The harness is
    /// deciding whether to bring the child back, or pausing before it does
    /// ([`Crash`]). If it does not, it ends as `event`.
    Crashed { event: ChildEvent, crash: Crash },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum UpdateLaunch {
    Target(ChildEvent),
    Fallback(FallbackCause),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FallbackCause {
    FailedSpawn,
    EarlyExit(ChildEvent),
}

/// A crash being answered: what is known of the conversation, and what the
/// harness waits for before its next step.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Crash {
    /// The crashed child's record, held for its conversation identity.
    record: SessionRecord,
    /// When the child's end was read (epoch ms); the pause runs from here.
    exit_ms: u64,
    /// When (epoch ms) the back-off's pause is over; set once the record is
    /// known to have survived.
    due_ms: u64,
    /// The resume argv, from the launch arguments and the surviving record.
    resume: Vec<String>,
    /// How long (ms) this crash has already waited for aterm in earlier waits:
    /// one crash waits at most [`link::RECLAIM_HOLD_MS`] for aterm in all.
    reclaim_spent_ms: u64,
    /// When (epoch ms) the wait for aterm now under way began, if one is.
    reclaim_since_ms: Option<u64>,
    /// When (epoch ms) aterm is asked again while a held link answered that
    /// it could answer only later.
    ask_at_ms: u64,
    /// Whether the terminal was told the relaunch is coming.
    told: bool,
    /// What the harness waits for now.
    wait: CrashWait,
}

impl Crash {
    /// When (epoch ms) the wait for aterm under way runs out: when the
    /// whole budget of [`link::RECLAIM_HOLD_MS`] has been spent, earlier waits
    /// counted.
    fn reclaim_until_ms(&self) -> Option<u64> {
        self.reclaim_since_ms.map(|since| {
            since.saturating_add(link::RECLAIM_HOLD_MS.saturating_sub(self.reclaim_spent_ms))
        })
    }

    /// aterm was found away (or answered "later") at `now`: a wait begins
    /// unless one is under way. `true` when it began here.
    fn reclaim_begins(&mut self, now: u64) -> bool {
        let began = self.reclaim_since_ms.is_none();
        if began {
            self.reclaim_since_ms = Some(now);
        }
        began
    }

    /// aterm answered at `now`: the wait under way, if any, is over, and its
    /// time is spent from the budget.
    fn reclaim_ends(&mut self, now: u64) {
        if let Some(since) = self.reclaim_since_ms.take() {
            self.reclaim_spent_ms = self
                .reclaim_spent_ms
                .saturating_add(now.saturating_sub(since));
        }
    }

    /// aterm's `word` on bringing the child back, heard at `now`
    /// ([`Supervisor::crash_judged`]):
    ///
    /// * permitted: a wait for aterm under way ends, and the pause runs
    ///   ([`CrashWait::Pause`]); `tell` the first time only;
    /// * denied: the crash is passed on;
    /// * the link is away, or aterm can answer only later: the crash waits in
    ///   [`CrashWait::Claim`] for what is left of [`link::RECLAIM_HOLD_MS`]
    ///   after this crash's earlier waits, journalled when a wait begins, and
    ///   is passed on once that is spent. Only a "later" is asked again on its
    ///   own clock ([`Crash::ask_at_ms`]); a lost link is asked once it is
    ///   claimed again.
    fn heard(&mut self, word: link::Relaunch, now: u64) -> Heard {
        let (waiting, why) = match word {
            link::Relaunch::Permitted => {
                self.reclaim_ends(now);
                self.wait = CrashWait::Pause;
                return Heard::Pause {
                    tell: !std::mem::replace(&mut self.told, true),
                };
            }
            link::Relaunch::Denied(why) => return Heard::PassOn { waited: None, why },
            link::Relaunch::Away(why) => ("claim", why),
            link::Relaunch::Later(why) => {
                self.ask_at_ms = now.saturating_add(link::LATER_RETRY_MS);
                ("aterm", why)
            }
        };
        let began = self.reclaim_begins(now);
        self.wait = CrashWait::Claim;
        let until = self.reclaim_until_ms().unwrap_or(now);
        let line = began.then(|| {
            format!(
                "crash waiting-for-{waiting} hold-ms={} {why}",
                until.saturating_sub(now)
            )
        });
        if now >= until {
            let gave_up = if waiting == "claim" {
                "claim-not-proven-again"
            } else {
                "aterm-not-answered"
            };
            return Heard::PassOn {
                waited: line,
                why: format!("{gave_up} {why}"),
            };
        }
        Heard::Wait(line)
    }
}

/// What a crash does with aterm's word ([`Crash::heard`]).
#[derive(Clone, Debug, PartialEq, Eq)]
enum Heard {
    /// The pause runs; `tell` the terminal the relaunch is coming.
    Pause { tell: bool },
    /// It waits for aterm; the journal line of a wait that began.
    Wait(Option<String>),
    /// It is passed on, as `crash-passed-on <why>`, after the journal line
    /// of a wait that began and ended at once (`waited`).
    PassOn { waited: Option<String>, why: String },
}

/// What a crash being answered waits for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CrashWait {
    /// A graceful exit removing its own session record: until this time
    /// (epoch ms), [`EXIT_SETTLE`] after the end.
    Settle { until_ms: u64 },
    /// aterm proving this harness's claim again, or answering what it could
    /// answer only later ([`Crash::reclaim_until_ms`]).
    Claim,
    /// The back-off's pause ([`Crash::due_ms`]).
    Pause,
}

/// Why the child is started again on its conversation.
enum Cause {
    /// A move onto another vendor build (or the fallback of one).
    Update,
    /// aterm asked for a restart, for this reason.
    Asked(String),
    /// The child crashed.
    Crash,
}

/// A restart aterm asked for that cannot be made yet.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Deferred {
    /// The request, including any model for the replacement child.
    restart: Restart,
    /// The conversation already selected, if the child supplied a record.
    record: Option<SessionRecord>,
    /// Why it waits, as journal words.
    why: String,
    /// When (epoch ms) it began to wait for aterm, for a request that waits
    /// on aterm's answer ([`link::RECLAIM_HOLD_MS`] in all); `None` for one
    /// that waits on the child or an own-image move.
    on_aterm_since_ms: Option<u64>,
    /// When (epoch ms) it is tried again.
    retry_at_ms: u64,
}

struct Supervisor {
    image: image::Runtime,
    setup: Setup,
    journal: Journal,
    config: PathBuf,
    cwd: Option<PathBuf>,
    termios: Option<Termios>,
    child: Child,
    ending: Ending,
    /// A vendor move whose target build just spawned and has neither written
    /// its session record nor left the saved canonical terminal mode: if it
    /// fails within [`STARTUP_GRACE_MS`], the harness marks the target abandoned
    /// and falls back to `old`. A build that has taken the terminal has started;
    /// its failure ends the session like any other exit.
    moved: Option<vendor::Plan>,
    /// The last wait reason journalled, so a reason is noted once per change.
    noted: String,
    /// The last update target journalled.
    seen_target: Option<u64>,
    /// Every target build whose update was abandoned: never tried again, even
    /// after `current` named other builds in between; a build not in the set
    /// re-arms. Bounded, and when full it refuses every further target rather
    /// than forget one ([`Abandoned`]).
    abandoned: Abandoned,
    /// Until when (epoch ms) found background work holds the update without
    /// reading the process table again.
    background_hold_until: u64,
    /// What the last read of the process table found (the wait reason).
    background_found: String,
    /// What the harness has of aterm: nothing, a proven claim, or a claim to
    /// make again ([`link::Link`]).
    link: link::Link,
    /// The crash back-off of this session. It is not carried across an
    /// own-image move: a new image starts with nothing on it.
    ladder: Ladder,
    /// The one restart aterm asked for that waits (for the child's session
    /// record, or for an own-image move to end); the latest request replaces
    /// an earlier one. While it waits no own-image move is started.
    restart_deferred: Option<Deferred>,
    /// The model, effort level and top-effort key the child states in its
    /// transcript, read on each look ([`Supervisor::read_facts`]). An
    /// own-image move carries it, so the new image goes on reading where this
    /// one stopped.
    facts: model_facts::Reader,
}

/// Run the vendor under supervision; never returns.
pub fn run(setup: &Setup) -> ! {
    run_with_image_host(setup, image::default_host)
}

/// Run with an explicit authority provider. The default uses this supervisor's
/// local authority outside aterm and admits no own-image move within it.
#[doc(hidden)]
pub fn run_with_image_host(setup: &Setup, factory: image::HostFactory) -> ! {
    let journal = Journal::new(
        std::env::var_os("XDG_STATE_HOME")
            .map(PathBuf::from)
            .as_deref(),
        Some(&setup.home),
        std::process::id(),
    );
    let mut pipe = match SignalPipe::install() {
        Ok(p) => p,
        Err(e) => {
            say(&format!("{TOOL}: cannot set up signal handling: {e}"));
            journal.note(&format!("refused signal-setup {e}"));
            std::process::exit(1);
        }
    };
    journal.note(&format!(
        "start harness={} vendor-build={} vendor={}",
        crate::cli::VERSION,
        setup.build.number,
        setup.build.version.as_deref().unwrap_or("-")
    ));
    // The terminal's modes as the harness found them, saved before the child
    // can put the terminal in raw mode; a relaunch restores them.
    let termios = Termios::save(0);
    // The claim is made, or given up for the session, before the first child
    // exists: aterm never sees an unclaimed child of a harness that will claim.
    let mut link = link::Link::launch(&journal, setup.build.version.as_deref());
    link.before_spawn(&journal, setup.build.version.as_deref());
    // The claim sequence can take seconds, and a signal caught meanwhile is
    // nobody's but the harness's: no child exists to have it, and one started
    // now would never see it. One that ends a session ends this one before it
    // starts, as it would have ended the vendor.
    // A TSTP caught meanwhile (Ctrl-Z) is the person's word to the job the
    // vendor is about to join: it is handed to the loop's first pass.
    let early = pipe.wait(0);
    if let Some(sig) = session_ending(&early) {
        journal.note(&format!("exit signal {sig} before-start"));
        link.release();
        sys::exit_like(ChildEvent::Signaled(sig));
    }
    let tstp = early.contains(&libc::SIGTSTP);
    let child = match spawn(&setup.build, &setup.args, &setup.opts, setup.depth) {
        Ok(c) => c,
        Err(e) => {
            say(&format!(
                "{TOOL}: cannot run {}: {e}",
                setup.build.exe.display()
            ));
            journal.note(&format!("refused spawn-failed {e}"));
            std::process::exit(EXIT_VENDOR_MISSING);
        }
    };
    let mut sup = Supervisor {
        image: image::Runtime::new(factory),
        setup: setup.clone(),
        journal,
        config: profile::config_dir(
            &setup.home,
            std::env::var_os("CLAUDE_CONFIG_DIR").as_deref(),
        ),
        cwd: std::env::current_dir().ok(),
        termios,
        child,
        ending: Ending::None,
        moved: None,
        noted: String::new(),
        seen_target: None,
        abandoned: Abandoned::default(),
        background_hold_until: 0,
        background_found: String::new(),
        link,
        ladder: Ladder::new(),
        restart_deferred: None,
        facts: model_facts::Reader::default(),
    };
    // An older host gets the original v3 launch unchanged. A host that can
    // prove a separate vendor child receives its native identity immediately
    // after spawn, before any built-in typing duty can use that child.
    sup.link_tick();
    sup.run(&mut pipe, tstp)
}

/// The first of `signals` that ends a session while no child runs: HUP, TERM,
/// INT or QUIT. TSTP, CONT and CHLD find nothing to act on then.
fn session_ending(signals: &[libc::c_int]) -> Option<libc::c_int> {
    signals.iter().copied().find(|sig| {
        matches!(
            *sig,
            libc::SIGHUP | libc::SIGTERM | libc::SIGINT | libc::SIGQUIT
        )
    })
}

fn spawn(build: &Build, args: &[OsString], opts: &Options, depth: u32) -> std::io::Result<Child> {
    let spawned_ms = now_ms();
    let mut command = crate::launch::vendor_command(&build.exe, args, true);
    command
        .envs(build.env.iter().map(|(k, v)| (k, v)))
        .envs(crate::profile::HARNESS_ENV.iter().copied())
        .envs(crate::cli::child_marks(depth));
    crate::launch::renderer(&mut command);
    let proc = command.spawn()?;
    launched(proc, build, args, opts, spawned_ms)
}

fn launched(
    proc: std::process::Child,
    build: &Build,
    args: &[OsString],
    opts: &Options,
    spawned_ms: u64,
) -> std::io::Result<Child> {
    let child_pid = proc.id();
    let pid = libc::pid_t::try_from(child_pid)
        .map_err(|_| std::io::Error::other("child pid out of range"))?;
    // The child is reaped with `waitpid` (which also reports stops), never through `proc`.
    drop(proc);
    let identity = crate::procs::capture_child(child_pid);
    let identity_pending = identity.as_ref().is_some_and(|identity| {
        use std::os::unix::fs::MetadataExt as _;
        !std::fs::metadata(&build.exe).is_ok_and(|expected| {
            expected.dev() == identity.executable.device
                && expected.ino() == identity.executable.inode
        })
    });
    Ok(Child {
        pid,
        build: build.clone(),
        args: args.to_vec(),
        identity,
        identity_pending,
        tracker: Tracker::new(spawned_ms, opts.poll_ms, opts.quiet_ms),
        spawned_ms,
        stopped: false,
    })
}

impl Supervisor {
    /// A token-route helper may exec the vendor after spawn returned. Pin
    /// that image only within the original child's exact kernel lifetime;
    /// own-image transfer keeps its existing immutable image witness thereafter.
    fn refresh_vendor_identity(&mut self) {
        if !self.child.identity_pending {
            return;
        }
        let Some(expected) = self.child.identity.as_ref() else {
            return;
        };
        if let Some(fresh) = crate::procs::refresh_child_image(expected, &self.child.build.exe) {
            self.journal.note(&format!(
                "vendor-image-pinned pid={} device={} inode={}",
                fresh.pid, fresh.executable.device, fresh.executable.inode
            ));
            self.child.identity = Some(fresh);
            self.child.identity_pending = false;
        }
    }

    /// The supervisor's loop: wait for a signal, an event from aterm or the
    /// next look; act on the signals; reap the child; then make the look. It
    /// ends only by ending the process. `early_tstp` is a TSTP caught before
    /// the first child existed, acted on in the first pass.
    fn run(&mut self, pipe: &mut SignalPipe, early_tstp: bool) -> ! {
        let poll = self.setup.opts.poll_ms;
        let mut next_look = 0u64;
        let mut early_tstp = early_tstp;
        let mut pending_signals = Vec::new();
        loop {
            let now = now_ms();
            // A crash being answered has its own clock: the wait ends when its
            // next step is due, whatever the poll.
            let timeout = self
                .crash_wake_ms()
                .map_or(next_look, |wake| wake.min(next_look))
                .min(self.link.due_ms().unwrap_or(u64::MAX))
                .saturating_sub(now)
                .min(if pending_signals.is_empty() { poll } else { 0 });
            // The first pass waits for nothing (`next_look` is 0).
            let mut tstp = std::mem::take(&mut early_tstp);
            // With no event stream this is `pipe.wait(timeout)`.
            let (signals, events) = pipe.wait_with(timeout, self.link.fds());
            for sig in pending_signals.drain(..).chain(signals) {
                match sig {
                    libc::SIGHUP | libc::SIGTERM => self.pass_on(sig),
                    // With no child, INT and QUIT are nobody's but the harness's.
                    libc::SIGINT | libc::SIGQUIT => self.childless_signalled(sig),
                    libc::SIGTSTP => tstp = true,
                    libc::SIGCONT if self.child.stopped => {
                        let _ = self.signal_child(libc::SIGCONT);
                    }
                    // CHLD is read below.
                    _ => {}
                }
            }
            // The child is reaped before aterm's events are read: a restart
            // request is never taken for a child that has already ended, or
            // one that has stopped. (A relaunch drains what aterm had sent
            // for the child that ended before it starts another,
            // [`Supervisor::relaunch`].)
            self.reap(pipe);
            // An event from aterm is drained at once (never left readable, which
            // would spin this loop) and makes a look due now. The poll stays:
            // nothing announces a move of the store's `current` link.
            let mut woken = events && self.link_events(false);
            if self.no_child() {
                if self.awaiting_update() {
                    if tstp {
                        self.stop_pending_update();
                    }
                } else {
                    // A crash never waits beside an own-image move in flight.
                    self.crash_tick(pipe);
                }
            } else if self.child.stopped {
                self.follow_stop(pipe);
                continue;
            } else if tstp {
                self.on_tstp(pipe);
                continue;
            }
            let due = now_ms() >= next_look;
            if due || woken {
                self.resume_update(pipe);
            }
            if due {
                next_look = now_ms().saturating_add(poll);
                if !self.no_child() && !self.image.blocks_vendor_move() {
                    self.refresh_vendor_identity();
                }
                let mut image = std::mem::take(&mut self.image);
                image.tick(self, pipe);
                self.image = image;
            }
            if due || self.link.due_ms().is_some_and(|at| now_ms() >= at) {
                self.link_tick();
                if self.link.streaming() {
                    woken |= self.link_events(false);
                }
            }
            if due {
                self.restart_retry();
            }
            if (due || woken) && !self.image.blocks_vendor_move() {
                self.look(|| {
                    pending_signals = pipe.wait(0);
                    // The process-table helper also produces SIGCHLD. The
                    // update checks the vendor's own wait status separately.
                    pending_signals.iter().any(|&sig| sig != libc::SIGCHLD)
                });
            }
        }
    }

    /// Whether [`Child::pid`] has been reaped and must be neither signalled
    /// nor waited for while its replacement is prepared.
    const fn no_child(&self) -> bool {
        self.awaiting_update() || matches!(self.ending, Ending::Crashed { .. })
    }

    const fn awaiting_update(&self) -> bool {
        matches!(self.ending, Ending::AwaitingUpdate { .. })
    }

    /// Send the child `sig`; `true` when the kernel accepted it. With no child
    /// ([`Supervisor::no_child`]) nothing is sent: the pid was reaped and may
    /// name another process by now.
    fn signal_child(&self, sig: libc::c_int) -> bool {
        !self.no_child() && sys::kill(self.child.pid, sig)
    }

    /// Read every pending child event; an end is handled here (relaunch or
    /// exit). With no child there is nothing to read, and a reaped pid is never
    /// waited for again.
    fn reap(&mut self, pipe: &SignalPipe) {
        while !self.no_child()
            && let Some(event) = sys::wait_child(self.child.pid)
        {
            self.dispatch(event, pipe);
        }
    }

    /// Act on one child event: a stop or a continue is recorded, an end is
    /// handled ([`Supervisor::on_end`]).
    fn dispatch(&mut self, event: ChildEvent, pipe: &SignalPipe) {
        match event {
            ChildEvent::Stopped(_) => self.child.stopped = true,
            ChildEvent::Continued => self.child.stopped = false,
            ChildEvent::Exited(_) | ChildEvent::Signaled(_) => self.on_end(event, pipe),
        }
    }

    /// HUP or TERM reached the harness: pass it on to the child, and the
    /// session then ends with it. With no child the signal ends the harness.
    fn pass_on(&mut self, sig: libc::c_int) {
        self.childless_signalled(sig);
        let _ = self.signal_child(sig);
        if self.child.stopped {
            // A stopped child acts on HUP or TERM only once continued.
            let _ = self.signal_child(libc::SIGCONT);
        }
        match &self.ending {
            Ending::ForUpdate { .. } => self
                .journal
                .note("update-cancelled a passed-on signal ends the session"),
            Ending::ForRestart { cause, .. } => self.journal.note(&format!(
                "restart-cancelled cause={cause} a passed-on signal ends the session"
            )),
            Ending::None
            | Ending::Passed(_)
            | Ending::Crashed { .. }
            | Ending::AwaitingUpdate { .. } => {}
        }
        self.ending = Ending::Passed(sig);
        self.moved = None;
        self.journal.note(&format!("signal passed-on {sig}"));
    }

    /// The child stopped: stop the harness too, then continue the child once the
    /// job is continued. With no child there is nothing to follow. The grace
    /// of a SIGTERM the harness sent starts again: a stopped child acts on no
    /// signal but SIGKILL, so its time stopped is not time it had to end.
    fn follow_stop(&mut self, pipe: &SignalPipe) {
        if self.no_child() {
            return;
        }
        self.journal.note("stopped following the child");
        sys::stop_self();
        // A shell's fg continues the whole group. Consume that native event
        // before deciding whether a harness-only CONT must wake the child.
        self.reap(pipe);
        if self.no_child() {
            return;
        }
        if self.child.stopped
            && self
                .child
                .identity
                .as_ref()
                .and_then(procs::observe_child)
                .is_none_or(|seen| seen.state == procs::ChildState::Stopped)
        {
            let _ = self.signal_child(libc::SIGCONT);
            self.child.stopped = false;
        }
        if let Ending::ForUpdate { sent, .. } | Ending::ForRestart { sent, .. } = &mut self.ending {
            *sent = Instant::now();
        }
        self.journal.note("continued");
    }

    /// A TSTP reached the harness: follow the child if it stops with it, else
    /// pass it on. With no child, and from the moment a child read here turns
    /// out to have crashed, nothing is waited for and nothing is sent.
    fn on_tstp(&mut self, pipe: &SignalPipe) {
        let deadline = now_ms().saturating_add(TSTP_WAIT_MS);
        while !self.no_child() && now_ms() < deadline {
            if let Some(event) = sys::wait_child(self.child.pid) {
                self.dispatch(event, pipe);
                if self.no_child() {
                    break;
                }
                if self.child.stopped {
                    self.follow_stop(pipe);
                    return;
                }
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        if self.awaiting_update() {
            self.stop_pending_update();
            return;
        }
        if self.no_child() {
            return;
        }
        let _ = self.signal_child(libc::SIGTSTP);
        self.journal.note("signal passed-on TSTP");
    }

    /// The child ended. An update's end inside its grace, and any end while a
    /// restart aterm asked for stands, is answered by the relaunch it was for;
    /// a moved build that refused to start falls back; an abnormal end nobody
    /// asked for may be a crash to answer ([`Supervisor::crash_begins`]).
    /// Every other end is the session's, and the harness ends the same way.
    fn on_end(&mut self, event: ChildEvent, pipe: &SignalPipe) {
        // dispatch reaches this only after consuming this current child's
        // terminal wait event. Retire its binding before any restart or reclaim.
        self.link
            .child_ended(&self.journal, self.child.build.version.as_deref());
        let ending = std::mem::replace(&mut self.ending, Ending::None);
        let moved = self.moved.take();
        match ending {
            Ending::ForUpdate { sent, plan }
                if sent.elapsed() <= Duration::from_millis(self.setup.opts.term_grace_ms) =>
            {
                self.await_update(UpdateLaunch::Target(event), plan);
                self.resume_update(pipe);
            }
            // The restart stands until the child has ended or was abandoned
            // after SIGKILL ([`Supervisor::look`]): an end that comes late is
            // still the one the harness asked for.
            Ending::ForRestart {
                plan,
                cause,
                selected_args,
                ..
            } => {
                self.relaunch(event, plan, &Cause::Asked(cause), pipe);
                if let Some(args) = selected_args {
                    self.setup.args = args;
                }
            }
            Ending::None
                if let Some(plan) = moved
                    && now_ms().saturating_sub(self.child.spawned_ms) <= STARTUP_GRACE_MS
                    && self.record().is_none()
                    && self.terminal_is_original()
                    && refused_start(event) =>
            {
                self.await_update(
                    UpdateLaunch::Fallback(FallbackCause::EarlyExit(event)),
                    plan,
                );
                self.resume_update(pipe);
            }
            Ending::None => {
                if !self.crash_begins(event) {
                    self.end_like(event);
                }
            }
            _ => self.end_like(event),
        }
    }

    /// End the harness the way the child ended (`event`): the saved terminal
    /// modes restored after a signal death, the claim given back, the same
    /// exit code or the same signal.
    fn end_like(&mut self, event: ChildEvent) -> ! {
        if matches!(event, ChildEvent::Signaled(_))
            && let Some(t) = &self.termios
        {
            // A child killed by a signal ran no cleanup: its keyboard
            // reporting is still on, under which the shell's Ctrl-C is no
            // `SIGINT`.
            t.restore();
            t.reset_input_modes();
        }
        self.image.finish_probe();
        self.journal.note(&format!("exit child={event:?}"));
        self.link_release();
        sys::exit_like(event)
    }

    /// The child's own end (`event`, with nothing asked of it): whether it is
    /// a crash the harness now answers. `true` when [`Ending::Crashed`] was
    /// entered, which waits [`EXIT_SETTLE`] for a graceful exit to finish
    /// removing its record before anything else is decided
    /// ([`Supervisor::crash_tick`]). `false` when the end is the session's own
    /// and the harness ends with it, as it does outside aterm:
    ///
    /// * the end is not abnormal ([`abnormal_exit`]);
    /// * the session never had a claim ([`link::Link::claims`]), so the
    ///   harness has no duty towards the child and journals nothing;
    /// * an own-image move is in flight, which a child that ends has always
    ///   ended;
    /// * the child left no session record: it removed it, as a graceful exit
    ///   does, or never wrote one, and there is no conversation to resume.
    fn crash_begins(&mut self, event: ChildEvent) -> bool {
        if !abnormal_exit(event) || !self.link.claims() {
            return false;
        }
        if self.image.blocks_vendor_move() {
            self.journal
                .note(&format!("crash-passed-on own-image-move child={event:?}"));
            return false;
        }
        let Some(record) = self.record() else {
            self.journal
                .note(&format!("crash-passed-on no-record child={event:?}"));
            return false;
        };
        if let Some(deferred) = self.restart_deferred.take() {
            self.journal.note(&format!(
                "aterm restart cause={} ignored child-ended",
                deferred.restart.cause
            ));
        }
        let now = now_ms();
        let settle = u64::try_from(EXIT_SETTLE.as_millis()).unwrap_or(u64::MAX);
        self.journal.note(&format!(
            "crash child={event:?} session={} settle-ms={settle}",
            record.session_id
        ));
        // A stop recorded before the end is over with the child.
        self.child.stopped = false;
        self.ending = Ending::Crashed {
            event,
            crash: Crash {
                record,
                exit_ms: now,
                due_ms: now,
                resume: Vec::new(),
                reclaim_spent_ms: 0,
                reclaim_since_ms: None,
                ask_at_ms: now,
                told: false,
                wait: CrashWait::Settle {
                    until_ms: now.saturating_add(settle),
                },
            },
        };
        // No child owns the terminal from here, and the crashed one left its
        // modes: raw, and its keyboard reporting, under which no key raises a
        // signal. They go back to what the harness found for the whole of the
        // settle, any wait for aterm and the pause, so Ctrl-C ends them.
        if let Some(t) = &self.termios {
            t.restore();
            t.reset_input_modes();
        }
        true
    }

    /// When (epoch ms) the crash being answered next needs the loop, if one is.
    fn crash_wake_ms(&self) -> Option<u64> {
        let Ending::Crashed { crash, .. } = &self.ending else {
            return None;
        };
        match crash.wait {
            CrashWait::Settle { until_ms } => Some(until_ms),
            // A lost link is asked nothing before it is claimed again, which
            // the poll's own clock tries ([`Supervisor::link_tick`]): its
            // `ask_at_ms` would be a time long past, and the loop would spin.
            CrashWait::Claim if self.link.lost() => crash.reclaim_until_ms(),
            CrashWait::Claim => crash
                .reclaim_until_ms()
                .map(|until| until.min(crash.ask_at_ms)),
            CrashWait::Pause => Some(crash.due_ms),
        }
    }

    /// The crash being answered, to change.
    fn crash_mut(&mut self) -> Option<&mut Crash> {
        match &mut self.ending {
            Ending::Crashed { crash, .. } => Some(crash),
            _ => None,
        }
    }

    fn crash_record(&self, crash: &Crash) -> Option<SessionRecord> {
        self.record()
            .filter(|current| crash.record.same_child_conversation(current))
    }

    /// The crash is not answered with a relaunch (`why`, journalled as
    /// `crash-passed-on <why>`): the harness ends as the crashed child did.
    /// What was typed at the dead screen is discarded first: it was meant for
    /// the child, and the shell that reads the terminal next would run it.
    fn crash_passed_on(&mut self, event: ChildEvent, why: &str) -> ! {
        self.journal.note(&format!("crash-passed-on {why}"));
        if let Some(t) = &self.termios {
            t.discard_input();
        }
        self.end_like(event)
    }

    /// A session-ending signal while no child runs cancels the replacement.
    fn childless_signalled(&mut self, sig: libc::c_int) {
        if let Ending::Crashed { event, .. } = self.ending {
            self.crash_passed_on(event, &format!("signal {sig}"));
        }
        if self.awaiting_update() {
            self.cancel_replacement(sig);
        }
    }

    /// One step of answering a crash, on every pass of the loop while no child
    /// runs. It never waits: each state is left when its time has come.
    ///
    /// 1. [`CrashWait::Settle`]: once [`EXIT_SETTLE`] has passed, the record
    ///    must still stand ([`Supervisor::crash_settled`]).
    /// 2. [`CrashWait::Claim`]: the link was away, or aterm could answer only
    ///    later. Once it is held again (and, after a "later", a second has
    ///    passed), or the hold has run out, aterm is asked again.
    /// 3. [`CrashWait::Pause`]: once the back-off's pause is over, aterm is
    ///    asked once more and the same build is started
    ///    ([`Supervisor::crash_due`]).
    ///
    /// An own-image move in flight ends the wait at any step: the crash is
    /// passed on.
    fn crash_tick(&mut self, pipe: &SignalPipe) {
        let Ending::Crashed { event, crash } = &self.ending else {
            return;
        };
        let (event, wait, due_ms, reclaim_until_ms, ask_at_ms) = (
            *event,
            crash.wait,
            crash.due_ms,
            crash.reclaim_until_ms(),
            crash.ask_at_ms,
        );
        if self.image.blocks_vendor_move() {
            self.crash_passed_on(event, "own-image-move");
        }
        let now = now_ms();
        match wait {
            CrashWait::Settle { until_ms } => {
                if now >= until_ms {
                    self.crash_settled(event);
                }
            }
            CrashWait::Claim => {
                // The loop's own clock claims a lost link again
                // ([`Supervisor::link_tick`]); nothing is asked before it has.
                if (!self.link.lost() && now >= ask_at_ms)
                    || reclaim_until_ms.is_none_or(|until| now >= until)
                {
                    self.crash_judged(event);
                }
            }
            CrashWait::Pause => {
                if now >= due_ms {
                    self.crash_due(event, pipe);
                }
            }
        }
    }

    /// [`EXIT_SETTLE`] after the child's end: the record must still stand (a
    /// graceful exit has removed its own by now), and the conversation must be
    /// resumable here. Then the back-off gives the pause, and aterm is asked.
    fn crash_settled(&mut self, event: ChildEvent) {
        let Ending::Crashed { crash, .. } = &self.ending else {
            return;
        };
        let exit_ms = crash.exit_ms;
        let Some(record) = self.crash_record(crash) else {
            self.crash_passed_on(event, "record-removed");
        };
        let resume = match self.resume_plan(&record, &self.setup.args) {
            Ok(resume) => resume,
            Err(why) => self.crash_passed_on(event, &why),
        };
        let pause = u64::try_from(self.ladder.crashed(exit_ms).as_millis()).unwrap_or(u64::MAX);
        self.journal.note(&format!(
            "crash record-stands session={} streak={} pause-ms={pause}",
            record.session_id,
            self.ladder.streak()
        ));
        if let Some(crash) = self.crash_mut() {
            crash.resume = resume;
            crash.due_ms = exit_ms.saturating_add(pause);
        }
        self.crash_judged(event);
    }

    /// Ask aterm whether the crashed child may be brought back
    /// ([`Supervisor::link_relaunch_word`]) and act on the answer; `true` when
    /// it may, with the crash left in [`CrashWait::Pause`].
    ///
    /// * Permitted: the pause runs, and the terminal is told once that the
    ///   relaunch is coming (the terminal's modes came back when the crash
    ///   began, [`Supervisor::crash_begins`]).
    /// * Denied: the crash is passed on.
    /// * The link is away, or aterm can answer only later: the crash waits in
    ///   [`CrashWait::Claim`] for the claim to be proven again or for aterm to
    ///   answer, for what is left of [`link::RECLAIM_HOLD_MS`] after this
    ///   crash's earlier waits, and is then passed on.
    ///
    /// A harness that is not the terminal's foreground group starts nothing
    /// on that terminal, and passes the crash on.
    fn crash_judged(&mut self, event: ChildEvent) -> bool {
        if !sys::in_foreground(0) {
            self.crash_passed_on(event, "background");
        }
        let word = self.link_relaunch_word(false);
        let now = now_ms();
        let version = self.child.build.version.clone();
        let Some(crash) = self.crash_mut() else {
            return false;
        };
        match crash.heard(word, now) {
            Heard::Pause { tell } => {
                if tell {
                    say(&pause_notice(
                        version.as_deref(),
                        event,
                        crash.due_ms.saturating_sub(now),
                    ));
                }
                true
            }
            Heard::Wait(line) => {
                if let Some(line) = line {
                    self.journal.note(&line);
                }
                false
            }
            Heard::PassOn { waited, why } => {
                if let Some(line) = waited {
                    self.journal.note(&line);
                }
                self.crash_passed_on(event, &why)
            }
        }
    }

    /// The back-off's pause is over: ask aterm once more, pin the plan, and
    /// start the same build on the same conversation through
    /// [`Supervisor::relaunch`]. A plan that cannot be pinned, or a build that
    /// cannot be started, passes the crash on, and so does a session signal
    /// caught while aterm was asked, the plan pinned or the pre-spawn
    /// handshake completed. The original conversation must still stand
    /// through those steps. What was typed at the dead screen is discarded
    /// before the start.
    fn crash_due(&mut self, event: ChildEvent, pipe: &SignalPipe) {
        if !self.crash_judged(event) {
            return;
        }
        let Ending::Crashed { crash, .. } = &self.ending else {
            return;
        };
        let plan = match self.same_build_plan(crash.record.session_id.clone(), crash.resume.clone())
        {
            Ok(plan) => plan,
            Err(why) => self.crash_passed_on(event, &why),
        };
        if let Some(sig) = session_ending(&pipe.wait(0)) {
            self.crash_passed_on(event, &format!("signal {sig}"));
        }
        if self.crash_record(crash).is_none() {
            self.crash_passed_on(event, "record-removed");
        }
        if let Some(t) = &self.termios {
            t.discard_input();
        }
        self.relaunch(event, plan, &Cause::Crash, pipe);
        self.ladder.relaunched(now_ms());
        self.journal.note(&format!(
            "crash-relaunch pid={} build={} streak={}",
            self.child.pid,
            self.child.build.number,
            self.ladder.streak()
        ));
    }

    /// The pinned plan that starts the running build again on `session_id`
    /// with `resume`: a [`vendor::Plan`] whose target is its old.
    fn same_build_plan(
        &self,
        session_id: String,
        resume: Vec<String>,
    ) -> Result<vendor::Plan, String> {
        let original_terminal = self
            .termios
            .as_ref()
            .map(Termios::snapshot)
            .transpose()
            .map_err(|error| format!("vendor-plan original-terminal {error}"))?;
        let build = self.child.build.clone();
        // The capture revalidates everything it pinned before it returns.
        vendor::Plan::capture(
            build.clone(),
            build,
            session_id,
            resume,
            self.setup.depth,
            original_terminal,
        )
        .map_err(|error| format!("vendor-plan {error}"))
    }

    /// aterm asked the claim holder to restart its child (`restart
    /// cause=<cause>`): end the child with SIGTERM, now, and start the same
    /// build on the same conversation when it has ended
    /// ([`Ending::ForRestart`]; a child still running [`RESTART_GRACE_MS`]
    /// later is sent SIGKILL, [`Supervisor::look`]). The request is aterm's, or
    /// a person's through aterm, so no safe point is waited for; it is no
    /// crash, and the back-off is not touched.
    ///
    /// It is made only when all of these hold, and is otherwise journalled as
    /// `aterm restart cause=<cause> ignored <why>`:
    ///
    /// * nothing else is ending or has ended the child, and the child has not
    ///   ended unreaped (`child-ended`);
    /// * the conversation's resume argv can be written;
    /// * one fresh policy and status read shows this harness's proven claim
    ///   taking `relaunch`, and the session neither halted nor driven
    ///   ([`Supervisor::link_relaunch_word`], [`alab_wire::relaunch::permitted`]);
    /// * the plan can be pinned, the child still names the same conversation,
    ///   and the kernel accepts the signal.
    ///
    /// With no session record yet (the child has not written one), while the
    /// child is stopped, or while an own-image move is in flight, the request
    /// waits ([`Supervisor::restart_deferred`], journalled once as `deferred
    /// <why>`) and is made on a later pass ([`Supervisor::restart_retry`]). So
    /// does one aterm could not answer now, or whose link is being claimed
    /// again, asked again every [`link::LATER_RETRY_MS`] and for at most
    /// [`link::RECLAIM_HOLD_MS`]: aterm has already answered the asker that
    /// the restart is under way. An already selected conversation stays
    /// bound to the request as it waits.
    fn restart_asked(&mut self, restart: &Restart) {
        if restart.account.is_some() {
            return self.restart_ignored(&restart.cause, "account-assignment-unsupported");
        }
        self.restart_for(restart, None);
    }

    fn restart_for(&mut self, restart: &Restart, bound: Option<SessionRecord>) {
        let cause = &restart.cause;
        let before = self.restart_deferred.take();
        let refusal = match &self.ending {
            Ending::None if sys::child_has_ended(self.child.pid) => Some("child-ended"),
            Ending::None => None,
            Ending::ForUpdate { .. } | Ending::AwaitingUpdate { .. } => Some("update-ending"),
            Ending::ForRestart { .. } => Some("restart-ending"),
            Ending::Passed(_) => Some("session-ending"),
            Ending::Crashed { .. } => Some("crash-pending"),
        };
        if let Some(why) = refusal {
            self.restart_ignored(cause, why);
            return;
        }
        if self.image.blocks_vendor_move() {
            return self.restart_waits(restart, bound, before, "own-image-move", false);
        }
        if self.child.stopped {
            return self.restart_waits(restart, bound, before, "child-stopped", false);
        }
        let record = match self.record() {
            Some(record)
                if bound
                    .as_ref()
                    .is_none_or(|bound| bound.same_child_conversation(&record)) =>
            {
                record
            }
            None if bound.is_none() => {
                return self.restart_waits(restart, None, before, "no-record", false);
            }
            _ => return self.restart_ignored(cause, "record-changed"),
        };
        let selected = match &restart.model {
            Some(model) => {
                match crate::launch::override_model(
                    self.setup.args.clone(),
                    model.clone(),
                    self.setup.args.len(),
                ) {
                    Ok(args) => Some(args),
                    Err(why) => return self.restart_ignored(cause, &why),
                }
            }
            None => None,
        };
        let launch_args = selected.as_deref().unwrap_or(&self.setup.args);
        let resume = match self.resume_plan(&record, launch_args) {
            Ok(resume) => resume,
            Err(why) => return self.restart_ignored(cause, &why),
        };
        match self.link_relaunch_word(true) {
            link::Relaunch::Permitted => {}
            link::Relaunch::Denied(why) => return self.restart_ignored(cause, &why),
            link::Relaunch::Away(why) | link::Relaunch::Later(why) => {
                return self.restart_waits(
                    restart,
                    Some(record),
                    before,
                    &format!("aterm {why}"),
                    true,
                );
            }
        }
        let plan = match self.same_build_plan(record.session_id.clone(), resume) {
            Ok(plan) => plan,
            Err(why) => return self.restart_ignored(cause, &why),
        };
        if sys::child_has_ended(self.child.pid) {
            return self.restart_ignored(cause, "child-ended");
        }
        if !self
            .record()
            .is_some_and(|current| record.same_child_conversation(&current))
        {
            return self.restart_ignored(cause, "record-changed");
        }
        if !self.signal_child(libc::SIGTERM) {
            return self.restart_ignored(cause, "signal-failed");
        }
        self.journal.note(&format!(
            "aterm restart cause={cause} SIGTERM pid={} session={} build={} argv={}",
            self.child.pid,
            record.session_id,
            self.child.build.number,
            plan.resume.join("\u{1f}")
        ));
        self.ending = Ending::ForRestart {
            sent: Instant::now(),
            plan,
            cause: cause.clone(),
            selected_args: selected,
            killed: false,
        };
    }

    /// Keep the restart request for a later pass because of `why`,
    /// journalled as `aterm restart cause=<cause> deferred <why>` unless that
    /// is what was waiting already (`before`). A request that waits on aterm
    /// (`on_aterm`) is asked again after [`link::LATER_RETRY_MS`], and once it
    /// has waited [`link::RECLAIM_HOLD_MS`] in all it is ignored.
    fn restart_waits(
        &mut self,
        restart: &Restart,
        record: Option<SessionRecord>,
        before: Option<Deferred>,
        why: &str,
        on_aterm: bool,
    ) {
        let cause = &restart.cause;
        let now = now_ms();
        let same = before
            .as_ref()
            .filter(|before| before.restart == *restart && before.why == why);
        let on_aterm_since_ms = on_aterm.then(|| {
            before
                .as_ref()
                .filter(|before| before.restart.cause == *cause)
                .and_then(|before| before.on_aterm_since_ms)
                .unwrap_or(now)
        });
        if on_aterm_since_ms.is_some_and(|since| now.saturating_sub(since) >= link::RECLAIM_HOLD_MS)
        {
            return self.restart_ignored(cause, why);
        }
        if same.is_none() {
            self.journal
                .note(&format!("aterm restart cause={cause} deferred {why}"));
        }
        self.restart_deferred = Some(Deferred {
            restart: restart.clone(),
            record,
            why: why.to_string(),
            on_aterm_since_ms,
            retry_at_ms: if on_aterm {
                now.saturating_add(link::LATER_RETRY_MS)
            } else {
                now
            },
        });
    }

    /// Journal a restart request that is not made: `aterm restart
    /// cause=<cause> ignored <why>`.
    fn restart_ignored(&self, cause: &str, why: &str) {
        self.journal
            .note(&format!("aterm restart cause={cause} ignored {why}"));
    }

    /// Make the restart that was waiting, if one is and its time has come
    /// ([`Supervisor::restart_asked`]).
    fn restart_retry(&mut self) {
        let now = now_ms();
        if let Some((restart, bound)) = self
            .restart_deferred
            .as_ref()
            .filter(|deferred| now >= deferred.retry_at_ms)
            .map(|deferred| (deferred.restart.clone(), deferred.record.clone()))
        {
            self.restart_for(&restart, bound);
        }
    }

    /// Handle signals caught after the old child was reaped. A stop is for
    /// the job the replacement joins and goes back to the regular loop.
    fn replacement_signals(&mut self, pipe: &SignalPipe) {
        let queued = pipe.wait(0);
        if let Some(sig) = session_ending(&queued) {
            self.cancel_replacement(sig);
        }
        if queued.contains(&libc::SIGTSTP) {
            pipe.defer_stop();
        }
    }

    fn cancel_replacement(&mut self, sig: libc::c_int) -> ! {
        self.journal
            .note(&format!("replacement-cancelled signal={sig}"));
        if let Some(t) = &self.termios {
            t.discard_input();
        }
        self.end_like(ChildEvent::Signaled(sig));
    }

    fn await_update(&mut self, launch: UpdateLaunch, plan: vendor::Plan) {
        self.child.stopped = false;
        self.ending = Ending::AwaitingUpdate {
            launch,
            plan: Some(plan),
        };
    }

    fn stop_pending_update(&self) {
        self.journal
            .note("replacement-stopped waiting for foreground");
        sys::stop_self();
    }

    fn resume_update(&mut self, pipe: &SignalPipe) {
        let Ending::AwaitingUpdate { launch, plan } = &mut self.ending else {
            return;
        };
        if !sys::in_foreground(0) {
            self.note_wait("replacement-wait foreground");
            return;
        }
        let launch = *launch;
        let plan = plan.take().expect("automatic replacement owns its plan");
        match launch {
            UpdateLaunch::Target(event) => self.relaunch(event, plan, &Cause::Update, pipe),
            UpdateLaunch::Fallback(why) => self.fall_back(why, plan, pipe),
        }
    }

    /// Start a pinned launch, or keep an automatic replacement pending when
    /// its job loses foreground. Only a real start error permits fallback.
    fn start_launch(
        &mut self,
        plan: &vendor::Plan,
        launch: &vendor::Launch,
        pipe: &SignalPipe,
    ) -> std::io::Result<Option<Child>> {
        let crash = matches!(self.ending, Ending::Crashed { .. });
        let pinned = plan.revalidate_for(launch);
        if !crash {
            self.replacement_signals(pipe);
        }
        if self.awaiting_update() && !sys::in_foreground(0) {
            return Ok(None);
        }
        pinned?;
        // All callers arrive after the prior child's wait event was consumed.
        // The server also refuses a begin while its named old child is alive.
        self.link
            .before_spawn(&self.journal, launch.build.version.as_deref());
        let pinned = plan.revalidate_for(launch);
        let mut command = launch.command();
        if !crash {
            self.replacement_signals(pipe);
        }
        if self.awaiting_update() && !sys::in_foreground(0) {
            return Ok(None);
        }
        pinned?;
        if let Ending::Crashed { event, crash } = &self.ending {
            let record_stands = self.crash_record(crash).is_some();
            if let Some(sig) = session_ending(&pipe.wait(0)) {
                self.crash_passed_on(*event, &format!("signal {sig}"));
            }
            if !record_stands {
                self.crash_passed_on(*event, "record-removed");
            }
            if !sys::in_foreground(0) {
                self.crash_passed_on(*event, "background");
            }
        }
        let spawned_ms = now_ms();
        let proc = command.spawn()?;
        launched(
            proc,
            &launch.build,
            &launch.args,
            &self.setup.opts,
            spawned_ms,
        )
        .map(Some)
    }

    /// The child ended (`event`) and is started again from `plan` on the same
    /// conversation, in this process: on the plan's target build after an
    /// update, on the same build after a restart aterm asked for or a crash
    /// (`cause`). The terminal is told of the resume ([`relaunch_notice`],
    /// [`restart_notice`], [`crash_notice`]) only once the build has been
    /// started. A same-build start that fails ends the harness: with the
    /// crashed child's status after a crash, and otherwise with
    /// [`EXIT_VENDOR_MISSING`], because the child ended at the harness's own
    /// signal and its status would say nothing.
    fn relaunch(
        &mut self,
        event: ChildEvent,
        plan: vendor::Plan,
        cause: &Cause,
        pipe: &SignalPipe,
    ) {
        let (old, new, session_id, resume) = (
            &plan.old.build,
            &plan.target.build,
            &plan.session_id,
            &plan.resume,
        );
        if let Some(t) = &self.termios {
            t.restore();
            // A child killed by a signal (a restart's SIGKILL) left its
            // keyboard reporting on, for whatever reads the terminal next:
            // the child started here, or, if none can be, the shell.
            if matches!(event, ChildEvent::Signaled(_)) {
                t.reset_input_modes();
            }
        }
        // What aterm sent before this point was sent for the child that ended
        // (a poll a signal interrupted reports no event, so it may still be
        // unread): a restart request among it is not taken for the next one.
        self.link_events(true);
        self.journal.note(&format!(
            "relaunch child={event:?} from-build={} to-build={} from={} to={} session={session_id} argv={}",
            old.number,
            new.number,
            old.version.as_deref().unwrap_or("-"),
            new.version.as_deref().unwrap_or("-"),
            resume.join("\u{1f}")
        ));
        let started = match self.start_launch(&plan, &plan.target, pipe) {
            Ok(Some(child)) => {
                // Said only now: the line claims a resume on the new build, so
                // the new build has to have been started. A target that could
                // not be spawned gets the fallback's own line instead.
                say(&match cause {
                    Cause::Update => {
                        relaunch_notice(old.version.as_deref(), new.version.as_deref(), session_id)
                    }
                    Cause::Asked(why) => restart_notice(new.version.as_deref(), why, session_id),
                    Cause::Crash => crash_notice(new.version.as_deref(), event, session_id),
                });
                child
            }
            Ok(None) => {
                self.await_update(UpdateLaunch::Target(event), plan);
                self.note_wait("replacement-wait foreground");
                return;
            }
            Err(e) => {
                self.journal
                    .note(&format!("relaunch-failed build={} {e}", new.number));
                if new.number == old.number {
                    if matches!(cause, Cause::Crash) {
                        say(&format!("{TOOL}: cannot restart {COMMAND}: {e}"));
                        self.crash_passed_on(event, &format!("relaunch-failed {e}"));
                    }
                    self.image.finish_probe();
                    say(&format!("{TOOL}: cannot restart {COMMAND}: {e}"));
                    self.journal.note(&format!("exit relaunch-failed {e}"));
                    self.link_release();
                    std::process::exit(EXIT_VENDOR_MISSING);
                }
                self.fall_back(FallbackCause::FailedSpawn, plan, pipe);
                return;
            }
        };
        let moved = (new.number != old.number).then_some(plan);
        self.install_replacement(started, moved);
    }

    fn install_replacement(&mut self, child: Child, moved: Option<vendor::Plan>) {
        self.child = child;
        self.ending = Ending::None;
        self.moved = moved;
        self.noted.clear();
        self.seen_target = None;
        self.restart_moot();
        self.link_reannounce();
    }

    /// The child was just replaced: a restart that was waiting for the old
    /// one's record has nothing left to restart.
    fn restart_moot(&mut self) {
        if let Some(deferred) = self.restart_deferred.take() {
            self.restart_ignored(&deferred.restart.cause, "child-replaced");
        }
    }

    /// The moved build `plan.target` refused to start (Decision 28): remember it
    /// as abandoned and start `plan.old` on the same conversation. `cause` is
    /// `child=<event>` for a start that ended, `spawn-failed` for one that never
    /// ran. The notice and the `relaunch-fallback` journal line say "resuming"
    /// and are written only once the previous build has been started. When it
    /// cannot be started either, one line says that both failed and why, the
    /// journal records `relaunch-fallback-failed`, and the harness exits: no
    /// build is left to run the session on.
    fn fall_back(&mut self, why: FallbackCause, plan: vendor::Plan, pipe: &SignalPipe) {
        if let Some(t) = &self.termios {
            t.restore();
        }
        self.link_events(true);
        let (old, new, session_id) = (&plan.old.build, &plan.target.build, &plan.session_id);
        self.abandoned.remember(new.number);
        let cause = match why {
            FallbackCause::FailedSpawn => "spawn-failed".to_string(),
            FallbackCause::EarlyExit(event) => format!("child={event:?}"),
        };
        match self.start_launch(&plan, &plan.old, pipe) {
            Ok(Some(child)) => {
                say(&fallback_notice(
                    new.version.as_deref(),
                    old.version.as_deref(),
                    session_id,
                    None,
                ));
                self.journal.note(&format!(
                    "relaunch-fallback {cause} failed-build={} fallback-build={} session={session_id}",
                    new.number, old.number
                ));
                self.install_replacement(child, None);
            }
            Ok(None) => {
                self.await_update(UpdateLaunch::Fallback(why), plan);
                self.note_wait("replacement-wait foreground");
            }
            Err(e) => {
                self.image.finish_probe();
                say(&fallback_notice(
                    new.version.as_deref(),
                    old.version.as_deref(),
                    session_id,
                    Some(&e),
                ));
                self.journal.note(&format!(
                    "relaunch-fallback-failed {cause} failed-build={} fallback-build={} session={session_id} {e}",
                    new.number, old.number
                ));
                self.journal.note(&format!("exit relaunch-failed {e}"));
                self.link_release();
                std::process::exit(EXIT_VENDOR_MISSING);
            }
        }
    }

    /// One look at the store and the child: keep the safe-point witnesses
    /// current, and end the child for an update when it is safe.
    ///
    /// The harness's own tracker is fed on every look, whatever decides, so a
    /// look that falls back to it has its history. While a proven aterm claim
    /// stands ([`link::Link::judges`]), aterm's word replaces three things and
    /// no others: the tracker's gate (the record must still read idle for the
    /// quiet period, [`crate::quiet::record_idle`]), the unread-input check,
    /// and the unread-input and input-clock part of the final re-read. The
    /// terminal input is judged by one fresh policy and status read
    /// ([`Supervisor::link_safe_point`]). A link that fails at that step is
    /// lost, and the same look decides on the three local checks instead.
    /// The pinned plan, idle conversation and selected build must still
    /// stand after that exchange and before SIGTERM.
    fn look(&mut self, signals_waiting: impl FnOnce() -> bool) {
        if let Ending::ForUpdate { sent, plan } = &self.ending
            && sent.elapsed() > Duration::from_millis(self.setup.opts.term_grace_ms)
        {
            let target = plan.target.build.number;
            self.journal.note(&format!(
                "update-abandoned the child kept running after SIGTERM; build={target} is not tried again"
            ));
            self.abandoned.remember(target);
            self.ending = Ending::None;
        }
        self.restart_escalate();
        if self.ending != Ending::None {
            return;
        }
        let now = now_ms();
        let record = self.record();
        self.read_facts(record.as_ref(), now);
        if self.moved.is_some()
            && (record.is_some()
                || !self.terminal_is_original()
                || now.saturating_sub(self.child.spawned_ms) > STARTUP_GRACE_MS)
        {
            self.moved = None;
        }
        let input = sys::stdin_input_ms();
        let gate = self.child.tracker.look(&Look {
            now_ms: now,
            status: record
                .as_ref()
                .map(|r| (r.status.as_str(), r.status_updated_at_ms)),
            input_ms: input,
        });
        let Some(current) = self.setup.store.current_number() else {
            return;
        };
        if current == self.child.build.number {
            return;
        }
        // aterm's last safe-point answer was a hold and the link was lost
        // since: nothing moves on the harness's own readings over it before
        // the claim is proven again.
        if let Some(held) = self.link.lost_hold() {
            self.note_wait(&held);
            return;
        }
        let claimed = self.link.judges();
        if claimed {
            // An aterm hold stands until an event or its own clock; `status` is
            // not read again before then, and neither is anything slower.
            if self.link.held_back(now) {
                return;
            }
            let status = record
                .as_ref()
                .map(|r| (r.status.as_str(), r.status_updated_at_ms));
            if let Err(wait) = crate::quiet::record_idle(status, now, self.setup.opts.quiet_ms) {
                self.note_wait(&format!("wait {}", wait.word()));
                return;
            }
        } else if let Err(wait) = gate {
            self.note_wait(&format!("wait {}", wait.word()));
            return;
        }
        if !sys::in_foreground(0) {
            self.note_wait("wait background");
            return;
        }
        if !claimed && self.unread_input_holds() {
            return;
        }
        let Some(record) = record else {
            self.note_wait(&format!("wait {}", Wait::NoRecord.word()));
            return;
        };
        // Keep the record/input history above even with no pending update:
        // losing the aterm claim must not forget whether a turn was submitted
        // over an unsent draft. A target's ready/provenance/environment files
        // and executable need no reads while these cheaper gates already hold.
        // Read them fresh here, with the final current/record/input recheck
        // below still immediately ahead of SIGTERM.
        let target = match self.setup.store.current(self.setup.own_exe.as_deref()) {
            Ok(b) if b.number != self.child.build.number => b,
            Ok(_) => return,
            Err(missing) => {
                self.note_wait(&format!("update-unusable {missing}"));
                return;
            }
        };
        match self.abandoned.refusal(target.number) {
            Some(AbandonedRefusal::Build) => {
                self.note_wait(&format!("wait abandoned build={}", target.number));
                return;
            }
            Some(AbandonedRefusal::Full) => {
                self.note_wait(&format!(
                    "wait abandoned-limit build={} is not tried: {MAX_ABANDONED} abandoned builds are remembered and none is forgotten",
                    target.number
                ));
                return;
            }
            None => {}
        }
        if self.seen_target != Some(target.number) {
            self.seen_target = Some(target.number);
            self.journal.note(&format!(
                "update-seen build={} version={} running-build={}",
                target.number,
                target.version.as_deref().unwrap_or("-"),
                self.child.build.number
            ));
        }
        let resume = match self.resume_plan(&record, &self.setup.args) {
            Ok(resume) => resume,
            Err(why) => {
                self.note_wait(&format!("wait {why}"));
                return;
            }
        };
        if let Err(why) = self.no_background_work() {
            self.note_wait(&format!("wait {why}"));
            return;
        }
        let original_terminal = match self.termios.as_ref().map(Termios::snapshot).transpose() {
            Ok(snapshot) => snapshot,
            Err(error) => {
                self.note_wait(&format!("wait vendor-plan original-terminal {error}"));
                return;
            }
        };
        let plan = match vendor::Plan::capture(
            self.child.build.clone(),
            target,
            record.session_id.clone(),
            resume,
            self.setup.depth,
            original_terminal,
        ) {
            Ok(plan) => plan,
            Err(error) => {
                self.note_wait(&format!("wait vendor-plan {error}"));
                return;
            }
        };
        let target = plan.target.build.number;
        let check_local_input = if claimed {
            if !self.update_snapshot_matches(&record, target) {
                self.note_wait("wait changed-while-deciding");
                return;
            }
            match self.link_safe_point() {
                link::Verdict::Safe => false,
                link::Verdict::Hold(why) => {
                    self.note_wait(&why);
                    return;
                }
                link::Verdict::Local => {
                    // aterm did not answer, or the claim no longer stands: the
                    // three checks its word stood in for are made now, on this
                    // look's own readings.
                    if let Err(wait) = gate {
                        self.note_wait(&format!("wait {}", wait.word()));
                        return;
                    }
                    if self.unread_input_holds() {
                        return;
                    }
                    true
                }
            }
        } else {
            true
        };
        if let Err(error) = plan.revalidate_all() {
            self.note_wait(&format!("wait vendor-plan changed {error}"));
            return;
        }
        // The plan and the exchange took time. Re-read the local witnesses
        // that the plan depends on. If input is ours to check, read the queue
        // before the clock so an already queued or newly read key holds.
        if !self.update_snapshot_matches(&record, target)
            || (check_local_input
                && (!sys::stdin_unread().is_ok_and(|queued| queued == 0)
                    || sys::stdin_input_ms() != input))
        {
            self.note_wait("wait changed-while-deciding");
            return;
        }
        // Let the loop act on the queued signals with this vendor still in
        // place. Leave the vendor's wait events for that loop to reap.
        if signals_waiting() {
            return;
        }
        if sys::child_has_stopped_or_ended(self.child.pid) {
            self.note_wait("wait changed-while-deciding");
            return;
        }
        if !sys::in_foreground(0) {
            self.note_wait("wait background");
            return;
        }
        if self.term_for_update(target) {
            self.journal.note(&format!(
                "end-for-update SIGTERM pid={} session={} target-build={} argv={}",
                self.child.pid,
                record.session_id,
                target,
                plan.resume.join("\u{1f}")
            ));
            self.ending = Ending::ForUpdate {
                sent: Instant::now(),
                plan,
            };
        }
    }

    /// The conversation, its idle interval and the selected build the update
    /// plan was prepared for still stand.
    fn update_snapshot_matches(&self, record: &SessionRecord, target: u64) -> bool {
        self.record().is_some_and(|fresh| {
            record.same_child_conversation(&fresh)
                && fresh.status == profile::STATUS_IDLE
                && fresh.status_updated_at_ms == record.status_updated_at_ms
        }) && self.setup.store.current_number() == Some(target)
    }

    /// A restart's child that outlived its grace ([`RESTART_GRACE_MS`], or a
    /// shorter `--alab-term-grace-ms`): sent SIGKILL, as aterm's own restart
    /// act does (`restart_act.rs`, `spawn_escalation`), since a program that
    /// stalled may be one that cannot run its SIGTERM handler. SIGKILL goes to
    /// the child alone, never the group, which is the harness's own. A child
    /// still there a grace after SIGKILL, or one the kernel would not signal,
    /// is left running and the request is abandoned; no build is remembered
    /// against (the target was the build it runs).
    fn restart_escalate(&mut self) {
        let grace = self.setup.opts.term_grace_ms.min(RESTART_GRACE_MS);
        let Ending::ForRestart {
            sent,
            cause,
            killed,
            ..
        } = &self.ending
        else {
            return;
        };
        if sent.elapsed() <= Duration::from_millis(grace) {
            return;
        }
        let (cause, killed) = (cause.clone(), *killed);
        if !killed && self.signal_child(libc::SIGKILL) {
            self.journal.note(&format!(
                "restart-escalated cause={cause} SIGKILL pid={} the child kept running after SIGTERM",
                self.child.pid
            ));
            if let Ending::ForRestart { sent, killed, .. } = &mut self.ending {
                *sent = Instant::now();
                *killed = true;
            }
            return;
        }
        let after = if killed { "SIGKILL" } else { "SIGTERM" };
        self.journal.note(&format!(
            "restart-abandoned cause={cause} the child kept running after {after}"
        ));
        self.ending = Ending::None;
    }

    /// Whether typed input the vendor has not read yet holds the update, noting
    /// the wait when it does. Such input moves no input clock, and it becomes a
    /// draft as soon as the vendor reads it.
    fn unread_input_holds(&mut self) -> bool {
        match sys::stdin_unread() {
            Ok(0) => false,
            Ok(_) => {
                self.note_wait("wait unread-input");
                true
            }
            Err(error) => {
                self.note_wait(&format!("wait unread-input {error}"));
                true
            }
        }
    }

    /// Send the child the update's SIGTERM for target build `target`; `true`
    /// when the kernel accepted it. A signal that could not be delivered is
    /// journalled, once per change like every other wait, as `wait
    /// signal-failed build=<target>`. Nothing was sent, so the child's end is
    /// still its own, the target is not abandoned, and the next look decides
    /// again from the start.
    fn term_for_update(&mut self, target: u64) -> bool {
        let sent = self.signal_child(libc::SIGTERM);
        if !sent {
            self.note_wait(&format!("wait signal-failed build={target}"));
        }
        sent
    }

    /// `Ok` when no background work runs under the child ([`crate::procs`]);
    /// found work (or an unreadable table) holds for [`BACKGROUND_RECHECK_MS`]
    /// before the table is read again.
    fn no_background_work(&mut self) -> Result<(), String> {
        if now_ms() < self.background_hold_until {
            return Err(self.background_found.clone());
        }
        let pid = u32::try_from(self.child.pid).map_err(|_| "background-work pid".to_string())?;
        let work = crate::procs::live_background(pid);
        if work.is_empty() {
            return Ok(());
        }
        self.background_hold_until = now_ms().saturating_add(BACKGROUND_RECHECK_MS);
        self.background_found = format!("background-work {}", work.join(","));
        Err(self.background_found.clone())
    }

    /// The resume argv for `record`'s conversation, or why it cannot be resumed here.
    fn resume_plan(
        &self,
        record: &SessionRecord,
        args: &[OsString],
    ) -> Result<Vec<String>, String> {
        if !profile::is_session_id(&record.session_id) {
            return Err("unresumable session-id".to_string());
        }
        if !self.same_dir(&record.cwd) {
            // `--resume` finds a conversation by the directory it started in.
            return Err("unresumable cwd-moved".to_string());
        }
        let argv = launch_argv(args).ok_or("unresumable argv-not-utf8")?;
        let flags = profile::resume_argv(&argv, &record.session_id)
            .map_err(|r| format!("unresumable {r}"))?;
        // The conversation's own permission mode, as aterm's relaunch carries it
        // (aterm `relaunch.rs:481-507`, `last_permission_mode`/`resume_mode`):
        // a session switched to plan or default inside is not resumed in the
        // launch's bypass. No transcript or no mode line: the launch flags stand.
        let mode = profile::transcript_in(&self.config, &record.session_id)
            .and_then(|path| profile::transcript_tail(&path))
            .and_then(|tail| profile::permission_mode_of(&tail));
        Ok(profile::with_permission_mode(flags, mode.as_deref()))
    }

    fn same_dir(&self, recorded: &str) -> bool {
        let Some(cwd) = &self.cwd else { return false };
        let recorded = Path::new(recorded);
        if cwd == recorded {
            return true;
        }
        match (cwd.canonicalize(), recorded.canonicalize()) {
            (Ok(a), Ok(b)) => a == b,
            _ => false,
        }
    }

    /// Follow the model, effort level and top-effort key the child states in
    /// its transcript ([`model_facts::Reader::look`]), and journal them when
    /// they change (`facts model=… effort=… key=…`). Until the stream's
    /// `model` record exists the journal is their only output.
    fn read_facts(&mut self, record: Option<&SessionRecord>, now: u64) {
        let Ok(pid) = u32::try_from(self.child.pid) else {
            return;
        };
        let process = model_facts::Process {
            pid,
            record: record.map(|r| (r.session_id.as_str(), r.started_at_ms)),
            args: &self.child.args,
        };
        let update = self.facts.look(&self.config, &process, now);
        if let Some(line) = update.journal_line {
            self.journal.note(line);
        }
        self.link.model(update.facts, now);
    }

    /// The child's own record, when it is this child's (pid, start, kind).
    fn record(&self) -> Option<SessionRecord> {
        let pid = u32::try_from(self.child.pid).ok()?;
        SessionRecord::read(&self.config, pid).filter(|r| {
            r.pid == pid
                && r.kind == profile::KIND_INTERACTIVE
                && r.started_at_ms.saturating_add(RECORD_SLACK_MS) >= self.child.spawned_ms
        })
    }

    fn terminal_is_original(&self) -> bool {
        self.termios.as_ref().is_none_or(Termios::is_original)
    }

    fn note_wait(&mut self, why: &str) {
        if self.noted != why {
            self.noted = why.to_string();
            self.journal.note(why);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn os(words: &[&str]) -> Vec<OsString> {
        words.iter().map(OsString::from).collect()
    }

    fn restart(cause: &str) -> Restart {
        Restart {
            cause: cause.into(),
            account: None,
            model: None,
        }
    }

    #[test]
    fn a_launch_that_can_never_be_moved_is_named_from_the_launch_alone() {
        let refusal = |live_update: bool, on_tty: bool, words: &[&str]| {
            live_update_refusal(live_update, on_tty, &os(words))
        };
        // Movable: a terminal, no one-shot word, a command line that resumes.
        for words in [
            &[][..],
            &["--model", "x"],
            &["-c"],
            &["--resume", "0f0f0f0f"],
            &["--dangerously-skip-permissions", "fix it"],
            // After `--` every word is a prompt, whatever it looks like.
            &["--", "-p", "--worktree"],
        ] {
            assert_eq!(refusal(true, true, words), None, "{words:?}");
        }
        // Each reason, in the journal's words.
        for (words, why) in [
            (&["-p", "hi"][..], "one-shot"),
            (&["--print"], "one-shot"),
            (&["-h"], "one-shot"),
            (&["--help"], "one-shot"),
            (&["-v", "x"], "one-shot"),
            (&["--model", "x", "--version"], "one-shot"),
            (&["--worktree"], "unresumable not-resumable --worktree"),
            (&["-w", "tree"], "unresumable not-resumable -w"),
            (&["--tmux"], "unresumable not-resumable --tmux"),
            (&["--frob"], "unresumable unknown-flag --frob"),
        ] {
            assert_eq!(
                refusal(true, true, words).as_deref(),
                Some(why),
                "{words:?}"
            );
        }
        assert_eq!(refusal(true, false, &[]).as_deref(), Some("no-tty"));
        assert_eq!(refusal(false, true, &[]).as_deref(), Some("no-live-update"));
        // The first reason that holds is the one named: the option, then the
        // terminal, then the arguments (one-shot before the resume grammar,
        // which refuses `-p` too).
        assert_eq!(
            refusal(false, false, &["-p"]).as_deref(),
            Some("no-live-update")
        );
        assert_eq!(refusal(true, false, &["-p"]).as_deref(), Some("no-tty"));
        assert_eq!(
            refusal(true, true, &["-p", "--frob"]).as_deref(),
            Some("one-shot")
        );
    }

    #[test]
    fn non_utf8_arguments_are_never_resumed() {
        use std::os::unix::ffi::OsStringExt as _;
        let bad = vec![OsString::from_vec(vec![0x66, 0xff])];
        assert_eq!(launch_argv(&bad), None);
        assert_eq!(
            live_update_refusal(true, true, &bad).as_deref(),
            Some("argv-not-utf8")
        );
        // Wherever the argument stands, and before the one-shot words are read.
        let mixed = vec![OsString::from("-p"), OsString::from_vec(vec![0xff])];
        assert_eq!(
            live_update_refusal(true, true, &mixed).as_deref(),
            Some("argv-not-utf8")
        );
        assert_eq!(
            launch_argv(&os(&["a b", ""])),
            Some(vec!["claude".to_string(), "a b".to_string(), String::new()])
        );
    }

    #[test]
    fn every_abandoned_build_stays_refused_after_later_ones() {
        // `current` moves A -> B (abandoned) -> C (abandoned) -> B.
        let (b, c, d) = (20, 30, 40);
        let mut abandoned = Abandoned::default();
        assert_eq!(abandoned.refusal(b), None);
        abandoned.remember(b);
        assert_eq!(abandoned.refusal(c), None);
        abandoned.remember(c);
        assert_eq!(abandoned.refusal(b), Some(AbandonedRefusal::Build));
        assert_eq!(abandoned.refusal(c), Some(AbandonedRefusal::Build));
        assert_eq!(abandoned.refusal(d), None, "a build never tried re-arms");
        abandoned.remember(b);
        assert_eq!(abandoned.builds().collect::<Vec<_>>(), [b, c]);
    }

    #[test]
    fn a_full_abandoned_set_refuses_every_target_and_forgets_none() {
        let mut abandoned = Abandoned::default();
        let bound = u64::try_from(MAX_ABANDONED).unwrap();
        for build in 1..=bound {
            assert_eq!(abandoned.refusal(build), None, "{build}");
            abandoned.remember(build);
        }
        assert_eq!(abandoned.builds().len(), MAX_ABANDONED);
        assert_eq!(abandoned.refusal(bound + 1), Some(AbandonedRefusal::Full));
        // Were a further build abandoned all the same, nothing is evicted for it.
        abandoned.remember(bound + 1);
        assert_eq!(
            abandoned.builds().collect::<Vec<_>>(),
            (1..=bound).collect::<Vec<_>>()
        );
        assert_eq!(abandoned.refusal(1), Some(AbandonedRefusal::Build));
        assert_eq!(abandoned.refusal(bound + 1), Some(AbandonedRefusal::Full));
    }

    #[test]
    fn a_transferred_abandoned_set_is_bounded_and_strictly_ascending() {
        let bound = u64::try_from(MAX_ABANDONED).unwrap();
        let full: Vec<u64> = (1..=bound).collect();
        assert_eq!(
            Abandoned::from_ascending(&full)
                .unwrap()
                .builds()
                .collect::<Vec<_>>(),
            full
        );
        assert_eq!(Abandoned::from_ascending(&[]), Some(Abandoned::default()));
        let over: Vec<u64> = (1..=bound + 1).collect();
        assert_eq!(Abandoned::from_ascending(&over), None);
        assert_eq!(Abandoned::from_ascending(&[7, 7]), None);
        assert_eq!(Abandoned::from_ascending(&[8, 7]), None);
    }

    /// A supervisor over a child that cannot be signalled (pid 0 names no
    /// process; [`sys::kill`] refuses it), journalling to `state`.
    pub(super) fn supervisor_of_an_unsignallable_child(state: &Path) -> Supervisor {
        let opts = Options::default();
        let build = Build {
            number: 1,
            dir: "/signal-failed/vendor/1".into(),
            exe: "/signal-failed/vendor/1/bin/claude".into(),
            version: None,
            env: Vec::new(),
        };
        Supervisor {
            image: image::Runtime::default(),
            setup: Setup {
                home: "/signal-failed/home".into(),
                store: Store::new(Path::new("/signal-failed"), "claude", "claude"),
                own_exe: None,
                build: build.clone(),
                opts: opts.clone(),
                args: Vec::new(),
                depth: 0,
            },
            journal: Journal::new(Some(state), None, std::process::id()),
            config: "/signal-failed/home/.claude".into(),
            cwd: None,
            termios: None,
            child: Child {
                pid: 0,
                build,
                args: Vec::new(),
                identity: None,
                identity_pending: false,
                tracker: Tracker::new(1, opts.poll_ms, opts.quiet_ms),
                spawned_ms: 1,
                stopped: false,
            },
            ending: Ending::None,
            moved: None,
            noted: String::new(),
            seen_target: None,
            abandoned: Abandoned::default(),
            background_hold_until: 0,
            background_found: String::new(),
            link: link::Link::absent(),
            ladder: Ladder::new(),
            restart_deferred: None,
            facts: model_facts::Reader::default(),
        }
    }

    #[test]
    fn a_sigterm_that_cannot_be_delivered_is_journalled_once_and_retried() {
        let state = std::env::temp_dir().join(format!(
            "claude-alab-signal-failed-{}-{}",
            std::process::id(),
            now_ms()
        ));
        let mut sup = supervisor_of_an_unsignallable_child(&state);
        let journal = || {
            std::fs::read_to_string(sup_journal(&state))
                .unwrap_or_default()
                .lines()
                .map(|line| line.split_once(" pid=").map_or(line, |(_, rest)| rest))
                .map(|rest| rest.split_once(' ').map_or(rest, |(_, event)| event))
                .map(str::to_string)
                .collect::<Vec<_>>()
        };
        // The same failure on every look is one line, and nothing is latched.
        for _ in 0..3 {
            assert!(!sup.term_for_update(7));
        }
        assert_eq!(journal(), ["wait signal-failed build=7"]);
        assert_eq!(sup.ending, Ending::None, "no signal was delivered");
        assert_eq!(sup.abandoned, Abandoned::default(), "the target re-arms");
        // Another wait in between, or another target, is a change.
        sup.note_wait("wait busy");
        assert!(!sup.term_for_update(7));
        assert!(!sup.term_for_update(8));
        assert_eq!(
            journal(),
            [
                "wait signal-failed build=7",
                "wait busy",
                "wait signal-failed build=7",
                "wait signal-failed build=8",
            ]
        );
        let _ = std::fs::remove_dir_all(&state);
    }

    /// Where a journal under state home `state` is written.
    fn sup_journal(state: &Path) -> PathBuf {
        state.join(profile::REPO).join("journal.log")
    }

    #[test]
    fn the_relaunch_notice_names_both_versions_and_the_conversation() {
        assert_eq!(
            relaunch_notice(Some("9.9.1"), Some("9.9.2"), "sid"),
            "claude-alab: claude 9.9.1 -> 9.9.2: resuming conversation sid on the new build"
        );
        assert_eq!(
            relaunch_notice(None, None, "sid"),
            "claude-alab: claude ? -> ?: resuming conversation sid on the new build"
        );
    }

    #[test]
    fn the_fallback_notice_claims_a_resume_only_when_the_previous_build_started() {
        let started = fallback_notice(Some("9.9.2"), Some("9.9.1"), "sid", None);
        assert_eq!(
            started,
            "claude-alab: claude 9.9.2 failed to start; resuming conversation sid on 9.9.1"
        );
        let refused = std::io::Error::other("pinned vendor working directory changed");
        let failed = fallback_notice(Some("9.9.2"), None, "sid", Some(&refused));
        assert_eq!(
            failed,
            "claude-alab: claude 9.9.2 failed to start and ? could not be restarted \
             (conversation sid): pinned vendor working directory changed"
        );
        assert!(!failed.contains("resuming") && !failed.contains('\n'));
    }

    #[test]
    fn the_crash_and_restart_notices_name_the_build_the_cause_and_the_conversation() {
        assert_eq!(
            crash_notice(Some("9.9.1"), ChildEvent::Exited(7), "sid"),
            "claude-alab: claude 9.9.1 ended abnormally (exit 7); resuming conversation sid"
        );
        assert_eq!(
            crash_notice(None, ChildEvent::Signaled(libc::SIGSEGV), "sid"),
            format!(
                "claude-alab: claude ? ended abnormally (signal {}); resuming conversation sid",
                libc::SIGSEGV
            )
        );
        assert_eq!(
            restart_notice(Some("9.9.1"), "asked", "sid"),
            "claude-alab: claude 9.9.1 restarted at aterm's request (asked); resuming \
             conversation sid"
        );
        for line in [
            crash_notice(None, ChildEvent::Exited(1), "sid"),
            restart_notice(None, "stall", "sid"),
        ] {
            assert!(
                !line.contains('\n') && !line.contains("new build"),
                "{line}"
            );
        }
    }

    /// A fresh state home for one test's journal.
    fn fresh_state(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "claude-alab-{name}-{}-{}",
            std::process::id(),
            now_ms()
        ))
    }

    /// The events of the journal under `state`, without their time and pid.
    fn events(state: &Path) -> Vec<String> {
        std::fs::read_to_string(sup_journal(state))
            .unwrap_or_default()
            .lines()
            .map(|line| line.split_once(" pid=").map_or(line, |(_, rest)| rest))
            .map(|rest| rest.split_once(' ').map_or(rest, |(_, event)| event))
            .map(str::to_string)
            .collect()
    }

    /// A crash being answered, as [`Supervisor::crash_begins`] leaves it.
    pub(super) fn crashed(event: ChildEvent) -> Ending {
        Ending::Crashed {
            event,
            crash: Crash {
                record: SessionRecord {
                    pid: 2,
                    session_id: "sid".to_string(),
                    cwd: "/".to_string(),
                    status: profile::STATUS_IDLE.to_string(),
                    status_updated_at_ms: 1,
                    started_at_ms: 1,
                    kind: profile::KIND_INTERACTIVE.to_string(),
                    version: "9.9.1".to_string(),
                },
                exit_ms: 1_000,
                due_ms: 2_000,
                resume: Vec::new(),
                reclaim_spent_ms: 0,
                reclaim_since_ms: None,
                ask_at_ms: 1_000,
                told: false,
                wait: CrashWait::Settle { until_ms: 1_250 },
            },
        }
    }

    #[test]
    fn one_crash_waits_for_aterm_forty_five_seconds_in_all_and_each_loss_gets_its_rest() {
        let Ending::Crashed { mut crash, .. } = crashed(ChildEvent::Exited(7)) else {
            unreachable!("a crash");
        };
        assert_eq!(crash.reclaim_until_ms(), None);
        // Lost at the exit: the whole hold is ahead.
        assert!(crash.reclaim_begins(1_000));
        assert!(!crash.reclaim_begins(3_000), "one wait, begun once");
        assert_eq!(crash.reclaim_until_ms(), Some(46_000));
        // Claimed again 5 s later: 5 s of the hold are spent.
        crash.reclaim_ends(6_000);
        assert_eq!(crash.reclaim_until_ms(), None);
        assert_eq!(crash.reclaim_spent_ms, 5_000);
        // Lost again at the end of a 60 s pause, long after the first wait's
        // deadline: the rest of the hold, 40 s, is waited, not none.
        assert!(crash.reclaim_begins(61_000));
        assert_eq!(crash.reclaim_until_ms(), Some(101_000));
        crash.reclaim_ends(100_000);
        assert_eq!(crash.reclaim_spent_ms, 44_000);
        // A third loss has one second left, and a spent hold none at all.
        assert!(crash.reclaim_begins(700_000));
        assert_eq!(crash.reclaim_until_ms(), Some(701_000));
        crash.reclaim_ends(702_000);
        assert!(crash.reclaim_begins(800_000));
        assert_eq!(crash.reclaim_until_ms(), Some(800_000));
    }

    #[test]
    fn a_crash_answered_away_permitted_then_away_again_waits_only_the_rest_of_its_budget() {
        let Ending::Crashed { mut crash, .. } = crashed(ChildEvent::Exited(7)) else {
            unreachable!("a crash");
        };
        let away = || link::Relaunch::Away("link-lost".to_string());
        // Away at the exit: the whole hold, journalled once.
        assert_eq!(
            crash.heard(away(), 1_000),
            Heard::Wait(Some(
                "crash waiting-for-claim hold-ms=45000 link-lost".to_string()
            ))
        );
        assert_eq!(crash.wait, CrashWait::Claim);
        assert_eq!(crash.heard(away(), 3_000), Heard::Wait(None));
        // A lost link sets no clock of its own to be asked on.
        assert_eq!(crash.ask_at_ms, 1_000);
        // Permitted 5 s later: the pause runs, told once.
        assert_eq!(
            crash.heard(link::Relaunch::Permitted, 6_000),
            Heard::Pause { tell: true }
        );
        assert_eq!(crash.wait, CrashWait::Pause);
        assert_eq!(crash.reclaim_until_ms(), None);
        // Away again at the end of a 60 s pause, long after the first wait's
        // own deadline (46 s): the rest of the budget, 40 s, is waited.
        assert_eq!(
            crash.heard(away(), 61_000),
            Heard::Wait(Some(
                "crash waiting-for-claim hold-ms=40000 link-lost".to_string()
            ))
        );
        assert_eq!(crash.reclaim_until_ms(), Some(101_000));
        // A later answer is asked again a second on, from the same budget.
        assert_eq!(
            crash.heard(
                link::Relaunch::Later("policy reply=ERR:main".to_string()),
                70_000
            ),
            Heard::Wait(None)
        );
        assert_eq!(crash.ask_at_ms, 71_000);
        assert_eq!(
            crash.heard(link::Relaunch::Permitted, 80_000),
            Heard::Pause { tell: false }
        );
        // The budget spent (5 s, then 19 s), a third wait has 21 s, after
        // which the crash is passed on.
        assert_eq!(
            crash.heard(away(), 200_000),
            Heard::Wait(Some(
                "crash waiting-for-claim hold-ms=21000 link-lost".to_string()
            ))
        );
        assert_eq!(
            crash.heard(away(), 221_000),
            Heard::PassOn {
                waited: None,
                why: "claim-not-proven-again link-lost".to_string()
            }
        );
        assert_eq!(
            crash.heard(link::Relaunch::Denied("relaunch=false".to_string()), 0),
            Heard::PassOn {
                waited: None,
                why: "relaunch=false".to_string()
            }
        );
    }

    #[test]
    fn a_crash_aterm_never_answers_is_passed_on_when_its_budget_is_spent() {
        let Ending::Crashed { mut crash, .. } = crashed(ChildEvent::Exited(7)) else {
            unreachable!("a crash");
        };
        let later = || link::Relaunch::Later("policy reply=ERR:main".to_string());
        assert_eq!(
            crash.heard(later(), 1_000),
            Heard::Wait(Some(
                "crash waiting-for-aterm hold-ms=45000 policy reply=ERR:main".to_string()
            ))
        );
        for at in (2_000..46_000).step_by(1_000) {
            assert_eq!(crash.heard(later(), at), Heard::Wait(None), "{at}");
            assert_eq!(crash.ask_at_ms, at + 1_000);
        }
        assert_eq!(
            crash.heard(later(), 46_000),
            Heard::PassOn {
                waited: None,
                why: "aterm-not-answered policy reply=ERR:main".to_string()
            }
        );
    }

    #[test]
    fn a_restart_waiting_on_aterm_is_ignored_after_forty_five_seconds() {
        let state = fresh_state("restart-expiry");
        let mut sup = supervisor_of_an_unsignallable_child(&state);
        let request = Restart {
            model: Some("opus[1m]".into()),
            ..restart("stall")
        };
        let waiting = |since: u64| Deferred {
            restart: request.clone(),
            record: None,
            why: "aterm link-lost".to_string(),
            on_aterm_since_ms: Some(since),
            retry_at_ms: 0,
        };
        let now = now_ms();
        sup.restart_waits(
            &request,
            None,
            Some(waiting(now - 44_000)),
            "aterm link-lost",
            true,
        );
        assert_eq!(
            sup.restart_deferred
                .as_ref()
                .and_then(|d| d.on_aterm_since_ms),
            Some(now - 44_000),
            "the wait keeps its start"
        );
        assert_eq!(sup.restart_deferred.as_ref().unwrap().restart, request);
        sup.restart_deferred = None;
        sup.restart_waits(
            &request,
            None,
            Some(waiting(now - 45_000)),
            "aterm link-lost",
            true,
        );
        assert_eq!(sup.restart_deferred, None);
        assert_eq!(
            events(&state),
            ["aterm restart cause=stall ignored aterm link-lost"]
        );
        let _ = std::fs::remove_dir_all(&state);
    }

    #[test]
    fn a_crash_waiting_on_a_lost_link_wakes_at_its_deadline_not_in_the_past() {
        let state = fresh_state("lost-wake");
        let mut sup = supervisor_of_an_unsignallable_child(&state);
        sup.ending = crashed(ChildEvent::Exited(7));
        if let Some(crash) = sup.crash_mut() {
            crash.wait = CrashWait::Claim;
            crash.reclaim_since_ms = Some(1_000);
            // Long past: a held link is asked at once.
            crash.ask_at_ms = 2_000;
        }
        assert_eq!(sup.crash_wake_ms(), Some(2_000));
        let locator = alab_wire::wire::Locator::new(state.clone(), "s-0000000000000000001a")
            .expect("a locator");
        sup.link = link::Link::launch_at(&locator, None, 1_000);
        assert!(sup.link.lost(), "a missing host is awaiting reclaim");
        // A lost link is asked nothing before it is claimed again: the loop
        // sleeps to the budget's end, the poll trying the claim meanwhile.
        assert_eq!(sup.crash_wake_ms(), Some(46_000));
        let _ = std::fs::remove_dir_all(&state);
    }

    #[test]
    fn a_pause_is_announced_in_whole_seconds_with_the_way_to_end_it() {
        assert_eq!(
            pause_notice(Some("9.9.1"), ChildEvent::Exited(7), 10_000),
            "claude-alab: claude 9.9.1 ended abnormally (exit 7); restarting in 10 s, Ctrl-C \
             ends the session"
        );
        assert_eq!(
            pause_notice(None, ChildEvent::Signaled(9), 750),
            "claude-alab: claude ? ended abnormally (signal 9); restarting in 1 s, Ctrl-C ends \
             the session"
        );
        assert!(pause_notice(None, ChildEvent::Exited(1), 0).contains("in 0 s"));
    }

    #[test]
    fn only_a_session_signal_ends_a_launch_or_a_relaunch_before_its_start() {
        assert_eq!(session_ending(&[]), None);
        assert_eq!(
            session_ending(&[libc::SIGCHLD, libc::SIGTSTP, libc::SIGCONT]),
            None
        );
        for sig in [libc::SIGHUP, libc::SIGTERM, libc::SIGINT, libc::SIGQUIT] {
            assert_eq!(
                session_ending(&[libc::SIGCHLD, sig, libc::SIGHUP]),
                Some(sig)
            );
        }
    }

    #[test]
    fn a_restart_is_not_taken_for_a_child_that_has_ended_unreaped() {
        let state = fresh_state("restart-zombie");
        let mut sup = supervisor_of_an_unsignallable_child(&state);
        let mut child = std::process::Command::new("/usr/bin/true")
            .spawn()
            .expect("a child that ends at once");
        sup.child.pid = libc::pid_t::try_from(child.id()).unwrap();
        // Ended, and left for the loop to reap.
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while !sys::child_has_ended(sup.child.pid) {
            assert!(
                std::time::Instant::now() < deadline,
                "the child never ended"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
        sup.restart_asked(&restart("stall"));
        assert_eq!(sup.ending, Ending::None);
        assert_eq!(sup.restart_deferred, None);
        assert_eq!(
            events(&state),
            ["aterm restart cause=stall ignored child-ended"]
        );
        // Still pending for the loop's own reap.
        assert!(child.wait().expect("still unreaped").success());
        let _ = std::fs::remove_dir_all(&state);
    }

    #[test]
    fn the_child_is_not_signalled_while_a_crash_is_answered() {
        let state = fresh_state("no-child");
        let mut sup = supervisor_of_an_unsignallable_child(&state);
        // A pid that takes a signal: this process, and a signal that does
        // nothing to it. With a child the signal is sent.
        sup.child.pid = libc::pid_t::try_from(std::process::id()).unwrap();
        assert!(!sup.no_child());
        assert!(sup.signal_child(libc::SIGCONT));
        assert_eq!(sup.crash_wake_ms(), None);
        sup.ending = crashed(ChildEvent::Exited(7));
        assert!(sup.no_child());
        assert!(
            !sup.signal_child(libc::SIGCONT),
            "a reaped pid is not signalled"
        );
        // Its clock is the state's own.
        assert_eq!(sup.crash_wake_ms(), Some(1_250));
        if let Some(crash) = sup.crash_mut() {
            crash.wait = CrashWait::Pause;
        }
        assert_eq!(sup.crash_wake_ms(), Some(2_000));
        if let Some(crash) = sup.crash_mut() {
            crash.wait = CrashWait::Claim;
            crash.reclaim_since_ms = Some(1_000);
            crash.ask_at_ms = 50_000;
        }
        assert_eq!(sup.crash_wake_ms(), Some(46_000));
        if let Some(crash) = sup.crash_mut() {
            crash.ask_at_ms = 2_000;
        }
        assert_eq!(sup.crash_wake_ms(), Some(2_000));
        // The look moves nothing and ends nothing.
        sup.look(|| false);
        assert!(sup.no_child());
        assert_eq!(events(&state), [] as [&str; 0]);
        let _ = std::fs::remove_dir_all(&state);
    }

    #[test]
    fn an_end_that_is_no_crash_to_answer_is_left_to_the_session() {
        let state = fresh_state("no-crash");
        let mut sup = supervisor_of_an_unsignallable_child(&state);
        // No claim was ever made: nothing is entered and nothing is journalled,
        // whatever the end.
        for event in [
            ChildEvent::Exited(7),
            ChildEvent::Signaled(libc::SIGSEGV),
            ChildEvent::Exited(0),
        ] {
            assert!(!sup.crash_begins(event), "{event:?}");
        }
        assert_eq!(sup.ending, Ending::None);
        assert_eq!(events(&state), [] as [&str; 0]);
        let _ = std::fs::remove_dir_all(&state);
    }

    #[test]
    fn a_restart_request_waits_for_a_record_and_is_refused_while_the_child_is_ending() {
        let state = fresh_state("restart");
        let mut sup = supervisor_of_an_unsignallable_child(&state);
        // No record yet: the request waits, journalled once however often it
        // is looked at again, and the latest request replaces it.
        sup.restart_asked(&restart("asked"));
        sup.restart_retry();
        sup.restart_retry();
        assert_eq!(
            sup.restart_deferred.as_ref().map(|d| (
                d.restart.cause.as_str(),
                d.why.as_str(),
                d.on_aterm_since_ms
            )),
            Some(("asked", "no-record", None))
        );
        sup.restart_asked(&restart("stall"));
        let account = Restart::parse("cause=account to=work home=- model=haiku".split_whitespace())
            .expect("typed account restart");
        sup.restart_asked(&account);
        assert_eq!(
            sup.restart_deferred
                .as_ref()
                .map(|d| d.restart.cause.as_str()),
            Some("stall")
        );
        // The child it was for is gone: nothing is left to restart.
        sup.restart_moot();
        assert_eq!(sup.restart_deferred, None);
        sup.restart_retry();
        // Whatever else is ending the child, or has, decides alone.
        sup.ending = Ending::Passed(libc::SIGTERM);
        sup.restart_asked(&restart("asked"));
        sup.ending = crashed(ChildEvent::Exited(7));
        sup.restart_asked(&restart("asked"));
        assert_eq!(sup.restart_deferred, None);
        assert_eq!(
            events(&state),
            [
                "aterm restart cause=asked deferred no-record",
                "aterm restart cause=stall deferred no-record",
                "aterm restart cause=account ignored account-assignment-unsupported",
                "aterm restart cause=stall ignored child-replaced",
                "aterm restart cause=asked ignored session-ending",
                "aterm restart cause=asked ignored crash-pending",
            ]
        );
        let _ = std::fs::remove_dir_all(&state);
    }

    #[test]
    fn an_abandoned_model_restart_leaves_the_launch_basis_intact() {
        use std::os::unix::fs::PermissionsExt as _;

        let state = fresh_state("restart-model-abandoned");
        let mut sup = supervisor_of_an_unsignallable_child(&state);
        sup.setup.args = os(&["--model", "sonnet"]);
        sup.child.build.dir = state.join("store/claude/1");
        sup.child.build.exe = sup.child.build.dir.join("bin/claude");
        std::fs::create_dir_all(sup.child.build.exe.parent().unwrap()).unwrap();
        std::fs::write(&sup.child.build.exe, b"lease fixture").unwrap();
        std::fs::set_permissions(&sup.child.build.exe, std::fs::Permissions::from_mode(0o700))
            .unwrap();
        let plan = sup
            .same_build_plan("conversation".into(), vec!["--model".into(), "opus".into()])
            .unwrap();
        sup.ending = Ending::ForRestart {
            sent: Instant::now() - Duration::from_millis(RESTART_GRACE_MS + 1),
            plan,
            cause: "model".into(),
            selected_args: Some(os(&["--model", "opus"])),
            killed: false,
        };
        sup.restart_escalate();
        assert_eq!(sup.ending, Ending::None);
        assert_eq!(sup.setup.args, os(&["--model", "sonnet"]));
        assert_eq!(
            events(&state),
            ["restart-abandoned cause=model the child kept running after SIGTERM"]
        );
        let _ = std::fs::remove_dir_all(&state);
    }

    #[test]
    fn no_own_image_move_is_encoded_while_a_crash_or_a_restart_is_pending() {
        let state = fresh_state("image-gate");
        let mut sup = supervisor_of_an_unsignallable_child(&state);
        let end = sys::DescriptorIdentity {
            device: 3,
            inode: 4,
            rdev: 0,
        };
        let signals = sys::SignalSnapshot {
            read_fd: 11,
            write_fd: 12,
            read_identity: end,
            write_identity: end,
            prior_mask: 0,
            ignored: 0,
        };
        const GATE: &str = "supervisor is ending or stopped";
        let refused =
            |sup: &Supervisor| transfer::encode(sup, &signals).map_err(|error| error.to_string());
        // The fixture is no transferable supervisor, but it passes this gate.
        assert_ne!(refused(&sup), Err(GATE.to_string()));
        sup.restart_deferred = Some(Deferred {
            restart: Restart {
                model: Some("opus".into()),
                ..restart("asked")
            },
            record: None,
            why: "no-record".to_string(),
            on_aterm_since_ms: None,
            retry_at_ms: 0,
        });
        assert_eq!(refused(&sup), Err(GATE.to_string()));
        sup.restart_deferred = None;
        sup.ending = crashed(ChildEvent::Exited(7));
        assert_eq!(refused(&sup), Err(GATE.to_string()));
        let _ = std::fs::remove_dir_all(&state);
    }

    #[test]
    fn refused_start_distinguishes_startup_failures_from_clean_and_user_signal_exits() {
        assert!(!refused_start(ChildEvent::Exited(0)));
        for code in [129, 130, 131, 143] {
            assert!(!refused_start(ChildEvent::Exited(code)), "{code}");
        }
        for code in [1, 2, 126, 127] {
            assert!(refused_start(ChildEvent::Exited(code)), "{code}");
        }
        for sig in [libc::SIGINT, libc::SIGQUIT, libc::SIGTERM, libc::SIGHUP] {
            assert!(!refused_start(ChildEvent::Signaled(sig)), "{sig}");
        }
        for sig in [libc::SIGABRT, libc::SIGILL, libc::SIGSEGV, libc::SIGKILL] {
            assert!(refused_start(ChildEvent::Signaled(sig)), "{sig}");
        }
    }
}
