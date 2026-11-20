// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

/// Claude Code 2.1.280's Bash box for `touch x`: `1. Yes`, `2. Yes, and
/// always allow access to …`, `3. No…` (aterm-phase's measured fixture).
const BASH_TOUCH: &str =
    include_str!("../vendor/aterm-phase/src/fixtures/claude-2.1.280-box-bash-touch.txt");
/// Claude Code 2.1.282's plan approval: `Yes, and use auto mode`, `Yes,
/// manually approve edits`, `No, keep planning`.
const PLAN_READY: &str =
    include_str!("../vendor/aterm-phase/src/fixtures/claude-2.1.282-plan-ready.txt");
/// One question, `(Recommended)` on option 2 of 3, the focus on option 1.
const RECOMMENDED_SECOND: &str = include_str!(
    "../vendor/aterm-phase/src/fixtures/claude-2.1.282-question-recommended-second.txt"
);
/// A question with no option marked `(Recommended)`.
const NO_RECOMMENDATION: &str = include_str!(
    "../vendor/aterm-phase/src/fixtures/claude-2.1.282-question-no-recommendation.txt"
);
/// A question whose free-text row a person typed into.
const FREE_TEXT_TYPED: &str =
    include_str!("../vendor/aterm-phase/src/fixtures/claude-2.1.282-question-free-text-typed.txt");
/// A question that takes several answers.
const MULTISELECT: &str =
    include_str!("../vendor/aterm-phase/src/fixtures/claude-2.1.282-question-multiselect.txt");
/// The review tab of a dialog whose every question has an answer.
const REVIEW: &str =
    include_str!("../vendor/aterm-phase/src/fixtures/claude-2.1.282-question-review.txt");
/// The review tab of a dialog with a question unanswered.
const REVIEW_UNANSWERED: &str = include_str!(
    "../vendor/aterm-phase/src/fixtures/claude-2.1.282-question-review-unanswered.txt"
);

/// What `approve = "all"` with questions answered allows.
const ALL: BoxPolicy = BoxPolicy {
    approve: Approve::All,
    answer_questions: true,
    walls: WallDialogs {
        limit_wait: true,
        autocompact_k: Some(400),
        moving: false,
    },
};

const T: u64 = 1_000_000;

/// A fixture's screen rows, its `# ` provenance line left out (as aterm-phase's
/// own `fixtures::screen` reads one).
fn screen(text: &str) -> Vec<String> {
    let body = match text.strip_prefix("# ") {
        Some(rest) => rest.split_once('\n').map_or("", |(_, body)| body),
        None => text,
    };
    body.lines().map(str::to_string).collect()
}

/// Claude Code's reading of `rows` as its reader reports a box it vouches for:
/// aterm-phase's own parse of the box.
fn vouched(rows: &[String]) -> Reading {
    Reading {
        program: Program::Claude,
        phase: Phase::Prompt,
        phase_authoritative: true,
        wall: None,
        prompt: aterm_phase::parse_prompt_v2(rows),
        survey: false,
        context_left: None,
        suggestion: None,
        goal_active: false,
        said_tail: None,
        interrupted: false,
        fresh: false,
        composer: None,
        credits: None,
    }
}

/// The act on the box `reading` found on `rows`, under `policy`.
fn act(reading: &Reading, rows: &[String], policy: &BoxPolicy) -> Act {
    match read(reading, rows, policy) {
        Ruling::Box(found) => found.act,
        other => panic!("not a box read whole: {other:?}"),
    }
}

fn pressed(act: Act) -> Step {
    match act {
        Act::Press(step) => step,
        Act::Leave(why) => panic!("left: {why}"),
    }
}

fn left(act: Act) -> String {
    match act {
        Act::Leave(why) => why,
        Act::Press(step) => panic!("pressed: {step:?}"),
    }
}

fn prompt(reading: &mut Reading) -> &mut PromptV2 {
    reading.prompt.as_mut().expect("a box")
}

