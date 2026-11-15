// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Reversible, single-threaded custody of signal and terminal state.

use std::io;
use std::marker::PhantomData;
use std::os::fd::{AsRawFd, BorrowedFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::fs::{FileTypeExt as _, MetadataExt as _};
use std::rc::Rc;
use std::sync::atomic::{AtomicI32, Ordering};

/// Signals managed by the supervisor, in the snapshot bit order.
pub const HANDLED: &[libc::c_int] = &[
    libc::SIGINT,
    libc::SIGQUIT,
    libc::SIGHUP,
    libc::SIGTERM,
    libc::SIGTSTP,
    libc::SIGCONT,
    libc::SIGCHLD,
];
const SIGNAL_BITS: u8 = (1 << HANDLED.len()) - 1;
const CHILD_BIT: u8 = 1 << (HANDLED.len() - 1);
/// Each managed signal with its bit in a mask indexed by [`HANDLED`]. The bits
/// are computed at compile time, so a mask operation never shifts at run time.
pub const MANAGED: [(libc::c_int, u8); HANDLED.len()] = {
    let mut managed = [(0, 0); HANDLED.len()];
    let mut index = 0;
    while index < HANDLED.len() {
        managed[index] = (HANDLED[index], 1 << index);
        index += 1;
    }
    managed
};
static PIPE_WRITE: AtomicI32 = AtomicI32::new(-1);

fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

fn unsupported(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::Unsupported, message)
}

/// Require one native thread before changing process signal or inheritance state.
///
/// The caller must not create another thread until its blocked transfer is over.
/// Guards cannot be sent between threads. An unreadable count fails closed.
/// # Errors
/// The platform cannot establish a single native thread, or more than one exists.
pub fn ensure_single_threaded() -> io::Result<()> {
    if thread_count()? == 1 {
        Ok(())
    } else {
        Err(unsupported(
            "signal transfer requires exactly one native thread",
        ))
    }
}

/// Whether all managed signals are currently blocked on this native thread.
/// # Errors
/// The signal mask cannot be read.
pub fn managed_signals_blocked() -> io::Result<bool> {
    Ok(mask_bits(&current_mask()?)? == SIGNAL_BITS)
}

/// Block every managed signal for the rest of this process's life, keeping
/// unmanaged membership. A held handoff then takes each one with
/// [`take_pending`], whatever handlers or exec guards were in place.
/// # Errors
/// The signal mask cannot be read or set.
pub fn block_managed() -> io::Result<()> {
    let mut blocked = current_mask()?;
    replace_mask_bits(&mut blocked, SIGNAL_BITS)?;
    set_mask(&blocked)
}

/// Take the managed signals now pending, in the order they are taken, except
/// those in `launch_blocked` (indexed by [`HANDLED`]): the launch mask blocks
/// them, so supervision leaves them pending and so does this. Each one taken
/// is consumed and is never delivered later. A signal this process ignores is
/// consumed but not returned, so an inherited ignore stays ignored as in
/// ordinary supervision (Linux keeps a blocked ignored signal pending; macOS
/// discards it). Only a blocked signal stays pending, so call this after
/// [`block_managed`].
/// # Errors
/// The bits are not managed signals, the pending set or a disposition cannot
/// be read, or a signal cannot be taken.
pub fn take_pending(launch_blocked: u8) -> io::Result<Vec<libc::c_int>> {
    validate_bits(launch_blocked)?;
    let wanted = SIGNAL_BITS & !launch_blocked;
    // Any initialized set is storage here; it is emptied, then given exactly
    // the wanted managed signals.
    let mut managed = current_mask()?;
    // SAFETY: managed is an initialized, writable native set.
    if unsafe { libc::sigemptyset(&raw mut managed) } != 0 {
        return Err(io::Error::last_os_error());
    }
    replace_mask_bits(&mut managed, wanted)?;
    let mut taken = Vec::new();
    // One signal per round; the bound keeps a flood of signals from holding
    // the caller, which takes the rest on its next call.
    for _ in 0..2 * HANDLED.len() {
        let mut pending = managed;
        // SAFETY: pending is an initialized, writable native set.
        if unsafe { libc::sigpending(&raw mut pending) } != 0 {
            return Err(io::Error::last_os_error());
        }
        if mask_bits(&pending)? & wanted == 0 {
            break;
        }
        let mut got: libc::c_int = 0;
        // SAFETY: valid set and out-parameter. A wanted signal is pending and
        // every managed signal is blocked. The wait takes any of them, not only
        // the one seen: the kernel discards a pending CONT or TSTP only when a
        // stop or a CONT is generated, and a CONT is managed and then pending,
        // while a stop stops this process until a CONT that is then pending.
        let rc = unsafe { libc::sigwait(&raw const managed, &raw mut got) };
        if rc != 0 {
            return Err(io::Error::from_raw_os_error(rc));
        }
        if get_action(got)?.sa_sigaction != libc::SIG_IGN {
            taken.push(got);
        }
    }
    Ok(taken)
}

/// Read the signal numbers queued as bytes on a signal self-pipe's read end,
/// in order, without waiting.
#[must_use]
pub fn queued_signals(read: BorrowedFd<'_>) -> Vec<libc::c_int> {
    queued_on(read.as_raw_fd())
}

/// The inherited self-pipe read end `saved` records, checked as attachment
/// checks it (a nonblocking read-only FIFO with the recorded identity) so a
/// hold may drain it with [`queued_signals`] before any role is owned.
/// # Errors
/// The descriptor is standard, closed, or not that same read end.
pub fn inherited_read_end(saved: &SignalSnapshot) -> io::Result<RawFd> {
    validate_pipe_end(saved.read_fd, saved.read_identity, libc::O_RDONLY)?;
    Ok(saved.read_fd)
}

/// One bounded `poll` over the signal pipe's read end `fd` and, when given, one
/// descriptor of the caller's; then the queued signals, and whether `extra`
/// would not block a read. A negative `extra` is no descriptor and never ready
/// (`poll` ignores it). An interrupted or failed `poll` reports `extra` as not
/// ready: the caller's next wait asks again, and a signal that interrupted this
/// one is drained here.
fn wait_two(fd: RawFd, extra: Option<RawFd>, timeout_ms: u64) -> (Vec<libc::c_int>, bool) {
    let mut pfds = [
        libc::pollfd {
            fd,
            events: libc::POLLIN,
            revents: 0,
        },
        libc::pollfd {
            fd: extra.unwrap_or(-1),
            events: libc::POLLIN,
            revents: 0,
        },
    ];
    let count: libc::nfds_t = if extra.is_some() { 2 } else { 1 };
    let timeout = libc::c_int::try_from(timeout_ms).unwrap_or(libc::c_int::MAX);
    // SAFETY: pfds holds `count` initialized, writable poll records.
    let ready = unsafe { libc::poll(pfds.as_mut_ptr(), count, timeout) };
    let extra_ready = ready > 0
        && extra.is_some_and(|extra| extra >= 0)
        && pfds[1].revents & (libc::POLLIN | libc::POLLHUP | libc::POLLERR | libc::POLLNVAL) != 0;
    (queued_on(fd), extra_ready)
}

