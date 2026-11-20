// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The probe's process: the words that start it, and the loop's look at it,
//! which never waits and never leaves it behind.

use super::*;
use crate::repo::RepoRead;

/// The hang detector for what must happen.
const MINUTE: Duration = Duration::from_secs(60);

fn os(words: &[&str]) -> Vec<OsString> {
    words.iter().map(OsString::from).collect()
}

fn sh(script: &str) -> Command {
    let mut command = Command::new("/bin/sh");
    command.args(["-c", script]);
    command
}

/// What `probing` answers, looked at every 10 ms as the loop would.
fn answer(probing: &mut Probing) -> Probed {
    let started = Instant::now();
    while started.elapsed() < MINUTE {
        if let Some(probed) = probing.poll() {
            return probed;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    panic!("the probe did not answer within a minute");
}

#[test]
fn the_probe_is_started_only_by_its_own_words_in_their_grammar() {
    assert_eq!(entry(&os(&["--alab-leads=claude"])), None);
    assert_eq!(entry(&os(&[])), None);
    assert_eq!(
        entry(&os(&[ENTRY, "--home", "/h", "--", "/a", "/b"])),
        Some(Ok(Request {
            home: "/h".into(),
            transcript: None,
            folders: vec!["/a".into(), "/b".into()],
        }))
    );
    assert_eq!(
        entry(&os(&[
            ENTRY,
            "--home",
            "/h",
            "--transcript",
            "/t.jsonl",
            "--"
        ])),
        Some(Ok(Request {
            home: "/h".into(),
            transcript: Some("/t.jsonl".into()),
            folders: Vec::new(),
        }))
    );
    // Negative controls: anything else is a usage error, never a probe.
    for words in [
        &[ENTRY][..],
        &[ENTRY, "--home", "h", "--"],
        &[ENTRY, "--home", "/h"],
        &[ENTRY, "--home", "/h", "--transcript", "/t"],
        &[ENTRY, "--home", "/h", "--transcript", "t", "--"],
        &[ENTRY, "--home", "/h", "--", "relative"],
        &[ENTRY, "--transcript", "/t", "--home", "/h", "--"],
    ] {
        assert!(matches!(entry(&os(words)), Some(Err(_))), "{words:?}");
    }
}

#[test]
fn the_loop_takes_the_probes_one_line_and_any_other_end_is_cut() {
    let mut clean = Probing::of_command(&mut sh("sleep 0.2; echo clean")).expect("started");
    assert_eq!(answer(&mut clean), Probed::Read(RepoRead::Clean));
    let line = "found\t/w\t0\t1\t0\t0\t0\t\tnew.txt";
    let mut found = Probing::of_command(&mut sh(&format!("printf '{line}\\n'"))).expect("started");
    match answer(&mut found) {
        Probed::Read(RepoRead::Found(work)) => assert_eq!(work.paths, ["new.txt"]),
        other => panic!("{other:?}"),
    }
    // Negative controls: a line not in the grammar, a failed exit, and a
    // signal are no reading.
    for script in ["echo maybe", "echo clean; exit 3", "kill -9 $$"] {
        let mut odd = Probing::of_command(&mut sh(script)).expect("started");
        assert_eq!(answer(&mut odd), Probed::Cut, "{script}");
    }
    assert!(Probing::of_command(&mut Command::new("/nonexistent/claude-alab-probe")).is_err());
}

#[test]
fn a_probe_dropped_before_its_end_leaves_nothing_running() {
    // A name no other test's scratch has: `repo::tests`' `scratch("probe")`
    // made `claude-alab-repo-probe-<pid>-<ms>` too, and in the millisecond
    // both began this test's removal took that one's folder from under it.
    let dir = std::env::temp_dir().join(".ubd").join(format!(
        "claude-alab-repo-probe-dropped-{}-{}.noindex",
        std::process::id(),
        crate::supervise::now_ms()
    ));
    std::fs::create_dir_all(&dir).expect("scratch");
    let file = dir.join("sleeper");
    let script = format!("/bin/sleep 30 & echo $! > '{}'; wait", file.display());
    let mut probing = Probing::of_command(&mut sh(&script)).expect("started");
    let written =
        || -> Option<libc::pid_t> { std::fs::read_to_string(&file).ok()?.trim().parse().ok() };
    let started = Instant::now();
    while written().is_none() && started.elapsed() < MINUTE {
        assert_eq!(probing.poll(), None, "it runs");
        std::thread::sleep(Duration::from_millis(10));
    }
    let sleeper = written().expect("the sleeper's pid");
    // SAFETY: signal 0 only asks whether `sleeper` can be signalled.
    let runs = unsafe { libc::kill(sleeper, 0) } == 0;
    assert!(runs, "the sleeper runs");
    drop(probing);
    let gone = || {
        // SAFETY: signal 0 only asks whether `sleeper` can be signalled.
        unsafe { libc::kill(sleeper, 0) != 0 }
    };
    let started = Instant::now();
    while !gone() && started.elapsed() < MINUTE {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(gone(), "the sleeper {sleeper} outlived its probe");
    let _ = std::fs::remove_dir_all(&dir);
}
