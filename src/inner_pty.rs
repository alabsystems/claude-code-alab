// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Unwired inner-PTY transport primitives. These do not grant input authority,
//! manage terminal modes, spawn a vendor, or implement the native host protocol.
//!
//! Each relay consumes one freshly allocated pair. Input retains its original
//! caller-owned origin until delivered or explicitly retired. A successful write
//! means bytes reached the tty driver, never that the vendor consumed them.

#[path = "inner_pty/allocate.rs"]
mod allocate;
#[cfg(debug_assertions)]
#[path = "inner_pty/fixture.rs"]
mod fixture;
#[path = "inner_pty/relay.rs"]
mod relay;

#[cfg(debug_assertions)]
#[doc(hidden)]
pub use allocate::AllocationControl;
pub use allocate::{AllocationError, PendingAllocation};
#[cfg(debug_assertions)]
#[doc(hidden)]
pub use fixture::{FixtureRefusal, FixtureWriter};
pub use relay::{
    Delivery, InputRefusal, Limits, OutputDrained, Relay, Retirement, Step, StepError,
    WriterSettlement, Written,
};

use std::ffi::{CStr, CString};
use std::io;
use std::os::fd::{AsFd, AsRawFd, BorrowedFd, FromRawFd, OwnedFd, RawFd};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

static NEXT_GENERATION: AtomicU64 = AtomicU64::new(1);

/// A fresh pair's logical generation; not a kernel identity or permission.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Generation(u64);

impl Generation {
    /// Its process-local, positive serial number.
    pub fn get(self) -> u64 {
        self.0
    }
}

/// Terminal dimensions, with nonzero rows and columns.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Size {
    rows: u16,
    columns: u16,
}

impl Size {
    /// Validate dimensions without changing any terminal.
    pub fn new(rows: u16, columns: u16) -> io::Result<Self> {
        if rows == 0 || columns == 0 {
            return Err(invalid("PTY dimensions must be positive"));
        }
        Ok(Self { rows, columns })
    }

    fn native(self) -> libc::winsize {
        libc::winsize {
            ws_row: self.rows,
            ws_col: self.columns,
            ws_xpixel: 0,
            ws_ypixel: 0,
        }
    }
}

/// Fresh, private descriptors. No constructor adopts an arbitrary existing PTY.
#[derive(Debug)]
pub struct PtyPair {
    master: OwnedFd,
    slave: OwnedFd,
    generation: Generation,
}

impl PtyPair {
    /// Allocate through a bounded, positively reaped, single-threaded helper.
    /// An unsettled failure retains the helper's wait ownership in the error.
    pub fn allocate(size: Size, deadline: Instant) -> Result<Self, AllocationError> {
        allocate::pair(size, deadline, allocate::Fault::None)
    }

    /// This fresh pair's logical generation.
    pub fn generation(&self) -> Generation {
        self.generation
    }

    /// Borrow both endpoints for process-isolated transport controls only.
    #[cfg(debug_assertions)]
    #[doc(hidden)]
    pub fn fixture_descriptors(&self) -> (BorrowedFd<'_>, BorrowedFd<'_>) {
        (self.master.as_fd(), self.slave.as_fd())
    }

    /// Process-isolated allocator fault controls; never selected by the runtime.
    #[cfg(debug_assertions)]
    #[doc(hidden)]
    pub fn fixture_allocate(
        size: Size,
        deadline: Instant,
        control: AllocationControl,
    ) -> Result<Self, AllocationError> {
        allocate::pair(size, deadline, control.into())
    }
}

/// The slave retained for the future keeper/vendor spawn. No mode-write API.
#[derive(Debug)]
pub struct Slave {
    fd: OwnedFd,
    generation: Generation,
    device: (u64, u64, u64),
}

impl AsFd for Slave {
    fn as_fd(&self) -> BorrowedFd<'_> {
        self.fd.as_fd()
    }
}

impl Slave {
    /// The fresh pair generation retained through future spawn custody.
    pub fn generation(&self) -> Generation {
        self.generation
    }

    /// Native device/inode/rdev facts for future custody binding, not authority.
    pub fn device_identity(&self) -> (u64, u64, u64) {
        self.device
    }

    /// Transfer the freshly allocated slave into spawn custody.
    pub fn into_owned_fd(self) -> OwnedFd {
        self.fd
    }

    /// Owned single-writer process control, never a runtime vendor spawn path.
    #[cfg(debug_assertions)]
    #[doc(hidden)]
    pub fn fixture_writer(self, bytes: &[u8]) -> Result<FixtureWriter, FixtureRefusal> {
        fixture::spawn(self, bytes, false)
    }

    /// A fixed paused fixture for native custody refusal controls only.
    #[cfg(debug_assertions)]
    #[doc(hidden)]
    pub fn fixture_paused_writer(self) -> Result<FixtureWriter, FixtureRefusal> {
        fixture::spawn(self, &[], true)
    }
}

/// Independent nonblocking descriptions of the caller's one outer tty.
/// Opening these never changes the borrowed descriptors' open-file flags.
#[derive(Debug)]
pub struct Outer {
    input: OwnedFd,
    output: OwnedFd,
}