/// The single-answer form of `reading`'s question dialog.
fn single(reading: &mut Reading) -> (&mut QuestionText, &mut Vec<QuestionOption>) {
    let dialog = prompt(reading)
        .question_dialog
        .as_mut()
        .expect("a question dialog");
    match &mut dialog.form {
        QuestionForm::Single {
            question, options, ..
        } => (question, options),
        other => panic!("not a single-answer question: {other:?}"),
    }
}

/// `reading`'s question dialog with its focus on option `n`.
fn focus_option(reading: &mut Reading, n: u8) {
    for o in single(reading).1.iter_mut() {
        o.focused = o.n == n;
    }
}

#[test]
fn approve_all_presses_the_one_shot_allow_guarded_on_the_command_row() {
    let rows = screen(BASH_TOUCH);
    let r = vouched(&rows);
    assert_eq!(
        pressed(act(&r, &rows, &ALL)),
        Step {
            rule: RULE_ALLOW_ONCE,
            key: "1".to_string(),
            guard: r"^\x20{3}touch\x20x\s*$".to_string(),
            act: Some(KeyAct::Approve),
            subject: "touch x => Yes".to_string(),
        }
    );
    // Negative controls: below `all` no permission box is pressed.
    for approve in [Approve::Safe, Approve::None] {
        let policy = BoxPolicy { approve, ..ALL };
        assert!(
            left(act(&r, &rows, &policy)).starts_with(&format!(
                "approve = \"{}\"",
                if approve == Approve::Safe {
                    "safe"
                } else {
                    "none"
                }
            )),
            "{approve:?}"
        );
    }
}

#[test]
fn no_press_chooses_a_standing_answer_a_purchase_or_a_wider_grant() {
    let rows = screen(BASH_TOUCH);
    let r = vouched(&rows);
    // The allow keeps its answer for later asks.
    let mut kept = r.clone();
    prompt(&mut kept).options[0].kept = true;
    assert!(left(act(&kept, &rows, &ALL)).contains("keeps its answer"));
    // The only allows are grants for the session or for good.
    let mut session = r.clone();
    prompt(&mut session).options[0].role = Role::Session;
    assert!(left(act(&session, &rows, &ALL)).starts_with("no one-shot allow"));
    // The allow buys something.
    let mut buys = r.clone();
    prompt(&mut buys).options[0].label = "Yes, buy more usage".to_string();
    assert!(left(act(&buys, &rows, &ALL)).starts_with("no one-shot allow"));
    // Negative control: the box as drawn is pressed.
    assert_eq!(pressed(act(&r, &rows, &ALL)).key, "1");
}

#[test]
fn a_box_the_owner_or_claude_code_keeps_for_a_person_is_left() {
    let rows = screen(BASH_TOUCH);
    let r = vouched(&rows);
    // A file box, on which the owner's ask rules are not read here.
    let mut edit = r.clone();
    prompt(&mut edit).kind = PromptKind::Edit;
    assert!(left(act(&edit, &rows, &ALL)).starts_with("a file box (Edit)"));
    // A box whose title is off the screen.
    let mut cut = r.clone();
    prompt(&mut cut).head_off_screen = true;
    assert!(left(act(&cut, &rows, &ALL)).contains("title row is not on the screen"));
    // The folder-trust dialog, an MCP server's, and one of no kind.
    for kind in [PromptKind::Trust, PromptKind::McpServer, PromptKind::Other] {
        let mut other = r.clone();
        prompt(&mut other).kind = kind;
        assert!(
            matches!(act(&other, &rows, &ALL), Act::Leave(_)),
            "{kind:?}"
        );
    }
    // A wall's dialog is the walls duty's.
    let mut wall = r.clone();
    prompt(&mut wall).kind = PromptKind::UsageLimit;
    match read(&wall, &rows, &ALL) {
        Ruling::Box(found) => assert_eq!(found.scope, Scope::Walls),
        other => panic!("{other:?}"),
    }
    // Negative control: the Bash box itself is pressed.
    assert!(matches!(act(&r, &rows, &ALL), Act::Press(_)));
}

