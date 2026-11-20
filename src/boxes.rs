// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! THE APPROVALS AND QUESTIONS DUTIES: what this harness presses on a box its
//! own Claude Code session draws, decided from one certain read of the screen.
//!
//! The claim takes `approvals` and `questions` ([`crate::aterm::Duties`]),
//! each by its own word, so this harness answers its own session's permission
//! boxes and question dialogs under the tab's Autopilot word, by the rules
//! aterm answered them by (`aterm/crates/aterm-agent/src/supervise/policy/
//! approval.rs` and `question.rs`, ported here, narrower where this harness
//! lacks one of aterm's readers). Nothing else answers them: a box no claim
//! takes is the person's. A box is one duty's
//! ([`scope_of`], aterm's `duty::prompt_scope`): a question dialog is
//! `questions`'; a usage limit, a model switch and an autocompact offer are
//! `walls`' (aterm's `limit_wait`, `autocompact_offer` and `model_confirm`,
//! ported here too); every other box is `approvals`'.
//!
//! **What it presses** ([`read`]), each under the rule id aterm's engine books
//! it by:
//!
//! * under `approve = "all"`, the one option that allows this one call
//!   (`allow-once@v1`), and on a plan box the yes that grants no standing mode
//!   (`plan-approve@v2`): never an option that keeps its answer for later
//!   asks, buys something, or grants more than the call (a session grant, a
//!   mode switch), so no press widens the launch's permission mode;
//! * at every level but `none`, Claude Code's own housekeeping dialogs as aterm
//!   answers them: `Not now` on the auto-mode setup offer at every level
//!   (`auto-mode-setup@v1`), `Resume` on the conversation-resume dialog
//!   (`conversation-resume@v1`), and the Claude in Chrome intro's confirm
//!   (`chrome-intro@v1`);
//! * on a question dialog, while the policy answers questions (`questions=`
//!   not `ask`), the option the agent marked `(Recommended)`
//!   (`answer-recommended@v1`), and the review tab's `Submit answers` once
//!   every question in it has an answer;
//! * on Claude Code's usage-limit dialog, while `limit_wait` is on, `Wait
//!   here, then continue automatically` (`limit-wait@v1`): Claude Code then
//!   goes on by itself at the reset, which the walls duty
//!   ([`crate::walls`]) leaves to it first. Never on the dialog of a limit
//!   the harness moves the session off ([`WallDialogs::moving`]), where
//!   waiting would keep the session on the model the move leaves;
//! * on the autocompact offer, while `autocompact_offer` names a size, the
//!   accept row stepped to that size (`autocompact-offer@v1`);
//! * under `approve = "all"`, a model switch's cost confirmation's yes
//!   (`model-confirm@v1`), unless the person's own hook asked for it.
//!
//! **What it leaves to the person** ([`Act::Leave`], said once a box): every
//! box under `approve = "none"`; every permission box under `approve =
//! "safe"`, whose proofs (aterm's read-only command rules) are not ported;
//! a box the owner's own Claude Code settings send to a person (its ask
//! reason drawn on it), and one of a kind Claude Code draws no ask reason on
//! (a file's Edit, Write, Overwrite or Read), since this harness does not read
//! the owner's ask rules and hooks; on the usage-limit dialog every row but
//! the wait (stop, an upgrade, usage credits, a limit reset: aterm's
//! `limit-reset@v1` is not ported, so no reset is ever spent); the
//! autocompact offer's keep row; Claude Code's rm safety warning; a box
//! whose title is off the screen; the MCP server and sign-in dialogs; the
//! folder-trust dialog, which the cold launch's own exchange settles
//! ([`crate::folder_trust`]); a dialog of no kind a duty answers; any box where
//! aterm would decline, wait or buy, since no key here declines; and a question
//! with no option marked `(Recommended)`, one that takes several answers, one
//! a person began typing into or ticked, one whose words name a destructive
//! act ([`alab_policy::words::destructive_word`]: the turn-end duty leaves
//! such a question to the person too), and one whose answer is kept for later
//! asks.
//!
//! **One key per read.** A box answered by a digit gets that digit; one
//! answered by moving its focus gets one arrow at a read, then, when the focus
//! is on the answer, Enter. Every key is fenced on the read it was decided
//! from ([`crate::typing::Press`]): aterm writes nothing once the screen moved
//! (`if-gen=`), no row matches the box's row (`if=`), or a person made a
//! gesture (`if-human=`). The key that answers carries what it means for the
//! box (`act=approve`, `act=answer`).
//!
//! **What it never does** ([`Watch`]), each a rule the tests hold with a
//! negative control:
//!
//! * press on a screen it could not read with certainty: Claude Code's reader
//!   must vouch for the box ([`Reading::phase_authoritative`]);
//! * press while a person is at the box: a gesture since the box showed makes
//!   it theirs until it goes, and a gesture before it holds the key for the
//!   box's grace ([`grace`]); a key never reaches a draft in the composer,
//!   which a box holds the focus from;
//! * press again on a screen that did not change, or on a face whose focus
//!   did not move since the last key (a key the session has not read yet
//!   would be followed by one decided from a stale focus); a face that shows
//!   the same focus [`KEY_SETTLE_MS`] after a key, or still stands after its
//!   answer, is the person's, and so is a box that comes back after
//!   [`MAX_REPEATS`] answers in a row;
//! * offer a key aterm presses nothing of for ever: a key aterm skipped (its
//!   guard matched no row, the screen changed, a person's gesture overtook
//!   it) counts as an attempt ([`Watch::skipped`]), and a face whose
//!   [`MAX_REPEATS`]th key in a row was skipped, or whose first skipped key
//!   is [`KEY_SETTLE_MS`] old with none written since, is the person's.
//!
//! Nothing here performs I/O; the supervisor's driver
//! (`src/supervise_box_duty.rs`) reads, presses and journals.

use std::cmp::Ordering;
use std::time::Duration;

use alab_policy::guard::{KeyAct, row_guard};
use alab_policy::vocab::Phase;
use alab_policy::words::destructive_word;
use aterm_phase::prompt::{Select, kept_answer, settles_later, title_row};
use aterm_phase::{
    Opt, Program, PromptKind, PromptV2, QuestionDialog, QuestionFocus, QuestionForm,
    QuestionOption, QuestionText, Reading, Recommended, Role,
};

use crate::aterm::Scope;
use crate::aterm_wire::{Fields, percent_decode};

