// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The turn-end duty, end to end: under a claim that takes `turn-end` the
//! harness reads its session through aterm and types Autopilot's act at a
//! turn end as one fenced `turn`, once for a screen, held for a person, and
//! never into a screen that is not a turn's end. aterm here is the fake
//! ([`FakeAterm`]), whose screen is an idle Claude Code that said `Done.`
//! unless a scenario shows another.

use super::*;

/// A policy body whose Cruise and Land words are short and known, as aterm
/// percent-encodes them.
fn policy_with_texts() -> String {
    format!(
        "{POLICY_ON} continue_text=Keep%20going. land_text=Land%20it. \
         land_nudge_text=Go%20on%20landing."
    )
}

/// A fake aterm for `s` whose roster carries this harness, whose policy
/// carries the known words, and whose `status` states `mode` (`None`: no
/// `autopilot=` at all).
fn carried_aterm(s: &Scenario, mode: Option<&str>) -> FakeAterm {
    let fake = rostered_aterm(s);
    fake.set_policy_quietly(&policy_with_texts());
    fake.set_status("autopilot", mode);
    fake
}

/// Claude Code 2.1.316's measured permission box, whole (aterm-phase's own
/// fixture, which its reader reads as a box), with the cursor on its focused
/// option.
fn measured_box() -> (Vec<String>, (usize, usize)) {
    let rows: Vec<String> = include_str!(
        "../../vendor/aterm-phase/src/fixtures/claude-2.1.316-box-rm-auto-block-bypass.txt"
    )
    .lines()
    .map(str::to_string)
    .collect();
    let focused = rows
        .iter()
        .position(|row| row.starts_with(" ❯ 1. Yes"))
        .expect("the box's focused option");
    (rows, (focused, 1))
}

/// The texts `fake` took as turns, in order.
fn turns(fake: &FakeAterm) -> Vec<String> {
    fake.typed()
        .into_iter()
        .filter_map(|typed| typed.strip_prefix("turn ").map(str::to_string))
        .collect()
}

/// The duties the claim standing on `fake` names (`status supervisor_scope=`).
fn claimed_scope(fake: &FakeAterm) -> String {
    fake.status_line()
        .split_ascii_whitespace()
        .find_map(|word| word.strip_prefix("supervisor_scope="))
        .unwrap_or("-")
        .to_string()
}

/// The scope every typing duty this build performs adds to the two that type
/// nothing.
const TYPING_SCOPE: &str = "relaunch,upgrade,turn-end,approvals,questions,walls";

/// Whether `s`'s duty read the screen again after its act and found the act
/// untaken on it: waiting for its own unsent text to be taken, or, where the
/// composer does not read as that text, doing nothing.
pub(super) fn awaited_again(s: &Scenario) -> bool {
    let journal = s.journal();
    journal.contains(" duty turn-end wait awaiting-take until=")
        || journal.contains(" duty turn-end nothing awaiting-take\n")
}

/// Under Cruise a turn end gets the policy's continuation, typed once through
/// the claim's fence and journalled with aterm's answer; the screen that then
/// shows it, read again, gets nothing more.
///
/// CONTROL: before that, the same screen in a session aterm states no mode
/// for (`autopilot=-`: it admits no agent yet) gets nothing, and the claim
/// names none of the duties that type (only the two that type nothing, which
/// it names under every word), until the session's Autopilot word is set,
/// whose event has the mode read again and the claim made again with them.
pub(super) fn aterm_cruise_types_the_continuation_once_through_the_fence() {
    let s = Scenario::new("aterm-turn-end-cruise");
    let fake = carried_aterm(&s, Some("-"));
    let pty = Pty::open();
    let (_child, pid) = start_in_aterm(&s, &pty, &fake, 40);
    await_record(&s, 0);
    // Several looks with no mode stated.
    std::thread::sleep(Duration::from_millis(600));
    assert!(fake.typed().is_empty(), "typed with no mode{}", s.report());
    assert!(
        !fake.verbs().iter().any(|verb| verb == "text --json"),
        "read the screen with no mode{}",
        s.report()
    );
    assert_eq!(
        claimed_scope(&fake),
        "relaunch,upgrade",
        "claimed a typing duty with no mode{}",
        s.report()
    );
    fake.set_status("autopilot", Some("cruise"));
    fake.event("meta field=autopilot value=cruise");
    assert!(
        wait_until(Duration::from_secs(15), || !turns(&fake).is_empty()),
        "no continuation{}",
        s.report()
    );
    assert_eq!(claimed_scope(&fake), TYPING_SCOPE, "{}", s.report());
    await_journal(
        &s,
        Duration::from_secs(5),
        &format!("aterm claimed holder=claude-code-alab@{pid} scope={TYPING_SCOPE} vendor=9.9.1"),
    );
    await_journal(
        &s,
        Duration::from_secs(5),
        "duty turn-end typed rule=continue@v1 reply=submitted",
    );
    // The next read of the screen that shows it (the recheck, 5 s on) sees
    // the act untaken and types nothing more.
    assert!(
        wait_until(Duration::from_secs(15), || awaited_again(&s)),
        "the unchanged screen was not read again{}",
        s.report()
    );
    assert_eq!(
        fake.typed(),
        ["turn Keep going.", "enter"],
        "{}",
        s.report()
    );
    end_session(&s, pid);
}

