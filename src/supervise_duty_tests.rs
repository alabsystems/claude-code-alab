// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;
use crate::duty::LandState;

/// This child's interactive record, in `status` since `at`.
fn record(status: &str, at: u64) -> SessionRecord {
    SessionRecord {
        pid: 4242,
        session_id: "conversation".to_string(),
        cwd: "/work".to_string(),
        status: status.to_string(),
        status_updated_at_ms: at,
        started_at_ms: 1,
        kind: crate::profile::KIND_INTERACTIVE.to_string(),
        version: "2.1.316".to_string(),
    }
}

#[test]
fn the_record_times_each_turn_and_shows_an_act_taken_up() {
    let mut driver = Driver::default();
    driver.carried.typed(Rule::Continue, "go on", 1_000);
    // Idle since before the act: the act still waits, and no turn was seen.
    driver.record_seen(Some(&record(STATUS_IDLE, 500)));
    assert!(driver.carried.awaiting.is_some());
    assert_eq!(driver.turn_ms, None);
    // An unchanged record makes nothing due (CONTROL for the next).
    driver.due_ms = 99;
    driver.record_seen(Some(&record(STATUS_IDLE, 500)));
    assert_eq!(driver.due_ms, 99);
    // The turn starts: the act was taken up, and a read is due.
    driver.record_seen(Some(&record(STATUS_BUSY, 2_000)));
    assert!(driver.carried.awaiting.is_none());
    assert_eq!(driver.due_ms, 0);
    // It ends: its length is the time between.
    driver.record_seen(Some(&record(STATUS_IDLE, 200_000)));
    assert_eq!(driver.turn_ms, Some(198_000));
    // A record that is neither changes no length.
    driver.record_seen(Some(&record("waiting", 210_000)));
    assert_eq!(driver.turn_ms, Some(198_000));
}

#[test]
fn a_turn_run_between_two_looks_after_the_act_takes_it_up_with_no_length() {
    let mut driver = Driver::default();
    driver.record_seen(Some(&record(STATUS_IDLE, 900)));
    driver.carried.typed(Rule::Continue, "go on", 1_000);
    // CONTROL: no record at a look moves nothing.
    driver.record_seen(None);
    assert!(driver.carried.awaiting.is_some());
    driver.record_seen(Some(&record(STATUS_IDLE, 3_000)));
    assert!(driver.carried.awaiting.is_none());
    assert_eq!(driver.turn_ms, None);
}

#[test]
fn an_act_typed_has_the_turn_that_answers_it_timed_afresh() {
    let mut driver = Driver::default();
    driver.record_seen(Some(&record(STATUS_BUSY, 1_000)));
    driver.record_seen(Some(&record(STATUS_IDLE, 2_000)));
    assert_eq!(driver.turn_ms, Some(1_000));
    driver.typed(Rule::Continue, "go on", 3_000);
    assert!(driver.carried.awaiting.is_some());
    assert_eq!(driver.turn_ms, None);
    // CONTROL: the turn that answers it is timed.
    driver.record_seen(Some(&record(STATUS_BUSY, 4_000)));
    driver.record_seen(Some(&record(STATUS_IDLE, 4_500)));
    assert_eq!(driver.turn_ms, Some(500));
}

#[test]
fn a_move_between_two_stated_modes_is_lands_new_task() {
    let mut driver = Driver::default();
    driver.carried.land = LandState::Landed;
    // The first mode stated, and the same again, move nothing.
    driver.mode_seen(Autopilot::Land);
    driver.mode_seen(Autopilot::Land);
    assert_eq!(driver.carried.land, LandState::Landed);
    driver.mode_seen(Autopilot::Cruise);
    assert_eq!(driver.carried.land, LandState::Untold);
}

#[test]
fn each_decision_is_said_once_and_an_act_has_the_next_said_again() {
    let journal = Journal::none();
    let mut driver = Driver::default();
    driver.note(&journal, "nothing busy");
    assert_eq!(driver.noted, "nothing busy");
    driver.act(&journal, "typed rule=continue@v1 reply=submitted");
    assert!(driver.noted.is_empty());
}

