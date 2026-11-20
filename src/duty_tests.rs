// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

const NOW: u64 = 1_000_000;
const MIN: u64 = 60_000;

/// An idle Claude Code at the end of a turn whose last words are `said`, its
/// composer empty, as aterm-phase's reader reports one it vouches for.
fn reading(phase: Phase, said: Option<&str>) -> Reading {
    Reading {
        program: Program::Claude,
        phase,
        phase_authoritative: true,
        wall: None,
        prompt: None,
        survey: false,
        context_left: None,
        suggestion: None,
        goal_active: false,
        said_tail: said.map(str::to_string),
        interrupted: false,
        fresh: false,
        composer: Some((3, vec![String::new()])),
        credits: None,
    }
}

fn idle(said: &str) -> Reading {
    reading(Phase::Idle, Some(said))
}

/// A look at `seq` under `mode`, `at` ms past [`NOW`], after a turn of
/// `turn_ms`.
fn look<'a>(
    mode: Autopilot,
    reading: &'a Reading,
    seq: u64,
    at: u64,
    turn_ms: Option<u64>,
) -> Look<'a> {
    Look {
        autopilot: Some(mode),
        reading,
        seq,
        now_ms: NOW + at,
        turn_ms,
        repo: None,
        old_wall: false,
        pace: Pace::default(),
    }
}

