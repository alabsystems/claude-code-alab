// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! THE TURN-END DUTY: what Autopilot types into this harness's own Claude Code
//! session when a turn ends, decided from one certain read of the screen.
//!
//! The claim takes `turn-end` ([`crate::aterm::Duties`]), so this harness
//! carries its own session's turn ends under the tab's Autopilot word (`status
//! autopilot=`), by the rules aterm carried them by and as codex-alab carries
//! itself; nothing else types them:
//!
//! * **Off** types nothing.
//! * **Cruise** continues every turn end and never parks: the continuation
//!   ([`Texts::continue_text`]); at each claim of done, the done check with the
//!   next of Cruise's six kinds of work ([`alab_policy::words::DONE_ANGLES`]).
//!   A question whose answer could consent to something irreversible
//!   ([`Said::Irreversible`]) is answered with aterm's [`REVERSIBLE_ANSWER`]
//!   every time it stands at a turn end, as aterm's engine answers it
//!   (`turn_end.rs`, `words_act`), and a done check over one says to approve
//!   nothing irreversible ([`alab_policy::words::DONE_CHECK_REVERSIBLE`]).
//! * **Land** carries the work until it is on the remote main branch: Land's
//!   instruction once, its nudge after it; at a claim of done, the land check;
//!   at the check's answer `LANDED` over a repository git reads clean
//!   ([`crate::repo`], [`Look::repo`]), nothing more until a person starts
//!   another turn. A `LANDED` over a repository that still holds work no
//!   remote main has, or one git could not read, is no landing: the nudge
//!   names what git found, as aterm's engine does
//!   (`Landed && reading.repo.is_clean()`). A check answered with a list of
//!   what is left gets the nudge quoting it
//!   ([`alab_policy::land_check::listed_suffix`]); one answered with a
//!   `NEEDS YOU:` line is the person's, and Land rests the same way. Every
//!   question, choice or stop a person is asked to answer, irreversible or
//!   not ([`Said::Question`], [`Said::Stop`], [`Said::Irreversible`]), is
//!   answered with Land's answer ([`alab_policy::words::LAND_ANSWER`]: nobody
//!   is here to choose, land the work soonest, never force-push or delete
//!   work) every time it stands at a turn end, in place of Land's
//!   instruction and of every nudge, the nudge after the check's answer
//!   included (where aterm's `land_policy` types it only while no check is
//!   owed): a nudge could read as the consent the ask asks for. A claim of
//!   done gets the check first; the check's `LANDED` is the word alone and
//!   asks nothing, and an answer with a `NEEDS YOU:` line rests whatever else
//!   it says: nothing typed consents to nothing.
//!
//! The texts are aterm's own (the `harness policy` reply's, with the owner's
//! standing rules; [`Texts::of_policy`]), and the vendored policy's where the
//! reply does not carry one ([`Texts::vendored`]).
//!
//! What it never does, each a rule the tests hold with a negative control:
//!
//! * type into a screen it could not read with certainty: Claude Code's reader
//!   must vouch for the phase it reads ([`Reading::phase_authoritative`]), and
//!   anything but an idle prompt or a prose question at the end of a turn is
//!   left alone (a box, a wall, a busy turn, a fresh session, a turn a person
//!   interrupted);
//! * meet an irreversible ask with the mode's continuation, which could read
//!   as consent: the ask gets the mode's answer each time it stands, the same
//!   words again backing the answer off as a short turn ([`Carried::said`]),
//!   so nothing parks and nothing is consented to;
//! * act twice on a screen that did not change: after an act nothing is typed
//!   until the agent has taken it (a busy turn, or another screen), and the only
//!   key ever pressed again is this harness's own Enter, once, where its own
//!   text stands unsent in the composer [`TurnEndTiming::take_within`] after it
//!   was typed;
//! * type while a person is typing or has unsent input: that is the typing
//!   client's judgement, made at the write ([`crate::typing::Typist::write`]:
//!   the person's grace, `composer=`, and aterm's own `if-human=` fence).
//!
//! A turn shorter than [`TurnEndTiming::min_work`] (or one whose last words
//! are those of the turn before) backs the next act off on the policy's ladder
//! ([`TurnEndTiming::short_wait_at`]), so an agent that answers at once is not
//! typed at in a loop; Land's check and its answer are never held back.
//!
//! The pace is the policy's own words, as aterm's engine reads them
//! ([`Pace`]): `continue=false` types nothing but the answer to an ask and
//! the resume this harness owes, and `continue_per_hour=<n>` types at most
//! `n` acts in [`TurnEndTiming::window`], waiting for the oldest to leave it.
//! The decider's timings are aterm's build defaults
//! ([`TurnEndTiming::default`]), which no policy word moves.
//!
//! Nothing here performs I/O; the supervisor's driver (`src/supervise_duty.rs`)
//! reads (the repository too, where [`Carried::reads_repo`] says the look
//! rests on it), writes and journals.

use std::io;
use std::time::Duration;

use alab_policy::config::{Autopilot, CONTINUE_TEXT, TurnEndTiming};
use alab_policy::land_check::{self, CheckAnswer};
use alab_policy::vocab::Phase;
use alab_policy::words::{self, Said};
use aterm_phase::{Program, Reading};

use crate::aterm_wire::{Fields, percent_decode};
use crate::repo::RepoRead;
use crate::transfer_capsule::{Decoder, Encoder};
use crate::walls::Class;