/// aterm's rule id for the one-shot allow.
pub const RULE_ALLOW_ONCE: &str = "allow-once@v1";
/// aterm's rule id for a plan box's narrow yes.
pub const RULE_PLAN: &str = "plan-approve@v2";
/// aterm's rule id for `Not now` on the auto-mode setup offer.
pub const RULE_AUTO_MODE_SETUP: &str = "auto-mode-setup@v1";
/// aterm's rule id for the Claude in Chrome intro's confirm.
pub const RULE_CHROME_INTRO: &str = "chrome-intro@v1";
/// aterm's rule id for `Resume` on the conversation-resume dialog.
pub const RULE_CONVERSATION_RESUME: &str = "conversation-resume@v1";
/// aterm's rule id for a question answered with its recommended option.
pub const RULE_ANSWER_RECOMMENDED: &str = "answer-recommended@v1";
/// aterm's rule id for `Wait here, then continue automatically` on the
/// usage-limit dialog.
pub const RULE_LIMIT_WAIT: &str = "limit-wait@v1";
/// aterm's rule id for the autocompact offer's accept row.
pub const RULE_AUTOCOMPACT_OFFER: &str = "autocompact-offer@v1";
/// aterm's rule id for a model switch's cost confirmation.
pub const RULE_MODEL_CONFIRM: &str = "model-confirm@v1";

/// The most rows the focus is moved to reach an answer (aterm's
/// `question::MAX_QUESTION_FOCUS_STEPS`).
pub const MAX_FOCUS_STEPS: u32 = 6;

/// The most focus keys pressed on one face of a box before it is left to the
/// person: twice the farthest answer, so a focus that does not move is not
/// pressed at for good.
pub const MAX_FOCUS_KEYS: u32 = 2 * MAX_FOCUS_STEPS;

/// The most times in a row the same face of a box is answered; the next time
/// it shows it is the person's.
pub const MAX_REPEATS: u32 = 3;

/// The grace a key on an untouched permission box waits out after a person's
/// last gesture (aterm's `approval_loop::PERSON_SETTLE`).
pub const PERSON_SETTLE: Duration = Duration::from_millis(2_500);

/// The longest grace a key on a choice dialog waits out after a person's last
/// gesture (aterm's `approval_loop::QUESTION_PERSON_GRACE`).
pub const QUESTION_PERSON_GRACE: Duration = Duration::from_secs(15);

/// The policy's `questions=` words that answer a question; `ask` and any
/// other word leave it to the person.
const ANSWERING: [&str; 3] = ["recommended", "long-term", "short-term"];

/// What the policy lets this harness approve on a permission box: `[harness]
/// approve`, as the `harness policy` reply's `approve=` states it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Approve {
    /// Any box's one-shot allow.
    All,
    /// Only what aterm's safe rules prove, which are not ported here.
    Safe,
    /// Nothing: every box is the owner's.
    None,
}

/// The policy's switches for boxes and questions, from the `harness policy`
/// reply ([`BoxPolicy::of_policy`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BoxPolicy {
    /// `approve=`.
    pub approve: Approve,
    /// Whether a question dialog is answered: the session's `questions=` word
    /// is one of [`ANSWERING`], or, in a reply without it,
    /// `answer_questions=true`.
    pub answer_questions: bool,
    /// The wall dialogs' switches.
    pub walls: WallDialogs,
}

impl Default for BoxPolicy {
    /// What a reply that states nothing allows: nothing.
    fn default() -> Self {
        Self {
            approve: Approve::None,
            answer_questions: false,
            walls: WallDialogs::default(),
        }
    }
}

/// What the walls duty answers on a wall's dialog: two switches of the
/// `harness policy` reply, and what the harness knows of the limit.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct WallDialogs {
    /// `limit_wait=true`: the usage-limit dialog is answered with its wait
    /// row.
    pub limit_wait: bool,
    /// `autocompact_offer=<k>k`: the autocompact offer is accepted at this
    /// size, in kilo-tokens, one of [`aterm_phase::autocompact::LADDER`];
    /// `None` for `off`, a size the offer has not, or a reply without it.
    pub autocompact_k: Option<u32>,
    /// The harness moves the session off the limit the usage-limit dialog
    /// stands for (Fable's, [`crate::limit::owes_move`]): set by the
    /// supervisor at each read, never by the reply.
    pub moving: bool,
}

/// `autocompact_offer=`'s size, as aterm's reply spells it (`400k`, or
/// `off`): one the offer steps through, or none.
fn autocompact_k(word: &str) -> Option<u32> {
    word.strip_suffix('k')
        .and_then(|k| k.parse::<u32>().ok())
        .filter(|k| aterm_phase::autocompact::LADDER.contains(k))
}

impl BoxPolicy {
    /// The switches the policy reply `reply` states
    /// (`aterm/crates/aterm-gui/src/harness_policy_verb.rs`, `reply_serving`:
    /// `approve=`, `limit_wait=` and `autocompact_offer=` among
    /// `SupervisorConfig::texts`, then `questions=`). A reply that is not one,
    /// an `approve=` that is not `all`, `safe` or `none`, a question word not
    /// known here, a `limit_wait=` that is not `true` and an
    /// `autocompact_offer=` that names no size the offer has allow nothing: an
    /// older or other aterm never widens what this harness presses.
    #[must_use]
    pub fn of_policy(reply: &str) -> Self {
        let Ok(fields) = Fields::parse_lenient(reply) else {
            return Self::default();
        };
        let approve = match fields.get("approve") {
            Some("all") => Approve::All,
            Some("safe") => Approve::Safe,
            _ => Approve::None,
        };
        let answer_questions = match fields.get("questions") {
            Some(word) => percent_decode(word).is_ok_and(|word| ANSWERING.contains(&word.as_str())),
            None => fields.get("answer_questions") == Some("true"),
        };
        Self {
            approve,
            answer_questions,
            walls: WallDialogs {
                limit_wait: fields.get("limit_wait") == Some("true"),
                autocompact_k: fields.get("autocompact_offer").and_then(autocompact_k),
                moving: false,
            },
        }
    }
}

/// One key the duty presses on a box.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Step {
    /// The rule that chose the answer, as aterm's engine books it.
    pub rule: &'static str,
    /// The key: a digit, `down`, `up` or `enter`.
    pub key: String,
    /// The guard of a row of the read the key was decided from.
    pub guard: String,
    /// What the key means for the box (`act=`): `None` for a key that only
    /// moves the focus.
    pub act: Option<KeyAct>,
    /// What the answer is to: the box's command or title, and the option.
    pub subject: String,
}

/// What a duty does with a box it read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Act {
    /// Nothing: the box is the person's, for this reason.
    Leave(String),
    /// Press this key.
    Press(Step),
}

/// A box read whole.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BoxRead {
    /// The duty that answers it ([`scope_of`]).
    pub scope: Scope,
    /// Its kind, which sets the person's grace ([`grace`]).
    pub kind: PromptKind,
    /// What stays the same while it stands: its kind, title and command, or
    /// a question dialog's tabs (its question too where it has one tab). A
    /// person who keys on it keeps it until it goes.
    pub identity: String,
    /// What one answer answers: the identity and the question its tab asks.
    pub face: String,
    /// Where its focus is: what a key that took changes on a face that stays.
    pub state: String,
    /// What the duty does with it.
    pub act: Act,
}

/// What one read of the screen holds for these duties.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Ruling {
    /// No box (or not Claude Code's): why.
    NoBox(&'static str),
    /// A box the reader does not vouch for: nothing now, and nothing forgotten.
    Uncertain,
    /// A box read whole.
    Box(BoxRead),
}

