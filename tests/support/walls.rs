// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The walls duty, end to end: under a claim that takes `walls` the harness
//! reads its session through aterm and answers the wall its turn ended on by
//! aterm's rules (`/compact` on a full context, `Wait here` on the
//! usage-limit dialog), once for a screen and through the claim's fence,
//! never `Wait here` on a limit it moves the session off, restarts the child
//! once at the critical-memory banner, and leaves a lost login to the person,
//! saying so once. aterm here is the fake
//! ([`FakeAterm`]), whose screen is aterm-phase's measured wall until the key
//! the program takes draws the next ([`FakeAterm::after_key`]).

use claude_code_alab::boxes::RULE_LIMIT_WAIT;
use claude_code_alab::walls::{COMPACT, RULE_COMPACT};

use super::boxes::{fixture, journalled};
use super::*;

/// The walls duty and the restarts, without `turn-end` or the box duties: the
/// walls duty's acts are the launch's only input.
const WALL_DUTIES: &str = "--alab-duties=relaunch,upgrade,walls";

/// [`POLICY_ON`] with every wall switch aterm's defaults turn on (usage
/// credits stay off): the fake's own body states none, and a switch the reply
/// does not state is off.
fn walls_on() -> String {
    format!(
        "{POLICY_ON} retry_api_errors=true resume_limits=true limit_wait=true \
         compact_on_context_wall=true autocompact_offer=400k"
    )
}

/// A fake aterm for `s` whose roster carries this harness, whose policy turns
/// every wall switch on and whose session's Autopilot is Cruise.
fn walled_aterm(s: &Scenario) -> FakeAterm {
    let fake = rostered_aterm(s);
    fake.set_policy_quietly(&walls_on());
    fake.set_status("autopilot", Some("cruise"));
    fake
}

/// A fixture's rows with the cursor after the empty composer's caret, as
/// Claude Code leaves it at a turn's end.
fn at_composer(text: &str) -> (Vec<String>, (usize, usize)) {
    let (rows, (at, _)) = fixture(text, "❯");
    (rows, (at, 2))
}

/// Claude Code 2.1.286's measured context wall: a turn that ended on
/// `Context limit reached · /compact or /clear to continue`.
fn context_wall() -> (Vec<String>, (usize, usize)) {
    at_composer(include_str!(
        "../../vendor/aterm-phase/src/fixtures/claude-2.1.286-wall-context-too-long-turn.txt"
    ))
}

/// Claude Code 2.1.281's login wall: `Login expired · Please run /login`.
fn login_wall() -> (Vec<String>, (usize, usize)) {
    at_composer(include_str!(
        "../../vendor/aterm-phase/src/fixtures/claude-2.1.281-login-expired.txt"
    ))
}

/// Claude Code's critical-memory banner between turns (aterm-phase's
/// fixture, which its reader reads as a memory wall at rest).
fn memory_banner() -> (Vec<String>, (usize, usize)) {
    at_composer(include_str!(
        "../../vendor/aterm-phase/src/fixtures/memory-banner-idle.txt"
    ))
}

/// Claude Code 2.1.295's measured usage-limit dialog at a five-hour limit,
/// its `Stop` row focused.
fn usage_dialog() -> (Vec<String>, (usize, usize)) {
    fixture(
        include_str!("../../vendor/aterm-phase/src/fixtures/claude-2.1.295-usage-limit-menu.txt"),
        "❯ 1. Stop and wait for limit to reset",
    )
}

/// [`usage_dialog`] as Claude Code draws it after one `down`: the focus on
/// `Wait here`.
fn usage_dialog_on_wait() -> (Vec<String>, (usize, usize)) {
    let rows: Vec<String> = usage_dialog()
        .0
        .into_iter()
        .map(|row| {
            row.replace("  ❯ 1. Stop", "    1. Stop")
                .replace("    2. Wait here", "  ❯ 2. Wait here")
        })
        .collect();
    let at = rows
        .iter()
        .position(|row| row.contains("❯ 2. Wait here"))
        .expect("the wait row");
    (rows, (at, 2))
}

