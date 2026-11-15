// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Reversible managed signals while the synchronous cold operation owns children.

use crate::sys::{AllSignals, ProbeSpawnRefusal, SignalPipe};
use std::io;

pub(super) struct Scope {
    pipe: Box<SignalPipe>,
    // Ordinary (non-real-time) signals may coalesce. Retain the first of each
    // managed kind in receipt order; a flood cannot grow this beyond HANDLED.
    caught: Vec<i32>,
}

fn cancels(signal: i32) -> bool {
    matches!(
        signal,
        libc::SIGINT | libc::SIGQUIT | libc::SIGHUP | libc::SIGTERM | libc::SIGTSTP
    )
}

impl Scope {
    pub(super) fn install() -> Result<Option<Self>, ProbeSpawnRefusal> {
        let mut mask = AllSignals::block()?;
        let installed = (|| -> io::Result<Option<Box<SignalPipe>>> {
            crate::sys::ensure_single_threaded()?;
            // Do not turn an unmanaged reaper into a usable fork premise. An
            // arbitrary caught callback could also consume unrelated events;
            // the synchronous cold operation explicitly does not support it.
            // SAFETY: initialized writable native action storage.
            let mut action: libc::sigaction = unsafe { std::mem::zeroed() };
            // SAFETY: a null action queries this process's current disposition.
            if unsafe { libc::sigaction(libc::SIGCHLD, std::ptr::null(), &raw mut action) } != 0 {
                return Err(io::Error::last_os_error());
            }
            if action.sa_sigaction != libc::SIG_DFL || action.sa_flags & libc::SA_NOCLDWAIT != 0 {
                return Ok(None);
            }
            SignalPipe::install().map(Box::new).map(Some)
        })();
        let restored = mask.restore();
        match (installed, restored) {
            (Ok(pipe), Ok(())) => Ok(pipe.map(|pipe| Self {
                pipe,
                caught: Vec::new(),
            })),
            (Ok(pipe), Err(error)) => Err(ProbeSpawnRefusal {
                error,
                child: None,
                signals: Some(Box::new(mask)),
                signal_pipe: pipe,
            }),
            (Err(error), Ok(())) => Err(error.into()),
            (Err(error), Err(_)) => Err(ProbeSpawnRefusal {
                error,
                child: None,
                signals: Some(Box::new(mask)),
                signal_pipe: None,
            }),
        }
    }

    pub(super) fn cancelled(&mut self) -> bool {
        for signal in self.pipe.wait(0) {
            if !self.caught.contains(&signal) {
                self.caught.push(signal);
            }
        }
        self.caught.iter().copied().any(cancels)
    }

    pub(super) fn retain(self, mut failure: ProbeSpawnRefusal) -> ProbeSpawnRefusal {
        // This is the one process-wide signal pipe. All inner errors are from
        // ordinary probes, which never install another signal scope.
        let mut pipe = self.pipe;
        pipe.retain_caught(&self.caught);
        failure.signal_pipe = Some(pipe);
        failure
    }

    pub(super) fn finish(mut self) -> Result<Option<i32>, ProbeSpawnRefusal> {
        self.cancelled();
        let late = match self.pipe.restore_checked() {
            Ok(late) => late,
            Err(error) => return Err(self.retain(error.into())),
        };
        for signal in late {
            if !self.caught.contains(&signal) {
                self.caught.push(signal);
            }
        }
        let interrupted = self.caught.iter().copied().find(|signal| cancels(*signal));
        for index in 0..self.caught.len() {
            let signal = self.caught[index];
            // Original dispositions and mask are now back and every actual
            // child is settled. Ignored/blocked signals were never caught.
            // CHLD is replayed only under its supported original SIG_DFL.
            // SAFETY: every byte came from this scope's fixed HANDLED handler.
            if unsafe { libc::raise(signal) } != 0 {
                return Err(self.retain(io::Error::last_os_error().into()));
            }
        }
        Ok(interrupted)
    }
}
