// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Single-threaded exact image exec and owned capability fork/exec. A cold
//! probe exec can block in its child without parking the continuing supervisor.

use super::custody::OwnedChild;
use super::{AllSignals, HANDLED, ensure_single_threaded};
use std::ffi::{CString, OsString};
use std::fs::File;
use std::io::{self, Read as _, Write as _};
use std::os::fd::{AsRawFd, BorrowedFd, FromRawFd, OwnedFd};
use std::os::unix::ffi::OsStrExt as _;
use std::os::unix::process::ExitStatusExt as _;
use std::path::Path;
use std::process::ExitStatus;

const FD_BOUND: i32 = 1_048_575;

fn invalid(why: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, why)
}

fn pipe() -> io::Result<(File, File)> {
    let mut raw = [-1; 2];
    // SAFETY: raw is writable storage for two new, distinct owned descriptors.
    if unsafe { libc::pipe(raw.as_mut_ptr()) } != 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: each endpoint was newly allocated by pipe and has one owner.
    let read = File::from(unsafe { OwnedFd::from_raw_fd(raw[0]) });
    // SAFETY: the independently allocated other endpoint has one owner.
    let write = File::from(unsafe { OwnedFd::from_raw_fd(raw[1]) });
    for fd in [read.as_raw_fd(), write.as_raw_fd()] {
        if fd < 3 {
            return Err(invalid("probe pipes cannot occupy standard descriptors"));
        }
        // SAFETY: these queries and setters operate on this function's owned new pipe.
        let status = unsafe { libc::fcntl(fd, libc::F_GETFL) };
        // SAFETY: F_GETFD queries this live owned pipe's integer descriptor flags.
        let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
        if status < 0 || flags < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: preserve all existing flags while making each endpoint nonblocking and private.
        if unsafe { libc::fcntl(fd, libc::F_SETFL, status | libc::O_NONBLOCK) } < 0
            // SAFETY: the endpoint is still uniquely owned and open.
            || unsafe { libc::fcntl(fd, libc::F_SETFD, flags | libc::FD_CLOEXEC) } < 0
        {
            return Err(io::Error::last_os_error());
        }
    }
    Ok((read, write))
}

fn highest_fd() -> io::Result<i32> {
    let directory = if cfg!(target_os = "linux") {
        "/proc/self/fd"
    } else {
        "/dev/fd"
    };
    let mut highest = 2;
    let mut count: i32 = 0;
    for entry in std::fs::read_dir(directory)? {
        count = match count.checked_add(1) {
            Some(next) if next <= FD_BOUND => next,
            _ => return Err(invalid("probe descriptor inventory exceeds bound")),
        };
        if let Some(fd) = entry?
            .file_name()
            .to_str()
            .and_then(|name| name.parse::<i32>().ok())
        {
            if fd > FD_BOUND {
                return Err(invalid("probe descriptor number exceeds bound"));
            }
            highest = highest.max(fd);
        }
    }
    Ok(highest)
}

fn cstring(value: &std::ffi::OsStr) -> io::Result<CString> {
    CString::new(value.as_bytes()).map_err(|_| invalid("exec argument or environment contains NUL"))
}

fn exec_environment(changes: &[(OsString, OsString)]) -> io::Result<Vec<CString>> {
    let mut environment: std::collections::BTreeMap<_, _> = std::env::vars_os().collect();
    for (name, value) in changes {
        environment.insert(name.clone(), value.clone());
    }
    environment
        .into_iter()
        .map(|(name, value)| {
            if name.as_bytes().contains(&b'=') || name.is_empty() {
                return Err(invalid("invalid exec environment name"));
            }
            let mut word = name;
            word.push("=");
            word.push(value);
            cstring(&word)
        })
        .collect()
}