/// Under the default claim a turn that ended on a full context gets
/// `/compact`, typed once through the fence under the walls duty and
/// journalled with aterm's answer; the unchanged screen read again gets
/// nothing more, and the claim names `walls`.
///
/// CONTROL: a claim that leaves `walls` out types nothing on the same wall
/// ([`aterm_a_claim_without_walls_types_nothing_on_them`]).
pub(super) fn aterm_a_full_context_is_compacted_once_through_the_fence() {
    let s = Scenario::new("aterm-wall-compact");
    let fake = walled_aterm(&s);
    fake.set_screen(context_wall());
    let pty = Pty::open();
    let (_child, pid) = start_in_aterm(&s, &pty, &fake, 40);
    await_record(&s, 0);
    let compact = format!("turn {COMPACT}");
    assert!(
        wait_until(Duration::from_secs(15), || fake.typed().contains(&compact)),
        "no /compact{}",
        s.report()
    );
    await_journal(
        &s,
        Duration::from_secs(5),
        &format!("duty walls typed rule={RULE_COMPACT} reply=submitted"),
    );
    // The screen that shows it, read again, gets nothing more: its own text
    // stands in the composer, and Enter on it waits out the take.
    assert!(
        wait_until(Duration::from_secs(15), || s
            .journal()
            .contains(" duty walls wait awaiting-take until=")
            || s.journal().contains(" duty walls nothing awaiting-take\n")),
        "the unchanged screen was not read again{}",
        s.report()
    );
    fake.event("status");
    std::thread::sleep(Duration::from_millis(1_000));
    assert_eq!(fake.typed(), [compact.as_str(), "enter"], "{}", s.report());
    assert!(
        fake.verbs().iter().any(|verb| verb.starts_with(&format!(
            "meta set supervisor claude-code-alab@{pid} contract=3 \
             scope=relaunch,upgrade,turn-end,approvals,questions,walls version="
        ))),
        "{:?}{}",
        fake.verbs(),
        s.report()
    );
    end_session(&s, pid);
}

/// A lost login is the person's: signing in opens their browser. It is left,
/// said once however often the session is read, and nothing is typed, not
/// `/login`.
///
/// CONTROL: a wall the harness answers is typed
/// ([`aterm_a_full_context_is_compacted_once_through_the_fence`]).
pub(super) fn aterm_a_lost_login_is_left_to_the_person_and_said_once() {
    let s = Scenario::new("aterm-wall-login");
    let fake = walled_aterm(&s);
    fake.set_screen(login_wall());
    let pty = Pty::open();
    let (_child, pid) = start_in_aterm(&s, &pty, &fake, 40);
    await_record(&s, 0);
    await_journal(
        &s,
        Duration::from_secs(15),
        "duty walls left a lost login: Login expired · Please run /login; signing in again \
         opens the person's browser, so it is theirs (/login)",
    );
    for _ in 0..3 {
        fake.event("status");
        std::thread::sleep(Duration::from_millis(700));
    }
    assert_eq!(
        s.journal().matches(" duty walls left a lost login").count(),
        1,
        "{}",
        s.report()
    );
    assert!(
        fake.typed().is_empty(),
        "typed at a login wall{}",
        s.report()
    );
    end_session(&s, pid);
}

/// Under `limit_wait` the usage-limit dialog gets `Wait here`: one arrow at a
/// read onto its row, then Enter once the screen shows the focus there, each
/// fenced on its read and pressed under the walls duty; the second key says it
/// approves. Nothing else on the dialog is ever chosen.
///
/// CONTROL: the same dialog under a Fable limit the harness moves the session
/// off gets no key ([`aterm_no_wait_here_on_a_limit_the_harness_moves_off`]).
pub(super) fn aterm_the_usage_dialog_gets_wait_here_through_the_fence() {
    let s = Scenario::new("aterm-wall-limit-wait");
    let fake = walled_aterm(&s);
    fake.set_screen(usage_dialog());
    fake.after_key("down", usage_dialog_on_wait());
    fake.after_key("enter", fake_aterm::composer_screen());
    let pty = Pty::open();
    let (_child, pid) = start_in_aterm_with(&s, &pty, &fake, 40, &[WALL_DUTIES]);
    await_record(&s, 0);
    assert!(
        wait_until(Duration::from_secs(15), || fake.typed().len() >= 2),
        "the dialog was not answered{}",
        s.report()
    );
    assert!(
        journalled(
            &s,
            &format!("duty walls pressed rule={RULE_LIMIT_WAIT} key=down reply=written subject=")
        ) && journalled(
            &s,
            &format!(
                "duty walls pressed rule={RULE_LIMIT_WAIT} key=enter act=approve reply=written \
                 subject="
            )
        ),
        "{}",
        s.report()
    );
    fake.event("status");
    std::thread::sleep(Duration::from_millis(1_000));
    assert_eq!(
        fake.typed(),
        ["key down", "key act=approve enter"],
        "{}",
        s.report()
    );
    end_session(&s, pid);
}

