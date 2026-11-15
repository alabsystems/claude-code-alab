// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::{
    PtyPair, Size, invalid, master_name, next_generation, owned, private, tty_identity, tty_name,
};
#[cfg(test)]
use crate::sys::custody::AllSignals;
use std::collections::{BTreeMap, BTreeSet};
use std::io;
use std::os::fd::{AsRawFd, OwnedFd, RawFd};
use std::time::{Duration, Instant};

/// Allocation failure with explicit ownership of any still-unsettled helper.
#[derive(Debug)]
#[must_use = "an unsettled helper must be retained and positively reaped"]
pub struct AllocationError {
    /// The allocation, validation, or settlement failure.
    pub error: io::Error,
    /// Unsettled child lifetime or signal-mask ownership.
    pub pending: Option<Box<PendingAllocation>>,
}

impl From<io::Error> for AllocationError {
    fn from(error: io::Error) -> Self {
        Self {
            error,
            pending: None,
        }
    }
}

/// Exact child wait custody. Drop never blocks or claims that cleanup succeeded.
/// Each wait/kill transaction blocks all signals on a freshly checked single
/// thread and restores the prior mask before its bounded progress poll. Failed
/// mask restoration stays owned here for explicit recovery; ordinary pending
/// waits never retain a global block. Abandoning the receipt tries to restore
/// its thread mask without claiming child settlement. Lost custody disables kill.
#[derive(Debug)]
#[must_use = "retain this receipt until settle positively reaps the helper"]
pub struct PendingAllocation {
    custody: crate::sys::custody::OwnedChild,
    channel: Option<OwnedFd>,
}

impl PendingAllocation {
    /// The owned allocator pid, for on-demand diagnostics only.
    pub fn pid(&self) -> Option<u32> {
        self.custody.pid.map(|pid| pid as u32)
    }
    /// Whether another reaper removed wait custody. This permanently disables kill.
    pub fn ownership_lost(&self) -> bool {
        self.custody.lost
    }
    /// Kill the exact unreaped child and positively consume its terminal event.
    pub fn settle(mut self, deadline: Instant) -> Result<(), Box<Self>> {
        let settled =
            if self.custody.pid.is_none() && self.custody.status.is_none() && !self.custody.lost {
                self.restore().is_ok()
            } else {
                self.wait(deadline, true).is_ok() && self.restore().is_ok()
            };
        if settled { Ok(()) } else { Err(Box::new(self)) }
    }
    pub(super) fn begin() -> Result<Self, AllocationError> {
        let mut receipt = Self {
            custody: crate::sys::custody::OwnedChild::empty(),
            channel: None,
        };
        if let Err(error) = receipt.exclude() {
            return Err(AllocationError {
                error,
                pending: receipt.settle(Instant::now()).err(),
            });
        }
        Ok(receipt)
    }
    fn exclude(&mut self) -> io::Result<()> {
        self.custody.exclude()
    }
    pub(super) fn start(
        &mut self,
        pid: libc::pid_t,
        channel: OwnedFd,
        release: bool,
    ) -> io::Result<()> {
        self.channel = Some(channel);
        self.custody.start(pid)?;
        let go = [0u8];
        // SAFETY: private live nonblocking channel, one-byte bounded bootstrap;
        // the child remains gated until its native lifetime has been pinned.
        if unsafe {
            libc::send(
                self.channel
                    .as_ref()
                    .expect("installed channel")
                    .as_raw_fd(),
                go.as_ptr().cast(),
                1,
                0,
            )
        } != 1
        {
            return Err(io::Error::last_os_error());
        }
        if release {
            self.restore()?;
        }
        Ok(())
    }
    #[cfg(debug_assertions)]
    pub(super) fn fixture_current(&mut self) -> bool {
        self.custody.fixture_current()
    }
    #[cfg(debug_assertions)]
    pub(super) fn fixture_reused_pid(&mut self, replacement: &Self) {
        self.custody.fixture_reused_pid(&replacement.custody);
    }
    pub(super) fn restore(&mut self) -> io::Result<()> {
        self.custody.restore()
    }
    pub(super) fn wait(&mut self, deadline: Instant, kill: bool) -> io::Result<libc::c_int> {
        self.custody.wait(deadline, kill)
    }
}

#[cfg(test)]
#[path = "allocate_tests.rs"]
mod tests;

#[derive(Clone, Copy)]
pub(super) enum Fault {
    None,
    #[cfg(debug_assertions)]
    Block,
    #[cfg(debug_assertions)]
    Fail,
    #[cfg(debug_assertions)]
    WrongCount,
    #[cfg(debug_assertions)]
    Reaper,
    #[cfg(debug_assertions)]
    Truncated,
    #[cfg(debug_assertions)]
    TruncatedSweep,
}

