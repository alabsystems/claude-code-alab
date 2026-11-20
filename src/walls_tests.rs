// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

use aterm_phase::Placement;

const NOW: u64 = 1_000_000_000;
const SEC: u64 = 1_000;
const MIN: u64 = 60 * SEC;

/// Every wall switch on, as aterm's defaults leave a session but for usage
/// credits.
const ON: WallPolicy = WallPolicy {
    retry_api_errors: true,
    resume_limits: true,
    usage_credits: false,
    compact_on_context_wall: true,
};

/// The weekly usage limit's dialog, its five-hour and weekly walls above it
/// (aterm-phase's measured fixture).
const LIMIT_MENU_WEEKLY: &str =
    include_str!("../vendor/aterm-phase/src/fixtures/claude-2.1.295-usage-limit-menu-weekly.txt");

/// A fixture's screen rows, its `# ` provenance line left out.
fn screen(text: &str) -> Vec<String> {
    let body = match text.strip_prefix("# ") {
        Some(rest) => rest.split_once('\n').map_or("", |(_, body)| body),
        None => text,
    };
    body.lines().map(str::to_string).collect()
}

/// A wall of `kind` saying `message`.
fn wall(kind: WallKind, message: &str) -> Wall {
    Wall {
        kind,
        message: message.to_string(),
        reset: None,
        row: 5,
        placement: Placement::Gutter,
        under_call: false,
    }
}

/// Claude Code at the end of a turn that ended on `wall`, its composer
/// empty, as its reader reports a screen it vouches for.
fn at(wall: Option<Wall>) -> Reading {
    let phase = match &wall {
        Some(w) if w.kind.reads_limited() => Phase::Limited {
            message: w.message.clone(),
            reset: w.reset.clone(),
        },
        _ => Phase::Idle,
    };
    Reading {
        program: Program::Claude,
        phase,
        phase_authoritative: true,
        wall,
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
    }
}

fn overloaded() -> Reading {
    at(Some(wall(
        WallKind::Overloaded,
        "API Error: 529 {\"type\":\"error\",\"error\":{\"type\":\"overloaded_error\"}}",
    )))
}

fn context_full() -> Reading {
    at(Some(wall(
        WallKind::Context,
        "Context limit reached · /compact or /clear to continue",
    )))
}

fn session_limit() -> Reading {
    at(Some(wall(
        WallKind::UsageSession,
        "You've hit your session limit · resets 3pm (America/Los_Angeles)",
    )))
}

/// A look at `reading`, screen sequence `seq`, `at_ms` past [`NOW`].
fn look<'a>(reading: &'a Reading, seq: u64, at_ms: u64) -> Look<'a> {
    Look {
        reading,
        rows: &[],
        seq,
        now_ms: NOW + at_ms,
        limit: Limit::default(),
        relaunch: true,
        seed: 0,
    }
}

fn decide(watch: &mut Watch, look: &Look<'_>, policy: &WallPolicy) -> Decision {
    watch.decide(look, policy, &TurnEndTiming::default())
}

/// Decide, and where the decision types, record it typed at the look.
fn act(watch: &mut Watch, look: &Look<'_>, policy: &WallPolicy) -> Decision {
    let decision = decide(watch, look, policy);
    if let Decision::Type { scope, rule, text } = &decision {
        watch.typed(*scope, rule, text, look.now_ms);
    }
    decision
}

fn typed(decision: &Decision) -> (Scope, &'static str, &str) {
    match decision {
        Decision::Type { scope, rule, text } => (*scope, *rule, text.as_str()),
        other => panic!("typed nothing: {other:?}"),
    }
}

fn until(decision: &Decision) -> u64 {
    match decision {
        Decision::Wait { until_ms, .. } => *until_ms,
        other => panic!("not a wait: {other:?}"),
    }
}