/// The usage-limit dialog of a Fable limit, on a launch the harness moves off
/// Fable's limit (no `--model`, so Claude Code's own choice), never gets
/// `Wait here`: waiting would keep the session on the model the move leaves.
/// It is left, said once, before the transcript's row for the limit is even
/// written.
///
/// CONTROL: the same dialog at a session limit gets `Wait here`
/// ([`aterm_the_usage_dialog_gets_wait_here_through_the_fence`]).
pub(super) fn aterm_no_wait_here_on_a_limit_the_harness_moves_off() {
    let s = Scenario::new("aterm-wall-limit-moving");
    let fake = walled_aterm(&s);
    let (rows, cursor) = usage_dialog();
    let fable: Vec<String> = rows
        .into_iter()
        .map(|row| {
            row.replace(
                "You've hit your session limit",
                "You've hit your Fable limit",
            )
        })
        .collect();
    assert!(
        fable.iter().any(|row| row.contains("Fable limit")),
        "PRECONDITION: the fixture's limit row"
    );
    fake.set_screen((fable, cursor));
    let pty = Pty::open();
    let (_child, pid) = start_in_aterm_with(&s, &pty, &fake, 40, &[WALL_DUTIES]);
    await_record(&s, 0);
    await_journal(
        &s,
        Duration::from_secs(15),
        "duty walls left the usage-limit dialog of the limit the harness moves this session off: \
         never `Wait here` on a limit the harness moves off",
    );
    for _ in 0..3 {
        fake.event("status");
        std::thread::sleep(Duration::from_millis(700));
    }
    assert!(
        fake.typed().is_empty(),
        "pressed on the dialog{}",
        s.report()
    );
    assert_eq!(
        s.journal().matches(" duty walls left ").count(),
        1,
        "{}",
        s.report()
    );
    end_session(&s, pid);
}

/// Walls are their own duty: a claim that leaves them out names no `walls`,
/// types nothing on a full context, which is then left to the person, and
/// journals no walls line, while the turn-end duty it takes reads the session
/// and leaves the wall alone.
pub(super) fn aterm_a_claim_without_walls_types_nothing_on_them() {
    let s = Scenario::new("aterm-wall-unclaimed");
    let fake = walled_aterm(&s);
    fake.set_screen(context_wall());
    let pty = Pty::open();
    let (_child, pid) = start_in_aterm_with(
        &s,
        &pty,
        &fake,
        40,
        &["--alab-duties=relaunch,upgrade,turn-end,approvals,questions"],
    );
    await_record(&s, 0);
    await_journal(&s, Duration::from_secs(15), "duty turn-end nothing wall");
    fake.event("status");
    std::thread::sleep(Duration::from_millis(1_000));
    assert!(
        fake.typed().is_empty(),
        "typed on a wall the claim does not take{}",
        s.report()
    );
    assert!(!s.journal().contains(" duty walls "), "{}", s.report());
    let claim = format!(
        "meta set supervisor claude-code-alab@{pid} contract=3 \
         scope=relaunch,upgrade,turn-end,approvals,questions version="
    );
    assert!(
        fake.verbs().iter().any(|verb| verb.starts_with(&claim)),
        "{:?}{}",
        fake.verbs(),
        s.report()
    );
    end_session(&s, pid);
}

