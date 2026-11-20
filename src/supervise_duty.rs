// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The turn-end duty's driver: when the supervisor reads aterm for its own
//! session's turn ends, what it hands the decider ([`crate::duty`]), and how it
//! types the decider's act through the claim's fenced writes
//! ([`crate::typing::Typist`]). The same read serves the approvals and
//! questions duties, whose driver is `src/supervise_box_duty.rs`, and the
//! walls duty, whose driver is `src/supervise_wall_duty.rs`: a key pressed on
//! a box takes the read, then an act on (or a wait at) a wall does, and the
//! turn-end decider waits for the next.
//!
//! **What it acts under.** Only a launch that takes `turn-end`, `approvals`,
//! `questions` or `walls` ([`crate::aterm::Duties`], `--alab-duties=`) reads,
//! each duty acting only on what it takes, and only while its claim stands
//! proven and aterm's `status` states the session's Autopilot as Cruise or
//! Land. The claim names those four only then ([`crate::aterm::Duties::under`]):
//! it is made again as the mode moves ([`Supervisor::link_duty_mode`]), so
//! under Off, or while aterm states no mode (`autopilot=-`, before it admits
//! the session's agent), the claim names no duty this harness would not
//! perform. Off, a mode aterm states none of, a claim released (with Autopilot
//! off aterm's policy is off) and a link lost type nothing. aterm keeps no
//! Claude Code duty of its own: a typing duty the launch leaves off is
//! performed by nobody.
//!
//! **What it never gives away.** The typing duties stay this harness's while
//! the claim stands, whatever aterm answers: a write aterm read otherwise
//! than it was written (`ERR usage: …`) is tried again at the next read, each
//! such answer in a row spacing the next by a rung of [`USAGE_BACKOFF_MS`];
//! reads that go unread are tried again, each in a row spacing the next by a
//! rung of [`UNREAD_BACKOFF_MS`]. A write aterm took, or an answered read,
//! counts its run afresh, and so does a child started again. The harness
//! types only through aterm's fenced writes: the vendor child reads the tab's
//! own terminal, which this harness does not stand between, so where aterm
//! takes no write nothing is typed, and the journal says so.
//!
//! **When it reads.** Each read is three requests on the claim's connection
//! (`harness policy`, `status`, `text --json`), made at a look of the
//! supervisor's loop when one is due: at once when aterm sent an event about
//! the session (its agent, a turn, a person, its status, its Autopilot word),
//! when the vendor's session record moved, or when the person's grace that
//! held the last write has run; at the decider's own time for a wait; and
//! otherwise every [`DUTY_RECHECK_MS`]. Nothing is read while the mode is not
//! one that types, so a session nothing is typed into costs aterm no request.
//!
//! **What it leaves to others.** A Fable-limit model move wanted at this look
//! comes first ([`Supervisor::limit_look`]); a return onto the launch's model
//! that is due takes only the turn end it would be made at, and while it waits
//! away from one the duties go on ([`Supervisor::limit_yields`]), so the two
//! never act at one turn end and a held return never parks the session. A
//! vendor update that is due has the turn end to itself for its quiet period
//! and [`UPDATE_YIELD_SLACK_MS`] more ([`Supervisor::duty_yields`]): an act typed
//! at every turn end would otherwise keep it from ever finding the session at
//! rest. Nothing is read while the child ends, is started again or is moved
//! onto another image.
//!
//! **What it reads beside aterm.** Where Land weighs a `LANDED` answer to its
//! check ([`Carried::reads_repo`]), git reads every repository the session
//! works in ([`crate::repo::probe`]: the vendor child's folder, the harness's,
//! and those the transcript shows it changing, with their superprojects and
//! submodules) in a process of its own beside the loop
//! ([`crate::repo_probe::Probing`], [`Supervisor::repo_look`]): the turn end
//! waits on it, looked at every [`REPO_POLL_MS`], and the decider counts the
//! land only over a clean reading, as aterm does. A probe that was cut (a
//! read past its minute, a process that did not answer) is no reading: the
//! repositories are read again [`REPO_RETRY_MS`] later, and nothing is typed
//! for it. After [`REPO_CUTS`] cut in a row for one `LANDED`, Land leaves its
//! rest to the person as a `NEEDS YOU:` answer does, and says why
//! ([`Supervisor::repo_cut`]), so it never waits on its repositories
//! unbounded and unreported. An unread reading (git refused a read that must
//! answer, or could not be asked) is a reading, which gets the nudge to show
//! the status; after [`REPO_UNREADS`] of them in a row for the task Land
//! carries, across its checks, Land leaves its rest to the person the same
//! way ([`Supervisor::repo_read`]), so a repository git never reads ends the
//! loop of check, `LANDED` and nudge instead of typing it for the session's
//! life. An own-image move waits while a probe runs ([`Driver::probing`]).
//!
//! **What it reports.** Where Land rests, its check answered `LANDED` or
//! with a `NEEDS YOU:` line, or its repositories left unread by every probe
//! or reading its bounds allow, aterm's task record is told as codex-alab
//! tells it (`landed@v1`, `left-to-person@v1`; [`Supervisor::land_report`]).
//!
//! **What it journals.** Each decision once per change (`duty turn-end
//! nothing busy`, `duty turn-end wait short-turn until=<ms>`), each repository
//! reading (`duty turn-end repo clean`, `repo unread`, `repo found <summary>`,
//! `repo probing folders=<n>`, `repo cut again-at=<ms>`, `repo cut
//! left-to-person cuts=<n>: …`, `repo unread left-to-person unread=<n>: …`),
//! and every write with aterm's answer (`duty turn-end typed
//! rule=continue@v1 reply=submitted`), and each report of Land's rest
//! (`duty turn-end reported act=landed@v1`, `duty turn-end unreported
//! act=<act> <why>[ later]`).
//! A write aterm took nothing of, or held, is said once while aterm's answer
//! stays the same (`duty turn-end unwritten rule=… reply=…`, `duty turn-end
//! held rule=… <field>=<value>`).

