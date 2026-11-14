// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Helpers shared by this crate's unit tests.

use std::fs::{File, TryLockError};
use std::time::{Duration, Instant};

/// Acquire atpkg's reclaim `gate` exclusively after the test dropped its last
/// in-process hold. When Cargo runs sibling unit tests (`procs::tests`) in
/// parallel threads, a concurrent `posix_spawn` briefly holds a `CLOEXEC`
/// `fileglob` reference between `fdcopy` and `fdexec` in the kernel, so the
/// `flock` can outlive the drop by that window; the wait is bounded at 2 s.
pub(crate) fn lock_reclaim_gate(gate: &File) {
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        match gate.try_lock() {
            Ok(()) => return,
            Err(TryLockError::WouldBlock) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(5));
            }
            Err(err) => panic!("the bounded handoff ended: {err:?}"),
        }
    }
}