/// The duty a box of `kind` is answered by (aterm's `duty::prompt_scope`):
/// a question is `questions`'; a usage limit, a model switch and an
/// autocompact offer are `walls`'; every other box, a dialog of no kind a duty
/// answers included (which is then left to the person), is `approvals`'.
#[must_use]
pub const fn scope_of(kind: PromptKind) -> Scope {
    match kind {
        PromptKind::Question => Scope::Questions,
        PromptKind::UsageLimit | PromptKind::ModelSwitch | PromptKind::AutocompactOffer => {
            Scope::Walls
        }
        _ => Scope::Approvals,
    }
}

/// The grace a key on a box of `kind` waits out after a person's last gesture,
/// under the policy's `human_grace` (aterm's `approval_loop`): a choice
/// dialog's is at most [`QUESTION_PERSON_GRACE`], a permission box's at most
/// [`PERSON_SETTLE`] (a person who keyed on the box since it showed has it,
/// [`Watch`]); a grant (a usage limit, a held message, a dialog of no kind)
/// waits the whole grace.
#[must_use]
pub fn grace(kind: PromptKind, policy: Duration) -> Duration {
    match kind {
        PromptKind::Question
        | PromptKind::PlanEnter
        | PromptKind::PlanExit
        | PromptKind::GoalProposal
        | PromptKind::ModelSwitch
        | PromptKind::AutocompactOffer
        | PromptKind::AutoModeSetup
        | PromptKind::ConversationResume
        | PromptKind::ChromeIntro => policy.min(QUESTION_PERSON_GRACE),
        PromptKind::UsageLimit | PromptKind::HeldMessage | PromptKind::Other => policy,
        _ => policy.min(PERSON_SETTLE),
    }
}

/// Read the box on `rows`, as `reading` (Claude Code's reader's) found it,
/// under `policy`.
#[must_use]
pub fn read(reading: &Reading, rows: &[String], policy: &BoxPolicy) -> Ruling {
    if !matches!(reading.program, Program::Claude) {
        return Ruling::NoBox("not-claude");
    }
    let Some(p) = reading.prompt.as_ref() else {
        return Ruling::NoBox("no-box");
    };
    if reading.phase != Phase::Prompt || !reading.phase_authoritative {
        return Ruling::Uncertain;
    }
    let identity = identity(p);
    let dialog = p
        .question_dialog
        .as_ref()
        .filter(|_| p.kind == PromptKind::Question);
    let asked = dialog.map(|dialog| dialog.question().map_or("review", |q| q.text.as_str()));
    let state = match dialog {
        Some(dialog) => format!("{:?}", dialog.focus()),
        None => {
            let focus = p.options.iter().position(|o| o.focused);
            // The autocompact offer's size moves under ←/→ with the focus
            // where it was: a step that took is a state that moved.
            match p.autocompact_offer() {
                Some(offer) => format!("{focus:?}\u{1f}{}", offer.tokens),
                None => format!("{focus:?}"),
            }
        }
    };
    Ruling::Box(BoxRead {
        scope: scope_of(p.kind),
        kind: p.kind,
        face: format!("{identity}\u{1f}{}", asked.unwrap_or_default()),
        identity,
        state,
        act: decide(p, rows, policy),
    })
}

/// What stays the same while the box `p` stands ([`BoxRead::identity`]).
fn identity(p: &PromptV2) -> String {
    match p.question_dialog.as_ref() {
        Some(dialog) if p.kind == PromptKind::Question => {
            let tabs: Vec<&str> = dialog.tabs.iter().map(|tab| tab.header.as_str()).collect();
            let asked = if tabs.len() > 1 {
                ""
            } else {
                dialog.question().map_or("", |q| q.text.as_str())
            };
            format!(
                "{:?}\u{1f}{}\u{1f}{}\u{1f}{asked}",
                p.kind,
                p.title,
                tabs.join("\u{1e}")
            )
        }
        _ => format!(
            "{:?}\u{1f}{}\u{1f}{}\u{1f}{}",
            p.kind,
            p.title,
            p.command,
            p.path.as_deref().unwrap_or_default()
        ),
    }
}

fn leave(why: impl Into<String>) -> Act {
    Act::Leave(why.into())
}

/// The act on the box `p`, read whole and vouched for, on `rows`: aterm's
/// `decide_unfenced` order, its rules narrowed as the module header says.
fn decide(p: &PromptV2, rows: &[String], policy: &BoxPolicy) -> Act {
    match p.kind {
        PromptKind::ConversationResume if policy.approve == Approve::None => {
            return leave("approve = \"none\": the conversation resume dialog is the owner's");
        }
        PromptKind::ConversationResume => return conversation_resume(p, rows),
        PromptKind::UsageLimit => return limit_wait(p, rows, &policy.walls),
        PromptKind::AutocompactOffer => {
            return autocompact_offer(p, rows, policy.walls.autocompact_k);
        }
        PromptKind::AutoModeSetup => return auto_mode_setup(p, rows),
        PromptKind::ChromeIntro if policy.approve == Approve::None => {
            return leave(
                "approve = \"none\": the Claude in Chrome intro is the owner's to confirm",
            );
        }
        PromptKind::ChromeIntro => return chrome_intro(p, rows),
        _ => {}
    }
    if aterm_phase::login_screen(rows).is_some() {
        return leave("Claude Code's sign-in menu: its method is the person's to choose");
    }
    if p.kind == PromptKind::Question {
        if !policy.answer_questions {
            return leave(
                "the policy's questions word is ask, or one not known here: a question is the \
                 person's to answer",
            );
        }
        return question(p, rows);
    }
    if p.kind == PromptKind::McpServer {
        return leave(
            "Claude Code's MCP server dialog saves every answer, Esc too, for good: no rule's to \
             answer",
        );
    }
    if let Some(review) = p.owner_review() {
        return leave(format!(
            "the owner's Claude Code settings send this box to a person: {}",
            review.text
        ));
    }
    if matches!(
        p.kind,
        PromptKind::Edit | PromptKind::Write | PromptKind::Overwrite | PromptKind::Read
    ) {
        return leave(format!(
            "a file box ({:?}), on which Claude Code draws no ask reason, and the owner's ask \
             rules and hooks are not read here: the person's",
            p.kind
        ));
    }
    if p.head_off_screen {
        return leave(
            "the box's title row is not on the screen: nothing is pressed on a box not read whole",
        );
    }
    if p.rm_breaker().is_some() {
        return leave("Claude Code's rm safety warning stands on this box: the person's");
    }
    match policy.approve {
        Approve::None => leave("approve = \"none\": every box is the owner's"),
        Approve::Safe => leave(
            "approve = \"safe\": aterm's safe rules are not held here, so the box is the owner's",
        ),
        Approve::All => full_power(p, rows),
    }
}