#[test]
fn the_policy_reads_each_wall_switch_and_an_unstated_one_is_off() {
    let reply = "OK gen=4 effective=on retry_api_errors=true resume_limits=false \
                 usage_credits=true compact_on_context_wall=true limit_wait=true";
    assert_eq!(
        WallPolicy::of_policy(reply),
        WallPolicy {
            retry_api_errors: true,
            resume_limits: false,
            usage_credits: true,
            compact_on_context_wall: true,
        }
    );
    // An older aterm that states none of them widens nothing.
    assert_eq!(
        WallPolicy::of_policy("OK gen=4 effective=on relaunch=true"),
        WallPolicy::default()
    );
    // A word other than `true` is not `true`.
    assert!(!WallPolicy::of_policy("OK retry_api_errors=yes").retry_api_errors);
    assert_eq!(WallPolicy::of_policy("ERR denied"), WallPolicy::default());
}

#[test]
fn a_screen_not_read_with_certainty_is_never_acted_on_and_a_certain_one_is() {
    let mut uncertain = context_full();
    uncertain.phase_authoritative = false;
    let mut watch = Watch::default();
    assert_eq!(
        decide(&mut watch, &look(&uncertain, 1, 0), &ON),
        Decision::Pass("uncertain")
    );
    // The negative control: the same wall, read with certainty, is acted on.
    let certain = context_full();
    assert_eq!(
        typed(&decide(&mut watch, &look(&certain, 1, 0), &ON)),
        (Scope::Walls, RULE_COMPACT, COMPACT)
    );
}

#[test]
fn a_box_a_busy_turn_and_another_program_are_not_this_dutys() {
    let mut boxed = context_full();
    boxed.phase = Phase::Prompt;
    let mut busy = context_full();
    busy.phase = Phase::Busy;
    let mut other = context_full();
    other.program = Program::Generic;
    let mut watch = Watch::default();
    assert_eq!(
        decide(&mut watch, &look(&boxed, 1, 0), &ON),
        Decision::Pass("box")
    );
    assert_eq!(
        decide(&mut watch, &look(&busy, 1, 0), &ON),
        Decision::Pass("busy")
    );
    assert_eq!(
        decide(&mut watch, &look(&other, 1, 0), &ON),
        Decision::Pass("not-claude")
    );
}

#[test]
fn an_outage_is_waited_out_in_silence_and_then_tried_blind() {
    let screen = overloaded();
    let mut watch = Watch::default();
    let first = decide(&mut watch, &look(&screen, 1, 0), &ON);
    assert_eq!(until(&first), NOW + 2 * MIN, "outage_first from the wall");
    assert_eq!(
        until(&decide(&mut watch, &look(&screen, 1, MIN), &ON)),
        NOW + 2 * MIN
    );
    let tried = act(&mut watch, &look(&screen, 1, 2 * MIN), &ON);
    assert_eq!(
        typed(&tried),
        (
            Scope::Walls,
            RULE_OUTAGE_RESUME,
            "[aterm harness] The API overload may have cleared; continue where you stopped."
        )
    );
}

#[test]
fn an_api_wall_is_left_while_retry_api_errors_is_off() {
    let screen = overloaded();
    let mut watch = Watch::default();
    let off = WallPolicy {
        retry_api_errors: false,
        ..ON
    };
    for at_ms in [0, 2 * MIN, 60 * MIN] {
        assert!(
            matches!(
                decide(&mut watch, &look(&screen, 1, at_ms), &off),
                Decision::Leave(_)
            ),
            "{at_ms}"
        );
    }
    // The negative control: with it on, the same wall is tried.
    assert!(matches!(
        decide(&mut watch, &look(&screen, 1, 2 * MIN), &ON),
        Decision::Type { .. }
    ));
}

#[test]
fn an_act_is_never_repeated_on_a_screen_that_did_not_change() {
    let screen = overloaded();
    let mut watch = Watch::default();
    let _ = decide(&mut watch, &look(&screen, 1, 0), &ON);
    assert!(matches!(
        act(&mut watch, &look(&screen, 1, 2 * MIN), &ON),
        Decision::Type { .. }
    ));
    // Hours on, the screen has not moved: nothing more is typed.
    for at_ms in [3 * MIN, 30 * MIN, 600 * MIN] {
        assert_eq!(
            decide(&mut watch, &look(&screen, 1, at_ms), &ON),
            Decision::Nothing("awaiting-take"),
            "{at_ms}"
        );
    }
    // The negative control: the screen moved and shows the wall again, so the
    // try met it, and the next try waits the doubled gap from here.
    let again = decide(&mut watch, &look(&screen, 2, 601 * MIN), &ON);
    assert_eq!(until(&again), NOW + 601 * MIN + 4 * MIN);
}