/// [`queued_signals`] on a read end the caller keeps open throughout.
fn queued_on(fd: RawFd) -> Vec<libc::c_int> {
    let mut out = Vec::new();
    let mut bytes = [0u8; 64];
    // A pipe holds a bounded number of bytes; the bound also keeps a writer
    // that never stops from holding the caller.
    for _ in 0..1024 {
        let mut pfd = libc::pollfd {
            fd,
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: one writable poll record for this open descriptor.
        let ready = unsafe { libc::poll(&raw mut pfd, 1, 0) };
        if ready < 0 && super::errno() == libc::EINTR {
            continue;
        }
        if ready <= 0 || pfd.revents & libc::POLLIN == 0 {
            break;
        }
        // SAFETY: bytes is writable for its supplied capacity; fd is open.
        let n = unsafe { libc::read(fd, bytes.as_mut_ptr().cast(), bytes.len()) };
        if n < 0 && super::errno() == libc::EINTR {
            continue;
        }
        let Some(read) = usize::try_from(n).ok().and_then(|n| bytes.get(..n)) else {
            break;
        };
        if read.is_empty() {
            break;
        }
        out.extend(read.iter().map(|&sig| libc::c_int::from(sig)));
    }
    out
}

/// Validate a raw descriptor without borrowing or taking ownership of it.
/// # Errors
/// The descriptor is not open or its flags cannot be read.
pub fn descriptor_flags(fd: RawFd) -> io::Result<libc::c_int> {
    fd_flags(fd)
}

/// Restore close-on-exec on one live borrowed descriptor without taking ownership.
/// # Errors
/// Descriptor flags cannot be read or changed.
pub fn restore_descriptor_cloexec(fd: BorrowedFd<'_>) -> io::Result<()> {
    set_fd_flags(fd.as_raw_fd(), fd_flags(fd.as_raw_fd())? | libc::FD_CLOEXEC)
}

#[cfg(target_os = "macos")]
fn thread_count() -> io::Result<usize> {
    let pid = libc::pid_t::try_from(std::process::id()).map_err(|_| invalid("invalid self pid"))?;
    let mut info = libc::proc_taskinfo::default();
    let size = libc::c_int::try_from(std::mem::size_of::<libc::proc_taskinfo>())
        .map_err(|_| invalid("invalid task-info size"))?;
    // SAFETY: info has exactly the writable layout this flavor fills; only an
    // exact-size result is read below. This does not create or suspend threads.
    let n =
        unsafe { libc::proc_pidinfo(pid, libc::PROC_PIDTASKINFO, 0, (&raw mut info).cast(), size) };
    if n != size {
        return Err(io::Error::last_os_error());
    }
    usize::try_from(info.pti_threadnum).map_err(|_| invalid("invalid native thread count"))
}

#[cfg(target_os = "linux")]
fn thread_count() -> io::Result<usize> {
    let mut count = 0;
    for entry in std::fs::read_dir("/proc/self/task")? {
        entry?;
        count += 1;
        if count > 1 {
            break;
        }
    }
    Ok(count)
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn thread_count() -> io::Result<usize> {
    Err(unsupported(
        "native signal-transfer thread counting is unavailable",
    ))
}

/// Identity read from one descriptor's native file metadata.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DescriptorIdentity {
    /// Device containing the object.
    pub device: u64,
    /// Object inode (each Darwin pipe endpoint has its own inode).
    pub inode: u64,
    /// Character-device identity, or the native zero for pipes.
    pub rdev: u64,
}

impl DescriptorIdentity {
    fn of(meta: &std::fs::Metadata) -> Self {
        Self {
            device: meta.dev(),
            inode: meta.ino(),
            rdev: meta.rdev(),
        }
    }
}

/// Read a descriptor's file identity without taking or changing its ownership.
/// # Errors
/// The descriptor is closed or its native metadata cannot be read.
pub fn descriptor_identity(fd: RawFd) -> io::Result<DescriptorIdentity> {
    metadata(fd).map(|meta| DescriptorIdentity::of(&meta))
}

fn metadata(fd: RawFd) -> io::Result<std::fs::Metadata> {
    // SAFETY: fcntl checks fd validity and returns a new owned duplicate; using
    // a duplicate keeps this function from borrowing an invalid raw descriptor.
    let copy = unsafe { libc::fcntl(fd, libc::F_DUPFD_CLOEXEC, 3) };
    if copy < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: copy is the newly allocated, uniquely owned descriptor above.
    std::fs::File::from(unsafe { OwnedFd::from_raw_fd(copy) }).metadata()
}

fn fd_flags(fd: RawFd) -> io::Result<libc::c_int> {
    // SAFETY: F_GETFD validates fd; it takes no pointer argument.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
    if flags < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(flags)
    }
}

fn set_fd_flags(fd: RawFd, flags: libc::c_int) -> io::Result<()> {
    // SAFETY: F_SETFD validates fd and takes only integer flags.
    if unsafe { libc::fcntl(fd, libc::F_SETFD, flags) } < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

fn status_flags(fd: RawFd) -> io::Result<libc::c_int> {
    // SAFETY: F_GETFL validates fd; it takes no pointer argument.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(flags)
    }
}

fn configure_pipe(fd: RawFd) -> io::Result<()> {
    let flags = status_flags(fd)?;
    // SAFETY: fd is an owned new pipe; F_SETFL receives integer status flags.
    if unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
        return Err(io::Error::last_os_error());
    }
    set_fd_flags(fd, fd_flags(fd)? | libc::FD_CLOEXEC)
}

fn validate_bits(bits: u8) -> io::Result<()> {
    if bits & !SIGNAL_BITS == 0 {
        Ok(())
    } else {
        Err(invalid("unknown signal snapshot bits"))
    }
}

fn inherited_policy(ignored: u8) -> u8 {
    // SIGCHLD must be caught even when inherited ignored: SIG_IGN would let
    // the kernel reap the child before the supervisor can preserve its exit.
    ignored & !CHILD_BIT
}

fn validate_policy(ignored: u8) -> io::Result<()> {
    validate_bits(ignored)?;
    if ignored & CHILD_BIT != 0 {
        return Err(invalid("SIGCHLD cannot be ignored by a supervising pipe"));
    }
    Ok(())
}

/// An empty signal set, built without `mem::zeroed` where the type allows.
#[cfg(any(target_os = "macos", target_os = "ios"))]
pub(super) const fn blank_sigset() -> libc::sigset_t {
    // Darwin's `sigset_t` is a `u32` bit set: <signal.h> defines
    // `sigemptyset(set)` as `*set = 0`.
    0
}

/// An empty signal set, built without `mem::zeroed` where the type allows.
#[cfg(not(any(target_os = "macos", target_os = "ios")))]
pub(super) fn blank_sigset() -> libc::sigset_t {
    // SAFETY: `sigset_t` is a plain array of integers; all-zero is a valid
    // value, and every caller fills it (sigprocmask, sigemptyset) before use.
    unsafe { std::mem::zeroed() }
}

/// A `sigaction` with no handler (`SIG_DFL`), an empty mask and no flags.
#[cfg(any(target_os = "macos", target_os = "ios"))]
pub(super) const fn blank_action() -> libc::sigaction {
    libc::sigaction {
        sa_sigaction: libc::SIG_DFL,
        sa_mask: blank_sigset(),
        sa_flags: 0,
    }
}

/// A `sigaction` with no handler (`SIG_DFL`), an empty mask and no flags.
#[cfg(not(any(target_os = "macos", target_os = "ios")))]
pub(super) fn blank_action() -> libc::sigaction {
    // SAFETY: `sigaction` is a plain C struct of a handler word, a signal set,
    // flags and a nullable restorer; all-zero is `SIG_DFL` with no flags.
    unsafe { std::mem::zeroed() }
}

/// Wait-record storage with every field zero, as `waitid` leaves it when no
/// child has changed state.
#[cfg(any(target_os = "macos", target_os = "ios"))]
pub(super) fn blank_siginfo() -> libc::siginfo_t {
    libc::siginfo_t::default()
}

/// Wait-record storage with every field zero, as `waitid` leaves it when no
/// child has changed state.
#[cfg(not(any(target_os = "macos", target_os = "ios")))]
pub(super) fn blank_siginfo() -> libc::siginfo_t {
    // SAFETY: `siginfo_t` is a plain C record of integers, a pointer and
    // padding; all-zero is a valid value (a null pid, no signal).
    unsafe { std::mem::zeroed() }
}