use std::ffi::OsString;
use std::path::PathBuf;
use std::time::Duration;

use alab_policy::config::TurnEndTiming;

use super::Supervisor;
#[cfg(test)]
use super::now_ms;
use crate::aterm::{Autopilot, LandAct, Policy, Reported, Scope};
use crate::boxes::Watch;
use crate::duty::{Carried, Decision, LandState, Look, Rule};
use crate::journal::Journal;
use crate::limit;
use crate::profile::{COMMAND, STATUS_IDLE, SessionRecord};
use crate::repo::{Probed, RepoRead};
use crate::repo_probe::{Probing, Request};
use crate::typing::{Reply, Typed, Write};
use crate::walls;

/// How long the duty goes without reading aterm while nothing wakes it.
pub const DUTY_RECHECK_MS: u64 = 5_000;

/// How long after a write aterm could not take now (`ERR busy …`) the duty
/// reads again.
pub const BUSY_RETRY_MS: u64 = 1_000;

/// How long after aterm said it types nothing for a harness into an agent
/// image it has not confirmed (`ERR busy supervisor_act=observe …`) the duty
/// reads again.
pub const OBSERVED_RETRY_MS: u64 = 60_000;

/// How much longer than the quiet period a turn end is left to a vendor update
/// that is due: the update's own reads (the process table, aterm's safe point)
/// after the quiet period.
pub const UPDATE_YIELD_SLACK_MS: u64 = 10_000;

/// How long after a write aterm read otherwise than it was written (`ERR
/// usage: …`) the duties read and write again, by how many such answers came
/// in a row: the first spaces the next by the first rung, and the last rung
/// repeats ([`Driver::usage`]).
pub const USAGE_BACKOFF_MS: [u64; 4] = [5_000, 30_000, 120_000, 300_000];

/// How long after a read aterm did not answer as asked the duties read again,
/// by how many such reads came in a row: the last rung repeats
/// ([`Driver::unread`]).
pub const UNREAD_BACKOFF_MS: [u64; 4] = [5_000, 10_000, 30_000, 60_000];

/// How often a look reads aterm while Land's repository probe runs, to take
/// its answer.
pub const REPO_POLL_MS: u64 = 1_000;

/// How long after a probe that was cut the repositories are read again.
pub const REPO_RETRY_MS: u64 = 60_000;

/// How many probes in a row may be cut, or fail to start, for one `LANDED`
/// before Land leaves its rest to the person ([`Supervisor::repo_cut`]): with
/// [`REPO_RETRY_MS`] between them and each bounded by
/// [`crate::repo_probe::PROBE_CAP`], an hour or so of trying at most.
pub const REPO_CUTS: u32 = 3;

/// How many readings in a row may be unread for the task Land carries, one
/// for each `LANDED` it weighs as a rule, before Land leaves its rest to the
/// person ([`Supervisor::repo_read`]): each before it gets the nudge to show
/// the status, and an agent that answers that nudge and claims done again is
/// checked again, so without a bound a repository git never reads would be
/// nudged for the session's life.
pub const REPO_UNREADS: u32 = 3;

/// The journal's subject for this duty.
const SUBJECT: &str = "duty turn-end";

/// The person's home folder Land's repository probe runs git under, from
/// `HOME` as `home` holds it.
///
/// # Errors
/// [`RepoRead::Unread`] where it is unset or not absolute: git cannot be
/// asked as the person, so what the repositories hold is not known, which is
/// never clean.
fn repo_home(home: Option<OsString>) -> Result<PathBuf, RepoRead> {
    home.map(PathBuf::from)
        .filter(|home| home.is_absolute())
        .ok_or(RepoRead::Unread)
}

/// Where Land's repository read stands ([`Supervisor::repo_look`]).
#[derive(Debug, Default)]
pub(super) enum RepoLook {
    /// No probe runs.
    #[default]
    Idle,
    /// A probe runs beside the loop.
    Probing(Probing),
    /// The last probe was cut: none is started before this (epoch ms).
    After(u64),
}

/// The vendor's record status of a turn that runs.
const STATUS_BUSY: &str = "busy";

