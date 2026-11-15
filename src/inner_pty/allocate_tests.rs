// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::AllSignals;

struct Restore(libc::sigset_t);

impl Drop for Restore {
    fn drop(&mut self) {
        // SAFETY: this initialized mask belongs to this test's current thread.
        let _ = unsafe {
            libc::pthread_sigmask(libc::SIG_SETMASK, &raw const self.0, std::ptr::null_mut())
        };
    }
}

#[test]
fn abandoning_an_exclusion_restores_the_callers_signal_mask() {
    // SAFETY: native writable sets, initialized before either is inspected.
    let mut empty = unsafe { std::mem::zeroed() };
    // SAFETY: writable native mask output for this thread's previous state.
    let mut original = unsafe { std::mem::zeroed() };
    // SAFETY: initialize the test's explicit mask without changing another thread.
    assert_eq!(unsafe { libc::sigemptyset(&raw mut empty) }, 0);
    assert_eq!(
        // SAFETY: both masks are live native storage; this changes only this test thread.
        unsafe { libc::pthread_sigmask(libc::SIG_SETMASK, &raw const empty, &raw mut original) },
        0
    );
    let _restore = Restore(original);
    let exclusion = AllSignals::block().unwrap();
    drop(exclusion);
    // SAFETY: a null input only queries this thread's mask into initialized storage.
    let mut after = unsafe { std::mem::zeroed() };
    assert_eq!(
        // SAFETY: after is valid writable native mask storage.
        unsafe { libc::pthread_sigmask(libc::SIG_BLOCK, std::ptr::null(), &raw mut after) },
        0
    );
    // SIGKILL/SIGSTOP cannot be blocked. Comparing membership avoids native padding bytes.
    for signal in 1..=128 {
        assert_eq!(
            // SAFETY: after is initialized; out-of-range signals return -1 for both sets.
            unsafe { libc::sigismember(&raw const after, signal) },
            // SAFETY: empty is initialized; out-of-range signals return -1 for both sets.
            unsafe { libc::sigismember(&raw const empty, signal) },
            "signal {signal} remained blocked after the exclusion was abandoned"
        );
    }
}