#[test]
fn only_a_text_that_reached_the_session_or_may_have_is_awaited() {
    for reply in [
        Reply::Submitted,
        Reply::Unverified,
        Reply::Left("guard".to_string()),
        Reply::InDoubt("ERR%20write%20failed".to_string()),
    ] {
        assert!(typed(&reply), "{reply:?}");
    }
    for reply in [
        Reply::Person { seq: Some(7) },
        Reply::Skipped("changed".to_string()),
        Reply::Busy("ERR%20busy%20sink".to_string()),
        Reply::Observed,
        Reply::Usage("ERR%20usage".to_string()),
        Reply::Refused("ERR%20halted".to_string()),
        Reply::Written { seq: Some(7) },
    ] {
        assert!(!typed(&reply), "{reply:?}");
    }
    assert_eq!(
        reply_words(&Reply::Left("guard".to_string())),
        "left reason=guard"
    );
}

#[test]
fn a_child_started_again_is_read_afresh_and_owes_what_it_is_owed() {
    let mut driver = Driver::default();
    driver.record_seen(Some(&record(STATUS_BUSY, 2_000)));
    driver.usage(1_000);
    driver.unread(&Journal::none(), "turn-end", "x", 1_000);
    driver.due_ms = 99;
    driver.child_restarted(true);
    assert!(driver.carried.owed_resume);
    assert_eq!(
        (
            driver.due_ms,
            driver.record.is_none(),
            driver.busy_since_ms,
            driver.usage_run,
            driver.unread_run
        ),
        (0, true, None, 0, 0)
    );
    // CONTROL: a child started again for any other reason owes nothing.
    let mut driver = Driver::default();
    driver.child_restarted(false);
    assert!(!driver.carried.owed_resume);
}

#[test]
fn a_child_started_again_forgets_its_wall_and_keeps_its_memory_restarts() {
    let mut driver = Driver::default();
    let reading = aterm_phase::Reading {
        program: aterm_phase::Program::Claude,
        phase: alab_policy::vocab::Phase::Idle,
        phase_authoritative: true,
        wall: Some(aterm_phase::Wall {
            kind: alab_policy::vocab::WallKind::Context,
            message: "Context limit reached · /compact or /clear to continue".to_string(),
            reset: None,
            row: 3,
            placement: aterm_phase::Placement::Gutter,
            under_call: false,
        }),
        prompt: None,
        survey: false,
        context_left: None,
        suggestion: None,
        goal_active: false,
        said_tail: None,
        interrupted: false,
        fresh: false,
        composer: Some((3, vec![String::new()])),
        credits: None,
    };
    let look = walls::Look {
        reading: &reading,
        rows: &[],
        seq: 1,
        now_ms: 1_000,
        limit: walls::Limit::default(),
        relaunch: true,
        seed: 0,
    };
    let policy = walls::WallPolicy {
        compact_on_context_wall: true,
        ..walls::WallPolicy::default()
    };
    assert!(matches!(
        driver
            .walls
            .decide(&look, &policy, &TurnEndTiming::default()),
        walls::Decision::Type { .. }
    ));
    driver
        .walls
        .typed(Scope::Walls, walls::RULE_COMPACT, walls::COMPACT, 1_000);
    // CONTROL: the act is remembered while the child runs.
    assert_ne!(driver.walls, walls::Watch::default());
    driver.memory_restarts = 2;
    driver.memory_after_ms = 9_000;
    driver.child_restarted(false);
    assert_eq!(driver.walls, walls::Watch::default());
    assert_eq!((driver.memory_restarts, driver.memory_after_ms), (2, 9_000));
    // A duty's own time is the next read's, whatever was due.
    driver.due_ms = 5;
    driver.read_at(77);
    assert_eq!(driver.due_ms, 77);
}