#[test]
fn only_a_box_claude_codes_reader_vouches_for_is_read() {
    let rows = screen(BASH_TOUCH);
    let r = vouched(&rows);
    match read(&r, &rows, &ALL) {
        Ruling::Box(found) => {
            assert_eq!(
                (found.scope, found.kind),
                (Scope::Approvals, PromptKind::Bash)
            );
            assert_eq!(found.state, "Some(0)");
        }
        other => panic!("{other:?}"),
    }
    let mut codex = r.clone();
    codex.program = Program::Codex;
    assert_eq!(read(&codex, &rows, &ALL), Ruling::NoBox("not-claude"));
    let mut none = r.clone();
    none.prompt = None;
    assert_eq!(read(&none, &rows, &ALL), Ruling::NoBox("no-box"));
    // A box the reader does not vouch for, or drawn in another phase: nothing
    // is decided.
    let mut unvouched = r.clone();
    unvouched.phase_authoritative = false;
    assert_eq!(read(&unvouched, &rows, &ALL), Ruling::Uncertain);
    let mut idle = r.clone();
    idle.phase = Phase::Idle;
    assert_eq!(read(&idle, &rows, &ALL), Ruling::Uncertain);
}

#[test]
fn a_plan_is_approved_by_the_yes_that_grants_no_standing_mode() {
    let rows = screen(PLAN_READY);
    let r = vouched(&rows);
    let step = pressed(act(&r, &rows, &ALL));
    assert_eq!(
        (step.rule, step.key.as_str(), step.act),
        (RULE_PLAN, "2", Some(KeyAct::Approve))
    );
    assert!(
        step.subject.ends_with("=> Yes, manually approve edits"),
        "{}",
        step.subject
    );
    // Negative control: a plan whose every yes grants a standing mode is left.
    let mut broad = r.clone();
    prompt(&mut broad).options[1].label = "Yes, and accept edits".to_string();
    assert!(left(act(&broad, &rows, &ALL)).contains("every yes grants a standing mode"));
}

#[test]
fn a_question_is_answered_with_its_recommended_option_one_key_at_a_read() {
    let rows = screen(RECOMMENDED_SECOND);
    let r = vouched(&rows);
    let question = r"^Which\x20test\x20runner\x20should\x20the\x20demo\x20project\x20use\?\s*$";
    // The focus on option 1: down, guarded by the question row.
    let down = pressed(act(&r, &rows, &ALL));
    assert_eq!(
        (down.rule, down.key.as_str(), down.guard.as_str(), down.act),
        (RULE_ANSWER_RECOMMENDED, "down", question, None)
    );
    assert!(down.subject.contains("Nextest"), "{}", down.subject);
    match read(&r, &rows, &ALL) {
        Ruling::Box(found) => {
            assert_eq!(found.scope, Scope::Questions);
            assert_eq!(found.state, "Option(1)");
        }
        other => panic!("{other:?}"),
    }
    // On the answer: Enter, guarded by the focused row itself.
    let mut on = r.clone();
    focus_option(&mut on, 2);
    let row = rows
        .iter()
        .find(|row| row.trim_start().starts_with("2. Nextest"))
        .expect("option 2's row");
    let enter = pressed(act(&on, &rows, &ALL));
    assert_eq!(
        (enter.key.as_str(), enter.guard, enter.act),
        ("enter", row_guard(row), Some(KeyAct::Answer))
    );
    // Past it: up.
    let mut past = r.clone();
    focus_option(&mut past, 3);
    assert_eq!(pressed(act(&past, &rows, &ALL)).key, "up");
    // Negative controls: the policy's `ask`, and a question whose words name
    // a destructive act, are the person's.
    let ask = BoxPolicy {
        answer_questions: false,
        ..ALL
    };
    assert!(left(act(&r, &rows, &ask)).contains("questions word is ask"));
    let mut deploy = r.clone();
    single(&mut deploy).0.text = "Which test runner should we deploy with?".to_string();
    assert!(left(act(&deploy, &rows, &ALL)).contains("deploy"));
    // A recommended answer that is kept for every later ask.
    let mut kept = r.clone();
    single(&mut kept).1[1].description = "Don't ask again for this project.".to_string();
    assert!(left(act(&kept, &rows, &ALL)).contains("kept for every later ask"));
}

