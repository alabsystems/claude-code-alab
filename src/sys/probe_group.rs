// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! An original probe group, kept distinct from a later reuse of its number.

use std::io;
#[cfg(target_os = "linux")]
use std::os::fd::{AsRawFd as _, FromRawFd as _, OwnedFd};

#[derive(Debug)]
pub(super) struct Group {
    #[cfg(not(target_os = "linux"))]
    id: libc::pid_t,
    #[cfg(target_os = "linux")]
    descriptor: OwnedFd,
    settled: bool,
}

/// Check the backend before forking. Linux requires descriptor-bound group
/// signalling; an older kernel is unavailable rather than a numeric fallback.
pub(super) fn available() -> io::Result<()> {
    #[cfg(target_os = "macos")]
    return Ok(());
    #[cfg(target_os = "linux")]
    {
        let descriptor = pidfd(std::process::id() as libc::pid_t)?;
        match signal_fd(descriptor.as_raw_fd(), 0) {
            Ok(()) => Ok(()),
            Err(error) if error.raw_os_error() == Some(libc::ESRCH) => Ok(()),
            Err(error) => Err(error),
        }
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "native probe group backend unavailable",
    ))
}

impl Group {
    /// The caller must own the unreaped child of this number and have established
    /// that it leads this group before capturing it.
    pub(super) fn capture(id: libc::pid_t) -> io::Result<Self> {
        if id <= 1 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid probe group",
            ));
        }
        #[cfg(not(any(target_os = "macos", target_os = "linux")))]
        available()?;
        Ok(Self {
            #[cfg(not(target_os = "linux"))]
            id,
            #[cfg(target_os = "linux")]
            descriptor: pidfd(id)?,
            settled: false,
        })
    }

    pub(super) fn pending(&self) -> bool {
        !self.settled
    }

    /// On Darwin a fresh whole kernel listing must contain only the pinned
    /// exited leader before its owning reap. Linux's retained descriptor permits
    /// reaping first without losing the identity used for later group signals.
    pub(super) fn may_reap(&self) -> io::Result<bool> {
        #[cfg(target_os = "macos")]
        {
            const MAXIMUM: usize = 256;
            let mut pids = [0_i32; MAXIMUM];
            let bytes = i32::try_from(std::mem::size_of_val(&pids)).map_err(io::Error::other)?;
            // SAFETY: this owned array supplies exactly bytes writable storage;
            // PROC_PGRP_ONLY lists the group the caller's unreaped leader pins.
            let got =
                unsafe { libc::proc_listpids(2, self.id as u32, pids.as_mut_ptr().cast(), bytes) };
            let got = usize::try_from(got).map_err(io::Error::other)?;
            if got == 0 || got >= bytes as usize || got % std::mem::size_of::<i32>() != 0 {
                return Err(io::Error::other(
                    "probe group listing unavailable or incomplete",
                ));
            }
            Ok(pids[..got / std::mem::size_of::<i32>()] == [self.id])
        }
        #[cfg(target_os = "linux")]
        return Ok(true);
        #[cfg(not(any(target_os = "macos", target_os = "linux")))]
        available().map(|()| false)
    }

    /// Numeric signalling is allowed only while the owning wait still pins the
    /// Darwin leader. Linux signals only through the original kernel pid object.
    pub(super) fn cancel(&self, root_unreaped: bool) -> io::Result<()> {
        if self.settled {
            return Ok(());
        }
        #[cfg(target_os = "macos")]
        {
            if !root_unreaped {
                return Err(io::Error::other(
                    "cannot signal a reaped numeric probe group",
                ));
            }
            // SAFETY: the protected caller still owns the exact unreaped group leader.
            let result = unsafe { libc::kill(-self.id, libc::SIGKILL) };
            if result == 0 {
                Ok(())
            } else {
                Err(io::Error::last_os_error())
            }
        }
        #[cfg(target_os = "linux")]
        {
            let _ = root_unreaped;
            signal_fd(self.descriptor.as_raw_fd(), libc::SIGKILL)
        }
        #[cfg(not(any(target_os = "macos", target_os = "linux")))]
        {
            let _ = root_unreaped;
            available()
        }
    }

    /// A non-mutating kernel absence read follows the actual owning root reap.
    /// A group still present, unread, or inaccessible retains its debt.
    pub(super) fn finish(&mut self) -> io::Result<bool> {
        if self.settled {
            return Ok(true);
        }
        #[cfg(target_os = "linux")]
        let result = signal_fd(self.descriptor.as_raw_fd(), 0);
        #[cfg(not(target_os = "linux"))]
        // SAFETY: signal zero observes existence only; no signal is delivered,
        // including where a numeric group was reused after the owning reap.
        let result = if unsafe { libc::kill(-self.id, 0) } == 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        };
        match result {
            Err(error) if error.raw_os_error() == Some(libc::ESRCH) => {
                self.settled = true;
                Ok(true)
            }
            Ok(()) => Ok(false),
            Err(error) if error.raw_os_error() == Some(libc::EPERM) => Ok(false),
            Err(error) => Err(error),
        }
    }
}

#[cfg(target_os = "linux")]
fn pidfd(pid: libc::pid_t) -> io::Result<OwnedFd> {
    // SAFETY: pidfd_open accepts a pid and zero flags and retains no memory.
    let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, pid, 0_u32) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: the syscall returned a fresh, close-on-exec descriptor.
    Ok(unsafe { OwnedFd::from_raw_fd(fd as i32) })
}

#[cfg(target_os = "linux")]
fn signal_fd(fd: i32, signal: i32) -> io::Result<()> {
    // Linux 6.9's PIDFD_SIGNAL_PROCESS_GROUP addresses the original struct pid's
    // group, including after its leader was reaped. No numeric signal fallback.
    // SAFETY: descriptor and signal are integers, the optional siginfo is null;
    // this syscall reads no caller-owned memory.
    let result = unsafe {
        libc::syscall(
            libc::SYS_pidfd_send_signal,
            fd,
            signal,
            std::ptr::null::<libc::siginfo_t>(),
            1_u32 << 2,
        )
    };
    if result == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}