/// The act this duty typed, by the rule that typed it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Rule {
    /// Cruise's continuation.
    Continue,
    /// Cruise's done check, one of its six kinds of work.
    DoneCheck,
    /// Land's instruction, typed once per task.
    Land,
    /// Land's nudge, after its instruction or a check answered with a list.
    LandNudge,
    /// Land's check, at a claim of done.
    LandCheck,
    /// The resume this harness owes a session moved off a usage limit, onto
    /// the fallback model ([`crate::limit`]) or by aterm onto another account
    /// (`restart cause=account`): the vendor stopped at the limit, and nothing
    /// else would ask the agent to go on where it now runs.
    FallbackResume,
    /// Cruise's answer to a question that could consent to something
    /// irreversible ([`REVERSIBLE_ANSWER`]).
    Answer,
    /// Land's answer to any question, choice or stop a person is asked to
    /// answer ([`words::LAND_ANSWER`]).
    LandAnswer,
}

/// [`Rule::Answer`]'s id: aterm's `RULE_ANSWER`, which the vendored policy
/// does not carry.
pub const RULE_ANSWER: &str = "answer@v1";

/// What Cruise answers a question or a choice that names an irreversible act
/// ([`Said::Irreversible`]): aterm's `turn_end::REVERSIBLE_ANSWER`, word for
/// word, which the vendored policy does not carry.
pub const REVERSIBLE_ANSWER: &str = "Nobody is here to approve an irreversible step: take the \
                                     option that deletes, overwrites and force-pushes nothing, \
                                     and keep going.";

/// [`Rule::FallbackResume`]'s id.
pub const RULE_FALLBACK_RESUME: &str = "fallback-resume@v1";

/// What [`Rule::FallbackResume`] types: true of a move onto the fallback
/// model and of one onto another account alike.
pub const FALLBACK_RESUME_TEXT: &str = concat!(
    alab_policy::mark!(),
    " A usage limit stopped the last turn, so this session was moved off it. Continue where \
     you stopped."
);

impl Rule {
    /// The rule's id, as aterm's engine names the same act
    /// (`alab_policy::words`, `alab_policy::land_check`).
    #[must_use]
    pub const fn id(self) -> &'static str {
        match self {
            Self::Continue => words::RULE_CONTINUE,
            Self::DoneCheck => words::RULE_DONE_CHECK,
            Self::Land => words::RULE_LAND,
            Self::LandNudge => words::RULE_LAND_NUDGE,
            Self::LandCheck => land_check::RULE_LAND_CHECK,
            Self::FallbackResume => RULE_FALLBACK_RESUME,
            Self::Answer => RULE_ANSWER,
            Self::LandAnswer => words::RULE_LAND_ANSWER,
        }
    }

    const fn tag(self) -> u8 {
        match self {
            Self::Continue => 1,
            Self::DoneCheck => 2,
            Self::Land => 3,
            Self::LandNudge => 4,
            Self::LandCheck => 5,
            Self::FallbackResume => 6,
            Self::Answer => 7,
            Self::LandAnswer => 8,
        }
    }

    const fn of_tag(tag: u8) -> Option<Self> {
        Some(match tag {
            1 => Self::Continue,
            2 => Self::DoneCheck,
            3 => Self::Land,
            4 => Self::LandNudge,
            5 => Self::LandCheck,
            6 => Self::FallbackResume,
            7 => Self::Answer,
            8 => Self::LandAnswer,
            _ => return None,
        })
    }
}

/// The words the duty types, as aterm's `harness policy` reply serves them for
/// this session, standing rules included.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Texts {
    /// `continue_text=`: Cruise's continuation.
    pub continue_text: String,
    /// `done_check_angles=`: Cruise's kinds of work, one asked at each claim
    /// of done in turn.
    pub done_angles: Vec<String>,
    /// `done_check_tail=`: what follows each angle.
    pub done_tail: String,
    /// What follows the tail: the standing rules aterm appended to its whole
    /// first done check (`done_check_text=`), empty where it appended none.
    pub done_rules: String,
    /// `land_text=`: Land's instruction.
    pub land: String,
    /// `land_nudge_text=`: Land's nudge.
    pub land_nudge: String,
    /// `land_check_text=`: Land's check, its plain form.
    pub land_check: String,
}

impl Texts {
    /// The vendored policy's own words, with no standing rules: what is typed
    /// where aterm's reply carries none.
    #[must_use]
    pub fn vendored() -> Self {
        Self {
            continue_text: CONTINUE_TEXT.to_string(),
            done_angles: words::DONE_ANGLES.map(str::to_string).to_vec(),
            done_tail: words::DONE_TAIL.to_string(),
            done_rules: String::new(),
            land: words::land_text(None),
            land_nudge: words::land_nudge_text(None),
            land_check: land_check::land_check_text(None, false, None),
        }
    }

    /// The texts of one `harness policy` reply: each key aterm sent, decoded,
    /// over the vendored word it replaces; a key absent, `-`, empty or not
    /// decodable keeps the vendored word. `done_check_angles=` is one angle to
    /// a line; a reply whose angles are not six keeps the vendored six.
    #[must_use]
    pub fn of_policy(reply: &str) -> Self {
        let mut texts = Self::vendored();
        let Ok(fields) = Fields::parse_lenient(reply) else {
            return texts;
        };
        let text = |key: &str| {
            fields
                .get(key)
                .filter(|value| *value != "-" && !value.is_empty())
                .and_then(|value| percent_decode(value).ok())
                .filter(|value| !value.trim().is_empty())
        };
        if let Some(value) = text("continue_text") {
            texts.continue_text = value;
        }
        if let Some(value) = text("done_check_angles") {
            let angles: Vec<String> = value.lines().map(str::to_string).collect();
            if angles.len() == words::DONE_ANGLES.len()
                && angles.iter().all(|angle| !angle.trim().is_empty())
            {
                texts.done_angles = angles;
            }
        }
        if let Some(value) = text("done_check_tail") {
            texts.done_tail = value;
        }
        if let Some(whole) = text("done_check_text") {
            let bare = texts.done_check_bare(0);
            if let Some(rules) = whole.strip_prefix(&bare) {
                texts.done_rules = rules.to_string();
            }
        }
        if let Some(value) = text("land_text") {
            texts.land = value;
        }
        if let Some(value) = text("land_nudge_text") {
            texts.land_nudge = value;
        }
        if let Some(value) = text("land_check_text") {
            texts.land_check = value;
        }
        texts
    }