/// `approve = "all"`'s answer to a permission box: aterm's `full_power`,
/// without its declines, its waits and its purchases.
fn full_power(p: &PromptV2, rows: &[String]) -> Act {
    let pick = match p.kind {
        PromptKind::PlanEnter | PromptKind::PlanExit => plan_pick(p),
        PromptKind::Trust => Err(
            "the folder-trust dialog: whether the folder is trusted is the cold launch's to \
             settle, or the person's, never a key's"
                .to_string(),
        ),
        PromptKind::HeldMessage | PromptKind::Other => Err(format!(
            "a dialog of no kind a duty answers ({})",
            options_words(p)
        )),
        PromptKind::ReadOutsideSetting => p
            .with_role(Role::Once)
            .map(|o| (RULE_ALLOW_ONCE, o))
            .ok_or_else(|| "the read-outside setting with no one-shot allow".to_string()),
        PromptKind::ModelSwitch => model_confirm(p, rows),
        _ => allow_once(p),
    };
    let (rule, o) = match pick {
        Ok(pick) => pick,
        Err(why) => return Act::Leave(why),
    };
    if kept_for_good(o) {
        return leave(format!(
            "`{}` keeps its answer for every later ask: no press chooses a standing answer",
            o.label
        ));
    }
    if buys(o) {
        return leave(format!(
            "`{}` buys something: no press chooses a purchase",
            o.label
        ));
    }
    // A plan's narrow yes is a mode switch out of planning that widens
    // nothing ([`plan_pick`]); every other standing grant widens the launch.
    if o.role.standing() && rule != RULE_PLAN {
        return leave(format!(
            "`{}` grants more than this one call: no press widens the launch's permission mode",
            o.label
        ));
    }
    let what = if p.command.is_empty() {
        p.title.as_str()
    } else {
        p.command.as_str()
    };
    let subject = format!("{what} => {}", o.label);
    let Some(judged) = rows.get(judged_row(p, rows)) else {
        return leave("the box's row is not on the rows read");
    };
    option_key(p, rows, o, rule, row_guard(judged), subject)
}

/// The labels and roles of `p`'s options, for a reason.
fn options_words(p: &PromptV2) -> String {
    p.options
        .iter()
        .map(|o| format!("{}:{}", o.role.name(), o.label))
        .collect::<Vec<_>>()
        .join(", ")
}

/// aterm's `allow_once` without its declines and waits: the box's one
/// one-shot allow, or, where some option is neither another kind nor a
/// standing grant, the first affirming option that is not one.
fn allow_once(p: &PromptV2) -> Result<(&'static str, &Opt), String> {
    let once: Vec<&Opt> = p.options.iter().filter(|o| o.role == Role::Once).collect();
    if let [o] = once.as_slice()
        && !buys(o)
    {
        return Ok((RULE_ALLOW_ONCE, *o));
    }
    let sound = p
        .options
        .iter()
        .any(|o| o.role != Role::Other && !o.role.standing());
    if sound
        && let Some(o) = p
            .options
            .iter()
            .find(|o| o.affirms() && !o.role.standing() && !buys(o))
    {
        return Ok((RULE_ALLOW_ONCE, o));
    }
    Err(format!(
        "no one-shot allow among the options ({}): declining or granting more is the person's",
        options_words(p)
    ))
}

/// The plan box words that grant a standing mode (aterm's `BROAD_PLAN`).
const BROAD_PLAN: &[&str] = &[
    "auto mode",
    "accept edits",
    "auto-accept",
    "bypass permissions",
    "clear context",
];

/// aterm's `plan_pick` without its decline: the yes that grants no standing
/// mode, a one-shot one first.
fn plan_pick(p: &PromptV2) -> Result<(&'static str, &Opt), String> {
    let yes = |o: &&Opt| o.affirms() && !buys(o);
    let broad = |o: &Opt| {
        let label = o.label.to_lowercase();
        BROAD_PLAN.iter().any(|word| label.contains(*word))
    };
    p.options
        .iter()
        .filter(yes)
        .find(|o| o.role == Role::Once && !broad(o))
        .or_else(|| p.options.iter().filter(yes).find(|o| !broad(o)))
        .map(|o| (RULE_PLAN, o))
        .ok_or_else(|| {
            format!(
                "a plan approval whose every yes grants a standing mode ({}): the person's",
                options_words(p)
            )
        })
}

/// What a purchase's label opens with (aterm's `PURCHASE`).
const PURCHASE: &[&str] = &[
    "re-enable",
    "turn on usage credits",
    "switch to usage credits",
    "usage credits",
    "buy",
    "purchase",
    "pay",
    "add funds",
    "upgrade",
    "subscribe",
    "adjust monthly limit",
    "adjust monthly spend limit",
    "set limit",
    "set to unlimited",
    "set monthly spend limit",
];

/// What a label that spends a limit reset opens with (aterm's `RESET_SPEND`).
const RESET_SPEND: &[&str] = &[
    "use my reset",
    "use your reset",
    "reset your session limit",
    "reset your 5-hour limit",
];

/// Whether `label`, past a leading `yes,` or `yes `, opens with one of `words`
/// as a whole word (aterm's `opens_with`).
fn opens_with(label: &str, words: &[&str]) -> bool {
    let lower = label.trim().to_lowercase();
    let rest = lower
        .strip_prefix("yes,")
        .or_else(|| lower.strip_prefix("yes "))
        .map_or(lower.as_str(), str::trim_start);
    words.iter().any(|word| {
        rest.strip_prefix(*word)
            .is_some_and(|after| !after.starts_with(|c: char| c.is_alphanumeric()))
    })
}

/// Whether choosing `opt` would buy something or spend a limit reset (aterm's
/// `buys`).
fn buys(opt: &Opt) -> bool {
    aterm_phase::spends_reset(&opt.label)
        || opens_with(&opt.label, RESET_SPEND)
        || aterm_phase::uses_a_reset(&opt.label)
        || opens_with(&opt.label, PURCHASE)
}

/// Whether `o` keeps its answer for every later ask (aterm's
/// `kept_for_good`).
fn kept_for_good(o: &Opt) -> bool {
    o.role == Role::Persist || o.kept || settles_later(&o.label) || kept_answer(&o.label)
}

/// Why no digit is pressed on a box whose `No, and …` input is open.
const OPEN_INPUT: &str =
    "the focus is on an open `No, and …` input: a digit would be typed into it";

/// Whether the focus is on the box's open `No, and …` input (aterm's
/// `amend_input_open`).
fn amend_input_open(p: &PromptV2) -> bool {
    let footer_alone = p.footer.as_deref().is_some_and(|footer| {
        aterm_phase::footer_keys(footer) == [aterm_phase::anchor!("prompt.cancel")]
    });
    footer_alone
        && p.options
            .iter()
            .any(|o| o.focused && o.role == Role::Deny && o.label.starts_with("No, "))
}

/// The row of `rows` inside `p`'s box that reads `text`, its gutter bar or
/// `$ ` aside (aterm's `row_of`).
fn row_of(rows: &[String], p: &PromptV2, text: &str) -> Option<usize> {
    let (top, bottom) = p.span;
    (top + 1..bottom.min(rows.len())).find(|&k| {
        let t = rows[k].trim();
        let t = t.strip_prefix('│').map_or(t, str::trim);
        t == text || t.strip_prefix("$ ").is_some_and(|rest| rest == text)
    })
}