/// A claim made under Cruise names every duty that types; once aterm states
/// no mode for the session (`autopilot=-`) it is made again with only the two
/// that type nothing, and nothing more is typed or read for the rest, which
/// nothing performs under no mode.
///
/// CONTROL: while the mode is Cruise the same screen gets its continuation
/// through the claim's fence, so the duties were the harness's until then.
pub(super) fn aterm_the_typing_duties_leave_the_claim_when_no_mode_is_stated() {
    let s = Scenario::new("aterm-turn-end-given-back");
    let fake = carried_aterm(&s, Some("cruise"));
    let pty = Pty::open();
    let (_child, pid) = start_in_aterm(&s, &pty, &fake, 40);
    await_record(&s, 0);
    assert!(
        wait_until(Duration::from_secs(15), || !turns(&fake).is_empty()),
        "no continuation under Cruise{}",
        s.report()
    );
    assert_eq!(claimed_scope(&fake), TYPING_SCOPE, "{}", s.report());
    await_journal(
        &s,
        Duration::from_secs(5),
        "duty turn-end typed rule=continue@v1 reply=submitted",
    );
    let typed = fake.typed();
    fake.set_status("autopilot", Some("-"));
    fake.event("meta field=autopilot value=-");
    assert!(
        wait_until(Duration::from_secs(15), || claimed_scope(&fake)
            == "relaunch,upgrade"),
        "the typing duties were kept with no mode: {}{}",
        claimed_scope(&fake),
        s.report()
    );
    // The launch's claim, the one made under Cruise, and the one made again
    // without the typing duties, each journalled.
    let claimed = format!(" aterm claimed holder=claude-code-alab@{pid} scope=");
    assert_eq!(s.journal().matches(&claimed).count(), 3, "{}", s.report());
    let reads = fake
        .verbs()
        .iter()
        .filter(|verb| *verb == "text --json")
        .count();
    fake.event("status");
    std::thread::sleep(Duration::from_millis(1_000));
    assert_eq!(fake.typed(), typed, "typed with no mode{}", s.report());
    assert_eq!(
        fake.verbs()
            .iter()
            .filter(|verb| *verb == "text --json")
            .count(),
        reads,
        "read the screen with no mode{}",
        s.report()
    );
    end_session(&s, pid);
}

/// A person's key within the policy's grace holds the turn end: nothing is
/// typed, and the journal says what held it.
///
/// CONTROL: the same turn end with nobody at the keys is typed
/// ([`aterm_cruise_types_the_continuation_once_through_the_fence`]).
pub(super) fn aterm_a_person_at_the_keys_holds_the_turn_end() {
    let s = Scenario::new("aterm-turn-end-person");
    let fake = carried_aterm(&s, Some("cruise"));
    // Inside the policy's 120 s grace for the whole scenario.
    fake.set_human_seq(3);
    let pty = Pty::open();
    let (_child, pid) = start_in_aterm(&s, &pty, &fake, 40);
    await_record(&s, 0);
    assert!(
        wait_until(Duration::from_secs(15), || s
            .journal()
            .contains(" duty turn-end held rule=continue@v1 human_ms=")),
        "never held for the person{}",
        s.report()
    );
    std::thread::sleep(Duration::from_millis(600));
    assert!(fake.typed().is_empty(), "typed over a person{}", s.report());
    end_session(&s, pid);
}