/// Replace this one continuing image with an absolute image and explicit argv/env.
/// The caller retains all descriptor/mask guards. Unlike Command::exec, this
/// performs no pre-syscall disposition or global environment mutation; a returned
/// execve leaves those original states intact, including unmanaged SIGPIPE.
/// Unix argument and environment bytes are preserved without UTF-8 conversion.
///
/// Returns only when quiescence, blocked HANDLED, absolute path, input validation,
/// or the actual execve fails. It never spawns another child or restores a mask.
pub fn exec_exact(
    executable: &Path,
    args: &[OsString],
    changes: &[(OsString, OsString)],
) -> io::Error {
    let result = (|| -> io::Result<()> {
        ensure_single_threaded()?;
        if !executable.is_absolute() || !super::managed_signals_blocked()? {
            return Err(invalid(
                "exact image exec requires an absolute path and blocked managed signals",
            ));
        }
        let executable = cstring(executable.as_os_str())?;
        let mut arguments = vec![executable.clone()];
        for arg in args {
            arguments.push(cstring(arg)?);
        }
        let environment = exec_environment(changes)?;
        let mut argv: Vec<_> = arguments.iter().map(|arg| arg.as_ptr()).collect();
        argv.push(std::ptr::null());
        let mut envp: Vec<_> = environment.iter().map(|word| word.as_ptr()).collect();
        envp.push(std::ptr::null());
        // SAFETY: one native thread, HANDLED blocked, absolute NUL-terminated
        // image, and complete immutable owned CString graphs with null tails.
        // Every pointed-to allocation lives through this single syscall. No
        // handler, mask, FD flag or global environ was changed in preparation.
        unsafe {
            libc::execve(executable.as_ptr(), argv.as_ptr(), envp.as_ptr());
        }
        Err(io::Error::last_os_error())
    })();
    result
        .err()
        .unwrap_or_else(|| io::Error::other("exact exec unexpectedly returned without an error"))
}

/// Exactly one owned probe child, with bounded nonblocking output. It never
/// substitutes for the vendor child or consumes another child's wait event.
#[must_use = "retain the owned probe until try_wait consumes its terminal event"]
pub struct ProbeChild {
    custody: OwnedChild,
    reported: bool,
    stdin: Option<File>,
    stdout: File,
    stderr: File,
    exec_report: File,
    exec_eof: bool,
    eof: [bool; 2],
}

/// A refused spawn, preserving every resource created after a successful fork.
#[derive(Debug)]
#[must_use = "a returned child and signal block require explicit reconciliation"]
pub struct ProbeSpawnRefusal {
    /// The preparation, fork or mask-restoration failure.
    pub error: io::Error,
    /// A successfully forked child, still owned and not claimed to be reaped.
    /// Only refusal custody is boxed; successful native polling keeps its mask inline.
    pub child: Option<Box<ProbeChild>>,
    /// An original mask which could not yet be restored. Boxed because a native
    /// `sigset_t` is 128 bytes on Linux, which would bloat every probe `Result`.
    pub signals: Option<Box<AllSignals>>,
}

impl From<io::Error> for ProbeSpawnRefusal {
    fn from(error: io::Error) -> Self {
        Self {
            error,
            child: None,
            signals: None,
        }
    }
}

impl std::fmt::Debug for ProbeChild {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProbeChild")
            .field("custody", &self.custody)
            .finish_non_exhaustive()
    }
}

impl ProbeChild {
    /// Fork an already single-threaded supervisor, then exec only in the child.
    /// Prepare every allocation before fork. The child redirects stdin to null,
    /// redirects output, closes every inherited nonstandard descriptor, restores
    /// default managed dispositions and unblocks managed signals before exec.
    /// # Errors
    /// Quiescence, input validation, descriptor inventory, mask setup or fork fails.
    pub fn spawn(
        executable: &Path,
        args: &[OsString],
        changes: &[(OsString, OsString)],
    ) -> Result<Self, ProbeSpawnRefusal> {
        Self::spawn_io(executable, args, changes, false, None)
    }