#[test]
fn a_question_with_no_recommendation_one_begun_or_several_answers_is_the_persons() {
    for (name, text, why) in [
        (
            "no-recommendation",
            NO_RECOMMENDATION,
            "no option is marked",
        ),
        ("free-text-typed", FREE_TEXT_TYPED, "began answering"),
        ("multiselect", MULTISELECT, ""),
        ("review-unanswered", REVIEW_UNANSWERED, ""),
    ] {
        let rows = screen(text);
        let r = vouched(&rows);
        assert!(left(act(&r, &rows, &ALL)).contains(why), "{name}");
    }
    // Negative control: a review whose every question has an answer is
    // submitted, on its focused `Submit answers` row.
    let rows = screen(REVIEW);
    let r = vouched(&rows);
    let row = rows
        .iter()
        .find(|row| row.trim() == "❯ 1. Submit answers")
        .expect("the submit row");
    let submit = pressed(act(&r, &rows, &ALL));
    assert_eq!(
        (submit.key.as_str(), submit.guard, submit.act),
        ("enter", row_guard(row), Some(KeyAct::Answer))
    );
    assert!(submit.subject.starts_with("submit answers"));
}

#[test]
fn the_policy_reply_states_what_may_be_pressed_and_a_word_not_known_allows_nothing() {
    let of = BoxPolicy::of_policy;
    let is = |approve, answer_questions| BoxPolicy {
        approve,
        answer_questions,
        walls: WallDialogs::default(),
    };
    assert_eq!(
        of("OK gen=3 effective=on approve=all questions=recommended"),
        is(Approve::All, true)
    );
    assert_eq!(
        of("OK approve=safe questions=long-term"),
        is(Approve::Safe, true)
    );
    // The session's word wins over the policy's switch.
    assert_eq!(
        of("OK approve=none questions=short-term answer_questions=false"),
        is(Approve::None, true)
    );
    assert_eq!(
        of("OK approve=all questions=ask answer_questions=true"),
        is(Approve::All, false)
    );
    // A reply without the word: the switch.
    assert_eq!(
        of("OK approve=all answer_questions=true"),
        is(Approve::All, true)
    );
    assert_eq!(
        of("OK approve=all answer_questions=false"),
        is(Approve::All, false)
    );
    // Negative controls: nothing stated, words not known here, a malformed
    // word, and a reply that is not one allow nothing.
    assert_eq!(of("OK gen=3"), BoxPolicy::default());
    assert_eq!(of("OK approve=ALL questions=land"), BoxPolicy::default());
    assert_eq!(
        of("OK approve=all questions=recommended%"),
        is(Approve::All, false)
    );
    assert_eq!(
        of("ERR denied approve=all questions=recommended"),
        BoxPolicy::default()
    );
}

#[test]
fn each_box_is_one_duties_and_waits_out_its_kinds_grace() {
    assert_eq!(scope_of(PromptKind::Question), Scope::Questions);
    for kind in [
        PromptKind::UsageLimit,
        PromptKind::ModelSwitch,
        PromptKind::AutocompactOffer,
    ] {
        assert_eq!(scope_of(kind), Scope::Walls, "{kind:?}");
    }
    for kind in [
        PromptKind::Bash,
        PromptKind::Edit,
        PromptKind::PlanExit,
        PromptKind::Trust,
        PromptKind::HeldMessage,
        PromptKind::Other,
    ] {
        assert_eq!(scope_of(kind), Scope::Approvals, "{kind:?}");
    }
    let policy = Duration::from_secs(120);
    assert_eq!(grace(PromptKind::Bash, policy), PERSON_SETTLE);
    assert_eq!(grace(PromptKind::Question, policy), QUESTION_PERSON_GRACE);
    assert_eq!(grace(PromptKind::UsageLimit, policy), policy);
    // Negative control: a policy grace shorter than the kind's is the grace.
    let short = Duration::from_secs(1);
    assert_eq!(grace(PromptKind::Bash, short), short);
    assert_eq!(grace(PromptKind::Question, short), short);
}