fn current_mask() -> io::Result<libc::sigset_t> {
    let mut old = blank_sigset();
    // SAFETY: a null new set only queries this thread's mask into valid storage.
    if unsafe { libc::sigprocmask(libc::SIG_BLOCK, std::ptr::null(), &raw mut old) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(old)
}

fn mask_bits(mask: &libc::sigset_t) -> io::Result<u8> {
    let mask = *mask;
    let mut bits = 0;
    for &(sig, bit) in &MANAGED {
        // SAFETY: mask is initialized and sig is a valid managed signal.
        match unsafe { libc::sigismember(&raw const mask, sig) } {
            0 => {}
            1 => bits |= bit,
            _ => return Err(io::Error::last_os_error()),
        }
    }
    Ok(bits)
}

fn replace_mask_bits(mask: &mut libc::sigset_t, bits: u8) -> io::Result<()> {
    validate_bits(bits)?;
    let mut changed = *mask;
    for &(sig, bit) in &MANAGED {
        // SAFETY: changed is initialized; each call only changes membership of a
        // valid managed signal, retaining every unmanaged signal's membership.
        let rc = unsafe {
            if bits & bit == 0 {
                libc::sigdelset(&raw mut changed, sig)
            } else {
                libc::sigaddset(&raw mut changed, sig)
            }
        };
        if rc != 0 {
            return Err(io::Error::last_os_error());
        }
    }
    *mask = changed;
    Ok(())
}

fn set_mask(mask: &libc::sigset_t) -> io::Result<()> {
    let mask = *mask;
    // SAFETY: mask is an initialized native set; no old-mask pointer is supplied.
    let rc = unsafe {
        libc::sigprocmask(
            libc::SIG_SETMASK,
            std::ptr::from_ref(&mask),
            std::ptr::null_mut(),
        )
    };
    if rc != 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

/// A managed-signal block, restored explicitly after adoption or on rollback.
/// Unmanaged signal membership is retained; no raw sigset bytes enter a capsule.
pub struct SignalBlock {
    previous: libc::sigset_t,
    bits: u8,
    active: bool,
    restore_on_drop: bool,
    _thread: PhantomData<Rc<()>>,
}

impl std::fmt::Debug for SignalBlock {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SignalBlock")
            .field("prior_mask", &self.bits)
            .field("active", &self.active)
            .finish()
    }
}

impl SignalBlock {
    /// Block managed signals for a single-threaded fork or another owned transaction.
    /// # Errors
    /// Native thread quiescence or signal-mask access cannot be established.
    pub fn managed() -> io::Result<Self> {
        Self::block()
    }

    fn block() -> io::Result<Self> {
        ensure_single_threaded()?;
        let previous = current_mask()?;
        let bits = mask_bits(&previous)?;
        let mut blocked = current_mask()?;
        replace_mask_bits(&mut blocked, SIGNAL_BITS)?;
        set_mask(&blocked)?;
        Ok(Self {
            previous,
            bits,
            active: true,
            restore_on_drop: true,
            _thread: PhantomData,
        })
    }

    fn inherited(bits: u8) -> io::Result<Self> {
        ensure_single_threaded()?;
        validate_bits(bits)?;
        let mut previous = current_mask()?;
        if mask_bits(&previous)? != SIGNAL_BITS {
            return Err(invalid("inherited managed signals are not all blocked"));
        }
        replace_mask_bits(&mut previous, bits)?;
        Ok(Self {
            previous,
            bits,
            active: true,
            restore_on_drop: true,
            _thread: PhantomData,
        })
    }

    /// Restore the prior mask only after handlers and the runtime are ready.
    /// # Errors
    /// Thread quiescence or native mask restoration cannot be established.
    pub fn restore(&mut self) -> io::Result<()> {
        if self.active {
            ensure_single_threaded()?;
            set_mask(&self.previous)?;
            self.active = false;
        }
        Ok(())
    }
}

impl Drop for SignalBlock {
    fn drop(&mut self) {
        if self.restore_on_drop {
            let _ = self.restore();
        }
    }
}

extern "C" fn on_signal(sig: libc::c_int) {
    let fd = PIPE_WRITE.load(Ordering::Relaxed);
    if fd < 0 {
        return;
    }
    let saved = super::errno();
    let byte = u8::try_from(sig).unwrap_or(0);
    // SAFETY: the pipe owner cannot close/reuse fd while a handler can run.
    // write is async-signal-safe; nonblocking full-pipe writes never wait.
    let _ = unsafe { libc::write(fd, (&raw const byte).cast(), 1) };
    super::set_errno(saved);
}

fn signal_action(ignored: bool) -> libc::sigaction {
    let mut action = blank_action();
    action.sa_sigaction = if ignored {
        libc::SIG_IGN
    } else {
        on_signal as extern "C" fn(libc::c_int) as libc::sighandler_t
    };
    action.sa_flags = if ignored { 0 } else { libc::SA_RESTART };
    // SAFETY: action.sa_mask is valid writable sigset_t storage.
    unsafe { libc::sigemptyset(&raw mut action.sa_mask) };
    action
}

fn get_action(sig: libc::c_int) -> io::Result<libc::sigaction> {
    let mut action = blank_action();
    // SAFETY: null new action queries the valid signal into writable storage.
    if unsafe { libc::sigaction(sig, std::ptr::null(), &raw mut action) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(action)
}

fn replace_action(sig: libc::c_int, action: &libc::sigaction) -> io::Result<libc::sigaction> {
    let action = *action;
    let mut old = blank_action();
    // SAFETY: action is initialized; old is writable native action storage.
    if unsafe { libc::sigaction(sig, &raw const action, &raw mut old) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(old)
}

fn install_actions(
    ignored: u8,
    mut replace: impl FnMut(libc::c_int, &libc::sigaction) -> io::Result<libc::sigaction>,
) -> io::Result<Vec<(libc::c_int, libc::sigaction)>> {
    validate_policy(ignored)?;
    let mut previous = Vec::new();
    for &(sig, bit) in &MANAGED {
        match replace(sig, &signal_action(ignored & bit != 0)) {
            Ok(old) => previous.push((sig, old)),
            Err(error) => {
                let mut rollback = None;
                for (sig, old) in previous.iter().rev() {
                    if let Err(restore) = replace(*sig, old) {
                        rollback.get_or_insert(restore);
                    }
                }
                if let Some(rollback) = rollback {
                    return Err(io::Error::other(format!(
                        "signal install failed: {error}; disposition rollback failed: {rollback}"
                    )));
                }
                return Err(error);
            }
        }
    }
    Ok(previous)
}

/// A checked transfer description; descriptor ownership stays with its pipe.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SignalSnapshot {
    /// Inherited read endpoint; never a standard descriptor.
    pub read_fd: RawFd,
    /// Inherited write endpoint; distinct from the read endpoint.
    pub write_fd: RawFd,
    /// The read endpoint originally captured by this supervisor.
    pub read_identity: DescriptorIdentity,
    /// The write endpoint originally captured by this supervisor.
    pub write_identity: DescriptorIdentity,
    /// Prior HANDLED mask membership, indexed by HANDLED.
    pub prior_mask: u8,
    /// Actual ignored dispositions, indexed by HANDLED; SIGCHLD is always caught.
    pub ignored: u8,
}

fn validate_pipe_end(
    fd: RawFd,
    expected: DescriptorIdentity,
    direction: libc::c_int,
) -> io::Result<()> {
    if fd < 3 {
        return Err(invalid("signal pipe cannot use standard descriptors"));
    }
    let meta = metadata(fd)?;
    let flags = status_flags(fd)?;
    if !meta.file_type().is_fifo()
        || DescriptorIdentity::of(&meta) != expected
        || flags & libc::O_ACCMODE != direction
        || flags & libc::O_NONBLOCK == 0
    {
        return Err(invalid(
            "signal pipe endpoint identity, direction or nonblocking state differs",
        ));
    }
    Ok(())
}

#[derive(Debug)]
struct FlagFailure {
    error: io::Error,
    restored: bool,
}

fn set_pair_flags(
    fds: [RawFd; 2],
    before: [libc::c_int; 2],
    after: [libc::c_int; 2],
    mut set: impl FnMut(RawFd, libc::c_int) -> io::Result<()>,
) -> Result<(), FlagFailure> {
    set(fds[0], after[0]).map_err(|error| FlagFailure {
        error,
        restored: true,
    })?;
    if let Err(error) = set(fds[1], after[1]) {
        return Err(match set(fds[0], before[0]) {
            Ok(()) => FlagFailure {
                error,
                restored: true,
            },
            Err(rollback) => FlagFailure {
                error: io::Error::other(format!(
                    "descriptor change failed: {error}; rollback failed: {rollback}"
                )),
                restored: false,
            },
        });
    }
    Ok(())
}

/// Owns both self-pipe endpoints and the process-global handler registration.
///
/// Drop closes only after proving single-threaded handler quiescence. If that
/// proof fails, both descriptors stay in process-lifetime custody rather than
/// letting a racing handler write to a reused fd. A second installation refuses.
pub struct SignalPipe {
    /// The read and write endpoints' numbers, fixed at installation: valid
    /// while `read` and `write` hold them, which is until `drop`.
    fds: [RawFd; 2],
    read: Option<OwnedFd>,
    write: Option<OwnedFd>,
    previous: Vec<(libc::c_int, libc::sigaction)>,
    ignored: u8,
    /// The launch mask's managed membership, indexed by HANDLED.
    prior_mask: u8,
    active: bool,
    restore_block: Option<super::AllSignals>,
    restore_caught: Vec<libc::c_int>,
    _thread: PhantomData<Rc<()>>,
}

impl std::fmt::Debug for SignalPipe {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SignalPipe")
            .field("read", &self.read)
            .field("write", &self.write)
            .field("ignored", &self.ignored)
            .finish()
    }
}

impl SignalPipe {
    const fn fds(&self) -> [RawFd; 2] {
        self.fds
    }

    /// Install with inherited ignores preserved, except SIGCHLD retains wait ownership.
    /// # Errors
    /// Another pipe exists, threads are not quiescent, or pipe/handler setup fails.
    pub fn install() -> io::Result<Self> {
        ensure_single_threaded()?;
        if PIPE_WRITE.load(Ordering::Relaxed) != -1 {
            return Err(invalid("a signal pipe is already installed"));
        }
        let mut block = SignalBlock::block()?;
        let mut raw = [0; 2];
        // SAFETY: raw is a writable two-descriptor output array.
        if unsafe { libc::pipe(raw.as_mut_ptr()) } != 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: pipe returned two distinct newly owned descriptors.
        let read = unsafe { OwnedFd::from_raw_fd(raw[0]) };
        // SAFETY: the other new pipe endpoint is independently owned.
        let write = unsafe { OwnedFd::from_raw_fd(raw[1]) };
        configure_pipe(raw[0])?;
        configure_pipe(raw[1])?;
        let mut ignored = 0;
        for &(sig, bit) in &MANAGED {
            if get_action(sig)?.sa_sigaction == libc::SIG_IGN {
                ignored |= bit;
            }
        }
        ignored = inherited_policy(ignored);
        let previous = install_actions(ignored, replace_action)?;
        PIPE_WRITE.store(raw[1], Ordering::Relaxed);
        let pipe = Self {
            fds: [read.as_raw_fd(), write.as_raw_fd()],
            read: Some(read),
            write: Some(write),
            previous,
            ignored,
            prior_mask: block.bits,
            active: true,
            restore_block: None,
            restore_caught: Vec::new(),
            _thread: PhantomData,
        };
        block.restore()?;
        Ok(pipe)
    }

    /// Block HANDLED and permit these same endpoints through exactly the pending exec attempt.
    /// No queued bytes are read or discarded. Dropping the guard rolls back.
    /// # Errors
    /// Thread quiescence, pipe identity, ownership or descriptor flags cannot be established.
    pub fn prepare_exec(&mut self) -> io::Result<SignalExecGuard<'_>> {
        if !self.active || self.restore_block.is_some() {
            return Err(invalid("signal scope is restored or restoring"));
        }
        ensure_single_threaded()?;
        let mut block = SignalBlock::block()?;
        let fds = self.fds();
        if PIPE_WRITE.load(Ordering::Relaxed) != fds[1] {
            return Err(invalid("this pipe does not own the signal handler"));
        }
        for &(sig, bit) in &MANAGED {
            let expected = signal_action(self.ignored & bit != 0);
            if get_action(sig)?.sa_sigaction != expected.sa_sigaction {
                return Err(invalid("a managed signal disposition changed before exec"));
            }
        }
        let snapshot = SignalSnapshot {
            read_fd: fds[0],
            write_fd: fds[1],
            read_identity: DescriptorIdentity::of(&metadata(fds[0])?),
            write_identity: DescriptorIdentity::of(&metadata(fds[1])?),
            prior_mask: block.bits,
            ignored: self.ignored,
        };
        validate_pipe_end(fds[0], snapshot.read_identity, libc::O_RDONLY)?;
        validate_pipe_end(fds[1], snapshot.write_identity, libc::O_WRONLY)?;
        let previous = [fd_flags(fds[0])?, fd_flags(fds[1])?];
        if let Err(failure) = set_pair_flags(
            fds,
            previous,
            previous.map(|flags| flags & !libc::FD_CLOEXEC),
            set_fd_flags,
        ) {
            if !failure.restored {
                block.restore_on_drop = false;
            }
            return Err(failure.error);
        }
        // If descriptor restoration fails, dropping the mask guard must not
        // release pending signals before the caller handles that failure.
        block.restore_on_drop = false;
        Ok(SignalExecGuard {
            pipe: self,
            block,
            previous,
            snapshot,
            active: true,
        })
    }

    /// Attach the same inherited endpoints while HANDLED remains blocked.
    /// CLOEXEC is restored before returning; the returned block is restored by
    /// the runtime only after its adoption is ready. No terminal modes are changed.
    ///
    /// # Safety
    /// The authenticated, validated capsule must name these uniquely owned
    /// endpoints from this supervisor's previous image. On success ownership
    /// transfers to Self; on error both raw descriptors remain caller-owned.
    /// No other code may close or use these endpoints during attachment.
    /// # Errors
    /// Quiescence, inherited mask/dispositions, identity, flags or handler setup differs.
    pub unsafe fn attach_inherited(saved: &SignalSnapshot) -> io::Result<(Self, SignalBlock)> {
        ensure_single_threaded()?;
        validate_policy(saved.ignored)?;
        validate_bits(saved.prior_mask)?;
        if PIPE_WRITE.load(Ordering::Relaxed) != -1 || saved.read_fd == saved.write_fd {
            return Err(invalid("duplicate signal owner or inherited endpoints"));
        }
        // Keep caller ownership on every failure, including local validation.
        validate_pipe_end(saved.read_fd, saved.read_identity, libc::O_RDONLY)?;
        validate_pipe_end(saved.write_fd, saved.write_identity, libc::O_WRONLY)?;
        for &(sig, bit) in &MANAGED {
            let expected = if saved.ignored & bit != 0 {
                libc::SIG_IGN
            } else {
                libc::SIG_DFL
            };
            if get_action(sig)?.sa_sigaction != expected {
                return Err(invalid(
                    "inherited signal disposition differs from the capsule",
                ));
            }
        }
        let mut block = SignalBlock::inherited(saved.prior_mask)?;
        // An attach failure must keep the inherited all-blocked mask for rollback.
        block.restore_on_drop = false;
        let fds = [saved.read_fd, saved.write_fd];
        let previous_flags = [fd_flags(fds[0])?, fd_flags(fds[1])?];
        set_pair_flags(
            fds,
            previous_flags,
            previous_flags.map(|flags| flags | libc::FD_CLOEXEC),
            set_fd_flags,
        )
        .map_err(|failure| failure.error)?;
        let previous = match install_actions(saved.ignored, replace_action) {
            Ok(previous) => previous,
            Err(error) => {
                if let Err(failure) = set_pair_flags(
                    fds,
                    previous_flags.map(|flags| flags | libc::FD_CLOEXEC),
                    previous_flags,
                    set_fd_flags,
                ) {
                    return Err(io::Error::other(format!(
                        "signal attach failed: {error}; {}",
                        failure.error
                    )));
                }
                return Err(error);
            }
        };
        PIPE_WRITE.store(saved.write_fd, Ordering::Relaxed);
        // SAFETY: the caller transfers these distinct endpoints on success only;
        // every fallible validation and mutation above has already completed.
        let read = unsafe { OwnedFd::from_raw_fd(saved.read_fd) };
        // SAFETY: independently transferred write endpoint, distinct from read.
        let write = unsafe { OwnedFd::from_raw_fd(saved.write_fd) };
        block.restore_on_drop = true;
        Ok((
            Self {
                fds: [read.as_raw_fd(), write.as_raw_fd()],
                read: Some(read),
                write: Some(write),
                previous,
                ignored: saved.ignored,
                prior_mask: saved.prior_mask,
                active: true,
                restore_block: None,
                restore_caught: Vec::new(),
                _thread: PhantomData,
            },
            block,
        ))
    }

    /// Wait at most timeout_ms and drain queued signal bytes, in order.
    #[must_use]
    pub fn wait(&self, timeout_ms: u64) -> Vec<libc::c_int> {
        let fd = self.read_fd();
        let mut pfd = libc::pollfd {
            fd,
            events: libc::POLLIN,
            revents: 0,
        };
        let timeout = libc::c_int::try_from(timeout_ms).unwrap_or(libc::c_int::MAX);
        // SAFETY: pfd is one writable poll descriptor for this owned pipe.
        let _ = unsafe { libc::poll(&raw mut pfd, 1, timeout) };
        // This pipe owns the read end for as long as it is borrowed here.
        queued_on(fd)
    }

    /// [`SignalPipe::wait`] that also wakes for one more descriptor: wait at most
    /// `timeout_ms` for a caught signal or for `extra` to become readable, then
    /// drain the queued signals, in order, and say whether `extra` is readable.
    ///
    /// The descriptor stays the caller's and is only polled, never read. It
    /// reads `true` for anything that keeps a read from blocking: bytes, the end
    /// of the stream, or an error on it, so the caller's own nonblocking read
    /// finds out which. With `None` this is exactly [`SignalPipe::wait`] and the
    /// flag is `false`.
    #[must_use]
    pub fn wait_with(&self, timeout_ms: u64, extra: Option<RawFd>) -> (Vec<libc::c_int>, bool) {
        wait_two(self.read_fd(), extra, timeout_ms)
    }

    /// The read endpoint, where caught signals wait as bytes.
    #[must_use]
    pub fn read_fd(&self) -> RawFd {
        self.read.as_ref().map_or(-1, AsRawFd::as_raw_fd)
    }

    /// The managed signals the launch mask blocks, indexed by [`HANDLED`].
    /// Supervision leaves them pending and never acts on them.
    #[must_use]
    pub const fn prior_mask(&self) -> u8 {
        self.prior_mask
    }

    pub(crate) fn retain_caught(&mut self, earlier: &[libc::c_int]) {
        let mut caught = earlier.to_vec();
        caught.append(&mut self.restore_caught);
        self.restore_caught = caught;
    }

    /// Drain caught signals, restore the original dispositions and close private
    /// descriptors under all-handler exclusion, then restore the original mask.
    /// A failure retains every still-live descriptor and the native mask guard.
    /// # Errors
    /// Native quiescence, handler ownership, action or mask restoration fails.
    pub fn restore_checked(&mut self) -> io::Result<Vec<libc::c_int>> {
        if !self.active {
            let caught = finish_signal_restore(&mut self.restore_caught, || {
                if let Some(block) = self.restore_block.as_mut() {
                    block.restore()?;
                }
                Ok(())
            })?;
            self.restore_block = None;
            return Ok(caught);
        }
        if self.restore_block.is_none() {
            self.restore_block = Some(super::AllSignals::block()?);
        }
        ensure_single_threaded()?;
        if PIPE_WRITE.load(Ordering::Relaxed) != self.fds()[1] {
            return Err(invalid("this scope no longer owns the signal handler"));
        }
        for (sig, old) in self.previous.iter().rev() {
            replace_action(*sig, old)?;
        }
        self.restore_caught.extend(queued_on(self.read_fd()));
        PIPE_WRITE.store(-1, Ordering::Relaxed);
        self.active = false;
        drop(self.read.take());
        drop(self.write.take());
        self.previous.clear();
        let caught = finish_signal_restore(&mut self.restore_caught, || {
            self.restore_block
                .as_mut()
                .expect("restoration block retained")
                .restore()
        })?;
        self.restore_block = None;
        Ok(caught)
    }
}