/// The critical-memory banner restarts the child on its conversation: one
/// `SIGTERM` once aterm's fresh read finds nobody it would interrupt, the same
/// build started again with `--resume`, journalled as the harness's own
/// restart and announced on the terminal. The banner still drawn over the
/// new child restarts nothing more: the next restart waits out the back-off.
///
/// CONTROL: with the policy's `relaunch` off the same banner is left
/// ([`aterm_the_memory_banner_under_relaunch_off_is_left`]).
pub(super) fn aterm_the_memory_banner_restarts_the_child_once() {
    let s = Scenario::new("aterm-wall-memory");
    let fake = walled_aterm(&s);
    fake.set_screen(memory_banner());
    let pty = Pty::open();
    let (_child, pid) = start_in_aterm(&s, &pty, &fake, 40);
    let first = await_record(&s, 0);
    assert!(
        wait_until(Duration::from_secs(15), || s.starts().len() >= 2),
        "no restart at the memory banner{}",
        s.report()
    );
    let starts = s.starts();
    assert_eq!(starts[1].build(), Some(BUILD_1), "{}", s.report());
    assert_eq!(starts[1].args, ["--resume", SID], "{}", s.report());
    let journal = s.journal();
    assert!(
        journal.contains(&format!(
            " own restart cause=memory SIGTERM pid={first} session={SID} build={BUILD_1} \
             argv=--resume\\u{{1f}}{SID}\n"
        )) && journal.contains(" duty walls restart rule=memory-restart@v1\n"),
        "{}",
        s.report()
    );
    assert!(
        wait_until(Duration::from_secs(5), || pty.output().contains(&format!(
            "claude-alab: claude 9.9.1 restarted for its memory wall; resuming conversation {SID}"
        ))),
        "--- pty ---\n{}{}",
        pty.output(),
        s.report()
    );
    // The new child under the same banner waits out the back-off (10 s).
    await_record(&s, 1);
    assert!(
        wait_until(Duration::from_secs(5), || s.journal().contains(
            " duty walls wait restart-backoff rule=memory-restart@v1 until="
        )),
        "the back-off was not waited{}",
        s.report()
    );
    assert_eq!(s.starts().len(), 2, "{}", s.report());
    assert_eq!(
        s.log().iter().filter(|l| *l == "signal TERM").count(),
        1,
        "exactly one SIGTERM{}",
        s.report()
    );
    assert!(fake.typed().is_empty(), "typed at the banner{}", s.report());
    end_session(&s, pid);
}

/// With the policy's `relaunch` off the critical-memory banner is the
/// person's: nothing is signalled or typed, and it is said once.
///
/// CONTROL: with `relaunch` on the child is restarted
/// ([`aterm_the_memory_banner_restarts_the_child_once`]).
pub(super) fn aterm_the_memory_banner_under_relaunch_off_is_left() {
    let s = Scenario::new("aterm-wall-memory-off");
    let fake = rostered_aterm(&s);
    fake.set_policy_quietly(&walls_on().replace("relaunch=true", "relaunch=false"));
    fake.set_status("autopilot", Some("cruise"));
    fake.set_screen(memory_banner());
    let pty = Pty::open();
    let (_child, pid) = start_in_aterm(&s, &pty, &fake, 40);
    await_record(&s, 0);
    assert!(
        wait_until(Duration::from_secs(15), || s
            .journal()
            .contains(" duty walls left memory critical: restart it: ")),
        "the banner was not left{}",
        s.report()
    );
    for _ in 0..3 {
        fake.event("status");
        std::thread::sleep(Duration::from_millis(700));
    }
    assert_eq!(s.starts().len(), 1, "{}", s.report());
    assert!(
        !s.log().iter().any(|l| l == "signal TERM"),
        "signalled{}",
        s.report()
    );
    assert_eq!(
        s.journal()
            .matches(" duty walls left memory critical")
            .count(),
        1,
        "{}",
        s.report()
    );
    assert!(fake.typed().is_empty(), "typed at the banner{}", s.report());
    end_session(&s, pid);
}
