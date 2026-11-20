// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! [`run_until`]: a command run to its end is read whole on the calling
//! thread, and a cut leaves nothing the command started running.

use super::*;

/// The hang detector for what must happen.
const MINUTE: Duration = Duration::from_secs(60);

fn sh(script: &str) -> Command {
    let mut command = Command::new("/bin/sh");
    command.args(["-c", script]);
    command
}

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "claude-alab-run-until-{name}-{}-{}",
        std::process::id(),
        crate::supervise::now_ms()
    ));
    std::fs::create_dir_all(&dir).expect("scratch dir");
    dir
}

/// The pid a script wrote to `file`.
fn written_pid(file: &Path) -> libc::pid_t {
    std::fs::read_to_string(file)
        .expect("the pid file")
        .trim()
        .parse()
        .expect("a pid")
}

/// Whether a process `pid` exists (a zombie included).
fn alive(pid: libc::pid_t) -> bool {
    // SAFETY: signal 0 only asks whether `pid` can be signalled.
    unsafe { libc::kill(pid, 0) == 0 }
}

/// Whether `done` holds within `limit`, looked at every 10 ms.
fn within(limit: Duration, mut done: impl FnMut() -> bool) -> bool {
    let started = Instant::now();
    while started.elapsed() < limit {
        if done() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    done()
}

#[test]
fn a_command_that_ends_is_read_whole_however_much_it_writes() {
    // Two MiB, more than a pipe holds: the reading keeps the writer going.
    let mut big = sh("/bin/dd if=/dev/zero bs=65536 count=32 2>/dev/null; exit 3");
    match run_until(&mut big, Instant::now() + MINUTE) {
        Ran::Ended { code, out } => {
            assert_eq!(code, Some(3));
            assert_eq!(out.len(), 32 * 65_536);
            assert!(out.iter().all(|byte| *byte == 0));
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(
        run_until(&mut sh("printf 'a\\nb'"), Instant::now() + MINUTE),
        Ran::Ended {
            code: Some(0),
            out: b"a\nb".to_vec()
        }
    );
    // Negative controls: a deadline already passed starts nothing, and a
    // program that is not there is not started.
    assert_eq!(run_until(&mut sh("echo never"), Instant::now()), Ran::Cut);
    assert_eq!(
        run_until(
            &mut Command::new("/nonexistent/claude-alab-run-until"),
            Instant::now() + MINUTE
        ),
        Ran::NotStarted
    );
}

#[test]
fn a_cut_kills_every_process_the_command_left_holding_its_pipe() {
    let dir = scratch("cut");
    let limit = Duration::from_millis(300);
    // The shell still waiting at the deadline, and the shell already ended
    // with the sleeper it left behind holding the pipe: each cut kills the
    // sleeper with its group.
    for (name, tail) in [("waiting", "; wait"), ("ended", "")] {
        let file = dir.join(name);
        let script = format!("/bin/sleep 30 & echo $! > '{}'{tail}", file.display());
        let started = Instant::now();
        assert_eq!(
            run_until(&mut sh(&script), started + limit),
            Ran::Cut,
            "{name}"
        );
        assert!(
            started.elapsed() >= limit,
            "{name}: cut before its deadline"
        );
        let sleeper = written_pid(&file);
        assert!(
            within(MINUTE, || !alive(sleeper)),
            "{name}: the sleeper {sleeper} outlived the cut"
        );
    }
    // Negative control: a sleeper that holds no end of the pipe lets the
    // run end with its output, and nothing kills it.
    let file = dir.join("apart");
    let script = format!(
        "/bin/sleep 30 >/dev/null & echo $! > '{}'; echo done",
        file.display()
    );
    assert_eq!(
        run_until(&mut sh(&script), Instant::now() + MINUTE),
        Ran::Ended {
            code: Some(0),
            out: b"done\n".to_vec()
        }
    );
    let sleeper = written_pid(&file);
    assert!(alive(sleeper), "a run that ended killed nothing");
    // SAFETY: a plain syscall to the sleeper this test started.
    unsafe { libc::kill(sleeper, libc::SIGKILL) };
    let _ = std::fs::remove_dir_all(&dir);
}