fn finish_signal_restore(
    caught: &mut Vec<libc::c_int>,
    restore: impl FnOnce() -> io::Result<()>,
) -> io::Result<Vec<libc::c_int>> {
    restore()?;
    Ok(std::mem::take(caught))
}

#[cfg(test)]
#[path = "signal_scope_tests.rs"]
mod signal_scope_tests;

impl Drop for SignalPipe {
    fn drop(&mut self) {
        if self.restore_checked().is_err() {
            if let Some(read) = self.read.take() {
                std::mem::forget(read);
            }
            if let Some(write) = self.write.take() {
                std::mem::forget(write);
            }
            if let Some(block) = self.restore_block.take() {
                // The abandoned pipe's descriptors are deliberately kept, so
                // its handlers cannot touch recycled descriptors. Unlike a live
                // returned refusal, Drop owns no future mask-restoration receipt.
                drop(block);
            }
        }
    }
}

/// Temporary inheritance and mask changes for one pending exec syscall.
#[derive(Debug)]
pub struct SignalExecGuard<'a> {
    pipe: &'a mut SignalPipe,
    block: SignalBlock,
    previous: [libc::c_int; 2],
    snapshot: SignalSnapshot,
    active: bool,
}

impl SignalExecGuard<'_> {
    /// Debug fixture observation of the actual endpoint flag words saved by this guard.
    #[cfg(debug_assertions)]
    #[doc(hidden)]
    #[must_use]
    pub fn fixture_original_flags(&self) -> [(RawFd, libc::c_int); 2] {
        let fds = self.pipe.fds();
        [(fds[0], self.previous[0]), (fds[1], self.previous[1])]
    }
    /// The explicit state to encode in the validated exec capsule.
    #[must_use]
    pub const fn snapshot(&self) -> SignalSnapshot {
        self.snapshot
    }

    /// Restore both endpoint flags before restoring the prior signal mask.
    /// # Errors
    /// Thread quiescence, descriptor flag restoration or mask restoration fails.
    pub fn rollback(&mut self) -> io::Result<()> {
        if !self.active {
            return Ok(());
        }
        ensure_single_threaded()?;
        let fds = self.pipe.fds();
        let first = set_fd_flags(fds[0], self.previous[0]);
        let second = set_fd_flags(fds[1], self.previous[1]);
        first?;
        second?;
        self.block.restore()?;
        self.active = false;
        Ok(())
    }
}