/// Under Land a box gets nothing; once it is answered, the turn end gets
/// Land's instruction in the policy's words.
pub(super) fn aterm_land_leaves_a_box_alone_and_instructs_at_the_turn_end() {
    let s = Scenario::new("aterm-turn-end-land");
    let fake = carried_aterm(&s, Some("land"));
    fake.set_screen(measured_box());
    let pty = Pty::open();
    let (_child, pid) = start_in_aterm(&s, &pty, &fake, 40);
    await_record(&s, 0);
    await_journal(&s, Duration::from_secs(15), "duty turn-end nothing box");
    std::thread::sleep(Duration::from_millis(600));
    assert!(fake.typed().is_empty(), "typed into a box{}", s.report());
    // The box is answered, and aterm says the session moved.
    fake.set_screen(fake_aterm::composer_screen());
    fake.event("status");
    assert!(
        wait_until(Duration::from_secs(15), || !turns(&fake).is_empty()),
        "no instruction{}",
        s.report()
    );
    assert_eq!(turns(&fake), ["Land it."], "{}", s.report());
    await_journal(
        &s,
        Duration::from_secs(5),
        "duty turn-end typed rule=land@v1 reply=submitted",
    );
    end_session(&s, pid);
}

/// A launch that claims no `turn-end` (`--alab-duties=relaunch,upgrade`)
/// performs no turn end: its claim names only the two, and it neither reads
/// the screen nor types, under Cruise.
///
/// CONTROL: the default claim takes `turn-end` and types
/// ([`aterm_cruise_types_the_continuation_once_through_the_fence`]).
pub(super) fn aterm_a_claim_without_turn_end_types_no_turn_end() {
    let s = Scenario::new("aterm-turn-end-unclaimed");
    let fake = carried_aterm(&s, Some("cruise"));
    let pty = Pty::open();
    let (_child, pid) =
        start_in_aterm_with(&s, &pty, &fake, 40, &["--alab-duties=relaunch,upgrade"]);
    await_record(&s, 0);
    std::thread::sleep(Duration::from_millis(1_000));
    let claim = format!(
        "meta set supervisor claude-code-alab@{pid} contract=3 scope=relaunch,upgrade version="
    );
    assert!(
        fake.verbs().iter().any(|verb| verb.starts_with(&claim)),
        "{:?}{}",
        fake.verbs(),
        s.report()
    );
    assert!(fake.typed().is_empty(), "typed{}", s.report());
    assert!(
        !fake.verbs().iter().any(|verb| verb == "text --json"),
        "read the screen for a duty it does not take{}",
        s.report()
    );
    end_session(&s, pid);
}

/// A launch that names one typing duty alone (`--alab-duties=turn-end`)
/// still claims the two that type nothing, under no mode as under Cruise:
/// nothing else in aterm moves the session onto a new build or brings a
/// crash back. With no mode stated it claims those two and moves the session
/// onto a new build under that claim; once the mode is Cruise the claim is
/// made again with `turn-end` too.
///
/// CONTROL: the typing duty is not claimed while no mode is stated, and the
/// claim with it names no other typing duty.
pub(super) fn aterm_a_launch_of_a_typing_duty_alone_claims_the_two_that_type_nothing() {
    let s = Scenario::new("aterm-turn-end-alone");
    let fake = carried_aterm(&s, Some("-"));
    let pty = Pty::open();
    let (_child, pid) = start_in_aterm_with(&s, &pty, &fake, 40, &["--alab-duties=turn-end"]);
    await_record(&s, 0);
    let holder = Some(format!("claude-code-alab@{pid}"));
    assert_eq!(fake.holder(), holder, "{}", s.report());
    assert_eq!(claimed_scope(&fake), "relaunch,upgrade", "{}", s.report());
    // The update is the claim's, and is made under it.
    s.set_current(BUILD_2);
    assert!(
        wait_until(Duration::from_secs(20), || s.starts().len() >= 2),
        "the session was not moved onto the new build{}",
        s.report()
    );
    assert_eq!(s.starts()[1].build(), Some(BUILD_2), "{}", s.report());
    assert_eq!(fake.holder(), holder, "{}", s.report());
    fake.set_status("autopilot", Some("cruise"));
    fake.event("meta field=autopilot value=cruise");
    assert!(
        wait_until(Duration::from_secs(15), || claimed_scope(&fake)
            == "relaunch,upgrade,turn-end"),
        "the typing duty was not claimed under Cruise: {}{}",
        claimed_scope(&fake),
        s.report()
    );
    end_session(&s, pid);
}