/// The driver's state. Only [`Driver::carried`] crosses an own-image move; the
/// rest is rebuilt at the new image's first look.
#[derive(Debug, Default)]
pub(super) struct Driver {
    /// What the decider remembers of the session.
    pub(super) carried: Carried,
    /// When (epoch ms) aterm is read for the duty next; 0 for the next look.
    due_ms: u64,
    /// The last mode aterm stated (Off, Cruise or Land), so a move between two
    /// is seen.
    stated: Option<Autopilot>,
    /// The last decision journalled, so each is said once.
    noted: String,
    /// The vendor record's status and its time at the last look.
    record: Option<(String, u64)>,
    /// When the turn the record shows running started (its `statusUpdatedAt`).
    busy_since_ms: Option<u64>,
    /// How long the last turn the record showed ran; `None` where the record
    /// never showed it running.
    turn_ms: Option<u64>,
    /// How many writes in a row aterm read otherwise than they were written
    /// (`ERR usage: …`), each of which spaced the next read by a rung of
    /// [`USAGE_BACKOFF_MS`]. A write aterm read as written, and a child
    /// started again, count afresh.
    usage_run: u32,
    /// The box duties' memory of the boxes they saw, the wall dialogs
    /// included ([`crate::boxes::Watch`], `src/supervise_box_duty.rs`).
    pub(super) boxes: Watch,
    /// The box duties' last line said once, so each is said once.
    box_noted: String,
    /// The walls duty's memory of the wall the session stands at
    /// ([`crate::walls::Watch`], `src/supervise_wall_duty.rs`).
    pub(super) walls: walls::Watch,
    /// The walls duty's last line, so each is said once.
    wall_noted: String,
    /// The last unread read's line, so each is said once while the reads
    /// fail alike.
    unread_noted: String,
    /// How many of the typing duties' reads in a row went unread, each of
    /// which spaced the next by a rung of [`UNREAD_BACKOFF_MS`]. An answered
    /// read counts afresh.
    unread_run: u32,
    /// Land's repository read for the `LANDED` it weighs.
    repo: RepoLook,
    /// How many probes in a row were cut, or failed to start, for the
    /// `LANDED` Land weighs ([`REPO_CUTS`]). A reading, and a look that
    /// weighs no `LANDED`, count afresh.
    repo_cuts: u32,
    /// How many readings in a row were unread for the task Land carries,
    /// across its checks ([`REPO_UNREADS`]): a clean or found reading, and a
    /// look at which Land carries no check (a new task, or one that rests),
    /// count afresh ([`Driver::repo_idle`]).
    repo_unread: u32,
    /// The rest of Land's check (`Landed` or `Theirs`) this image settled
    /// with aterm's task record: reported, or found unserved. `None` while
    /// Land does not rest, or rests unreported.
    land_told: Option<LandState>,
    /// How many restarts the critical-memory banner has called for in this
    /// session: each spaces the next by a rung of
    /// [`TurnEndTiming::restart_backoff`]. It outlives the child it restarted.
    pub(super) memory_restarts: u32,
    /// No restart for the banner before this (epoch ms).
    pub(super) memory_after_ms: u64,
}

impl Driver {
    /// The driver of a new image, carrying `carried` from the old one.
    pub(super) fn carrying(carried: Carried) -> Self {
        Self {
            carried,
            ..Self::default()
        }
    }

    /// aterm sent an event about the session: read at the next look.
    pub(super) fn wake(&mut self) {
        self.due_ms = 0;
    }

    /// Whether Land's repository probe runs beside the loop: an own-image
    /// move waits for it, since an exec would keep its process with nobody
    /// to reap it.
    pub(super) fn probing(&self) -> bool {
        matches!(self.repo, RepoLook::Probing(_))
    }

    /// The harness is about to exit: a running repository probe is killed
    /// with its group and reaped, since an exit runs no drop.
    pub(super) fn repo_stop(&mut self) {
        self.repo = RepoLook::Idle;
    }

    /// This look weighs no `LANDED`: a probe that runs is dropped (killed
    /// with its group, so the turn end never waits on a stale one), and the
    /// cuts of the last `LANDED` count afresh. The unread readings count on
    /// while Land carries its task's check (its instruction or nudge typed,
    /// or its check owed: `Told`, `Checking`), since the next `LANDED` reads
    /// the same repositories, and afresh once it carries none (a new task,
    /// or a rest).
    pub(super) fn repo_idle(&mut self) {
        self.repo = RepoLook::Idle;
        self.repo_cuts = 0;
        if !matches!(self.carried.land, LandState::Told | LandState::Checking) {
            self.repo_unread = 0;
        }
    }

    /// Journal `words` as this duty's decision, unless it is the one said last.
    pub(super) fn note(&mut self, journal: &Journal, words: &str) {
        if self.noted != words {
            journal.note(&format!("{SUBJECT} {words}"));
            self.noted = words.to_string();
        }
    }

    /// Journal that the read every typing duty the claim takes shares went
    /// unread at `now`: `duty <duties> unread <why>`, `<duties>` those duties'
    /// words comma-joined (`read` where it names none), unless it is the line
    /// said last. The turn-end decision after it is said again. The duties
    /// stay claimed, and read again a rung of [`UNREAD_BACKOFF_MS`] from
    /// `now`, by how many reads in a row went unread, or at once for an event
    /// aterm sends ([`Driver::wake`]).
    // Called by the claim's typed exchange, which the shared link does not
    // carry yet (`src/supervise_link.rs`).
    #[allow(dead_code)]
    pub(super) fn unread(&mut self, journal: &Journal, duties: &str, why: &str, now: u64) {
        let duties = if duties.is_empty() { "read" } else { duties };
        let line = format!("duty {duties} unread {why}");
        if self.unread_noted != line {
            journal.note(&line);
            self.unread_noted = line;
        }
        self.noted.clear();
        self.due_ms = now.saturating_add(rung(&UNREAD_BACKOFF_MS, self.unread_run));
        self.unread_run = self.unread_run.saturating_add(1);
    }