#[test]
fn its_own_unsent_text_is_entered_once_and_anyone_elses_never() {
    let screen = context_full();
    let mut watch = Watch::default();
    assert!(matches!(
        act(&mut watch, &look(&screen, 1, 0), &ON),
        Decision::Type { .. }
    ));
    let mut stuck = context_full();
    stuck.composer = Some((3, vec![COMPACT.to_string()]));
    let waited = decide(&mut watch, &look(&stuck, 2, 10 * SEC), &ON);
    assert_eq!(
        until(&waited),
        NOW + 30 * SEC,
        "take_within after it was typed"
    );
    assert_eq!(
        decide(&mut watch, &look(&stuck, 2, 30 * SEC), &ON),
        Decision::Submit {
            scope: Scope::Walls,
            rule: RULE_COMPACT
        }
    );
    assert_eq!(
        decide(&mut watch, &look(&stuck, 2, 90 * SEC), &ON),
        Decision::Nothing("awaiting-take"),
        "Enter is pressed once"
    );
    // The negative control: a composer holding someone else's words is never
    // entered.
    let mut other = Watch::default();
    let _ = act(&mut other, &look(&screen, 1, 0), &ON);
    let mut theirs = context_full();
    theirs.composer = Some((3, vec!["please look at the parser first".to_string()]));
    assert_eq!(
        decide(&mut other, &look(&theirs, 2, 60 * SEC), &ON),
        Decision::Nothing("awaiting-take")
    );
}

#[test]
fn a_rate_limits_retry_after_sets_the_first_try_and_bounds_the_later_ones() {
    let timing = TurnEndTiming::default();
    let after = Some(Duration::from_secs(7 * 60));
    assert_eq!(outage_gap(&timing, 0, after), Duration::from_secs(7 * 60));
    assert_eq!(
        outage_gap(&timing, 1, after),
        Duration::from_secs(7 * 60),
        "never sooner than it named"
    );
    assert_eq!(outage_gap(&timing, 3, after), Duration::from_secs(16 * 60));
    assert_eq!(
        outage_gap(&timing, 0, Some(Duration::from_secs(5 * 3600))),
        timing.outage_cap
    );
    // The negative control: no retry-after, the plain doubling ladder.
    assert_eq!(outage_gap(&timing, 0, None), Duration::from_secs(2 * 60));
    assert_eq!(outage_gap(&timing, 9, None), timing.outage_cap);
}

#[test]
fn a_reply_cut_off_is_tried_at_once_and_then_on_its_ladder() {
    let screen = at(Some(wall(
        WallKind::ApiError {
            code: None,
            retryable: true,
            cause: ApiCause::CutOff,
        },
        "API Error: Connection closed mid-stream",
    )));
    let mut watch = Watch::default();
    let first = act(&mut watch, &look(&screen, 1, 0), &ON);
    let (scope, rule, text) = typed(&first);
    assert_eq!((scope, rule), (Scope::Walls, RULE_API_CUTOFF));
    assert_eq!(
        text,
        format!(
            "Claude Code reported an API error (the reply was cut off), so your last reply may be \
             incomplete. {CARRY_ON}"
        )
    );
    assert_eq!(
        decide(&mut watch, &look(&screen, 1, SEC), &ON),
        Decision::Nothing("awaiting-take")
    );
    let again = decide(&mut watch, &look(&screen, 2, MIN), &ON);
    assert_eq!(until(&again), NOW + 2 * MIN, "net_backoff's first rung");
}