    fn done_check_bare(&self, n: usize) -> String {
        let angle = &self.done_angles[n % self.done_angles.len()];
        format!("{} {angle} {}", words::DONE_CHECK, self.done_tail)
    }

    /// Cruise's done check at its `n`th claim of done (`n` counts from 0):
    /// the base, the angle, the tail, then the standing rules.
    #[must_use]
    pub fn done_check(&self, n: usize) -> String {
        format!("{}{}", self.done_check_bare(n), self.done_rules)
    }

    /// [`Texts::done_check`] at a claim of done that also names an
    /// irreversible act: aterm's reversible clause
    /// ([`words::DONE_CHECK_REVERSIBLE`]) before the standing rules.
    #[must_use]
    pub fn done_check_reversible(&self, n: usize) -> String {
        format!(
            "{} {}{}",
            self.done_check_bare(n),
            words::DONE_CHECK_REVERSIBLE,
            self.done_rules
        )
    }

    /// Cruise's answer to an irreversible ask, as aterm's engine types it:
    /// [`REVERSIBLE_ANSWER`], then the needs hint
    /// ([`alab_policy::needs::NEEDS_HINT`]), then the standing rules (the done
    /// check's, [`Texts::done_rules`]: the policy reply carries no answer
    /// text).
    #[must_use]
    pub fn reversible_answer(&self) -> String {
        format!(
            "{REVERSIBLE_ANSWER} {}{}",
            alab_policy::needs::NEEDS_HINT,
            self.done_rules
        )
    }

    /// Land's answer to any ask, as aterm's engine types it under Land: [`words::LAND_ANSWER`], then the standing rules
    /// (the done check's, [`Texts::done_rules`]).
    #[must_use]
    pub fn land_answer(&self) -> String {
        format!("{}{}", words::LAND_ANSWER, self.done_rules)
    }
}

/// What the turn end's last words ask of a person, read as aterm's engine
/// reads it (`turn_end.rs`, `asked_by`): an ask is answered each time it
/// stands, never met with Land's instruction or nudge, nor, where it is
/// irreversible, with Cruise's continuation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Ask {
    /// Nothing is asked ([`Said::Plain`], [`Said::Offer`]).
    Nothing,
    /// A question, a choice or a stop that names nothing irreversible
    /// ([`Said::Question`], [`Said::Stop`]): Land answers it; Cruise's
    /// continuation already says to decide.
    Choice,
    /// An ask whose answer could consent to something irreversible
    /// ([`Said::Irreversible`]): both modes answer it.
    Irreversible,
}

/// An act typed and not yet taken up by the agent.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Awaiting {
    /// The rule that typed it.
    pub rule: Rule,
    /// What it typed, so its own unsent text can be told from anyone else's.
    pub text: String,
    /// When it was typed, epoch ms.
    pub at_ms: u64,
    /// The screen's content sequence at the first look after it, which every
    /// later look compares: an unchanged screen is never acted on again.
    pub seen_seq: Option<u64>,
    /// Whether its Enter was pressed again, which is done once.
    pub entered: bool,
}

/// Where Land stands for the task in hand.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum LandState {
    /// Land's instruction is owed.
    #[default]
    Untold,
    /// The instruction was typed; the nudge carries the work on.
    Told,
    /// The check was typed; its answer is owed.
    Checking,
    /// The check was answered `LANDED`: nothing more is typed for this task.
    Landed,
    /// The check was answered with a `NEEDS YOU:` line: the rest is the
    /// person's, and nothing more is typed for this task.
    Theirs,
}

impl LandState {
    const fn tag(self) -> u8 {
        match self {
            Self::Untold => 0,
            Self::Told => 1,
            Self::Checking => 2,
            Self::Landed => 3,
            Self::Theirs => 4,
        }
    }

    const fn of_tag(tag: u8) -> Option<Self> {
        Some(match tag {
            0 => Self::Untold,
            1 => Self::Told,
            2 => Self::Checking,
            3 => Self::Landed,
            4 => Self::Theirs,
            _ => return None,
        })
    }
}

/// The turn end being handled: when it was first seen, and how many short
/// turns in a row it ends.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Ended {
    /// When this harness first saw the turn over, epoch ms.
    pub at_ms: u64,
    /// The short turns in a row, this one included: 0 after a real one.
    pub streak: u32,
}

/// The most act times [`Carried::acts`] keeps: a `continue_per_hour` above it
/// never waits.
pub const MAX_PACED_ACTS: usize = 1024;