impl Fault {
    fn reaper(self) -> bool {
        #[cfg(debug_assertions)]
        {
            matches!(self, Self::Reaper)
        }
        #[cfg(not(debug_assertions))]
        {
            let _ = self;
            false
        }
    }
}

/// Explicit process-isolated test controls, not a runtime or environment option.
#[cfg(debug_assertions)]
#[derive(Clone, Copy, Debug)]
pub enum AllocationControl {
    /// Allocate normally but block before any descriptor handoff.
    Block,
    /// Exit unsuccessfully before allocation.
    Fail,
    /// Send one descriptor rather than the required pair.
    WrongCount,
    /// Release an installed active SIGCHLD reaper before the custody wait.
    Reaper,
    /// Send more descriptor rights than the bounded receiver can hold.
    Truncated,
    /// Truncate rights and force allocation-free descriptor recovery.
    TruncatedSweep,
}

#[cfg(debug_assertions)]
impl From<AllocationControl> for Fault {
    fn from(value: AllocationControl) -> Self {
        match value {
            AllocationControl::Block => Self::Block,
            AllocationControl::Fail => Self::Fail,
            AllocationControl::WrongCount => Self::WrongCount,
            AllocationControl::Reaper => Self::Reaper,
            AllocationControl::Truncated => Self::Truncated,
            AllocationControl::TruncatedSweep => Self::TruncatedSweep,
        }
    }
}

pub(super) fn pair(
    size: Size,
    deadline: Instant,
    fault: Fault,
) -> Result<PtyPair, AllocationError> {
    let mut receipt = PendingAllocation::begin()?;
    if let Err(error) = retained_waits() {
        return failure(error, receipt, deadline);
    }
    let (parent, child) = match channel() {
        Ok(pair) => pair,
        Err(error) => return failure(error, receipt, deadline),
    };
    // SAFETY: ALL handlers remain blocked and the actual thread count is one.
    // Channel creation and this fork share that same unbroken exclusion.
    let pid = unsafe { libc::fork() };
    if pid < 0 {
        let error = io::Error::last_os_error();
        drop((parent, child));
        return failure(error, receipt, deadline);
    }
    if pid == 0 {
        drop(parent);
        helper(child.as_raw_fd(), size, fault, deadline);
    }
    drop(child);
    if let Err(error) = receipt.start(pid, parent, !fault.reaper()) {
        return failure(error, receipt, deadline);
    }
    let channel = receipt
        .channel
        .as_ref()
        .expect("channel just installed")
        .as_raw_fd();
    let result = (|| {
        let mut fds = [libc::pollfd {
            fd: channel,
            events: libc::POLLIN,
            revents: 0,
        }];
        super::relay::poll_until(&mut fds, deadline)?;
        if fds[0].revents & libc::POLLNVAL != 0 {
            return Err(invalid("allocator channel invalid"));
        }
        if fds[0].revents & libc::POLLIN == 0 {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "PTY allocation deadline",
            ));
        }
        let [master, slave] = receive(channel, &mut receipt, fault)?;
        private(master.as_raw_fd(), true)?;
        private(slave.as_raw_fd(), false)?;
        tty_identity(master.as_raw_fd())?;
        tty_identity(slave.as_raw_fd())?;
        if master_name(master.as_raw_fd())? != tty_name(slave.as_raw_fd())? {
            return Err(invalid("allocator did not return one PTY pair"));
        }
        let mut actual = std::mem::MaybeUninit::<libc::winsize>::uninit();
        // SAFETY: TIOCGWINSZ initializes the correctly sized native output buffer.
        if unsafe { libc::ioctl(master.as_raw_fd(), libc::TIOCGWINSZ, actual.as_mut_ptr()) } != 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: successful ioctl initialized actual.
        let actual = unsafe { actual.assume_init() };
        if actual.ws_row != size.rows || actual.ws_col != size.columns {
            return Err(invalid("allocated PTY dimensions changed"));
        }
        Ok(PtyPair {
            master,
            slave,
            generation: next_generation()?,
        })
    })();
    #[cfg(debug_assertions)]
    if fault.reaper() {
        // Hold SIGCHLD for this explicit fault until terminal native observation,
        // then let the test's real installed handler steal the event.
        while !crate::sys::child_has_ended(pid) {
            let now = Instant::now();
            if now >= deadline {
                return failure(
                    io::Error::new(io::ErrorKind::TimedOut, "reaper fixture deadline"),
                    receipt,
                    deadline,
                );
            }
            if let Err(error) =
                super::relay::poll_until(&mut [], deadline.min(now + Duration::from_millis(10)))
            {
                return failure(error, receipt, deadline);
            }
        }
        if let Err(error) = receipt.restore() {
            return failure(error, receipt, deadline);
        }
    }
    // FD receipt is insufficient: success requires a zero terminal wait event.
    match result {
        Ok(pair) => match receipt.wait(deadline, false) {
            Ok(status) => {
                if let Err(error) = receipt.restore() {
                    return Err(AllocationError {
                        error,
                        pending: Some(Box::new(receipt)),
                    });
                }
                if libc::WIFEXITED(status) && libc::WEXITSTATUS(status) == 0 {
                    Ok(pair)
                } else {
                    Err(io::Error::other("allocator failed after descriptor handoff").into())
                }
            }
            Err(error) => failure(error, receipt, deadline),
        },
        Err(error) => failure(error, receipt, deadline),
    }
}