    /// The typing duties' read was answered: an unread one after it is said
    /// again, and the unread reads are counted afresh.
    // Called by the claim's typed exchange, which the shared link does not
    // carry yet (`src/supervise_link.rs`).
    #[allow(dead_code)]
    pub(super) fn read_answered(&mut self) {
        self.unread_noted.clear();
        self.unread_run = 0;
    }

    /// A typed exchange ended with `typed`. A write aterm answered with
    /// anything but `ERR usage: …` was read as it was written, whatever aterm
    /// did with it: the writes it read otherwise count afresh. Nothing else
    /// does: a usage answer climbs the run ([`Driver::usage`]), a write held
    /// before it was sent and one whose read before it went unanswered sent
    /// aterm nothing to read, and a write refused for its authority says
    /// nothing of how aterm reads the words.
    // Called by the claim's typed exchange, which the shared link does not
    // carry yet (`src/supervise_link.rs`).
    #[allow(dead_code)]
    pub(super) fn write_answered(&mut self, typed: &Typed) {
        if matches!(typed, Typed::Answered(reply) if !matches!(reply, Reply::Usage(_))) {
            self.usage_run = 0;
        }
    }

    /// Journal an act; the decision after it is said again.
    fn act(&mut self, journal: &Journal, words: &str) {
        journal.note(&format!("{SUBJECT} {words}"));
        self.noted.clear();
    }

    /// Journal `words` for the box duty `scope` (`duty approvals …`, `duty
    /// questions …`), unless it is the line said last.
    pub(super) fn box_note(&mut self, journal: &Journal, scope: Scope, words: &str) {
        let line = format!("duty {} {words}", scope.word());
        if self.box_noted != line {
            journal.note(&line);
            self.box_noted = line;
        }
    }

    /// Journal an act of the box duty `scope`; the line after it is said
    /// again.
    pub(super) fn box_act(&mut self, journal: &Journal, scope: Scope, words: &str) {
        journal.note(&format!("duty {} {words}", scope.word()));
        self.box_noted.clear();
    }

    /// Journal `words` as the walls duty's decision (`duty walls …`), unless
    /// it is the line said last.
    pub(super) fn wall_note(&mut self, journal: &Journal, words: &str) {
        let line = format!("duty {} {words}", Scope::Walls.word());
        if self.wall_noted != line {
            journal.note(&line);
            self.wall_noted = line;
        }
    }

    /// Journal an act of the walls duty; the line after it is said again.
    pub(super) fn wall_act(&mut self, journal: &Journal, words: &str) {
        journal.note(&format!("duty {} {words}", Scope::Walls.word()));
        self.wall_noted.clear();
    }

    /// Read aterm next at `at_ms`, whatever was due: a duty's own time (a
    /// wait, or the person's grace that held a write).
    pub(super) fn read_at(&mut self, at_ms: u64) {
        self.due_ms = at_ms;
    }

    /// Read aterm again `wait` after `now`, sooner than the duty's own time.
    pub(super) fn read_in(&mut self, now: u64, wait: Duration) {
        self.due_ms = self.due_ms.min(after(now, wait));
    }

    /// aterm read a write otherwise than it was written (`ERR usage: …`) at
    /// `now`: no later write in the same words would read otherwise, but a
    /// policy or a screen read afresh may give other words, so the duties
    /// read again a rung of [`USAGE_BACKOFF_MS`] from `now`, by how many such
    /// answers came in a row, whatever was due. The duties stay claimed.
    pub(super) fn usage(&mut self, now: u64) {
        self.due_ms = now.saturating_add(rung(&USAGE_BACKOFF_MS, self.usage_run));
        self.usage_run = self.usage_run.saturating_add(1);
    }

    /// A child was started again ([`Carried::child_restarted`]); `owed` is a
    /// resume this harness owes it. The boxes and the wall seen are the old
    /// child's; the memory restarts made stay counted. The writes and reads
    /// aterm did not take are counted afresh.
    pub(super) fn child_restarted(&mut self, owed: bool) {
        self.carried.child_restarted(owed);
        self.due_ms = 0;
        self.record = None;
        self.busy_since_ms = None;
        self.turn_ms = None;
        self.usage_run = 0;
        self.unread_run = 0;
        self.boxes = Watch::default();
        self.box_noted.clear();
        self.walls = walls::Watch::default();
        self.wall_noted.clear();
    }

    /// Whether an act this duty typed is still to be taken up at `now`, and
    /// was typed less than [`TurnEndTiming::take_within`] ago: a restart of
    /// this harness's own at a turn end (the return at Fable's reset,
    /// [`Supervisor::limit_return`]) is not made over it. One the agent has
    /// not taken that long after is no turn the restart would cut, and the
    /// safe point's own reads judge it.
    pub(super) fn act_pending(&self, now: u64) -> bool {
        let within =
            u64::try_from(TurnEndTiming::default().take_within.as_millis()).unwrap_or(u64::MAX);
        self.carried
            .awaiting
            .as_ref()
            .is_some_and(|act| now.saturating_sub(act.at_ms) < within)
    }

    /// An act was typed, or may have been, at `now`: the decider awaits it
    /// ([`Carried::typed`]), and the turn that answers it is timed afresh, so
    /// a turn end the record never showed running is not judged by the length
    /// of the turn before.
    fn typed(&mut self, rule: Rule, text: &str, now: u64) {
        self.carried.typed(rule, text, now);
        self.turn_ms = None;
    }

