// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The approvals and questions duties, end to end: under a claim that takes
//! `approvals` or `questions` the harness reads its session through aterm and
//! presses one fenced key at a read on a box or a question dialog its policy
//! lets it answer, never on one a person keyed on, and leaves the rest to the
//! person, saying so once. aterm here is the fake ([`FakeAterm`]), whose
//! screen is aterm-phase's measured box or question until the key the program
//! takes draws the next ([`FakeAterm::after_key`]).

use super::*;

/// The box duties without `turn-end`: the keys are the launch's only input.
const BOX_DUTIES: &str = "--alab-duties=relaunch,upgrade,approvals,questions";

/// [`POLICY_ON`] with `approve = "all"`.
fn approving() -> String {
    format!("{POLICY_ON} approve=all")
}

/// A fake aterm for `s` whose roster carries this harness, whose policy body
/// is `policy` and whose session's Autopilot is Cruise.
fn cruising_aterm(s: &Scenario, policy: &str) -> FakeAterm {
    let fake = rostered_aterm(s);
    fake.set_policy_quietly(policy);
    fake.set_status("autopilot", Some("cruise"));
    fake
}

/// A fixture's rows, its `# ` provenance line left out (as aterm-phase's own
/// `fixtures::screen` reads one), with the cursor on the row that reads
/// `focused`.
pub(super) fn fixture(text: &str, focused: &str) -> (Vec<String>, (usize, usize)) {
    let body = match text.strip_prefix("# ") {
        Some(rest) => rest.split_once('\n').map_or("", |(_, body)| body),
        None => text,
    };
    let rows: Vec<String> = body.lines().map(str::to_string).collect();
    let at = rows
        .iter()
        .position(|row| row.trim() == focused)
        .expect("the focused row");
    (rows, (at, 1))
}

/// Claude Code 2.1.280's measured Bash box for `touch x`, its `1. Yes`
/// focused.
fn bash_box() -> (Vec<String>, (usize, usize)) {
    fixture(
        include_str!("../../vendor/aterm-phase/src/fixtures/claude-2.1.280-box-bash-touch.txt"),
        "❯ 1. Yes",
    )
}

/// Claude Code 2.1.282's measured question: three options, `(Recommended)` on
/// the second, the focus on the first.
fn question() -> (Vec<String>, (usize, usize)) {
    fixture(
        include_str!(
            "../../vendor/aterm-phase/src/fixtures/claude-2.1.282-question-recommended-second.txt"
        ),
        "❯ 1. Built-in runner",
    )
}

/// [`question`] as Claude Code draws it after one `down`: the focus on the
/// recommended option.
fn question_on_the_answer() -> (Vec<String>, (usize, usize)) {
    let rows: Vec<String> = question()
        .0
        .into_iter()
        .map(|row| match row.as_str() {
            "❯ 1. Built-in runner" => "  1. Built-in runner".to_string(),
            "  2. Nextest (Recommended)" => "❯ 2. Nextest (Recommended)".to_string(),
            _ => row,
        })
        .collect();
    let at = rows
        .iter()
        .position(|row| row == "❯ 2. Nextest (Recommended)")
        .expect("the answer's row");
    (rows, (at, 1))
}

/// Whether `s`'s journal has a line that starts with `head` after its stamp.
pub(super) fn journalled(s: &Scenario, head: &str) -> bool {
    s.journal().contains(&format!(" {head}"))
}

