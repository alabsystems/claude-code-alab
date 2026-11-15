// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Process-isolated native entry states for the actual launch preparation code.

use std::fs::{File, OpenOptions};
use std::io::Write as _;
use std::os::fd::AsRawFd as _;
use std::os::unix::process::CommandExt as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicI32, Ordering};

static ENTERED: AtomicI32 = AtomicI32::new(-1);
static CONSUMED: AtomicI32 = AtomicI32::new(0);

extern "C" fn foreign_wait(_: libc::c_int) {
    let fd = ENTERED.load(Ordering::SeqCst);
    let bytes = b"entered\n";
    // SAFETY: the entry keeps this pre-opened private file alive while its handler runs.
    let written = unsafe { libc::write(fd, bytes.as_ptr().cast(), bytes.len()) };
    if written != bytes.len() as isize {
        CONSUMED.store(-2, Ordering::SeqCst);
        return;
    }
    let mut status = 0;
    // SAFETY: this isolated negative control deliberately consumes its only helper's wait.
    let pid = unsafe { libc::waitpid(-1, &raw mut status, 0) };
    CONSUMED.store(pid, Ordering::SeqCst);
}

fn prepare(kind: &str, dir: &Path) -> Option<File> {
    claude_code_alab::sys::ensure_single_threaded().expect("isolated native entry");
    if kind == "direct-inherited" {
        return None;
    }
    // SAFETY: exact writable native action and mask layouts, initialized before use.
    let (mut action, mut mask): (libc::sigaction, libc::sigset_t) = unsafe { std::mem::zeroed() };
    action.sa_sigaction = libc::SIG_DFL;
    if matches!(kind, "no-cldwait" | "direct-no-cldwait") {
        action.sa_flags = libc::SA_NOCLDWAIT;
    }
    // SAFETY: native action storage belongs to this fresh single-threaded process.
    unsafe {
        assert_eq!(libc::sigemptyset(&raw mut action.sa_mask), 0);
        assert_eq!(
            libc::sigaction(libc::SIGCHLD, &raw const action, std::ptr::null_mut()),
            0
        );
        assert_eq!(libc::sigemptyset(&raw mut mask), 0);
        assert_eq!(libc::sigaddset(&raw mut mask, libc::SIGUSR2), 0);
        assert_eq!(
            libc::sigprocmask(libc::SIG_BLOCK, &raw const mask, std::ptr::null_mut()),
            0
        );
    }
    if kind != "foreign-reaper" {
        return None;
    }
    let entered = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(dir.join("reaper-entered"))
        .expect("private reaper acknowledgement");
    ENTERED.store(entered.as_raw_fd(), Ordering::SeqCst);
    action.sa_sigaction = foreign_wait as *const () as usize;
    action.sa_flags = libc::SA_RESTART;
    // SAFETY: only this isolated entry installs its own deliberate foreign waiter.
    unsafe {
        assert_eq!(libc::sigfillset(&raw mut action.sa_mask), 0);
        assert_eq!(
            libc::sigaction(libc::SIGUSR1, &raw const action, std::ptr::null_mut()),
            0
        );
        assert_eq!(libc::sigemptyset(&raw mut mask), 0);
        assert_eq!(libc::sigaddset(&raw mut mask, libc::SIGUSR1), 0);
        assert_eq!(
            libc::sigprocmask(libc::SIG_UNBLOCK, &raw const mask, std::ptr::null_mut()),
            0
        );
    }
    Some(entered)
}

pub(super) fn entry() {
    let Some(expected) = std::env::var_os("ALAB_TEST_QUERY_ENTRY_IMAGE") else {
        return;
    };
    if std::env::current_exe().ok().as_deref() != Some(Path::new(&expected)) {
        return;
    }
    let kind = std::env::var("ALAB_TEST_QUERY_ENTRY_KIND").expect("native entry kind");
    assert!(matches!(
        kind.as_str(),
        "no-cldwait" | "direct-no-cldwait" | "direct-inherited" | "foreign-reaper"
    ));
    let dir =
        PathBuf::from(std::env::var_os("FAKE_CLAUDE_DIR").expect("private fixture directory"));
    let _entered = prepare(&kind, &dir);
    super::launch_host::record_signal_state(&dir, "entry-signals");
    if matches!(kind.as_str(), "direct-no-cldwait" | "direct-inherited") {
        let host = std::env::var_os("ATERM_HARNESS_HOST").expect("private native host");
        let error = std::process::Command::new(host)
            .args(std::env::args_os().skip(1))
            .exec();
        panic!("direct native transition refused: {error}");
    }
    match claude_code_alab::cli::main() {
        Ok(never) => match never {},
        Err(retained) => {
            let lost = retained
                .child
                .as_ref()
                .is_some_and(|child| child.pending() && child.ownership_lost());
            // Hold the error through every diagnostic write or panic below.
            std::mem::forget(retained);
            super::launch_host::record_signal_state(&dir, "return-signals");
            let unchanged = match (
                std::fs::read(dir.join("entry-signals")),
                std::fs::read(dir.join("return-signals")),
            ) {
                (Ok(before), Ok(after)) => before == after,
                _ => false,
            };
            let mut code = if unchanged { 70 } else { 71 };
            if kind == "foreign-reaper" {
                let consumed = CONSUMED.load(Ordering::SeqCst);
                std::fs::write(dir.join("reaper-result"), format!("{consumed}\n"))
                    .expect("actual foreign wait result");
                if consumed <= 1 || !lost {
                    code = 71;
                    let _ = writeln!(
                        std::io::stderr(),
                        "foreign waiter did not retain the original lost receipt"
                    );
                }
            }
            // This is the fixture executable's terminal boundary, never an end witness.
            std::process::exit(code);
        }
    }
}