    /// aterm stated `mode`: a move between two modes is Land's new task.
    fn mode_seen(&mut self, mode: Autopilot) {
        if self.stated.is_some_and(|stated| stated != mode) {
            self.carried.autopilot_moved();
            self.due_ms = 0;
        }
        self.stated = Some(mode);
    }

    /// The vendor's session record at this look. A record that moved makes a
    /// read due; one that shows a turn running, or a turn that ended after the
    /// act typed last was typed, says the agent took that act up
    /// ([`Carried::agent_busy`]); the time between a turn's start and its end
    /// is the turn's length.
    fn record_seen(&mut self, record: Option<&SessionRecord>) {
        let Some(record) = record else {
            return;
        };
        let at = record.status_updated_at_ms;
        if self
            .record
            .as_ref()
            .is_some_and(|(status, seen)| *status == record.status && *seen == at)
        {
            return;
        }
        let first = self.record.is_none();
        self.record = Some((record.status.clone(), at));
        self.due_ms = 0;
        if record.status == STATUS_BUSY {
            self.busy_since_ms.get_or_insert(at);
            self.carried.agent_busy();
            return;
        }
        if record.status != STATUS_IDLE {
            return;
        }
        match self.busy_since_ms.take() {
            Some(since) => self.turn_ms = Some(at.saturating_sub(since)),
            // A turn that started and ended between two looks: its length is
            // not known.
            None if !first => self.turn_ms = None,
            None => {}
        }
        if self
            .carried
            .awaiting
            .as_ref()
            .is_some_and(|awaiting| at > awaiting.at_ms)
        {
            self.carried.agent_busy();
        }
    }
}

/// `reply` as journal words.
pub(super) fn reply_words(reply: &Reply) -> String {
    match reply {
        Reply::Written { .. } => "written".to_string(),
        Reply::Submitted => "submitted".to_string(),
        Reply::Unverified => "unverified".to_string(),
        Reply::Left(reason) => format!("left reason={reason}"),
        Reply::Person { .. } => "person".to_string(),
        Reply::Skipped(reason) => format!("skipped reason={reason}"),
        Reply::Busy(words) => format!("busy {words}"),
        Reply::Observed => "observed".to_string(),
        Reply::Denied => "denied".to_string(),
        Reply::Usage(words) => format!("usage {words}"),
        Reply::InDoubt(words) => format!("in-doubt {words}"),
        Reply::Refused(words) => format!("refused {words}"),
    }
}

/// Whether aterm's answer `reply` to a turn says its text reached the session,
/// or may have: the act then waits to be taken up and is never typed again on
/// the same screen. A turn left unsubmitted stands in the composer, where only
/// this harness's own Enter is pressed on it.
pub(super) const fn typed(reply: &Reply) -> bool {
    matches!(
        reply,
        Reply::Submitted | Reply::Unverified | Reply::Left(_) | Reply::InDoubt(_)
    )
}

/// `wait` past `now`, saturating.
fn after(now: u64, wait: Duration) -> u64 {
    now.saturating_add(u64::try_from(wait.as_millis()).unwrap_or(u64::MAX))
}

/// The `run`th rung of `rungs` (from 0), the last for any later one.
fn rung(rungs: &[u64], run: u32) -> u64 {
    let at = usize::try_from(run).unwrap_or(usize::MAX);
    rungs
        .get(at)
        .or_else(|| rungs.last())
        .copied()
        .unwrap_or_default()
}