    /// The same retained wait ownership with a private nonblocking input writer.
    /// # Errors
    /// As [`Self::spawn`], or pipe preparation fails. The caller must retain the child
    /// and any refused signal block until each has been explicitly settled.
    pub fn spawn_duplex(
        executable: &Path,
        args: &[OsString],
        changes: &[(OsString, OsString)],
    ) -> Result<Self, ProbeSpawnRefusal> {
        Self::spawn_io(executable, args, changes, true, None)
    }

    /// Run an isolated test image on a caller-owned private terminal, retaining
    /// the same exact exec and native wait custody as an ordinary probe.
    /// # Errors
    /// As [`Self::spawn`], or the slave cannot be cloned or adopted in the child.
    #[cfg(debug_assertions)]
    #[doc(hidden)]
    pub fn fixture_spawn_terminal(
        executable: &Path,
        args: &[OsString],
        changes: &[(OsString, OsString)],
        slave: BorrowedFd<'_>,
    ) -> Result<Self, ProbeSpawnRefusal> {
        Self::spawn_io(executable, args, changes, false, Some(slave))
    }

    fn spawn_io(
        executable: &Path,
        args: &[OsString],
        changes: &[(OsString, OsString)],
        duplex: bool,
        terminal: Option<BorrowedFd<'_>>,
    ) -> Result<Self, ProbeSpawnRefusal> {
        let mut custody = OwnedChild::empty();
        if let Err(error) = custody.exclude() {
            return Err(ProbeSpawnRefusal {
                error,
                child: None,
                signals: custody.all_signals.take().map(Box::new),
            });
        }
        let preparation = (|| -> io::Result<_> {
            let executable = cstring(executable.as_os_str())?;
            let mut arguments = vec![executable.clone()];
            for arg in args {
                arguments.push(cstring(arg)?);
            }
            let environment = exec_environment(changes)?;
            let mut argv: Vec<_> = arguments.iter().map(|arg| arg.as_ptr()).collect();
            argv.push(std::ptr::null());
            let mut envp: Vec<_> = environment.iter().map(|word| word.as_ptr()).collect();
            envp.push(std::ptr::null());
            let (stdout, out_write) = pipe()?;
            let (stderr, err_write) = pipe()?;
            let (exec_report, exec_write) = pipe()?;
            let (input, stdin) = if duplex {
                let (read, write) = pipe()?;
                (read, Some(write))
            } else {
                (File::open("/dev/null")?, None)
            };
            let terminal = terminal
                .map(|slave| slave.try_clone_to_owned().map(File::from))
                .transpose()?;
            // Only parent pipe endpoints are nonblocking. Every child receives ordinary
            // blocking pipes; these private descriptions never include a borrowed terminal.
            for fd in [
                input.as_raw_fd(),
                out_write.as_raw_fd(),
                err_write.as_raw_fd(),
            ] {
                // SAFETY: these three live descriptors are owned by this preparation.
                let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
                if flags < 0
                // SAFETY: this live child endpoint is uniquely owned by preparation.
                || unsafe { libc::fcntl(fd, libc::F_SETFL, flags & !libc::O_NONBLOCK) } < 0
                {
                    return Err(io::Error::last_os_error());
                }
            }
            // The signal pipe's caught SIGCHLD is required to keep this child's wait ownership.
            let mut action = super::transfer::blank_action();
            // SAFETY: this only queries the valid SIGCHLD disposition.
            if unsafe { libc::sigaction(libc::SIGCHLD, std::ptr::null(), &raw mut action) } != 0 {
                return Err(io::Error::last_os_error());
            }
            if action.sa_sigaction == libc::SIG_IGN || action.sa_flags & libc::SA_NOCLDWAIT != 0 {
                return Err(invalid("probe requires retained child wait ownership"));
            }
            let mut child_mask = custody
                .all_signals
                .as_ref()
                .expect("fork exclusion")
                .previous();
            for &sig in HANDLED {
                // SAFETY: child_mask is initialized; these managed signals are valid.
                if unsafe { libc::sigdelset(&raw mut child_mask, sig) } != 0 {
                    return Err(io::Error::last_os_error());
                }
            }
            let mut default = super::transfer::blank_action();
            // SAFETY: default.sa_mask is writable native signal storage.
            if unsafe { libc::sigemptyset(&raw mut default.sa_mask) } != 0 {
                return Err(io::Error::last_os_error());
            }
            let mut reset = Vec::new();
            for signal in 1..=128 {
                let mut inherited = super::transfer::blank_action();
                // SAFETY: disposition queries occur under all-handler exclusion;
                // invalid signal numbers simply return EINVAL and are skipped.
                if unsafe { libc::sigaction(signal, std::ptr::null(), &raw mut inherited) } == 0
                    && (HANDLED.contains(&signal) || inherited.sa_sigaction != libc::SIG_IGN)
                    && signal != libc::SIGKILL
                    && signal != libc::SIGSTOP
                {
                    reset.push(signal);
                }
            }
            let last_fd = highest_fd()?;
            let descriptors = terminal.as_ref().map_or(
                [
                    input.as_raw_fd(),
                    out_write.as_raw_fd(),
                    err_write.as_raw_fd(),
                ],
                |slave| [slave.as_raw_fd(); 3],
            );
            Ok((
                executable,
                arguments,
                environment,
                argv,
                envp,
                stdout,
                out_write,
                stderr,
                err_write,
                exec_report,
                exec_write,
                input,
                stdin,
                child_mask,
                default,
                reset,
                last_fd,
                descriptors,
                terminal,
            ))
        })();
        let (
            executable,
            _arguments,
            _environment,
            argv,
            envp,
            stdout,
            out_write,
            stderr,
            err_write,
            exec_report,
            exec_write,
            input,
            stdin,
            child_mask,
            default,
            reset,
            last_fd,
            descriptors,
            terminal,
        ) = match preparation {
            Ok(prepared) => prepared,
            Err(error) => {
                let signals = if custody.restore().is_err() {
                    custody.all_signals.take().map(Box::new)
                } else {
                    None
                };
                return Err(ProbeSpawnRefusal {
                    error,
                    child: None,
                    signals,
                });
            }
        };
        // SAFETY: one native thread is established and ALL handlers stay blocked.
        // All child buffers and pointer graphs are complete, immutable allocations.
        let pid = unsafe { libc::fork() };
        if pid < 0 {
            let error = io::Error::last_os_error();
            return match custody.restore() {
                Ok(()) => Err(error.into()),
                Err(restore) => Err(ProbeSpawnRefusal {
                    error: io::Error::other(format!(
                        "fork failed: {error}; mask restore failed: {restore}"
                    )),
                    child: None,
                    signals: custody.all_signals.take().map(Box::new),
                }),
            };
        }
        if pid == 0 {
            let report = exec_write.as_raw_fd();
            macro_rules! refuse {
                ($code:expr) => {{
                    libc::write(report, c"!".as_ptr().cast(), 1);
                    libc::_exit($code);
                }};
            }
            // SAFETY: only async-signal-safe native calls occur in this branch;
            // it never returns into Rust ownership or unwinds after fork.
            unsafe {
                if terminal.is_none() && libc::setpgid(0, 0) != 0 {
                    refuse!(126);
                }
                for (to, &from) in descriptors.iter().enumerate() {
                    if libc::dup2(from, to as libc::c_int) < 0 {
                        refuse!(126);
                    }
                }
                if terminal.is_some()
                    && (libc::setsid() < 0
                        || libc::ioctl(0, libc::TIOCSCTTY as _, 0) < 0
                        || libc::tcsetpgrp(0, libc::getpgrp()) != 0)
                {
                    refuse!(126);
                }
                for fd in 3..=last_fd {
                    if fd != report {
                        libc::close(fd);
                    }
                }
                for &sig in &reset {
                    if libc::sigaction(sig, &raw const default, std::ptr::null_mut()) != 0 {
                        refuse!(126);
                    }
                }
                if libc::sigprocmask(
                    libc::SIG_SETMASK,
                    &raw const child_mask,
                    std::ptr::null_mut(),
                ) != 0
                {
                    refuse!(126);
                }
                libc::execve(executable.as_ptr(), argv.as_ptr(), envp.as_ptr());
                libc::write(2, c"probe exec failed\n".as_ptr().cast(), 18);
                refuse!(127);
            }
        }
        let mut child = Self {
            custody,
            reported: false,
            stdin,
            stdout,
            stderr,
            exec_report,
            exec_eof: false,
            eof: [false; 2],
        };
        drop(out_write);
        drop(err_write);
        drop(input);
        drop(exec_write);
        match child
            .custody
            .start(pid)
            .and_then(|()| child.custody.restore())
        {
            Ok(()) => Ok(child),
            Err(error) => Err(ProbeSpawnRefusal {
                error,
                child: Some(Box::new(child)),
                signals: None,
            }),
        }
    }

