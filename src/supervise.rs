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
//! ([`crate::profile::resume_argv`], in the conversation's own permission mode
//! where that is no wider than the launch asked for,
//! [`crate::profile::with_permission_mode`]) in the same process, on the same
//! terminal. A child still running when the grace ends keeps running, and that
//! target build is never tried again (a later build re-arms the update): the
//! harness sends one SIGTERM per target, never a second. The harness process
//! never exits for an update.
//!
//! **aterm.** Inside an aterm tab the harness claims the session's duties
//! before the first child starts ([`link`]): `relaunch`, `upgrade` and, where
//! aterm serves contract 4, `accounts`, through the shared link, whatever
//! `--alab-duties=` names. An account move aterm asks for is made as a
//! restart ([`Supervisor::restart_asked`], Decision 63). The four duties
//! that type (`turn-end`, `approvals`, `questions`, `walls`) are built here
//! and not claimed yet: the shared link has no fenced typing channel
//! ([`link::UNTYPED`]), so aterm's own engine keeps them, and their deciders
//! and drivers find no mode to act under ([`Supervisor::link_duty_mode`]). A launch aterm refuses the claim, or that
//! it does not answer, is supervised all the same, unclaimed, and the claim
//! is asked for again by the shared link. While the claim stands proven, aterm's
//! `status` replaces the harness's own reading of the terminal's input (the
//! input clock, the composer rule and the unread-input check) at the safe
//! point; every check the harness makes against the kernel and the store
//! stays. Without a proven claim, and whenever aterm stops answering, the
//! harness decides on its own readings exactly as it does outside aterm, and
//! types nothing: it types only through the claim's fenced writes, since the
//! child reads the tab's terminal directly ([`Supervisor::note_untyped`]).
//!
//! **Crash relaunch.** Under a claim, which always takes `relaunch`, the
//! harness brings a crashed child back by aterm's rule ([`crate::relaunch`]);
//! a session that never had a claim ends with its child, as outside aterm
//! ([`link::Link::claims`]). A child that
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
//! waits for one.
//!
//! The cause is aterm's word, and the harness reads it for two things only:
//! `model`, and the two that move an account ([`crate::aterm::ACCOUNT_CAUSES`]).
//! Every other cause is told bare (no `model=`, no `to=`, no `home=`) and is
//! the restart above, of the child as it is: a person's `asked`, the stall's
//! `stall`, the two aterm's own loop sends a `relaunch` holder, `memory` (its
//! memory restart) and `input-lost` (a line it wrote that the child never
//! drew), and `route`, the word aterm's HX-58 gives a session whose route
//! must change (the vault's token for a stored login, or back). The harness
//! obeys `route` whether or not the aterm beside it sends it yet: a cause it
//! does not know is a bare restart too. A `to=` or a `home=` under any of
//! them is not passed over: the shared link reads no request in such a line
//! (`alab_wire::claim::Restart::parse`), so no restart is made, the event
//! subscription that carried it is given up as faulted (`aterm events-lost
//! restart event has no single cause`) and a stream record that carried it
//! is refused. None of the bare causes needs the `accounts` grant.
//!
//! The child of a bare restart comes back as every start of this
//! launch is made ([`crate::launch::vendor_command_as`]): through aterm's
//! token helper told `--auto` where the launch reaches one, on the login
//! directory and the model the session had. For `route` that helper is the
//! whole of the move: aterm chose the route, and its helper judges it at
//! the new exec. The harness names no route and reads none.
//!
//! **A model aterm names.** A restart that carries `model=<word>` is made the
//! same way, and the child is resumed with `--model <word>` in place of any
//! `--model` of the launch ([`crate::profile::with_model`]). For a request,
//! aterm decides when; the harness makes the move. The
//! model is then kept ([`Supervisor::model`]): every later resume line this
//! harness writes for the session (a crash relaunch, a vendor update, another
//! restart) names it, until a restart names another, and an own-image move
//! carries it. A restart that names the launch's own model again restores the
//! launch's line, and so does the reserved word `model=launch`
//! ([`crate::aterm::MODEL_LAUNCH`]): the kept model is dropped and the child
//! is resumed on the launch line's own model words. Where the launch named no
//! model, that resume line names the alias `fable` ([`limit::back_to`]): a
//! line with no `--model` takes up the conversation's newest model, which is
//! the one the session was moved onto. Every relaunch made before the child
//! answers on it names the alias too ([`Supervisor::launch_pin`]), since the
//! newest model is still the one it was moved off; once it has answered
//! nothing is kept, and every relaunch is on the launch's own line. A
//! restart with no `model=` changes nothing: the child comes back on the
//! model it had. A request with cause `model` that names the model the child
//! already runs on, while nothing else is ending, restarts nothing (`ignored
//! already-on`); so does `model=launch` while nothing is kept.
//!
//! A model is asked of the conversation, not of the child that happens to
//! run: a model restart that arrives while something else is already ending
//! the child, or bringing it back (a vendor update's SIGTERM, another
//! restart's or the harness's own, a crash being answered), is kept, and the
//! relaunch that ending makes is made on the model
//! ([`Supervisor::model_joins`]). If that ending makes no relaunch after all,
//! the request is made as a restart of its own. Either way the child is
//! ended once.
//!
//! **Fable's limit.** The harness decides one model move by itself
//! ([`crate::limit`], docs/DECISIONS.md 44). When the child's transcript
//! shows it refused at Fable's own usage limit and the session's model is
//! Fable (its launch names Fable, or no model), the harness ends the child
//! with SIGTERM and resumes the conversation on [`limit::FALLBACK`], through
//! the same restart as above ([`Ending::ForRestart`], journalled `own restart
//! cause=fable-limit …`). Nothing is typed: the signal closes the vendor's
//! dialog. It waits for a moment nobody is interrupted
//! ([`Supervisor::wall_point`]): the limit row is a quiet period old, the
//! record is not `busy`, the harness is the terminal's foreground group, no
//! background work runs under the child (background work holds for at most
//! [`limit::WALL_PATIENCE_MS`] from the row, a busy record for as long from
//! the later of the row and the start of its busy run), and the person is
//! quiet with no draft to lose: by aterm's `status` (its `composer=` among
//! it) under a proven claim ([`crate::aterm::judge_wall_point`]), and
//! otherwise by the terminal's input clock and queue, the tracker's draft
//! rule and the newest prompt the transcript shows was sent
//! ([`Supervisor::wall_typed`]). Once the limit has
//! reset the session is resumed on its launch's own model again
//! (`cause=fable-reset`), by the same restart as aterm's `model=launch`, at
//! the ordinary safe point, judged for `relaunch`; under a proven claim its
//! record half is also met at a turn's end, before a continuation is typed
//! ([`Supervisor::claimed_rest`]): the turn-end duty below types nothing into
//! the turn end a due return would take, and the return is never made over an
//! act the duty typed that is still to be taken up
//! ([`Supervisor::limit_yields`]). No aterm switch is read
//! for either: where aterm's policy is off (Autopilot off among its reasons)
//! the claim is given back, and the harness decides on its own readings. A move that could not be made backs off
//! ([`limit::retry_after_ms`]); a return that is refused again climbs
//! [`limit::RETURN_RUNGS_MS`]; a session on the fallback is never moved by a
//! limit row. What is kept of this ([`limit::State`]) crosses an own-image
//! move.
//!
//! **Turn ends.** Once the claim takes `turn-end` (it does not yet, see
//! **aterm** above) the harness carries its own
//! session's turn ends under the tab's Autopilot word, by the rules aterm
//! carries them by ([`turn_end`], [`crate::duty`]): nothing under
//! Off; under Cruise the continuation at every turn end and a done check at
//! each claim of done; under Land its instruction, its nudge and its land
//! check, until the check is answered `LANDED`. Each act is one fenced `turn`
//! through the claim ([`crate::typing`]), held for a person's grace and draft.
//! A session moved off Fable's limit is owed one resume, which nothing else
//! would type: the vendor stopped at its limit wall. A model move wanted at a
//! look comes before the duty; a return that is due takes only the turn end
//! it is made at, and leaves the duty every other
//! ([`Supervisor::limit_yields`]). A vendor update that is due has a turn end
//! to itself for its quiet period.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use alab_wire::record::UpgradePhase;

use crate::aterm::{MODEL_LAUNCH, Restart, Scope};
use crate::cli::{EXIT_VENDOR_MISSING, Options, say};
use crate::journal::Journal;
use crate::limit;
use crate::model_facts;
use crate::procs;
use crate::profile::{self, COMMAND, SessionRecord, TOOL};
use crate::quiet::{Look, Tracker, Wait};
use crate::relaunch::{EXIT_SETTLE, Ladder, abnormal_exit};
use crate::store::{Build, Store};
use crate::sys::{self, ChildEvent, SignalPipe, Termios};

#[path = "supervise_box_duty.rs"]
mod box_duty;
#[doc(hidden)]
#[path = "supervise_image.rs"]
pub mod image;
#[path = "supervise_link.rs"]
mod link;
#[path = "supervise_transfer.rs"]
mod transfer;
#[path = "supervise_duty.rs"]
mod turn_end;
#[path = "supervise_vendor.rs"]
mod vendor;
#[path = "supervise_wall_duty.rs"]
mod wall_duty;

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

/// How long after an account move was read the harness may still end its
/// child for it, whatever the move waited on: the child's record, a relaunch
/// under way, aterm's word or the safe point. Past it the move is not made
/// (`ignored account-offer-expired`), and it joins no relaunch either. aterm
/// counts a move unmade from this number (its own bounds follow from it), so
/// it is part of the contract between the two and no field carries it.
pub const ACCOUNT_OFFER_MS: u64 = 15_000;

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
/// started again on its conversation because aterm asked (`cause`, which for a
/// model move also names the model: `model: <word>`).
fn restart_notice(version: Option<&str>, cause: &str, session_id: &str) -> String {
    format!(
        "{TOOL}: {COMMAND} {} restarted at aterm's request ({cause}); resuming conversation {session_id}",
        version.unwrap_or("?"),
    )
}

/// The one line the terminal gets when the build of version `version` has been
/// started again on its conversation because the start on the account aterm
/// named was not made ([`Supervisor::account_unmade`]).
fn unmade_notice(version: Option<&str>, session_id: &str) -> String {
    format!(
        "{TOOL}: {COMMAND} {} was not started on the account aterm named; resuming conversation {session_id} on the account it left",
        version.unwrap_or("?"),
    )
}

/// The one line the terminal gets when the build of version `version` has been
/// started again on its conversation by the harness itself for a wall
/// (`cause`: [`crate::walls::CAUSE_MEMORY`] for the critical-memory banner).
fn wall_notice(version: Option<&str>, cause: &str, session_id: &str) -> String {
    format!(
        "{TOOL}: {COMMAND} {} restarted for its {cause} wall; resuming conversation {session_id}",
        version.unwrap_or("?"),
    )
}

/// The one line the terminal gets when the harness itself has resumed the
/// conversation on `model` for Fable's usage limit (`cause`:
/// [`limit::CAUSE_LIMIT`] at the limit, else its reset).
fn limit_notice(cause: &str, model: &str, session_id: &str) -> String {
    let what = if cause == limit::CAUSE_LIMIT {
        "Fable's usage limit is reached"
    } else {
        "Fable's usage limit has reset"
    };
    format!("{TOOL}: {what}; resuming conversation {session_id} on {model}")
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
    /// Whether `args` starts with the `--settings <value>` aterm's usage tap
    /// put there ([`crate::launch::usage_tap`]). A launch through `run --`
    /// asks the tap again, so the resume words a `session` record gives aterm
    /// leave the two out ([`Supervisor::session_look`]).
    pub tapped: bool,
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

/// The longest word aterm's harness watchdog types in a relaunch line, in
/// bytes (aterm `crates/aterm-gui/src/harness_claim.rs`, `RUN_WORD_MAX`).
const RELAUNCH_WORD_MAX: usize = 1024;

/// The most resume words that line holds: aterm's `RUN_WORDS_MAX` (64) less
/// the most it puts before them, the tool, `run`, `--account <label>`,
/// `--home <dir>`, `--model <word>` and `--` ([`alab_wire::Run`]).
const RELAUNCH_RESUME_MAX: usize = 64 - 9;

/// Why aterm's watchdog would refuse to type `words` after `claude-alab run
/// --`, if it would: its `validate_run` holds every word of the line to 1 to
/// [`RELAUNCH_WORD_MAX`] bytes of printable ASCII, a space allowed, and the
/// line to 64 words, and types nothing at all where one word is out. A
/// `session` record whose words aterm would refuse carries none, so aterm
/// resumes from its own snapshot of the child (`--resume <id>`, no launch
/// flags) rather than not at all. Leaving the one flag out instead could
/// widen what the resumed session may do (a `--settings` deny list, a
/// `--system-prompt`), so no flag is ever left out.
fn relaunchable(words: &[String]) -> Result<(), String> {
    if words.len() > RELAUNCH_RESUME_MAX {
        return Err(format!("unrelaunchable words={}", words.len()));
    }
    let typable = |word: &String| {
        (1..=RELAUNCH_WORD_MAX).contains(&word.len())
            && word.bytes().all(|byte| (b' '..=b'~').contains(&byte))
    };
    match words.iter().position(|word| !typable(word)) {
        Some(at) => Err(format!("unrelaunchable word={at}")),
        None => Ok(()),
    }
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
    /// `moved` is the model the plan's resume line carries because a
    /// restart named it, with that restart's cause
    /// ([`Supervisor::model_joins`]).
    AwaitingUpdate {
        launch: UpdateLaunch,
        plan: Option<vendor::Plan>,
        moved: Option<(String, String)>,
    },
    /// HUP or TERM was passed on: the session ends with the child.
    Passed(libc::c_int),
    /// The harness sent SIGTERM because aterm asked for a restart (`cause`),
    /// or to make its own model move for Fable's limit (`own`,
    /// [`Supervisor::limit_look`]) or its own restart for a wall (`own` with
    /// no `model`: the critical-memory banner, [`Supervisor::wall_look`]), and
    /// SIGKILL once the child outlived
    /// [`RESTART_GRACE_MS`] (`killed`):
    /// the same build is started again from `plan`, whose target is its old,
    /// whenever the child ends while this stands. `sent` is when the last
    /// signal was sent, on a clock that stops while the machine sleeps; time
    /// the child spends stopped does not count. `model` is the model the
    /// request named, which `plan`'s resume line already carries; it becomes
    /// the session's kept model once the child has been started on it
    /// ([`Supervisor::model_moved`]), unless a later model restart joined the
    /// relaunch ([`Supervisor::model_joins`]).
    ForRestart {
        sent: Instant,
        plan: vendor::Plan,
        cause: String,
        model: Option<String>,
        killed: bool,
        own: bool,
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
#[derive(Clone, Debug, PartialEq, Eq)]
enum Cause {
    /// A move onto another vendor build (or the fallback of one).
    Update,
    /// aterm asked for a restart, for this reason.
    Asked(String),
    /// The harness's own restart for a wall (the critical-memory banner,
    /// [`crate::walls::CAUSE_MEMORY`]), on the model the child ran.
    Wall(String),
    /// The harness's own model move for Fable's limit.
    Own {
        /// [`limit::CAUSE_LIMIT`] or [`limit::CAUSE_RESET`].
        cause: String,
        /// The model the conversation is resumed on, as the terminal is told
        /// ([`Supervisor::shown_model`]), unless a model restart aterm asked
        /// for joins the relaunch ([`Supervisor::model_joins`]): the notice
        /// then names that one's.
        model: String,
    },
    /// The child crashed.
    Crash,
    /// The start aterm's account move admitted was not made (the token
    /// helper started nothing): the conversation is started again on the
    /// account it left ([`Supervisor::account_unmade`]).
    Unmade,
}

/// A start made on an account aterm named ([`Supervisor::restart_asked`])
/// whose vendor has not been seen running yet: the child may still be aterm's
/// token helper, which starts nothing for a key the login directory does not
/// resolve to ([`crate::launch::NOT_STARTED`]). Until the vendor is seen
/// ([`Supervisor::account_seen`]) the account is not the session's: no claim
/// names it, nothing is journalled as moved, and a start that was refused is
/// made again as it was pinned before the move
/// ([`Supervisor::account_unmade`]).
#[derive(Clone, Debug, PartialEq, Eq)]
struct Admitting {
    /// The account the child was started on.
    account: crate::aterm::Account,
    /// The plan the child was started from, as it was pinned before the
    /// account was put on it ([`vendor::Plan::unmade`]): the same build and
    /// resume line, on the account the session ran on.
    before: vendor::Plan,
    /// Why the start was made, as `account-move` names it (`cause=`).
    cause: String,
    /// The limit's floor before this start raised it to the new child's
    /// start ([`Supervisor::account_admitting`]), which a start that was not
    /// made puts back: the wall the old account stood at is then still the
    /// session's.
    floor_ms: u64,
}

/// A restart aterm asked for that cannot be made yet.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Deferred {
    /// What aterm asked for: its `cause=`, and the model and the account it
    /// named, if any.
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
    /// When (epoch ms) the account move it asks for was first read, which
    /// its offer is counted from ([`ACCOUNT_OFFER_MS`], [`read_at`]). It says
    /// nothing of a request that names no account.
    read_ms: u64,
}

/// When the account move `restart` asks for was first read: when the request
/// that was waiting (`before`) was, where that one asked for the same
/// account, and `now` otherwise. A move asked again, and a later request
/// that joined a waiting move and so carries its account, start no new offer.
fn read_at(restart: &Restart, before: Option<&Deferred>, now: u64) -> u64 {
    before
        .filter(|before| before.restart.account == restart.account)
        .map_or(now, |before| before.read_ms)
}

/// What the journal already says about Fable's limit and the move it calls
/// for. Every word of it is cleared by a completed model move
/// ([`Supervisor::model_moved`]).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct LimitSaid {
    /// The last decision journalled (`limit-seen …`).
    seen: String,
    /// Whether the return that is due was journalled (`limit-return due …`).
    due: bool,
    /// The last reason journalled for a move that waits (`limit-move wait
    /// …`), noted once per change.
    wait: String,
    /// The kinds of work the move was journalled as made over (`limit-move
    /// over …`), as their first words (`busy`, `background-work`) with a
    /// space between. Kept apart from the waits: a move made over one thing
    /// may still wait on another, look after look, and no line is repeated.
    over: String,
}

/// What one look read of the person's typing, for the move at a limit
/// ([`Supervisor::wall_typed`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Typed {
    /// The tracker's safe-point verdict on this look ([`Tracker::look`]).
    gate: Result<(), Wait>,
    /// The terminal's input clock, when there is a terminal.
    input: Option<u64>,
    /// The newest moment a prompt or command a person sent reached the
    /// vendor, as the transcript shows it ([`limit::Sight::typed_ms`]).
    taken_ms: Option<u64>,
}