impl Supervisor {
    /// The typing duties at one look, after a Fable-limit model move declined
    /// it: make the claim follow the mode ([`Supervisor::link_duty_mode`]),
    /// read aterm when due, answer a box the approvals, questions or walls
    /// duty takes (`src/supervise_box_duty.rs`), act on a wall the walls duty
    /// takes (`src/supervise_wall_duty.rs`), and decide and type the turn-end
    /// duty's act. One read serves every duty the claim takes: a mode is
    /// stated only while the claim names every typing duty the launch takes,
    /// so the launch's duties are the claim's here. `record` is the vendor's
    /// session record this look read, and `sight` what it read of the
    /// limit's witness ([`Supervisor::limit_sight`]).
    pub(super) fn duty_look(
        &mut self,
        record: Option<&SessionRecord>,
        sight: &limit::Sight,
        now: u64,
    ) {
        let duties = self.setup.opts.duties;
        let turn_end = duties.takes(Scope::TurnEnd);
        let wall = duties.takes(Scope::Walls);
        // The wall dialogs are boxes, answered under the walls duty.
        let boxes = duties.takes(Scope::Approvals) || duties.takes(Scope::Questions) || wall;
        if !turn_end && !boxes {
            return;
        }
        self.turn_end.record_seen(record);
        let Some(mode) = self.link_duty_mode() else {
            return;
        };
        self.turn_end.mode_seen(mode);
        if mode == Autopilot::Off {
            return;
        }
        // A vendor update that is due has the turn end, a wall's act included,
        // never a box: a claim that takes a box duty still reads the session
        // for its boxes.
        let yields = (turn_end || wall) && self.duty_yields(record, now);
        if yields && !boxes {
            self.turn_end.note(&self.journal, "wait update");
            return;
        }
        if now < self.turn_end.due_ms {
            return;
        }
        self.turn_end.due_ms = now.saturating_add(DUTY_RECHECK_MS);
        let Some(mut read) = self.link_duty_read() else {
            return;
        };
        if let Some(mode) = read.status.autopilot {
            self.turn_end.mode_seen(mode);
        }
        let screen = &read.screen;
        let reading = aterm_phase::read_seen(
            Some(COMMAND),
            &screen.rows,
            screen.cursor.map(|(row, _)| row),
            screen.cursor.map(|(_, col)| col),
            screen.title.as_deref(),
            None,
        );
        // What the harness knows of a usage limit: the usage-limit dialog of
        // one it moves the session off is never waited on.
        let limit = if wall {
            let (limit, dialog_moving) = self.wall_limit(&reading, &read.screen.rows, sight, now);
            read.boxes.walls.moving = dialog_moving;
            limit
        } else {
            walls::Limit::default()
        };
        // A key pressed on a box, or awaited, has the read: the wall and
        // turn-end deciders act on none a key made stale.
        if boxes && self.box_look(&read, &reading, now) {
            return;
        }
        if !turn_end && !wall {
            return;
        }
        if yields {
            self.turn_end.note(&self.journal, "wait update");
            return;
        }
        // A wall acted on, or waited on, has the read too.
        if wall && self.wall_look(&read, &reading, limit, now) {
            return;
        }
        if !turn_end {
            return;
        }
        // Land's `LANDED` counts only over clean repositories: git reads them
        // for that answer, and only then, beside the loop; the turn end waits
        // for the reading.
        let repo = if self
            .turn_end
            .carried
            .reads_repo(read.status.autopilot, &reading)
        {
            let Some(repo) = self.repo_look(now) else {
                return;
            };
            Some(repo)
        } else {
            self.turn_end.repo_idle();
            None
        };
        // Whether a limit on the screen of a session owed its resume is the
        // old one: the transcript names none since the child started.
        let old_wall = self.turn_end.carried.owed_resume && sight.wall.is_none();
        let screen = &read.screen;
        let look = Look {
            autopilot: read.status.autopilot,
            reading: &reading,
            seq: screen.seq,
            now_ms: now,
            turn_ms: self.turn_end.turn_ms,
            repo: repo.as_ref(),
            old_wall,
            pace: read.pace,
        };
        let decision = self
            .turn_end
            .carried
            .decide(&look, &read.texts, &TurnEndTiming::default());
        // An act's line names its rule and aterm's answer; every other
        // decision is its own line.
        match decision {
            Decision::Type { rule, text } => self.duty_type(&read.policy, rule, &text, now),
            Decision::Submit(rule) => self.duty_submit(&read.policy, rule, now),
            other => {
                self.turn_end.note(&self.journal, &other.words());
                if let Decision::Wait { until_ms, .. } = other {
                    self.turn_end.due_ms = until_ms;
                }
            }
        }
        self.land_report();
    }

    /// Report where Land rests to aterm's task record, as codex-alab does
    /// ([`Supervisor::link_report`]): `landed@v1` once its check was answered
    /// `LANDED` over a clean repository ([`Decision::Landed`]), and
    /// `left-to-person@v1` once it was answered with a `NEEDS YOU:` line
    /// ([`Decision::Theirs`]) or its repositories went unread by every probe
    /// [`REPO_CUTS`] allows ([`Supervisor::repo_cut`]) or at every reading
    /// [`REPO_UNREADS`] allows ([`Supervisor::repo_read`]), so aterm reads the
    /// task as done, or as the person's. Made at the look that
    /// decided it and at each later read look until aterm records it or is
    /// found to take no such report; a refusal that may pass (aterm's reading
    /// not yet at rest, a lease run out) and a lost link are reported again at
    /// the next. A new image reports the rest it carries again, which aterm
    /// reads as a resend.
    pub(super) fn land_report(&mut self) {
        let land = self.turn_end.carried.land;
        let act = match land {
            LandState::Landed => LandAct::Landed,
            LandState::Theirs => LandAct::Left,
            LandState::Untold | LandState::Told | LandState::Checking => {
                self.turn_end.land_told = None;
                return;
            }
        };
        if self.turn_end.land_told == Some(land) {
            return;
        }
        let Some(heard) = self.link_report(act) else {
            return;
        };
        let word = act.word();
        match heard {
            Reported::Recorded => {
                self.turn_end.land_told = Some(land);
                self.turn_end
                    .act(&self.journal, &format!("reported act={word}"));
            }
            Reported::Unserved(words) => {
                self.turn_end.land_told = Some(land);
                self.turn_end
                    .act(&self.journal, &format!("unreported act={word} {words}"));
            }
            Reported::Later(words) => self.turn_end.note(
                &self.journal,
                &format!("unreported act={word} {words} later"),
            ),
        }
    }