#[test]
fn an_act_still_to_be_taken_up_holds_a_restart_for_its_take_window_only() {
    let mut driver = Driver::default();
    // CONTROL: nothing typed holds nothing.
    assert!(!driver.act_pending(1_000));
    driver.carried.typed(Rule::Continue, "go on", 1_000);
    let within = u64::try_from(TurnEndTiming::default().take_within.as_millis()).unwrap();
    assert!(driver.act_pending(1_000));
    assert!(driver.act_pending(1_000 + within - 1));
    // An act the agent has not taken that long after is no turn to cut.
    assert!(!driver.act_pending(1_000 + within));
    // Taken up: the turn it starts is the record's to show.
    driver.record_seen(Some(&record(STATUS_BUSY, 2_000)));
    assert!(!driver.act_pending(2_000));
}

#[test]
fn a_write_aterm_reads_otherwise_is_tried_again_further_apart_and_never_given_away() {
    let mut driver = Driver::default();
    // Each usage answer in a row spaces the next read by the next rung, and
    // the last rung repeats, whatever was due.
    let mut at = 1_000;
    for wait in USAGE_BACKOFF_MS
        .into_iter()
        .chain([USAGE_BACKOFF_MS[USAGE_BACKOFF_MS.len() - 1]])
    {
        driver.due_ms = 0;
        driver.usage(at);
        assert_eq!(driver.due_ms, at + wait);
        at = driver.due_ms;
    }
    // A write aterm read as written counts the run afresh.
    driver.write_answered(&Typed::Answered(Reply::Submitted));
    driver.usage(at);
    assert_eq!(driver.due_ms, at + USAGE_BACKOFF_MS[0]);
    // Negative control: a read refused in between neither climbs nor resets
    // the writes' run.
    driver.unread(&Journal::none(), "turn-end", "x", at);
    driver.read_answered();
    driver.usage(at);
    assert_eq!(driver.due_ms, at + USAGE_BACKOFF_MS[1]);
    // Negative control: a write held before it was sent, one whose read
    // went unanswered, one refused for its authority and another usage
    // answer sent aterm nothing it read as written: the run climbs on.
    for nothing_heard in [
        Typed::Held(crate::aterm::Hold {
            field: "composer",
            value: "draft".to_string(),
            grace_left: None,
        }),
        Typed::Unread(crate::aterm::ReplyError::Missing("text")),
        Typed::Denied { status: None },
        Typed::Answered(Reply::Usage("ERR%20usage".to_string())),
    ] {
        driver.write_answered(&nothing_heard);
    }
    driver.usage(at);
    assert_eq!(driver.due_ms, at + USAGE_BACKOFF_MS[2]);
    // Any other answer to a write is aterm's reading of it as written.
    for heard in [Reply::Person { seq: Some(7) }, Reply::Busy("x".to_string())] {
        driver.usage(at);
        driver.write_answered(&Typed::Answered(heard));
        driver.usage(at);
        assert_eq!(driver.due_ms, at + USAGE_BACKOFF_MS[0]);
    }
}

#[test]
fn reads_that_go_unread_are_tried_again_further_apart_until_one_is_answered() {
    let journal = Journal::none();
    let duties = "turn-end,approvals,questions,walls";
    let mut driver = Driver::default();
    let mut at = 10_000;
    for wait in UNREAD_BACKOFF_MS
        .into_iter()
        .chain([UNREAD_BACKOFF_MS[UNREAD_BACKOFF_MS.len() - 1]; 3])
    {
        driver.unread(&journal, duties, "policy reply=ERR:denied", at);
        assert_eq!(driver.due_ms, at + wait);
        at = driver.due_ms;
    }
    // An event aterm sends makes a read due at once, back-off or not.
    driver.wake();
    assert_eq!(driver.due_ms, 0);
    // Negative control: an answered read between unread ones counts them
    // afresh, so a read that fails now and then is tried again soon.
    driver.read_answered();
    driver.unread(&journal, duties, "policy reply=ERR:denied", at);
    assert_eq!(driver.due_ms, at + UNREAD_BACKOFF_MS[0]);
}
