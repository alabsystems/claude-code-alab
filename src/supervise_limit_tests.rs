// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The supervisor's own model move for Fable's limit, up to the signal: what
//! it journals, what holds it, what it keeps, and the restart request that
//! names the model the child already runs on. The moves themselves are made
//! end to end in `tests/harness.rs` (`a_fable_limit_…`).

use super::model_tests::{SID, events, launched, onto, scratch};
use super::tests::ask;
use super::*;

/// A witness that reports the same thing at every look.
struct Fixed(limit::Sight);

impl limit::Source for Fixed {
    fn look(&mut self, _: &limit::At<'_>) -> limit::Sight {
        self.0.clone()
    }
}

impl Supervisor {
    /// [`Supervisor::limit_look`] on what the witness sees now, as a look
    /// reads it once for the whole look.
    fn limit_look_now(
        &mut self,
        record: Option<&SessionRecord>,
        gate: Result<(), Wait>,
        input: Option<u64>,
        now: u64,
    ) -> bool {
        let sight = self.limit_sight();
        self.limit_look(&sight, record, gate, input, now)
    }
}

/// A look at which nobody typed: no input clock, and the tracker passes.
const QUIET: Typed = Typed {
    gate: Ok(()),
    input: None,
    taken_ms: None,
};

/// A limit of `bucket` the child was refused at `at_ms`, resetting at
/// `resets_at_s`.
fn wall(bucket: &str, at_ms: u64, resets_at_s: Option<u64>) -> limit::Wall {
    limit::Wall {
        at_ms,
        resets_at_s,
        bucket: Some(bucket.to_string()),
    }
}

/// `sup` sees `wall` standing from here on.
fn sees(sup: &mut Supervisor, wall: Option<limit::Wall>) {
    sup.limit_source = Box::new(Fixed(limit::Sight {
        wall,
        ..limit::Sight::default()
    }));
}

/// `sup` sees no limit and a real answer since the floor from here on.
fn sees_answer(sup: &mut Supervisor) {
    sup.limit_source = Box::new(Fixed(limit::Sight {
        answered: true,
        ..limit::Sight::default()
    }));
}

/// The child answers on `model` at `stamp`: one main-thread `assistant` row
/// appended to the conversation's transcript under `sup`'s config, which the
/// facts reader reads at its next look.
fn answers_on(sup: &Supervisor, stamp: &str, model: &str) {
    use std::io::Write as _;
    let dir = sup.config.join("projects").join("-w");
    std::fs::create_dir_all(&dir).expect("a project folder");
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join(format!("{SID}.jsonl")))
        .expect("a transcript")
        .write_all(
            format!(
                "{{\"type\":\"assistant\",\"isSidechain\":false,\"timestamp\":\"{stamp}\",\
                 \"message\":{{\"model\":\"{model}\",\"role\":\"assistant\",\"content\":[]}}}}\n"
            )
            .as_bytes(),
        )
        .expect("a row");
}

/// The journal's line for a Fable limit seen at `at_ms`.
fn seen(at_ms: u64, resets: &str, decision: &str) -> String {
    format!(
        "limit-seen bucket={} at={} resets-at={resets} decision={decision}",
        limit::FABLE_BUCKET,
        at_ms / 1_000
    )
}