#[test]
fn a_refused_request_waits_its_own_ladder() {
    let screen = at(Some(wall(
        WallKind::ApiError {
            code: Some(400),
            retryable: false,
            cause: ApiCause::Request,
        },
        "API Error: 400 invalid_request_error",
    )));
    let mut watch = Watch::default();
    assert_eq!(
        until(&decide(&mut watch, &look(&screen, 1, 0), &ON)),
        NOW + MIN
    );
    let decision = decide(&mut watch, &look(&screen, 1, MIN), &ON);
    let (scope, rule, text) = typed(&decision);
    assert_eq!((scope, rule), (Scope::Walls, RULE_API_RETRY));
    assert!(
        text.starts_with(
            "Claude Code reported an API error (400, the request was refused). Carry on"
        )
    );
}

#[test]
fn a_full_context_is_compacted_and_left_while_compacting_is_off() {
    let screen = context_full();
    let mut watch = Watch::default();
    assert_eq!(
        typed(&act(&mut watch, &look(&screen, 1, 0), &ON)),
        (Scope::Walls, RULE_COMPACT, COMPACT)
    );
    assert_eq!(
        decide(&mut watch, &look(&screen, 1, SEC), &ON),
        Decision::Nothing("awaiting-take")
    );
    // Still full after the compaction: the next waits retry_backoff's rung.
    let again = decide(&mut watch, &look(&screen, 2, 5 * SEC), &ON);
    assert_eq!(until(&again), NOW + 5 * SEC + MIN);
    let off = WallPolicy {
        compact_on_context_wall: false,
        ..ON
    };
    assert!(matches!(
        decide(&mut Watch::default(), &look(&screen, 1, 0), &off),
        Decision::Leave(_)
    ));
}

#[test]
fn a_lost_login_is_the_persons_and_slash_login_is_never_typed() {
    let screen = at(Some(wall(
        WallKind::Auth,
        "Login expired · Please run /login",
    )));
    let mut watch = Watch::default();
    for (seq, at_ms) in [(1, 0), (1, 60 * MIN), (2, 600 * MIN)] {
        let decision = decide(&mut watch, &look(&screen, seq, at_ms), &ON);
        assert!(matches!(decision, Decision::Leave(_)), "{decision:?}");
    }
}

#[test]
fn a_usage_limit_is_waited_out_without_typing_and_resumed_past_its_reset() {
    let screen = session_limit();
    let reset = NOW + 90 * MIN;
    let mut seen = look(&screen, 1, 0);
    seen.limit.reset_at_ms = Some(reset);
    let mut watch = Watch::default();
    let due = reset + 150 * SEC;
    assert_eq!(
        until(&decide(&mut watch, &seen, &ON)),
        due,
        "the reset and its grace"
    );
    seen.now_ms = due - 1;
    assert_eq!(until(&decide(&mut watch, &seen, &ON)), due);
    seen.now_ms = due;
    assert_eq!(
        typed(&decide(&mut watch, &seen, &ON)),
        (
            Scope::TurnEnd,
            RULE_LIMIT_RESUME,
            "[aterm harness] The usage limit has cleared; continue where you stopped."
        )
    );
    // The seed spreads the resume over the jitter.
    let mut spread = look(&screen, 1, 0);
    spread.limit.reset_at_ms = Some(reset);
    spread.seed = 61_000;
    assert_eq!(
        until(&decide(&mut Watch::default(), &spread, &ON)),
        due + SEC
    );
}

#[test]
fn a_limit_with_no_reset_known_is_tried_blind_on_the_ladders_last_rung() {
    let screen = session_limit();
    let mut watch = Watch::default();
    assert_eq!(
        until(&decide(&mut watch, &look(&screen, 1, 0), &ON)),
        NOW + 30 * MIN
    );
    let decision = decide(&mut watch, &look(&screen, 1, 30 * MIN), &ON);
    let (scope, rule, text) = typed(&decision);
    assert_eq!((scope, rule), (Scope::TurnEnd, RULE_LIMIT_RESUME));
    assert!(text.contains("may have cleared"), "{text}");
}

