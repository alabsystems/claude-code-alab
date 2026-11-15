// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Shared single-threaded fork/wait custody for private helpers and SDK children.

use crate::procs::OwnedLifetime;
use std::io;
use std::marker::PhantomData;
use std::rc::Rc;
use std::time::{Duration, Instant};

fn invalid(why: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, why)
}

/// An all-handler exclusion belonging to the current native thread.
/// Dropping it restores thread state, without claiming that a child was settled.
pub struct AllSignals {
    previous: libc::sigset_t,
    active: bool,
    _thread: PhantomData<Rc<()>>,
}

impl std::fmt::Debug for AllSignals {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AllSignals")
            .field("active", &self.active)
            .finish_non_exhaustive()
    }
}

impl AllSignals {
    pub(crate) fn previous(&self) -> libc::sigset_t {
        self.previous
    }
    pub(crate) fn block() -> io::Result<Self> {
        // SAFETY: writable native sets initialized by the following native calls.
        let mut all = unsafe { std::mem::zeroed() };
        // SAFETY: native mask output storage for this thread's previous state.
        let mut previous = unsafe { std::mem::zeroed() };
        // SAFETY: all is writable native signal storage.
        if unsafe { libc::sigfillset(&raw mut all) } != 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: initialized masks; the block occurs BEFORE checking the native
        // thread count, so a handler cannot fork or start a thread in that gap.
        let error =
            unsafe { libc::pthread_sigmask(libc::SIG_BLOCK, &raw const all, &raw mut previous) };
        if error != 0 {
            return Err(io::Error::from_raw_os_error(error));
        }
        Ok(Self {
            previous,
            active: true,
            _thread: PhantomData,
        })
    }

    /// Restore precisely the current thread's prior mask.
    /// # Errors
    /// The native mask restore fails. Ownership remains with this guard.
    pub fn restore(&mut self) -> io::Result<()> {
        if self.active {
            // SAFETY: the initialized prior mask was captured on this same thread;
            // the !Send guard cannot move the exclusion onto another thread.
            let error = unsafe {
                libc::pthread_sigmask(
                    libc::SIG_SETMASK,
                    &raw const self.previous,
                    std::ptr::null_mut(),
                )
            };
            if error != 0 {
                return Err(io::Error::from_raw_os_error(error));
            }
            self.active = false;
        }
        Ok(())
    }
}

impl Drop for AllSignals {
    fn drop(&mut self) {
        let _ = self.restore();
    }
}

/// Native lifetime and positive wait custody. Missing or stolen custody never
/// authorizes signalling a numeric pid, even if it later names a live child.
#[derive(Debug)]
pub(crate) struct OwnedChild {
    pub(crate) pid: Option<libc::pid_t>,
    pub(crate) status: Option<libc::c_int>,
    pub(crate) lost: bool,
    identity: Option<OwnedLifetime>,
    pub(crate) all_signals: Option<AllSignals>,
}

impl OwnedChild {
    pub(crate) fn empty() -> Self {
        Self {
            pid: None,
            status: None,
            lost: false,
            identity: None,
            all_signals: None,
        }
    }

    pub(crate) fn exclude(&mut self) -> io::Result<()> {
        if self.all_signals.is_none() {
            self.all_signals = Some(AllSignals::block()?);
        }
        crate::sys::ensure_single_threaded()
    }

    /// Call only under the same unbroken exclusion which surrounded fork.
    pub(crate) fn start(&mut self, pid: libc::pid_t) -> io::Result<()> {
        self.pid = Some(pid);
        self.exclude()?;
        self.identity = Some(
            OwnedLifetime::capture(pid as u32)
                .ok_or_else(|| invalid("fresh child native lifetime unavailable"))?,
        );
        Ok(())
    }

    pub(crate) fn restore(&mut self) -> io::Result<()> {
        if let Some(pid) = self.pid
            && self.identity.is_none()
            && !self.lost
        {
            // A failed initial read retains the unbroken fork exclusion. It may
            // not unmask, then adopt a later process merely bearing that pid.
            self.exclude()?;
            self.identity = Some(
                OwnedLifetime::capture(pid as u32)
                    .ok_or_else(|| invalid("fresh child native lifetime unavailable"))?,
            );
        }
        if let Some(mask) = self.all_signals.as_mut() {
            mask.restore()?;
        }
        self.all_signals = None;
        Ok(())
    }