#[test]
fn a_model_restart_that_names_the_model_the_child_runs_on_is_a_journalled_no_op() {
    let root = scratch("already-on");
    let (mut sup, _) = launched(&root, &["--model", "fable"]);
    // The launch's own model.
    sup.restart_asked(&onto("model", "fable"));
    assert_eq!(sup.restart_deferred, None, "nothing waits");
    assert_eq!(sup.ending, Ending::None, "nothing is ending");
    // The model a restart moved the session onto.
    sup.model_moved(limit::FALLBACK.to_string(), "model");
    sup.restart_asked(&onto("model", limit::FALLBACK));
    assert_eq!(sup.restart_deferred, None);
    // CONTROL: another model is a request (it waits here only because the
    // fixture's child has no record), and so is the launch's model once the
    // session is off it.
    sup.restart_asked(&onto("model", "sonnet"));
    assert_eq!(
        sup.restart_deferred.as_ref().map(|d| d.restart.clone()),
        Some(onto("model", "sonnet"))
    );
    // A request that waits is still owed its restart: the model named over
    // it replaces its model, even the one the child runs on.
    sup.restart_asked(&onto("model", limit::FALLBACK));
    assert_eq!(
        sup.restart_deferred.as_ref().map(|d| d.restart.clone()),
        Some(onto("model", limit::FALLBACK))
    );
    sup.restart_deferred = None;
    sup.restart_asked(&onto("model", "fable"));
    assert!(
        sup.restart_deferred.take().is_some(),
        "back onto the launch's"
    );
    // CONTROL: a restart for any other cause is made whatever model it
    // names, and one that names none is not a model move at all.
    sup.restart_asked(&onto("asked", limit::FALLBACK));
    assert!(sup.restart_deferred.take().is_some());
    sup.restart_asked(&ask("asked"));
    assert!(sup.restart_deferred.take().is_some());
    // CONTROL: a launch that names no model runs on what Claude Code's own
    // settings chose, which the harness does not know while nothing pins it
    // and the child has stated no model: the request is made.
    let (mut unnamed, _) = launched(&root, &[]);
    unnamed.restart_asked(&onto("model", "fable"));
    assert!(unnamed.restart_deferred.is_some());
    assert_eq!(
        events(&root),
        [
            "aterm restart cause=model model=fable ignored already-on",
            "model-move to=claude-opus-5-5 from=fable cause=model kept=switched back-at=-",
            "aterm restart cause=model model=claude-opus-5-5 ignored already-on",
            "aterm restart cause=model model=sonnet deferred no-record",
            "aterm restart cause=model model=claude-opus-5-5 deferred no-record",
            "aterm restart cause=model model=fable deferred no-record",
            "aterm restart cause=asked model=claude-opus-5-5 deferred no-record",
            "aterm restart cause=asked deferred no-record",
            "aterm restart cause=model model=fable deferred no-record",
        ]
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_move_onto_the_fallback_at_the_limit_is_kept_as_off_fable_whoever_made_it() {
    let root = scratch("moved");
    let now = now_ms();
    let resets = now / 1_000 + 3_600;
    let grace = Options::default().reset_grace_ms;
    let back = resets * 1_000 + grace;
    // aterm moved the session (a restart that named the model) while the
    // child stood at the limit: the harness keeps the return, so the two stay
    // in step.
    let (mut sup, _) = launched(&root, &["--model", "fable"]);
    sees(&mut sup, Some(wall(limit::FABLE_BUCKET, now, Some(resets))));
    sup.child.spawned_ms = 777;
    sup.model_moved(limit::FALLBACK.to_string(), "model");
    assert_eq!(
        sup.limit,
        limit::State {
            floor_ms: 777,
            phase: limit::Phase::OffFable { back_at_ms: back },
            walled_returns: 0,
            attempts: 0,
            retry_at_ms: 0,
        }
    );
    // Asked onto the fallback again while off Fable: only the floor moves.
    sup.child.spawned_ms = 888;
    sup.model_moved(limit::FALLBACK.to_string(), "model");
    assert_eq!(
        (sup.limit.floor_ms, sup.limit.phase),
        (888, limit::Phase::OffFable { back_at_ms: back })
    );
    // Back, by the harness or by aterm: a return that has yet to answer.
    sup.child.spawned_ms = 999;
    sup.model_moved("fable".to_string(), limit::CAUSE_RESET);
    assert_eq!(
        (sup.limit.floor_ms, sup.limit.phase, sup.model.clone()),
        (999, limit::Phase::OnLaunch { returned: true }, None)
    );
    // CONTROL: the same move with no limit standing is somebody's choice of
    // model and schedules no return.
    let (mut chosen, _) = launched(&root, &["--model", "fable"]);
    chosen.model_moved(limit::FALLBACK.to_string(), "model");
    assert_eq!(
        chosen.limit.phase,
        limit::Phase::OnLaunch { returned: false }
    );
    // CONTROL: another window's limit is not Fable's.
    sees(&mut chosen, Some(wall("five_hour", now, Some(resets))));
    chosen.model_moved(limit::FALLBACK.to_string(), "model");
    assert_eq!(
        chosen.limit.phase,
        limit::Phase::OnLaunch { returned: false }
    );
    // CONTROL: a session launched on another model, asked onto the fallback
    // while a Fable limit stands in its transcript, was never Fable's: no
    // return is owed, so it is never brought "back" onto Fable.
    let (mut other, _) = launched(&root, &["--model", "sonnet"]);
    sees(
        &mut other,
        Some(wall(limit::FABLE_BUCKET, now, Some(resets))),
    );
    other.model_moved(limit::FALLBACK.to_string(), "model");
    assert_eq!(
        other.limit.phase,
        limit::Phase::OnLaunch { returned: false }
    );
    // The harness's own move is off Fable even when no row names the limit
    // any more: it goes back up the rungs.
    let (mut own, _) = launched(&root, &[]);
    own.model_moved(limit::FALLBACK.to_string(), limit::CAUSE_LIMIT);
    let limit::Phase::OffFable { back_at_ms } = own.limit.phase else {
        panic!("not off Fable: {:?}", own.limit);
    };
    let rung = limit::RETURN_RUNGS_MS[0];
    assert!(
        (now + rung..=now + rung + 60_000).contains(&back_at_ms),
        "{back_at_ms} is not half an hour from {now}"
    );
    let lines = events(&root);
    assert_eq!(
        lines[..5],
        [
            format!(
                "model-move to=claude-opus-5-5 from=fable cause=model kept=switched back-at={}",
                back / 1_000
            ),
            format!(
                "model-move to=claude-opus-5-5 from=claude-opus-5-5 cause=model kept=switched \
                 back-at={}",
                back / 1_000
            ),
            "model-move to=fable from=claude-opus-5-5 cause=fable-reset kept=launch back-at=-"
                .to_string(),
            "model-move to=claude-opus-5-5 from=fable cause=model kept=switched back-at=-"
                .to_string(),
            "model-move to=claude-opus-5-5 from=claude-opus-5-5 cause=model kept=switched \
             back-at=-"
                .to_string(),
        ]
    );
    assert_eq!(
        lines[5],
        "model-move to=claude-opus-5-5 from=sonnet cause=model kept=switched back-at=-"
    );
    assert_eq!(
        lines[6],
        format!(
            "model-move to=claude-opus-5-5 from=- cause=fable-limit kept=switched back-at={}",
            back_at_ms / 1_000
        )
    );
    assert_eq!(lines.len(), 7);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_fable_limit_is_journalled_once_with_its_decision_and_only_a_fable_session_is_held() {
    let root = scratch("seen");
    let now = now_ms();
    // A minute old: past the quiet period, inside the limit's patience.
    let old = now - 60_000;
    let resets = now / 1_000 + 3_600;
    let fable = wall(limit::FABLE_BUCKET, old, Some(resets));
    // A session on another model stays, and its look goes on to the store.
    let (mut opus, record) = launched(&root, &["--model", "opus"]);
    sees(&mut opus, Some(fable.clone()));
    for _ in 0..3 {
        assert!(!opus.limit_look_now(Some(&record), Ok(()), None, now));
    }
    // So does a Fable session whose limit has already reset.
    let (mut past, record) = launched(&root, &["--model", "fable"]);
    sees(
        &mut past,
        Some(wall(limit::FABLE_BUCKET, old, Some(now / 1_000 - 1))),
    );
    assert!(!past.limit_look_now(Some(&record), Ok(()), None, now));
    // So does a session launched on another model that a restart has since
    // moved onto Fable: the launch's own word is kept to.
    let (mut asked, record) = launched(&root, &["--model", "sonnet"]);
    asked.model = Some("fable".to_string());
    sees(&mut asked, Some(fable.clone()));
    assert!(!asked.limit_look_now(Some(&record), Ok(()), None, now));
    // Another window's limit is nobody's to move for, and is not journalled.
    let (mut weekly, record) = launched(&root, &["--model", "fable"]);
    sees(&mut weekly, Some(wall("seven_day", old, Some(resets))));
    assert!(!weekly.limit_look_now(Some(&record), Ok(()), None, now));
    // CONTROL: the same wall on a Fable session is a move that is wanted. It
    // holds the look, and waits for its moment: the row a quiet period old,
    // a record, and the record not busy.
    let (mut sup, record) = launched(&root, &["--model", "fable"]);
    sees(&mut sup, Some(wall(limit::FABLE_BUCKET, now, Some(resets))));
    for _ in 0..3 {
        assert!(sup.limit_look_now(Some(&record), Ok(()), None, now));
    }
    sees(&mut sup, Some(fable.clone()));
    for _ in 0..3 {
        assert!(sup.limit_look_now(None, Ok(()), None, now));
    }
    let busy = SessionRecord {
        status: profile::STATUS_BUSY.to_string(),
        ..record
    };
    for _ in 0..3 {
        assert!(sup.limit_look_now(Some(&busy), Ok(()), None, now));
    }
    assert_eq!(sup.ending, Ending::None, "nothing was signalled");
    assert_eq!(sup.limit, limit::State::new(1), "and nothing is kept yet");
    // A restart aterm asked for that waits decides first.
    sup.restart_asked(&onto("model", "sonnet"));
    assert!(!sup.limit_look_now(None, Ok(()), None, now));
    let resets = resets.to_string();
    assert_eq!(
        events(&root),
        [
            seen(old, &resets, "stay model=opus"),
            seen(old, &(now / 1_000 - 1).to_string(), "stay reset-passed"),
            seen(old, &resets, "stay model=sonnet"),
            seen(now, &resets, "move to=claude-opus-5-5"),
            "limit-move wait settling".to_string(),
            seen(old, &resets, "move to=claude-opus-5-5"),
            "limit-move wait no-record".to_string(),
            "limit-move wait busy".to_string(),
            "aterm restart cause=model model=sonnet deferred no-record".to_string(),
        ]
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_move_that_failed_backs_off_and_is_not_tried_again_before_its_pause() {
    let root = scratch("backoff");
    let now = now_ms();
    let (mut sup, _) = launched(&root, &["--model", "fable"]);
    sees(
        &mut sup,
        Some(wall(
            limit::FABLE_BUCKET,
            now - 3_600_000,
            Some(now / 1_000 + 3_600),
        )),
    );
    // CONTROL: before any failure the move is wanted on every look.
    assert!(sup.limit_look_now(None, Ok(()), None, now));
    sup.limit_failed(limit::CAUSE_LIMIT, "vendor-plan gone", now);
    assert_eq!(
        (sup.limit.attempts, sup.limit.retry_at_ms),
        (1, now + 60_000)
    );
    // Inside the pause nothing is tried, noted or held, however many looks.
    for later in [now, now + 1, now + 59_999] {
        assert!(!sup.limit_look_now(None, Ok(()), None, later));
    }
    // When it is over the move is wanted again, and a second failure doubles
    // the pause.
    assert!(sup.limit_look_now(None, Ok(()), None, now + 60_000));
    sup.limit_failed(limit::CAUSE_LIMIT, "vendor-plan gone", now + 60_000);
    assert_eq!(sup.limit.retry_at_ms, now + 60_000 + 120_000);
    assert!(!sup.limit_look_now(None, Ok(()), None, now + 179_999));
    let lines = events(&root);
    assert_eq!(
        lines[1..],
        [
            "limit-move wait no-record",
            "limit-move failed cause=fable-limit vendor-plan gone attempt=1 retry-in-ms=60000",
            "limit-move wait no-record",
            "limit-move failed cause=fable-limit vendor-plan gone attempt=2 retry-in-ms=120000",
        ]
    );
    assert!(lines[0].starts_with("limit-seen "), "{lines:?}");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_session_off_fable_goes_back_when_its_time_has_come_and_not_before() {
    let root = scratch("return");
    let now = now_ms();
    let off = |sup: &mut Supervisor| {
        sup.model = Some(limit::FALLBACK.to_string());
        sup.limit.phase = limit::Phase::OffFable {
            back_at_ms: now + 1_000,
        };
    };
    let (mut sup, record) = launched(&root, &["--model", "claude-fable-5-2"]);
    off(&mut sup);
    // A Fable limit still in the transcript's tail never moves a session
    // that is on the fallback.
    sees(
        &mut sup,
        Some(wall(
            limit::FABLE_BUCKET,
            now - 5_000,
            Some(now / 1_000 + 1),
        )),
    );
    // CONTROL: before its time nothing is wanted, and the look goes on.
    for early in [now, now + 999] {
        assert!(!sup.limit_look_now(Some(&record), Err(Wait::Busy), None, early));
    }
    // Its time has come: the return is wanted, at the ordinary safe point,
    // whose first gate here is the tracker's.
    for due in [now + 1_000, now + 1_040, now + 9_000] {
        assert!(sup.limit_look_now(Some(&record), Err(Wait::Busy), None, due));
    }
    assert!(sup.limit_look_now(Some(&record), Err(Wait::Draft), None, now + 9_040));
    assert_eq!(sup.ending, Ending::None);
    // A launch that names no model is named Fable on the way back: an
    // unnamed resume would stay on the fallback. CONTROL: one that names a
    // model goes back to its own, whichever it is.
    let unnamed: &[&str] = &[];
    for args in [unnamed, &["--model", "opus"]] {
        let (mut other, record) = launched(&root, args);
        off(&mut other);
        assert!(other.limit_look_now(Some(&record), Err(Wait::Busy), None, now + 1_000));
    }
    assert_eq!(
        events(&root),
        [
            seen(
                now - 5_000,
                &(now / 1_000 + 1).to_string(),
                "stay on-fallback"
            ),
            "limit-return due to=claude-fable-5-2".to_string(),
            "limit-move wait busy".to_string(),
            "limit-move wait draft".to_string(),
            "limit-return due to=fable".to_string(),
            "limit-move wait busy".to_string(),
            "limit-return due to=opus".to_string(),
            "limit-move wait busy".to_string(),
        ]
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_due_return_holds_the_duties_only_at_the_turn_end_it_would_take() {
    let root = scratch("yields");
    let now = now_ms();
    let (mut sup, record) = launched(&root, &["--model", "fable"]);
    sup.model = Some(limit::FALLBACK.to_string());
    sup.limit.phase = limit::Phase::OffFable {
        back_at_ms: now - 1,
    };
    let at = |status: &str, ago: u64| SessionRecord {
        status: status.to_string(),
        status_updated_at_ms: now - ago,
        ..record.clone()
    };
    // A turn that ended inside the window is the return's: nothing is typed
    // into it.
    for ago in [0, 1_000, crate::quiet::TURN_END_WITHIN_MS] {
        assert!(
            !sup.limit_yields(Some(&at(profile::STATUS_IDLE, ago)), now),
            "{ago}"
        );
    }
    // Past it, during a turn, at a box and with no record the duties go on,
    // so a return that waits never parks the session.
    let past = crate::quiet::TURN_END_WITHIN_MS + 1;
    assert!(sup.limit_yields(Some(&at(profile::STATUS_IDLE, past)), now));
    for status in [profile::STATUS_BUSY, "waiting", "question"] {
        assert!(sup.limit_yields(Some(&at(status, 1_000)), now), "{status}");
    }
    assert!(sup.limit_yields(None, now));
    // CONTROL: the move off Fable at its limit keeps the whole look, and so
    // does a child that is ending.
    let (on, _) = launched(&root, &["--model", "fable"]);
    assert!(!on.limit_yields(Some(&at(profile::STATUS_BUSY, 1_000)), now));
    sup.ending = Ending::Passed(libc::SIGTERM);
    assert!(!sup.limit_yields(Some(&at(profile::STATUS_BUSY, 1_000)), now));
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn under_a_claim_the_return_is_made_at_a_turns_end_and_an_update_is_not() {
    let root = scratch("turn-end");
    let now = now_ms();
    let (sup, _) = launched(&root, &["--model", "fable"]);
    let idle = |ago: u64| Some((profile::STATUS_IDLE, now - ago));
    // A turn ended a second ago: the moment before the turn-end duty types a
    // Cruise continuation, which is the moment the return under `relaunch`
    // is made at.
    assert_eq!(sup.claimed_rest(Scope::Relaunch, idle(1_000), now), Ok(()));
    // CONTROL: a move onto another build keeps aterm's own upgrade gate,
    // the whole quiet period, on the same record.
    assert_eq!(
        sup.claimed_rest(Scope::Upgrade, idle(1_000), now),
        Err(Wait::Settling)
    );
    // Past the turn's end the return waits too, up to the quiet period; a
    // record idle that long passes both.
    assert_eq!(
        sup.claimed_rest(Scope::Relaunch, idle(5_000), now),
        Err(Wait::Settling)
    );
    for duty in [Scope::Relaunch, Scope::Upgrade] {
        assert_eq!(
            sup.claimed_rest(duty, idle(sup.setup.opts.quiet_ms), now),
            Ok(()),
            "{duty:?}"
        );
        assert_eq!(
            sup.claimed_rest(duty, Some((profile::STATUS_BUSY, now - 1_000)), now),
            Err(Wait::Busy),
            "{duty:?}"
        );
    }
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn work_still_running_at_the_limit_holds_the_move_for_its_patience_and_no_longer() {
    let root = scratch("patience");
    let now = now_ms();
    let (mut sup, record) = launched(&root, &["--model", "fable"]);
    let busy = SessionRecord {
        status: profile::STATUS_BUSY.to_string(),
        ..record.clone()
    };
    let at = |age_ms: u64| wall(limit::FABLE_BUCKET, now - age_ms, None);
    let held = Err(Some("wait busy".to_string()));
    // A record that reads busy holds the move up to the patience.
    for age in [60_000, limit::WALL_PATIENCE_MS - 1] {
        assert_eq!(
            sup.wall_point(Some(&busy), &at(age), QUIET, now),
            held,
            "{age}"
        );
    }
    // From then on it does not: the next condition decides (the terminal's
    // foreground group, which a test run may or may not be).
    for age in [limit::WALL_PATIENCE_MS, 6 * limit::WALL_PATIENCE_MS] {
        assert_ne!(
            sup.wall_point(Some(&busy), &at(age), QUIET, now),
            held,
            "{age}"
        );
    }
    // A busy run that began after the limit row is a turn of its own: its
    // patience counts from when it began, so a row a patience old does not
    // outwait a run a minute old.
    let late = SessionRecord {
        status_updated_at_ms: now - 60_000,
        ..busy.clone()
    };
    for age in [limit::WALL_PATIENCE_MS, 6 * limit::WALL_PATIENCE_MS] {
        assert_eq!(
            sup.wall_point(Some(&late), &at(age), QUIET, now),
            held,
            "{age}"
        );
    }
    // CONTROL: the same run, begun a patience ago, holds no longer.
    let long = SessionRecord {
        status_updated_at_ms: now - limit::WALL_PATIENCE_MS,
        ..busy.clone()
    };
    assert_ne!(
        sup.wall_point(Some(&long), &at(6 * limit::WALL_PATIENCE_MS), QUIET, now),
        held
    );
    // CONTROL: a record that is not busy is never held as busy, and a limit
    // row younger than the quiet period is settling whatever the record says.
    assert_ne!(sup.wall_point(Some(&record), &at(60_000), QUIET, now), held);
    assert_eq!(
        sup.wall_point(Some(&busy), &at(0), QUIET, now),
        Err(Some("wait settling".to_string()))
    );
    // What the move is made over is said once per kind of work, however
    // often a later condition is noted between two looks, and whichever job
    // the table names at each.
    for job in ["shell", "server", "shell"] {
        sup.note_limit_over("busy");
        sup.note_limit("wait aterm human_ms=1000");
        sup.note_limit_over(&format!("background-work {job}"));
        sup.note_limit("wait typing");
    }
    // CONTROL: a completed move clears it, and the next limit says it again.
    sup.model_moved("fable".to_string(), limit::CAUSE_RESET);
    sup.note_limit_over("busy");
    assert_eq!(
        events(&root),
        [
            "limit-move over busy patience-ms=600000",
            "limit-move wait aterm human_ms=1000",
            "limit-move over background-work shell patience-ms=600000",
            "limit-move wait typing",
            "limit-move wait aterm human_ms=1000",
            "limit-move wait typing",
            "limit-move wait aterm human_ms=1000",
            "limit-move wait typing",
            "model-move to=fable from=fable cause=fable-reset kept=launch back-at=-",
            "limit-move over busy patience-ms=600000",
        ]
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn without_a_claim_typing_nothing_shows_was_sent_holds_the_move_at_the_limit() {
    let root = scratch("typed");
    let now = now_ms();
    let (mut sup, _) = launched(&root, &["--model", "fable"]);
    // A limit row two minutes old; the child was spawned long before it.
    let at = now - 120_000;
    let typed = |gate: Result<(), Wait>, input: Option<u64>, taken_ms: Option<u64>| Typed {
        gate,
        input,
        taken_ms,
    };
    let draft = Err("wait draft".to_string());
    // A key read after the limit row (the dialog closed, words typed into
    // the composer) that no later prompt shows was sent holds, however long
    // ago it was read and whatever the record says.
    for taken in [None, Some(at), Some(at + 999)] {
        assert_eq!(
            sup.wall_typed(at, typed(Err(Wait::Busy), Some(at + 1_000), taken), now),
            draft,
            "{taken:?}"
        );
    }
    // The tracker's draft rule holds whatever the transcript shows, and so
    // does input read inside the quiet period.
    assert_eq!(
        sup.wall_typed(
            at,
            typed(Err(Wait::Draft), Some(at - 60_000), Some(now)),
            now
        ),
        draft
    );
    assert_eq!(
        sup.wall_typed(at, typed(Ok(()), Some(now - 1), Some(now)), now),
        Err("wait typing".to_string())
    );
    // CONTROLS: each goes on to the input queue, the last condition, which
    // a test's stdin may or may not pass. The same key that a prompt sent at
    // or after it shows was taken; input read before the limit row; no
    // input clock; the tracker's other waits, which the limit judges itself.
    let passed = |verdict: Result<(), String>| {
        verdict.is_ok() || verdict.is_err_and(|wait| wait.starts_with("wait unread-input"))
    };
    for (gate, input, taken) in [
        (Err(Wait::Busy), Some(at + 1_000), Some(at + 1_000)),
        (Err(Wait::Busy), Some(at + 1_000), Some(at + 5_000)),
        (Ok(()), Some(at - 1), None),
        (Err(Wait::Settling), Some(at), None),
        (Err(Wait::NoRecord), None, None),
    ] {
        let verdict = sup.wall_typed(at, typed(gate, input, taken), now);
        assert!(
            passed(verdict.clone()),
            "{gate:?} {input:?} {taken:?}: {verdict:?}"
        );
    }
    // CONTROL: a key the previous child read, before this one was spawned,
    // is no draft of this one's.
    sup.child.spawned_ms = at + 2_000;
    assert!(passed(sup.wall_typed(
        at,
        typed(Err(Wait::Busy), Some(at + 1_000), None),
        now
    )));
    let _ = std::fs::remove_dir_all(&root);
}

/// The words of a resume line.
fn words(line: &[&str]) -> Vec<String> {
    line.iter().map(|word| (*word).to_string()).collect()
}

#[test]
fn the_return_at_the_reset_is_the_restart_aterm_asks_for_with_model_launch() {
    let root = scratch("return-word");
    let back = Restart {
        cause: limit::CAUSE_RESET.to_string(),
        model: Some(MODEL_LAUNCH.to_string()),
        account: None,
    };
    // The commonest launch names no model. The return's own line names the
    // alias, since a line that names none would stay on the fallback.
    let (mut unnamed, record) = launched(&root, &["--dangerously-skip-permissions"]);
    unnamed.model_moved(limit::FALLBACK.to_string(), limit::CAUSE_LIMIT);
    assert_eq!(unnamed.model.as_deref(), Some(limit::FALLBACK));
    assert_eq!(unnamed.model_for(&back).as_deref(), Some(limit::FABLE));
    assert_eq!(unnamed.shown_model(MODEL_LAUNCH), limit::FABLE);
    assert_eq!(
        unnamed
            .resume_plan_on(&record, unnamed.model_for(&back).as_deref())
            .unwrap(),
        words(&[
            "--dangerously-skip-permissions",
            "--model",
            "fable",
            "--resume",
            SID
        ])
    );
    // Back: nothing is kept, and the session is on its launch's terms again,
    // a return yet to answer. Until it answers, the conversation's newest
    // model is still the fallback, so a vendor update, a crash relaunch or a
    // plain restart made now names the alias as the return did.
    unnamed.model_moved(MODEL_LAUNCH.to_string(), limit::CAUSE_RESET);
    assert_eq!(unnamed.model, None);
    assert_eq!(
        unnamed.limit.phase,
        limit::Phase::OnLaunch { returned: true }
    );
    let on_fable = words(&[
        "--dangerously-skip-permissions",
        "--model",
        "fable",
        "--resume",
        SID,
    ]);
    assert_eq!(unnamed.resume_plan(&record).unwrap(), on_fable);
    assert_eq!(
        unnamed.model_for(&ask("stall")).as_deref(),
        Some(limit::FABLE)
    );
    // The child answers on Fable (a real answer since the return's floor):
    // from then on every line is the launch's own, which names no model.
    sees_answer(&mut unnamed);
    assert!(!unnamed.limit_look_now(Some(&record), Ok(()), None, now_ms()));
    assert_eq!(
        unnamed.limit.phase,
        limit::Phase::OnLaunch { returned: false }
    );
    assert_eq!(
        unnamed.resume_plan(&record).unwrap(),
        words(&["--dangerously-skip-permissions", "--resume", SID])
    );
    assert_eq!(unnamed.model_for(&ask("stall")), None);
    // aterm's own `model=launch` now asks for what the child runs on.
    unnamed.restart_asked(&onto("model", MODEL_LAUNCH));
    assert_eq!(unnamed.restart_deferred, None);
    // CONTROL: a launch that named its Fable goes back on its own line, the
    // same way, and the word is never kept as a model.
    let (mut named, record) = launched(&root, &["--model", "claude-fable-5-2"]);
    named.model_moved(limit::FALLBACK.to_string(), limit::CAUSE_LIMIT);
    assert_eq!(named.model_for(&back), None);
    assert_eq!(named.shown_model(MODEL_LAUNCH), "claude-fable-5-2");
    named.model_moved(MODEL_LAUNCH.to_string(), limit::CAUSE_RESET);
    assert_eq!(named.model, None);
    assert_eq!(
        named.resume_plan(&record).unwrap(),
        words(&["--model", "claude-fable-5-2", "--resume", SID])
    );
    // CONTROL: while the fallback is kept, `model=launch` is a request.
    named.model_moved(limit::FALLBACK.to_string(), "model");
    named.restart_asked(&onto("model", MODEL_LAUNCH));
    assert_eq!(
        named.restart_deferred.as_ref().map(|d| d.restart.clone()),
        Some(onto("model", MODEL_LAUNCH))
    );
    let lines = events(&root);
    assert_eq!(
        [lines[1].as_str(), lines[2].as_str(), lines[4].as_str()],
        [
            "model-move to=launch launch-model=- from=claude-opus-5-5 cause=fable-reset \
             kept=launch back-at=-",
            "aterm restart cause=model model=launch ignored already-on",
            "model-move to=launch launch-model=claude-fable-5-2 from=claude-opus-5-5 \
             cause=fable-reset kept=launch back-at=-",
        ]
    );
    assert_eq!(
        lines[6],
        "aterm restart cause=model model=launch deferred no-record"
    );
    assert_eq!(lines.len(), 7, "{lines:?}");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn aterms_alias_for_the_move_back_is_already_on_while_the_child_runs_on_fable() {
    let root = scratch("alias-back");
    let fable_words = || {
        events(&root)
            .into_iter()
            .filter(|line| !line.starts_with("facts "))
            .collect::<Vec<_>>()
    };
    // A launch that named no model, back on it after Fable's limit and not
    // yet answered there: the alias aterm sends for the move back (an aterm
    // that predates `model=launch`) asks for what the child runs on.
    let (mut sup, record) = launched(&root, &["--dangerously-skip-permissions"]);
    sup.model_moved(limit::FALLBACK.to_string(), limit::CAUSE_LIMIT);
    sup.model_moved(MODEL_LAUNCH.to_string(), limit::CAUSE_RESET);
    for word in ["fable", "Fable"] {
        sup.restart_asked(&onto("model", word));
        assert_eq!(sup.restart_deferred, None, "{word}");
    }
    // Answered on Fable: nothing is pinned any more, and the child's own rows
    // say it runs on Fable, so the alias still asks for nothing new.
    sup.limit.answered();
    assert_eq!(sup.launch_pin(), None);
    answers_on(&sup, "2026-11-15T17:01:00.000Z", "claude-fable-5-2");
    sup.read_facts(Some(&record), now_ms());
    assert_eq!(
        sup.facts.facts().model,
        model_facts::Said::Is("Fable 5.2".to_string()),
        "the fixture states the model"
    );
    sup.restart_asked(&onto("model", "fable"));
    assert_eq!(sup.restart_deferred, None);
    // CONTROL: another Fable word is a choice of its own, and is made.
    for word in ["claude-fable-5-2", "fable[1m]"] {
        sup.restart_asked(&onto("model", word));
        assert!(sup.restart_deferred.take().is_some(), "{word}");
    }
    // CONTROL: the child's newest rows say another model: the alias is a
    // move, and is made.
    answers_on(&sup, "2026-11-15T17:02:00.000Z", "claude-opus-5-5");
    sup.read_facts(Some(&record), now_ms());
    sup.restart_asked(&onto("model", "fable"));
    assert!(sup.restart_deferred.take().is_some());
    // CONTROL: a launch that named another model: the alias is that model's
    // replacement by name, whatever the child states.
    let named_root = scratch("alias-back-named");
    let (mut named, record) = launched(&named_root, &["--model", "sonnet"]);
    answers_on(&named, "2026-11-15T17:01:00.000Z", "claude-fable-5-2");
    named.read_facts(Some(&record), now_ms());
    named.restart_asked(&onto("model", "fable"));
    assert!(named.restart_deferred.take().is_some());
    assert_eq!(
        fable_words()[2..],
        [
            "aterm restart cause=model model=fable ignored already-on",
            "aterm restart cause=model model=Fable ignored already-on",
            "aterm restart cause=model model=fable ignored already-on",
            "aterm restart cause=model model=claude-fable-5-2 deferred no-record",
            "aterm restart cause=model model=fable[1m] deferred no-record",
            "aterm restart cause=model model=fable deferred no-record",
        ]
    );
    let _ = std::fs::remove_dir_all(&root);
    let _ = std::fs::remove_dir_all(&named_root);
}

/// `sup` sees a model chosen inside the session from here on, named by
/// `word` where its rows give one.
fn sees_choice(sup: &mut Supervisor, word: Option<&str>) {
    sup.limit_source = Box::new(Fixed(limit::Sight {
        chose: Some(limit::Choice {
            at_ms: now_ms(),
            word: word.map(str::to_string),
        }),
        ..limit::Sight::default()
    }));
}

#[test]
fn a_model_a_person_chose_off_fable_gives_the_return_up_and_is_kept() {
    let root = scratch("chose");
    let now = now_ms();
    let off = |sup: &mut Supervisor| {
        sup.model_moved(limit::FALLBACK.to_string(), limit::CAUSE_LIMIT);
        assert!(matches!(sup.limit.phase, limit::Phase::OffFable { .. }));
    };
    // Moved to Opus at the limit, the person runs `/model sonnet`: no return
    // is owed, and every resume line names Sonnet, so neither a vendor
    // update nor a crash relaunch puts the session back on Opus.
    let (mut sup, record) = launched(&root, &["--model", "fable"]);
    off(&mut sup);
    sees_choice(&mut sup, Some("sonnet"));
    assert!(!sup.limit_look_now(Some(&record), Ok(()), None, now));
    assert_eq!(sup.limit.phase, limit::Phase::OnLaunch { returned: false });
    assert_eq!(sup.model.as_deref(), Some("sonnet"));
    assert_eq!(
        sup.resume_plan(&record).unwrap(),
        words(&["--model", "sonnet", "--resume", SID])
    );
    // Long after the reset nothing is due, and a second look says nothing.
    let later = now + 30 * 86_400_000;
    assert!(!sup.limit_look_now(Some(&record), Ok(()), None, later));
    // A choice made in the picker names no word: nothing is kept, and a
    // launch that named no model resumes on the conversation's newest.
    let (mut picked, record) = launched(&root, &["--dangerously-skip-permissions"]);
    off(&mut picked);
    sees_choice(&mut picked, None);
    assert!(!picked.limit_look_now(Some(&record), Ok(()), None, now));
    assert_eq!(picked.model, None);
    assert_eq!(
        picked.resume_plan(&record).unwrap(),
        words(&["--dangerously-skip-permissions", "--resume", SID])
    );
    // Moved back by hand onto the launch's own model: nothing to keep.
    let (mut back, record) = launched(&root, &["--model", "fable"]);
    off(&mut back);
    sees_choice(&mut back, Some("fable"));
    assert!(!back.limit_look_now(Some(&record), Ok(()), None, now));
    assert_eq!(back.model, None);
    assert_eq!(back.limit.phase, limit::Phase::OnLaunch { returned: false });
    // CONTROL: on its launch's terms a choice changes nothing here (its
    // relaunches already follow what the person chose), and off Fable with
    // no choice the return stands.
    let (mut on, record) = launched(&root, &["--model", "fable"]);
    sees_choice(&mut on, Some("sonnet"));
    assert!(!on.limit_look_now(Some(&record), Ok(()), None, now));
    assert_eq!(on.model, None);
    let (mut away, record) = launched(&root, &["--model", "fable"]);
    off(&mut away);
    sees(&mut away, None);
    assert!(!away.limit_look_now(Some(&record), Ok(()), None, now));
    assert!(matches!(away.limit.phase, limit::Phase::OffFable { .. }));
    assert_eq!(away.model.as_deref(), Some(limit::FALLBACK));
    let given_up: Vec<String> = events(&root)
        .into_iter()
        .filter(|line| line.starts_with("limit-move given-up "))
        .collect();
    assert_eq!(
        given_up,
        [
            "limit-move given-up chose=sonnet",
            "limit-move given-up chose=-",
            "limit-move given-up chose=fable",
        ]
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_new_process_takes_up_a_session_kept_off_fable_where_its_launch_names_no_model() {
    let root = scratch("adopt");
    let now = now_ms();
    let unnamed: &[&str] = &["--dangerously-skip-permissions"];
    let path =
        limit::kept_path(&root.join(profile::REPO).join("journal.log"), SID).expect("a kept path");
    // The process before: moved off Fable at the limit; its relaunch keeps
    // that beside the journal.
    let (mut before, _) = launched(&root, unnamed);
    before.model_moved(limit::FALLBACK.to_string(), limit::CAUSE_LIMIT);
    before.limit_keep(SID);
    let limit::Phase::OffFable { back_at_ms } = before.limit.phase else {
        panic!("off Fable: {:?}", before.limit.phase);
    };
    assert_eq!(
        limit::kept(&path),
        Some(limit::Kept {
            back_at_ms,
            walled_returns: 0
        })
    );
    // A new process on the same conversation (a tab restored after a
    // restart of the machine): no record, nothing taken up yet.
    let (mut after, record) = launched(&root, unnamed);
    sees(&mut after, None);
    assert!(!after.limit_look_now(None, Ok(()), None, now));
    assert!(!after.limit_adopted);
    // Its first look with a record takes the session up as it was.
    assert!(!after.limit_look_now(Some(&record), Ok(()), None, now));
    assert_eq!(after.limit.phase, limit::Phase::OffFable { back_at_ms });
    assert_eq!(after.model.as_deref(), Some(limit::FALLBACK));
    // So at its time the return is due, onto Fable.
    assert!(after.limit_look_now(Some(&record), Err(Wait::Busy), None, back_at_ms));
    // CONTROLS, while the state is still kept: a launch that names a model
    // runs on it, not on the fallback; a process that has moved the session
    // itself keeps its own.
    let (mut named, record) = launched(&root, &["--model", "fable"]);
    assert!(!named.limit_look_now(Some(&record), Ok(()), None, now));
    assert!(named.limit_adopted);
    assert_eq!(
        named.limit.phase,
        limit::Phase::OnLaunch { returned: false }
    );
    assert_eq!(named.model, None);
    let (mut asked, record) = launched(&root, unnamed);
    asked.model = Some("sonnet".to_string());
    assert!(!asked.limit_look_now(Some(&record), Ok(()), None, now));
    assert_eq!(
        asked.limit.phase,
        limit::Phase::OnLaunch { returned: false }
    );
    // Back on Fable: nothing is kept any more, and a new process takes up
    // nothing.
    after.model_moved(MODEL_LAUNCH.to_string(), limit::CAUSE_RESET);
    after.limit_keep(SID);
    assert_eq!(limit::kept(&path), None);
    let (mut later, record) = launched(&root, unnamed);
    assert!(!later.limit_look_now(Some(&record), Ok(()), None, now));
    assert_eq!(
        later.limit.phase,
        limit::Phase::OnLaunch { returned: false }
    );
    // CONTROL: a state whose time to go back is more than the longest stay
    // away past is removed, not taken up.
    let stale = limit::Kept {
        back_at_ms: now - limit::MAX_AWAY_MS - 1,
        walled_returns: 1,
    };
    limit::keep(&path, &stale).expect("kept");
    let (mut old, record) = launched(&root, unnamed);
    assert!(!old.limit_look_now(Some(&record), Ok(()), None, now));
    assert_eq!(old.limit.phase, limit::Phase::OnLaunch { returned: false });
    assert_eq!(limit::kept(&path), None);
    let adopted: Vec<String> = events(&root)
        .into_iter()
        .filter(|line| line.starts_with("limit-adopted "))
        .collect();
    assert_eq!(
        adopted,
        [format!(
            "limit-adopted back-at={} walled-returns=0",
            back_at_ms / 1_000
        )]
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn the_words_aterm_relaunches_a_session_off_fable_with_let_it_go_back() {
    let root = scratch("off-fable-words");
    let now = now_ms();
    let resumed = |words: &[&str]| {
        words
            .iter()
            .map(|word| (*word).to_string())
            .chain(["--resume".to_string(), SID.to_string()])
            .collect::<Vec<_>>()
    };
    // The commonest launch names no model; another names Fable's by id.
    let unnamed: &[&str] = &["--dangerously-skip-permissions"];
    let named: &[&str] = &[
        "--model",
        "claude-fable-5-2",
        "--dangerously-skip-permissions",
    ];
    for (launch, back) in [
        (
            unnamed,
            resumed(&["--dangerously-skip-permissions", "--model", "fable"]),
        ),
        (named, resumed(named)),
    ] {
        let (mut before, record) = launched(&root, launch);
        before.model_moved(limit::FALLBACK.to_string(), limit::CAUSE_LIMIT);
        before.limit_keep(SID);
        let limit::Phase::OffFable { back_at_ms } = before.limit.phase else {
            panic!("off Fable: {:?}", before.limit.phase);
        };
        // Off Fable, the words a `session` record gives aterm name no model:
        // the conversation's newest is the fallback.
        let words = before.resume_words(&record).expect("words");
        assert_eq!(words, resumed(unnamed), "{launch:?}");
        // The harness is gone and aterm types them: the new process takes the
        // session up off Fable, and so goes back at the reset.
        let words: Vec<&str> = words.iter().map(String::as_str).collect();
        let (mut after, relaunched) = launched(&root, &words);
        assert!(!after.limit_look_now(Some(&relaunched), Ok(()), None, now));
        assert_eq!(
            after.limit.phase,
            limit::Phase::OffFable { back_at_ms },
            "{launch:?}"
        );
        assert_eq!(after.model.as_deref(), Some(limit::FALLBACK));
        assert!(after.limit_look_now(Some(&relaunched), Err(Wait::Busy), None, back_at_ms));
        // CONTROL: back on its launch's model, the words name it again, the
        // alias for a launch that named none.
        before.model_moved(MODEL_LAUNCH.to_string(), limit::CAUSE_RESET);
        before.limit_keep(SID);
        assert_eq!(before.resume_words(&record).expect("words"), back);
    }
    // CONTROL: a session moved onto the fallback by aterm's own choice is not
    // off Fable: its words name the model it was moved onto.
    let (mut chosen, record) = launched(&root, unnamed);
    chosen.model_moved(limit::FALLBACK.to_string(), "model");
    assert_eq!(
        chosen.limit.phase,
        limit::Phase::OnLaunch { returned: false }
    );
    assert_eq!(
        chosen.resume_words(&record).expect("words"),
        resumed(&["--dangerously-skip-permissions", "--model", limit::FALLBACK])
    );
    let _ = std::fs::remove_dir_all(&root);
}