#[test]
fn a_resume_the_limit_met_again_climbs_the_ladder() {
    let screen = session_limit();
    let reset = NOW + 10 * MIN;
    let mut seen = look(&screen, 1, 0);
    seen.limit.reset_at_ms = Some(reset);
    let mut watch = Watch::default();
    seen.now_ms = reset + 150 * SEC;
    assert!(matches!(act(&mut watch, &seen, &ON), Decision::Type { .. }));
    seen.now_ms += SEC;
    assert_eq!(
        decide(&mut watch, &seen, &ON),
        Decision::Nothing("awaiting-take")
    );
    // The limit met it again, the transcript naming the same reset.
    seen.seq = 2;
    seen.now_ms += MIN;
    assert_eq!(
        until(&decide(&mut watch, &seen, &ON)),
        seen.now_ms + 10 * MIN
    );
    // A later reset named since the meeting is waited for instead.
    seen.limit.reset_at_ms = Some(seen.now_ms + 5 * MIN);
    assert_eq!(
        until(&decide(&mut watch, &seen, &ON)),
        seen.now_ms + 5 * MIN + 150 * SEC
    );
}

#[test]
fn a_limit_claude_code_goes_on_past_by_itself_is_left_to_it_first() {
    let screen = at(Some(wall(
        WallKind::UsageSession,
        "Usage limit reached · continuing automatically at 3:15pm",
    )));
    let reset = NOW + 20 * MIN;
    let mut seen = look(&screen, 1, 0);
    seen.limit.reset_at_ms = Some(reset);
    assert_eq!(
        until(&decide(&mut Watch::default(), &seen, &ON)),
        reset + 10 * MIN,
        "auto_resume_grace past the reset, after the vendor's own"
    );
    // The negative control: a limit that does not go on by itself waits the
    // reset's grace only.
    let plain = session_limit();
    let mut plain_seen = look(&plain, 1, 0);
    plain_seen.limit.reset_at_ms = Some(reset);
    assert_eq!(
        until(&decide(&mut Watch::default(), &plain_seen, &ON)),
        reset + 150 * SEC
    );
}

#[test]
fn a_limit_the_harness_moves_off_is_never_acted_on() {
    let screen = at(Some(wall(
        WallKind::ModelBucket { consent: false },
        "You've hit your Fable limit · resets 1:36am (America/Los_Angeles)",
    )));
    let mut seen = look(&screen, 1, 0);
    seen.limit.reset_at_ms = Some(NOW + MIN);
    seen.limit.moving = true;
    let mut watch = Watch::default();
    for at_ms in [0, 10 * MIN, 600 * MIN] {
        seen.now_ms = NOW + at_ms;
        assert_eq!(
            decide(&mut watch, &seen, &ON),
            Decision::Nothing("fable-move")
        );
    }
    // The negative control: not moving, the same limit is resumed past its
    // reset.
    seen.limit.moving = false;
    assert!(matches!(
        decide(&mut watch, &seen, &ON),
        Decision::Type {
            rule: RULE_LIMIT_RESUME,
            ..
        }
    ));
}

#[test]
fn a_limit_whose_resume_the_turn_end_duty_owes_passes_to_it() {
    let screen = session_limit();
    let mut seen = look(&screen, 1, 600 * MIN);
    seen.limit.owed_resume = true;
    assert_eq!(
        decide(&mut Watch::default(), &seen, &ON),
        Decision::Pass("owed-resume")
    );
}

#[test]
fn a_limit_is_left_alone_while_resume_limits_is_off() {
    let screen = session_limit();
    let off = WallPolicy {
        resume_limits: false,
        ..ON
    };
    let mut seen = look(&screen, 1, 600 * MIN);
    seen.limit.reset_at_ms = Some(NOW);
    assert_eq!(
        decide(&mut Watch::default(), &seen, &off),
        Decision::Nothing("resume-limits-off")
    );
}

#[test]
fn a_limit_only_usage_credits_lift_is_left_while_they_are_off() {
    let screen = at(Some(wall(
        WallKind::Spend,
        "You're out of extra usage · add more at claude.ai/settings/usage",
    )));
    let decision = decide(&mut Watch::default(), &look(&screen, 1, 600 * MIN), &ON);
    assert!(matches!(decision, Decision::Leave(_)), "{decision:?}");
    // The negative control: with credits on, it climbs the entitlement
    // ladder and is resumed blind.
    let credits = WallPolicy {
        usage_credits: true,
        ..ON
    };
    let mut watch = Watch::default();
    assert_eq!(
        until(&decide(&mut watch, &look(&screen, 1, 0), &credits)),
        NOW + 30 * MIN
    );
    assert!(matches!(
        decide(&mut watch, &look(&screen, 1, 30 * MIN), &credits),
        Decision::Type {
            scope: Scope::TurnEnd,
            ..
        }
    ));
}