impl Drop for SignalExecGuard<'_> {
    fn drop(&mut self) {
        let _ = self.rollback();
    }
}

/// Temporarily inherit one borrowed descriptor; neither failure nor restoration closes it.
/// All prior descriptor flags are restored, not merely CLOEXEC's current bit.
/// # Errors
/// The process is not single-threaded, HANDLED is not blocked, or flag access fails.
pub fn inherit_for_one_exec(fd: BorrowedFd<'_>) -> io::Result<DescriptorExecGuard<'_>> {
    ensure_single_threaded()?;
    if mask_bits(&current_mask()?)? != SIGNAL_BITS {
        return Err(invalid(
            "managed signals must be blocked before descriptor inheritance",
        ));
    }
    let previous = fd_flags(fd.as_raw_fd())?;
    set_fd_flags(fd.as_raw_fd(), previous & !libc::FD_CLOEXEC)?;
    Ok(DescriptorExecGuard {
        fd,
        previous,
        active: true,
        _thread: PhantomData,
    })
}

/// One borrowed descriptor's reversible inheritance flag change.
#[derive(Debug)]
pub struct DescriptorExecGuard<'a> {
    fd: BorrowedFd<'a>,
    previous: libc::c_int,
    active: bool,
    _thread: PhantomData<Rc<()>>,
}

impl AsRawFd for DescriptorExecGuard<'_> {
    fn as_raw_fd(&self) -> RawFd {
        self.fd.as_raw_fd()
    }
}

impl DescriptorExecGuard<'_> {
    /// Debug fixture observation of the actual flag word saved by this guard.
    #[cfg(debug_assertions)]
    #[doc(hidden)]
    #[must_use]
    pub fn fixture_original_flags(&self) -> (RawFd, libc::c_int) {
        (self.as_raw_fd(), self.previous)
    }
    /// Restore the exact prior flags after a returned exec syscall.
    /// # Errors
    /// Thread quiescence or flag restoration fails; ownership remains with the borrower.
    pub fn restore(&mut self) -> io::Result<()> {
        if self.active {
            ensure_single_threaded()?;
            set_fd_flags(self.as_raw_fd(), self.previous)?;
            self.active = false;
        }
        Ok(())
    }
}