/// The policy words that pace the duty's acts, as aterm's own loop reads them
/// from its `[harness]` table and sends them in every `harness policy` reply
/// (`crates/aterm-agent/src/supervise/config.rs`: `continue_policy`,
/// `continue_per_hour`; `policy/turn_end.rs`: `words_gate`, `budget_spent`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Pace {
    /// `continue=true`: the mode's act is typed at a turn end. `false` types
    /// none but the answer to an ask ([`Rule::Answer`], [`Rule::LandAnswer`])
    /// and the resume this harness owes ([`Rule::FallbackResume`]).
    pub continues: bool,
    /// `continue_per_hour=<n>`: at most `n` acts in [`TurnEndTiming::window`];
    /// `0` is no cap.
    pub per_hour: u32,
}

impl Default for Pace {
    /// aterm's own defaults: every act typed, no cap.
    fn default() -> Self {
        Self {
            continues: true,
            per_hour: 0,
        }
    }
}

impl Pace {
    /// The pace one `harness policy` reply states. A key absent, or not
    /// `true`/`false` and a plain decimal that fits, keeps aterm's default
    /// ([`Pace::default`]).
    #[must_use]
    pub fn of_policy(reply: &str) -> Self {
        let mut pace = Self::default();
        let Ok(fields) = Fields::parse_lenient(reply) else {
            return pace;
        };
        if fields.get("continue") == Some("false") {
            pace.continues = false;
        }
        if let Some(n) = fields
            .get("continue_per_hour")
            .filter(|value| !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit()))
            .and_then(|value| value.parse::<u32>().ok())
        {
            pace.per_hour = n;
        }
        pace
    }
}

/// The duty's memory of the session, which an own-image move carries
/// ([`Carried::put`]) so the new image never types again what the old one
/// already typed.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Carried {
    /// The act typed last and not yet taken up.
    pub awaiting: Option<Awaiting>,
    /// How many done checks Cruise has typed ([`words::next_checks`]); the
    /// next angle is this count's.
    pub checks: u8,
    /// Land's state for the task in hand.
    pub land: LandState,
    /// The turn end being handled, until the agent is busy again.
    pub ended: Option<Ended>,
    /// The short turns in a row up to the turn end handled last.
    pub streak: u32,
    /// A digest of the last words of the turn end handled last, so a turn
    /// that says the same again counts as short.
    pub said: Option<u64>,
    /// A resume is owed: the child was ended at a usage limit and started
    /// again off it, on the fallback model or on another account.
    pub owed_resume: bool,
    /// When each act was typed (epoch ms), oldest first and at most
    /// [`MAX_PACED_ACTS`], for [`Pace::per_hour`].
    pub acts: Vec<u64>,
}

/// What one look sees.
#[derive(Clone, Copy, Debug)]
pub struct Look<'a> {
    /// The session's Autopilot mode, as aterm's `status` states it.
    pub autopilot: Option<Autopilot>,
    /// Claude Code's screen, as aterm-phase reads it.
    pub reading: &'a Reading,
    /// The screen's content sequence.
    pub seq: u64,
    /// Now, epoch ms.
    pub now_ms: u64,
    /// How long the turn that ended ran, where the vendor's session record
    /// said; `None` where it did not.
    pub turn_ms: Option<u64>,
    /// What git read of the session's repository for this look, where the
    /// driver read it ([`Carried::reads_repo`]). Land counts a `LANDED` only
    /// over a clean reading; with none, the repository counts as unread.
    pub repo: Option<&'a RepoRead>,
    /// The conversation's transcript names no usage limit since the child
    /// started ([`crate::limit::Sight::wall`] past the move's floor): a usage
    /// wall on the screen of a session this harness moved is the old one the
    /// resumed child still shows, not the limit of the model it now runs.
    pub old_wall: bool,
    /// The policy's pace for this look ([`Pace::of_policy`]).
    pub pace: Pace,
}

/// What the duty does at one look.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Decision {
    /// Nothing, for the reason named (one journal word).
    Nothing(&'static str),
    /// Nothing until `until_ms`, for the reason named: look again then.
    Wait {
        /// When to look again, epoch ms.
        until_ms: u64,
        /// Why (one journal word).
        why: &'static str,
    },
    /// Type `text` under `rule` and submit it.
    Type {
        /// The rule.
        rule: Rule,
        /// What is typed.
        text: String,
    },
    /// Press Enter on this harness's own text under `rule`, which stands
    /// unsent in the composer.
    Submit(Rule),
    /// The land check was answered `LANDED`: the task is done, and Land rests.
    Landed,
    /// The land check was answered with a `NEEDS YOU:` line: the person's.
    Theirs,
}

impl Decision {
    /// The decision as journal words: `nothing <why>`, `wait <why> until=<ms>`,
    /// `type <rule>`, `submit <rule>`, `landed`, `theirs`.
    #[must_use]
    pub fn words(&self) -> String {
        match self {
            Self::Nothing(why) => format!("nothing {why}"),
            Self::Wait { until_ms, why } => format!("wait {why} until={until_ms}"),
            Self::Type { rule, .. } => format!("type {}", rule.id()),
            Self::Submit(rule) => format!("submit {}", rule.id()),
            Self::Landed => "landed".to_string(),
            Self::Theirs => "theirs".to_string(),
        }
    }
}

/// A digest of `text`, to tell one turn's last words from the next.
fn digest(text: &str) -> u64 {
    // FNV-1a: the same in every build and image, unlike the std hasher's.
    text.bytes().fold(0xcbf2_9ce4_8422_2325, |hash, byte| {
        (hash ^ u64::from(byte)).wrapping_mul(0x0100_0000_01b3)
    })
}

/// How Claude Code folds a long paste in its composer, `[Pasted text #<n>
/// +<m> lines]` (aterm's `supervise/report.rs`, `PASTED`).
const PASTED: &str = "[Pasted text";

