// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The operating-system calls the harness makes, each behind a safe wrapper.
//!
//! Signal, terminal and wait wrappers live here; kernel identity readers live
//! in [`crate::procs`]. Each unsafe call explains its preconditions beside it.
//! Signals reach the event loop through a self-pipe: a handler only writes the
//! signal number to a non-blocking, close-on-exec pipe (async-signal-safe), and
//! the loop reads it. Inherited ignores stay ignored, except SIGCHLD must be
//! caught to retain wait ownership. The transfer primitives are used by the
//! own-image move, which a default launch outside aterm performs.

use std::io;

pub(crate) mod custody;
mod probe;
mod probe_group;
mod transfer;
pub use custody::AllSignals;
pub use probe::{ProbeChild, ProbeSpawnRefusal, exec_exact};
pub use transfer::{
    DescriptorExecGuard, DescriptorIdentity, HANDLED, INPUT_MODE_RESET, MANAGED, SignalBlock,
    SignalExecGuard, SignalPipe, SignalSnapshot, Termios, TermiosSnapshot, block_managed,
    descriptor_flags, descriptor_identity, ensure_single_threaded, inherit_for_one_exec,
    inherited_read_end, managed_signals_blocked, queued_signals, restore_descriptor_cloexec,
    take_pending,
};

/// Remove private exec markers only while this image is single-threaded.
/// # Errors
/// Native thread quiescence cannot be established.
pub fn clear_exec_markers(names: &[&str]) -> io::Result<()> {
    ensure_single_threaded()?;
    for name in names {
        // SAFETY: a single native thread is established and this synchronous
        // entry does not spawn another thread before environment mutation ends.
        unsafe { std::env::remove_var(name) };
    }
    Ok(())
}

/// The exact terminal snapshot ABI supported by this image, without reading a terminal.
/// # Errors
/// This native termios field layout is unsupported.
pub fn terminal_snapshot_abi() -> io::Result<String> {
    transfer::termios_abi()
}

#[cfg(any(target_os = "macos", target_os = "ios", target_os = "freebsd"))]
fn errno_location() -> *mut libc::c_int {
    // SAFETY: `__error` returns this thread's errno slot; it has no preconditions.
    unsafe { libc::__error() }
}

#[cfg(not(any(target_os = "macos", target_os = "ios", target_os = "freebsd")))]
fn errno_location() -> *mut libc::c_int {
    // SAFETY: `__errno_location` returns this thread's errno slot; no preconditions.
    unsafe { libc::__errno_location() }
}

fn errno() -> libc::c_int {
    // std reads this thread's errno without allocating (async-signal-safe).
    io::Error::last_os_error().raw_os_error().unwrap_or(0)
}

fn set_errno(value: libc::c_int) {
    // SAFETY: the pointer is this thread's errno slot, valid and aligned for the
    // thread's life; writing restores what the interrupted code saw.
    unsafe { *errno_location() = value }
}

/// What `waitpid` reported about the child.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChildEvent {
    /// It exited with this code.
    Exited(i32),
    /// A signal ended it.
    Signaled(i32),
    /// A signal stopped it.
    Stopped(i32),
    /// It continued after a stop.
    Continued,
}

/// One non-blocking `waitpid` on `pid` that also reports stops and continues.
#[must_use]
pub fn wait_child(pid: libc::pid_t) -> Option<ChildEvent> {
    wait_with(pid, libc::WNOHANG | libc::WUNTRACED | libc::WCONTINUED)
}

/// One non-blocking `waitpid` on `pid` that reports only its end, leaving a
/// stop or continue pending for [`wait_child`].
#[must_use]
pub fn wait_child_end(pid: libc::pid_t) -> Option<ChildEvent> {
    wait_with(pid, libc::WNOHANG)
}

fn wait_with(pid: libc::pid_t, options: libc::c_int) -> Option<ChildEvent> {
    use std::os::unix::process::ExitStatusExt as _;
    let mut status: libc::c_int = 0;
    loop {
        // SAFETY: `status` is a valid out-parameter; `pid` is this process's child.
        let rc = unsafe { libc::waitpid(pid, &raw mut status, options) };
        if rc == pid {
            break;
        }
        if rc < 0 && errno() == libc::EINTR {
            continue;
        }
        return None;
    }
    // std decodes the wait status exactly as the `W*` macros do (`code` is
    // `WIFEXITED`/`WEXITSTATUS`, `signal` is `WIFSIGNALED`/`WTERMSIG`,
    // `stopped_signal` is `WIFSTOPPED`/`WSTOPSIG`), with no foreign call.
    let status = std::process::ExitStatus::from_raw(status);
    Some(if let Some(code) = status.code() {
        ChildEvent::Exited(code)
    } else if let Some(sig) = status.signal() {
        ChildEvent::Signaled(sig)
    } else if let Some(sig) = status.stopped_signal() {
        ChildEvent::Stopped(sig)
    } else {
        ChildEvent::Continued
    })
}