/// Why the harness's own model move is not made on this look.
#[derive(Clone, Debug, PartialEq, Eq)]
enum LimitHold {
    /// Something passes by itself; the wait reason, as journal words.
    Wait(String),
    /// The move cannot be made as things are; why. The harness backs off.
    Failed(String),
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
    /// record, for an own-image move to end, or, when it names a model, for
    /// the relaunch another ending is about to make); the latest request
    /// replaces an earlier one. While it waits no own-image move is started.
    restart_deferred: Option<Deferred>,
    /// The model, effort level and top-effort key the child states in its
    /// transcript, read on each look ([`Supervisor::read_facts`]). An
    /// own-image move carries it, so the new image goes on reading where this
    /// one stopped.
    facts: model_facts::Reader,
    /// The model a restart aterm asked for last moved the child onto, when it
    /// is not the launch's own: every resume line names it in place of the
    /// launch's `--model` ([`Supervisor::resume_plan`]). `None` while the
    /// session runs on what its launch said, where a return onto the launch's
    /// own that has yet to answer names the launch pin instead
    /// ([`Supervisor::kept_model`]). An own-image move carries it.
    model: Option<String>,
    /// The last reason journalled for nothing being typed in this tab
    /// ([`Supervisor::note_untyped`]), so each reason is said once.
    untyped_noted: String,
    /// Where the session is with respect to Fable's limit
    /// ([`Supervisor::limit_look`]). An own-image move carries it.
    limit: limit::State,
    /// The witness of the usage limit the child stands at. It is rebuilt
    /// after an own-image move.
    limit_source: Box<dyn limit::Source>,
    /// What the journal already says about Fable's limit, so each thing is
    /// said once. An own-image move carries it.
    limit_said: LimitSaid,
    /// The turn-end duty ([`Supervisor::duty_look`]). An own-image move
    /// carries what its decider remembers.
    turn_end: turn_end::Driver,
    /// The account aterm last moved the session onto
    /// ([`Supervisor::restart_asked`], `restart cause=account`), once the
    /// vendor was seen running on it ([`Supervisor::account_seen`]): every
    /// later start is made on its login directory
    /// ([`vendor::Plan::on_account`]) and every claim names it (`login=`).
    /// `None` while the session runs on what its launch said.
    account: Option<crate::aterm::Account>,
    /// The start on an account whose vendor has not been seen running yet
    /// ([`Admitting`]). Not carried by an own-image move, which is not made
    /// while one stands.
    admitting: Option<Admitting>,
    /// Whether this image has asked for the off-Fable state a harness before
    /// it kept for the conversation ([`Supervisor::limit_adopt`]). Not
    /// carried by an own-image move: asking again is a no-op where the state
    /// crossed.
    limit_adopted: bool,
    /// What the last `session` record aterm was told was made from
    /// ([`Supervisor::session_look`]). Not carried by an own-image move,
    /// whose new image has no link to tell.
    described: Option<Described>,
    /// Where the move onto a newer vendor build stands, as aterm is told it
    /// ([`Supervisor::upgrade_told`]). Not carried by an own-image move.
    upgrading: Option<Upgrading>,
}

/// What a `session` record was made from ([`Supervisor::session_look`]):
/// a change of any of it can change what the record says.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Described {
    pid: libc::pid_t,
    conversation: String,
    /// The model every resume line names ([`Supervisor::kept_model`]).
    model: Option<String>,
    /// Whether the session is off Fable for its limit: the words then name
    /// no model ([`Supervisor::resume_words`]).
    off_fable: bool,
    /// The label of the account aterm moved the session onto.
    account: Option<String>,
    /// The record's `updatedAt` when it last read idle: a turn end can move
    /// the permission mode the resume words carry
    /// ([`profile::with_permission_mode`]).
    idle_at: u64,
}

/// A move onto build `target`, of the vendor version `to` (`-` where its
/// build states none), at `phase` (`seen`, `waiting` or `moving`: one that is
/// done or abandoned is forgotten).
#[derive(Clone, Debug, PartialEq, Eq)]
struct Upgrading {
    target: u64,
    to: String,
    phase: UpgradePhase,
}

/// `restart` as the journal names it: `cause=<why>`, ` to=<key>` when it
/// names an account (the word aterm sent only: the login directory and any
/// credential are never journalled), and ` model=<word>` when it names a
/// model.
fn ask_words(restart: &Restart) -> String {
    let mut words = format!("cause={}", restart.cause);
    if let Some(account) = &restart.account {
        words.push_str(&format!(" to={}", account.key));
    }
    if let Some(model) = &restart.model {
        words.push_str(&format!(" model={model}"));
    }
    words
}

/// Whether `restart` asks the conversation for something whichever child
/// runs it, a model or an account: it waits for a relaunch already under way
/// and joins it ([`Supervisor::model_joins`]) rather than be passed over.
fn moves_conversation(restart: &Restart) -> bool {
    restart.model.is_some() || restart.account.is_some()
}

/// Whether `cause` is one of aterm's causes for an account move
/// ([`crate::aterm::ACCOUNT_CAUSES`]).
fn account_cause(cause: &str) -> bool {
    crate::aterm::ACCOUNT_CAUSES.contains(&cause)
}

/// Whether `restart` is aterm's advice to move the session onto an account:
/// it names one, and its cause is an account's. The child is ended for it
/// only at the harness's own safe point ([`Supervisor::account_point`]). A
/// restart aterm ordered for another cause that carries a waiting move's
/// account ([`Supervisor::restart_joined`]) is no advice: it ends the child
/// as it would have without the account.
fn advised(restart: &Restart) -> bool {
    restart.account.is_some() && account_cause(&restart.cause)
}

/// Whether a child can be started with `account`'s login directory in its
/// environment: the directory is absolute and holds no NUL, which no
/// environment can carry ([`crate::aterm::Account::of_words`] holds a typed
/// request to the same rule). The default login (`home=-`) names none.
fn startable_home(account: &crate::aterm::Account) -> bool {
    account
        .home
        .as_ref()
        .is_none_or(|home| home.is_absolute() && !home.as_os_str().as_encoded_bytes().contains(&0))
}

/// The moment an account move's child may be ended at, as the harness's own
/// witness chose it ([`Supervisor::account_point`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AccountMoment {
    /// A quiet moment: the child's record reads `idle`.
    Quiet,
    /// The child stands at the usage limit whose row is stamped `at_ms`.
    Wall {
        /// The limit row's time, epoch milliseconds.
        at_ms: u64,
    },
}

/// Why an account move waits for its safe point
/// ([`Supervisor::account_point`]).
#[derive(Clone, Debug, PartialEq, Eq)]
struct AccountHold {
    /// The journal's wait words.
    why: String,
    /// Whether aterm's word holds it, which is asked again after
    /// [`link::LATER_RETRY_MS`]; the harness's own witness is read again on
    /// the next pass.
    on_aterm: bool,
}

/// The journal's word for a restart that asks for what the session already
/// runs on, a model ([`Supervisor::already_on`]) or an account
/// ([`Supervisor::account_refusal`]): nothing is ended for it.
const ALREADY_ON: &str = "already-on";

/// The cause of aterm's account move made at a usage limit, one of
/// [`crate::aterm::ACCOUNT_CAUSES`]; the other, `account-ahead`, is made
/// before a limit, at a turn end the agent reached by itself.
const ACCOUNT_AT_LIMIT: &str = "account";

/// Whether the child started again after `cause` is owed a resume nothing
/// else types: it was ended at a usage limit and started again off it, by
/// this harness's own move onto the fallback model ([`limit::CAUSE_LIMIT`]),
/// or by aterm's move onto another account made at the limit
/// ([`ACCOUNT_AT_LIMIT`], kept by [`Cause::Asked`] as `account`, or as
/// `account: <model>` where the restart named a model too). The new child
/// stands idle at the old limit, and nothing in the conversation asks it to
/// go on, as aterm's own rule for the same move says (`turn_end.rs`,
/// `account_resume`). A return to Fable at its reset, a move ahead of a
/// limit, a model restart aterm asked for, an update, a wall's restart and a
/// crash owe none: their turn end gets Autopilot's own act.
fn owes_resume(cause: &Cause) -> bool {
    match cause {
        Cause::Own { cause, .. } => cause == limit::CAUSE_LIMIT,
        Cause::Asked(why) => why.split(':').next() == Some(ACCOUNT_AT_LIMIT),
        Cause::Update | Cause::Wall(_) | Cause::Crash | Cause::Unmade => false,
    }
}

