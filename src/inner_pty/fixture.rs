// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Debug-only owned writer custody. No runtime or environment selects this path.

use super::{Generation, PendingAllocation, Slave, WriterSettlement, invalid};
use std::io;
use std::os::fd::{AsRawFd, RawFd};
use std::time::{Duration, Instant};

/// Failed fixture setup, retaining every unsettled child/mask transaction.
#[derive(Debug)]
pub struct FixtureRefusal {
    /// Setup, signal, or terminal-status error.
    pub error: io::Error,
    /// Exact writer custody if fork succeeded.
    pub writer: Option<Box<FixtureWriter>>,
    /// A transaction that failed before writer ownership was constructed.
    pub pending: Option<Box<PendingAllocation>>,
}

impl From<io::Error> for FixtureRefusal {
    fn from(error: io::Error) -> Self {
        Self {
            error,
            writer: None,
            pending: None,
        }
    }
}

impl From<super::AllocationError> for FixtureRefusal {
    fn from(failure: super::AllocationError) -> Self {
        Self {
            error: failure.error,
            writer: None,
            pending: failure.pending,
        }
    }
}

/// Exactly one fixed fixture body; it cannot fork or retain other writers.
#[derive(Debug)]
#[must_use = "positively reap the owned writer before retiring its cohort"]
pub struct FixtureWriter {
    custody: Box<PendingAllocation>,
    generation: Generation,
    device: (u64, u64, u64),
}

impl FixtureWriter {
    /// Exact terminal reap grants evidence only after the fixed writer exits.
    pub fn finish(mut self, deadline: Instant) -> Result<WriterSettlement, FixtureRefusal> {
        let status = match self.custody.wait(deadline, false) {
            Ok(status) => status,
            Err(error) => {
                return Err(FixtureRefusal {
                    error,
                    writer: Some(Box::new(self)),
                    pending: None,
                });
            }
        };
        if let Err(error) = self.custody.restore() {
            return Err(FixtureRefusal {
                error,
                writer: Some(Box::new(self)),
                pending: None,
            });
        }
        if status == 0 {
            Ok(WriterSettlement {
                generation: self.generation,
                device: self.device,
            })
        } else {
            Err(io::Error::other("owned writer exited unsuccessfully").into())
        }
    }

    /// Kill/reap exact custody; no successful-writer/drain proof is granted.
    pub fn kill(mut self, deadline: Instant) -> Result<(), Self> {
        if self.custody.wait(deadline, true).is_ok() && self.custody.restore().is_ok() {
            Ok(())
        } else {
            Err(self)
        }
    }

    /// Observe this exact fixture lifetime without consuming a wait event.
    pub fn alive(&mut self) -> bool {
        self.custody.fixture_current()
    }

    /// Model numeric PID reuse: reap old custody, then substitute a real other
    /// child's numeric pid while retaining a distinct stale birth. The other
    /// FixtureWriter remains the only genuine owner of that replacement.
    pub fn stale_receipt_against(
        mut self,
        replacement: &Self,
        deadline: Instant,
    ) -> Result<PendingAllocation, FixtureRefusal> {
        if let Err(error) = self.custody.wait(deadline, false) {
            return Err(FixtureRefusal {
                error,
                writer: Some(Box::new(self)),
                pending: None,
            });
        }
        self.custody.fixture_reused_pid(&replacement.custody);
        Ok(*self.custody)
    }
}

pub(super) fn spawn(
    slave: Slave,
    bytes: &[u8],
    paused: bool,
) -> Result<FixtureWriter, FixtureRefusal> {
    if bytes.len() > 8192 {
        return Err(invalid("fixture payload exceeds bound").into());
    }
    let deadline = Instant::now() + Duration::from_secs(60);
    let mut custody = PendingAllocation::begin()?;
    let setup = (|| -> io::Result<_> {
        super::allocate::retained_waits()?;
        let channels = super::allocate::channel()?;
        let directory = if cfg!(target_os = "linux") {
            "/proc/self/fd"
        } else {
            "/dev/fd"
        };
        let mut close: Vec<RawFd> = Vec::new();
        for entry in std::fs::read_dir(directory)? {
            if let Some(fd) = entry?.file_name().to_str().and_then(|s| s.parse().ok())
                && fd > 2
                && fd != slave.fd.as_raw_fd()
            {
                close.push(fd);
            }
            if close.len() > 65_536 {
                return Err(invalid("fixture fd inventory exceeds bound"));
            }
        }
        Ok((channels, close))
    })();
    let ((parent, child), close) = match setup {
        Ok(setup) => setup,
        Err(error) => {
            return Err(FixtureRefusal {
                error,
                writer: None,
                pending: custody.settle(deadline).err(),
            });
        }
    };
    // SAFETY: channel creation/inventory/fork share ALL-handler exclusion and
    // a freshly checked actual single native thread. Child never returns.
    let pid = unsafe { libc::fork() };
    if pid < 0 {
        let error = io::Error::last_os_error();
        drop((parent, child));
        return Err(FixtureRefusal {
            error,
            writer: None,
            pending: custody.settle(deadline).err(),
        });
    }
    if pid == 0 {
        drop(parent);
        if !super::allocate::await_start(child.as_raw_fd(), deadline) {
            // SAFETY: bounded bootstrap refusal does not unwind through caller.
            unsafe {
                libc::_exit(1);
            }
        }
        for fd in close {
            // SAFETY: child-private fd table; close every nonstandard holder
            // except this exact writer. _exit bypasses Rust descriptor drops.
            unsafe {
                libc::close(fd);
            }
        }
        if paused {
            loop {
                // SAFETY: fixed test child awaits its genuine owner's SIGKILL.
                unsafe {
                    libc::pause();
                }
            }
        }
        let mut sent = 0;
        while sent < bytes.len() {
            // SAFETY: uniquely owned live slave, exact readable bounded payload.
            let n = unsafe {
                libc::write(
                    slave.fd.as_raw_fd(),
                    bytes[sent..].as_ptr().cast(),
                    bytes.len() - sent,
                )
            };
            if n <= 0 {
                if n < 0 && io::Error::last_os_error().kind() == io::ErrorKind::Interrupted {
                    continue;
                }
                // SAFETY: never unwind through continuing supervisor image.
                unsafe {
                    libc::_exit(1);
                }
            }
            sent += n as usize;
        }
        // SAFETY: kernel exit closes the cohort's only active writer.
        unsafe {
            libc::_exit(0);
        }
    }
    drop(child);
    let started = custody.start(pid, parent, true);
    let writer = FixtureWriter {
        custody: Box::new(custody),
        generation: slave.generation,
        device: slave.device,
    };
    drop(slave);
    match started {
        Ok(()) => Ok(writer),
        Err(error) => Err(FixtureRefusal {
            error,
            writer: Some(Box::new(writer)),
            pending: None,
        }),
    }
}