/// A text with a line break, or of this many characters, may be folded into
/// [`PASTED`]'s chip (aterm's `report.rs`, `paste_like`).
const PASTE_CHARS: usize = 200;

/// `text` without its whitespace.
fn bare(text: &str) -> String {
    text.chars().filter(|c| !c.is_whitespace()).collect()
}

/// Whether the composer's rows, joined as `shown`, are the chip Claude Code
/// folds `text` into: one [`PASTED`] chip alone, for a text that may fold,
/// whose `+<m> lines`, where it names a count, is within one of `text`'s line
/// breaks (aterm's `report.rs`, `paste_fits`).
fn folded(shown: &str, text: &str) -> bool {
    let shown = shown.trim();
    let may_fold = text.contains('\n') || text.chars().count() >= PASTE_CHARS;
    if !may_fold
        || !shown.starts_with(PASTED)
        || !shown.ends_with(']')
        || shown.matches(']').count() != 1
    {
        return false;
    }
    let Some(lines) = shown
        .split_once(" +")
        .and_then(|(_, rest)| rest.split_once(" line"))
        .map(|(count, _)| count)
        .filter(|count| !count.is_empty() && count.bytes().all(|byte| byte.is_ascii_digit()))
        .and_then(|count| count.parse::<u64>().ok())
    else {
        return true;
    };
    let breaks = u64::try_from(text.matches('\n').count()).unwrap_or(u64::MAX);
    lines.abs_diff(breaks) <= 1
}

/// Whether the composer on `reading` holds this harness's own `text`, unsent:
/// what it shows, whitespace aside (rows a long text wraps over may break
/// inside a word), opens `text` and is no shorter than `text`'s first twenty
/// characters that are not whitespace; or it is the chip Claude Code folds
/// `text` into when it is pasted ([`PASTED`]).
#[must_use]
pub fn own_draft(reading: &Reading, text: &str) -> bool {
    let Some((_, lines)) = &reading.composer else {
        return false;
    };
    if folded(&lines.join(" "), text) {
        return true;
    }
    let shown: String = lines.iter().map(|line| bare(line)).collect();
    let text = bare(text);
    let floor = text.chars().take(20).count();
    !shown.is_empty() && shown.chars().count() >= floor && text.starts_with(&shown)
}

/// `ms` past `at_ms`, saturating.
fn after(at_ms: u64, wait: Duration) -> u64 {
    at_ms.saturating_add(u64::try_from(wait.as_millis()).unwrap_or(u64::MAX))
}

impl Carried {
    /// The agent is busy: an act typed is taken up, and the turn end handled
    /// is over. A busy turn nobody here typed for (the person's, or the
    /// agent's own wake) after a turn end is a new task, which Land carries
    /// afresh after it rested.
    pub fn agent_busy(&mut self) {
        if self.awaiting.is_none()
            && self.ended.is_some()
            && matches!(self.land, LandState::Landed | LandState::Theirs)
        {
            self.land = LandState::Untold;
        }
        self.awaiting = None;
        self.ended = None;
    }

    /// A child was started again (a crash relaunch, a vendor update, a
    /// restart, a model move): an act typed into the child that ended was not
    /// taken up, and the turn end being handled was that child's. The new
    /// child shows the conversation's last words again, which are no second
    /// answer, so they count as no short turn. `owed` is a resume this
    /// harness owes the new child (a move off a usage limit).
    pub fn child_restarted(&mut self, owed: bool) {
        self.awaiting = None;
        self.ended = None;
        self.said = None;
        self.owed_resume = self.owed_resume || owed;
    }

    /// The walls duty typed `rule` into the session ([`crate::walls`]). Its
    /// resume past a usage limit ([`crate::walls::RULE_LIMIT_RESUME`]),
    /// typed at the limit of the model or account the session was moved
    /// onto, carries the work on where the owed resume would have: the
    /// resume is no longer owed, so no second one follows at the next turn
    /// end. Every other wall act (an outage's resume, `/compact`) leaves it
    /// owed.
    pub fn wall_typed(&mut self, rule: &str) {
        if rule == crate::walls::RULE_LIMIT_RESUME {
            self.owed_resume = false;
        }
    }

    /// Land's check was answered `LANDED`, and no probe of the session's
    /// repositories ended within its bounds for it, or git could not read
    /// them at as many readings in a row as the driver allows: the rest is
    /// left to the person, as a `NEEDS YOU:` answer leaves it, so Land
    /// neither waits unbounded on a read that never ends, nor nudges without
    /// end over repositories git never reads, nor counts a land git did not
    /// see. Nothing changes where no check is owed.
    pub fn leave_unread(&mut self) {
        if self.land == LandState::Checking {
            self.land = LandState::Theirs;
        }
    }

    /// The session's Autopilot mode moved: Land carries the task in hand
    /// afresh.
    pub fn autopilot_moved(&mut self) {
        self.land = LandState::Untold;
    }

    /// Whether a look at `reading` under `autopilot` may rest on the
    /// session's repository ([`Look::repo`]): Land waits on its check's
    /// answer, and the agent's last words answer it `LANDED`. Only then is the
    /// repository read, so it is read once for each `LANDED` the decider
    /// weighs, and never at an ordinary turn end.
    #[must_use]
    pub fn reads_repo(&self, autopilot: Option<Autopilot>, reading: &Reading) -> bool {
        autopilot == Some(Autopilot::Land)
            && self.land == LandState::Checking
            && reading.said_tail.as_deref().is_some_and(|tail| {
                land_check::land_check_answer(tail, None, false) == CheckAnswer::Landed
            })
    }