/// A box of `face` read with its focus at `state`, whose act is `act`.
fn found(face: &str, state: &str, act: Act) -> Ruling {
    Ruling::Box(BoxRead {
        scope: Scope::Approvals,
        kind: PromptKind::Bash,
        identity: "Bash\u{1f}touch x".to_string(),
        face: face.to_string(),
        state: state.to_string(),
        act,
    })
}

fn key(key: &str, act: Option<KeyAct>) -> Act {
    Act::Press(Step {
        rule: RULE_ALLOW_ONCE,
        key: key.to_string(),
        guard: r"^x\s*$".to_string(),
        act,
        subject: "touch x => Yes".to_string(),
    })
}

fn yes() -> Ruling {
    found("f", "Some(0)", key("1", Some(KeyAct::Approve)))
}

const fn all(_: Scope) -> bool {
    true
}

fn is_press(next: &Next) -> bool {
    matches!(next, Next::Press { .. })
}

fn left_for(why: &str) -> Next {
    Next::Left {
        scope: Scope::Approvals,
        why: why.to_string(),
    }
}

#[test]
fn a_person_who_keys_on_a_box_has_it_until_it_goes() {
    let mut watch = Watch::default();
    assert!(is_press(&watch.look(&yes(), 10, 5, T, all)));
    // A read the reader does not vouch for keeps what was seen.
    assert_eq!(
        watch.look(&Ruling::Uncertain, 11, 5, T, all),
        Next::Nothing("uncertain")
    );
    // A gesture since the box showed: theirs, said once.
    assert_eq!(
        watch.look(&yes(), 12, 6, T, all),
        left_for("a person keyed on it since it showed")
    );
    assert_eq!(watch.look(&yes(), 13, 6, T, all), Next::Nothing("left"));
    // The box goes; the next one is judged from its own first sight.
    assert_eq!(
        watch.look(&Ruling::NoBox("no-box"), 14, 6, T, all),
        Next::Nothing("no-box")
    );
    assert!(is_press(&watch.look(&yes(), 15, 6, T, all)));
    // Negative control: no gesture, and a key aterm took nothing of is
    // offered again.
    let mut quiet = Watch::default();
    assert!(is_press(&quiet.look(&yes(), 10, 5, T, all)));
    assert!(is_press(&quiet.look(&yes(), 11, 5, T, all)));
}

#[test]
fn a_box_of_a_duty_not_claimed_is_aterms_wholly() {
    let mut watch = Watch::default();
    assert_eq!(
        watch.look(&yes(), 10, 5, T, |scope| scope != Scope::Approvals),
        Next::Nothing("unclaimed")
    );
    // Negative control: claimed, it is pressed.
    assert!(is_press(&watch.look(&yes(), 11, 5, T, all)));
}

#[test]
fn no_key_follows_one_the_screen_has_not_shown_taken() {
    let mut watch = Watch::default();
    assert!(is_press(&watch.look(&yes(), 10, 5, T, all)));
    watch.pressed(T);
    // The same screen: nothing.
    assert_eq!(
        watch.look(&yes(), 10, 5, T + 100, all),
        Next::Nothing("awaiting-screen")
    );
    // The screen moved and the box still stands: awaited, then the person's.
    assert_eq!(
        watch.look(&yes(), 11, 5, T + 1_000, all),
        Next::Nothing("awaiting-key")
    );
    assert_eq!(
        watch.look(&yes(), 12, 5, T + KEY_SETTLE_MS, all),
        left_for("it still stands after its answer")
    );

    // A focus key: the next follows only once the focus moved.
    let down = |state: &str| found("q", state, key("down", None));
    let mut focus = Watch::default();
    assert!(is_press(&focus.look(&down("Option(1)"), 10, 5, T, all)));
    focus.pressed(T);
    assert!(is_press(&focus.look(
        &down("Option(2)"),
        11,
        5,
        T + 500,
        all
    )));
    focus.pressed(T + 500);
    assert_eq!(
        focus.look(&down("Option(2)"), 12, 5, T + 1_000, all),
        Next::Nothing("awaiting-key")
    );
    assert_eq!(
        focus.look(&down("Option(2)"), 13, 5, T + 500 + KEY_SETTLE_MS, all),
        left_for("its focus did not move after a key")
    );
}

