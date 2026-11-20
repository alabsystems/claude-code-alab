// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Land's repository read beside the loop: the turn end waits while it runs,
//! takes its reading once it answers, and reads again later after a cut,
//! never acting on one.

use std::os::unix::fs::DirBuilderExt as _;
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use super::super::model_tests::events;
use super::*;

fn root() -> PathBuf {
    static SERIAL: AtomicU64 = AtomicU64::new(0);
    let parent = std::env::temp_dir().join(".ubd");
    std::fs::create_dir_all(&parent).unwrap();
    let root = parent.join(format!(
        "claude-duty-repo-{}-{}-{}.noindex",
        std::process::id(),
        now_ms(),
        SERIAL.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(&root)
        .unwrap();
    root
}

fn probing(script: &str) -> Probing {
    let mut command = Command::new("/bin/sh");
    command.args(["-c", script]);
    Probing::of_command(&mut command).expect("started")
}

/// The reading `supervisor`'s repository look takes within a minute, looking
/// every 10 ms from `now`; each look that has none must wait for the probe.
fn reading(supervisor: &mut Supervisor, now: u64) -> RepoRead {
    let started = Instant::now();
    while started.elapsed() < Duration::from_secs(60) {
        if let Some(read) = supervisor.repo_look(now) {
            return read;
        }
        assert!(supervisor.turn_end.probing(), "the probe runs");
        assert_eq!(supervisor.turn_end.due_ms, now + REPO_POLL_MS);
        std::thread::sleep(Duration::from_millis(10));
    }
    panic!("the probe did not answer within a minute");
}

#[test]
fn the_turn_end_waits_for_the_probe_and_a_cut_one_is_read_again_later() {
    let root = root();
    let mut supervisor = super::super::tests::supervisor_of_an_unsignallable_child(&root);
    let now = 1_000_000;
    supervisor.turn_end.repo = RepoLook::Probing(probing("sleep 0.2; echo clean"));
    assert_eq!(reading(&mut supervisor, now), RepoRead::Clean);
    assert!(!supervisor.turn_end.probing());
    // A cut probe is no reading: nothing until REPO_RETRY_MS later.
    supervisor.turn_end.repo = RepoLook::Probing(probing("exit 1"));
    let started = Instant::now();
    while supervisor.turn_end.probing() && started.elapsed() < Duration::from_secs(60) {
        assert_eq!(supervisor.repo_look(now), None);
        std::thread::sleep(Duration::from_millis(10));
    }
    let again = now + REPO_RETRY_MS;
    assert!(matches!(supervisor.turn_end.repo, RepoLook::After(at) if at == again));
    assert_eq!(supervisor.turn_end.due_ms, again);
    assert_eq!(supervisor.repo_look(again - 1), None);
    assert_eq!(supervisor.turn_end.due_ms, again);
    // At its time a new probe is asked for. This supervisor knows no folder
    // and no transcript: nothing to read, which is unread, never clean.
    assert_eq!(supervisor.repo_look(again), Some(RepoRead::Unread));
    // A folder known, but no executable to run the probe as: no reading,
    // and read again later.
    supervisor.cwd = Some(root.clone());
    assert_eq!(supervisor.repo_look(again), None);
    let said = events(&root);
    for line in [
        "duty turn-end repo clean".to_string(),
        format!("duty turn-end repo cut again-at={again}"),
        "duty turn-end repo unread".to_string(),
        format!(
            "duty turn-end repo unstarted own-exe-unknown again-at={}",
            again + REPO_RETRY_MS
        ),
    ] {
        assert!(said.contains(&line), "{line}: {said:?}");
    }
    // Negative control: a look that weighs no `LANDED` drops a probe that
    // runs, killing it, so the turn end never waits on a stale one.
    supervisor.turn_end.repo = RepoLook::Probing(probing("sleep 30"));
    assert!(supervisor.turn_end.probing());
    supervisor.turn_end.repo_stop();
    assert!(!supervisor.turn_end.probing());
    let _ = std::fs::remove_dir_all(&root);
}

/// Run a probe that is cut on `supervisor`'s look at `now` to its end.
fn cut_once(supervisor: &mut Supervisor, now: u64) {
    supervisor.turn_end.repo = RepoLook::Probing(probing("exit 1"));
    let started = Instant::now();
    while supervisor.turn_end.probing() && started.elapsed() < Duration::from_secs(60) {
        assert_eq!(supervisor.repo_look(now), None);
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(!supervisor.turn_end.probing(), "the probe ended");
}

/// Probes cut again and again for one `LANDED` never let Land wait unbounded
/// and unreported: the bound's last leaves its rest to the person, with the
/// reason, and starts no probe after it.
#[test]
fn probes_cut_for_one_landed_leave_lands_rest_to_the_person_at_the_bound() {
    let root = root();
    let mut supervisor = super::super::tests::supervisor_of_an_unsignallable_child(&root);
    supervisor.turn_end.carried.land = LandState::Checking;
    let mut now = 1_000_000;
    for cut in 1..REPO_CUTS {
        cut_once(&mut supervisor, now);
        assert!(
            matches!(supervisor.turn_end.repo, RepoLook::After(_)),
            "cut {cut} is read again later"
        );
        assert_eq!(supervisor.turn_end.carried.land, LandState::Checking);
        now += REPO_RETRY_MS;
    }
    cut_once(&mut supervisor, now);
    assert!(matches!(supervisor.turn_end.repo, RepoLook::Idle));
    assert_eq!(supervisor.turn_end.carried.land, LandState::Theirs);
    let said = events(&root);
    let left = format!(
        "duty turn-end repo cut left-to-person cuts={REPO_CUTS}: no probe of the session's \
         repositories ended within its bounds"
    );
    assert_eq!(
        said.iter().filter(|line| **line == left).count(),
        1,
        "{said:?}"
    );
    // Negative control: a reading between cuts counts them afresh, and the
    // same number of cuts after it is read again later.
    supervisor.turn_end.carried.land = LandState::Checking;
    for _ in 1..REPO_CUTS {
        cut_once(&mut supervisor, now);
    }
    supervisor.turn_end.repo = RepoLook::Probing(probing("echo clean"));
    assert_eq!(reading(&mut supervisor, now), RepoRead::Clean);
    for _ in 1..REPO_CUTS {
        cut_once(&mut supervisor, now);
        assert!(matches!(supervisor.turn_end.repo, RepoLook::After(_)));
    }
    assert_eq!(supervisor.turn_end.carried.land, LandState::Checking);
    let _ = std::fs::remove_dir_all(&root);
}

/// Readings that are unread again and again for Land's task never keep Land
/// nudging for the session's life: the bound's last leaves its rest to the
/// person, with the reason, whether the reading came from a probe or from a
/// request that could read nothing. The run outlives the nudge between two
/// checks of one task and ends at a reading git made, or once Land carries
/// no check.
#[test]
fn unread_readings_for_one_task_leave_lands_rest_to_the_person_at_the_bound() {
    let root = root();
    let mut supervisor = super::super::tests::supervisor_of_an_unsignallable_child(&root);
    supervisor.turn_end.carried.land = LandState::Checking;
    let now = 1_000_000;
    // The first reading is a probe's; this supervisor knows no folder and no
    // transcript, so every later one is the request's own unread.
    supervisor.turn_end.repo = RepoLook::Probing(probing("echo unread"));
    assert_eq!(reading(&mut supervisor, now), RepoRead::Unread);
    for _ in 2..REPO_UNREADS {
        // The nudge was typed, and the agent claimed done again: a look in
        // between weighs no `LANDED`, and the run counts on.
        supervisor.turn_end.carried.land = LandState::Told;
        supervisor.turn_end.repo_idle();
        supervisor.turn_end.carried.land = LandState::Checking;
        assert_eq!(supervisor.repo_look(now), Some(RepoRead::Unread));
        assert_eq!(supervisor.turn_end.carried.land, LandState::Checking);
    }
    assert_eq!(supervisor.repo_look(now), None, "the bound weighs nothing");
    assert_eq!(supervisor.turn_end.carried.land, LandState::Theirs);
    assert_eq!(supervisor.turn_end.due_ms, now);
    let left = format!(
        "duty turn-end repo unread left-to-person unread={REPO_UNREADS}: what the session's \
         repositories hold was not known at {REPO_UNREADS} readings in a row"
    );
    let said = events(&root);
    assert_eq!(
        said.iter().filter(|line| **line == left).count(),
        1,
        "{said:?}"
    );
    // Negative control: a reading git made between them counts the run
    // afresh, and as many unread ones after it are each Land's to weigh.
    supervisor.turn_end.carried.land = LandState::Checking;
    for _ in 1..REPO_UNREADS {
        assert_eq!(supervisor.repo_look(now), Some(RepoRead::Unread));
    }
    supervisor.turn_end.repo = RepoLook::Probing(probing("echo clean"));
    assert_eq!(reading(&mut supervisor, now), RepoRead::Clean);
    for _ in 1..REPO_UNREADS {
        assert_eq!(supervisor.repo_look(now), Some(RepoRead::Unread));
    }
    assert_eq!(supervisor.turn_end.carried.land, LandState::Checking);
    // Negative control: a new task (Land's instruction owed again) counts
    // the run afresh too.
    supervisor.turn_end.carried.land = LandState::Untold;
    supervisor.turn_end.repo_idle();
    supervisor.turn_end.carried.land = LandState::Checking;
    for _ in 1..REPO_UNREADS {
        assert_eq!(supervisor.repo_look(now), Some(RepoRead::Unread));
    }
    assert_eq!(supervisor.turn_end.carried.land, LandState::Checking);
    assert_eq!(
        events(&root).iter().filter(|line| **line == left).count(),
        1
    );
    let _ = std::fs::remove_dir_all(&root);
}

/// A `HOME` that is unset or relative leaves git unasked: unread, which
/// gets the ask to show the status, never a clean reading Land would rest
/// on.
#[test]
fn a_home_unset_or_relative_is_unread_and_never_clean() {
    assert_eq!(repo_home(None), Err(RepoRead::Unread));
    assert_eq!(
        repo_home(Some(OsString::from("relative/home"))),
        Err(RepoRead::Unread)
    );
    // Negative control: an absolute one is the probe's home.
    assert_eq!(
        repo_home(Some(OsString::from("/homes/someone"))),
        Ok(PathBuf::from("/homes/someone"))
    );
}