    /// The write of `text` under `rule` was answered as typed, or as possibly
    /// typed: wait for it to be taken up, and count what it did.
    pub fn typed(&mut self, rule: Rule, text: &str, now_ms: u64) {
        self.awaiting = Some(Awaiting {
            rule,
            text: text.to_string(),
            at_ms: now_ms,
            seen_seq: None,
            entered: false,
        });
        match rule {
            Rule::DoneCheck => self.checks = words::next_checks(self.checks),
            Rule::Land | Rule::LandNudge => self.land = LandState::Told,
            Rule::LandCheck => self.land = LandState::Checking,
            Rule::FallbackResume => self.owed_resume = false,
            Rule::Answer | Rule::LandAnswer | Rule::Continue => {}
        }
        if self.acts.len() >= MAX_PACED_ACTS {
            self.acts.remove(0);
        }
        self.acts.push(now_ms);
    }

    /// `act` under the policy's pace ([`Look::pace`]): with `continue=false`
    /// only the answer to an ask and the owed resume are typed, and no act is
    /// typed while [`TurnEndTiming::window`] holds `continue_per_hour` acts:
    /// the duty waits for the oldest to leave it. Every other decision passes.
    fn paced(&mut self, act: Decision, look: &Look<'_>, timing: &TurnEndTiming) -> Decision {
        let rule = match &act {
            Decision::Type { rule, .. } => *rule,
            _ => return act,
        };
        if !look.pace.continues
            && !matches!(rule, Rule::Answer | Rule::LandAnswer | Rule::FallbackResume)
        {
            return Decision::Nothing("continue-off");
        }
        let window = u64::try_from(timing.window.as_millis()).unwrap_or(u64::MAX);
        self.acts
            .retain(|at| look.now_ms.saturating_sub(*at) < window);
        let cap = usize::try_from(look.pace.per_hour).unwrap_or(usize::MAX);
        match self.acts.first() {
            Some(oldest) if cap != 0 && self.acts.len() >= cap => Decision::Wait {
                until_ms: oldest.saturating_add(window),
                why: "continue-per-hour",
            },
            _ => act,
        }
    }

    /// Decide one look with `texts` and `timing`, updating what the duty
    /// remembers of the session.
    #[must_use]
    pub fn decide(&mut self, look: &Look<'_>, texts: &Texts, timing: &TurnEndTiming) -> Decision {
        let reading = look.reading;
        let mode = match look.autopilot {
            None => return Decision::Nothing("autopilot-unknown"),
            Some(Autopilot::Off) => return Decision::Nothing("autopilot-off"),
            Some(mode) => mode,
        };
        if !matches!(reading.program, Program::Claude) {
            return Decision::Nothing("not-claude");
        }
        if !reading.phase_authoritative {
            return Decision::Nothing("uncertain");
        }
        if matches!(reading.phase, Phase::Busy) {
            self.agent_busy();
            return Decision::Nothing("busy");
        }
        if let Some(awaiting) = &mut self.awaiting {
            let seen = *awaiting.seen_seq.get_or_insert(look.seq);
            if seen == look.seq {
                return Self::awaiting_take(awaiting, look, timing);
            }
            // The screen moved with no busy turn seen: the agent took the act
            // up and answered between two looks, and this is that turn's end.
            self.awaiting = None;
            self.ended = None;
        }
        // The resume a moved session is owed is typed at the old limit's
        // wall, which the resumed child still shows, but never at a limit of
        // the model it now runs: that one is kept owed, and the wall is the
        // walls duty's.
        let at_old_wall = self.owed_resume
            && look.old_wall
            && reading
                .wall
                .as_ref()
                .is_none_or(|wall| matches!(Class::of(wall.kind), Class::Usage | Class::Model));
        match &reading.phase {
            Phase::Prompt => return Decision::Nothing("box"),
            Phase::Limited { .. } if !at_old_wall => return Decision::Nothing("wall"),
            Phase::Limited { .. } | Phase::Busy | Phase::Idle | Phase::Question => {}
        }
        if reading.wall.is_some() && !at_old_wall {
            return Decision::Nothing("wall");
        }
        if reading.prompt.is_some() || reading.survey {
            return Decision::Nothing("box");
        }
        if reading.fresh {
            return Decision::Nothing("fresh");
        }
        if reading.interrupted {
            return Decision::Nothing("interrupted");
        }
        let tail = reading.said_tail.as_deref();
        let ended = self.turn_ended(look, timing, tail);
        if self.owed_resume {
            let resume = Decision::Type {
                rule: Rule::FallbackResume,
                text: FALLBACK_RESUME_TEXT.to_string(),
            };
            return self.paced(resume, look, timing);
        }
        // An ask that could consent to something irreversible gets the
        // mode's answer each time it stands, and under Land so does any ask,
        // as aterm's engine answers them (`words_act`, `land_policy`): the
        // continuation could read as the consent it asks for, and Land's
        // nudge leaves a choice unmade. The same words again are a short turn ([`Carried::said`]),
        // which backs the answer off, so nothing parks; the awaited act
        // ([`Awaiting::seen_seq`]) keeps it from being typed twice into one
        // screen.
        let ask = match words::classify_said(tail) {
            Said::Irreversible(_) => Ask::Irreversible,
            Said::Question | Said::Stop(_) => Ask::Choice,
            Said::Plain | Said::Offer => Ask::Nothing,
        };
        let act = match mode {
            Autopilot::Land => self.land_act(ended, look, tail, ask, texts, timing),
            Autopilot::Cruise | Autopilot::Off => {
                self.cruise_act(ended, look.now_ms, tail, ask, texts, timing)
            }
        };
        self.paced(act, look, timing)
    }