#[test]
fn a_box_that_keeps_coming_back_and_a_focus_that_never_arrives_are_the_persons() {
    let mut watch = Watch::default();
    for n in 0..u64::from(MAX_REPEATS) {
        let seq = 10 + 2 * n;
        assert!(is_press(&watch.look(&yes(), seq, 5, T, all)), "{n}");
        watch.pressed(T);
        assert_eq!(
            watch.look(&Ruling::NoBox("no-box"), seq + 1, 5, T, all),
            Next::Nothing("no-box")
        );
    }
    assert_eq!(
        watch.look(&yes(), 100, 5, T, all),
        left_for(&format!(
            "it came back after {MAX_REPEATS} answers in a row"
        ))
    );
    // Negative control: another box answered between starts the count again.
    let other = found("g", "Some(0)", key("1", Some(KeyAct::Approve)));
    let mut between = Watch::default();
    for (n, ruling) in [yes(), yes(), other, yes(), yes()].iter().enumerate() {
        let seq = 10 + 2 * u64::try_from(n).unwrap();
        assert!(is_press(&between.look(ruling, seq, 5, T, all)), "{n}");
        between.pressed(T);
        between.look(&Ruling::NoBox("no-box"), seq + 1, 5, T, all);
    }

    // Focus keys that move the focus but never reach the answer.
    let mut focus = Watch::default();
    for k in 0..MAX_FOCUS_KEYS {
        let seq = 10 + u64::from(k);
        let ruling = found("q", &k.to_string(), key("down", None));
        assert!(is_press(&focus.look(&ruling, seq, 5, T + seq, all)), "{k}");
        focus.pressed(T + seq);
    }
    assert_eq!(
        focus.look(&found("q", "last", key("down", None)), 100, 5, T + 100, all),
        left_for("its focus did not reach the answer")
    );
}

#[test]
fn a_key_aterm_keeps_pressing_nothing_of_is_the_persons_said_once() {
    let mut watch = Watch::default();
    for n in 0..u64::from(MAX_REPEATS) {
        let at = T + 500 * n;
        assert!(is_press(&watch.look(&yes(), 10 + n, 5, at, all)), "{n}");
        watch.skipped(at);
    }
    let why = format!("aterm pressed none of the keys offered on it ({MAX_REPEATS} in a row)");
    assert_eq!(watch.look(&yes(), 20, 5, T + 2_000, all), left_for(&why));
    assert_eq!(
        watch.look(&yes(), 21, 5, T + 2_500, all),
        Next::Nothing("left")
    );
    // One skipped key, and none written for KEY_SETTLE_MS after it.
    let mut slow = Watch::default();
    assert!(is_press(&slow.look(&yes(), 10, 5, T, all)));
    slow.skipped(T);
    assert!(is_press(&slow.look(&yes(), 11, 5, T + 1_000, all)));
    assert_eq!(
        slow.look(&yes(), 12, 5, T + KEY_SETTLE_MS, all),
        left_for("aterm pressed none of the keys offered on it (1 in a row)")
    );
    // Negative control: a key written between the skips starts the count
    // again, and the focus key after it is pressed.
    let down = |state: &str| found("q", state, key("down", None));
    let mut written = Watch::default();
    assert!(is_press(&written.look(&down("0"), 10, 5, T, all)));
    written.skipped(T);
    assert!(is_press(&written.look(&down("0"), 11, 5, T + 500, all)));
    written.skipped(T + 500);
    assert!(is_press(&written.look(&down("0"), 12, 5, T + 1_000, all)));
    written.pressed(T + 1_000);
    assert!(is_press(&written.look(&down("1"), 13, 5, T + 1_500, all)));
    written.skipped(T + 1_500);
    assert!(is_press(&written.look(&down("1"), 14, 5, T + 2_000, all)));
}

/// Claude Code 2.1.295's usage-limit dialog at a five-hour limit, the focus on
/// `Stop and wait for limit to reset`, the wait row second.
const LIMIT_MENU: &str =
    include_str!("../vendor/aterm-phase/src/fixtures/claude-2.1.295-usage-limit-menu.txt");