fn failure<T>(
    error: io::Error,
    receipt: PendingAllocation,
    deadline: Instant,
) -> Result<T, AllocationError> {
    // Retain a distinct bounded settlement budget even if allocation used its deadline.
    let settlement = deadline.max(Instant::now() + Duration::from_millis(250));
    let pending = receipt.settle(settlement).err();
    Err(AllocationError { error, pending })
}

pub(super) fn retained_waits() -> io::Result<()> {
    let mut action = std::mem::MaybeUninit::<libc::sigaction>::uninit();
    // SAFETY: null action only queries SIGCHLD into writable native storage.
    if unsafe { libc::sigaction(libc::SIGCHLD, std::ptr::null(), action.as_mut_ptr()) } != 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: successful sigaction initialized action.
    let action = unsafe { action.assume_init() };
    if action.sa_sigaction == libc::SIG_IGN || action.sa_flags & libc::SA_NOCLDWAIT != 0 {
        return Err(invalid("allocator requires retained child wait ownership"));
    }
    Ok(())
}

pub(super) fn channel() -> io::Result<(OwnedFd, OwnedFd)> {
    let mut fds = [-1; 2];
    // SAFETY: socketpair initializes two independent owned descriptors.
    if unsafe { libc::socketpair(libc::AF_UNIX, libc::SOCK_DGRAM, 0, fds.as_mut_ptr()) } != 0 {
        return Err(io::Error::last_os_error());
    }
    let a = owned(fds[0])?;
    let b = owned(fds[1])?;
    private(a.as_raw_fd(), true)?;
    private(b.as_raw_fd(), true)?;
    Ok((a, b))
}

pub(super) fn await_start(channel: RawFd, deadline: Instant) -> bool {
    let mut fds = [libc::pollfd {
        fd: channel,
        events: libc::POLLIN,
        revents: 0,
    }];
    if super::relay::poll_until(&mut fds, deadline).is_err() || fds[0].revents & libc::POLLIN == 0 {
        return false;
    }
    let mut byte = [1u8; 2];
    // SAFETY: fixed live private channel and writable bounded bootstrap buffer.
    unsafe { libc::recv(channel, byte.as_mut_ptr().cast(), byte.len(), 0) == 1 && byte[0] == 0 }
}

fn helper(channel: RawFd, size: Size, fault: Fault, deadline: Instant) -> ! {
    // No inherited handler may write into the continuing supervisor's self-pipe.
    // SAFETY: initialized native set is filled and installed only in this child.
    unsafe {
        let mut all = std::mem::zeroed();
        libc::sigfillset(&raw mut all);
        if libc::sigprocmask(libc::SIG_SETMASK, &raw const all, std::ptr::null_mut()) != 0 {
            libc::_exit(1);
        }
    }
    if !await_start(channel, deadline) {
        // SAFETY: bounded bootstrap refusal never unwinds through the parent.
        unsafe {
            libc::_exit(1);
        }
    }
    match fault {
        Fault::None => {}
        #[cfg(debug_assertions)]
        Fault::Block => loop {
            // SAFETY: the owned helper waits for the parent's terminal SIGKILL.
            unsafe {
                libc::pause();
            }
        },
        #[cfg(debug_assertions)]
        Fault::Fail => {
            // SAFETY: this test helper has no caller-side cleanup to run.
            unsafe {
                libc::_exit(3);
            }
        }
        #[cfg(debug_assertions)]
        Fault::WrongCount => {}
        #[cfg(debug_assertions)]
        Fault::Reaper => {}
        #[cfg(debug_assertions)]
        Fault::Truncated => {}
        #[cfg(debug_assertions)]
        Fault::TruncatedSweep => {}
    }
    let mut master = -1;
    let mut slave = -1;
    let mut size = size.native();
    // SAFETY: native output integers and winsize live for the call. A transient
    // owned process contains Darwin's potentially unbounded grantpt/openpty.
    let result = unsafe {
        libc::openpty(
            &raw mut master,
            &raw mut slave,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &raw mut size,
        )
    };
    let normal = [master, slave];
    #[cfg(debug_assertions)]
    let extra = [master; 64];
    let descriptors = &normal[..];
    #[cfg(debug_assertions)]
    let descriptors = match fault {
        Fault::WrongCount => &normal[..1],
        Fault::Truncated | Fault::TruncatedSweep => &extra[..],
        _ => descriptors,
    };
    let ok = result == 0
        && private(master, false).is_ok()
        && private(slave, false).is_ok()
        && send(channel, descriptors).is_ok();
    // SAFETY: the helper never returns or unwinds through the caller's application.
    unsafe { libc::_exit(if ok { 0 } else { 1 }) }
}