    /// The unreaped owned probe's pid, absent after its normal wait consumed the end.
    #[must_use]
    pub fn pid(&self) -> Option<libc::pid_t> {
        self.custody.pid
    }

    /// Whether native wait or mask custody remains unsettled. Lost wait
    /// ownership remains pending and never becomes a successful reap.
    #[must_use]
    pub fn pending(&self) -> bool {
        self.custody.pid.is_some() || self.custody.lost || self.custody.all_signals.is_some()
    }

    /// Whether another reaper or an unavailable/changed native lifetime has
    /// permanently disarmed this receipt. It never authorizes another signal.
    #[must_use]
    pub fn ownership_lost(&self) -> bool {
        self.custody.lost
    }

    /// Bracket a bounded native observation with this original child's exact
    /// wait lifetime, excluding every handler on one freshly checked thread.
    /// The read must not spawn, reap, change process state, or perform blocking
    /// I/O. Its result is observation data and supplies no input authority.
    /// # Errors
    /// Handler exclusion or original native wait custody is unavailable.
    pub fn observe_native<T>(&mut self, read: impl FnOnce(u32) -> T) -> io::Result<T> {
        self.custody.observe(read)
    }

    /// Whether the child crossed the exact exec syscall rather than merely
    /// still sharing the parent's pre-exec image. A live native image check is
    /// also required: EOF from a child killed before exec proves no execution.
    /// # Errors
    /// The child reported pre-exec refusal, or its private status pipe failed.
    pub fn exec_observed(&mut self) -> io::Result<bool> {
        if self.exec_eof {
            return Ok(true);
        }
        let mut byte = [0];
        match self.exec_report.read(&mut byte) {
            Ok(0) => {
                self.exec_eof = true;
                Ok(true)
            }
            Ok(_) => Err(invalid("probe reported pre-exec refusal")),
            Err(error)
                if error.kind() == io::ErrorKind::WouldBlock
                    || error.kind() == io::ErrorKind::Interrupted =>
            {
                Ok(false)
            }
            Err(error) => Err(error),
        }
    }