#[test]
fn the_memory_banner_restarts_the_child_even_mid_turn_and_only_under_relaunch() {
    let mut screen = at(Some(wall(
        WallKind::Memory,
        "Memory usage critical — restart Claude Code to free memory",
    )));
    screen.phase = Phase::Busy;
    let mut watch = Watch::default();
    assert_eq!(
        decide(&mut watch, &look(&screen, 1, 0), &ON),
        Decision::Restart {
            rule: RULE_MEMORY_RESTART
        }
    );
    let mut off = look(&screen, 1, 0);
    off.relaunch = false;
    let left = decide(&mut watch, &off, &ON);
    assert!(
        matches!(&left, Decision::Leave(why) if why.contains("Memory usage critical")),
        "{left:?}"
    );
}

#[test]
fn an_ended_turn_with_no_wall_ends_the_track_and_a_busy_one_keeps_it() {
    let screen = context_full();
    let mut watch = Watch::default();
    let _ = act(&mut watch, &look(&screen, 1, 0), &ON);
    let mut busy = at(None);
    busy.phase = Phase::Busy;
    assert_eq!(
        decide(&mut watch, &look(&busy, 2, SEC), &ON),
        Decision::Pass("no-wall")
    );
    // Met again after the busy turn: the ladder's next rung.
    assert_eq!(
        until(&decide(&mut watch, &look(&screen, 3, 2 * SEC), &ON)),
        NOW + 2 * SEC + MIN
    );
    // Got through: the track ends, and the next wall starts afresh.
    assert_eq!(
        decide(&mut watch, &look(&at(None), 4, 3 * SEC), &ON),
        Decision::Pass("no-wall")
    );
    assert!(matches!(
        decide(&mut watch, &look(&screen, 5, 4 * SEC), &ON),
        Decision::Type { .. }
    ));
}

#[test]
fn claude_codes_own_auto_continue_is_read_from_its_words_and_its_countdown() {
    assert!(goes_on_by_itself(
        "Usage limit reached · Continuing automatically at 3:15pm",
        &[]
    ));
    assert!(goes_on_by_itself(
        "You've hit your session limit · continues automatically when it resets",
        &[]
    ));
    assert!(!goes_on_by_itself(
        "You've hit your session limit · resets 3pm",
        &[]
    ));
}

#[test]
fn a_fable_limit_is_told_from_another_above_the_usage_dialog() {
    let rows = screen(LIMIT_MENU_WEEKLY);
    let title = rows
        .iter()
        .position(|row| row.contains("What do you want to do?"))
        .expect("the dialog's title");
    assert!(
        !fable_limit_above(&rows, title),
        "a weekly limit is not Fable's"
    );
    let mut fable = rows.clone();
    fable.insert(
        title - 2,
        "  ⎿  You've hit your Fable limit · resets 1:36am (America/Los_Angeles)".to_string(),
    );
    assert!(fable_limit_above(&fable, title + 1));
}

#[test]
fn the_api_wall_words_follow_atems_for_every_cause() {
    assert_eq!(
        api_wall_words(WallKind::Overloaded),
        "an API error (overloaded)"
    );
    assert_eq!(
        api_wall_words(WallKind::ApiError {
            code: Some(500),
            retryable: true,
            cause: ApiCause::Server
        }),
        "an API error (500)"
    );
    assert_eq!(
        api_wall_words(WallKind::ApiError {
            code: None,
            retryable: true,
            cause: ApiCause::Unreachable
        }),
        "an API error (the API could not be reached)"
    );
    assert_eq!(
        cleared_text("connection error", true),
        "[aterm harness] The connection error has cleared; continue where you stopped."
    );
}