impl Outer {
    /// Refuse different/non-tty input and output, or a changed device/path.
    pub fn reopen(input: BorrowedFd<'_>, output: BorrowedFd<'_>) -> io::Result<Self> {
        // Opening ptmx by name can produce a DIFFERENT tty whose master fstat
        // device/inode is identical. Only outer slave/device roles qualify.
        if master_name(input.as_raw_fd()).is_ok() || master_name(output.as_raw_fd()).is_ok() {
            return Err(invalid("outer descriptors cannot be PTY masters"));
        }
        let expected = tty_identity(input.as_raw_fd())?;
        if tty_identity(output.as_raw_fd())? != expected {
            return Err(invalid("outer input and output must name the same tty"));
        }
        let path = tty_name(input.as_raw_fd())?;
        let open = |flags| -> io::Result<OwnedFd> {
            // SAFETY: path is a live NUL-terminated name; open allocates a new description.
            let fd = unsafe {
                libc::open(
                    path.as_ptr(),
                    flags | libc::O_NOCTTY | libc::O_CLOEXEC | libc::O_NONBLOCK | libc::O_NOFOLLOW,
                )
            };
            let fd = owned(fd)?;
            if tty_identity(fd.as_raw_fd())? != expected {
                return Err(invalid("reopened outer tty changed identity"));
            }
            Ok(fd)
        };
        let result = Self {
            input: open(libc::O_RDONLY)?,
            output: open(libc::O_WRONLY)?,
        };
        if tty_identity(input.as_raw_fd())? != expected
            || tty_identity(output.as_raw_fd())? != expected
        {
            return Err(invalid("borrowed outer tty changed during reopen"));
        }
        Ok(result)
    }
}

fn invalid(why: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, why)
}

fn owned(fd: RawFd) -> io::Result<OwnedFd> {
    if fd < 0 {
        Err(io::Error::last_os_error())
    } else {
        // SAFETY: callers pass only a newly allocated, uniquely owned descriptor.
        Ok(unsafe { OwnedFd::from_raw_fd(fd) })
    }
}

fn private(fd: RawFd, nonblocking: bool) -> io::Result<()> {
    // SAFETY: fcntl queries and sets integer flags on this live private descriptor.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
    // SAFETY: F_GETFL only queries the live descriptor's flags.
    let status = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 || status < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: preserve existing flags on a uniquely owned new descriptor.
    if unsafe { libc::fcntl(fd, libc::F_SETFD, flags | libc::FD_CLOEXEC) } < 0
        // SAFETY: this description is private, so changing O_NONBLOCK affects no borrowed fd.
        || (nonblocking && unsafe { libc::fcntl(fd, libc::F_SETFL, status | libc::O_NONBLOCK) } < 0)
    {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

fn tty_identity(fd: RawFd) -> io::Result<(u64, u64, u64)> {
    let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
    // SAFETY: stat is writable; isatty/fstat validate the live descriptor.
    if unsafe { libc::isatty(fd) } != 1 || unsafe { libc::fstat(fd, stat.as_mut_ptr()) } != 0 {
        return Err(invalid("descriptor is not a readable native tty identity"));
    }
    // SAFETY: successful fstat initialized stat.
    let stat = unsafe { stat.assume_init() };
    #[allow(clippy::unnecessary_cast)]
    Ok((stat.st_dev as u64, stat.st_ino as u64, stat.st_rdev as u64))
}

fn tty_name(fd: RawFd) -> io::Result<CString> {
    let mut name = [0; 1024];
    // SAFETY: name is writable storage of the specified length.
    let error = unsafe { libc::ttyname_r(fd, name.as_mut_ptr(), name.len()) };
    if error != 0 {
        return Err(io::Error::from_raw_os_error(error));
    }
    // SAFETY: ttyname_r succeeded and NUL-terminated within name.
    Ok(unsafe { CStr::from_ptr(name.as_ptr()) }.to_owned())
}

fn master_name(fd: RawFd) -> io::Result<CString> {
    let mut name = [0; 128];
    #[cfg(target_os = "macos")]
    // SAFETY: TIOCPTYGNAME writes exactly this 128-byte native buffer.
    let result = unsafe { libc::ioctl(fd, libc::TIOCPTYGNAME as libc::c_ulong, name.as_mut_ptr()) };
    #[cfg(not(target_os = "macos"))]
    // SAFETY: ptsname_r receives a live master and bounded writable name buffer.
    let result = unsafe { libc::ptsname_r(fd, name.as_mut_ptr(), name.len()) };
    if result != 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: the successful native call NUL-terminated name.
    Ok(unsafe { CStr::from_ptr(name.as_ptr()) }.to_owned())
}

fn next_generation() -> io::Result<Generation> {
    NEXT_GENERATION
        .try_update(Ordering::Relaxed, Ordering::Relaxed, |n| n.checked_add(1))
        .map(Generation)
        .map_err(|_| io::Error::other("PTY generation space exhausted"))
}

#[cfg(test)]
#[path = "inner_pty/tests.rs"]
mod tests;