    fn current(&mut self, pid: libc::pid_t) -> io::Result<()> {
        if self.lost
            || !self
                .identity
                .as_ref()
                .is_some_and(|identity| identity.pid == pid as u32 && identity.current())
        {
            self.lost = true;
            self.pid = None;
            return Err(invalid("child native lifetime changed or is unavailable"));
        }
        Ok(())
    }

    pub(crate) fn observe<T>(&mut self, read: impl FnOnce(u32) -> T) -> io::Result<T> {
        self.restore()?;
        let pid = self
            .pid
            .ok_or_else(|| invalid("child has no live wait custody"))?;
        self.exclude()?;
        struct Restore<'a>(&'a mut OwnedChild);
        impl Drop for Restore<'_> {
            fn drop(&mut self) {
                // A caught panic must not strand the embedding thread's mask.
                // This restores thread state without reaping or dropping the
                // still-owned child. A failed restore remains in that receipt.
                let _ = self.0.restore();
            }
        }
        let protected = Restore(self);
        let result = (|| {
            protected.0.current(pid)?;
            let observed = read(pid as u32);
            protected.0.current(pid)?;
            Ok(observed)
        })();
        protected.0.restore()?;
        result
    }

    /// One protected, nonblocking native observe/wait/optional-kill transaction.
    /// All handlers remain excluded until the exact wait and signal have finished.
    pub(crate) fn poll(&mut self, kill: bool) -> io::Result<Option<libc::c_int>> {
        self.restore()?;
        if self.lost {
            return Err(invalid("child wait ownership was lost"));
        }
        if self.status.is_some() {
            return Ok(self.status);
        }
        let Some(pid) = self.pid else { return Ok(None) };
        self.exclude()?;
        let result = (|| {
            self.current(pid)?;
            let mut status = 0;
            // SAFETY: sole native thread, all handlers blocked, exact native
            // lifetime plus non-consuming child custody just established.
            let waited = unsafe { libc::waitpid(pid, &raw mut status, libc::WNOHANG) };
            if waited == pid {
                self.pid = None;
                self.status = Some(status);
                return Ok(Some(status));
            }
            if waited < 0 {
                let error = io::Error::last_os_error();
                if error.raw_os_error() == Some(libc::ECHILD) {
                    self.lost = true;
                    self.pid = None;
                }
                return Err(error);
            }
            if kill {
                // SAFETY: the exact unreaped child is still pinned; no handler
                // or other native thread can reap it between waitpid and kill.
                if unsafe { libc::kill(pid, libc::SIGKILL) } != 0 {
                    let error = io::Error::last_os_error();
                    if error.raw_os_error() != Some(libc::ESRCH) {
                        return Err(error);
                    }
                }
            }
            Ok(None)
        })();
        self.restore()?;
        result
    }

    pub(crate) fn wait(&mut self, deadline: Instant, mut kill: bool) -> io::Result<libc::c_int> {
        loop {
            if let Some(status) = self.poll(kill)? {
                return Ok(status);
            }
            kill = false;
            let now = Instant::now();
            if now >= deadline {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "child settlement deadline",
                ));
            }
            std::thread::sleep((deadline - now).min(Duration::from_millis(10)));
        }
    }

    #[cfg(debug_assertions)]
    pub(crate) fn fixture_current(&mut self) -> bool {
        if self.exclude().is_err() {
            return false;
        }
        let current = self.identity.as_ref().is_some_and(|identity| {
            identity.current() && !crate::sys::child_has_ended(identity.pid as libc::pid_t)
        });
        self.restore().is_ok() && current
    }

    #[cfg(debug_assertions)]
    pub(crate) fn fixture_reused_pid(&mut self, replacement: &Self) {
        assert!(self.status.is_some() && self.pid.is_none());
        let pid = replacement.pid.expect("live replacement custody");
        let old = self.identity.as_mut().expect("old native identity");
        let replacement = replacement
            .identity
            .as_ref()
            .expect("replacement native identity");
        old.pid = pid as u32;
        if old.birth == replacement.birth {
            match &mut old.birth {
                crate::procs::Birth::MacOs { microseconds, .. } => *microseconds ^= 1,
                crate::procs::Birth::Linux(ticks) => *ticks ^= 1,
            }
        }
        self.pid = Some(pid);
        self.status = None;
        self.lost = false;
    }
}