    /// Borrow raw descriptors of output streams not yet positively at EOF.
    /// Descriptor ownership stays with this child for the entire borrow.
    pub fn output_fds(&self) -> impl Iterator<Item = i32> + '_ {
        [self.stdout.as_raw_fd(), self.stderr.as_raw_fd()]
            .into_iter()
            .zip(self.eof)
            .filter_map(|(fd, eof)| (!eof).then_some(fd))
    }

    /// The owned input writer's descriptor, if this is an open duplex child.
    pub fn input_fd(&self) -> Option<i32> {
        self.stdin.as_ref().map(AsRawFd::as_raw_fd)
    }

    /// Attempt one nonblocking write to the owned private input.
    /// # Errors
    /// The pipe is closed, would block, or the native write fails.
    pub fn write_input(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.stdin
            .as_mut()
            .ok_or_else(|| io::Error::new(io::ErrorKind::BrokenPipe, "probe input is closed"))?
            .write(bytes)
    }

    /// Close this parent's private input writer, preserving output/wait custody.
    pub fn close_input(&mut self) {
        self.stdin = None;
    }

    /// Whether reads positively observed EOF on both owned output pipes.
    #[must_use]
    pub fn output_finished(&self) -> bool {
        self.eof.into_iter().all(|done| done)
    }

    /// Drain at most `maximum` bytes from each output without blocking.
    /// # Errors
    /// Output exceeds the bound, or an output read fails.
    pub fn drain(
        &mut self,
        stdout: &mut Vec<u8>,
        stderr: &mut Vec<u8>,
        maximum: usize,
    ) -> io::Result<()> {
        self.drain_bounded(stdout, stderr, maximum, false)
    }

    /// Drain with one aggregate byte limit shared by stdout and stderr.
    /// # Errors
    /// Either read fails or even one byte exceeds the shared bound. Retained
    /// vectors never exceed the bound, including the refusing read.
    pub fn drain_total(
        &mut self,
        stdout: &mut Vec<u8>,
        stderr: &mut Vec<u8>,
        maximum: usize,
    ) -> io::Result<()> {
        self.drain_bounded(stdout, stderr, maximum, true)
    }

    fn drain_bounded(
        &mut self,
        stdout: &mut Vec<u8>,
        stderr: &mut Vec<u8>,
        maximum: usize,
        aggregate: bool,
    ) -> io::Result<()> {
        fn one(
            file: &mut File,
            bytes: &mut Vec<u8>,
            maximum: usize,
            eof: &mut bool,
        ) -> io::Result<()> {
            let mut buffer = [0; 1024];
            for _ in 0..32 {
                let remaining = maximum.saturating_sub(bytes.len());
                let capacity = buffer.len().min(remaining.saturating_add(1));
                match file.read(&mut buffer[..capacity]) {
                    Ok(0) => {
                        *eof = true;
                        break;
                    }
                    Ok(n) => {
                        // A read reporting more than its buffer is refused, never sliced past.
                        let chunk = buffer
                            .get(..n)
                            .ok_or_else(|| invalid("probe output read exceeded its buffer"))?;
                        bytes.extend_from_slice(&chunk[..n.min(remaining)]);
                        if n > remaining {
                            return Err(invalid("probe output exceeds bound"));
                        }
                    }
                    Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
                    Err(error) => return Err(error),
                }
            }
            Ok(())
        }
        let out_max = if aggregate {
            maximum.saturating_sub(stderr.len())
        } else {
            maximum
        };
        one(&mut self.stdout, stdout, out_max, &mut self.eof[0])?;
        let err_max = if aggregate {
            maximum.saturating_sub(stdout.len())
        } else {
            maximum
        };
        one(&mut self.stderr, stderr, err_max, &mut self.eof[1])
    }

    /// Consume only this probe's terminal event, nonblocking.
    /// # Errors
    /// Wait ownership is lost or the kernel wait fails. Uncertainty retains the pid.
    pub fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
        if self.reported {
            return Ok(None);
        }
        let status = self.custody.poll(false)?;
        if status.is_some() {
            self.reported = true;
        }
        Ok(status.map(ExitStatus::from_raw))
    }

    /// Cancel only a probe whose non-consuming wait still proves child ownership.
    /// # Errors
    /// Ownership cannot be proved or the signal fails. No unrelated pid is signaled.
    pub fn cancel(&mut self) -> io::Result<()> {
        self.custody.poll(true).map(|_| ())
    }
}