    /// An act stands untaken on a screen that has not moved: nothing, unless
    /// its own text still stands unsent [`TurnEndTiming::take_within`] after it
    /// was typed, when its Enter is pressed once more.
    fn awaiting_take(awaiting: &mut Awaiting, look: &Look<'_>, timing: &TurnEndTiming) -> Decision {
        if awaiting.entered || !own_draft(look.reading, &awaiting.text) {
            return Decision::Nothing("awaiting-take");
        }
        let due = after(awaiting.at_ms, timing.take_within);
        if look.now_ms < due {
            return Decision::Wait {
                until_ms: due,
                why: "awaiting-take",
            };
        }
        awaiting.entered = true;
        Decision::Submit(awaiting.rule)
    }

    /// The turn end of this look: the one being handled, or a new one, whose
    /// streak of short turns is counted here.
    fn turn_ended(&mut self, look: &Look<'_>, timing: &TurnEndTiming, tail: Option<&str>) -> Ended {
        if let Some(ended) = self.ended {
            return ended;
        }
        let said = tail.map(digest);
        let short = look
            .turn_ms
            .is_some_and(|ms| Duration::from_millis(ms) < timing.min_work)
            || (said.is_some() && said == self.said);
        self.streak = if short {
            self.streak.saturating_add(1)
        } else {
            0
        };
        self.said = said;
        let ended = Ended {
            at_ms: look.now_ms,
            streak: self.streak,
        };
        self.ended = Some(ended);
        ended
    }

    /// The short-turn back-off: a wait while the policy's ladder still holds
    /// the turn end `ended` at `now_ms`.
    fn backed_off(ended: Ended, now_ms: u64, timing: &TurnEndTiming) -> Option<Decision> {
        let until_ms = after(ended.at_ms, timing.short_wait_at(ended.streak)?);
        (now_ms < until_ms).then_some(Decision::Wait {
            until_ms,
            why: "short-turn",
        })
    }

    /// Cruise's act, past the short-turn back-off: the done check at a claim
    /// of done (with the reversible clause where the words also name an
    /// irreversible act), the reversible answer to an irreversible ask, else
    /// the continuation.
    fn cruise_act(
        &mut self,
        ended: Ended,
        now_ms: u64,
        tail: Option<&str>,
        ask: Ask,
        texts: &Texts,
        timing: &TurnEndTiming,
    ) -> Decision {
        if let Some(wait) = Self::backed_off(ended, now_ms, timing) {
            return wait;
        }
        if words::says_done(tail) {
            let n = usize::from(self.checks);
            return Decision::Type {
                rule: Rule::DoneCheck,
                text: if ask == Ask::Irreversible {
                    texts.done_check_reversible(n)
                } else {
                    texts.done_check(n)
                },
            };
        }
        if ask == Ask::Irreversible {
            return Decision::Type {
                rule: Rule::Answer,
                text: texts.reversible_answer(),
            };
        }
        Decision::Type {
            rule: Rule::Continue,
            text: texts.continue_text.clone(),
        }
    }

    /// Land's act: the check's answer read first and the check typed at a
    /// claim of done, neither ever held back, nor its instruction; Land's
    /// answer to any ask ([`Ask::Choice`], [`Ask::Irreversible`]), and the
    /// nudge, only past the short-turn back-off. A `LANDED` counts only over a
    /// clean repository ([`Look::repo`]); over any other the nudge is typed
    /// with what git found, or with aterm's ask to show the status where git
    /// could not read it, and Land waits for the next claim of done. An
    /// answer to the check that asks anything gets Land's answer in place of
    /// the nudge, which could read as the consent it asks for and would leave
    /// its choice unmade; the check stays owed. `LANDED` is the word alone
    /// and asks nothing; an answer with a `NEEDS YOU:` line rests whatever
    /// else it says: nothing is typed, so nothing is consented to.
    fn land_act(
        &mut self,
        ended: Ended,
        look: &Look<'_>,
        tail: Option<&str>,
        ask: Ask,
        texts: &Texts,
        timing: &TurnEndTiming,
    ) -> Decision {
        let now_ms = look.now_ms;
        match self.land {
            LandState::Landed => return Decision::Nothing("landed"),
            LandState::Theirs => return Decision::Nothing("theirs"),
            LandState::Checking => {
                let Some(tail) = tail else {
                    return Decision::Nothing("check-unanswered");
                };
                return match land_check::land_check_answer(tail, None, false) {
                    // A repository nobody read counts as unread, never as
                    // clean.
                    CheckAnswer::Landed => match look.repo.unwrap_or(&RepoRead::Unread).ask() {
                        None => {
                            self.land = LandState::Landed;
                            Decision::Landed
                        }
                        Some(found) => Decision::Type {
                            rule: Rule::LandNudge,
                            text: format!("{} {found}", texts.land_nudge),
                        },
                    },
                    CheckAnswer::Needs { .. } => {
                        self.land = LandState::Theirs;
                        Decision::Theirs
                    }
                    CheckAnswer::Left(_) if ask != Ask::Nothing => {
                        Self::land_answered(ended, now_ms, texts, timing)
                    }
                    CheckAnswer::Left(list) => Decision::Type {
                        rule: Rule::LandNudge,
                        text: match land_check::listed_suffix(&list) {
                            Some(listed) => format!("{} {listed}", texts.land_nudge),
                            None => texts.land_nudge.clone(),
                        },
                    },
                };
            }
            LandState::Untold | LandState::Told => {}
        }
        if words::says_done(tail) {
            return Decision::Type {
                rule: Rule::LandCheck,
                text: texts.land_check.clone(),
            };
        }
        // Any ask under Land, a choice as much as an irreversible one, is
        // answered before Land's instruction or nudge.
        if ask != Ask::Nothing {
            return Self::land_answered(ended, now_ms, texts, timing);
        }
        if self.land == LandState::Untold {
            return Decision::Type {
                rule: Rule::Land,
                text: texts.land.clone(),
            };
        }
        if let Some(wait) = Self::backed_off(ended, now_ms, timing) {
            return wait;
        }
        Decision::Type {
            rule: Rule::LandNudge,
            text: texts.land_nudge.clone(),
        }
    }