/// `look` where the transcript names no usage limit since the child started:
/// a limit on the screen is the old one.
fn old(look: Look<'_>) -> Look<'_> {
    Look {
        old_wall: true,
        ..look
    }
}

/// `look` with the repository's reading `repo`.
fn over<'a>(look: Look<'a>, repo: &'a RepoRead) -> Look<'a> {
    Look {
        repo: Some(repo),
        ..look
    }
}

/// A turn of real work.
const LONG: Option<u64> = Some(10 * MIN);

fn decide(carried: &mut Carried, look: &Look<'_>) -> Decision {
    carried.decide(look, &Texts::vendored(), &TurnEndTiming::default())
}

fn typed_rule(decision: &Decision) -> Option<Rule> {
    match decision {
        Decision::Type { rule, .. } => Some(*rule),
        _ => None,
    }
}

/// Decide, and where the decision types, record it typed at the look.
fn act(carried: &mut Carried, look: &Look<'_>) -> Decision {
    let decision = decide(carried, look);
    if let Decision::Type { rule, text } = &decision {
        carried.typed(*rule, text, look.now_ms);
    }
    decision
}

#[test]
fn off_and_an_unknown_mode_type_nothing_and_cruise_continues() {
    let screen = idle("I fixed the parser and its tests pass.");
    for mode in [None, Some(Autopilot::Off)] {
        let mut carried = Carried::default();
        let mut seen = look(Autopilot::Cruise, &screen, 7, 0, LONG);
        seen.autopilot = mode;
        assert!(
            matches!(decide(&mut carried, &seen), Decision::Nothing(_)),
            "{mode:?}"
        );
    }
    // Negative control: the same screen under Cruise is continued with
    // Cruise's own words.
    let mut carried = Carried::default();
    assert_eq!(
        decide(&mut carried, &look(Autopilot::Cruise, &screen, 7, 0, LONG)),
        Decision::Type {
            rule: Rule::Continue,
            text: CONTINUE_TEXT.to_string()
        }
    );
}

#[test]
fn a_screen_read_without_certainty_or_not_at_a_turns_end_is_left_alone() {
    let plain = idle("I fixed the parser and its tests pass.");
    let mut uncertain = plain.clone();
    uncertain.phase_authoritative = false;
    let mut other = plain.clone();
    other.program = Program::Codex;
    let mut fresh = plain.clone();
    fresh.fresh = true;
    let mut interrupted = plain.clone();
    interrupted.interrupted = true;
    let mut survey = plain.clone();
    survey.survey = true;
    let limited = reading(
        Phase::Limited {
            message: "You've hit your limit".to_string(),
            reset: None,
        },
        None,
    );
    let busy = reading(Phase::Busy, None);
    let prompt = reading(Phase::Prompt, None);
    for (screen, why) in [
        (&uncertain, "uncertain"),
        (&other, "not-claude"),
        (&fresh, "fresh"),
        (&interrupted, "interrupted"),
        (&survey, "box"),
        (&limited, "wall"),
        (&busy, "busy"),
        (&prompt, "box"),
    ] {
        let mut carried = Carried::default();
        assert_eq!(
            decide(&mut carried, &look(Autopilot::Cruise, screen, 7, 0, LONG)),
            Decision::Nothing(why)
        );
    }
    // Negative control: the plain turn end those screens differ from is acted on.
    let mut carried = Carried::default();
    assert_eq!(
        typed_rule(&decide(
            &mut carried,
            &look(Autopilot::Cruise, &plain, 7, 0, LONG)
        )),
        Some(Rule::Continue)
    );
}

#[test]
fn an_act_is_never_repeated_on_a_screen_that_did_not_change() {
    let screen = idle("I fixed the parser and its tests pass.");
    let mut carried = Carried::default();
    assert!(
        typed_rule(&act(
            &mut carried,
            &look(Autopilot::Cruise, &screen, 7, 0, LONG)
        ))
        .is_some()
    );
    // The same screen, however long after: nothing, for as long as it stands.
    for at in [1_000, 60_000, 3_600_000] {
        assert_eq!(
            decide(&mut carried, &look(Autopilot::Cruise, &screen, 7, at, LONG)),
            Decision::Nothing("awaiting-take"),
            "{at}"
        );
    }
    // Negative control: once the agent has been busy, the next turn end is
    // acted on again.
    let busy = reading(Phase::Busy, None);
    assert_eq!(
        decide(
            &mut carried,
            &look(Autopilot::Cruise, &busy, 9, 4_000_000, LONG)
        ),
        Decision::Nothing("busy")
    );
    assert!(carried.awaiting.is_none());
    let next = idle("Rewrote the lexer; its tests pass too.");
    assert_eq!(
        typed_rule(&decide(
            &mut carried,
            &look(Autopilot::Cruise, &next, 12, 5_000_000, LONG)
        )),
        Some(Rule::Continue)
    );
    // So is a screen that moved with no busy turn seen between two looks.
    let mut carried = Carried::default();
    act(&mut carried, &look(Autopilot::Cruise, &screen, 7, 0, LONG));
    assert_eq!(
        decide(
            &mut carried,
            &look(Autopilot::Cruise, &screen, 7, 1_000, LONG)
        ),
        Decision::Nothing("awaiting-take")
    );
    let answered = idle("Nothing left: the suite ran green.");
    assert_eq!(
        typed_rule(&decide(
            &mut carried,
            &look(Autopilot::Cruise, &answered, 8, 2_000, LONG)
        )),
        Some(Rule::DoneCheck)
    );
}

#[test]
fn only_its_own_unsent_text_has_its_enter_pressed_again_and_only_once() {
    let screen = idle("I fixed the parser and its tests pass.");
    let mut carried = Carried::default();
    act(&mut carried, &look(Autopilot::Cruise, &screen, 7, 0, LONG));
    let mut stuck = screen.clone();
    let shown: String = CONTINUE_TEXT.chars().take(70).collect();
    stuck.composer = Some((3, vec![shown]));
    let take = u64::try_from(TurnEndTiming::default().take_within.as_millis()).unwrap();
    assert_eq!(
        decide(
            &mut carried,
            &look(Autopilot::Cruise, &stuck, 8, 1_000, LONG)
        ),
        Decision::Wait {
            until_ms: NOW + take,
            why: "awaiting-take"
        }
    );
    assert_eq!(
        decide(
            &mut carried,
            &look(Autopilot::Cruise, &stuck, 8, take, LONG)
        ),
        Decision::Submit(Rule::Continue)
    );
    assert_eq!(
        decide(
            &mut carried,
            &look(Autopilot::Cruise, &stuck, 8, 2 * take, LONG)
        ),
        Decision::Nothing("awaiting-take")
    );
    // Negative control: a draft that is not its own text is never submitted.
    let mut carried = Carried::default();
    act(&mut carried, &look(Autopilot::Cruise, &screen, 7, 0, LONG));
    let mut theirs = screen.clone();
    theirs.composer = Some((3, vec!["please also look at the flaky test".to_string()]));
    for at in [take, 10 * take] {
        assert_eq!(
            decide(&mut carried, &look(Autopilot::Cruise, &theirs, 8, at, LONG)),
            Decision::Nothing("awaiting-take")
        );
    }
    // Nor is a few characters of it that merely open it.
    let mut short = screen;
    short.composer = Some((3, vec!["[aterm".to_string()]));
    assert!(!own_draft(&short, CONTINUE_TEXT));
}

#[test]
fn cruise_asks_its_six_kinds_of_work_in_turn_at_each_claim_of_done() {
    let busy = reading(Phase::Busy, None);
    let mut carried = Carried::default();
    let texts = Texts::vendored();
    for n in 0..8_u64 {
        // Each claim in its own words: the same words twice count as a short
        // turn, which a later test holds.
        let done = idle(&format!(
            "Nothing left: ran check {n} on the merged tree; all green."
        ));
        let decision = act(
            &mut carried,
            &look(Autopilot::Cruise, &done, 10 * n, 1_000 * n, LONG),
        );
        let angle = words::DONE_ANGLES[usize::try_from(n).unwrap() % 6];
        match decision {
            Decision::Type {
                rule: Rule::DoneCheck,
                text,
            } => {
                assert!(text.contains(angle), "{n}: {text}");
                assert_eq!(text, texts.done_check(usize::try_from(n).unwrap()));
            }
            other => panic!("{n}: {other:?}"),
        }
        let _ = decide(
            &mut carried,
            &look(Autopilot::Cruise, &busy, 10 * n + 5, 1_000 * n + 500, None),
        );
    }
    // Negative control: words that claim nothing are continued.
    let mut carried = Carried::default();
    assert_eq!(
        typed_rule(&decide(
            &mut carried,
            &look(Autopilot::Cruise, &idle("Done."), 7, 0, LONG)
        )),
        Some(Rule::Continue)
    );
}

#[test]
fn land_instructs_once_nudges_checks_at_done_and_rests_on_landed() {
    let busy = reading(Phase::Busy, None);
    let working = idle("Wrote the parser; its tests come next.");
    let more = idle("The tests pass; the docs come next.");
    let done = idle("Nothing left: the merge is on origin/main and git status is clean.");
    let mut carried = Carried::default();
    let mut seq = 0;
    let mut at = 0;
    // One turn end acted on, then the agent's busy turn that takes the act up.
    let mut turn = |carried: &mut Carried, screen: &Reading| {
        seq += 10;
        at += 10 * MIN;
        let decision = act(carried, &look(Autopilot::Land, screen, seq, at, LONG));
        let _ = decide(
            carried,
            &look(Autopilot::Land, &busy, seq + 1, at + 1, None),
        );
        decision
    };
    assert_eq!(typed_rule(&turn(&mut carried, &working)), Some(Rule::Land));
    assert_eq!(
        typed_rule(&turn(&mut carried, &more)),
        Some(Rule::LandNudge)
    );
    assert_eq!(
        typed_rule(&turn(&mut carried, &done)),
        Some(Rule::LandCheck)
    );
    assert_eq!(carried.land, LandState::Checking);
    let rested = idle("LANDED");
    assert!(carried.reads_repo(Some(Autopilot::Land), &rested));
    assert_eq!(
        decide(
            &mut carried,
            &over(
                look(Autopilot::Land, &rested, 90, 90 * MIN, LONG),
                &RepoRead::Clean
            )
        ),
        Decision::Landed
    );
    assert!(!carried.reads_repo(Some(Autopilot::Land), &rested));
    // Landed: nothing more for this task, at any later look.
    assert_eq!(
        decide(
            &mut carried,
            &look(Autopilot::Land, &rested, 99, 100 * MIN, LONG)
        ),
        Decision::Nothing("landed")
    );
    // A turn a person starts is a new task, which Land carries afresh.
    let _ = decide(
        &mut carried,
        &look(Autopilot::Land, &busy, 120, 110 * MIN, None),
    );
    assert_eq!(carried.land, LandState::Untold);
    assert_eq!(
        typed_rule(&decide(
            &mut carried,
            &look(Autopilot::Land, &working, 130, 120 * MIN, LONG)
        )),
        Some(Rule::Land)
    );
}

#[test]
fn land_reads_the_checks_answer_a_list_is_nudged_and_a_need_is_the_persons() {
    let check_owed = || Carried {
        land: LandState::Checking,
        ..Carried::default()
    };
    let left = idle("Not landed:\n- the docs branch is unpushed\n- one flaky test");
    let mut carried = check_owed();
    match decide(&mut carried, &look(Autopilot::Land, &left, 7, 0, LONG)) {
        Decision::Type {
            rule: Rule::LandNudge,
            text,
        } => {
            assert!(text.starts_with(&words::land_nudge_text(None)), "{text}");
            assert!(text.contains("docs branch is unpushed"), "{text}");
            assert!(text.ends_with(land_check::LISTED_RESOLVE), "{text}");
        }
        other => panic!("{other:?}"),
    }
    let mut carried = check_owed();
    assert_eq!(
        decide(
            &mut carried,
            &look(
                Autopilot::Land,
                // A need only a person can meet: one that asks for a
                // credential is the lab's (`alab_policy::needs::persons_needs`).
                &idle("NEEDS YOU: choose which of the two designs ships"),
                7,
                0,
                LONG
            )
        ),
        Decision::Theirs
    );
    assert_eq!(carried.land, LandState::Theirs);
    // Negative control: before the check, the same words are no answer.
    let mut carried = Carried {
        land: LandState::Told,
        ..Carried::default()
    };
    assert_eq!(
        typed_rule(&decide(
            &mut carried,
            &look(Autopilot::Land, &idle("LANDED"), 7, 0, LONG)
        )),
        Some(Rule::LandNudge)
    );
}

#[test]
fn a_landed_answer_counts_only_over_a_repository_git_reads_clean() {
    let check_owed = || Carried {
        land: LandState::Checking,
        ..Carried::default()
    };
    let landed = idle("LANDED");
    let found = RepoRead::Found(
        crate::repo::RepoWork::of_git("/w/repo", "?? wip.txt\0", "", 0, 0, "").expect("work"),
    );
    // Work no remote main has: the nudge names it, and Land does not rest.
    let mut carried = check_owed();
    assert!(carried.reads_repo(Some(Autopilot::Land), &landed));
    let decision = act(
        &mut carried,
        &over(look(Autopilot::Land, &landed, 7, 0, LONG), &found),
    );
    match &decision {
        Decision::Type {
            rule: Rule::LandNudge,
            text,
        } => {
            assert!(text.starts_with(&words::land_nudge_text(None)), "{text}");
            assert!(
                text.ends_with(&found.ask().expect("a finding's ask")),
                "{text}"
            );
            assert!(text.contains("/w/repo: 1 untracked (wip.txt)"), "{text}");
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(carried.land, LandState::Told);
    // A repository git could not read, or one nobody read, is no clean one.
    for repo in [Some(&RepoRead::Unread), None] {
        let mut carried = check_owed();
        let mut at = look(Autopilot::Land, &landed, 7, 0, LONG);
        at.repo = repo;
        match decide(&mut carried, &at) {
            Decision::Type {
                rule: Rule::LandNudge,
                text,
            } => assert!(text.ends_with(crate::repo::REPO_UNREAD), "{text}"),
            other => panic!("{other:?}"),
        }
        assert_eq!(carried.land, LandState::Checking);
    }
    // Negative control: the same answer over a clean repository rests.
    let mut carried = check_owed();
    assert_eq!(
        decide(
            &mut carried,
            &over(look(Autopilot::Land, &landed, 7, 0, LONG), &RepoRead::Clean)
        ),
        Decision::Landed
    );
    // The repository is read only for a LANDED Land weighs: not under
    // Cruise, not before the check, not for another answer.
    let carried = check_owed();
    assert!(!carried.reads_repo(Some(Autopilot::Cruise), &landed));
    assert!(!carried.reads_repo(None, &landed));
    assert!(!carried.reads_repo(Some(Autopilot::Land), &idle("NEEDS YOU: a key")));
    let told = Carried {
        land: LandState::Told,
        ..Carried::default()
    };
    assert!(!told.reads_repo(Some(Autopilot::Land), &landed));
}

#[test]
fn a_short_turn_backs_the_next_act_off_but_never_lands_check_or_instruction() {
    let timing = TurnEndTiming::default();
    let backoff = u64::try_from(timing.short_backoff.as_millis()).unwrap();
    let screen = idle("Looked at it.");
    let short = Some(1_000);
    let mut carried = Carried::default();
    assert_eq!(
        decide(&mut carried, &look(Autopilot::Cruise, &screen, 7, 0, short)),
        Decision::Wait {
            until_ms: NOW + backoff,
            why: "short-turn"
        }
    );
    // The same turn end, past the back-off: acted on.
    assert_eq!(
        typed_rule(&decide(
            &mut carried,
            &look(Autopilot::Cruise, &screen, 7, backoff, short)
        )),
        Some(Rule::Continue)
    );
    // Negative control: a turn of real work is acted on at once.
    let mut carried = Carried::default();
    assert!(
        typed_rule(&decide(
            &mut carried,
            &look(Autopilot::Cruise, &screen, 7, 0, LONG)
        ))
        .is_some()
    );
    // The same last words twice in a row count as a short turn.
    let mut carried = Carried::default();
    act(&mut carried, &look(Autopilot::Cruise, &screen, 7, 0, LONG));
    let _ = decide(
        &mut carried,
        &look(Autopilot::Cruise, &reading(Phase::Busy, None), 8, 1, None),
    );
    assert!(matches!(
        decide(
            &mut carried,
            &look(Autopilot::Cruise, &screen, 9, 20 * MIN, LONG)
        ),
        Decision::Wait {
            why: "short-turn",
            ..
        }
    ));
    // Land's instruction and its check are never held back.
    let done = idle("Nothing left: all of it is on origin/main.");
    let mut carried = Carried {
        land: LandState::Told,
        ..Carried::default()
    };
    assert_eq!(
        typed_rule(&decide(
            &mut carried,
            &look(Autopilot::Land, &done, 7, 0, short)
        )),
        Some(Rule::LandCheck)
    );
    let mut carried = Carried::default();
    assert_eq!(
        typed_rule(&decide(
            &mut carried,
            &look(Autopilot::Land, &screen, 7, 0, short)
        )),
        Some(Rule::Land)
    );
    // Its nudge is.
    let mut carried = Carried {
        land: LandState::Told,
        ..Carried::default()
    };
    assert!(matches!(
        decide(&mut carried, &look(Autopilot::Land, &screen, 7, 0, short)),
        Decision::Wait {
            why: "short-turn",
            ..
        }
    ));
}

#[test]
fn an_irreversible_ask_is_answered_each_time_it_stands_and_never_parks() {
    let ask = reading(
        Phase::Question,
        Some("Should I force-push the branch to main?"),
    );
    let busy = reading(Phase::Busy, None);
    let texts = Texts::vendored();
    // Cruise answers it with aterm's reversible answer.
    let mut carried = Carried::default();
    match act(&mut carried, &look(Autopilot::Cruise, &ask, 7, 0, LONG)) {
        Decision::Type {
            rule: Rule::Answer,
            text,
        } => {
            assert_eq!(text, texts.reversible_answer());
            assert!(text.starts_with(REVERSIBLE_ANSWER), "{text}");
            assert!(text.contains(alab_policy::needs::NEEDS_HINT), "{text}");
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(Rule::Answer.id(), "answer@v1");
    // The same screen again gets nothing: the answer is awaited there.
    assert_eq!(
        decide(&mut carried, &look(Autopilot::Cruise, &ask, 7, 1, LONG)),
        Decision::Nothing("awaiting-take")
    );
    // The agent asks the same again: the repeated words back the answer off
    // as a short turn, so nothing parks, and past the back-off the ask is
    // answered again, never continued, since a continuation could read as
    // the consent it asks for.
    let _ = decide(&mut carried, &look(Autopilot::Cruise, &busy, 8, 2, None));
    let Decision::Wait {
        until_ms,
        why: "short-turn",
    } = decide(
        &mut carried,
        &look(Autopilot::Cruise, &ask, 9, 60 * MIN, LONG),
    )
    else {
        panic!("the repeated words back the next act off");
    };
    let again = decide(
        &mut carried,
        &look(Autopilot::Cruise, &ask, 9, until_ms - NOW, LONG),
    );
    assert_eq!(typed_rule(&again), Some(Rule::Answer), "{again:?}");
    // A claim of done that also names an irreversible act gets the done
    // check with aterm's reversible clause.
    let mut carried = Carried::default();
    let done =
        idle("Nothing left: the work is committed.\nShould I force-push the branch to main?");
    assert!(matches!(
        words::classify_said(done.said_tail.as_deref()),
        Said::Irreversible(_)
    ));
    match decide(&mut carried, &look(Autopilot::Cruise, &done, 7, 0, LONG)) {
        Decision::Type {
            rule: Rule::DoneCheck,
            text,
        } => assert_eq!(text, texts.done_check_reversible(0)),
        other => panic!("{other:?}"),
    }
    // Land answers it with Land's answer before its instruction, and again
    // each time it stands, past the back-off its repeated words earn.
    let mut carried = Carried::default();
    match act(&mut carried, &look(Autopilot::Land, &ask, 7, 0, LONG)) {
        Decision::Type {
            rule: Rule::LandAnswer,
            text,
        } => assert_eq!(text, words::land_answer_text(None)),
        other => panic!("{other:?}"),
    }
    assert_eq!(carried.land, LandState::Untold);
    let _ = decide(&mut carried, &look(Autopilot::Land, &busy, 8, 1, None));
    let Decision::Wait {
        until_ms,
        why: "short-turn",
    } = decide(
        &mut carried,
        &look(Autopilot::Land, &ask, 9, 60 * MIN, LONG),
    )
    else {
        panic!("the repeated words back Land's answer off");
    };
    assert_eq!(
        typed_rule(&decide(
            &mut carried,
            &look(Autopilot::Land, &ask, 9, until_ms - NOW, LONG)
        )),
        Some(Rule::LandAnswer)
    );
    // Negative control: a question that names nothing irreversible is
    // continued under Cruise, whose words say to decide.
    let mut carried = Carried::default();
    assert_eq!(
        typed_rule(&decide(
            &mut carried,
            &look(
                Autopilot::Cruise,
                &reading(
                    Phase::Question,
                    Some("Which of the two parsers should I keep?")
                ),
                7,
                0,
                LONG
            )
        )),
        Some(Rule::Continue)
    );
}

#[test]
fn land_answers_a_choice_or_a_question_with_lands_answer_as_aterm_types_it() {
    let choice = reading(
        Phase::Question,
        Some("Should I keep the old parser or the new one?"),
    );
    let question = reading(
        Phase::Question,
        Some("Which of the two parsers should I keep?"),
    );
    assert!(matches!(
        words::classify_said(choice.said_tail.as_deref()),
        Said::Stop(_)
    ));
    assert_eq!(
        words::classify_said(question.said_tail.as_deref()),
        Said::Question
    );
    let texts = Texts::vendored();
    for screen in [&choice, &question] {
        // Before Land's instruction, as aterm answers it first.
        let mut carried = Carried::default();
        assert_eq!(
            act(&mut carried, &look(Autopilot::Land, screen, 7, 0, LONG)),
            Decision::Type {
                rule: Rule::LandAnswer,
                text: texts.land_answer(),
            },
            "{screen:?}"
        );
        assert_eq!(carried.land, LandState::Untold);
        // After it, in place of the nudge, which would leave it unmade.
        let mut carried = Carried {
            land: LandState::Told,
            ..Carried::default()
        };
        assert_eq!(
            typed_rule(&decide(
                &mut carried,
                &look(Autopilot::Land, screen, 7, 0, LONG)
            )),
            Some(Rule::LandAnswer),
            "{screen:?}"
        );
        // Negative control: Cruise continues it, its words saying to decide.
        let mut carried = Carried::default();
        assert_eq!(
            typed_rule(&decide(
                &mut carried,
                &look(Autopilot::Cruise, screen, 7, 0, LONG)
            )),
            Some(Rule::Continue),
            "{screen:?}"
        );
    }
    // Negative control: a report that asks nothing gets Land's instruction.
    let mut carried = Carried::default();
    assert_eq!(
        typed_rule(&decide(
            &mut carried,
            &look(
                Autopilot::Land,
                &idle("Wrote the parser; its tests come next."),
                7,
                0,
                LONG
            )
        )),
        Some(Rule::Land)
    );
}

/// An answer to Land's check that asks anything, an irreversible act or a
/// choice, gets Land's answer in place of the nudge, which could read as the
/// consent it asks for, and the check stays owed.
#[test]
fn an_ask_in_the_checks_answer_gets_lands_answer_and_never_the_nudge() {
    let check_owed = || Carried {
        land: LandState::Checking,
        ..Carried::default()
    };
    let texts = Texts::vendored();
    let irreversible = idle(
        "Not landed:\n- the docs branch is unpushed\n\
         Should I force-push the rewritten branch to main?",
    );
    let choice = idle(
        "Not landed:\n- the docs branch is unpushed\n\
         Should I keep the old parser or the new one?",
    );
    assert!(matches!(
        words::classify_said(irreversible.said_tail.as_deref()),
        Said::Irreversible(_)
    ));
    assert!(matches!(
        words::classify_said(choice.said_tail.as_deref()),
        Said::Stop(_) | Said::Question
    ));
    for screen in [&irreversible, &choice] {
        assert!(
            matches!(
                land_check::land_check_answer(
                    screen.said_tail.as_deref().unwrap_or_default(),
                    None,
                    false
                ),
                CheckAnswer::Left(_)
            ),
            "{screen:?}"
        );
        let mut carried = check_owed();
        assert_eq!(
            act(&mut carried, &look(Autopilot::Land, screen, 7, 0, LONG)),
            Decision::Type {
                rule: Rule::LandAnswer,
                text: texts.land_answer(),
            },
            "{screen:?}"
        );
        // The check stays owed: the agent's next answer is read as its.
        assert_eq!(carried.land, LandState::Checking, "{screen:?}");
    }
    // Negative control: the same list asking nothing gets the nudge.
    let mut carried = check_owed();
    assert_eq!(
        typed_rule(&decide(
            &mut carried,
            &look(
                Autopilot::Land,
                &idle("Not landed:\n- the docs branch is unpushed\n- one flaky test"),
                7,
                0,
                LONG
            )
        )),
        Some(Rule::LandNudge)
    );
    // An answer with a `NEEDS YOU:` line rests whatever else it asks: nothing
    // is typed, so nothing is consented to.
    let mut carried = check_owed();
    assert_eq!(
        decide(
            &mut carried,
            &look(
                Autopilot::Land,
                &idle(
                    "NEEDS YOU: choose which of the two designs ships\n\
                     Should I force-push the rewritten branch to main?"
                ),
                7,
                0,
                LONG
            )
        ),
        Decision::Theirs
    );
}

#[test]
fn a_move_off_a_usage_limit_owes_one_resume_at_the_wall_it_stopped_at() {
    let wall = reading(
        Phase::Limited {
            message: "You've hit your limit".to_string(),
            reset: None,
        },
        None,
    );
    // The old limit: the transcript names none since the move.
    let mut carried = Carried::default();
    carried.child_restarted(true);
    let decision = act(
        &mut carried,
        &old(look(Autopilot::Cruise, &wall, 7, 0, None)),
    );
    assert_eq!(
        decision,
        Decision::Type {
            rule: Rule::FallbackResume,
            text: FALLBACK_RESUME_TEXT.to_string()
        }
    );
    assert!(!carried.owed_resume);
    assert_eq!(
        decide(
            &mut carried,
            &look(Autopilot::Cruise, &wall, 7, 1_000, None)
        ),
        Decision::Nothing("awaiting-take")
    );
    // Owed under Land too, and on a screen that no longer shows the wall.
    let mut carried = Carried::default();
    carried.child_restarted(true);
    assert_eq!(
        typed_rule(&decide(
            &mut carried,
            &look(Autopilot::Land, &idle("Working on it."), 7, 0, None)
        )),
        Some(Rule::FallbackResume)
    );
    // Negative controls: a wall nobody here moved off owes no resume (the
    // turn end does nothing at it; the walls duty reads it), and Off owes
    // nothing.
    let mut carried = Carried::default();
    carried.child_restarted(false);
    assert_eq!(
        decide(
            &mut carried,
            &old(look(Autopilot::Cruise, &wall, 7, 0, None))
        ),
        Decision::Nothing("wall")
    );
    // The limit of the model the session now runs (the transcript names it
    // since the move) gets nothing, and the resume stays owed for the turn
    // end after it.
    let mut carried = Carried::default();
    carried.child_restarted(true);
    assert_eq!(
        decide(&mut carried, &look(Autopilot::Cruise, &wall, 7, 0, None)),
        Decision::Nothing("wall")
    );
    assert!(carried.owed_resume);
    assert_eq!(
        typed_rule(&decide(
            &mut carried,
            &look(Autopilot::Cruise, &idle("Working on it."), 8, 1, None)
        )),
        Some(Rule::FallbackResume)
    );
    // A resume owed is typed only where every act may be: never under a box,
    // into a fresh session or a turn the person interrupted, nor at a wall
    // that is no usage limit, old or not.
    let context = aterm_phase::Wall {
        kind: alab_policy::vocab::WallKind::Context,
        message: "Context limit reached".to_string(),
        reset: None,
        row: 5,
        placement: aterm_phase::Placement::Gutter,
        under_call: false,
    };
    for (screen, why) in [
        (
            Reading {
                wall: Some(context),
                ..idle("Working on it.")
            },
            "wall",
        ),
        (
            Reading {
                survey: true,
                ..idle("Working on it.")
            },
            "box",
        ),
        (
            Reading {
                fresh: true,
                ..idle("Working on it.")
            },
            "fresh",
        ),
        (
            Reading {
                interrupted: true,
                ..idle("Working on it.")
            },
            "interrupted",
        ),
    ] {
        let mut carried = Carried::default();
        carried.child_restarted(true);
        assert_eq!(
            decide(
                &mut carried,
                &old(look(Autopilot::Cruise, &screen, 7, 0, None))
            ),
            Decision::Nothing(why)
        );
        assert!(carried.owed_resume, "{why}");
    }
    let mut carried = Carried::default();
    carried.child_restarted(true);
    assert_eq!(
        decide(&mut carried, &look(Autopilot::Off, &wall, 7, 0, None)),
        Decision::Nothing("autopilot-off")
    );
}

#[test]
fn the_walls_duty_resume_past_the_moved_sessions_own_limit_pays_the_owed_resume() {
    let report = idle("Working on it.");
    // The walls duty resumed the session past the limit of the model it was
    // moved onto: the work goes on, so no second resume follows.
    let mut carried = Carried::default();
    carried.child_restarted(true);
    carried.wall_typed(crate::walls::RULE_LIMIT_RESUME);
    assert!(!carried.owed_resume);
    assert_eq!(
        typed_rule(&decide(
            &mut carried,
            &look(Autopilot::Cruise, &report, 7, 0, LONG)
        )),
        Some(Rule::Continue)
    );
    // Negative control: any other wall act leaves the resume owed.
    for rule in [
        crate::walls::RULE_OUTAGE_RESUME,
        crate::walls::RULE_API_RETRY,
        crate::walls::RULE_COMPACT,
    ] {
        let mut carried = Carried::default();
        carried.child_restarted(true);
        carried.wall_typed(rule);
        assert!(carried.owed_resume, "{rule}");
        assert_eq!(
            typed_rule(&decide(
                &mut carried,
                &look(Autopilot::Cruise, &report, 7, 0, LONG)
            )),
            Some(Rule::FallbackResume),
            "{rule}"
        );
    }
}

#[test]
fn a_child_started_again_forgets_the_act_typed_into_the_one_that_ended() {
    let screen = idle("I fixed the parser and its tests pass.");
    let mut carried = Carried::default();
    act(&mut carried, &look(Autopilot::Cruise, &screen, 7, 0, LONG));
    // CONTROL: before the restart, the same screen gets nothing.
    assert_eq!(
        decide(&mut carried, &look(Autopilot::Cruise, &screen, 7, 1, LONG)),
        Decision::Nothing("awaiting-take")
    );
    carried.child_restarted(false);
    assert!(carried.awaiting.is_none() && carried.ended.is_none());
    // The new child shows the same last words, which are no short turn.
    assert_eq!(
        typed_rule(&decide(
            &mut carried,
            &look(Autopilot::Cruise, &screen, 7, 2, LONG)
        )),
        Some(Rule::Continue)
    );
}

#[test]
fn the_policy_replys_texts_replace_the_vendored_ones_key_by_key() {
    let vendored = Texts::vendored();
    let angles: Vec<String> = (1..=6).map(|n| format!("Angle {n}.")).collect();
    let rules = " (standing rules: land through verify land)";
    let whole = format!("{} Angle 1. My tail.{rules}", words::DONE_CHECK);
    let enc = |text: &str| {
        text.bytes()
            .map(|byte| {
                if byte.is_ascii_alphanumeric() {
                    char::from(byte).to_string()
                } else {
                    format!("%{byte:02X}")
                }
            })
            .collect::<String>()
    };
    let reply = format!(
        "OK gen=4 effective=on why=enabled contract=2,3 continue_text={} done_check_angles={} \
         done_check_tail={} done_check_text={} land_text=- land_nudge_text=%ZZ",
        enc("Keep going."),
        enc(&angles.join("\n")),
        enc("My tail."),
        enc(&whole),
    );
    let texts = Texts::of_policy(&reply);
    assert_eq!(texts.continue_text, "Keep going.");
    assert_eq!(texts.done_angles, angles);
    assert_eq!(texts.done_tail, "My tail.");
    assert_eq!(texts.done_rules, rules);
    assert_eq!(
        texts.done_check(1),
        format!("{} Angle 2. My tail.{rules}", words::DONE_CHECK)
    );
    // Negative controls: `-`, an undecodable value and an absent key keep the
    // vendored words, and so does a reply that is not one.
    assert_eq!(texts.land, vendored.land);
    assert_eq!(texts.land_nudge, vendored.land_nudge);
    assert_eq!(texts.land_check, vendored.land_check);
    assert_eq!(Texts::of_policy("ERR denied"), vendored);
    // Angles that are not six keep the vendored six.
    let five = format!(
        "OK gen=4 done_check_angles={}",
        enc(&angles[..5].join("\n"))
    );
    assert_eq!(Texts::of_policy(&five).done_angles, vendored.done_angles);
}

#[test]
fn the_memory_crosses_an_own_image_move_whole() {
    let carried = Carried {
        awaiting: Some(Awaiting {
            rule: Rule::LandCheck,
            text: "check ✓".to_string(),
            at_ms: 77,
            seen_seq: Some(90_262),
            entered: true,
        }),
        checks: 9,
        land: LandState::Checking,
        ended: Some(Ended {
            at_ms: 66,
            streak: 2,
        }),
        streak: 2,
        said: Some(0xfeed),
        owed_resume: true,
        acts: vec![NOW, NOW + MIN],
    };
    for value in [carried, Carried::default()] {
        let mut encoder = Encoder::new();
        value.put(&mut encoder).unwrap();
        let bytes = encoder.finish();
        let mut decoder = Decoder::new(&bytes).unwrap();
        assert_eq!(Carried::get(&mut decoder).unwrap(), value);
        decoder.finish().unwrap();
    }
    // More act times than are kept cross as the newest kept.
    let many = Carried {
        acts: (0..=MAX_PACED_ACTS as u64).collect(),
        ..Carried::default()
    };
    let mut encoder = Encoder::new();
    many.put(&mut encoder).unwrap();
    let bytes = encoder.finish();
    let mut decoder = Decoder::new(&bytes).unwrap();
    let crossed = Carried::get(&mut decoder).unwrap();
    assert_eq!(crossed.acts.len(), MAX_PACED_ACTS);
    assert_eq!(crossed.acts.first(), Some(&1));
    // Negative control: a Land state this build does not know is refused.
    let mut encoder = Encoder::new();
    Carried::default().put(&mut encoder).unwrap();
    let mut bytes = encoder.finish();
    // No act awaited (one byte), the checks, then the Land state.
    bytes[2] = 9;
    let mut decoder = Decoder::new(&bytes).unwrap();
    assert!(Carried::get(&mut decoder).is_err());
}

/// `look` under the policy's pace `pace`.
fn paced(look: Look<'_>, pace: Pace) -> Look<'_> {
    Look { pace, ..look }
}

#[test]
fn the_policys_continue_words_are_read_as_aterm_sends_them() {
    assert_eq!(
        Pace::of_policy("OK gen=3 effective=on continue=false continue_per_hour=4"),
        Pace {
            continues: false,
            per_hour: 4
        }
    );
    assert_eq!(
        Pace::of_policy("OK gen=3 effective=on continue=true continue_per_hour=0"),
        Pace::default()
    );
    // Absent, or outside its grammar, a word keeps aterm's default.
    for reply in [
        "OK gen=3 effective=on",
        "OK gen=3 continue=no continue_per_hour=+4",
        "OK gen=3 continue_per_hour=4x",
        "OK gen=3 continue_per_hour=99999999999",
        "ERR denied",
    ] {
        assert_eq!(Pace::of_policy(reply), Pace::default(), "{reply}");
    }
}

#[test]
fn continue_off_types_only_the_answer_to_an_ask() {
    let off = Pace {
        continues: false,
        per_hour: 0,
    };
    let plain = idle("I fixed the parser and its tests pass.");
    for mode in [Autopilot::Cruise, Autopilot::Land] {
        let mut carried = Carried::default();
        assert_eq!(
            decide(&mut carried, &paced(look(mode, &plain, 7, 0, LONG), off)),
            Decision::Nothing("continue-off"),
            "{mode:?}"
        );
    }
    // An irreversible ask is still answered.
    let ask = reading(
        Phase::Question,
        Some("Should I force-push the branch to main?"),
    );
    let mut carried = Carried::default();
    assert_eq!(
        typed_rule(&decide(
            &mut carried,
            &paced(look(Autopilot::Cruise, &ask, 7, 0, LONG), off)
        )),
        Some(Rule::Answer)
    );
    // Negative control: aterm's default pace continues.
    let mut carried = Carried::default();
    assert_eq!(
        typed_rule(&decide(
            &mut carried,
            &look(Autopilot::Cruise, &plain, 7, 0, LONG)
        )),
        Some(Rule::Continue)
    );
}

#[test]
fn continue_per_hour_waits_for_the_oldest_act_to_leave_the_window() {
    let two = Pace {
        continues: true,
        per_hour: 2,
    };
    let window = u64::try_from(TurnEndTiming::default().window.as_millis()).unwrap();
    let busy = reading(Phase::Busy, None);
    let mut carried = Carried::default();
    // Two acts in the window, each taken up by a busy turn.
    for (n, said) in [(0, "I fixed the parser."), (1, "I fixed the lexer.")] {
        let screen = idle(said);
        let at = n * MIN;
        assert_eq!(
            typed_rule(&act(
                &mut carried,
                &paced(look(Autopilot::Cruise, &screen, 10 + 2 * n, at, LONG), two)
            )),
            Some(Rule::Continue)
        );
        let _ = decide(
            &mut carried,
            &paced(
                look(Autopilot::Cruise, &busy, 11 + 2 * n, at + 1, None),
                two,
            ),
        );
    }
    let before = carried.clone();
    // The third turn end in the window waits for the first act to leave it.
    let third = idle("I fixed the printer.");
    assert_eq!(
        decide(
            &mut carried,
            &paced(look(Autopilot::Cruise, &third, 20, 2 * MIN, LONG), two)
        ),
        Decision::Wait {
            until_ms: NOW + window,
            why: "continue-per-hour"
        }
    );
    assert_eq!(
        typed_rule(&decide(
            &mut carried,
            &paced(look(Autopilot::Cruise, &third, 20, window, LONG), two)
        )),
        Some(Rule::Continue)
    );
    // Negative controls: no cap, or one the window has not reached, types
    // the third at once.
    for pace in [
        Pace::default(),
        Pace {
            continues: true,
            per_hour: 3,
        },
    ] {
        let mut free = before.clone();
        assert_eq!(
            typed_rule(&decide(
                &mut free,
                &paced(look(Autopilot::Cruise, &third, 20, 2 * MIN, LONG), pace)
            )),
            Some(Rule::Continue),
            "{pace:?}"
        );
    }
}

#[test]
fn its_own_text_wrapped_or_folded_into_a_paste_chip_is_its_own_draft() {
    fn composer(rows: &[&str]) -> Reading {
        let mut screen = idle("I fixed the parser and its tests pass.");
        screen.composer = Some((3, rows.iter().map(|row| (*row).to_string()).collect()));
        screen
    }
    // Its own text wrapped over two rows, broken inside a word.
    let head: Vec<char> = CONTINUE_TEXT.chars().take(70).collect();
    let (first, second) = head.split_at(33);
    let first: String = first.iter().collect();
    let second: String = second.iter().collect();
    assert!(own_draft(
        &composer(&[first.trim(), second.trim()]),
        CONTINUE_TEXT
    ));
    // Negative control: a person's text wrapped the same way is theirs.
    assert!(!own_draft(
        &composer(&["please also look at the flaky", "test in the parser suite"]),
        CONTINUE_TEXT
    ));
    // A text of thirteen lines folded into Claude Code's paste chip.
    let long = format!("{CONTINUE_TEXT}{}", "\nand one more line".repeat(12));
    assert!(own_draft(&composer(&["[Pasted text #1 +12 lines]"]), &long));
    assert!(own_draft(&composer(&["[Pasted text #3 +13 lines]"]), &long));
    assert!(own_draft(&composer(&["[Pasted text #2]"]), &long));
    // Negative controls: a chip of another length, a chip with a person's
    // words after it, and a chip for a text too short to fold.
    assert!(!own_draft(&composer(&["[Pasted text #1 +3 lines]"]), &long));
    assert!(!own_draft(
        &composer(&["[Pasted text #1 +12 lines] and the tests too"]),
        &long
    ));
    assert!(!own_draft(
        &composer(&["[Pasted text #1 +0 lines]"]),
        "keep going"
    ));
}