/// Under `approve = "all"` the one-shot allow of a Bash box is pressed once,
/// fenced on the read it was decided from and journalled with aterm's answer;
/// the screen the program draws after it gets nothing more. The claim names
/// the box duties it takes and no other.
///
/// CONTROL: the same box under a policy that approves nothing is left
/// ([`aterm_a_box_the_policy_keeps_is_left_and_said_once`]).
pub(super) fn aterm_a_box_is_approved_once_through_the_fence() {
    let s = Scenario::new("aterm-box-approved");
    let fake = cruising_aterm(&s, &approving());
    fake.set_screen(bash_box());
    fake.after_key("1", fake_aterm::composer_screen());
    let pty = Pty::open();
    let (_child, pid) = start_in_aterm_with(&s, &pty, &fake, 40, &[BOX_DUTIES]);
    await_record(&s, 0);
    assert!(
        wait_until(Duration::from_secs(15), || !fake.typed().is_empty()),
        "nothing pressed{}",
        s.report()
    );
    assert!(
        wait_until(Duration::from_secs(5), || journalled(
            &s,
            "duty approvals pressed rule=allow-once@v1 key=1 act=approve reply=written \
             subject=touch%20x%20=>%20Yes\n"
        )),
        "{}",
        s.report()
    );
    // Reads after it find the screen the key drew and press nothing more.
    fake.event("status");
    std::thread::sleep(Duration::from_millis(1_000));
    assert_eq!(fake.typed(), ["key act=approve 1"], "{}", s.report());
    let claim = format!(
        "meta set supervisor claude-code-alab@{pid} contract=3 \
         scope=relaunch,upgrade,approvals,questions version="
    );
    assert!(
        fake.verbs().iter().any(|verb| verb.starts_with(&claim)),
        "{:?}{}",
        fake.verbs(),
        s.report()
    );
    end_session(&s, pid);
}

/// A person's key between the read and the press is aterm's skip; the box is
/// then the person's for as long as it stands, said once, and no later read
/// presses on it, however long after the person's grace.
///
/// CONTROL: with nobody at the keys the same box is pressed
/// ([`aterm_a_box_is_approved_once_through_the_fence`]).
pub(super) fn aterm_a_person_who_keys_on_a_box_keeps_it() {
    let s = Scenario::new("aterm-box-person");
    let fake = cruising_aterm(&s, &approving());
    fake.set_screen(bash_box());
    fake.before_next_write(fake_aterm::NextWrite::PersonKeys);
    let pty = Pty::open();
    let (_child, pid) = start_in_aterm_with(&s, &pty, &fake, 40, &[BOX_DUTIES]);
    await_record(&s, 0);
    await_journal(
        &s,
        Duration::from_secs(15),
        "duty approvals unwritten rule=allow-once@v1 key=1 act=approve reply=person",
    );
    await_journal(
        &s,
        Duration::from_secs(5),
        "duty approvals left a person keyed on it since it showed",
    );
    // Past a permission box's 2.5 s grace, each read still presses nothing.
    for _ in 0..3 {
        fake.event("human");
        std::thread::sleep(Duration::from_millis(1_200));
    }
    assert!(
        fake.typed().is_empty(),
        "pressed on a person's box{}",
        s.report()
    );
    assert_eq!(
        s.journal().matches(" duty approvals left ").count(),
        1,
        "{}",
        s.report()
    );
    end_session(&s, pid);
}

/// A box the policy approves nothing of (an `approve=` the reply does not
/// state reads as `none`) is left to the person, said once however often the
/// session is read; under the session's `questions=ask` a question is too.
///
/// CONTROL: under `approve = "all"` the box is pressed
/// ([`aterm_a_box_is_approved_once_through_the_fence`]); under
/// `questions=recommended` the question is answered
/// ([`aterm_a_question_is_answered_with_its_recommended_option`]).
pub(super) fn aterm_a_box_the_policy_keeps_is_left_and_said_once() {
    let s = Scenario::new("aterm-box-policy");
    let fake = cruising_aterm(&s, POLICY_ON);
    fake.set_screen(bash_box());
    let pty = Pty::open();
    let (_child, pid) = start_in_aterm_with(&s, &pty, &fake, 40, &[BOX_DUTIES]);
    await_record(&s, 0);
    await_journal(
        &s,
        Duration::from_secs(15),
        "duty approvals left approve = \"none\": every box is the owner's",
    );
    for _ in 0..3 {
        fake.event("status");
        std::thread::sleep(Duration::from_millis(700));
    }
    assert_eq!(
        s.journal().matches(" duty approvals left ").count(),
        1,
        "{}",
        s.report()
    );
    fake.set_policy_quietly(&POLICY_ON.replace("questions=recommended", "questions=ask"));
    fake.set_screen(question());
    fake.event("status");
    await_journal(
        &s,
        Duration::from_secs(15),
        "duty questions left the policy's questions word is ask, or one not known here: a \
         question is the person's to answer",
    );
    std::thread::sleep(Duration::from_millis(1_000));
    assert!(fake.typed().is_empty(), "pressed{}", s.report());
    end_session(&s, pid);
}