    /// Land's repository reading for the `LANDED` this look weighs
    /// ([`crate::repo::probe`]), journalled, read beside the loop: a probe is
    /// started at the first such look ([`Supervisor::repo_request`]) and
    /// looked at again every [`REPO_POLL_MS`] while it runs, which is no
    /// reading yet (`None`). A probe that was cut, or that could not be
    /// started, is no reading either: the next is started [`REPO_RETRY_MS`]
    /// later. A reading is weighed by [`Supervisor::repo_read`], which leaves
    /// Land's rest to the person at the bound of unread ones (`None`). Each
    /// `None` sets the duty's next look.
    fn repo_look(&mut self, now: u64) -> Option<RepoRead> {
        match std::mem::take(&mut self.turn_end.repo) {
            RepoLook::Probing(mut probing) => {
                return match probing.poll() {
                    None => {
                        self.turn_end.repo = RepoLook::Probing(probing);
                        self.turn_end.due_ms = now.saturating_add(REPO_POLL_MS);
                        None
                    }
                    Some(Probed::Read(read)) => self.repo_read(now, read),
                    Some(Probed::Cut) => self.repo_cut(now, "cut"),
                };
            }
            RepoLook::After(until) if now < until => {
                self.turn_end.repo = RepoLook::After(until);
                self.turn_end.due_ms = until;
                return None;
            }
            RepoLook::Idle | RepoLook::After(_) => {}
        }
        let request = match self.repo_request() {
            Ok(request) => request,
            Err(read) => return self.repo_read(now, read),
        };
        let Some(exe) = self.setup.own_exe.clone() else {
            return self.repo_cut(now, "unstarted own-exe-unknown");
        };
        match Probing::start(&exe, &request) {
            Ok(probing) => {
                self.turn_end.note(
                    &self.journal,
                    &format!("repo probing folders={}", request.folders.len()),
                );
                self.turn_end.repo = RepoLook::Probing(probing);
                self.turn_end.due_ms = now.saturating_add(REPO_POLL_MS);
                None
            }
            Err(why) => self.repo_cut(now, &format!("unstarted {why}")),
        }
    }

    /// What Land's repository probe is given: the person's home folder, the
    /// transcript the model reader found, and the folders the vendor child
    /// runs in and the harness started in, the child's first. Where a probe
    /// could read nothing, the reading it stands for: [`RepoRead::Unread`],
    /// with no absolute `HOME` ([`repo_home`]) as with no folder and no
    /// transcript known, never a clean one.
    ///
    /// # Errors
    /// The reading that stands for the probe, as above.
    fn repo_request(&self) -> Result<Request, RepoRead> {
        let home = repo_home(std::env::var_os("HOME"))?;
        let child = if self.no_child() {
            None
        } else {
            u32::try_from(self.child.pid)
                .ok()
                .filter(|pid| *pid > 1)
                .and_then(crate::procs::cwd_of)
        };
        let mut folders: Vec<PathBuf> = Vec::new();
        for folder in child.into_iter().chain(self.cwd.clone()) {
            if !folders.contains(&folder) {
                folders.push(folder);
            }
        }
        let transcript = self.facts.transcript().map(std::path::Path::to_path_buf);
        if folders.is_empty() && transcript.is_none() {
            return Err(RepoRead::Unread);
        }
        Ok(Request {
            home,
            transcript,
            folders,
        })
    }

    /// The reading `read` for the `LANDED` this look weighs, journalled: the
    /// probes cut before it count afresh. A clean or found one counts the
    /// unread ones afresh too and is Land's to weigh. An unread one is Land's
    /// to weigh (it gets the nudge to show the status) unless it is the
    /// [`REPO_UNREADS`]th in a row for the task Land carries
    /// ([`Driver::repo_idle`] says when the run ends): then Land's rest is
    /// left to the person ([`Carried::leave_unread`]), said with the reason
    /// and reported to aterm's task record as `left-to-person@v1`
    /// ([`Supervisor::land_report`]), and this look weighs nothing (`None`),
    /// since a `LANDED` git never read is no land and the agent's next claim
    /// of done would only be checked and nudged again. Each reading counts,
    /// a `LANDED` read again because its nudge was held or not taken among
    /// them.
    fn repo_read(&mut self, now: u64, read: RepoRead) -> Option<RepoRead> {
        self.turn_end.repo_cuts = 0;
        self.turn_end
            .note(&self.journal, &format!("repo {}", read.words()));
        if !matches!(read, RepoRead::Unread) {
            self.turn_end.repo_unread = 0;
            return Some(read);
        }
        self.turn_end.repo_unread = self.turn_end.repo_unread.saturating_add(1);
        if self.turn_end.repo_unread < REPO_UNREADS {
            return Some(read);
        }
        let unread = self.turn_end.repo_unread;
        self.turn_end.repo_unread = 0;
        self.turn_end.carried.leave_unread();
        self.turn_end.due_ms = now;
        self.turn_end.act(
            &self.journal,
            &format!(
                "repo unread left-to-person unread={unread}: what the session's repositories \
                 hold was not known at {unread} readings in a row"
            ),
        );
        self.land_report();
        None
    }

    /// The repository probe was cut, or could not start, for `why`: no
    /// reading, and the next probe [`REPO_RETRY_MS`] from `now`. The
    /// [`REPO_CUTS`]th in a row for one `LANDED` starts none: Land's rest is
    /// left to the person ([`Carried::leave_unread`]), said with the reason,
    /// and reported to aterm's task record as `left-to-person@v1`
    /// ([`Supervisor::land_report`]), since a `LANDED` git never read is no
    /// land and the session's repositories may never read within the
    /// probe's bounds.
    fn repo_cut(&mut self, now: u64, why: &str) -> Option<RepoRead> {
        self.turn_end.repo_cuts = self.turn_end.repo_cuts.saturating_add(1);
        if self.turn_end.repo_cuts >= REPO_CUTS {
            let cuts = self.turn_end.repo_cuts;
            self.turn_end.repo = RepoLook::Idle;
            self.turn_end.repo_cuts = 0;
            self.turn_end.carried.leave_unread();
            self.turn_end.due_ms = now;
            self.turn_end.act(
                &self.journal,
                &format!(
                    "repo {why} left-to-person cuts={cuts}: no probe of the session's \
                     repositories ended within its bounds"
                ),
            );
            self.land_report();
            return None;
        }
        let until = now.saturating_add(REPO_RETRY_MS);
        self.turn_end.repo = RepoLook::After(until);
        self.turn_end.due_ms = until;
        self.turn_end
            .note(&self.journal, &format!("repo {why} again-at={until}"));
        None
    }