/// `row` without the `│` bars around it, trimmed.
fn unbar(row: &str) -> Option<&str> {
    row.strip_prefix('│')
        .and_then(|t| t.strip_suffix('│'))
        .map(str::trim)
}

/// The row of `rows` inside `p`'s box that names the file `path`: the path
/// alone, or Claude Code's `Read(<path>)` (aterm's `file_path_row`).
fn file_path_row(rows: &[String], p: &PromptV2, path: &str) -> Option<usize> {
    row_of(rows, p, path).or_else(|| {
        row_of(
            rows,
            p,
            &format!("{}{path})", aterm_phase::anchor!("box.read_call")),
        )
    })
}

/// The row a key on `p` is guarded by (aterm's `judged_row`): a shell box's
/// first command row (bare, after a `$` glyph, or inside a nested box's bars),
/// a network request's URL row, a file's path row (or, failing it, the
/// question row of a box that names a file), else the box's first row.
fn judged_row(p: &PromptV2, rows: &[String]) -> usize {
    let (top, bottom) = p.span;
    let within = || top + 1..bottom.min(rows.len());
    let own = match p.kind {
        PromptKind::Bash | PromptKind::PowerShell => p.command_rows.first().and_then(|c| {
            row_of(rows, p, c)
                .or_else(|| {
                    within().find(|&k| {
                        rows[k]
                            .trim()
                            .strip_suffix(c.as_str())
                            .is_some_and(|glyph| !glyph.is_empty() && glyph.trim_end() == "$")
                    })
                })
                .or_else(|| {
                    within().find(|&k| unbar(rows[k].trim()).and_then(unbar) == Some(c.as_str()))
                })
        }),
        PromptKind::NetworkRequest => row_of(
            rows,
            p,
            &format!("{}{}", aterm_phase::anchor!("muse.box.full_url"), p.command),
        ),
        _ => p
            .path
            .as_deref()
            .and_then(|path| file_path_row(rows, p, path))
            .or(p.question.filter(|&q| q < rows.len() && p.path.is_some())),
    };
    own.unwrap_or(top)
}

/// The one key that moves toward or presses the option `o` of `p`: its digit,
/// or an arrow toward it, or Enter once the focus is on it.
fn option_key(
    p: &PromptV2,
    rows: &[String],
    o: &Opt,
    rule: &'static str,
    guard: String,
    subject: String,
) -> Act {
    match p.select {
        Select::Digits if amend_input_open(p) => leave(OPEN_INPUT),
        Select::Digits => match o.n.filter(|n| (1..=9).contains(n)) {
            Some(n) => Act::Press(Step {
                rule,
                key: n.to_string(),
                guard,
                act: Some(KeyAct::Approve),
                subject,
            }),
            None => leave(format!("the option `{}` has no one-digit number", o.label)),
        },
        Select::ArrowsEnter => {
            let Some(at) = p.options.iter().position(|q| q.focused) else {
                return leave("an unnumbered box with no focused option");
            };
            let Some(to) = p.options.iter().position(|q| std::ptr::eq(q, o)) else {
                return leave("the option chosen is not among the box's");
            };
            let steps = i32::try_from(to).unwrap_or(0) - i32::try_from(at).unwrap_or(0);
            let focused = rows.get(p.options[at].row);
            focus_key(steps, focused, rule, guard, KeyAct::Approve, subject)
        }
    }
}

/// The one key `steps` rows from the focus: an arrow guarded by `guard`, or,
/// at the answer, Enter guarded by the focused row itself (`focused`), which
/// carries the focus mark, so it is pressed only while the focus is there.
fn focus_key(
    steps: i32,
    focused: Option<&String>,
    rule: &'static str,
    guard: String,
    act: KeyAct,
    subject: String,
) -> Act {
    if steps.unsigned_abs() > MAX_FOCUS_STEPS {
        return leave(format!(
            "the focus is {} rows from the answer (at most {MAX_FOCUS_STEPS} are moved)",
            steps.unsigned_abs()
        ));
    }
    let (key, guard, act) = match steps.cmp(&0) {
        Ordering::Greater => ("down", guard, None),
        Ordering::Less => ("up", guard, None),
        Ordering::Equal => match focused {
            Some(row) => ("enter", row_guard(row), Some(act)),
            None => return leave("the focused row is not on the rows read"),
        },
    };
    Act::Press(Step {
        rule,
        key: key.to_string(),
        guard,
        act,
        subject,
    })
}

/// `Not now` on Claude Code's auto-mode setup offer, at every level (aterm's
/// `auto_mode_setup`): it puts the offer off a week and changes nothing else.
fn auto_mode_setup(p: &PromptV2, rows: &[String]) -> Act {
    let Some(later) = p.auto_mode_later() else {
        return leave("an auto-mode setup offer not read whole");
    };
    let (Some(n), Select::Digits) = (later.n, p.select) else {
        return leave("an auto-mode setup offer whose `Not now` takes no digit");
    };
    let Some(title) = title_row(p, rows) else {
        return leave("the auto-mode setup offer's title is not on the screen");
    };
    if !(1..=9).contains(&n) {
        return leave("an auto-mode setup offer whose `Not now` takes no one digit");
    }
    Act::Press(Step {
        rule: RULE_AUTO_MODE_SETUP,
        key: n.to_string(),
        guard: row_guard(title),
        act: Some(KeyAct::Approve),
        subject: format!("{} => {}", p.title, later.label),
    })
}

/// `Resume` on Claude Code's conversation-resume dialog (aterm's
/// `conversation_resume`): the conversation the launch chose is kept.
fn conversation_resume(p: &PromptV2, rows: &[String]) -> Act {
    let [resume, new] = p.options.as_slice() else {
        return leave("a conversation resume dialog with changed options");
    };
    if p.head_off_screen
        || resume.label != aterm_phase::anchor!("conversation_resume.resume")
        || !new
            .label
            .starts_with(aterm_phase::anchor!("conversation_resume.new"))
    {
        return leave("a conversation resume dialog not read whole");
    }
    let Some(title) = title_row(p, rows) else {
        return leave("the conversation resume title is not on the screen");
    };
    let subject = format!("{} => {}", p.title, resume.label);
    option_key(
        p,
        rows,
        resume,
        RULE_CONVERSATION_RESUME,
        row_guard(title),
        subject,
    )
}

/// The Claude in Chrome intro's confirm (aterm's `chrome_intro`): it offers
/// nothing to choose, and the session's launch asked for what it introduces.
fn chrome_intro(p: &PromptV2, rows: &[String]) -> Act {
    let Some(confirm) = p.chrome_intro_confirm() else {
        return leave("a Claude in Chrome intro not read whole");
    };
    let Some(title) = rows.get(p.span.0) else {
        return leave("the Claude in Chrome intro's title is not on the screen");
    };
    Act::Press(Step {
        rule: RULE_CHROME_INTRO,
        key: "enter".to_string(),
        guard: row_guard(title),
        act: Some(KeyAct::Approve),
        subject: format!("{} => {}", p.title, confirm.label),
    })
}