/// The relaunch `cause` makes, as one journal word.
const fn cause_word(cause: &Cause) -> &'static str {
    match cause {
        Cause::Update => "update",
        Cause::Asked(_) => "restart",
        Cause::Wall(_) => "wall",
        Cause::Own { .. } => "own",
        Cause::Crash => "crash",
        Cause::Unmade => "account-unmade",
    }
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
    // The claim is made, or asked for again later, before the first child
    // exists: aterm never sees an unclaimed child of a harness that has a
    // claim. Whatever aterm answers, the harness supervises the child.
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
    let limit = limit::State::new(child.spawned_ms);
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
        model: None,
        untyped_noted: String::new(),
        limit,
        limit_source: Box::new(limit::Transcript::default()),
        limit_said: LimitSaid::default(),
        turn_end: turn_end::Driver::default(),
        account: None,
        admitting: None,
        limit_adopted: false,
        described: None,
        upgrading: None,
    };
    if !sup.link.absent_for_good() {
        sup.note_untyped(link::UNTYPED);
    }
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
    /// a start on an account that was refused is made again on the account
    /// the session left ([`Supervisor::account_unmade`]); a moved build that
    /// refused to start falls back; an abnormal end nobody
    /// asked for may be a crash to answer ([`Supervisor::crash_begins`]).
    /// Every other end is the session's, and the harness ends the same way.
    fn on_end(&mut self, event: ChildEvent, pipe: &SignalPipe) {
        // A start on an account whose vendor ran is the session's move,
        // whatever ended the child since: booked before the child is
        // retired, so the retirement and every start after it name the
        // account. One nothing saw running is judged below.
        let record = self.record();
        self.account_seen(record.as_ref());
        let admitting = self.admitting.take();
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
                self.await_update(UpdateLaunch::Target(event), plan, None);
                self.resume_update(pipe);
            }
            // The restart stands until the child has ended or was abandoned
            // after SIGKILL ([`Supervisor::look`]): an end that comes late is
            // still the one the harness asked for.
            Ending::ForRestart {
                plan,
                cause,
                model,
                own,
                ..
            } => {
                let why = match model.as_deref() {
                    Some(model) if own => Cause::Own {
                        cause: cause.clone(),
                        model: self.shown_model(model),
                    },
                    None if own => Cause::Wall(cause.clone()),
                    None => Cause::Asked(cause.clone()),
                    Some(MODEL_LAUNCH) => Cause::Asked(format!("{cause}: the launch's own")),
                    Some(model) => Cause::Asked(format!("{cause}: {model}")),
                };
                self.relaunch(event, plan, &why, model.map(|model| (model, cause)), pipe);
            }
            // The start aterm's account move admitted ended before its
            // vendor wrote a record or took the terminal, with a status no
            // clean exit and no person's signal gives: the token helper
            // started nothing ([`crate::launch::NOT_STARTED`]), or the vendor
            // could not start on that login. No time bounds it: the start
            // still stands as admitted only because nothing ever saw its
            // vendor run, however long the helper took to say so. Judged
            // before the build move's fallback: the build is not what failed.
            Ending::None
                if let Some(admitting) = admitting
                    && refused_start(event) =>
            {
                self.account_unmade(admitting, event, pipe);
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
                    None,
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
        self.turn_end.repo_stop();
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
        // A plain restart that waited for this child has nothing left to
        // restart. One that names a model or an account stays: the relaunch
        // this crash may make is made on it ([`Supervisor::model_joins`]).
        if let Some(deferred) = self
            .restart_deferred
            .take_if(|deferred| !moves_conversation(&deferred.restart))
        {
            self.restart_ignored(&deferred.restart, "child-ended");
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
        let resume = match self.resume_plan(&record) {
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
        self.relaunch(event, plan, &Cause::Crash, None, pipe);
        self.ladder.relaunched(now_ms());
        self.journal.note(&format!(
            "crash-relaunch pid={} build={} streak={}",
            self.child.pid,
            self.child.build.number,
            self.ladder.streak()
        ));
    }

    /// The pinned plan that starts the running build again on `session_id`
    /// with `resume`: a [`vendor::Plan`] whose target is its old, on the
    /// account the session runs on ([`vendor::Plan::on_account`]: its login
    /// directory, and no account named).
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
        .map(|plan| plan.on_account(self.account.as_ref()))
        .map_err(|error| format!("vendor-plan {error}"))
    }

    /// aterm asked the claim holder to restart its child (`restart
    /// cause=<cause>[ model=<word>]`): end the child with SIGTERM, now, and
    /// start the same build on the same conversation when it has ended
    /// ([`Ending::ForRestart`]; a child still running [`RESTART_GRACE_MS`]
    /// later is sent SIGKILL, [`Supervisor::look`]). The request is aterm's, or
    /// a person's through aterm, so no safe point is waited for; it is no
    /// crash, and the back-off is not touched.
    ///
    /// A request that names a model is resumed on it
    /// ([`Supervisor::resume_plan_on`]); one that names none keeps the model
    /// the session was last moved onto. A model named by a request that was
    /// still waiting is kept when a later request names none: the one restart
    /// then answers both ([`Supervisor::restart_joined`]).
    ///
    /// A request that names an account (`cause=account to=<key>
    /// home=<dir|->`, [`crate::aterm::Account`]) is made the same way, on that
    /// account ([`vendor::Plan::with_account`]), unless it is not this
    /// harness's to make ([`Supervisor::account_refusal`]): the child is
    /// started again on the same conversation with the account's login
    /// directory, through aterm's token helper named its key, and the
    /// account is the session's from then on
    /// ([`Supervisor::account_moved`]). Like a model, it is asked of the
    /// conversation, and waits for a relaunch already under way and joins
    /// it.
    ///
    /// A request whose cause is `model` and whose model is the one the child
    /// already runs on ([`Supervisor::already_on`]) asks for nothing: it is
    /// journalled `ignored already-on` and the child is left alone. aterm and
    /// this harness may both decide the same move for Fable's limit, and
    /// whichever acts first makes the other's request this one.
    /// With no request waiting and nothing ending only: a waiting one is still
    /// owed its restart, and one that arrives while an ending stands is judged
    /// by the relaunch it joins, so a move already under way and the same
    /// move asked again end the child once.
    ///
    /// It is made only when all of these hold, and is otherwise journalled as
    /// `aterm restart cause=<cause>[ model=<word>] ignored <why>`:
    ///
    /// * nothing else is ending or has ended the child, and the child has not
    ///   ended unreaped (`child-ended`). A request that names a model is not
    ///   ignored for that: unless the session itself is ending
    ///   (`session-ending`), it waits (`deferred update-ending`,
    ///   `restart-ending`, `crash-pending` or `child-ended`) and joins the
    ///   relaunch that ending makes ([`Supervisor::model_joins`]), or is made
    ///   on its own once that ending is over without one;
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
    ///
    /// An account move is aterm's advice, sent to no person's order
    /// ([`advised`]): beyond the above it is made only at a safe point the
    /// harness judges by its own witness ([`Supervisor::account_point`],
    /// read once more just before the signal,
    /// [`Supervisor::account_still`]), for which it waits too; only while
    /// the running build can be started again and the token helper is still
    /// reachable; and only within [`ACCOUNT_OFFER_MS`] of being read,
    /// whatever it waited on ([`Supervisor::offered`]). A restart aterm
    /// orders for another cause that joins a waiting move
    /// ([`Supervisor::restart_joined`]) is none of that: it is made at once
    /// and carries the account while the offer stands.
    fn restart_asked(&mut self, restart: &Restart) {
        if let Some(why) = self.account_refusal(restart) {
            self.restart_ignored(restart, why);
            // aterm reads its answer off the claim alone: a move asked again
            // is answered by the claim said again, where a child stands for
            // it to name.
            if why == ALREADY_ON && self.ending == Ending::None {
                self.link_restate();
            }
            return;
        }
        self.restart_for(restart, None);
    }

    /// Why the account move `restart` asks for is not this harness's to
    /// make, as the journal's word; `None` for a request that is no account
    /// move (it names no account and its cause is none of
    /// [`crate::aterm::ACCOUNT_CAUSES`]), and for a move that may be made.
    /// The first that holds, in this order:
    ///
    /// 1. `account-assignment-unsupported`: the standing claim was not
    ///    granted `accounts` ([`link::Link::moves_accounts`]), so the move is
    ///    aterm's own engine's;
    /// 2. `account-word`: `to=` is no account key
    ///    ([`crate::aterm::account_key`]), or the request has an account's
    ///    cause and names no account at all. A label, a slug or an address
    ///    names an account only to the aterm that wrote it, and a start named
    ///    one could run on another account than the one aterm chose; a
    ///    request that names none asks for a move onto nothing, and its
    ///    cause is no order to end the child;
    /// 3. `account-home`: `home=` is no login directory a child can be given
    ///    ([`startable_home`]). The shared link reads any absolute path, a
    ///    NUL among its bytes too, and a child whose environment holds one
    ///    cannot be started: the move would end the child and then the
    ///    session;
    /// 4. `account-no-token-helper`: this launch reaches no token helper
    ///    ([`link::Link::token_helper`]). Started directly, the vendor would
    ///    run on whatever the login directory holds, with nobody to refuse a
    ///    directory that is not the account's;
    /// 5. `already-on` ([`ALREADY_ON`]): it names the account, key and login
    ///    directory both, that the session runs on or is being started on
    ///    ([`Supervisor::account_stands`]), and no model beside it. aterm may
    ///    send one move twice (as an event and as a record of its stream, or
    ///    again after a read it lost): the second ends no child.
    fn account_refusal(&self, restart: &Restart) -> Option<&'static str> {
        let account = restart.account.as_ref();
        if account.is_none() && !account_cause(&restart.cause) {
            return None;
        }
        if !self.link.moves_accounts() {
            return Some("account-assignment-unsupported");
        }
        let Some(account) = account.filter(|account| crate::aterm::account_key(&account.key))
        else {
            return Some("account-word");
        };
        if !startable_home(account) {
            return Some("account-home");
        }
        if !self.link.token_helper() {
            return Some("account-no-token-helper");
        }
        (restart.model.is_none() && self.account_stands(account)).then_some(ALREADY_ON)
    }

    /// Whether `account` is the one the session runs on, or the one a start
    /// under way is made on: a child that is being ended to be started on
    /// an account, or was started on it and has yet to be seen running
    /// ([`Admitting`]), is on its way there, and the same move asked
    /// meanwhile asks for nothing more.
    fn account_stands(&self, account: &crate::aterm::Account) -> bool {
        if let Some(admitting) = &self.admitting {
            return admitting.account == *account;
        }
        let pending = match &self.ending {
            Ending::ForRestart { plan, .. } | Ending::ForUpdate { plan, .. } => {
                plan.target.account.as_ref()
            }
            Ending::AwaitingUpdate {
                plan: Some(plan), ..
            } => plan.target.account.as_ref(),
            _ => None,
        };
        pending.or(self.account.as_ref()) == Some(account)
    }

    fn restart_for(&mut self, restart: &Restart, bound: Option<SessionRecord>) {
        // A plan made below starts from the account the session runs on: a
        // start on one that the vendor has taken up is booked first.
        if self.admitting.is_some() {
            let record = self.record();
            if self.account_seen(record.as_ref()) {
                self.link_restate();
            }
        }
        let (restart, before) = self.restart_joined(restart);
        let read_ms = read_at(&restart, before.as_ref(), now_ms());
        let Some(restart) = self.offered(restart, read_ms) else {
            return;
        };
        let restart = &restart;
        if before.is_none()
            && self.ending == Ending::None
            && restart.cause == "model"
            && self.already_on(restart.model.as_deref())
        {
            return self.restart_ignored(restart, ALREADY_ON);
        }
        let refusal = match &self.ending {
            Ending::None if sys::child_has_ended(self.child.pid) => Some("child-ended"),
            Ending::None => None,
            Ending::ForUpdate { .. } | Ending::AwaitingUpdate { .. } => Some("update-ending"),
            Ending::ForRestart { .. } => Some("restart-ending"),
            Ending::Passed(_) => Some("session-ending"),
            Ending::Crashed { .. } => Some("crash-pending"),
        };
        if let Some(why) = refusal {
            // A model or an account is asked of the conversation, whichever
            // child runs it: it waits for the relaunch this ending makes.
            // Only a session that is itself ending has no relaunch to wait
            // for.
            if moves_conversation(restart) && !matches!(self.ending, Ending::Passed(_)) {
                return self.restart_waits(restart, bound, before, why, false);
            }
            self.restart_ignored(restart, why);
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
            _ => return self.restart_ignored(restart, "record-changed"),
        };
        let resume = match self.resume_plan_on(&record, self.model_for(restart).as_deref()) {
            Ok(resume) => resume,
            Err(why) => return self.restart_ignored(restart, &why),
        };
        // An account move is aterm's advice, never a person's request: the
        // child is ended for it only at a point the harness itself judges
        // safe, and the move waits for one for as long as its offer stands.
        // A restart aterm ordered that carries a waiting move's account
        // ([`Supervisor::restart_joined`]) is not that: it is made now, as
        // it was before it joined, and the account goes with it.
        let moment = if advised(restart) {
            // Advice ends no child that could not be started again: the
            // session would end where a child left alone only waits.
            if !self.child.build.exe.is_file() {
                let why = format!("vendor-missing build={}", self.child.build.number);
                return self.restart_ignored(restart, &why);
            }
            // The helper the request was admitted on may be gone since it
            // began to wait.
            if !self.link.token_helper() {
                return self.restart_ignored(restart, "account-no-token-helper");
            }
            match self.account_point(&record) {
                Ok(moment) => Some(moment),
                Err(held) => {
                    return self.restart_waits(
                        restart,
                        Some(record),
                        before,
                        &held.why,
                        held.on_aterm,
                    );
                }
            }
        } else {
            None
        };
        match self.link_relaunch_word(true) {
            link::Relaunch::Permitted => {}
            link::Relaunch::Denied(why) => return self.restart_ignored(restart, &why),
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
            Ok(plan) => plan.with_account(restart.account.as_ref()),
            Err(why) => return self.restart_ignored(restart, &why),
        };
        // The exchanges and the plan took time: the moment the move was
        // judged at must still stand, on a fresh reading, just before the
        // signal.
        if let Some(moment) = moment
            && !self.account_still(&record, moment)
        {
            let why = "wait changed-while-deciding";
            return self.restart_waits(restart, Some(record), before, why, false);
        }
        if let Err(why) = self.restart_signal(restart, &record, plan, false) {
            self.restart_ignored(restart, &why);
        }
    }

    /// `restart` while its offer stands: an account move the harness read
    /// at `read_ms` is made only within [`ACCOUNT_OFFER_MS`] of that. Past
    /// it the move is journalled once (`aterm restart … ignored
    /// account-offer-expired`) and is over. What the request asked for
    /// besides the account has no such bound and comes back without it: a
    /// model it named, which was asked of the conversation, and a restart
    /// aterm ordered (a cause that is no account's: a person's, a stall's)
    /// that joined the waiting move and so carries its account
    /// ([`Supervisor::restart_joined`]). aterm has answered the asker of
    /// such a restart that it is under way, and the account's wait must not
    /// lose it. `None` when nothing is left to make. A request that names
    /// no account comes back as it is.
    fn offered(&self, mut restart: Restart, read_ms: u64) -> Option<Restart> {
        if restart.account.is_none() || now_ms().saturating_sub(read_ms) < ACCOUNT_OFFER_MS {
            return Some(restart);
        }
        self.restart_ignored(&restart, "account-offer-expired");
        if restart.model.is_none() && account_cause(&restart.cause) {
            return None;
        }
        restart.account = None;
        Some(restart)
    }

    /// Whether the child may be ended now for an account move aterm asked
    /// for, by the harness's own witness first and then one fresh policy and
    /// status pair ([`Supervisor::link_safe_point`]); `record` is the
    /// child's own session record as just read. aterm sent the move, so its
    /// status alone does not say the child rests:
    ///
    /// * Where the harness's own look reads the child standing at a usage
    ///   limit ([`limit::Sight::wall`]) the moment is a wall
    ///   ([`link::Point::Wall`], which does not ask that the agent be idle),
    ///   unless the record reads `busy` in a run that has not had the
    ///   limit's patience ([`Supervisor::busy_held`]): a turn begun after
    ///   the refusal is a turn of its own, and the limit's row stays the
    ///   newest until that turn's first answer is written.
    /// * Otherwise it is a quiet moment under `relaunch`, whatever cause
    ///   aterm's request names, and the record must read `idle`: a turn in
    ///   flight is not ended on aterm's word that the agent is idle. aterm
    ///   decides a move at a turn's end, so no quiet period is asked of the
    ///   record; what a person is doing is aterm's to say.
    ///
    /// aterm's word that the session is at a wall selects nothing: a child
    /// that is working is not ended on it. `Ok` is the moment that held,
    /// which is read once more just before the signal
    /// ([`Supervisor::account_still`]).
    ///
    /// # Errors
    /// What holds, as the journal's wait words, and whether it is aterm's
    /// to end: the record's `wait busy`, aterm's own hold, or `wait aterm
    /// unread` where no standing claim answered, since an account move is
    /// made only under one.
    fn account_point(&mut self, record: &SessionRecord) -> Result<AccountMoment, AccountHold> {
        let busy = || AccountHold {
            why: format!("wait {}", Wait::Busy.word()),
            on_aterm: false,
        };
        let (moment, point) = match self.limit_sight().wall {
            Some(wall) => {
                if Self::busy_held(record, wall.at_ms, now_ms()) {
                    return Err(busy());
                }
                (AccountMoment::Wall { at_ms: wall.at_ms }, link::Point::Wall)
            }
            None => {
                if record.status != profile::STATUS_IDLE {
                    return Err(busy());
                }
                (AccountMoment::Quiet, link::Point::Quiet(Scope::Relaunch))
            }
        };
        let on_aterm = |why: String| AccountHold {
            why,
            on_aterm: true,
        };
        match self.link_safe_point(point) {
            link::Verdict::Safe => Ok(moment),
            link::Verdict::Hold(why) => Err(on_aterm(why)),
            link::Verdict::Local => Err(on_aterm("wait aterm unread".to_string())),
        }
    }

    /// The last look before SIGTERM at the moment an account move was
    /// judged safe at ([`Supervisor::account_point`]) for `record`: the
    /// exchanges with aterm and the plan took time. A quiet moment still
    /// stands while the child's record reads `idle` in the same idle run; a
    /// wall while the harness's witness still reads that limit and the
    /// record is not `busy` in a run the limit's patience covers.
    fn account_still(&mut self, record: &SessionRecord, moment: AccountMoment) -> bool {
        let Some(fresh) = self.record() else {
            return false;
        };
        match moment {
            AccountMoment::Quiet => {
                fresh.status == profile::STATUS_IDLE
                    && fresh.status_updated_at_ms == record.status_updated_at_ms
            }
            AccountMoment::Wall { at_ms } => {
                self.limit_sight()
                    .wall
                    .is_some_and(|wall| wall.at_ms == at_ms)
                    && !Self::busy_held(&fresh, at_ms, now_ms())
            }
        }
    }

    /// End the child with SIGTERM for the restart `restart`, to be started
    /// again from `plan` on `record`'s conversation when it has ended
    /// ([`Ending::ForRestart`]): aterm's request, or the harness's `own` model
    /// move. One journal line, `aterm restart …` or `own restart …`, with the
    /// signal, the child, the conversation, the build and the resume line.
    /// The pinned plan and the selected conversation must still stand after
    /// every exchange that came before the signal.
    ///
    /// # Errors
    /// `child-ended`: the child ended unreaped while the plan was made.
    /// `record-changed`: it no longer names the conversation the plan
    /// resumes. `signal-failed`: the kernel would not signal the child. In
    /// each case nothing was sent and nothing is ending.
    fn restart_signal(
        &mut self,
        restart: &Restart,
        record: &SessionRecord,
        plan: vendor::Plan,
        own: bool,
    ) -> Result<(), String> {
        if sys::child_has_ended(self.child.pid) {
            return Err("child-ended".to_string());
        }
        if !self
            .record()
            .is_some_and(|current| record.same_child_conversation(&current))
        {
            return Err("record-changed".to_string());
        }
        if !self.signal_child(libc::SIGTERM) {
            return Err("signal-failed".to_string());
        }
        self.journal.note(&format!(
            "{} restart {} SIGTERM pid={} session={} build={} argv={}",
            if own { "own" } else { "aterm" },
            ask_words(restart),
            self.child.pid,
            record.session_id,
            self.child.build.number,
            plan.resume.join("\u{1f}")
        ));
        self.ending = Ending::ForRestart {
            sent: Instant::now(),
            plan,
            cause: restart.cause.clone(),
            model: restart.model.clone(),
            killed: false,
            own,
        };
        Ok(())
    }

    /// The launch's own `--model`, when it named one ([`profile::model_of`]).
    /// `None` where it named none, and Claude Code's own settings chose.
    fn launch_model(&self) -> Option<String> {
        launch_argv(&self.setup.args)
            .as_deref()
            .and_then(profile::model_of)
            .map(str::to_string)
    }

    /// The model the session runs on, as far as a command line names it: the
    /// one every resume line names in place of the launch's
    /// ([`Supervisor::kept_model`]), else its launch's `--model`. `None` when
    /// neither names one, and Claude Code's own settings chose.
    fn named_model(&self) -> Option<String> {
        self.kept_model().or_else(|| self.launch_model())
    }

    /// The model every resume line names in place of the launch's `--model`:
    /// the one a restart last moved the session onto ([`Supervisor::model`]),
    /// else the launch's own model words while a return onto them has yet to
    /// answer ([`Supervisor::launch_pin`]). `None` is the launch's own line.
    fn kept_model(&self) -> Option<String> {
        self.model
            .clone()
            .or_else(|| self.launch_pin().map(str::to_string))
    }

    /// The words a move back onto the launch's own model put on its line in
    /// place of the launch's none ([`limit::back_to`]'s alias), named on every
    /// later resume line too while that return has yet to answer
    /// ([`limit::Phase::OnLaunch`] with `returned`, [`limit::State::back_on_launch`]).
    /// A line with no `--model` takes up the conversation's newest model
    /// (docs/measured/model-effort-key-2026-11-13.md), and until the returned
    /// child answers that is still the model it was moved off: a vendor update,
    /// a crash relaunch or a plain restart made in that window would put the
    /// session back on it for good. Once the child has answered
    /// ([`limit::State::answered`]) the newest model is the launch's, and the
    /// launch's own line, which names none, is reproduced exactly. `None`
    /// where the launch named a model (its own line says it), and where a
    /// model is kept.
    fn launch_pin(&self) -> Option<&'static str> {
        if self.model.is_some()
            || !matches!(self.limit.phase, limit::Phase::OnLaunch { returned: true })
        {
            return None;
        }
        limit::back_to(self.launch_model().as_deref())
    }

    /// Whether a model restart that names `model` asks for what the child
    /// already runs on: a model is the one [`Supervisor::named_model`] names,
    /// and [`MODEL_LAUNCH`] is the launch's own line, which every resume line
    /// is while nothing is kept ([`Supervisor::model`]). A restart that names
    /// no model asks for no move.
    ///
    /// For a launch that named no model, with nothing kept, the alias
    /// [`limit::back_to`] puts on a return's line (`fable`, in any case) is
    /// aterm's word for the same move back (an aterm that predates
    /// `model=launch` sends it): it is already-on while the launch pin stands
    /// ([`Supervisor::launch_pin`]) and once the running child itself states
    /// it runs on Fable ([`Supervisor::child_states_fable`]). Any other model
    /// word, a Fable id or the alias with a window among them, is a choice of
    /// its own and is made.
    fn already_on(&self, model: Option<&str>) -> bool {
        match model {
            None => false,
            Some(MODEL_LAUNCH) => self.model.is_none(),
            Some(named) if self.named_model().as_deref() == Some(named) => true,
            Some(named) => {
                self.model.is_none()
                    && limit::back_to(self.launch_model().as_deref())
                        .is_some_and(|alias| alias.eq_ignore_ascii_case(named))
                    && (self.launch_pin().is_some() || self.child_states_fable())
            }
        }
    }

    /// Whether the running child itself states that it runs on Fable: the
    /// facts reader has read this child ([`model_facts::Reader::pid`]), and
    /// the newest model its transcript states since it started is a Fable
    /// display name ([`limit::is_fable_display`]).
    fn child_states_fable(&self) -> bool {
        self.facts
            .pid()
            .is_some_and(|pid| u32::try_from(self.child.pid).ok() == Some(pid))
            && self
                .facts
                .facts()
                .model
                .shown()
                .is_some_and(|model| limit::is_fable_display(model))
    }

    /// `restart` as it is acted on, and the request that was waiting before
    /// it, which it replaces: the latest request's cause, and its model and
    /// its account, or the waiting request's where it names none. A restart
    /// aterm orders that so carries a waiting move's account is still an
    /// order ([`advised`]).
    fn restart_joined(&mut self, restart: &Restart) -> (Restart, Option<Deferred>) {
        let before = self.restart_deferred.take();
        let restart = Restart {
            cause: restart.cause.clone(),
            model: restart.model.clone().or_else(|| {
                before
                    .as_ref()
                    .and_then(|before| before.restart.model.clone())
            }),
            account: restart.account.clone().or_else(|| {
                before
                    .as_ref()
                    .and_then(|before| before.restart.account.clone())
            }),
        };
        (restart, before)
    }

    /// aterm's restart request, read while the child it was sent for is being
    /// replaced ([`Supervisor::relaunch`], [`Supervisor::fallback_from_move`]).
    /// The child started next is no stall and nobody's request, so a request
    /// that names no model is not taken for it (`ignored child-replaced`).
    /// One that names a model is kept (`deferred child-replaced`): the model
    /// was asked of the conversation, and the relaunch under way is made on
    /// it ([`Supervisor::model_joins`]), or the request is made on its own
    /// once the new child runs.
    fn restart_for_replaced(&mut self, restart: &Restart) {
        // The relaunch under way decides what the session runs on next, so
        // nothing is `already-on` here: the same move asked again joins it.
        if let Some(why) = self
            .account_refusal(restart)
            .filter(|why| *why != ALREADY_ON)
        {
            return self.restart_ignored(restart, why);
        }
        let (joined, before) = self.restart_joined(restart);
        if moves_conversation(&joined) {
            self.restart_waits(&joined, None, before, "child-replaced", false);
        } else {
            self.restart_deferred = before;
            self.restart_ignored(restart, "child-replaced");
        }
    }

    /// A restart that names a model and waited for this relaunch
    /// ([`Supervisor::restart_asked`]) is made by it: `plan`, pinned before
    /// the request came, is resumed on the model instead, every pin kept
    /// ([`vendor::Plan::resumed_with`]), and the request is over, journalled
    /// as `aterm restart cause=<cause> model=<word> joined
    /// <update|restart|own|crash> argv=<resume line>` (`own`: the relaunch of
    /// the harness's own model move for Fable's limit, which the request
    /// therefore decides, so the child is ended once for both). The model and
    /// the request's cause come back for [`Supervisor::model_moved`], in place
    /// of `moved`, the model the plan's own line carried. A resume line that
    /// cannot be written leaves the plan as it was pinned, and the request is
    /// journalled as ignored.
    fn model_joins(
        &mut self,
        plan: vendor::Plan,
        cause: &Cause,
        moved: Option<(String, String)>,
    ) -> (vendor::Plan, Option<(String, String)>) {
        let Some(deferred) = self
            .restart_deferred
            .take_if(|deferred| moves_conversation(&deferred.restart))
        else {
            return (plan, moved);
        };
        // An account move whose offer has passed joins nothing. A restart
        // aterm ordered that carried it asks for nothing more than this
        // relaunch makes.
        let Some(restart) = self.offered(deferred.restart, deferred.read_ms) else {
            return (plan, moved);
        };
        if !moves_conversation(&restart) {
            self.restart_ignored(&restart, "child-replaced");
            return (plan, moved);
        }
        // An account the request names is the relaunch's whatever its model.
        let plan = plan.with_account(restart.account.as_ref());
        let Some(model) = restart.model.clone() else {
            self.journal.note(&format!(
                "aterm restart {} joined {} argv={}",
                ask_words(&restart),
                cause_word(cause),
                plan.resume.join("\u{1f}")
            ));
            return (plan, moved);
        };
        match self.resume_line(&plan.session_id, self.model_for(&restart).as_deref()) {
            Ok(resume) => {
                self.journal.note(&format!(
                    "aterm restart {} joined {} argv={}",
                    ask_words(&restart),
                    cause_word(cause),
                    resume.join("\u{1f}")
                ));
                (plan.resumed_with(resume), Some((model, restart.cause)))
            }
            Err(why) => {
                self.restart_ignored(&restart, &why);
                (plan, moved)
            }
        }
    }

    /// The model the child is resumed on for `restart`, as its resume line
    /// names it: the one it names, or with no `model=` the one every resume
    /// line names ([`Supervisor::kept_model`]). `None` is the launch's own
    /// line. [`MODEL_LAUNCH`] asks for the launch's own model whatever the
    /// session was moved onto: the launch's own line where it named a model,
    /// and [`limit::back_to`]'s alias where it named none, because a resume
    /// line with no `--model` takes up the conversation's newest model
    /// (Claude Code restores it onto a process nothing pinned,
    /// docs/measured/model-effort-key-2026-11-13.md), which is the one the
    /// session was moved onto. The harness's own return at Fable's reset is
    /// such a restart ([`Supervisor::limit_return`]).
    fn model_for(&self, restart: &Restart) -> Option<String> {
        match restart.model.as_deref() {
            Some(MODEL_LAUNCH) => {
                limit::back_to(self.launch_model().as_deref()).map(str::to_string)
            }
            Some(named) => Some(named.to_string()),
            None => self.kept_model(),
        }
    }

    /// The model a child resumed for a restart that names `model` runs on, as
    /// the terminal and the journal say it: the model itself, and for
    /// [`MODEL_LAUNCH`] the launch's own `--model`, or [`limit::back_to`]'s
    /// alias where it named none ([`Supervisor::model_for`]).
    fn shown_model(&self, model: &str) -> String {
        if model != MODEL_LAUNCH {
            return model.to_string();
        }
        let launched = self.launch_model();
        limit::back_to(launched.as_deref())
            .map(str::to_string)
            .or(launched)
            .unwrap_or_default()
    }

    /// The child has just been resumed on `model` (`cause`: aterm's, or the
    /// harness's own [`limit::CAUSE_LIMIT`] or [`limit::CAUSE_RESET`]): from
    /// here every resume line names it ([`Supervisor::resume_plan`]), until a
    /// restart names another. A move that names the launch's own model again
    /// leaves nothing to keep: the launch's line already says it.
    ///
    /// [`MODEL_LAUNCH`] is no model: the child was resumed on the launch's
    /// own model ([`Supervisor::model_for`]), and whatever was kept is
    /// dropped, so every later resume line is the launch's own once the
    /// child has answered there, and names the launch pin until then
    /// ([`Supervisor::launch_pin`]). aterm's `model=launch` and the harness's
    /// own return at Fable's reset both end here. Its line says so apart from
    /// a move onto a model that happens to be the launch's: `model-move
    /// to=launch launch-model=<the launch's --model, or -> …`.
    ///
    /// This is the one place the state of Fable's limit moves with the
    /// session ([`Supervisor::limit`]), whoever asked for the move, so aterm's
    /// requests and the harness's own moves stay in step:
    ///
    /// * onto [`limit::FALLBACK`] by the harness's own decision, or by anyone
    ///   while a Fable session's child that just ended stood at Fable's limit:
    ///   the session is off Fable until the limit's reset
    ///   ([`limit::State::left_fable`]). A session whose launch, or whose last
    ///   move, named another model is not Fable's, and is never brought "back"
    ///   onto a model it did not ask for;
    /// * onto [`limit::FALLBACK`] again while it is off Fable: nothing moves
    ///   but the floor;
    /// * onto the launch's own model ([`MODEL_LAUNCH`], or the launch's own
    ///   `--model` by name) from a model the session's lines named in its
    ///   place: a return that has yet to answer
    ///   ([`limit::State::back_on_launch`]), so the launch pin stands until it
    ///   does;
    /// * onto anything else: the session is on its launch's terms again
    ///   ([`limit::State::on_launch`]), a return if it was off Fable.
    ///
    /// The rows before the child just started are spent either way. One
    /// journal line per move: `model-move to=<model> from=<model|-> cause=<cause>
    /// kept=switched|launch back-at=<epoch s|->`, the last word naming when the
    /// session goes back (`-` when it is not off Fable).
    fn model_moved(&mut self, model: String, cause: &str) {
        let launch = launch_argv(&self.setup.args);
        let launched = launch.as_deref().and_then(profile::model_of);
        // What the lines named in place of the launch's own before this move.
        let kept_before = self.kept_model();
        let from = kept_before
            .as_deref()
            .or(launched)
            .unwrap_or("-")
            .to_string();
        let to_launch = model == MODEL_LAUNCH;
        let kept = if to_launch || launched == Some(model.as_str()) {
            "launch"
        } else {
            "switched"
        };
        // Whether the session was Fable's before this move: its launch and
        // what it was last moved onto each name Fable, or nothing.
        let was_fable =
            limit::admits_fallback(launched) && limit::admits_fallback(kept_before.as_deref());
        // The source still reads the rows of the child that just ended: the
        // model reader has not looked at the new one yet.
        let wall = self.limit_sight().fable().cloned();
        let floor = self.child.spawned_ms;
        let back = match self.limit.phase {
            limit::Phase::OffFable { back_at_ms } if model == limit::FALLBACK => {
                self.limit.floor_ms = floor;
                Some(back_at_ms)
            }
            _ if model == limit::FALLBACK
                && (cause == limit::CAUSE_LIMIT || (was_fable && wall.is_some())) =>
            {
                Some(self.limit.left_fable(
                    wall.as_ref(),
                    now_ms(),
                    floor,
                    self.setup.opts.reset_grace_ms,
                ))
            }
            _ if kept == "launch" && kept_before.is_some() => {
                self.limit.back_on_launch(floor);
                None
            }
            _ => {
                self.limit.on_launch(floor);
                None
            }
        };
        let to = if to_launch {
            format!("{MODEL_LAUNCH} launch-model={}", launched.unwrap_or("-"))
        } else {
            model.clone()
        };
        self.journal.note(&format!(
            "model-move to={to} from={from} cause={cause} kept={kept} back-at={}",
            back.map_or_else(|| "-".to_string(), |at| (at / 1_000).to_string())
        ));
        self.model = (kept == "switched").then_some(model);
        self.limit_said = LimitSaid::default();
    }

    /// Keep the request `restart` for a later pass because of `why`,
    /// journalled as `aterm restart cause=<cause>[ model=<word>] deferred
    /// <why>` unless that is what was waiting already (`before`). A request
    /// that waits on aterm (`on_aterm`) is asked again after
    /// [`link::LATER_RETRY_MS`], and once it has waited
    /// [`link::RECLAIM_HOLD_MS`] in all it is ignored. `record` is the
    /// conversation already selected for it, when one was. An account move
    /// keeps the time it was first read ([`read_at`]), which its offer is
    /// counted from however often it waits again.
    fn restart_waits(
        &mut self,
        restart: &Restart,
        record: Option<SessionRecord>,
        before: Option<Deferred>,
        why: &str,
        on_aterm: bool,
    ) {
        let now = now_ms();
        let same = before
            .as_ref()
            .filter(|before| before.restart == *restart && before.why == why);
        let on_aterm_since_ms = on_aterm.then(|| {
            before
                .as_ref()
                .filter(|before| before.restart.cause == restart.cause)
                .and_then(|before| before.on_aterm_since_ms)
                .unwrap_or(now)
        });
        if on_aterm_since_ms.is_some_and(|since| now.saturating_sub(since) >= link::RECLAIM_HOLD_MS)
        {
            return self.restart_ignored(restart, why);
        }
        if same.is_none() {
            self.journal.note(&format!(
                "aterm restart {} deferred {why}",
                ask_words(restart)
            ));
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
            read_ms: read_at(restart, before.as_ref(), now),
        });
    }

    /// Journal a restart request that is not made: `aterm restart
    /// cause=<cause>[ model=<word>] ignored <why>`.
    fn restart_ignored(&self, restart: &Restart, why: &str) {
        self.journal.note(&format!(
            "aterm restart {} ignored {why}",
            ask_words(restart)
        ));
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

    fn await_update(
        &mut self,
        launch: UpdateLaunch,
        plan: vendor::Plan,
        moved: Option<(String, String)>,
    ) {
        self.child.stopped = false;
        self.ending = Ending::AwaitingUpdate {
            launch,
            plan: Some(plan),
            moved,
        };
    }

    fn stop_pending_update(&self) {
        self.journal
            .note("replacement-stopped waiting for foreground");
        sys::stop_self();
    }

    fn resume_update(&mut self, pipe: &SignalPipe) {
        let Ending::AwaitingUpdate {
            launch,
            plan,
            moved,
        } = &mut self.ending
        else {
            return;
        };
        if !sys::in_foreground(0) {
            self.note_wait("replacement-wait foreground");
            return;
        }
        let launch = *launch;
        let plan = plan.take().expect("automatic replacement owns its plan");
        let moved = moved.take();
        match launch {
            UpdateLaunch::Target(event) => {
                self.relaunch(event, plan, &Cause::Update, moved, pipe);
            }
            UpdateLaunch::Fallback(why) => self.fall_back(why, plan, moved, pipe),
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
        let mut command = launch.command()?;
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
    /// update, on the same build after a restart aterm asked for, the
    /// harness's own model move or a crash (`cause`). The terminal is told of
    /// the resume ([`relaunch_notice`], [`restart_notice`], [`limit_notice`],
    /// [`crash_notice`]) only once the build has been
    /// started. A same-build start that fails ends the harness: with the
    /// crashed child's status after a crash, and otherwise with
    /// [`EXIT_VENDOR_MISSING`], because the child ended at the harness's own
    /// signal and its status would say nothing.
    ///
    /// `moved` is the model `plan`'s resume line carries because a restart
    /// named it, with that restart's cause. A model restart that waited for
    /// this relaunch replaces it ([`Supervisor::model_joins`]). Either is
    /// kept for the session once the child runs
    /// ([`Supervisor::model_moved`]): every start below is made on the
    /// plan's resume line, the fallback to the previous build included.
    fn relaunch(
        &mut self,
        event: ChildEvent,
        plan: vendor::Plan,
        cause: &Cause,
        moved: Option<(String, String)>,
        pipe: &SignalPipe,
    ) {
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
        // unread): a restart request among it is not taken for the next one,
        // unless it names a model, which this relaunch is then made on.
        self.link_events(true);
        let (plan, moved) = self.model_joins(plan, cause, moved);
        let (plan, unmade) = self.account_startable(plan);
        // A start aterm asked for that is made on the account the session
        // left is told as that, and owes no resume at a limit it never left.
        let left = Cause::Unmade;
        let cause = if unmade && matches!(cause, Cause::Asked(_)) {
            &left
        } else {
            cause
        };
        let account = plan.target.account.clone();
        let (old, new, session_id, resume) = (
            &plan.old.build,
            &plan.target.build,
            &plan.session_id,
            &plan.resume,
        );
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
                    Cause::Wall(why) => wall_notice(new.version.as_deref(), why, session_id),
                    // A model restart that joined names the model resumed.
                    Cause::Own { cause, model } => limit_notice(
                        cause,
                        &moved
                            .as_ref()
                            .map_or_else(|| model.clone(), |(model, _)| self.shown_model(model)),
                        session_id,
                    ),
                    Cause::Crash => crash_notice(new.version.as_deref(), event, session_id),
                    Cause::Unmade => unmade_notice(new.version.as_deref(), session_id),
                });
                child
            }
            Ok(None) => {
                self.await_update(UpdateLaunch::Target(event), plan, moved);
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
                    self.turn_end.repo_stop();
                    self.link_release();
                    std::process::exit(EXIT_VENDOR_MISSING);
                }
                self.fall_back(FallbackCause::FailedSpawn, plan, moved, pipe);
                return;
            }
        };
        let session = plan.session_id.clone();
        let admitted = account.map(|account| (account, plan.clone().unmade()));
        let moved_plan = (new.number != old.number).then_some(plan);
        // A child moved off a usage limit, onto the fallback model or onto
        // another account, stands at the wall it stopped at, and is owed a
        // resume nothing else types.
        self.install_replacement(started, moved_plan, owes_resume(cause));
        self.limit_moves(moved, &session);
        if let Some((account, before)) = admitted {
            self.account_admitting(account, before, cause);
        }
        self.link_reannounce();
    }

    /// `plan` as it may be started now, and whether its account was taken
    /// off it. A plan on an account aterm named ([`vendor::Plan::with_account`])
    /// is started only through aterm's token helper, and the helper that was
    /// reachable when the move was admitted may be gone by the time the old
    /// child has ended (an aterm update replaces it; a move may wait on a
    /// relaunch for as long as its offer stands). Then the move is not made:
    /// one journal line, `account-move unmade to=<key> why=no-token-helper`,
    /// and the plan goes back to what was pinned before the account
    /// ([`vendor::Plan::unmade`]), so the conversation is started again on
    /// the account it left and no claim ever names the other. The start
    /// itself refuses the same way ([`vendor::Launch::command`]), whatever
    /// this look found.
    fn account_startable(&mut self, plan: vendor::Plan) -> (vendor::Plan, bool) {
        let Some(account) = plan
            .target
            .account
            .as_ref()
            .filter(|_| !self.link.token_helper())
        else {
            return (plan, false);
        };
        self.journal.note(&format!(
            "account-move unmade to={} why=no-token-helper",
            account.key
        ));
        (plan.unmade(), true)
    }

    /// `child` now runs in place of the one that ended. `moved` is the vendor
    /// move it came from, when its build is the plan's target; `owes_resume`
    /// says the turn-end duty owes it a resume ([`owes_resume`]). The caller
    /// books the model the start moved ([`Supervisor::limit_moves`]), notes
    /// the account it was made on ([`Supervisor::account_admitting`]) and
    /// announces the child ([`Supervisor::link_reannounce`]).
    fn install_replacement(
        &mut self,
        child: Child,
        moved: Option<vendor::Plan>,
        owes_resume: bool,
    ) {
        self.child = child;
        self.ending = Ending::None;
        self.moved = moved;
        self.turn_end.child_restarted(owes_resume);
        self.noted.clear();
        self.seen_target = None;
        self.restart_moot();
    }

    /// The start that moved the session of `session` carried the model
    /// `moved` names, with the cause of the restart that named it: it is the
    /// session's from here ([`Supervisor::model_moved`]), and what a harness
    /// process after this one takes up is written
    /// ([`Supervisor::limit_keep`]). An account the same start was made on is
    /// booked after it, and only once its vendor runs
    /// ([`Supervisor::account_admitting`], [`Supervisor::account_seen`]): the
    /// model move reads the wall the child that ended stood at, and the
    /// account's start spends that child's rows (it raises the limit's floor
    /// to the new child's start). Read after it, a move onto the fallback
    /// that aterm joined to an account move at Fable's limit would see no
    /// wall and keep no return to Fable.
    fn limit_moves(&mut self, moved: Option<(String, String)>, session: &str) {
        if let Some((model, asked)) = moved {
            self.model_moved(model, &asked);
            self.limit_keep(session);
        }
    }

    /// The child was just started (`cause`) on `account`, from a plan that
    /// was `before` as it was pinned ahead of the account
    /// ([`vendor::Plan::unmade`]): the start stands as [`Admitting`] until
    /// its vendor is seen running ([`Supervisor::account_seen`]) or it ends
    /// having started nothing ([`Supervisor::account_unmade`]). Nothing is
    /// booked and no claim names the account yet. The limit's witness reads
    /// from the new child on at once: the rows before it are the walls of
    /// the account the child that ended ran on, and a look made while the
    /// start is pending must not take one of them for the new child's.
    fn account_admitting(
        &mut self,
        account: crate::aterm::Account,
        before: vendor::Plan,
        cause: &Cause,
    ) {
        let cause = match cause {
            Cause::Asked(why) | Cause::Wall(why) => why.clone(),
            other => cause_word(other).to_string(),
        };
        let floor_ms = self.limit.floor_ms;
        self.limit.floor_ms = floor_ms.max(self.child.spawned_ms);
        self.admitting = Some(Admitting {
            account,
            before,
            cause,
            floor_ms,
        });
    }

    /// Whether the vendor of the start on an account that stands
    /// ([`Admitting`]) is seen running, by this look's `record` of the child
    /// or by a terminal that has left the harness's saved modes (aterm's
    /// token helper writes no record and never touches the terminal). Seen,
    /// the start is the session's move: booked
    /// ([`Supervisor::account_moved`]), Fable's limit moved with it
    /// ([`Supervisor::limit_rebased`], since the account decides which Fable
    /// limit the session stands at) and kept ([`Supervisor::limit_keep`]).
    /// The caller says the claim again ([`Supervisor::link_restate`]), which
    /// names the account from here (`login=<key>`) and not before: aterm
    /// counts the move made on that word, and a wrapper that is still
    /// deciding is no account's child. No lapse of time stands in for the
    /// sight.
    fn account_seen(&mut self, record: Option<&SessionRecord>) -> bool {
        if record.is_none() && self.terminal_is_original() {
            return false;
        }
        let Some(admitting) = self.admitting.take() else {
            return false;
        };
        let session = admitting.before.session_id.clone();
        if self.account_moved(admitting.account, &admitting.cause) && self.limit_rebased() {
            self.limit_keep(&session);
        }
        true
    }

    /// The start on the account `admitting` names ended (`event`) before its
    /// vendor was seen running: the move is not made. One journal line,
    /// `account-move unmade to=<key> why=not-started child=<event>`; the
    /// limit's witness reads the old account's rows again; and the
    /// conversation is started again from the plan as it was pinned before
    /// the move ([`Admitting::before`]): the same build and resume line, on
    /// the account and the credentials it left. No claim ever named the
    /// refused account, so the claim announced for the new child states the
    /// login the session had. It is no crash: the back-off is not touched.
    fn account_unmade(&mut self, admitting: Admitting, event: ChildEvent, pipe: &SignalPipe) {
        self.journal.note(&format!(
            "account-move unmade to={} why=not-started child={event:?}",
            admitting.account.key
        ));
        self.limit.floor_ms = admitting.floor_ms;
        self.relaunch(event, admitting.before, &Cause::Unmade, None, pipe);
    }

    /// The session was just started on another account
    /// ([`Supervisor::account_moved`]). Off Fable, the time it goes back was
    /// read off the old account's wall, which says nothing of the new one's:
    /// it goes back at the next point a return may be made
    /// ([`limit::State::account_moved`]), where the new account's Fable
    /// answers for itself. One journal line where it was off Fable:
    /// `limit-account to=<key> back-at=<epoch s>`. Whether it was.
    fn limit_rebased(&mut self) -> bool {
        let Some(back) = self.limit.account_moved(now_ms()) else {
            return false;
        };
        self.journal.note(&format!(
            "limit-account to={} back-at={}",
            self.account
                .as_ref()
                .map_or("-", |account| account.key.as_str()),
            back / 1_000
        ));
        true
    }

    /// The vendor runs on `account`, which a start made for `why` put it on
    /// ([`Supervisor::account_seen`]): from here the account is the
    /// session's, for every later start and every claim (`login=`,
    /// [`link::Link::set_login`]). A start onto another account than the one
    /// before is a move: one journal line, `account-move to=<key>
    /// from=<key|-> cause=<why> login=directory|default` (keys only;
    /// no directory and no credential is journalled), and the limit's
    /// witness reads from the new child on, since the rows before it are the
    /// old account's walls. How often a session may be moved is aterm's to
    /// bound: it is the one that decides a move. Whether the start moved the
    /// account.
    fn account_moved(&mut self, account: crate::aterm::Account, why: &str) -> bool {
        self.link.set_login(Some(&account.key));
        if self.account.as_ref() == Some(&account) {
            return false;
        }
        self.journal.note(&format!(
            "account-move to={} from={} cause={why} login={}",
            account.key,
            self.account
                .as_ref()
                .map_or("-", |before| before.key.as_str()),
            if account.home.is_some() {
                "directory"
            } else {
                "default"
            }
        ));
        self.limit.floor_ms = self.limit.floor_ms.max(self.child.spawned_ms);
        self.account = Some(account);
        true
    }

    /// Keep where the conversation `session` is with respect to Fable's
    /// limit beside the journal ([`limit::Kept`]), for a harness process
    /// after this one: written while it is off Fable, removed otherwise. A
    /// write that fails is journalled (`limit-kept failed <error>`) and
    /// changes nothing here.
    fn limit_keep(&self, session: &str) {
        let Some(path) = self
            .journal
            .path()
            .and_then(|journal| limit::kept_path(journal, session))
        else {
            return;
        };
        let kept = match self.limit.phase {
            limit::Phase::OffFable { back_at_ms } => limit::keep(
                &path,
                &limit::Kept {
                    back_at_ms,
                    walled_returns: self.limit.walled_returns,
                },
            ),
            limit::Phase::OnLaunch { .. } => limit::forget(&path),
        };
        if let Err(error) = kept {
            self.journal.note(&format!("limit-kept failed {error}"));
        }
    }

    /// Take up, once per image, what a harness process before this one kept
    /// of the conversation of `record` ([`Supervisor::limit_keep`]): where
    /// this launch names no model (so its child resumed on the
    /// conversation's newest, the fallback), nothing was moved here yet, and
    /// a state is kept whose time to go back is less than
    /// [`limit::MAX_AWAY_MS`] past, the session is off Fable until that time,
    /// as it was. One journal line: `limit-adopted back-at=<epoch s>
    /// walled-returns=<n>`. A state whose time to go back is further past
    /// than that is removed and not taken up.
    fn limit_adopt(&mut self, record: &SessionRecord, now: u64) {
        self.limit_adopted = true;
        let launch = launch_argv(&self.setup.args);
        if launch.as_deref().and_then(profile::model_of).is_some()
            || self.model.is_some()
            || self.limit.phase != (limit::Phase::OnLaunch { returned: false })
        {
            return;
        }
        let Some(path) = self
            .journal
            .path()
            .and_then(|journal| limit::kept_path(journal, &record.session_id))
        else {
            return;
        };
        let Some(kept) = limit::kept(&path) else {
            return;
        };
        if kept.back_at_ms.saturating_add(limit::MAX_AWAY_MS) < now {
            let _ = limit::forget(&path);
            return;
        }
        self.limit.adopted(&kept);
        self.model = Some(limit::FALLBACK.to_string());
        self.journal.note(&format!(
            "limit-adopted back-at={} walled-returns={}",
            kept.back_at_ms / 1_000,
            kept.walled_returns
        ));
    }

    /// The child was just replaced: a restart that was waiting for the old
    /// one's record has nothing left to restart. One that names a model is
    /// not about that child and stays: it is made on the child that runs now
    /// ([`Supervisor::restart_retry`]).
    fn restart_moot(&mut self) {
        if let Some(deferred) = self
            .restart_deferred
            .take_if(|deferred| !moves_conversation(&deferred.restart))
        {
            self.restart_ignored(&deferred.restart, "child-replaced");
        } else if let Some(deferred) = &mut self.restart_deferred {
            // It is asked of the conversation, not of the child that ended.
            deferred.record = None;
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
    ///
    /// `moved` is the model the plan's resume line carries because a restart
    /// named it ([`Supervisor::relaunch`]): the previous build is started on
    /// that line too, so the model is the session's from there.
    fn fall_back(
        &mut self,
        why: FallbackCause,
        plan: vendor::Plan,
        moved: Option<(String, String)>,
        pipe: &SignalPipe,
    ) {
        if let Some(t) = &self.termios {
            t.restore();
        }
        self.link_events(true);
        self.upgrade_told(
            plan.target.build.number,
            plan.target.build.version.clone(),
            UpgradePhase::Abandoned,
        );
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
                // A start on an account goes back, if it is not made, to the
                // build that runs now: the abandoned target is not tried.
                let admitted = plan.old.account.clone().map(|account| {
                    let mut before = plan.clone().unmade();
                    before.target = before.old.clone();
                    (account, before)
                });
                let session = plan.session_id.clone();
                self.install_replacement(child, None, false);
                self.limit_moves(moved, &session);
                if let Some((account, before)) = admitted {
                    self.account_admitting(account, before, &Cause::Update);
                }
                self.link_reannounce();
            }
            Ok(None) => {
                self.await_update(UpdateLaunch::Fallback(why), plan, moved);
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
                self.turn_end.repo_stop();
                self.link_release();
                std::process::exit(EXIT_VENDOR_MISSING);
            }
        }
    }

    /// One look at the store and the child: keep the safe-point witnesses
    /// current, make the model move Fable's limit calls for
    /// ([`Supervisor::limit_look`]), and end the child for an update when it
    /// is safe.
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
            let version = plan.target.build.version.clone();
            self.journal.note(&format!(
                "update-abandoned the child kept running after SIGTERM; build={target} is not tried again"
            ));
            self.abandoned.remember(target);
            self.ending = Ending::None;
            self.upgrade_told(target, version, UpgradePhase::Abandoned);
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
            && let Some(plan) = self.moved.take()
        {
            // The target has started: the move is made.
            let build = &plan.target.build;
            self.upgrade_told(build.number, build.version.clone(), UpgradePhase::Done);
        }
        // The vendor of a start on an account is seen: the claim names the
        // account from here, and the session record below says it too.
        if self.account_seen(record.as_ref()) {
            self.link_restate();
        }
        self.session_look(record.as_ref(), now);
        let input = sys::stdin_input_ms();
        let gate = self.child.tracker.look(&Look {
            now_ms: now,
            status: record
                .as_ref()
                .map(|r| (r.status.as_str(), r.status_updated_at_ms)),
            input_ms: input,
        });
        // One reading of the limit's witness serves the whole look: the
        // model move and the walls and turn-end duties decide on the same
        // rows.
        let sight = self.limit_sight();
        // A model move for Fable's limit comes before a vendor update: one
        // that is wanted now holds the update for this look, so no look ends
        // the child for both. The update is made at a later safe point. It
        // holds the typing duties too, but for a return that waits away from
        // the turn end it would take ([`Supervisor::limit_yields`]).
        let limit_wanted = self.limit_look(&sight, record.as_ref(), gate, input, now);
        if limit_wanted && !self.limit_yields(record.as_ref(), now) {
            return;
        }
        // The turn end is the duty's unless a vendor update that is due has
        // it first ([`Supervisor::duty_yields`]).
        self.duty_look(record.as_ref(), &sight, now);
        // A restart the walls duty made (the critical-memory banner's) has
        // the child: no update ends it twice. Nor while a return that is due
        // waits: it ends the child next. A limit that is the account's own
        // is aterm's to see and to answer: nothing is asked of it here.
        if self.ending != Ending::None || limit_wanted {
            return;
        }
        let Some(current) = self
            .setup
            .store
            .current_number()
            .filter(|current| *current != self.child.build.number)
        else {
            self.upgrade_none();
            return;
        };
        if self.abandoned.refusal(current).is_none() {
            self.upgrade_pending(current);
        }
        let (record, claimed) = match self.quiet_gates(record, gate, Scope::Upgrade, now) {
            Ok(point) => point,
            Err(wait) => {
                if let Some(wait) = wait {
                    self.note_wait(&wait);
                }
                return;
            }
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
        let resume = match self.resume_plan(&record) {
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
            Ok(plan) => plan.on_account(self.account.as_ref()),
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
            match self.link_safe_point(link::Point::Quiet(Scope::Upgrade)) {
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
                    if let Err(wait) = Self::unread_input() {
                        self.note_wait(&wait);
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
            self.upgrade_told(
                target,
                plan.target.build.version.clone(),
                UpgradePhase::Moving,
            );
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
            own,
            ..
        } = &self.ending
        else {
            return;
        };
        if sent.elapsed() <= Duration::from_millis(grace) {
            return;
        }
        let (cause, killed, own) = (cause.clone(), *killed, *own);
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
        // A model move's failure backs the move off; a wall's restart has its
        // own back-off, counted when it was signalled.
        if own && (cause == limit::CAUSE_LIMIT || cause == limit::CAUSE_RESET) {
            self.limit_failed(&cause, "restart-abandoned", now_ms());
        }
    }

    /// The wait of typed input the vendor has not read yet, when there is
    /// any. Such input moves no input clock, and it becomes a draft as soon as
    /// the vendor reads it.
    fn unread_input() -> Result<(), String> {
        match sys::stdin_unread() {
            Ok(0) => Ok(()),
            Ok(_) => Err("wait unread-input".to_string()),
            Err(error) => Err(format!("wait unread-input {error}")),
        }
    }

    /// The gates of a safe point for `duty` that cost nothing to read, in
    /// order: a hold aterm stated before the link was lost; under a proven
    /// claim, an aterm hold still on its clock and the record at rest for
    /// `duty` ([`Supervisor::claimed_rest`]), and otherwise the tracker's
    /// `gate`; the harness in the terminal's foreground group; without a
    /// claim, no unread input; and a record of this child. `Ok` is that
    /// record and whether the claim judges.
    ///
    /// # Errors
    /// The first gate that holds, as the journal's wait line; `None` for an
    /// aterm hold that is waited out without a line.
    fn quiet_gates(
        &self,
        record: Option<SessionRecord>,
        gate: Result<(), Wait>,
        duty: Scope,
        now: u64,
    ) -> Result<(SessionRecord, bool), Option<String>> {
        // aterm's last safe-point answer was a hold and the link was lost
        // since: nothing moves on the harness's own readings over it before
        // the claim is proven again.
        if let Some(held) = self.link.lost_hold() {
            return Err(Some(held));
        }
        let wait = |wait: Wait| Some(format!("wait {}", wait.word()));
        let claimed = self.link.judges();
        if claimed {
            // An aterm hold stands until an event or its own clock; `status` is
            // not read again before then, and neither is anything slower.
            if self.link.held_back(now) {
                return Err(None);
            }
            let status = record
                .as_ref()
                .map(|r| (r.status.as_str(), r.status_updated_at_ms));
            self.claimed_rest(duty, status, now).map_err(wait)?;
        } else {
            gate.map_err(wait)?;
        }
        if !sys::in_foreground(0) {
            return Err(Some("wait background".to_string()));
        }
        if !claimed {
            Self::unread_input().map_err(Some)?;
        }
        let record = record.ok_or_else(|| wait(Wait::NoRecord))?;
        Ok((record, claimed))
    }

    /// The record half of a safe point for `duty` under a proven claim,
    /// whose last read of aterm's `status` says the rest: for a move onto
    /// another build, the record idle for the quiet period, aterm's own
    /// upgrade gate ([`crate::quiet::record_idle`]); for the harness's own
    /// restart under `relaunch` (the return at a reset), that or a turn's end
    /// before a continuation is typed ([`crate::quiet::record_turn_end`]),
    /// the only moment a session that Cruise or Land continues has: by this
    /// harness's turn-end duty, which types nothing into it while the return
    /// is due ([`Supervisor::limit_yields`]).
    ///
    /// # Errors
    /// [`Wait::NoRecord`], [`Wait::Busy`] or [`Wait::Settling`].
    fn claimed_rest(&self, duty: Scope, status: Option<(&str, u64)>, now: u64) -> Result<(), Wait> {
        let quiet = self.setup.opts.quiet_ms;
        if duty == Scope::Relaunch {
            crate::quiet::record_turn_end(status, now, quiet)
        } else {
            crate::quiet::record_idle(status, now, quiet)
        }
    }

    /// The last look before SIGTERM at a safe point whose gates
    /// ([`Supervisor::quiet_gates`]) held for `record`. Reading the
    /// transcript and the process table took time: the record, the empty
    /// input queue and the input clock (`input`) must still be what the safe
    /// point was decided on, and so must what the caller decided on (`also`).
    /// Nothing slow runs between this and the signal. The queue is read
    /// before the clock: a key still queued then is counted, and a read that
    /// starts before the clock is read moves it. Under a proven claim
    /// (`claimed`) the queue and the clock are aterm's to judge (`input=`,
    /// `human_ms=`), in the one exchange that follows, for the duty `duty`
    /// ([`Supervisor::link_safe_point`]). Where aterm does not answer, or the
    /// claim no longer stands, the three checks its word stood in for are made
    /// here, on this look's own readings (`gate`), and the record and `also`
    /// are read once more because the exchange took time.
    ///
    /// # Errors
    /// What holds, as the journal's wait line.
    fn quiet_still(
        &mut self,
        record: &SessionRecord,
        claimed: bool,
        gate: Result<(), Wait>,
        input: Option<u64>,
        duty: Scope,
        also: impl Fn(&Self) -> bool,
    ) -> Result<(), String> {
        let idle = |sup: &Self| {
            sup.record().is_some_and(|r| {
                r.status == profile::STATUS_IDLE
                    && r.status_updated_at_ms == record.status_updated_at_ms
            })
        };
        let quiet_input = || {
            sys::stdin_unread().is_ok_and(|queued| queued == 0) && sys::stdin_input_ms() == input
        };
        let changed = || "wait changed-while-deciding".to_string();
        if !(idle(self) && (claimed || quiet_input()) && also(self)) {
            return Err(changed());
        }
        if !claimed {
            return Ok(());
        }
        match self.link_safe_point(link::Point::Quiet(duty)) {
            link::Verdict::Safe => Ok(()),
            link::Verdict::Hold(why) => Err(why),
            link::Verdict::Local => {
                gate.map_err(|wait| format!("wait {}", wait.word()))?;
                Self::unread_input()?;
                if idle(self) && quiet_input() && also(self) {
                    Ok(())
                } else {
                    Err(changed())
                }
            }
        }
    }

    /// What the limit's witness sees now ([`limit::Source`]), in the
    /// transcript the model reader found, since the state's floor.
    fn limit_sight(&mut self) -> limit::Sight {
        self.limit_source.look(&limit::At {
            transcript: self.facts.transcript(),
            floor_ms: self.limit.floor_ms,
        })
    }

    /// Journal what was decided about a Fable limit, once per change.
    fn limit_decided(&mut self, line: String) {
        if self.limit_said.seen != line {
            self.journal.note(&line);
            self.limit_said.seen = line;
        }
    }

    /// Journal why a model move waits, as `limit-move <why>`, once per change.
    fn note_limit(&mut self, why: &str) {
        if self.limit_said.wait != why {
            self.limit_said.wait = why.to_string();
            self.journal.note(&format!("limit-move {why}"));
        }
    }

    /// Journal that the move at a limit is made over `what`, which outlasted
    /// its patience ([`limit::WALL_PATIENCE_MS`]), as `limit-move over <what>
    /// patience-ms=<ms>`: once per kind of work (the first word of `what`)
    /// until a move is completed.
    fn note_limit_over(&mut self, what: &str) {
        let kind = what.split(' ').next().unwrap_or(what);
        if self.limit_said.over.split(' ').any(|said| said == kind) {
            return;
        }
        if !self.limit_said.over.is_empty() {
            self.limit_said.over.push(' ');
        }
        self.limit_said.over.push_str(kind);
        self.journal.note(&format!(
            "limit-move over {what} patience-ms={}",
            limit::WALL_PATIENCE_MS
        ));
    }

    /// A model move of the harness's own (`cause`) could not be made at `now`
    /// (`why`): the next attempt waits the back-off's pause
    /// ([`limit::State::failed`]). One journal line per failure: `limit-move
    /// failed cause=<cause> <why> attempt=<n> retry-in-ms=<ms>`.
    fn limit_failed(&mut self, cause: &str, why: &str, now: u64) {
        let pause = self.limit.failed(now);
        self.journal.note(&format!(
            "limit-move failed cause={cause} {why} attempt={} retry-in-ms={pause}",
            self.limit.attempts
        ));
        self.limit_said.wait.clear();
    }

    /// A person chose a model inside the session (`choice`) while it was off
    /// Fable: the return is given up ([`limit::State::given_up`]), and every
    /// resume line from here names what was chosen, so neither a relaunch
    /// nor the reset overrides it. Where the rows name no word for the choice
    /// (the picker, `/fast on`) nothing is kept, and the lines are the
    /// launch's own: one that names no model takes up the conversation's
    /// newest, which is the choice. One journal line: `limit-move given-up
    /// chose=<word|->`. What was kept of the session beside the journal is
    /// removed ([`Supervisor::limit_keep`]), for `record`'s conversation, or
    /// where no record was read, the one whose transcript the choice was
    /// read in.
    fn limit_given_up(&mut self, choice: &limit::Choice, record: Option<&SessionRecord>) {
        let launch = launch_argv(&self.setup.args);
        let launched = launch.as_deref().and_then(profile::model_of);
        self.limit.given_up();
        self.model = choice
            .word
            .clone()
            .filter(|word| Some(word.as_str()) != launched);
        self.limit_said = LimitSaid::default();
        self.journal.note(&format!(
            "limit-move given-up chose={}",
            choice.word.as_deref().unwrap_or("-")
        ));
        let session = record.map(|record| record.session_id.clone()).or_else(|| {
            self.facts
                .transcript()
                .and_then(Path::file_stem)
                .and_then(std::ffi::OsStr::to_str)
                .map(str::to_string)
        });
        if let Some(session) = session {
            self.limit_keep(&session);
        }
    }

    /// The model move Fable's limit calls for, decided on every look
    /// ([`crate::limit`]); `true` when one is wanted now, which holds the rest
    /// of the look (a vendor update, the account's question) so the child is
    /// ended once, and the typing duties but where a return waits away from
    /// the turn end it would take ([`Supervisor::limit_yields`]). A model a
    /// person chose inside the session while it was off Fable gives the
    /// return up first ([`Supervisor::limit_given_up`]).
    ///
    /// * On its launch's terms, with the child refused at Fable's own limit
    ///   ([`limit::Sight::fable`]) and the decision to move
    ///   ([`limit::decide`]): at the limit's moment
    ///   ([`Supervisor::wall_point`]) the child is ended and resumed on
    ///   [`limit::FALLBACK`] ([`Supervisor::limit_leave`]).
    /// * Off Fable, once the time to go back has come: at the ordinary safe
    ///   point the child is ended and resumed on its launch's model
    ///   ([`Supervisor::limit_return`]).
    ///
    /// Each Fable limit and what is decided about it is one journal line
    /// (`limit-seen …`), and so is a return that is due (`limit-return …`).
    /// Nothing is decided while a restart aterm asked for waits, or before
    /// the back-off's pause after a move that failed is over; neither holds
    /// the look. `sight` is what this look read of the limit's witness
    /// ([`Supervisor::limit_sight`]).
    fn limit_look(
        &mut self,
        sight: &limit::Sight,
        record: Option<&SessionRecord>,
        gate: Result<(), Wait>,
        input: Option<u64>,
        now: u64,
    ) -> bool {
        // What a harness before this one kept is taken up before anything is
        // decided on it; the witness's reading does not depend on it.
        if !self.limit_adopted
            && let Some(record) = record
        {
            self.limit_adopt(record, now);
        }
        if sight.answered {
            self.limit.answered();
        }
        if let (limit::Phase::OffFable { .. }, Some(choice)) = (self.limit.phase, &sight.chose) {
            self.limit_given_up(choice, record);
        }
        if self.restart_deferred.is_some() {
            return false;
        }
        let launch = launch_argv(&self.setup.args);
        let launched = launch.as_deref().and_then(profile::model_of);
        let current = self.named_model();
        let decision = sight
            .fable()
            .map(|wall| limit::decide(&self.limit, wall, launched, current.as_deref(), now));
        if let (Some(wall), Some(decision)) = (sight.fable(), &decision) {
            self.limit_decided(limit::seen_line(wall, decision));
        }
        let due = now >= self.limit.retry_at_ms;
        match self.limit.phase {
            limit::Phase::OffFable { back_at_ms } => {
                due && now >= back_at_ms && self.limit_return(record, gate, input, now)
            }
            limit::Phase::OnLaunch { .. } => match sight.fable() {
                Some(wall) if due && decision == Some(limit::Decision::Move) => {
                    let typed = Typed {
                        gate,
                        input,
                        taken_ms: sight.typed_ms,
                    };
                    self.limit_leave(record, wall, typed, now)
                }
                _ => false,
            },
        }
    }

    /// Whether a model move wanted at this look ([`Supervisor::limit_look`])
    /// leaves the typing duties ([`Supervisor::duty_look`]) their part of
    /// it: only a return onto the launch's model that waits, and only away
    /// from the turn end it would take. While `record` reads `idle` for no
    /// longer than [`crate::quiet::TURN_END_WITHIN_MS`], that turn end is
    /// the return's ([`Supervisor::claimed_rest`]), and nothing is typed into
    /// it. Past that window, or while the record shows a turn running, a box
    /// up, or no record at all, the duties go on, a box answered and a turn
    /// end continued, so a return held by a person, by background work or by
    /// aterm's word never parks the session; it waits for the next turn end,
    /// and never for an act the duty typed that is still to be taken up
    /// ([`Supervisor::limit_return`]). The move off Fable at its limit, and a
    /// child that is ending, keep the whole look.
    fn limit_yields(&self, record: Option<&SessionRecord>, now: u64) -> bool {
        self.ending == Ending::None
            && matches!(self.limit.phase, limit::Phase::OffFable { .. })
            && !record.is_some_and(|record| {
                record.status == profile::STATUS_IDLE
                    && now.saturating_sub(record.status_updated_at_ms)
                        <= crate::quiet::TURN_END_WITHIN_MS
            })
    }

    /// The moment a child that stands at the usage limit `wall` may be ended,
    /// in order. The vendor's dialog is open, so the safe point of an update
    /// does not apply (the record need not read `idle`, and aterm sees no
    /// composer): what is asked is that the turn is over and nobody is
    /// interrupted.
    ///
    /// 1. No hold aterm stated before the link was lost.
    /// 2. The limit row is at least the quiet period old (`wait settling`).
    /// 3. A record of this child, not `busy` (`wait no-record`, `wait busy`).
    ///    A record that still reads `busy` [`limit::WALL_PATIENCE_MS`] after
    ///    the limit row, and after its busy run began, holds no longer
    ///    ([`Supervisor::busy_held`]): the conversation's own thread was
    ///    refused, so what keeps it busy is not a turn the move would cut.
    /// 4. The harness is the terminal's foreground group (`wait background`).
    /// 5. The person is quiet and no draft would be lost: under a proven
    ///    claim that is aterm's to say (its `composer=` among it), in the last
    ///    step ([`Supervisor::wall_still`]), and an aterm hold still on its
    ///    clock is waited out; otherwise the harness's own witness says so
    ///    ([`Supervisor::wall_typed`]).
    ///
    /// A stopped child and an own-image move in flight never reach a look.
    /// `Ok` is the record and whether the claim judges.
    ///
    /// # Errors
    /// The first that holds, as wait words; `None` for an aterm hold that is
    /// waited out without a line.
    fn wall_point(
        &self,
        record: Option<&SessionRecord>,
        wall: &limit::Wall,
        typed: Typed,
        now: u64,
    ) -> Result<(SessionRecord, bool), Option<String>> {
        if let Some(held) = self.link.lost_hold() {
            return Err(Some(held));
        }
        let wait = |word: &str| Some(format!("wait {word}"));
        let quiet = self.setup.opts.quiet_ms;
        if now.saturating_sub(wall.at_ms) < quiet {
            return Err(wait(Wait::Settling.word()));
        }
        let record = record.ok_or_else(|| wait(Wait::NoRecord.word()))?;
        if Self::busy_held(record, wall.at_ms, now) {
            return Err(wait(Wait::Busy.word()));
        }
        if !sys::in_foreground(0) {
            return Err(wait("background"));
        }
        let claimed = self.link.judges();
        if claimed {
            if self.link.held_back(now) {
                return Err(None);
            }
        } else {
            self.wall_typed(wall.at_ms, typed, now).map_err(Some)?;
        }
        Ok((record.clone(), claimed))
    }

    /// The harness's own witness, without a claim (or where aterm does not
    /// answer for one), that ending a child at the limit stamped `at_ms`
    /// loses nobody's typing, in order:
    ///
    /// 1. The terminal's input clock is a quiet period old (`wait typing`).
    /// 2. The tracker's draft rule ([`crate::quiet::Tracker`]) does not hold
    ///    (`wait draft`, `wait typing`): the input since the composer was last
    ///    known empty may sit in it. The rule says nothing while the record
    ///    is not `idle`; its other waits (`busy`, `settling`) are the limit's
    ///    own to judge and are not asked here.
    /// 3. Nothing read after the limit row, after the child's spawn, and
    ///    after the newest prompt or command the transcript shows a person
    ///    sent ([`limit::Sight::typed_ms`]) (`wait draft`): such input may be
    ///    words typed once the dialog closed, which nothing shows were sent.
    ///    This is the witness while the record is not `idle`.
    /// 4. Nothing typed is unread (`wait unread-input`).
    ///
    /// # Errors
    /// The first that holds, as the journal's wait line.
    fn wall_typed(&self, at_ms: u64, typed: Typed, now: u64) -> Result<(), String> {
        let wait = |wait: Wait| format!("wait {}", wait.word());
        if typed
            .input
            .is_some_and(|at| now.saturating_sub(at) < self.setup.opts.quiet_ms)
        {
            return Err(wait(Wait::Typing));
        }
        if let Err(held @ (Wait::Draft | Wait::Typing)) = typed.gate {
            return Err(wait(held));
        }
        let taken = at_ms
            .max(self.child.spawned_ms)
            .max(typed.taken_ms.unwrap_or(0));
        if typed.input.is_some_and(|at| at > taken) {
            return Err(wait(Wait::Draft));
        }
        Self::unread_input()
    }

    /// Whether work still running at a limit whose row is stamped `at_ms` has
    /// had its patience at `now` ([`limit::WALL_PATIENCE_MS`]).
    const fn wall_outwaited(at_ms: u64, now: u64) -> bool {
        now.saturating_sub(at_ms) >= limit::WALL_PATIENCE_MS
    }

    /// Whether `record` holds the move at a limit whose row is stamped
    /// `at_ms`: it reads `busy`, and that busy run has not had its patience
    /// at `now`. The patience counts from the later of the row and the run's
    /// start (`statusUpdatedAt`): a run that began after the refusal is a
    /// turn of its own, which nothing says was refused, and gets the whole
    /// patience before the move is made over it.
    fn busy_held(record: &SessionRecord, at_ms: u64, now: u64) -> bool {
        record.status == profile::STATUS_BUSY
            && !Self::wall_outwaited(at_ms.max(record.status_updated_at_ms), now)
    }

    /// The last look before SIGTERM at a limit's moment
    /// ([`Supervisor::wall_point`]) that held for `record`: the same limit
    /// still stands (`at_ms`), the record is still this conversation's and
    /// not `busy` (unless the limit's patience is over), and without a claim
    /// the input queue is empty and the input clock (`typed.input`) has not
    /// moved. Under a proven claim (`claimed`) the person and the composer
    /// are aterm's to judge, in one fresh policy and status read
    /// ([`crate::aterm::judge_wall_point`]); where aterm does not answer, or
    /// the claim no longer stands, the harness's own witness
    /// ([`Supervisor::wall_typed`]) is asked here instead, with the newest
    /// prompt this last read of the transcript shows.
    ///
    /// # Errors
    /// What holds, as wait words.
    fn wall_still(
        &mut self,
        record: &SessionRecord,
        at_ms: u64,
        claimed: bool,
        typed: Typed,
        now: u64,
    ) -> Result<(), String> {
        let sight = self.limit_sight();
        let standing = sight.fable().is_some_and(|wall| wall.at_ms == at_ms);
        let input = typed.input;
        let typed = Typed {
            taken_ms: sight.typed_ms,
            ..typed
        };
        let same = |sup: &Self| {
            sup.record().is_some_and(|r| {
                r.session_id == record.session_id && !Self::busy_held(&r, at_ms, now)
            })
        };
        let quiet_input = || {
            sys::stdin_unread().is_ok_and(|queued| queued == 0) && sys::stdin_input_ms() == input
        };
        let changed = || "wait changed-while-deciding".to_string();
        if !(standing && same(self) && (claimed || quiet_input())) {
            return Err(changed());
        }
        if !claimed {
            return Ok(());
        }
        match self.link_safe_point(link::Point::Wall) {
            link::Verdict::Safe => Ok(()),
            link::Verdict::Hold(why) => Err(why),
            link::Verdict::Local => {
                self.wall_typed(at_ms, typed, now)?;
                if same(self) && quiet_input() {
                    Ok(())
                } else {
                    Err(changed())
                }
            }
        }
    }

    /// The pinned plan of the harness's own restart `restart` on `record`'s
    /// conversation: the resume line on the model it names
    /// ([`Supervisor::model_for`], which reads [`MODEL_LAUNCH`] as the
    /// launch's own), and the same build. Background work under the child holds it, as it holds an
    /// update; at a limit (`wall`) it holds for at most
    /// [`limit::WALL_PATIENCE_MS`] from the limit row's time, and then the
    /// move is made over it, journalled once (`limit-move over
    /// background-work …`).
    ///
    /// The running build's executable must still be a file
    /// (`vendor-missing`): the restart is the harness's own idea, and a child
    /// ended for it that could not be started again would end the session,
    /// where a child left at the limit only waits. The plan's own pin holds
    /// the build's lease, not that file.
    fn limit_plan(
        &mut self,
        restart: &Restart,
        record: &SessionRecord,
        wall: Option<&limit::Wall>,
        now: u64,
    ) -> Result<vendor::Plan, LimitHold> {
        let resume = self
            .resume_plan_on(record, self.model_for(restart).as_deref())
            .map_err(LimitHold::Failed)?;
        if !self.child.build.exe.is_file() {
            return Err(LimitHold::Failed(format!(
                "vendor-missing build={}",
                self.child.build.number
            )));
        }
        if let Err(work) = self.no_background_work() {
            if !wall.is_some_and(|wall| Self::wall_outwaited(wall.at_ms, now)) {
                return Err(LimitHold::Wait(format!("wait {work}")));
            }
            self.note_limit_over(&work);
        }
        self.same_build_plan(record.session_id.clone(), resume)
            .map_err(LimitHold::Failed)
    }

    /// Move the session off Fable at the limit `wall`: at the limit's moment
    /// the child is sent SIGTERM, which closes the vendor's dialog with
    /// nothing typed into it, and is resumed on [`limit::FALLBACK`] when it
    /// has ended. Always `true`: the move is wanted, and the look is held.
    fn limit_leave(
        &mut self,
        record: Option<&SessionRecord>,
        wall: &limit::Wall,
        typed: Typed,
        now: u64,
    ) -> bool {
        let (record, claimed) = match self.wall_point(record, wall, typed, now) {
            Ok(point) => point,
            Err(wait) => {
                if let Some(wait) = wait {
                    self.note_limit(&wait);
                }
                return true;
            }
        };
        if record.status == profile::STATUS_BUSY {
            self.note_limit_over(Wait::Busy.word());
        }
        let restart = Restart {
            cause: limit::CAUSE_LIMIT.to_string(),
            model: Some(limit::FALLBACK.to_string()),
            account: None,
        };
        let plan = match self.limit_plan(&restart, &record, Some(wall), now) {
            Ok(plan) => plan,
            Err(LimitHold::Wait(wait)) => {
                self.note_limit(&wait);
                return true;
            }
            Err(LimitHold::Failed(why)) => {
                self.limit_failed(&restart.cause, &why, now);
                return true;
            }
        };
        if let Err(wait) = self.wall_still(&record, wall.at_ms, claimed, typed, now) {
            self.note_limit(&wait);
        } else if let Err(why) = self.restart_signal(&restart, &record, plan, true) {
            self.limit_failed(&restart.cause, &why, now);
        }
        true
    }

    /// The limit has reset (the state's time to go back has come): at the
    /// ordinary safe point, judged for `relaunch`, the child is sent SIGTERM
    /// and is resumed on its launch's own model when it has ended. Under a
    /// proven claim the record half of that point is also met at a turn's
    /// end, before a continuation is typed ([`Supervisor::claimed_rest`]): a
    /// session Cruise or Land continues is never idle for a whole quiet
    /// period. Where the claim takes `turn-end`, this harness's own duty
    /// types nothing into that turn end ([`Supervisor::limit_yields`]), and
    /// the return waits for an act the duty typed at one it let pass to be
    /// taken up (`wait duty-act`); the duty continues the resumed child
    /// ([`turn_end::Driver::child_restarted`]). That is the restart aterm asks
    /// for with `model=launch` ([`MODEL_LAUNCH`]),
    /// made the same way from here on ([`Supervisor::model_for`],
    /// [`Supervisor::model_moved`]): the launch's own line, with the alias
    /// `fable` where the launch named no model, named on later lines too until
    /// the returned child has answered ([`Supervisor::launch_pin`]) and kept
    /// for none after. One journal line when the return falls due, naming the
    /// model it goes back to. Always `true`.
    fn limit_return(
        &mut self,
        record: Option<&SessionRecord>,
        gate: Result<(), Wait>,
        input: Option<u64>,
        now: u64,
    ) -> bool {
        let to = self.shown_model(MODEL_LAUNCH);
        if !self.limit_said.due {
            self.limit_said.due = true;
            self.journal.note(&format!("limit-return due to={to}"));
        }
        let (record, claimed) = match self.quiet_gates(record.cloned(), gate, Scope::Relaunch, now)
        {
            Ok(point) => point,
            Err(wait) => {
                if let Some(wait) = wait {
                    self.note_limit(&wait);
                }
                return true;
            }
        };
        // An act the turn-end duty typed at a turn end this return let pass
        // ([`Supervisor::limit_yields`]) is the agent's to take up: the
        // return is made at the turn end after it, never over it.
        if self.turn_end.act_pending(now) {
            self.note_limit("wait duty-act");
            return true;
        }
        let restart = Restart {
            cause: limit::CAUSE_RESET.to_string(),
            model: Some(MODEL_LAUNCH.to_string()),
            account: None,
        };
        let plan = match self.limit_plan(&restart, &record, None, now) {
            Ok(plan) => plan,
            Err(LimitHold::Wait(wait)) => {
                self.note_limit(&wait);
                return true;
            }
            Err(LimitHold::Failed(why)) => {
                self.limit_failed(&restart.cause, &why, now);
                return true;
            }
        };
        if let Err(wait) =
            self.quiet_still(&record, claimed, gate, input, Scope::Relaunch, |_| true)
        {
            self.note_limit(&wait);
        } else if let Err(why) = self.restart_signal(&restart, &record, plan, true) {
            self.limit_failed(&restart.cause, &why, now);
        }
        true
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

    /// The resume argv for `record`'s conversation on the model every resume
    /// line names ([`Supervisor::kept_model`]: the one the session was last
    /// moved onto, or the launch pin), or why it cannot be resumed here.
    fn resume_plan(&self, record: &SessionRecord) -> Result<Vec<String>, String> {
        self.resume_plan_on(record, self.kept_model().as_deref())
    }

    /// The resume argv for `record`'s conversation, or why it cannot be
    /// resumed here: the launch's own flags, with `model` in place of the
    /// launch's `--model` when one is given ([`profile::with_model`]), in the
    /// conversation's permission mode where that is within what the launch
    /// asked for ([`profile::with_permission_mode`]).
    fn resume_plan_on(
        &self,
        record: &SessionRecord,
        model: Option<&str>,
    ) -> Result<Vec<String>, String> {
        self.resumable(record)?;
        self.resume_line(&record.session_id, model)
    }

    /// Why `record`'s conversation cannot be resumed from here, if it cannot.
    fn resumable(&self, record: &SessionRecord) -> Result<(), String> {
        if !profile::is_session_id(&record.session_id) {
            return Err("unresumable session-id".to_string());
        }
        if !self.same_dir(&record.cwd) {
            // `--resume` finds a conversation by the directory it started in.
            return Err("unresumable cwd-moved".to_string());
        }
        Ok(())
    }

    /// The words a `session` record gives aterm to resume `record`'s
    /// conversation after `claude-alab run --` ([`Supervisor::session_look`]):
    /// the harness's own resume line ([`Supervisor::resume_plan`]) made from
    /// the launch's words without the usage tap's `--settings <value>`, which
    /// that launch asks aterm for again ([`Setup::tapped`]), or why it cannot
    /// be resumed or aterm cannot type it ([`relaunchable`]).
    ///
    /// While the session is off Fable for its limit the words name no model,
    /// neither the fallback nor the launch's own: a resume line with none
    /// takes up the conversation's newest model, the fallback, and the new
    /// harness takes up the state this one keeps beside the journal
    /// ([`Supervisor::limit_adopt`], which only a launch that names no model
    /// asks for), so it goes back onto Fable at the reset. Named, the
    /// fallback would be that harness's launch model for good.
    fn resume_words(&self, record: &SessionRecord) -> Result<Vec<String>, String> {
        self.resumable(record)?;
        let args = if self.setup.tapped {
            self.setup.args.get(2..).unwrap_or_default()
        } else {
            &self.setup.args
        };
        let words = if matches!(self.limit.phase, limit::Phase::OffFable { .. }) {
            profile::without_model(self.resume_line_of(args, &record.session_id, None)?)
                .ok_or("unresumable model-flag")?
        } else {
            self.resume_line_of(args, &record.session_id, self.kept_model().as_deref())?
        };
        relaunchable(&words)?;
        Ok(words)
    }

    /// The resume argv for the conversation `session_id` on `model`
    /// ([`Supervisor::resume_plan_on`], which also checks that the
    /// conversation can be resumed from here).
    fn resume_line(&self, session_id: &str, model: Option<&str>) -> Result<Vec<String>, String> {
        self.resume_line_of(&self.setup.args, session_id, model)
    }

    /// [`Supervisor::resume_line`], from the launch words `args`.
    fn resume_line_of(
        &self,
        args: &[OsString],
        session_id: &str,
        model: Option<&str>,
    ) -> Result<Vec<String>, String> {
        let argv = launch_argv(args).ok_or("unresumable argv-not-utf8")?;
        let flags =
            profile::resume_argv(&argv, session_id).map_err(|r| format!("unresumable {r}"))?;
        let flags = match model {
            Some(model) => profile::with_model(flags, model).ok_or("unresumable model-flag")?,
            None => flags,
        };
        // The conversation's own permission mode, as aterm's relaunch carries it
        // (aterm `relaunch.rs:481-507`, `last_permission_mode`/`resume_mode`):
        // a session switched to plan or default inside is not resumed in the
        // launch's bypass. No transcript or no mode line: the launch flags
        // stand, and so they do where the transcript's mode is wider than the
        // launch asked for (the line is history and may predate this launch).
        let mode = profile::transcript_in(&self.config, session_id)
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

    /// Tell aterm, on an open stream, how the child's conversation resumes
    /// (a `session` record, [`link::Link::session`]): once the child has
    /// written its session record, and again whenever the child, its
    /// conversation, the model every resume line names, the account it runs
    /// on or, at a turn end, its permission mode may have moved what the
    /// record says. So every launch, relaunch, `/resume`, `/clear`, model move
    /// and account move is told. Its resume words are [`Supervisor::resume_words`];
    /// where there are none, the reason is journalled `session-words <why>`
    /// and the record carries none, which aterm reads as no conversation to
    /// resume. Nothing is read while no stream is open.
    fn session_look(&mut self, record: Option<&SessionRecord>, now: u64) {
        if !self.link.streaming() {
            return;
        }
        if let Some(record) = record {
            let same = self
                .described
                .as_ref()
                .filter(|was| was.pid == self.child.pid && was.conversation == record.session_id);
            let idle_at = if record.status == profile::STATUS_IDLE {
                record.status_updated_at_ms
            } else {
                same.map_or(0, |was| was.idle_at)
            };
            let now_described = Described {
                pid: self.child.pid,
                conversation: record.session_id.clone(),
                model: self.kept_model(),
                off_fable: matches!(self.limit.phase, limit::Phase::OffFable { .. }),
                account: self.account.as_ref().map(|account| account.key.clone()),
                idle_at,
            };
            if self.described.as_ref() != Some(&now_described) {
                let resume = self.resume_words(record).unwrap_or_else(|why| {
                    self.journal.note(&format!("session-words {why}"));
                    Vec::new()
                });
                let started = self
                    .described
                    .as_ref()
                    .is_none_or(|was| was.pid != self.child.pid);
                self.described = Some(now_described);
                self.link.session(
                    link::Resumes {
                        vendor: self.child.build.version.as_deref(),
                        conversation: &record.session_id,
                        cwd: &record.cwd,
                        resume,
                        started,
                    },
                    &self.journal,
                    now,
                );
                return;
            }
        }
        // What the stream had no room for before.
        self.link.tell(&self.journal, now);
    }

    /// Tell aterm that the move onto build `target` is at `phase` (an
    /// `upgrade` record, [`link::Link::upgrade`]), naming `version`, else the
    /// version an earlier phase of the same move named, else `-`. A move that
    /// is done or abandoned is forgotten. Nothing is kept or read where no
    /// link can ever be made (outside aterm).
    fn upgrade_told(&mut self, target: u64, version: Option<String>, phase: UpgradePhase) {
        if self.link.absent_for_good() {
            return;
        }
        let to = version
            .or_else(|| {
                self.upgrading
                    .as_ref()
                    .filter(|was| was.target == target)
                    .map(|was| was.to.clone())
            })
            .unwrap_or_else(|| "-".to_string());
        self.upgrading = matches!(
            phase,
            UpgradePhase::Seen | UpgradePhase::Waiting | UpgradePhase::Moving
        )
        .then(|| Upgrading {
            target,
            to: to.clone(),
            phase,
        });
        self.link.upgrade(phase, &to, &self.journal, now_ms());
    }

    /// A look found the store's `current` naming `target`, a build other than
    /// the child's and not abandoned: the move is `seen` the first time, with
    /// the version its `.vendor` record states ([`Store::version_of`]), and
    /// `waiting` from the next look that finds it still to make. That record
    /// is the one file of a pending build read before the gates allow the
    /// update, once per build, and only for what aterm is told.
    fn upgrade_pending(&mut self, target: u64) {
        if self.link.absent_for_good() {
            return;
        }
        match &self.upgrading {
            Some(was) if was.target == target => {
                if was.phase == UpgradePhase::Seen {
                    self.upgrade_told(target, None, UpgradePhase::Waiting);
                }
            }
            _ => {
                let version = self.setup.store.version_of(target);
                self.upgrade_told(target, version, UpgradePhase::Seen);
            }
        }
    }

    /// A look found no move to make: `current` names the child's build, or
    /// none. A move seen or waited for that the store took back is abandoned.
    fn upgrade_none(&mut self) {
        if let Some(was) = &self.upgrading
            && matches!(was.phase, UpgradePhase::Seen | UpgradePhase::Waiting)
        {
            let target = was.target;
            self.upgrade_told(target, None, UpgradePhase::Abandoned);
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

    /// This harness types nothing in this tab, for `why` ([`link::UNTYPED`]):
    /// the shared link's claim names `relaunch` and `upgrade` alone, so the
    /// typing duties the launch names (`--alab-duties=`) are not claimed and
    /// stay with aterm's own engine. Every duty that types nothing (the
    /// vendor update, the move off Fable's limit and back) goes on.
    /// Journalled once per reason, as `duty untyped <why>: …`, for a launch
    /// that names a typing duty; the harness changes nothing else for it.
    fn note_untyped(&mut self, why: &str) {
        if !self.setup.opts.duties.types() || self.untyped_noted == why {
            return;
        }
        self.untyped_noted = why.to_string();
        self.journal.note(&format!(
            "duty untyped {why}: this harness types nothing in this tab; the typing duties \
             stay aterm's"
        ));
    }
}

#[cfg(test)]
#[path = "supervise_model_tests.rs"]
mod model_tests;

#[cfg(test)]
#[path = "supervise_limit_tests.rs"]
mod limit_tests;

#[cfg(test)]
#[path = "supervise_account_tests.rs"]
mod account_tests;

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
                tapped: false,
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
            model: None,
            untyped_noted: String::new(),
            limit: limit::State::new(1),
            limit_source: Box::new(limit::Transcript::default()),
            limit_said: LimitSaid::default(),
            turn_end: turn_end::Driver::default(),
            account: None,
            admitting: None,
            limit_adopted: false,
            described: None,
            upgrading: None,
        }
    }

    /// A restart aterm asks for `cause`, naming no model.
    pub(super) fn ask(cause: &str) -> Restart {
        Restart {
            cause: cause.to_string(),
            model: None,
            account: None,
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
            read_ms: 0,
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
            ["aterm restart cause=stall model=opus[1m] ignored aterm link-lost"]
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
                "aterm restart cause=account to=work model=haiku ignored account-assignment-unsupported",
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
            model: Some("opus".into()),
            killed: false,
            own: false,
        };
        sup.restart_escalate();
        assert_eq!(sup.ending, Ending::None);
        assert_eq!(sup.setup.args, os(&["--model", "sonnet"]));
        assert_eq!(sup.model, None, "no model is kept for a restart never made");
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
            read_ms: 0,
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