    /// Land's answer to an ask that stands at the turn end `ended`, past the
    /// short-turn back-off: the same ask again is a short turn, which spaces
    /// the answers, so nothing parks and nothing loops.
    fn land_answered(ended: Ended, now_ms: u64, texts: &Texts, timing: &TurnEndTiming) -> Decision {
        if let Some(wait) = Self::backed_off(ended, now_ms, timing) {
            return wait;
        }
        Decision::Type {
            rule: Rule::LandAnswer,
            text: texts.land_answer(),
        }
    }

    /// Write the memory into an own-image move's payload. The text of an act
    /// awaited is cut to its first [`MAX_TEXT_BYTES`] at a character's
    /// boundary: a composer that shows more than the cut never reads as its
    /// own draft ([`own_draft`]), so nothing is pressed on it.
    ///
    /// # Errors
    /// The payload's bound was reached.
    pub fn put(&self, encoder: &mut Encoder) -> io::Result<()> {
        encoder.boolean(self.awaiting.is_some())?;
        if let Some(awaiting) = &self.awaiting {
            encoder.u8(awaiting.rule.tag())?;
            let mut end = awaiting.text.len().min(MAX_TEXT_BYTES);
            while !awaiting.text.is_char_boundary(end) {
                end -= 1;
            }
            encoder.bytes(&awaiting.text.as_bytes()[..end])?;
            encoder.u64(awaiting.at_ms)?;
            encoder.boolean(awaiting.seen_seq.is_some())?;
            encoder.u64(awaiting.seen_seq.unwrap_or(0))?;
            encoder.boolean(awaiting.entered)?;
        }
        encoder.u8(self.checks)?;
        encoder.u8(self.land.tag())?;
        encoder.boolean(self.ended.is_some())?;
        if let Some(ended) = self.ended {
            encoder.u64(ended.at_ms)?;
            encoder.u32(ended.streak)?;
        }
        encoder.u32(self.streak)?;
        encoder.boolean(self.said.is_some())?;
        encoder.u64(self.said.unwrap_or(0))?;
        encoder.boolean(self.owed_resume)?;
        let kept = &self.acts[self.acts.len().saturating_sub(MAX_PACED_ACTS)..];
        encoder.u32(u32::try_from(kept.len()).unwrap_or(u32::MAX))?;
        for at in kept {
            encoder.u64(*at)?;
        }
        Ok(())
    }

    /// Read the memory [`Carried::put`] wrote.
    ///
    /// # Errors
    /// The payload ends early, or holds a rule, a Land state or a text this
    /// build does not know, or more than [`MAX_PACED_ACTS`] act times.
    pub fn get(decoder: &mut Decoder<'_>) -> io::Result<Self> {
        let invalid = |what: &str| io::Error::new(io::ErrorKind::InvalidData, what.to_string());
        let awaiting = if decoder.boolean()? {
            let rule = Rule::of_tag(decoder.u8()?).ok_or_else(|| invalid("unknown duty rule"))?;
            let text = String::from_utf8(decoder.bytes(MAX_TEXT_BYTES)?.to_vec())
                .map_err(|_| invalid("duty text is not UTF-8"))?;
            let at_ms = decoder.u64()?;
            let seen = decoder.boolean()?;
            let seq = decoder.u64()?;
            Some(Awaiting {
                rule,
                text,
                at_ms,
                seen_seq: seen.then_some(seq),
                entered: decoder.boolean()?,
            })
        } else {
            None
        };
        let checks = decoder.u8()?;
        let land = LandState::of_tag(decoder.u8()?).ok_or_else(|| invalid("unknown Land state"))?;
        let ended = if decoder.boolean()? {
            Some(Ended {
                at_ms: decoder.u64()?,
                streak: decoder.u32()?,
            })
        } else {
            None
        };
        let streak = decoder.u32()?;
        let said_known = decoder.boolean()?;
        let said = decoder.u64()?;
        let owed_resume = decoder.boolean()?;
        let count = usize::try_from(decoder.u32()?).unwrap_or(usize::MAX);
        if count > MAX_PACED_ACTS {
            return Err(invalid("too many paced acts"));
        }
        let mut acts = Vec::with_capacity(count);
        for _ in 0..count {
            acts.push(decoder.u64()?);
        }
        Ok(Self {
            awaiting,
            checks,
            land,
            ended,
            streak,
            said: said_known.then_some(said),
            owed_resume,
            acts,
        })
    }
}

/// The longest text an act may carry across an own-image move.
pub const MAX_TEXT_BYTES: usize = 16 * 1024;

#[cfg(test)]
#[path = "duty_tests.rs"]
mod tests;