/// The rows between `from` and `to` (two option indices of `p`), as a focus
/// move: positive down.
fn steps_between(from: usize, to: usize) -> i32 {
    i32::try_from(to).unwrap_or(0) - i32::try_from(from).unwrap_or(0)
}

/// `Wait here, then continue automatically` on Claude Code's usage-limit
/// dialog (aterm's `limit_wait`): the one row that waits, reached by the
/// focus and pressed with Enter. Never stop, an upgrade, usage credits or a
/// reset claim; never while the harness moves the session off the limit,
/// whose wait would keep it on the model the move leaves.
fn limit_wait(p: &PromptV2, rows: &[String], walls: &WallDialogs) -> Act {
    if walls.moving {
        return leave(
            "the usage-limit dialog of the limit the harness moves this session off: never `Wait \
             here` on a limit the harness moves off",
        );
    }
    if !walls.limit_wait {
        return leave("a usage-limit dialog (limit_wait is off)");
    }
    let title = aterm_phase::anchor!("limit.title");
    if p.title != title {
        return leave(format!(
            "a usage-limit dialog whose title is not `{title}`: {}",
            p.title
        ));
    }
    let waits: Vec<usize> = (0..p.options.len())
        .filter(|&k| p.options[k].role == Role::AutoResume)
        .collect();
    let [target] = waits.as_slice() else {
        return leave(format!(
            "a usage-limit dialog with {} `{} …` row ({}): only that row is chosen, never stop, \
             usage credits, an upgrade or a reset claim",
            if waits.is_empty() {
                "no"
            } else {
                "more than one"
            },
            aterm_phase::anchor!("limit.wait"),
            options_words(p)
        ));
    };
    let wait = &p.options[*target];
    if buys(wait) {
        return leave("a usage-limit dialog whose wait row reads as a purchase");
    }
    let Some(at) = p.options.iter().position(|o| o.focused) else {
        return leave("a usage-limit dialog with no focused option");
    };
    let Some(head) = title_row(p, rows) else {
        return leave("the usage-limit dialog's title is not on the screen");
    };
    focus_key(
        steps_between(at, *target),
        rows.get(p.options[at].row),
        RULE_LIMIT_WAIT,
        row_guard(head),
        KeyAct::Approve,
        format!("{title} => {}", wait.label),
    )
}

/// The autocompact offer's accept row at the policy's size (aterm's
/// `autocompact_offer`): the focus moved onto it, its size stepped toward
/// `target` (kilo-tokens) one `←`/`→` at a read, then Enter. A size the
/// offer does not reach leaves it to the person: a step that does not move
/// it is a state that did not move ([`Watch`]).
fn autocompact_offer(p: &PromptV2, rows: &[String], target: Option<u32>) -> Act {
    let Some(target) = target else {
        return leave("an autocompact offer (autocompact_offer is off)");
    };
    let Some(offer) = p.autocompact_offer() else {
        return leave("an autocompact offer not read whole");
    };
    let Some(head) = title_row(p, rows) else {
        return leave("the autocompact offer's title is not on the screen");
    };
    let (Some(accept), Some(keep)) = (p.options.get(offer.accept), p.options.get(offer.keep))
    else {
        return leave("an autocompact offer whose rows are not among its options");
    };
    let at = match offer.focused {
        aterm_phase::OfferFocus::Accept => offer.accept,
        aterm_phase::OfferFocus::Keep => offer.keep,
    };
    let focused = if at == offer.accept { accept } else { keep };
    let steps = steps_between(at, offer.accept);
    let target_tokens = target.saturating_mul(1_000);
    if steps != 0 || offer.tokens == target_tokens {
        return focus_key(
            steps,
            rows.get(focused.row),
            RULE_AUTOCOMPACT_OFFER,
            row_guard(head),
            KeyAct::Approve,
            format!("{} (autocompact at {target}k)", accept.label),
        );
    }
    let key = if offer.tokens < target_tokens {
        "right"
    } else {
        "left"
    };
    Act::Press(Step {
        rule: RULE_AUTOCOMPACT_OFFER,
        key: key.to_string(),
        guard: row_guard(head),
        act: None,
        subject: format!(
            "{}: one step {key}, from {} toward {target}k",
            accept.label,
            aterm_phase::autocompact::size_words(offer.tokens)
        ),
    })
}