impl Drop for DescriptorExecGuard<'_> {
    fn drop(&mut self) {
        let _ = self.restore();
    }
}

/// A lossless, explicit terminal-mode snapshot, never native struct bytes or padding.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TermiosSnapshot {
    /// Target/ABI and field-shape discriminator.
    pub abi: String,
    /// The terminal originally saved before the vendor entered raw mode.
    pub terminal: DescriptorIdentity,
    /// Input, output, control and local flags, in that order.
    pub flags: [u64; 4],
    /// Exact platform NCCS control bytes, including zero and disabled controls.
    pub controls: Vec<u8>,
    /// Linux line discipline; absent on Apple.
    pub line: Option<u8>,
    /// Explicit raw speed fields where this platform's termios has them.
    pub raw_speeds: Option<[u64; 2]>,
}

/// The terminal's original modes and descriptor identity, saved before child startup.
#[derive(Clone, Copy)]
pub struct Termios {
    fd: RawFd,
    terminal: DescriptorIdentity,
    modes: libc::termios,
}

impl std::fmt::Debug for Termios {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Termios")
            .field("fd", &self.fd)
            .field("terminal", &self.terminal)
            .finish()
    }
}

fn terminal_identity(fd: RawFd) -> io::Result<DescriptorIdentity> {
    let meta = metadata(fd)?;
    if !meta.file_type().is_char_device() {
        return Err(invalid("saved terminal is not a character device"));
    }
    Ok(DescriptorIdentity::of(&meta))
}

fn wide<T: Into<u64>>(value: T) -> u64 {
    value.into()
}

fn narrow<T: TryFrom<u64>>(value: u64) -> io::Result<T> {
    T::try_from(value).map_err(|_| invalid("terminal mode integer exceeds the native width"))
}

pub(super) fn termios_abi() -> io::Result<String> {
    if !cfg!(any(
        target_os = "macos",
        target_os = "ios",
        all(
            target_os = "linux",
            any(target_env = "gnu", target_env = "musl")
        )
    )) {
        return Err(unsupported(
            "terminal snapshots are unavailable for this ABI",
        ));
    }
    let environment = if cfg!(target_env = "musl") {
        "musl"
    } else if cfg!(target_env = "gnu") {
        "gnu"
    } else {
        "apple"
    };
    Ok(format!(
        "termios-1:{}:{}:{environment}:{}:{}:{}",
        std::env::consts::OS,
        std::env::consts::ARCH,
        std::mem::size_of::<libc::tcflag_t>(),
        std::mem::size_of::<libc::speed_t>(),
        libc::NCCS
    ))
}

#[cfg(any(target_os = "macos", target_os = "ios"))]
fn extra_modes(modes: &libc::termios) -> (Option<u8>, Option<[u64; 2]>) {
    (None, Some([wide(modes.c_ispeed), wide(modes.c_ospeed)]))
}

#[cfg(all(target_os = "linux", target_env = "gnu"))]
fn extra_modes(modes: &libc::termios) -> (Option<u8>, Option<[u64; 2]>) {
    #[cfg(not(any(
        target_arch = "sparc",
        target_arch = "sparc64",
        target_arch = "mips",
        target_arch = "mips32r6",
        target_arch = "mips64",
        target_arch = "mips64r6"
    )))]
    let speeds = Some([wide(modes.c_ispeed), wide(modes.c_ospeed)]);
    #[cfg(any(
        target_arch = "sparc",
        target_arch = "sparc64",
        target_arch = "mips",
        target_arch = "mips32r6",
        target_arch = "mips64",
        target_arch = "mips64r6"
    ))]
    let speeds = None;
    (Some(modes.c_line), speeds)
}

#[cfg(all(target_os = "linux", target_env = "musl"))]
fn extra_modes(modes: &libc::termios) -> (Option<u8>, Option<[u64; 2]>) {
    (
        Some(modes.c_line),
        Some([wide(modes.__c_ispeed), wide(modes.__c_ospeed)]),
    )
}

#[cfg(not(any(
    target_os = "macos",
    target_os = "ios",
    all(target_os = "linux", any(target_env = "gnu", target_env = "musl"))
)))]
fn extra_modes(_modes: &libc::termios) -> (Option<u8>, Option<[u64; 2]>) {
    (None, None)
}

fn decode_modes(saved: &TermiosSnapshot) -> io::Result<libc::termios> {
    if saved.abi != termios_abi()? || saved.controls.len() != libc::NCCS {
        return Err(invalid("terminal snapshot ABI or control count differs"));
    }
    let mut modes = blank_termios();
    let shape = extra_modes(&modes);
    if saved.line.is_some() != shape.0.is_some() || saved.raw_speeds.is_some() != shape.1.is_some()
    {
        return Err(invalid(
            "terminal snapshot optional fields differ from the native layout",
        ));
    }
    modes.c_iflag = narrow(saved.flags[0])?;
    modes.c_oflag = narrow(saved.flags[1])?;
    modes.c_cflag = narrow(saved.flags[2])?;
    modes.c_lflag = narrow(saved.flags[3])?;
    modes.c_cc.copy_from_slice(&saved.controls);
    #[cfg(target_os = "linux")]
    {
        modes.c_line = saved
            .line
            .ok_or_else(|| invalid("terminal line discipline missing"))?;
    }
    #[cfg(any(
        target_os = "macos",
        target_os = "ios",
        all(
            target_os = "linux",
            target_env = "gnu",
            not(any(
                target_arch = "sparc",
                target_arch = "sparc64",
                target_arch = "mips",
                target_arch = "mips32r6",
                target_arch = "mips64",
                target_arch = "mips64r6"
            ))
        )
    ))]
    {
        let speeds = saved
            .raw_speeds
            .ok_or_else(|| invalid("terminal speed storage missing"))?;
        modes.c_ispeed = narrow(speeds[0])?;
        modes.c_ospeed = narrow(speeds[1])?;
    }
    #[cfg(all(target_os = "linux", target_env = "musl"))]
    {
        let speeds = saved
            .raw_speeds
            .ok_or_else(|| invalid("terminal speed storage missing"))?;
        modes.__c_ispeed = narrow(speeds[0])?;
        modes.__c_ospeed = narrow(speeds[1])?;
    }
    Ok(modes)
}

/// Terminal-mode storage with every field zero; snapshots assign each native
/// semantic field explicitly.
#[cfg(any(target_os = "macos", target_os = "ios"))]
const fn blank_termios() -> libc::termios {
    libc::termios {
        c_iflag: 0,
        c_oflag: 0,
        c_cflag: 0,
        c_lflag: 0,
        c_cc: [0; libc::NCCS],
        c_ispeed: 0,
        c_ospeed: 0,
    }
}

/// Terminal-mode storage with every field zero; snapshots assign each native
/// semantic field explicitly.
#[cfg(not(any(target_os = "macos", target_os = "ios")))]
fn blank_termios() -> libc::termios {
    // SAFETY: all fields are integer scalars/arrays. Zero is a valid value for
    // constructing storage.
    unsafe { std::mem::zeroed() }
}

/// What [`Termios::reset_input_modes`] writes: the kitty keyboard flags set to
/// none and their stack popped (`CSI = 0 u`, `CSI < 99 u`) on the screen in
/// use, then on the normal screen after leaving the alternate one (`CSI ? 1049
/// l`; aterm keeps a stack per screen); modifyOtherKeys reset (`CSI > 4 m`);
/// mouse, focus and bracketed-paste reporting off; the cursor shown.
pub const INPUT_MODE_RESET: &[u8] = b"\x1b[=0u\x1b[<99u\x1b[?1049l\x1b[=0u\x1b[<99u\x1b[>4m\
    \x1b[?1000l\x1b[?1002l\x1b[?1003l\x1b[?1006l\x1b[?1004l\x1b[?2004l\x1b[?25h";