// cmsghdr requires native alignment; u64 storage meets alignment on supported OSes.
fn send(channel: RawFd, fds: &[RawFd]) -> io::Result<()> {
    let mut byte = [1u8];
    let mut iov = libc::iovec {
        iov_base: byte.as_mut_ptr().cast(),
        iov_len: 1,
    };
    let mut control = [0u64; 40];
    if fds.len() > 64 {
        return Err(invalid("descriptor sender bound exceeded"));
    }
    // SAFETY: zero is a valid empty native msghdr representation.
    let mut msg: libc::msghdr = unsafe { std::mem::zeroed() };
    msg.msg_iov = &raw mut iov;
    msg.msg_iovlen = 1;
    msg.msg_control = control.as_mut_ptr().cast();
    // SAFETY: fds byte length fits the bounded native ancillary buffer.
    msg.msg_controllen = unsafe { libc::CMSG_SPACE(std::mem::size_of_val(fds) as _) } as _;
    // SAFETY: control is aligned writable storage with space for one rights record.
    unsafe {
        let cmsg = libc::CMSG_FIRSTHDR(&msg);
        (*cmsg).cmsg_level = libc::SOL_SOCKET;
        (*cmsg).cmsg_type = libc::SCM_RIGHTS;
        (*cmsg).cmsg_len = libc::CMSG_LEN(std::mem::size_of_val(fds) as _) as _;
        std::ptr::copy_nonoverlapping(fds.as_ptr(), libc::CMSG_DATA(cmsg).cast(), fds.len());
    }
    // SAFETY: msg references live bounded buffers for this one nonblocking send.
    if unsafe { libc::sendmsg(channel, &msg, 0) } != 1 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

fn receive(
    channel: RawFd,
    receipt: &mut PendingAllocation,
    fault: Fault,
) -> io::Result<[OwnedFd; 2]> {
    let outer_exclusion = receipt.custody.all_signals.is_some();
    receipt.exclude()?;
    let result = receive_protected(channel, fault);
    if !outer_exclusion {
        if let Some(mask) = &mut receipt.custody.all_signals {
            mask.restore()?;
        }
        receipt.custody.all_signals = None;
    }
    result
}

fn descriptors() -> io::Result<BTreeSet<RawFd>> {
    let directory = if cfg!(target_os = "linux") {
        "/proc/self/fd"
    } else {
        "/dev/fd"
    };
    let mut found = BTreeSet::new();
    for entry in std::fs::read_dir(directory)? {
        if let Some(fd) = entry?
            .file_name()
            .to_str()
            .and_then(|name| name.parse::<RawFd>().ok())
        {
            found.insert(fd);
        }
        if found.len() > 65_536 {
            return Err(invalid("descriptor inventory exceeds bound"));
        }
    }
    // The iterator's own fd is now closed; exclude it without a new open.
    found.retain(|fd| {
        // SAFETY: F_GETFD only tests whether this integer descriptor is live.
        unsafe { libc::fcntl(*fd, libc::F_GETFD) >= 0 }
    });
    Ok(found)
}

fn receive_protected(channel: RawFd, fault: Fault) -> io::Result<[OwnedFd; 2]> {
    let before = descriptors()?;
    let mut limit = std::mem::MaybeUninit::<libc::rlimit>::uninit();
    // SAFETY: writable native limit storage; no resource limit is changed.
    if unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, limit.as_mut_ptr()) } != 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: successful getrlimit initialized limit.
    let limit = unsafe { limit.assume_init() }.rlim_cur;
    if limit > 1_048_576 {
        return Err(invalid("descriptor recovery limit exceeds bound"));
    }
    let mut byte = [0u8; 2];
    let mut iov = libc::iovec {
        iov_base: byte.as_mut_ptr().cast(),
        iov_len: byte.len(),
    };
    let mut control = [0u64; 16];
    // SAFETY: zero initializes an empty native message whose pointers are set below.
    let mut msg: libc::msghdr = unsafe { std::mem::zeroed() };
    msg.msg_iov = &raw mut iov;
    msg.msg_iovlen = 1;
    msg.msg_control = control.as_mut_ptr().cast();
    msg.msg_controllen = std::mem::size_of_val(&control) as _;
    // SAFETY: every output buffer referenced by msg is writable and bounded.
    let n = unsafe { libc::recvmsg(channel, &raw mut msg, 0) };
    let receive_error = (n < 0).then(io::Error::last_os_error);
    // If EMFILE prevents opening /dev/fd after recv, F_GETFD needs no extra
    // descriptor. RLIMIT_NOFILE bounds every newly installed fd; originals
    // above a subsequently lowered limit remain in the baseline unchanged.
    let inventory = descriptors();
    #[cfg(debug_assertions)]
    let inventory = if matches!(fault, Fault::TruncatedSweep) {
        Err(io::Error::from_raw_os_error(libc::EMFILE))
    } else {
        inventory
    };
    #[cfg(not(debug_assertions))]
    let _ = fault;
    let after = inventory.unwrap_or_else(|_| {
        let mut after = before.clone();
        for fd in 0..limit as RawFd {
            // SAFETY: F_GETFD queries only this integer descriptor table slot;
            // all handlers are excluded and there is exactly one native thread.
            if unsafe { libc::fcntl(fd, libc::F_GETFD) } >= 0 {
                after.insert(fd);
            } else {
                after.remove(&fd);
            }
        }
        after
    });
    let installed: BTreeSet<_> = after.difference(&before).copied().collect();
    let mut owners = BTreeMap::new();
    for &fd in &installed {
        // These exact new slots came only from this protected recvmsg. Adopt
        // ALL of them, including Darwin's unreported truncated rights, before
        // any error return can restore handlers or let caller code open files.
        owners.insert(fd, owned(fd)?);
    }
    if n != 1
        || msg.msg_flags & (libc::MSG_CTRUNC | libc::MSG_TRUNC) != 0
        || msg.msg_controllen as usize > std::mem::size_of_val(&control)
    {
        return Err(receive_error.unwrap_or_else(|| {
            invalid("allocator descriptor envelope is truncated or malformed")
        }));
    }
    let mut received = Vec::new();
    let mut records = 0;
    let mut valid = byte[0] == 1;
    let supplied = std::mem::size_of_val(&control);
    let returned = msg.msg_controllen as usize;
    valid &= returned <= supplied;
    msg.msg_controllen = returned.min(supplied) as _;
    let end = control.as_ptr() as usize + returned.min(supplied);
    // SAFETY: each complete header and payload is checked against actual owned
    // storage before reading. No integer is adopted before the exact protected
    // descriptor inventory agrees with the envelope.
    unsafe {
        let mut cmsg = libc::CMSG_FIRSTHDR(&msg);
        while !cmsg.is_null() {
            records += 1;
            let len = (*cmsg).cmsg_len as usize;
            let head = libc::CMSG_LEN(0) as usize;
            let available = end.saturating_sub(cmsg as usize);
            if len < head || len > available {
                valid = false;
                break;
            }
            if (*cmsg).cmsg_level == libc::SOL_SOCKET
                && (*cmsg).cmsg_type == libc::SCM_RIGHTS
                && len >= head
                && available >= head
            {
                let payload = len - head;
                valid &= payload.is_multiple_of(std::mem::size_of::<RawFd>());
                for index in 0..payload / std::mem::size_of::<RawFd>() {
                    received.push(std::ptr::read_unaligned(
                        libc::CMSG_DATA(cmsg).cast::<RawFd>().add(index),
                    ));
                }
            } else {
                valid = false;
            }
            cmsg = libc::CMSG_NXTHDR(&msg, cmsg);
        }
    }
    let reported: BTreeSet<_> = received.iter().copied().collect();
    valid &= records == 1 && received.len() == 2 && reported.len() == 2 && installed == reported;
    if !valid {
        return Err(invalid("allocator descriptor envelope is malformed"));
    }
    for &fd in &received {
        private(fd, false)?;
    }
    let mut received = received.into_iter();
    Ok([
        owners
            .remove(&received.next().expect("length checked"))
            .expect("inventory equality checked"),
        owners
            .remove(&received.next().expect("length checked"))
            .expect("inventory equality checked"),
    ])
}