/// Whether the child `pid` has ended and waits to be reaped: one non-blocking
/// `waitid` that leaves its end pending for [`wait_child`] (`WNOWAIT`). A
/// signal sent to such a child is accepted by the kernel and reaches nobody.
#[must_use]
pub fn child_has_ended(pid: libc::pid_t) -> bool {
    let Ok(id) = libc::id_t::try_from(pid) else {
        return false;
    };
    loop {
        let mut info = transfer::blank_siginfo();
        // SAFETY: `info` is zeroed, writable siginfo_t storage. WNOWAIT leaves
        // the child's end pending; WNOHANG never waits for a live child.
        let rc = unsafe {
            libc::waitid(
                libc::P_PID,
                id,
                &raw mut info,
                libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
            )
        };
        if rc == 0 {
            // `info` is zeroed, and filled in when an end was found.
            return siginfo_pid(&info) == pid;
        }
        if errno() != libc::EINTR {
            return false;
        }
    }
}

#[cfg(target_os = "linux")]
fn siginfo_pid(info: &libc::siginfo_t) -> libc::pid_t {
    // SAFETY: waitid wrote a SIGCHLD record, or left the zeroed one, whose
    // pid field is the one this accessor reads.
    unsafe { info.si_pid() }
}

#[cfg(not(target_os = "linux"))]
fn siginfo_pid(info: &libc::siginfo_t) -> libc::pid_t {
    info.si_pid
}

/// Send `sig` to `pid`; true when the kernel accepted it.
pub fn kill(pid: libc::pid_t, sig: libc::c_int) -> bool {
    if pid <= 0 {
        return false;
    }
    // SAFETY: a plain signal to one positive pid (this process's own child or itself).
    unsafe { libc::kill(pid, sig) == 0 }
}

/// Stop this process (SIGSTOP cannot be caught); returns once it is continued.
/// The pid is std's (`std::process::id`), so this needs no `getpid` call.
pub fn stop_self() {
    if let Ok(me) = libc::pid_t::try_from(std::process::id()) {
        let _ = kill(me, libc::SIGSTOP);
    }
}

/// Whether stdin is a terminal (std's `IsTerminal`, the `isatty(0)` question,
/// with no `unsafe` here).
#[must_use]
pub fn stdin_is_terminal() -> bool {
    use std::io::IsTerminal as _;
    io::stdin().is_terminal()
}

/// Whether this process's group is the foreground group of the terminal on `fd`.
#[must_use]
pub fn in_foreground(fd: libc::c_int) -> bool {
    // SAFETY: both calls only read process and terminal state.
    unsafe {
        let fg = libc::tcgetpgrp(fd);
        fg > 0 && fg == libc::getpgrp()
    }
}

/// The last time input was read from the terminal on stdin, epoch milliseconds:
/// the device's access time, which the kernel moves when a reader takes input
/// from it (measured on macOS 27 with a pty: output moves only the modification
/// time, queued-but-unread input moves neither). It is the clock `w(1)` reads
/// as a terminal's idle time. Read through a duplicate of fd 0 (`fstat` by std),
/// so this needs no `unsafe`.
#[must_use]
pub fn stdin_input_ms() -> Option<u64> {
    use std::os::fd::AsFd as _;
    use std::os::unix::fs::MetadataExt as _;
    let fd = io::stdin().as_fd().try_clone_to_owned().ok()?;
    let meta = std::fs::File::from(fd).metadata().ok()?;
    let secs = u64::try_from(meta.atime()).ok()?;
    let nanos = u64::try_from(meta.atime_nsec()).ok()?;
    secs.checked_mul(1000)?.checked_add(nanos / 1_000_000)
}

/// How many bytes typed on the terminal on stdin no reader has taken yet
/// (`FIONREAD`). Queued input moves neither clock [`stdin_input_ms`] reads, so
/// a keystroke the vendor has not read yet is seen only here. In canonical
/// mode macOS counts only completed lines; in the vendor's raw mode every byte
/// counts.
/// # Errors
/// stdin is not a terminal, or the query fails.
pub fn stdin_unread() -> io::Result<usize> {
    let mut queued: libc::c_int = 0;
    // SAFETY: FIONREAD only reads the terminal's input queue length into this
    // one valid int.
    if unsafe { libc::ioctl(0, libc::FIONREAD, &raw mut queued) } != 0 {
        return Err(io::Error::last_os_error());
    }
    usize::try_from(queued)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "negative unread input"))
}

/// End this process the way the child ended: the same exit code, or death by
/// the same signal (re-raised with its default action, core dumps off so the
/// harness never leaves a core of its own). Falls back to `128 + signal`.
pub fn exit_like(end: ChildEvent) -> ! {
    match end {
        ChildEvent::Exited(code) => std::process::exit(code),
        ChildEvent::Signaled(sig) => {
            let none = libc::rlimit {
                rlim_cur: 0,
                rlim_max: 0,
            };
            let mut set = transfer::blank_sigset();
            // SAFETY: plain process-state calls on this process with valid arguments;
            // `sigemptyset` empties `set` before `sigaddset` and `sigprocmask` read it.
            unsafe {
                let _ = libc::setrlimit(libc::RLIMIT_CORE, std::ptr::from_ref(&none));
                let _ = libc::signal(sig, libc::SIG_DFL);
                libc::sigemptyset(&mut set);
                libc::sigaddset(&mut set, sig);
                let _ = libc::sigprocmask(
                    libc::SIG_UNBLOCK,
                    std::ptr::from_ref(&set),
                    std::ptr::null_mut(),
                );
                let _ = libc::raise(sig);
            }
            std::process::exit(sig.saturating_add(128))
        }
        ChildEvent::Stopped(_) | ChildEvent::Continued => std::process::exit(1),
    }
}