    /// Whether this look's turn end is left to a vendor update that is due: a
    /// build other than the child's is `current` and was not abandoned, and the
    /// record has been idle for less than the quiet period the update waits for
    /// and [`UPDATE_YIELD_SLACK_MS`]. Past that the update waits for the next
    /// turn end, so a held update never parks the session.
    fn duty_yields(&self, record: Option<&SessionRecord>, now: u64) -> bool {
        let due = self.setup.store.current_number().is_some_and(|current| {
            current != self.child.build.number && self.abandoned.refusal(current).is_none()
        });
        due && record.is_some_and(|record| {
            record.status == STATUS_IDLE
                && now
                    < record
                        .status_updated_at_ms
                        .saturating_add(self.setup.opts.quiet_ms)
                        .saturating_add(UPDATE_YIELD_SLACK_MS)
        })
    }

    /// Type `text` under `rule` as one guarded turn, under `policy`, and
    /// journal aterm's answer. A text that reached the session, or may have,
    /// is the act the decider waits on ([`Carried::typed`]); a write held for a
    /// person, refused for now or overtaken by a person's key is read again
    /// when that has passed.
    fn duty_type(&mut self, policy: &Policy, rule: Rule, text: &str, now: u64) {
        // A turn is one line: a control character in the policy's words (a
        // line break in a quoted list) is typed as the space it stands for.
        let line: String = text
            .chars()
            .map(|c| if c.is_control() { ' ' } else { c })
            .collect();
        let text = line.as_str();
        // A link lost in the write is journalled where it is lost.
        let Some(outcome) = self.link_duty_write(policy, Scope::TurnEnd, Write::Turn(text)) else {
            return;
        };
        let id = rule.id();
        match outcome {
            Typed::Answered(reply) => {
                let words = reply_words(&reply);
                if typed(&reply) {
                    self.turn_end.typed(rule, text, now);
                    self.turn_end
                        .act(&self.journal, &format!("typed rule={id} reply={words}"));
                    return;
                }
                // Nothing reached the session: said once while aterm gives
                // the same answer at each read.
                self.duty_unwritten(policy, &reply, now);
                self.turn_end
                    .note(&self.journal, &format!("unwritten rule={id} reply={words}"));
            }
            Typed::Held(hold) => {
                if let Some(left) = hold.grace_left {
                    self.turn_end.due_ms = after(now, left);
                }
                self.turn_end.note(
                    &self.journal,
                    &format!("held rule={id} {}={}", hold.field, hold.value),
                );
            }
            Typed::Unread(error) => {
                self.turn_end
                    .note(&self.journal, &format!("unread rule={id} {error}"));
            }
            Typed::Denied { .. } => {
                self.turn_end
                    .act(&self.journal, &format!("denied rule={id}"));
            }
        }
    }

    /// Press Enter on this harness's own text under `rule`, which stands
    /// unsent in the composer; the decider never asks for it twice for one act.
    fn duty_submit(&mut self, policy: &Policy, rule: Rule, now: u64) {
        let Some(outcome) = self.link_duty_write(policy, Scope::TurnEnd, Write::Key("enter"))
        else {
            return;
        };
        let id = rule.id();
        let words = match outcome {
            Typed::Answered(reply) => {
                if !matches!(reply, Reply::Written { .. }) {
                    self.duty_unwritten(policy, &reply, now);
                }
                format!("reply={}", reply_words(&reply))
            }
            Typed::Held(hold) => format!("held {}={}", hold.field, hold.value),
            Typed::Unread(error) => format!("unread {error}"),
            Typed::Denied { .. } => "denied".to_string(),
        };
        self.turn_end
            .act(&self.journal, &format!("enter rule={id} {words}"));
    }

    /// When the duty reads again after aterm wrote nothing for `reply`: after
    /// the person's grace for a person's key, soon for a busy sink, after
    /// [`OBSERVED_RETRY_MS`] for an image aterm has not confirmed, and after
    /// a rung of [`USAGE_BACKOFF_MS`] for a request aterm read otherwise than
    /// it was written ([`Driver::usage`]).
    fn duty_unwritten(&mut self, policy: &Policy, reply: &Reply, now: u64) {
        match reply {
            Reply::Person { .. } => self.turn_end.due_ms = after(now, policy.human_grace),
            Reply::Busy(_) => self.turn_end.due_ms = now.saturating_add(BUSY_RETRY_MS),
            Reply::Observed => self.turn_end.due_ms = now.saturating_add(OBSERVED_RETRY_MS),
            Reply::Usage(_) => self.turn_end.usage(now),
            _ => {}
        }
    }
}

#[cfg(test)]
#[path = "supervise_duty_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "supervise_duty_repo_tests.rs"]
mod repo_tests;
