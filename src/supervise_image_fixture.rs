// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Explicit debug-only observation and fault injection at the production handoff boundaries.
//! No command-line or environment flag installs these hooks. A custom single-threaded
//! Rust fixture entry installs its own callbacks before calling the real CLI.

use crate::sys::{self, SignalSnapshot};
use crate::transfer_capsule::Phase;
use std::io;
use std::os::fd::RawFd;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// One actual immutable handoff, borrowed only for this callback.
pub struct Facts<'a> {
    /// Forward or the sole rollback.
    pub phase: Phase,
    /// This image's real compiled version.
    pub version: &'a str,
    /// Original absolute nanoseconds, never a fixture-relative deadline.
    pub deadline_ns: u64,
    /// Original encoded Saved bytes, without a recapture for rollback.
    pub original: &'a [u8],
    /// Original signal dispositions, endpoints and managed mask.
    pub signals: SignalSnapshot,
}

/// An observation in the actual production algorithm, never a second implementation.
pub enum Event<'a> {
    /// Every inherited role and image has been validated and taken into custody.
    Entry(Facts<'a>),
    /// All actual one-exec guards are active, immediately before the syscall.
    BeforeExec(Facts<'a>, &'a [(RawFd, libc::c_int)]),
    /// The actual exec syscall returned this errno.
    ExecReturned(Phase, Option<i32>),
    /// Non-signal descriptor guards have restored their original complete flag words.
    DescriptorsRestored(Facts<'a>, &'a [(RawFd, libc::c_int)]),
    /// Pipe endpoints and original managed mask have then been restored.
    SignalsRestored(Facts<'a>, &'a [(RawFd, libc::c_int)]),
    /// Normal same-child supervision is about to resume, after restoring the original mask.
    Resumed(Facts<'a>, bool),
}

/// Fixture-owned callbacks. They are absent from release builds.
#[derive(Clone, Copy)]
pub struct Hooks {
    /// Record real events; an entry observer may hold its own private test barrier.
    pub observe: for<'a> fn(&Event<'a>) -> io::Result<()>,
    /// Optionally choose a nonexistent path at the real exec syscall boundary.
    pub exec_path: fn(Phase, &Path) -> io::Result<PathBuf>,
    /// Optionally refuse new-image precommit, exercising the real rollback algorithm.
    pub precommit: fn(Phase) -> io::Result<()>,
}

static HOOKS: OnceLock<Hooks> = OnceLock::new();

/// Install fixture callbacks once in a genuinely single-threaded custom Rust entry.
/// # Errors
/// Native thread quiescence is unavailable, or this image already installed hooks.
pub fn install(hooks: Hooks) -> io::Result<()> {
    sys::ensure_single_threaded()?;
    HOOKS
        .set(hooks)
        .map_err(|_| io::Error::other("image fixture hooks already installed"))
}

fn guarded<T>(call: impl FnOnce() -> io::Result<T> + std::panic::UnwindSafe) -> io::Result<T> {
    std::panic::catch_unwind(call)
        .unwrap_or_else(|_| Err(io::Error::other("image fixture callback panicked")))
}

pub(super) fn observe(event: &Event<'_>) -> io::Result<()> {
    match HOOKS.get() {
        Some(hooks) => guarded(|| (hooks.observe)(event)),
        None => Ok(()),
    }
}

pub(super) fn exec_path(phase: Phase, path: &Path) -> io::Result<PathBuf> {
    match HOOKS.get() {
        Some(hooks) => guarded(|| (hooks.exec_path)(phase, path)),
        None => Ok(path.to_owned()),
    }
}

pub(super) fn precommit(phase: Phase) -> io::Result<()> {
    match HOOKS.get() {
        Some(hooks) => guarded(|| (hooks.precommit)(phase)),
        None => Ok(()),
    }
}
