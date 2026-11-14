// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Single-threaded exact image exec and owned capability fork/exec. A cold
//! probe exec can block in its child without parking the continuing supervisor.

use super::{HANDLED, SignalBlock, ensure_single_threaded};
use std::ffi::{CString, OsString};
use std::fs::File;
use std::io::{self, Read as _};
use std::marker::PhantomData;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::ffi::OsStrExt as _;
use std::os::unix::process::ExitStatusExt as _;
use std::path::Path;
use std::process::ExitStatus;
use std::rc::Rc;

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
    pid: Option<libc::pid_t>,
    stdout: File,
    stderr: File,
    _thread: PhantomData<Rc<()>>,
}

/// A refused spawn, preserving every resource created after a successful fork.
#[derive(Debug)]
#[must_use = "a returned child and signal block require explicit reconciliation"]
pub struct ProbeSpawnRefusal {
    /// The preparation, fork or mask-restoration failure.
    pub error: io::Error,
    /// A successfully forked child, still owned and not claimed to be reaped.
    pub child: Option<ProbeChild>,
    /// An original mask which could not yet be restored.
    pub signals: Option<SignalBlock>,
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
            .field("pid", &self.pid)
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
        ensure_single_threaded()?;
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
        let null = File::open("/dev/null")?;
        // The signal pipe's caught SIGCHLD is required to keep this child's wait ownership.
        let mut action = super::transfer::blank_action();
        // SAFETY: this only queries the valid SIGCHLD disposition.
        if unsafe { libc::sigaction(libc::SIGCHLD, std::ptr::null(), &raw mut action) } != 0 {
            return Err(io::Error::last_os_error().into());
        }
        if action.sa_sigaction == libc::SIG_IGN || action.sa_flags & libc::SA_NOCLDWAIT != 0 {
            return Err(invalid("probe requires retained child wait ownership").into());
        }
        let mut child_mask = super::transfer::blank_sigset();
        // SAFETY: null input only queries into writable native mask storage.
        if unsafe { libc::sigprocmask(libc::SIG_BLOCK, std::ptr::null(), &raw mut child_mask) } != 0
        {
            return Err(io::Error::last_os_error().into());
        }
        for &sig in HANDLED {
            // SAFETY: child_mask is initialized; these managed signals are valid.
            if unsafe { libc::sigdelset(&raw mut child_mask, sig) } != 0 {
                return Err(io::Error::last_os_error().into());
            }
        }
        let mut default = super::transfer::blank_action();
        // SAFETY: default.sa_mask is writable native signal storage.
        if unsafe { libc::sigemptyset(&raw mut default.sa_mask) } != 0 {
            return Err(io::Error::last_os_error().into());
        }
        let last_fd = highest_fd()?;
        let descriptors = [
            null.as_raw_fd(),
            out_write.as_raw_fd(),
            err_write.as_raw_fd(),
        ];
        let mut block = SignalBlock::managed()?;
        // SAFETY: one native thread is established and managed signals stay blocked.
        // All child buffers and pointer graphs are complete, immutable allocations.
        let pid = unsafe { libc::fork() };
        if pid < 0 {
            let error = io::Error::last_os_error();
            return match block.restore() {
                Ok(()) => Err(error.into()),
                Err(restore) => Err(ProbeSpawnRefusal {
                    error: io::Error::other(format!(
                        "fork failed: {error}; mask restore failed: {restore}"
                    )),
                    child: None,
                    signals: Some(block),
                }),
            };
        }
        if pid == 0 {
            // SAFETY: only async-signal-safe native calls occur in this branch;
            // it never returns into Rust ownership or unwinds after fork.
            unsafe {
                if libc::setpgid(0, 0) != 0 {
                    libc::_exit(126);
                }
                for (to, &from) in descriptors.iter().enumerate() {
                    if libc::dup2(from, to as libc::c_int) < 0 {
                        libc::_exit(126);
                    }
                }
                for fd in 3..=last_fd {
                    libc::close(fd);
                }
                for &sig in HANDLED {
                    if libc::sigaction(sig, &raw const default, std::ptr::null_mut()) != 0 {
                        libc::_exit(126);
                    }
                }
                if libc::sigprocmask(
                    libc::SIG_SETMASK,
                    &raw const child_mask,
                    std::ptr::null_mut(),
                ) != 0
                {
                    libc::_exit(126);
                }
                libc::execve(executable.as_ptr(), argv.as_ptr(), envp.as_ptr());
                libc::write(2, c"probe exec failed\n".as_ptr().cast(), 18);
                libc::_exit(127);
            }
        }
        let child = Self {
            pid: Some(pid),
            stdout,
            stderr,
            _thread: PhantomData,
        };
        drop(out_write);
        drop(err_write);
        drop(null);
        match block.restore() {
            Ok(()) => Ok(child),
            Err(error) => Err(ProbeSpawnRefusal {
                error,
                child: Some(child),
                signals: Some(block),
            }),
        }
    }

    /// The unreaped owned probe's pid, absent after its normal wait consumed the end.
    #[must_use]
    pub fn pid(&self) -> Option<libc::pid_t> {
        self.pid
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
        fn one(file: &mut File, bytes: &mut Vec<u8>, maximum: usize) -> io::Result<()> {
            let mut buffer = [0; 1024];
            for _ in 0..32 {
                match file.read(&mut buffer) {
                    Ok(0) => break,
                    Ok(n) => {
                        if bytes.len().checked_add(n).is_none_or(|size| size > maximum) {
                            return Err(invalid("probe output exceeds bound"));
                        }
                        // A read reporting more than its buffer is refused, never sliced past.
                        let chunk = buffer
                            .get(..n)
                            .ok_or_else(|| invalid("probe output read exceeded its buffer"))?;
                        bytes.extend_from_slice(chunk);
                    }
                    Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
                    Err(error) => return Err(error),
                }
            }
            Ok(())
        }
        one(&mut self.stdout, stdout, maximum)?;
        one(&mut self.stderr, stderr, maximum)
    }

    /// Consume only this probe's terminal event, nonblocking.
    /// # Errors
    /// Wait ownership is lost or the kernel wait fails. Uncertainty retains the pid.
    pub fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
        ensure_single_threaded()?;
        let Some(pid) = self.pid else { return Ok(None) };
        let mut status = 0;
        // SAFETY: exact positive owned child pid and writable status; no other child is reaped.
        let waited = unsafe { libc::waitpid(pid, &raw mut status, libc::WNOHANG) };
        if waited == 0 {
            return Ok(None);
        }
        if waited < 0 {
            return Err(io::Error::last_os_error());
        }
        self.pid = None;
        Ok(Some(ExitStatus::from_raw(status)))
    }

    /// Cancel only a probe whose non-consuming wait still proves child ownership.
    /// # Errors
    /// Ownership cannot be proved or the signal fails. No unrelated pid is signaled.
    pub fn cancel(&self) -> io::Result<()> {
        ensure_single_threaded()?;
        let Some(pid) = self.pid else { return Ok(()) };
        // Initialized writable native wait storage; WNOWAIT never consumes the event.
        let mut info = super::transfer::blank_siginfo();
        // SAFETY: exact child pid, writable storage, nonblocking non-consuming flags.
        if unsafe {
            libc::waitid(
                libc::P_PID,
                pid as libc::id_t,
                &raw mut info,
                libc::WEXITED | libc::WNOWAIT | libc::WNOHANG,
            )
        } != 0
        {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: the owned unreaped child prevents pid reuse, regardless of live/terminal state.
        if unsafe { libc::kill(pid, libc::SIGKILL) } != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }
}