impl Termios {
    /// Save original modes once, before a child can put the terminal in raw mode.
    #[must_use]
    pub fn save(fd: RawFd) -> Option<Self> {
        let terminal = terminal_identity(fd).ok()?;
        let mut modes = blank_termios();
        // SAFETY: modes is writable termios storage; the kernel validates fd.
        if unsafe { libc::tcgetattr(fd, &raw mut modes) } != 0 {
            return None;
        }
        if terminal_identity(fd).ok()? != terminal {
            return None;
        }
        Some(Self {
            fd,
            terminal,
            modes,
        })
    }

    /// Export original saved modes, without reading the child's current modes.
    /// # Errors
    /// This platform's exact termios field layout is unsupported.
    pub fn snapshot(&self) -> io::Result<TermiosSnapshot> {
        let (line, raw_speeds) = extra_modes(&self.modes);
        Ok(TermiosSnapshot {
            abi: termios_abi()?,
            terminal: self.terminal,
            flags: [
                wide(self.modes.c_iflag),
                wide(self.modes.c_oflag),
                wide(self.modes.c_cflag),
                wide(self.modes.c_lflag),
            ],
            controls: self.modes.c_cc.to_vec(),
            line,
            raw_speeds,
        })
    }

    /// Reconstruct saved original modes for the same terminal. This performs
    /// only fresh file-metadata validation, never tcgetattr or tcsetattr.
    /// # Errors
    /// The original terminal identity, ABI, field shape or integer widths differ.
    pub fn from_snapshot(fd: RawFd, saved: &TermiosSnapshot) -> io::Result<Self> {
        Self::from_snapshot_using(fd, saved, terminal_identity)
    }

    fn from_snapshot_using(
        fd: RawFd,
        saved: &TermiosSnapshot,
        identity: impl FnOnce(RawFd) -> io::Result<DescriptorIdentity>,
    ) -> io::Result<Self> {
        if identity(fd)? != saved.terminal {
            return Err(invalid("the original terminal descriptor was replaced"));
        }
        Ok(Self {
            fd,
            terminal: saved.terminal,
            modes: decode_modes(saved)?,
        })
    }