/// Under `questions=recommended` a question is answered with the option the
/// agent marked `(Recommended)`: one arrow at a read, Enter once the screen
/// shows the focus on it, each fenced on its read; the second key says it
/// answers the question.
///
/// CONTROL: under `questions=ask` the same question is left
/// ([`aterm_a_box_the_policy_keeps_is_left_and_said_once`]).
pub(super) fn aterm_a_question_is_answered_with_its_recommended_option() {
    let s = Scenario::new("aterm-question-answered");
    let fake = cruising_aterm(&s, POLICY_ON);
    fake.set_screen(question());
    fake.after_key("down", question_on_the_answer());
    fake.after_key("enter", fake_aterm::composer_screen());
    let pty = Pty::open();
    let (_child, pid) = start_in_aterm_with(&s, &pty, &fake, 40, &[BOX_DUTIES]);
    await_record(&s, 0);
    assert!(
        wait_until(Duration::from_secs(15), || fake.typed().len() >= 2),
        "the question was not answered{}",
        s.report()
    );
    assert!(
        journalled(
            &s,
            "duty questions pressed rule=answer-recommended@v1 key=down reply=written subject="
        ) && journalled(
            &s,
            "duty questions pressed rule=answer-recommended@v1 key=enter act=answer \
             reply=written subject="
        ),
        "{}",
        s.report()
    );
    std::thread::sleep(Duration::from_millis(1_000));
    assert_eq!(
        fake.typed(),
        ["key down", "key act=answer enter"],
        "{}",
        s.report()
    );
    end_session(&s, pid);
}

/// Each box duty is its own word: a claim that takes `questions` and not
/// `approvals` presses nothing on a Bash box, which is then left to the
/// person, and journals nothing of it, while the question it takes is
/// answered.
pub(super) fn aterm_a_box_duty_not_claimed_is_left_to_the_person() {
    let s = Scenario::new("aterm-box-unclaimed");
    let fake = cruising_aterm(&s, &approving());
    fake.set_screen(bash_box());
    let pty = Pty::open();
    let (_child, pid) = start_in_aterm_with(
        &s,
        &pty,
        &fake,
        40,
        &["--alab-duties=relaunch,upgrade,questions"],
    );
    await_record(&s, 0);
    let claim = format!(
        "meta set supervisor claude-code-alab@{pid} contract=3 \
         scope=relaunch,upgrade,questions version="
    );
    assert!(
        wait_until(Duration::from_secs(15), || fake
            .verbs()
            .iter()
            .any(|verb| verb == "text --json")),
        "the session was not read for its questions{}",
        s.report()
    );
    assert!(
        fake.verbs().iter().any(|verb| verb.starts_with(&claim)),
        "{:?}{}",
        fake.verbs(),
        s.report()
    );
    fake.event("status");
    std::thread::sleep(Duration::from_millis(1_000));
    assert!(
        fake.typed().is_empty(),
        "pressed on a box of a duty not claimed{}",
        s.report()
    );
    assert!(!s.journal().contains(" duty approvals "), "{}", s.report());
    // CONTROL: the duty the claim takes answers its own.
    fake.after_key("down", question_on_the_answer());
    fake.after_key("enter", fake_aterm::composer_screen());
    fake.set_screen(question());
    fake.event("status");
    assert!(
        wait_until(Duration::from_secs(15), || fake.typed().len() >= 2),
        "the claimed question was not answered{}",
        s.report()
    );
    assert_eq!(
        fake.typed(),
        ["key down", "key act=answer enter"],
        "{}",
        s.report()
    );
    end_session(&s, pid);
}