/// The autocompact offer, `Keep auto (1m tokens)` first and focused, the
/// accept row at 400k.
const AUTOCOMPACT_KEEP_FIRST: &str = include_str!(
    "../vendor/aterm-phase/src/fixtures/claude-2.1.302-autocompact-offer-keep-first.txt"
);
/// The autocompact offer, the accept row first and focused, at 400k.
const AUTOCOMPACT_LOWER_FIRST: &str = include_str!(
    "../vendor/aterm-phase/src/fixtures/claude-2.1.302-autocompact-offer-lower-first.txt"
);
/// Claude Code 2.1.283's model-switch confirmation with its cost warning.
const MODEL_SWITCH: &str =
    include_str!("../vendor/aterm-phase/src/fixtures/claude-2.1.283-model-switch.txt");
/// The same confirmation, asked for by the person's own `PreModelSwitch` hook.
const MODEL_SWITCH_HOOK: &str =
    include_str!("../vendor/aterm-phase/src/fixtures/claude-2.1.283-model-switch-hook.txt");

/// `rows` with every `from` replaced by `to`.
fn swapped(rows: &[String], from: &str, to: &str) -> Vec<String> {
    rows.iter().map(|row| row.replace(from, to)).collect()
}

/// The guard of the title row of the box `reading` found on `rows`.
fn title_guard(reading: &Reading, rows: &[String]) -> String {
    let p = reading.prompt.as_ref().expect("a box");
    row_guard(title_row(p, rows).expect("the title row"))
}

#[test]
fn the_policy_reply_states_the_wall_dialogs_switches_and_an_unknown_size_is_off() {
    let of = |reply: &str| BoxPolicy::of_policy(reply).walls;
    assert_eq!(
        of("OK approve=all limit_wait=true autocompact_offer=500k"),
        WallDialogs {
            limit_wait: true,
            autocompact_k: Some(500),
            moving: false,
        }
    );
    // Negative controls: off, a size the offer has not, a word other than
    // `true`, and a reply that names neither.
    assert_eq!(of("OK autocompact_offer=off").autocompact_k, None);
    assert_eq!(of("OK autocompact_offer=450k").autocompact_k, None);
    assert_eq!(of("OK autocompact_offer=400").autocompact_k, None);
    assert!(!of("OK limit_wait=yes").limit_wait);
    assert_eq!(of("OK approve=all"), WallDialogs::default());
}

#[test]
fn the_usage_limit_dialog_gets_its_wait_row_and_never_another() {
    let rows = screen(LIMIT_MENU);
    let r = vouched(&rows);
    assert_eq!(
        r.prompt.as_ref().map(|p| p.kind),
        Some(PromptKind::UsageLimit)
    );
    // The focus is on `Stop`: one row down, guarded by the title.
    let down = pressed(act(&r, &rows, &ALL));
    assert_eq!(
        (down.rule, down.key.as_str(), down.act, down.guard),
        (RULE_LIMIT_WAIT, "down", None, title_guard(&r, &rows))
    );
    // On the wait row: Enter, guarded by the focused row itself.
    let on_wait = swapped(
        &swapped(&rows, "  ❯ 1. Stop", "    1. Stop"),
        "    2. Wait here",
        "  ❯ 2. Wait here",
    );
    let r_wait = vouched(&on_wait);
    let enter = pressed(act(&r_wait, &on_wait, &ALL));
    let focused = on_wait
        .iter()
        .find(|row| row.contains("❯ 2. Wait here"))
        .expect("the focused wait row");
    assert_eq!(
        (enter.rule, enter.key.as_str(), enter.act, enter.guard),
        (
            RULE_LIMIT_WAIT,
            "enter",
            Some(KeyAct::Approve),
            row_guard(focused)
        )
    );
    assert!(
        enter
            .subject
            .contains("Wait here, then continue automatically")
    );
    // Never on a limit the harness moves the session off, and never while
    // limit_wait is off.
    let moving = BoxPolicy {
        walls: WallDialogs {
            moving: true,
            ..ALL.walls
        },
        ..ALL
    };
    assert!(left(act(&r_wait, &on_wait, &moving)).contains("never `Wait here`"));
    let off = BoxPolicy {
        walls: WallDialogs {
            limit_wait: false,
            ..ALL.walls
        },
        ..ALL
    };
    assert!(left(act(&r_wait, &on_wait, &off)).contains("limit_wait is off"));
    // A dialog with no wait row (its row read as something else) is the
    // person's: nothing else is ever chosen.
    let mut no_wait = r_wait.clone();
    for o in &mut prompt(&mut no_wait).options {
        if o.role == Role::AutoResume {
            o.role = Role::Other;
        }
    }
    assert!(left(act(&no_wait, &on_wait, &ALL)).contains("no `Wait here"));
}