/// A model switch's confirmation, answered yes under `approve = "all"`
/// (aterm's `model_confirm`): only Claude Code's own cost warning, never one
/// the person's `PreModelSwitch` hook asked for, and only its one yes.
fn model_confirm<'p>(p: &'p PromptV2, rows: &[String]) -> Result<(&'static str, &'p Opt), String> {
    let head_end = p
        .options
        .first()
        .map_or(p.span.1, |o| o.row)
        .min(rows.len());
    let subtitle = rows
        .get(p.span.0 + 1..head_end)
        .unwrap_or_default()
        .iter()
        .map(|r| r.trim())
        .take_while(|r| !r.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    let hook = aterm_phase::anchor!("model_switch.hook");
    if subtitle.contains(hook) {
        return Err(format!(
            "the person's own PreModelSwitch hook asked for this confirmation (`{hook}`)"
        ));
    }
    if subtitle != aterm_phase::anchor!("model_switch.cost") {
        return Err(format!(
            "a model-switch confirmation whose subtitle is not the cost warning: `{subtitle}`"
        ));
    }
    match p
        .options
        .iter()
        .filter(|o| o.role == Role::Once)
        .collect::<Vec<_>>()
        .as_slice()
    {
        [o] if !buys(o) => Ok((RULE_MODEL_CONFIRM, *o)),
        _ => Err(format!(
            "a model-switch confirmation without exactly one `{}` ({})",
            aterm_phase::anchor!("model_switch.yes"),
            options_words(p)
        )),
    }
}

/// The answer to a question dialog (aterm's `answer_question` under the
/// `recommended` priority, narrowed as the module header says).
fn question(p: &PromptV2, rows: &[String]) -> Act {
    let Some(dialog) = p.question_dialog.as_ref() else {
        return leave("a question not read as Claude Code's question dialog: the person's");
    };
    if let QuestionForm::Unknown { why } = &dialog.form {
        return leave(format!(
            "a question aterm-phase could not read whole: {why}"
        ));
    }
    let focus = dialog.focus();
    let Some(from) = focus_position(dialog, focus) else {
        return leave("a question with no visible focus (no ❯ row read)");
    };
    let Some(guard) = dialog
        .guard_row()
        .and_then(|k| rows.get(k))
        .map(|row| row_guard(row))
    else {
        return leave("a question whose question row is not on the rows read");
    };
    let (target, subject) = match choose(dialog) {
        Ok(chosen) => chosen,
        Err(why) => return Act::Leave(why),
    };
    let Some(to) = focus_position(dialog, target) else {
        return leave("a question whose answer is not in its focus order");
    };
    let focused = focus_row(dialog, focus).and_then(|k| rows.get(k));
    focus_key(
        to - from,
        focused,
        RULE_ANSWER_RECOMMENDED,
        guard,
        KeyAct::Answer,
        subject,
    )
}

/// Where `focus` stands in the dialog's focus order, from 0 (aterm's
/// `question::focus_position`).
fn focus_position(dialog: &QuestionDialog, focus: QuestionFocus) -> Option<i32> {
    let n = |options: &[QuestionOption]| i32::try_from(options.len()).ok();
    let option = |k: u8, options: &[QuestionOption]| {
        (1..=options.len())
            .contains(&usize::from(k))
            .then(|| i32::from(k) - 1)
    };
    match (&dialog.form, focus) {
        (QuestionForm::Single { options, .. }, QuestionFocus::Option(k))
        | (QuestionForm::Multi { options, .. }, QuestionFocus::Option(k))
        | (QuestionForm::Preview { options, .. }, QuestionFocus::Option(k)) => option(k, options),
        (QuestionForm::Single { options, .. }, QuestionFocus::FreeText)
        | (QuestionForm::Multi { options, .. }, QuestionFocus::FreeText)
        | (QuestionForm::Preview { options, .. }, QuestionFocus::Chat) => n(options),
        (QuestionForm::Single { options, .. }, QuestionFocus::Chat)
        | (QuestionForm::Multi { options, .. }, QuestionFocus::Button) => n(options).map(|n| n + 1),
        (QuestionForm::Multi { options, .. }, QuestionFocus::Chat) => n(options).map(|n| n + 2),
        (QuestionForm::Review { .. }, QuestionFocus::ReviewSubmit) => Some(0),
        (QuestionForm::Review { .. }, QuestionFocus::ReviewCancel) => Some(1),
        _ => None,
    }
}

/// The screen row the dialog draws `focus` on (aterm's `question::focus_row`).
fn focus_row(dialog: &QuestionDialog, focus: QuestionFocus) -> Option<usize> {
    let option =
        |options: &[QuestionOption], k: u8| options.iter().find(|o| o.n == k).map(|o| o.row);
    match (&dialog.form, focus) {
        (QuestionForm::Single { options, .. }, QuestionFocus::Option(k))
        | (QuestionForm::Multi { options, .. }, QuestionFocus::Option(k))
        | (QuestionForm::Preview { options, .. }, QuestionFocus::Option(k)) => option(options, k),
        (QuestionForm::Single { free_text, .. }, QuestionFocus::FreeText)
        | (QuestionForm::Multi { free_text, .. }, QuestionFocus::FreeText) => Some(free_text.row),
        (QuestionForm::Multi { button, .. }, QuestionFocus::Button) => Some(button.row),
        (QuestionForm::Single { chat, .. }, QuestionFocus::Chat)
        | (QuestionForm::Multi { chat, .. }, QuestionFocus::Chat)
        | (QuestionForm::Preview { chat, .. }, QuestionFocus::Chat) => Some(chat.row),
        (QuestionForm::Review { submit, .. }, QuestionFocus::ReviewSubmit) => Some(submit.row),
        (QuestionForm::Review { cancel, .. }, QuestionFocus::ReviewCancel) => Some(cancel.row),
        _ => None,
    }
}

/// Where the answer to `dialog`'s tab is, and what it answers.
fn choose(dialog: &QuestionDialog) -> Result<(QuestionFocus, String), String> {
    match &dialog.form {
        QuestionForm::Single {
            question,
            options,
            free_text,
            ..
        } => {
            if !free_text.pristine {
                return Err(
                    "a question a person began answering (text in its free-text row): theirs"
                        .to_string(),
                );
            }
            choose_one(question, options)
        }
        QuestionForm::Preview {
            question, options, ..
        } => {
            if let Some(o) = options.iter().find(|o| o.selected) {
                return Err(format!(
                    "a question a person has answered (option {} is ticked ✔): theirs",
                    o.n
                ));
            }
            choose_one(question, options)
        }
        QuestionForm::Multi { .. } => {
            Err("a question that takes several answers: the person's to choose".to_string())
        }
        QuestionForm::Review { unanswered, .. } => {
            if *unanswered || dialog.tabs.iter().any(|tab| !tab.answered) {
                return Err(
                    "the review of a dialog with a question unanswered: the person's".to_string(),
                );
            }
            let n = dialog.tabs.len();
            Ok((
                QuestionFocus::ReviewSubmit,
                format!("submit answers ({n} of {n} answered)"),
            ))
        }
        QuestionForm::Unknown { why } => Err(format!(
            "a question aterm-phase could not read whole: {why}"
        )),
    }
}

/// The option the agent marked `(Recommended)` on its own row, else on a
/// wrapped row; a question with none, one whose words name a destructive act,
/// or whose answer is kept for later asks is the person's.
fn choose_one(
    question: &QuestionText,
    options: &[QuestionOption],
) -> Result<(QuestionFocus, String), String> {
    let marked = options
        .iter()
        .find(|o| o.recommended == Recommended::Label)
        .or_else(|| {
            options
                .iter()
                .find(|o| matches!(o.recommended, Recommended::Wrapped { .. }))
        });
    let Some(o) = marked else {
        return Err("no option is marked (Recommended): a person answers that".to_string());
    };
    if let Some(word) = destructive(question, options) {
        return Err(format!(
            "a question whose words name an act that would {word}: a person answers that"
        ));
    }
    if settles_later(&format!("{}\n{}", o.label, o.description)) || kept_answer(&o.label) {
        return Err(format!(
            "a question whose answer `{}` is kept for every later ask: a person answers that",
            o.label
        ));
    }
    Ok((
        QuestionFocus::Option(o.n),
        format!("{} => {}", question.text, o.label),
    ))
}

/// The first destructive act the question or any of its options names
/// (aterm's `question::destructive`).
fn destructive(question: &QuestionText, options: &[QuestionOption]) -> Option<&'static str> {
    let mut text = question.text.to_lowercase();
    for o in options {
        text.push('\n');
        text.push_str(&o.label.to_lowercase());
        text.push('\n');
        text.push_str(&o.description.to_lowercase());
    }
    destructive_word(&text)
}

/// What the duties do at one read ([`Watch::look`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Next {
    /// Nothing at this read: why.
    Nothing(&'static str),
    /// Press `step`, a key of duty `scope` on a box of `kind`.
    Press {
        /// The duty.
        scope: Scope,
        /// The box's kind, which sets its grace ([`grace`]).
        kind: PromptKind,
        /// The key.
        step: Step,
    },
    /// The box is the person's, for `why`, said this once for this box.
    Left {
        /// The duty whose box it is.
        scope: Scope,
        /// Why.
        why: String,
    },
}

/// How long a face that still shows what a key was pressed on is waited for:
/// a focus that did not move, or a box that stands after its answer. Then it
/// is the person's.
pub const KEY_SETTLE_MS: u64 = 5_000;

/// The read a key was decided from: the screen's `seq`, the box's
/// [`BoxRead::state`], and whether the key answers the box.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Offered {
    seq: u64,
    state: String,
    answers: bool,
}