    /// Put the saved original modes back once no live child owns the terminal:
    /// after the child dies by a signal (ordinary supervision's end, or a held
    /// image handoff's `park`), and before a vendor relaunch or the Decision 28
    /// fallback starts the next child. Never while a live child holds the
    /// terminal, as during an image transfer.
    /// # Errors
    /// The terminal changed, this process is in the background, or tcsetattr fails.
    pub fn restore_checked(&self) -> io::Result<()> {
        if terminal_identity(self.fd)? != self.terminal {
            return Err(invalid("the saved terminal descriptor changed"));
        }
        if !super::in_foreground(self.fd) {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "terminal is not foreground",
            ));
        }
        // SAFETY: modes came from tcgetattr or a lossless validated snapshot;
        // the descriptor still names the original foreground terminal.
        if unsafe { libc::tcsetattr(self.fd, libc::TCSANOW, &raw const self.modes) } != 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(())
        }
    }

    /// Best-effort [`Self::restore_checked`], for the same uses.
    pub fn restore(&self) {
        let _ = self.restore_checked();
    }

    /// Whether the saved descriptor still names the saved terminal and this
    /// process's group is its foreground group: a write or a flush from a
    /// background group would stop the harness (`SIGTTOU`).
    fn held_here(&self) -> bool {
        terminal_identity(self.fd).is_ok_and(|now| now == self.terminal)
            && super::in_foreground(self.fd)
    }

    /// Put the terminal's keyboard and pointer reporting back to its defaults
    /// once no live child owns it, best effort, so a key sends what a cooked
    /// terminal expects: Ctrl-C a `0x03` the line discipline turns into
    /// `SIGINT`. Claude Code pushes the kitty keyboard flags and
    /// modifyOtherKeys and pops them only on a clean exit; under them aterm
    /// sends Ctrl-C as `ESC [ 9 9 ; 5 u` (`aterm-types` keyboard tests), and
    /// aterm reverts a dead program's modes only when its group's leader is
    /// gone, which the harness is not. [`INPUT_MODE_RESET`] is written to the
    /// terminal; nothing is written from the background.
    pub fn reset_input_modes(&self) {
        if !self.held_here() {
            return;
        }
        let mut rest = INPUT_MODE_RESET;
        while !rest.is_empty() {
            // SAFETY: `rest` is a valid byte slice; fd names the saved terminal.
            let n = unsafe { libc::write(self.fd, rest.as_ptr().cast(), rest.len()) };
            match usize::try_from(n) {
                Ok(written) if written > 0 => rest = rest.get(written..).unwrap_or_default(),
                _ if super::errno() == libc::EINTR => {}
                _ => return,
            }
        }
    }

    /// Discard what was typed on the terminal and not read, best effort
    /// (`tcflush(TCIFLUSH)`), once no live child owns it: input meant for a
    /// program that is gone must reach neither the shell nor the program
    /// started next. Nothing is done from the background.
    pub fn discard_input(&self) {
        if self.held_here() {
            // SAFETY: tcflush only drops the input queue of the saved terminal.
            let _ = unsafe { libc::tcflush(self.fd, libc::TCIFLUSH) };
        }
    }

    /// Whether the terminal still holds the saved canonical (`ICANON`) mode
    /// (for example, before an interactive child enters raw mode).
    #[must_use]
    pub fn is_original(&self) -> bool {
        let Some(current) = Self::save(self.fd) else {
            return true;
        };
        (current.modes.c_lflag & libc::ICANON) == (self.modes.c_lflag & libc::ICANON)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fresh pipe, `(read, write)`, for the two-descriptor wait.
    fn test_pipe() -> (OwnedFd, OwnedFd) {
        let mut fds = [-1; 2];
        // SAFETY: fds is writable for the two descriptors pipe stores.
        assert_eq!(unsafe { libc::pipe(fds.as_mut_ptr()) }, 0);
        // A child another test starts must not carry these across its exec.
        for fd in fds {
            // SAFETY: fd is a descriptor pipe just returned to this test.
            let set = unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) };
            assert_eq!(set, 0);
        }
        // SAFETY: pipe returned two new descriptors this test owns.
        unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) }
    }

    fn put(fd: &OwnedFd, bytes: &[u8]) {
        // SAFETY: bytes is readable for its length; fd is this test's open pipe.
        let wrote = unsafe { libc::write(fd.as_raw_fd(), bytes.as_ptr().cast(), bytes.len()) };
        assert_eq!(usize::try_from(wrote).ok(), Some(bytes.len()));
    }

    #[test]
    fn the_two_descriptor_wait_drains_signals_and_reports_the_other_descriptor() {
        let (signals, signal_writer) = test_pipe();
        let (events, event_writer) = test_pipe();
        let (fd, extra) = (signals.as_raw_fd(), Some(events.as_raw_fd()));
        // Nothing queued: a zero wait returns at once with neither.
        assert_eq!(wait_two(fd, extra, 0), (Vec::new(), false));
        assert_eq!(wait_two(fd, None, 0), (Vec::new(), false));
        // Signals only, in order; the other descriptor is not readable.
        put(&signal_writer, &[libc::SIGTERM as u8, libc::SIGCHLD as u8]);
        assert_eq!(
            wait_two(fd, extra, 0),
            (vec![libc::SIGTERM, libc::SIGCHLD], false)
        );
        // The other descriptor only: reported, and never read from.
        put(&event_writer, b"EVENT 1 human\n");
        assert_eq!(wait_two(fd, extra, 0), (Vec::new(), true));
        assert_eq!(wait_two(fd, extra, 0), (Vec::new(), true));
        // Both at once.
        put(&signal_writer, &[libc::SIGHUP as u8]);
        assert_eq!(wait_two(fd, extra, 0), (vec![libc::SIGHUP], true));
        // Without the descriptor it is the plain wait: its bytes wake nothing.
        assert_eq!(wait_two(fd, None, 0), (Vec::new(), false));
        put(&signal_writer, &[libc::SIGINT as u8]);
        assert_eq!(wait_two(fd, None, 0), (vec![libc::SIGINT], false));
    }

    #[test]
    fn the_other_descriptor_wakes_a_long_wait_and_its_end_reads_as_ready() {
        // An interrupted poll answers "nothing yet" and the supervisor's loop
        // asks again; this asks again the same way, until the time is spent.
        fn wait_two(fd: RawFd, extra: Option<RawFd>, timeout_ms: u64) -> (Vec<libc::c_int>, bool) {
            let started = std::time::Instant::now();
            loop {
                let got = super::wait_two(fd, extra, timeout_ms);
                let spent = started.elapsed() >= std::time::Duration::from_millis(timeout_ms);
                if !got.0.is_empty() || got.1 || spent {
                    return got;
                }
            }
        }
        let (signals, signal_writer) = test_pipe();
        let (events, event_writer) = test_pipe();
        let (fd, extra) = (signals.as_raw_fd(), Some(events.as_raw_fd()));
        // An idle pair waits out its timeout, and no longer than the hang detector.
        let started = std::time::Instant::now();
        assert_eq!(wait_two(fd, extra, 30), (Vec::new(), false));
        let waited = started.elapsed();
        assert!(waited >= std::time::Duration::from_millis(25), "{waited:?}");
        // A byte on the other descriptor ends a wait that would last a minute.
        let writer = std::thread::spawn(move || {
            put(&event_writer, b"x");
            event_writer
        });
        let started = std::time::Instant::now();
        assert_eq!(wait_two(fd, extra, 60_000), (Vec::new(), true));
        assert!(started.elapsed() < std::time::Duration::from_secs(30));
        // So does a signal byte, which leaves the other flag to what is queued.
        let event_writer = writer.join().expect("writer");
        let mut byte = [0_u8; 1];
        // SAFETY: byte is writable for one byte; events is this test's open pipe.
        let taken = unsafe { libc::read(events.as_raw_fd(), byte.as_mut_ptr().cast(), 1) };
        assert_eq!(taken, 1);
        let signaller = std::thread::spawn(move || {
            put(&signal_writer, &[libc::SIGTERM as u8]);
            signal_writer
        });
        let started = std::time::Instant::now();
        assert_eq!(wait_two(fd, extra, 60_000), (vec![libc::SIGTERM], false));
        assert!(started.elapsed() < std::time::Duration::from_secs(30));
        drop(signaller.join().expect("signaller"));
        // The writer gone: end of stream is readable, so the caller's read sees it.
        // A child that another test forked while this pipe was being made holds
        // a copy of the writer until its exec, so the end may arrive a moment late.
        drop(event_writer);
        assert_eq!(wait_two(fd, extra, 10_000), (Vec::new(), true));
        // A negative descriptor is no descriptor at all.
        assert_eq!(wait_two(fd, Some(-1), 0), (Vec::new(), false));
    }

    #[test]
    fn inherited_ignores_survive_but_child_wait_ownership_is_retained() {
        let policy = inherited_policy(SIGNAL_BITS);
        assert_eq!(policy, SIGNAL_BITS & !CHILD_BIT);
        assert!(validate_policy(policy).is_ok());
        assert!(validate_policy(CHILD_BIT).is_err());
        assert!(validate_policy(0x80).is_err());
        let mut drawn = Vec::new();
        let old = signal_action(false);
        install_actions(policy, |sig, action| {
            drawn.push((sig, action.sa_sigaction == libc::SIG_IGN));
            Ok(old)
        })
        .expect("injected policy installation");
        for (sig, ignored) in drawn {
            assert_eq!(ignored, sig != libc::SIGCHLD, "signal {sig}");
        }
    }

    #[test]
    fn a_partial_handler_install_restores_prior_dispositions_in_reverse_order() {
        let mut changes = Vec::new();
        let mut failure = true;
        let old = signal_action(true);
        let result = install_actions(0, |sig, action| {
            changes.push((sig, action.sa_sigaction == libc::SIG_IGN));
            if sig == HANDLED[2] && failure {
                failure = false;
                return Err(io::Error::other("injected third-handler failure"));
            }
            Ok(old)
        });
        assert!(result.is_err());
        assert_eq!(
            changes,
            [
                (HANDLED[0], false),
                (HANDLED[1], false),
                (HANDLED[2], false),
                (HANDLED[1], true),
                (HANDLED[0], true)
            ]
        );
    }

    #[test]
    fn a_partial_inheritance_change_restores_the_entire_first_fd_flag_word() {
        let before = [libc::FD_CLOEXEC | 0x40, libc::FD_CLOEXEC | 0x80];
        let mut changes = Vec::new();
        let result = set_pair_flags(
            [10, 11],
            before,
            before.map(|f| f & !libc::FD_CLOEXEC),
            |fd, flags| {
                changes.push((fd, flags));
                if fd == 11 {
                    Err(io::Error::other("injected second-fd failure"))
                } else {
                    Ok(())
                }
            },
        );
        assert!(result.is_err());
        assert_eq!(changes, [(10, 0x40), (11, 0x80), (10, before[0])]);
    }

    #[test]
    fn an_unrestorable_flag_change_is_reported_instead_of_claiming_rollback() {
        let mut changes = 0;
        let failure = set_pair_flags([10, 11], [1, 1], [0, 0], |_, _| {
            changes += 1;
            if changes == 1 {
                Ok(())
            } else {
                Err(io::Error::other("injected restore failure"))
            }
        })
        .expect_err("both change and rollback fail");
        assert!(!failure.restored);
        assert!(failure.error.to_string().contains("rollback failed"));
    }

    #[cfg(any(
        target_os = "macos",
        target_os = "ios",
        all(target_os = "linux", any(target_env = "gnu", target_env = "musl"))
    ))]
    fn saved_modes() -> TermiosSnapshot {
        let mut modes = blank_termios();
        modes.c_iflag = 0x123;
        modes.c_oflag = 0x234;
        modes.c_cflag = 0x345;
        modes.c_lflag = 0x456;
        for (index, byte) in modes.c_cc.iter_mut().enumerate() {
            *byte = u8::try_from(index).expect("control index").wrapping_mul(17);
        }
        let saved = Termios {
            fd: 0,
            terminal: DescriptorIdentity {
                device: 1,
                inode: 2,
                rdev: 3,
            },
            modes,
        };
        let mut snapshot = saved.snapshot().expect("native snapshot ABI");
        if snapshot.line.is_some() {
            snapshot.line = Some(7);
        }
        if snapshot.raw_speeds.is_some() {
            snapshot.raw_speeds = Some([0x1234, 0x5678]);
        }
        snapshot
    }

    #[cfg(any(
        target_os = "macos",
        target_os = "ios",
        all(target_os = "linux", any(target_env = "gnu", target_env = "musl"))
    ))]
    #[test]
    fn original_modes_round_trip_without_reading_or_restoring_current_raw_modes() {
        let saved = saved_modes();
        let restored =
            Termios::from_snapshot_using(0, &saved, |_| Ok(saved.terminal)).expect("same terminal");
        assert_eq!(restored.snapshot().unwrap(), saved);
        let mut replaced = saved.terminal;
        replaced.rdev ^= 1;
        assert!(Termios::from_snapshot_using(0, &saved, |_| Ok(replaced)).is_err());
        assert!(
            Termios::from_snapshot_using(0, &saved, |_| Err(io::Error::other(
                "unreadable terminal"
            )))
            .is_err()
        );
    }

    #[cfg(any(
        target_os = "macos",
        target_os = "ios",
        all(target_os = "linux", any(target_env = "gnu", target_env = "musl"))
    ))]
    #[test]
    fn terminal_snapshot_rejects_abi_control_count_and_native_field_shape_changes() {
        let saved = saved_modes();
        let mut altered = saved.clone();
        altered.abi.push_str("-other-image-abi");
        assert!(decode_modes(&altered).is_err());
        altered = saved.clone();
        altered.controls.pop();
        assert!(decode_modes(&altered).is_err());
        altered = saved.clone();
        altered.line = if saved.line.is_some() { None } else { Some(0) };
        assert!(decode_modes(&altered).is_err());
        altered = saved;
        altered.raw_speeds = if altered.raw_speeds.is_some() {
            None
        } else {
            Some([0, 0])
        };
        assert!(decode_modes(&altered).is_err());
        assert!(narrow::<u32>(u64::MAX).is_err());
    }
}