#[test]
fn the_autocompact_offer_is_stepped_to_the_policys_size_and_accepted() {
    let at = |k: u32| BoxPolicy {
        walls: WallDialogs {
            autocompact_k: Some(k),
            ..ALL.walls
        },
        ..ALL
    };
    // The focus on `Keep`, the accept row under it: the focus moves onto it
    // first.
    let keep_first = screen(AUTOCOMPACT_KEEP_FIRST);
    let r = vouched(&keep_first);
    let toward = pressed(act(&r, &keep_first, &at(400)));
    assert_eq!(
        (toward.rule, toward.key.as_str(), toward.act),
        (RULE_AUTOCOMPACT_OFFER, "down", None)
    );
    // On the accept row at the size: Enter.
    let rows = screen(AUTOCOMPACT_LOWER_FIRST);
    let r = vouched(&rows);
    let enter = pressed(act(&r, &rows, &at(400)));
    assert_eq!(
        (enter.rule, enter.key.as_str(), enter.act),
        (RULE_AUTOCOMPACT_OFFER, "enter", Some(KeyAct::Approve))
    );
    // Another size: one step toward it, which moves no focus.
    for (k, key) in [(500, "right"), (300, "left")] {
        let step = pressed(act(&r, &rows, &at(k)));
        assert_eq!((step.key.as_str(), step.act), (key, None), "{k}");
        assert_eq!(step.guard, title_guard(&r, &rows), "{k}");
    }
    // A step that took is a state that moved.
    let stepped = swapped(&rows, "400k", "500k");
    let state = |rows: &[String]| match read(&vouched(rows), rows, &ALL) {
        Ruling::Box(found) => found.state,
        other => panic!("{other:?}"),
    };
    assert_ne!(state(&rows), state(&stepped));
    // Negative control: with the offer off, nothing is pressed, never `Keep`.
    let off = BoxPolicy {
        walls: WallDialogs {
            autocompact_k: None,
            ..ALL.walls
        },
        ..ALL
    };
    assert!(left(act(&r, &rows, &off)).contains("autocompact_offer is off"));
}

#[test]
fn a_model_switchs_cost_warning_is_confirmed_under_all_and_a_hooks_never() {
    let rows = screen(MODEL_SWITCH);
    let r = vouched(&rows);
    assert_eq!(
        r.prompt.as_ref().map(|p| p.kind),
        Some(PromptKind::ModelSwitch)
    );
    let yes = pressed(act(&r, &rows, &ALL));
    assert_eq!(
        (yes.rule, yes.act),
        (RULE_MODEL_CONFIRM, Some(KeyAct::Approve))
    );
    assert!(yes.subject.contains("Yes, switch to Sonnet 5"), "{yes:?}");
    match read(&r, &rows, &ALL) {
        Ruling::Box(found) => assert_eq!(found.scope, Scope::Walls),
        other => panic!("{other:?}"),
    }
    // Negative controls: the person's own hook asked, and a policy below
    // `all`.
    let hook = screen(MODEL_SWITCH_HOOK);
    assert!(left(act(&vouched(&hook), &hook, &ALL)).contains("PreModelSwitch hook"));
    let safe = BoxPolicy {
        approve: Approve::Safe,
        ..ALL
    };
    assert!(matches!(act(&r, &rows, &safe), Act::Leave(_)));
}