/// A key that reached the session, or may have, at `at_ms`.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Pressed {
    offered: Offered,
    at_ms: u64,
}

/// The box standing on the screen, as the duties have seen it.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Standing {
    identity: String,
    face: String,
    /// The person's count when the box showed.
    human_seq: u64,
    /// Why it is the person's for as long as it stands, once it is.
    theirs: Option<String>,
    /// The reason said last for it, so each is said once.
    said: Option<String>,
    /// The read the last key offered on this face was decided from.
    offered: Option<Offered>,
    /// The last key pressed on this face.
    pressed: Option<Pressed>,
    /// Focus keys pressed on this face.
    focus_keys: u32,
    /// Keys offered on this face in a row that aterm pressed nothing of
    /// ([`Watch::skipped`]), and when the first of them was.
    skipped: Option<Skipped>,
}

/// Keys offered on one face that aterm pressed nothing of, in a row.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Skipped {
    /// How many.
    keys: u32,
    /// When the first was answered, epoch ms.
    since_ms: u64,
}

/// The duties' memory of the boxes they have seen: rebuilt with each child.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Watch {
    standing: Option<Standing>,
    /// The face this harness answered last, and how many times in a row.
    answered: Option<(String, u32)>,
}

impl Watch {
    /// What to do at a read of the screen at `seq`, at `now_ms`, with the
    /// person's count at `human_seq`, that found `ruling`; `takes` says
    /// whether the claim takes a duty. A box of a duty not taken is left
    /// alone: nothing in the session answers it, so it is the person's.
    ///
    /// After a key, nothing more is pressed on the face it was pressed on
    /// until the screen moved and the face's focus with it; a face that still
    /// shows what the key was pressed on [`KEY_SETTLE_MS`] after it, or still
    /// stands after its answer, is the person's.
    pub fn look(
        &mut self,
        ruling: &Ruling,
        seq: u64,
        human_seq: u64,
        now_ms: u64,
        takes: impl Fn(Scope) -> bool,
    ) -> Next {
        let read = match ruling {
            Ruling::NoBox(why) => {
                self.standing = None;
                return Next::Nothing(why);
            }
            Ruling::Uncertain => return Next::Nothing("uncertain"),
            Ruling::Box(read) => read,
        };
        if !takes(read.scope) {
            self.standing = None;
            return Next::Nothing("unclaimed");
        }
        let fresh = self
            .standing
            .as_ref()
            .is_none_or(|standing| standing.identity != read.identity);
        if fresh {
            self.standing = Some(Standing {
                identity: read.identity.clone(),
                face: read.face.clone(),
                human_seq,
                theirs: None,
                said: None,
                offered: None,
                pressed: None,
                focus_keys: 0,
                skipped: None,
            });
        }
        let repeated = self
            .answered
            .as_ref()
            .is_some_and(|(face, n)| *face == read.face && *n >= MAX_REPEATS);
        let Some(standing) = self.standing.as_mut() else {
            return Next::Nothing("no-box");
        };
        if standing.face != read.face {
            standing.face.clone_from(&read.face);
            standing.offered = None;
            standing.pressed = None;
            standing.focus_keys = 0;
            standing.skipped = None;
        }
        if standing.theirs.is_none() && human_seq != standing.human_seq {
            standing.theirs = Some("a person keyed on it since it showed".to_string());
        }
        if standing.theirs.is_none()
            && let Some(last) = &standing.pressed
        {
            if last.offered.seq == seq {
                return Next::Nothing("awaiting-screen");
            }
            if last.offered.answers || last.offered.state == read.state {
                if now_ms.saturating_sub(last.at_ms) < KEY_SETTLE_MS {
                    return Next::Nothing("awaiting-key");
                }
                standing.theirs = Some(if last.offered.answers {
                    "it still stands after its answer".to_string()
                } else {
                    "its focus did not move after a key".to_string()
                });
            }
        }
        if standing.theirs.is_none() {
            standing.theirs = if fresh && repeated {
                Some(format!("it came back after {MAX_REPEATS} answers in a row"))
            } else if standing.focus_keys >= MAX_FOCUS_KEYS {
                Some("its focus did not reach the answer".to_string())
            } else if let Some(skipped) = standing.skipped
                && (skipped.keys >= MAX_REPEATS
                    || now_ms.saturating_sub(skipped.since_ms) >= KEY_SETTLE_MS)
            {
                Some(format!(
                    "aterm pressed none of the keys offered on it ({} in a row)",
                    skipped.keys
                ))
            } else {
                None
            };
        }
        let why = match (&standing.theirs, &read.act) {
            (Some(why), _) | (None, Act::Leave(why)) => why.clone(),
            (None, Act::Press(step)) => {
                standing.offered = Some(Offered {
                    seq,
                    state: read.state.clone(),
                    answers: step.act.is_some(),
                });
                return Next::Press {
                    scope: read.scope,
                    kind: read.kind,
                    step: step.clone(),
                };
            }
        };
        if standing.said.as_deref() == Some(why.as_str()) {
            return Next::Nothing("left");
        }
        standing.said = Some(why.clone());
        Next::Left {
            scope: read.scope,
            why,
        }
    }

    /// The key [`Self::look`] offered last reached the session at `now_ms`, or
    /// may have: nothing more is pressed on its face until the face's focus
    /// moved, and a key that answers the box counts toward [`MAX_REPEATS`]. A
    /// key not offered, or offered on a box that went, records nothing.
    pub fn pressed(&mut self, now_ms: u64) {
        let Some(standing) = self.standing.as_mut() else {
            return;
        };
        let Some(offered) = standing.offered.take() else {
            return;
        };
        standing.skipped = None;
        let answers = offered.answers;
        standing.pressed = Some(Pressed {
            offered,
            at_ms: now_ms,
        });
        if !answers {
            standing.focus_keys += 1;
            return;
        }
        self.answered = Some(match self.answered.take() {
            Some((face, n)) if face == standing.face => (face, n + 1),
            _ => (standing.face.clone(), 1),
        });
    }

    /// aterm pressed nothing of the key [`Self::look`] offered last, answered
    /// at `now_ms`: its guard matched no row, the screen changed, or a
    /// person's gesture overtook it. The key may be offered again at a later
    /// read, and counts as an attempt: the face is the person's once
    /// [`MAX_REPEATS`] keys in a row were skipped, or [`KEY_SETTLE_MS`] after
    /// the first of them with none written since. A key not offered, or
    /// offered on a box that went, records nothing.
    pub fn skipped(&mut self, now_ms: u64) {
        let Some(standing) = self.standing.as_mut() else {
            return;
        };
        if standing.offered.take().is_none() {
            return;
        }
        standing.skipped = Some(match standing.skipped {
            Some(skipped) => Skipped {
                keys: skipped.keys.saturating_add(1),
                ..skipped
            },
            None => Skipped {
                keys: 1,
                since_ms: now_ms,
            },
        });
    }
}

#[cfg(test)]
#[path = "boxes_tests.rs"]
mod tests;
