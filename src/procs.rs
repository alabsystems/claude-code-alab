// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The child's own background work: a safe point waits while any runs.
//!
//! Claude Code's record says `idle` as soon as a turn ends, even while a shell
//! it started (a background Bash task, a background agent's command) is still
//! running; ending the child then ends that work and loses its result. aterm's
//! Claude upgrade lane therefore holds a restart while the vendor process has
//! any descendant shell (aterm `upgrade.rs:3383-3391`, `restart_gate`:
//! `if !f.background.is_empty() { return Gate::Wait("background") }`), read
//! from the process table (aterm `upgrade_drive.rs:981-1034`, `BACKGROUND`,
//! `background_procs`, `background_or_unreadable`). This module is that rule,
//! ported: the same names, the same walk, and an unreadable table reads as
//! work (never as "nothing runs").
//!
//! The background table comes from `ps -A -o pid=,ppid=,comm=` under `LC_ALL=C`,
//! as aterm reads it off macOS (aterm `upgrade_drive.rs:689-706`, `ps_table`), under
//! one deadline ([`run_until`]). It is read only when every other witness already
//! says "safe", never on every look. Separate kernel readers capture a child's
//! identity without consuming any wait event; an incomplete read is no proof.

use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// One process: pid, parent pid, command name (its base name).
pub type Row = (u32, u32, String);

/// A kernel birth stamp, never a vendor record's wall-clock approximation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Birth {
    /// macOS's process start time in seconds and microseconds since the epoch.
    MacOs {
        /// Whole epoch seconds.
        seconds: u64,
        /// Microseconds within that second.
        microseconds: u32,
    },
    /// Linux's start clock ticks since boot (`/proc/<pid>/stat`, field 22).
    Linux(u64),
}

/// The kernel-named executable and its file identity.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Executable {
    /// The kernel's absolute image path, with Unix bytes preserved.
    pub path: PathBuf,
    /// The executable's device.
    pub device: u64,
    /// The executable's inode.
    pub inode: u64,
}

/// The complete identity of a child whose wait ownership was confirmed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChildIdentity {
    /// The child's pid.
    pub pid: u32,
    /// The supervising parent's pid.
    pub parent: u32,
    /// The child's process group.
    pub pgid: u32,
    /// The child's session.
    pub session: u32,
    /// The kernel birth stamp.
    pub birth: Birth,
    /// The executable observed while the child was alive.
    pub executable: Executable,
}

/// A complete process observation, without claiming that it is this process's child.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProcessIdentity {
    /// The observed process.
    pub pid: u32,
    /// Its current parent.
    pub parent: u32,
    /// Its current process group.
    pub pgid: u32,
    /// Its current session.
    pub session: u32,
    /// Its kernel birth stamp, which survives an image replacement.
    pub birth: Birth,
    /// Its kernel-named mapped executable.
    pub executable: Executable,
}

/// Exact native executable observation, separate from the supervisor's capsule ABI.
/// This is an observation, never input or transition authority.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExecIdentity {
    process: ProcessIdentity,
    generation: (u64, i32, [u8; 16]),
}

impl ExecIdentity {
    /// The process, lifetime and mapped executable observed together.
    #[must_use]
    pub fn process(&self) -> &ProcessIdentity {
        &self.process
    }

    /// Whether the same native executable generation is still observable.
    #[must_use]
    pub fn current(&self) -> bool {
        capture_exact_exec(self.process.pid).as_ref() == Some(self)
    }

    /// Diagnostic native identity; it is not a bearer token.
    #[must_use]
    pub fn diagnostic(&self) -> String {
        format!(
            "{}@{:?}:{}:{}",
            self.process.pid, self.process.birth, self.generation.0, self.generation.1
        )
    }
}

/// Capture native exec generation around the complete process observation.
/// Unknown generation stays unavailable; Linux birth/image equality is not exact exec evidence.
#[must_use]
pub fn capture_exact_exec(pid: u32) -> Option<ExecIdentity> {
    let generation = exec_generation(pid)?;
    let process = capture_process(pid)?;
    (exec_generation(pid)? == generation).then_some(ExecIdentity {
        process,
        generation,
    })
}

#[cfg(target_os = "macos")]
fn exec_generation(pid: u32) -> Option<(u64, i32, [u8; 16])> {
    // <sys/proc_info.h> proc_uniqidentifierinfo, the same flavor/layout used by
    // aterm's vetted native process reader. No aterm library is linked here.
    #[repr(C)]
    struct Uniq {
        uuid: [u8; 16],
        unique: u64,
        parent_unique: u64,
        version: i32,
        original_parent_version: i32,
        reserved: [u64; 2],
    }
    const _: () = assert!(std::mem::size_of::<Uniq>() == 56);
    let mut value = std::mem::MaybeUninit::<Uniq>::zeroed();
    let size = i32::try_from(std::mem::size_of::<Uniq>()).ok()?;
    // SAFETY: flavor17 writes at most size bytes to this correctly laid-out native record.
    let got = unsafe {
        libc::proc_pidinfo(
            i32::try_from(pid).ok()?,
            17,
            0,
            value.as_mut_ptr().cast(),
            size,
        )
    };
    if got != size {
        return None;
    }
    // SAFETY: the kernel returned the complete initialized plain-data record.
    let value = unsafe { value.assume_init() };
    (value.unique > 0 && value.version > 0).then_some((value.unique, value.version, value.uuid))
}

#[cfg(not(target_os = "macos"))]
fn exec_generation(_: u32) -> Option<(u64, i32, [u8; 16])> {
    None
}

/// Capture complete live process facts, including this harness's own identity.
///
/// This grants no child wait ownership. Unreaped processes and changing or
/// unavailable image/session facts return `None`.
#[must_use]
pub fn capture_process(pid: u32) -> Option<ProcessIdentity> {
    let before = kernel_identity(pid)?;
    if before.state == ChildState::Unreaped {
        return None;
    }
    let identity = ProcessIdentity {
        pid,
        parent: before.parent,
        pgid: before.pgid?,
        session: before.session?,
        birth: before.birth,
        executable: executable(pid)?,
    };
    let after = kernel_identity(pid)?;
    (after.state != ChildState::Unreaped
        && after.pid == identity.pid
        && after.parent == identity.parent
        && after.pgid == Some(identity.pgid)
        && after.session == Some(identity.session)
        && after.birth == identity.birth
        && executable(pid)? == identity.executable)
        .then_some(identity)
}

/// An owned child's state; observing an end never reaps it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChildState {
    /// Alive, including a sleeping process.
    Live,
    /// Job-control or tracing stop.
    Stopped,
    /// Exited and still available for the supervisor's normal wait. This is
    /// terminal-event dispatch evidence, never live-session adoption evidence.
    Unreaped,
}

/// Exact wait lifetime only, without live-image or input authority. A helper's
/// terminal event remains identifiable when its executable/session is gone.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct OwnedLifetime {
    pub(crate) pid: u32,
    parent: u32,
    pub(crate) birth: Birth,
}

impl OwnedLifetime {
    /// Must be called while the sole thread excludes every signal handler from
    /// the fork/capture or observe/wait transaction. This never consumes events.
    pub(crate) fn capture(pid: u32) -> Option<Self> {
        let before = native_lifetime(pid)?;
        if before.parent != std::process::id() || !owns_child(pid) {
            return None;
        }
        let after = native_lifetime(pid)?;
        (before == after).then_some(after)
    }

    /// Missing or changed native lifetime fails closed, including terminal state.
    pub(crate) fn current(&self) -> bool {
        Self::capture(self.pid).as_ref() == Some(self)
    }

    /// Observe an owned wait lifetime without requiring a live executable.
    /// The caller excludes reapers throughout these non-consuming reads.
    pub(crate) fn observation(&self) -> Option<ChildObservation> {
        if !self.current() {
            return None;
        }
        let read = kernel_identity(self.pid)?;
        (read.parent == self.parent && read.birth == self.birth && self.current()).then_some(
            ChildObservation {
                state: read.state,
                pgid: read.pgid,
                session: read.session,
            },
        )
    }
}

/// Fresh facts about the captured child, including facts unavailable after exit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ChildObservation {
    /// Its state. Only a live or stopped observation proves live-session continuity.
    pub state: ChildState,
    /// Its freshly observed process group, absent when the kernel no longer supplies it.
    pub pgid: Option<u32>,
    /// Its freshly observed session, absent on Darwin after exit.
    pub session: Option<u32>,
}

#[derive(Clone, Debug)]
struct KernelIdentity {
    pid: u32,
    parent: u32,
    pgid: Option<u32>,
    session: Option<u32>,
    birth: Birth,
    state: ChildState,
}

impl KernelIdentity {
    fn matches(&self, expected: &ChildIdentity) -> bool {
        self.pid == expected.pid
            && self.parent == expected.parent
            && self.birth == expected.birth
            && (self.state == ChildState::Unreaped
                || (self.pgid == Some(expected.pgid) && self.session == Some(expected.session)))
    }
}

/// Capture a complete, live child's identity, or no identity on any failed read.
///
/// Unsupported platforms, an already-exited child, and a racing identity change
/// return `None`. Failure does not signal, reap or otherwise change the child.
#[must_use]
pub fn capture_child(pid: u32) -> Option<ChildIdentity> {
    let before = kernel_identity(pid)?;
    if before.state == ChildState::Unreaped || before.parent != std::process::id() {
        return None;
    }
    let identity = ChildIdentity {
        pid,
        parent: before.parent,
        pgid: before.pgid?,
        session: before.session?,
        birth: before.birth,
        executable: executable(pid)?,
    };
    let after = kernel_identity(pid)?;
    (after.state != ChildState::Unreaped
        && after.matches(&identity)
        && owns_child(pid)
        && executable(pid)? == identity.executable)
        .then_some(identity)
}

/// Pin the expected vendor image after a launch helper execs it in place.
/// Every lifetime and parentage fact must still be the original child's, and
/// the newly observed executable must be the exact expected file. This never
/// authorizes following an arbitrary executable or a reused process id.
#[must_use]
pub fn refresh_child_image(expected: &ChildIdentity, target: &Path) -> Option<ChildIdentity> {
    use std::os::unix::fs::MetadataExt as _;
    let fresh = capture_child(expected.pid)?;
    if fresh.pid != expected.pid
        || fresh.parent != expected.parent
        || fresh.pgid != expected.pgid
        || fresh.session != expected.session
        || fresh.birth != expected.birth
    {
        return None;
    }
    let target = std::fs::metadata(target).ok()?;
    (target.is_file()
        && fresh.executable.device == target.dev()
        && fresh.executable.inode == target.ino())
    .then_some(fresh)
}

/// Observe the same owned child, including its unreaped exit, without consuming events.
///
/// A live child must still run the captured image. An exited child's image is
/// no longer readable, and Darwin cannot query its session. Its captured birth,
/// parent and non-consuming wait ownership prove the same child's pending end;
/// returned group/session facts remain explicitly optional. An `Unreaped`
/// result may only dispatch the normal terminal event, never authorize live
/// adoption. Missing identity or live-session evidence returns `None`.
#[must_use]
pub fn observe_child(expected: &ChildIdentity) -> Option<ChildObservation> {
    observe_child_using(expected, kernel_identity, executable, owns_child)
}

/// The most children [`own_children`] reports; one more means "several".
pub const OWN_CHILDREN_MAX: usize = 16;

/// This process's own children, live, stopped or exited but unreaped, as the
/// kernel lists them: at most [`OWN_CHILDREN_MAX`] + 1 pids, each one this
/// process can wait for (a non-consuming wait proves the parentage, and an
/// unreaped child's pid cannot be reused). `None` when the list cannot be read
/// or the platform has none. An empty list is returned only when no child at
/// all can be waited for, because macOS reports a listing it refused (a
/// sandbox, an allocation failure) as an empty one. A listed child is not yet
/// a [`ChildIdentity`]; [`capture_child`] proves a live one before anything
/// signals it.
#[must_use]
pub fn own_children() -> Option<Vec<u32>> {
    let owned: Vec<u32> = listed_children()?
        .into_iter()
        .filter(|pid| owns_child(*pid))
        .collect();
    (!owned.is_empty() || has_no_children()).then_some(owned)
}

/// Whether this process has no child at all: a non-consuming, non-blocking
/// wait on any child fails with `ECHILD` only then.
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn has_no_children() -> bool {
    loop {
        let mut info = std::mem::MaybeUninit::<libc::siginfo_t>::zeroed();
        // SAFETY: `info` is writable siginfo_t storage. WNOWAIT leaves every
        // child event pending; WNOHANG never waits for a live child.
        let rc = unsafe {
            libc::waitid(
                libc::P_ALL,
                0,
                info.as_mut_ptr(),
                libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
            )
        };
        if rc == 0 {
            return false;
        }
        match std::io::Error::last_os_error().raw_os_error() {
            Some(libc::EINTR) => {}
            Some(libc::ECHILD) => return true,
            _ => return false,
        }
    }
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn has_no_children() -> bool {
    false
}

#[cfg(target_os = "macos")]
fn listed_children() -> Option<Vec<u32>> {
    let me = libc::pid_t::try_from(std::process::id()).ok()?;
    let mut pids: [libc::pid_t; OWN_CHILDREN_MAX + 1] = [0; OWN_CHILDREN_MAX + 1];
    let size = libc::c_int::try_from(std::mem::size_of_val(&pids)).ok()?;
    // SAFETY: `pids` is writable storage of exactly `size` bytes; the call
    // writes at most that many and returns how many pids it wrote.
    let count = unsafe { libc::proc_listchildpids(me, pids.as_mut_ptr().cast(), size) };
    let count = usize::try_from(count).ok()?;
    pids.get(..count)?
        .iter()
        .map(|pid| u32::try_from(*pid).ok().filter(|pid| *pid > 1))
        .collect()
}

#[cfg(target_os = "linux")]
fn listed_children() -> Option<Vec<u32>> {
    let me = std::process::id();
    let mut found = Vec::new();
    for entry in std::fs::read_dir("/proc").ok()? {
        let Some(pid) = entry
            .ok()?
            .file_name()
            .to_str()
            .and_then(|name| name.parse::<u32>().ok())
        else {
            continue;
        };
        // A process that ended between the listing and the read is skipped.
        if kernel_identity(pid).is_some_and(|seen| seen.parent == me) {
            found.push(pid);
            if found.len() > OWN_CHILDREN_MAX {
                break;
            }
        }
    }
    Some(found)
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn listed_children() -> Option<Vec<u32>> {
    None
}

fn observe_child_using(
    expected: &ChildIdentity,
    mut kernel: impl FnMut(u32) -> Option<KernelIdentity>,
    mut image: impl FnMut(u32) -> Option<Executable>,
    owns: impl FnOnce(u32) -> bool,
) -> Option<ChildObservation> {
    let before = kernel(expected.pid)?;
    if expected.parent != std::process::id() || !before.matches(expected) {
        return None;
    }
    if before.state != ChildState::Unreaped && image(expected.pid)? != expected.executable {
        return None;
    }
    let after = kernel(expected.pid)?;
    // An exit between the two reads is retained for the normal supervisor to reap.
    if !after.matches(expected) || !owns(expected.pid) {
        return None;
    }
    // Exec preserves every process identity field above. A live/stopped result
    // needs a fresh image witness after the final kernel identity observation.
    if after.state != ChildState::Unreaped && image(expected.pid)? != expected.executable {
        return None;
    }
    Some(ChildObservation {
        state: after.state,
        pgid: after.pgid,
        session: after.session,
    })
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn owns_child(pid: u32) -> bool {
    let Ok(pid) = libc::pid_t::try_from(pid) else {
        return false;
    };
    if pid <= 1 {
        return false;
    }
    loop {
        let mut info = std::mem::MaybeUninit::<libc::siginfo_t>::zeroed();
        // SAFETY: `info` is writable siginfo_t storage. WNOWAIT preserves every
        // pending child event; WNOHANG prevents waiting for a live child to exit.
        let rc = unsafe {
            libc::waitid(
                libc::P_PID,
                pid as libc::id_t,
                info.as_mut_ptr(),
                libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
            )
        };
        if rc < 0 && std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted {
            continue;
        }
        return rc == 0;
    }
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn owns_child(_pid: u32) -> bool {
    false
}

#[cfg(target_os = "macos")]
fn bsd_identity(pid: u32) -> Option<libc::proc_bsdinfo> {
    let p = libc::pid_t::try_from(pid).ok().filter(|p| *p > 1)?;
    let mut info = libc::proc_bsdinfo::default();
    let size = libc::c_int::try_from(std::mem::size_of::<libc::proc_bsdinfo>()).ok()?;
    // SAFETY: `info` points to exactly the record PROC_PIDTBSDINFO writes. A
    // short result is rejected before the initialized record is accessed.
    // XNU proc_info.c enables proc_find_zombref for this flavor only when arg is
    // nonzero; arg=0 excludes the very pending exit this observer must retain.
    let n =
        unsafe { libc::proc_pidinfo(p, libc::PROC_PIDTBSDINFO, 1, (&raw mut info).cast(), size) };
    if n != size {
        return None;
    }
    (info.pbi_pid == pid && info.pbi_ppid > 0 && info.pbi_start_tvsec > 0).then_some(info)
}

#[cfg(target_os = "macos")]
fn lifetime_from_bsd(info: &libc::proc_bsdinfo) -> Option<OwnedLifetime> {
    Some(OwnedLifetime {
        pid: info.pbi_pid,
        parent: info.pbi_ppid,
        birth: Birth::MacOs {
            seconds: info.pbi_start_tvsec,
            microseconds: u32::try_from(info.pbi_start_tvusec)
                .ok()
                .filter(|us| *us < 1_000_000)?,
        },
    })
}

#[cfg(target_os = "macos")]
fn native_lifetime(pid: u32) -> Option<OwnedLifetime> {
    lifetime_from_bsd(&bsd_identity(pid)?)
}

#[cfg(target_os = "linux")]
fn native_lifetime(pid: u32) -> Option<OwnedLifetime> {
    let identity = kernel_identity(pid)?;
    Some(OwnedLifetime {
        pid: identity.pid,
        parent: identity.parent,
        birth: identity.birth,
    })
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn native_lifetime(_pid: u32) -> Option<OwnedLifetime> {
    None
}

#[cfg(target_os = "macos")]
fn kernel_identity(pid: u32) -> Option<KernelIdentity> {
    let info = bsd_identity(pid)?;
    kernel_identity_from_bsd(&info, |pid| {
        let p = libc::pid_t::try_from(pid).ok()?;
        // SAFETY: getsid is a read-only query and reports errors as a negative pid.
        u32::try_from(unsafe { libc::getsid(p) })
            .ok()
            .filter(|s| *s > 0)
    })
}

#[cfg(target_os = "macos")]
fn kernel_identity_from_bsd(
    info: &libc::proc_bsdinfo,
    read_session: impl FnOnce(u32) -> Option<u32>,
) -> Option<KernelIdentity> {
    let lifetime = lifetime_from_bsd(info)?;
    let state = match info.pbi_status {
        libc::SZOMB => ChildState::Unreaped,
        libc::SSTOP => ChildState::Stopped,
        libc::SRUN | libc::SSLEEP => ChildState::Live,
        _ => return None,
    };
    // XNU getsid excludes zombies, including a child that exits after this BSD
    // snapshot. Its missing session cannot erase the native lifetime/state.
    // Live image/session identity still requires an actual session below.
    let session = if state == ChildState::Unreaped {
        None
    } else {
        read_session(lifetime.pid).filter(|s| *s > 0)
    };
    Some(KernelIdentity {
        pid: lifetime.pid,
        parent: info.pbi_ppid,
        pgid: (info.pbi_pgid > 0).then_some(info.pbi_pgid),
        session,
        birth: lifetime.birth,
        state,
    })
}

#[cfg(all(test, target_os = "macos"))]
#[path = "procs_bsd_tests.rs"]
mod bsd_tests;

#[cfg(target_os = "linux")]
fn kernel_identity(pid: u32) -> Option<KernelIdentity> {
    if pid <= 1 {
        return None;
    }
    let path = PathBuf::from(format!("/proc/{pid}/stat"));
    parse_linux_identity(&crate::store::read_bounded(&path, 16 * 1024, false)?, pid)
}

#[cfg(any(target_os = "linux", test))]
fn parse_linux_identity(text: &str, pid: u32) -> Option<KernelIdentity> {
    let (named, rest) = text.split_once(" (")?;
    if named.parse::<u32>().ok()? != pid || pid <= 1 {
        return None;
    }
    let (_, fields) = rest.rsplit_once(") ")?;
    let mut fields = fields.split_ascii_whitespace();
    let state = match fields.next()? {
        "Z" => ChildState::Unreaped,
        "T" | "t" => ChildState::Stopped,
        "R" | "S" | "D" | "I" | "P" => ChildState::Live,
        _ => return None,
    };
    let positive = |word: &str| word.parse::<u32>().ok().filter(|n| *n > 0);
    let parent = positive(fields.next()?)?;
    let pgid = positive(fields.next()?)?;
    let session = positive(fields.next()?)?;
    // The remaining iterator begins at field 7; starttime is field 22.
    let start = fields
        .nth(15)?
        .parse::<u64>()
        .ok()
        .filter(|ticks| *ticks > 0)?;
    Some(KernelIdentity {
        pid,
        parent,
        pgid: Some(pgid),
        session: Some(session),
        birth: Birth::Linux(start),
        state,
    })
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn kernel_identity(_pid: u32) -> Option<KernelIdentity> {
    None
}

#[cfg(target_os = "linux")]
fn executable(pid: u32) -> Option<Executable> {
    use std::os::unix::fs::MetadataExt as _;
    let link = PathBuf::from(format!("/proc/{pid}/exe"));
    let path = std::fs::read_link(&link).ok()?;
    let meta = std::fs::metadata(link).ok()?;
    (path.is_absolute() && meta.is_file() && meta.ino() != 0).then_some(Executable {
        path,
        device: meta.dev(),
        inode: meta.ino(),
    })
}

#[cfg(target_os = "macos")]
fn executable(pid: u32) -> Option<Executable> {
    use std::os::unix::{ffi::OsStringExt as _, fs::MetadataExt as _};
    let pid = libc::pid_t::try_from(pid).ok().filter(|p| *p > 1)?;
    let mut path = [0u8; 4096];
    // SAFETY: the kernel receives the full writable path buffer and its capacity.
    let n = unsafe { libc::proc_pidpath(pid, path.as_mut_ptr().cast(), path.len() as u32) };
    if n <= 0 || usize::try_from(n).ok()? >= path.len() {
        return None;
    }
    let end = path.iter().position(|b| *b == 0)?;
    let path = PathBuf::from(std::ffi::OsString::from_vec(path[..end].to_vec()));
    let meta = std::fs::metadata(&path).ok()?;
    if !path.is_absolute() || !meta.is_file() || meta.ino() == 0 {
        return None;
    }
    // Verify the running mapping's vnode too: a replacement at the image's
    // pathname must not silently change the captured executable identity.
    let (device, inode) = macos_image_file(pid)?;
    (device == meta.dev() && inode == meta.ino()).then_some(Executable {
        path,
        device,
        inode,
    })
}

#[cfg(target_os = "macos")]
fn macos_image_file(pid: libc::pid_t) -> Option<(u64, u64)> {
    // sys/proc_info.h: proc_regioninfo followed by vnode_info_path.
    const REGION_SIZE: usize = 96 + std::mem::size_of::<libc::vnode_info_path>();
    const _: () = assert!(REGION_SIZE == 1272);
    let mut address = 0u64;
    for _ in 0..64 {
        let mut bytes = [0u8; REGION_SIZE];
        // SAFETY: bytes has exactly the size requested by flavor 8
        // (PROC_PIDREGIONPATHINFO). Every offset below is bounded and decoded.
        let n = unsafe {
            libc::proc_pidinfo(
                pid,
                8,
                address,
                bytes.as_mut_ptr().cast(),
                REGION_SIZE as i32,
            )
        };
        if usize::try_from(n).ok()? != REGION_SIZE {
            return None;
        }
        let u64_at = |at: usize| {
            let end = at.checked_add(8)?;
            Some(u64::from_ne_bytes(bytes.get(at..end)?.try_into().ok()?))
        };
        let inode = u64_at(104)?;
        if inode != 0 {
            let device = u32::from_ne_bytes(bytes.get(96..100)?.try_into().ok()?);
            return Some((u64::from(device), inode));
        }
        let next = u64_at(80)?.checked_add(u64_at(88)?)?;
        if next <= address {
            return None;
        }
        address = next;
    }
    None
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn executable(_pid: u32) -> Option<Executable> {
    None
}

/// The descendants that are background work (aterm `upgrade_drive.rs:981-990`).
pub const BACKGROUND: &[&str] = &[
    "sh",
    "bash",
    "zsh",
    "fish",
    "dash",
    "ksh",
    "tcsh",
    "caffeinate",
];

/// The vendor's own keep-awake child, which is not work (aterm
/// `upgrade_drive.rs:992`, `OWN_KEEP_AWAKE`): only a `caffeinate` whose parent
/// is the vendor itself is exempt.
pub const OWN_KEEP_AWAKE: &str = "caffeinate";

/// The most processes the walk visits (aterm's bound, `upgrade_drive.rs:1007`).
pub const MAX_WALK: usize = 4096;

/// What the safe point reports when the table cannot be read (aterm
/// `upgrade_drive.rs:1023`, `UNREADABLE_TABLE`).
pub const UNREADABLE: &str = "ps-unreadable";

/// How long the whole read of the table may take, whichever `ps` answers it,
/// before the table counts as unreadable.
pub const PS_DEADLINE_MS: u64 = 5_000;

/// Where `ps` is looked for, in order (never through `PATH`): a later path is
/// asked only when the one before it cannot be started at all.
pub const PS_PATHS: &[&str] = &["/bin/ps", "/usr/bin/ps"];

/// A command's base name, a login shell's leading `-` removed (aterm
/// `upgrade_drive.rs:743-749`, `base_name`).
#[must_use]
pub fn base_name(comm: &str) -> String {
    comm.rsplit('/')
        .next()
        .unwrap_or(comm)
        .trim_start_matches('-')
        .to_string()
}

/// Parse `ps -A -o pid=,ppid=,comm=` output (aterm `ps_table`).
#[must_use]
pub fn parse_ps(out: &str) -> Vec<Row> {
    out.lines()
        .filter_map(|l| {
            let mut it = l.split_whitespace();
            let pid = it.next()?.parse().ok()?;
            let ppid = it.next()?.parse().ok()?;
            Some((pid, ppid, base_name(&it.collect::<Vec<_>>().join(" "))))
        })
        .collect()
}

/// The background work under `pid` in `table`: every descendant whose name is
/// in [`BACKGROUND`], except the vendor's own direct keep-awake (aterm
/// `upgrade_drive.rs:1001-1021`, `background_procs`).
#[must_use]
pub fn background(pid: u32, table: &[Row]) -> Vec<String> {
    let mut frontier = vec![pid];
    let mut found = Vec::new();
    let mut seen = 0usize;
    while let Some(p) = frontier.pop() {
        seen = seen.saturating_add(1);
        if seen > MAX_WALK {
            break;
        }
        for (c, pp, name) in table {
            if *pp == p {
                frontier.push(*c);
                let own_keep_awake = p == pid && name == OWN_KEEP_AWAKE;
                if BACKGROUND.contains(&name.as_str()) && !own_keep_awake {
                    found.push(name.clone());
                }
            }
        }
    }
    found
}

/// The background work under `pid` now, or [`UNREADABLE`] when the table
/// cannot be read (aterm `background_or_unreadable`): an empty answer always
/// means a table was read and nothing runs.
#[must_use]
pub fn live_background(pid: u32) -> Vec<String> {
    match table() {
        Some(t) if !t.is_empty() => background(pid, &t),
        _ => vec![UNREADABLE.to_string()],
    }
}

/// The process table, `None` when `ps` did not answer in time. The whole read
/// has one deadline ([`PS_DEADLINE_MS`]) whichever of [`PS_PATHS`] answers it.
#[must_use]
pub fn table() -> Option<Vec<Row>> {
    ps_table(PS_PATHS, Duration::from_millis(PS_DEADLINE_MS)).map(|out| parse_ps(&out))
}

/// What running one `ps` came to.
#[derive(Debug, PartialEq, Eq)]
enum Ps {
    /// It ran and exited with success: its output.
    Answered(String),
    /// It could not be started at all (not there, not executable): nothing
    /// ran and no time passed, so the next path may be tried.
    NotStarted,
    /// It started and then timed out, failed or wrote no readable text. The
    /// table is unreadable; another path is not asked.
    Failed,
}

/// The output of the first of `paths` that can be started, under one deadline
/// `limit` from now for the whole read. A later path is tried only when the
/// one before it could not be started at all: a `ps` that started and then
/// timed out or failed leaves the table unreadable, because a second `ps` run
/// after it would hold the supervisor (which passes on no signal and reaps
/// nothing meanwhile) for a second deadline.
fn ps_table(paths: &[&str], limit: Duration) -> Option<String> {
    let deadline = Instant::now() + limit;
    for ps in paths {
        match ps_output(ps, deadline) {
            Ps::Answered(out) => return Some(out),
            Ps::NotStarted => {}
            Ps::Failed => return None,
        }
    }
    None
}

/// Run one `ps` until `deadline` ([`run_until`]).
fn ps_output(ps: &str, deadline: Instant) -> Ps {
    let mut command = Command::new(ps);
    command
        .args(["-A", "-o", "pid=,ppid=,comm="])
        .env("LC_ALL", "C");
    match run_until(&mut command, deadline) {
        Ran::Ended { code: Some(0), out } => {
            String::from_utf8(out).map_or(Ps::Failed, Ps::Answered)
        }
        Ran::NotStarted => Ps::NotStarted,
        Ran::Ended { .. } | Ran::Cut => Ps::Failed,
    }
}

/// The folder process `pid` runs in (its current directory), as the kernel
/// names it; `None` where it cannot be read (the process is gone, is not
/// this user's, or the platform has no such read).
#[cfg(target_os = "macos")]
#[must_use]
pub fn cwd_of(pid: u32) -> Option<PathBuf> {
    use std::os::unix::ffi::OsStringExt as _;
    let pid = libc::c_int::try_from(pid).ok()?;
    let mut info = std::mem::MaybeUninit::<libc::proc_vnodepathinfo>::zeroed();
    let size = libc::c_int::try_from(std::mem::size_of::<libc::proc_vnodepathinfo>()).ok()?;
    // SAFETY: PROC_PIDVNODEPATHINFO writes at most `size` bytes into this
    // correctly laid-out native record.
    let got = unsafe {
        libc::proc_pidinfo(
            pid,
            libc::PROC_PIDVNODEPATHINFO,
            0,
            info.as_mut_ptr().cast(),
            size,
        )
    };
    if got != size {
        return None;
    }
    // SAFETY: zeroed plain-data storage the kernel filled whole.
    let info = unsafe { info.assume_init() };
    let path: Vec<u8> = info
        .pvi_cdir
        .vip_path
        .iter()
        .flatten()
        .map(|c| u8::from_ne_bytes(c.to_ne_bytes()))
        .take_while(|byte| *byte != 0)
        .collect();
    let path = PathBuf::from(std::ffi::OsString::from_vec(path));
    path.is_absolute().then_some(path)
}

/// The folder process `pid` runs in (its current directory), as the kernel
/// names it; `None` where it cannot be read (the process is gone, is not
/// this user's, or the platform has no such read).
#[cfg(target_os = "linux")]
#[must_use]
pub fn cwd_of(pid: u32) -> Option<PathBuf> {
    std::fs::read_link(format!("/proc/{pid}/cwd"))
        .ok()
        .filter(|path| path.is_absolute())
}

/// The folder process `pid` runs in: no platform read here.
#[cfg(not(any(target_os = "macos", target_os = "linux")))]
#[must_use]
pub fn cwd_of(_pid: u32) -> Option<PathBuf> {
    None
}

/// What running one command under a deadline came to ([`run_until`]).
#[derive(Debug, PartialEq, Eq)]
pub enum Ran {
    /// It ran to its end: its exit code (`None` where a signal ended it) and
    /// everything it wrote to its standard output.
    Ended {
        /// The exit code, `None` for a signal.
        code: Option<i32>,
        /// Its standard output, whole.
        out: Vec<u8>,
    },
    /// It could not be started at all (not there, not executable): nothing
    /// ran and no time passed.
    NotStarted,
    /// The deadline had passed before it could start (nothing was started),
    /// it was still running at the deadline, or a process it left behind
    /// still held its output open then (its whole group was then killed and
    /// it was reaped), or its output or its end could not be read: what it
    /// would have said is not known.
    Cut,
}

/// How long one wait of [`run_until`] lasts at most before it looks at the
/// command's exit again: the pipe wakes it sooner for output or its end.
const RUN_STEP: Duration = Duration::from_millis(5);

/// The most bytes [`run_until`] reads from the pipe at once.
const RUN_READ: usize = 16 * 1024;

/// Run `command` until `deadline`, its standard input empty and its standard
/// error discarded, in a process group of its own. A deadline already passed
/// starts nothing. Its output is read on the calling thread as it is written
/// ([`Running::poll`]), so a large one cannot fill the pipe and stall it, and
/// no thread outlives the call: an own-image move, which needs this process
/// single-threaded, never meets one. Run to its end with its pipe closed, it
/// is reaped and its output returned whole. At the deadline, still running or
/// still holding the pipe open through a process it left behind, its whole
/// group is killed, it is reaped, and the run is cut; so is a run whose pipe
/// or end could not be read.
pub fn run_until(command: &mut Command, deadline: Instant) -> Ran {
    if Instant::now() >= deadline {
        return Ran::Cut;
    }
    let mut running = match Running::start(command) {
        Ok(running) => running,
        Err(ran) => return ran,
    };
    loop {
        let step = deadline
            .saturating_duration_since(Instant::now())
            .min(RUN_STEP);
        if let Some(ran) = running.poll(step) {
            return ran;
        }
        if Instant::now() >= deadline {
            return running.cut();
        }
    }
}

/// One command run a look at a time ([`Running::poll`]), its standard input
/// empty and its standard error discarded, in a process group of its own: the
/// caller decides between looks how long it may run, so a run can outlast
/// many turns of a loop that never waits on it. Its end is looked at without
/// reaping it (`waitid` with `WNOWAIT`), so its pid, and the group it leads, stay
/// its own until the reap. Dropped before its end, or cut ([`Running::cut`]),
/// its whole group is killed and it is reaped.
#[derive(Debug)]
pub struct Running {
    /// The command, until it is reaped.
    child: Option<std::process::Child>,
    /// Its standard output.
    stdout: std::process::ChildStdout,
    /// What it wrote so far.
    out: Vec<u8>,
    /// Room for one read of [`RUN_READ`] bytes.
    buf: Vec<u8>,
    /// Whether its standard output reached its end.
    closed: bool,
}

impl Running {
    /// Start `command`.
    ///
    /// # Errors
    /// [`Ran::NotStarted`] where it could not be started (not there, not
    /// executable); [`Ran::Cut`] where its output could not be taken, when it
    /// is killed with its group and reaped.
    pub fn start(command: &mut Command) -> Result<Self, Ran> {
        use std::os::unix::process::CommandExt as _;
        let Ok(mut child) = command
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .process_group(0)
            .spawn()
        else {
            return Err(Ran::NotStarted);
        };
        let Some(stdout) = child.stdout.take() else {
            return Err(cut(&mut child));
        };
        Ok(Self {
            child: Some(child),
            stdout,
            out: Vec::new(),
            buf: vec![0_u8; RUN_READ],
            closed: false,
        })
    }

    /// One look, waiting up to `wait` for output: `None` while it runs or
    /// holds its pipe open; [`Ran::Ended`] once it has ended with its pipe
    /// closed, when it is reaped and its output returned whole; [`Ran::Cut`]
    /// where its pipe or its end could not be read, when its group is killed
    /// and it is reaped, and at every look after it answered.
    pub fn poll(&mut self, wait: Duration) -> Option<Ran> {
        let Some(child) = self.child.as_mut() else {
            return Some(Ran::Cut);
        };
        let Some(ended) = exited_unreaped(child.id()) else {
            return Some(self.cut());
        };
        if ended && self.closed {
            let ran = match child.wait() {
                Ok(status) => Ran::Ended {
                    code: status.code(),
                    out: std::mem::take(&mut self.out),
                },
                Err(_) => Ran::Cut,
            };
            self.child = None;
            return Some(ran);
        }
        if self.closed {
            std::thread::sleep(wait);
            return None;
        }
        match readable(&self.stdout, wait) {
            Some(true) => match self.stdout.read(&mut self.buf) {
                Ok(0) => self.closed = true,
                Ok(n) => {
                    if let Some(read) = self.buf.get(..n) {
                        self.out.extend_from_slice(read);
                    } else {
                        return Some(self.cut());
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                Err(_) => return Some(self.cut()),
            },
            Some(false) => {}
            None => return Some(self.cut()),
        }
        None
    }

    /// End the run without its answer ([`cut`]): [`Ran::Cut`].
    pub fn cut(&mut self) -> Ran {
        match self.child.take() {
            Some(mut child) => cut(&mut child),
            None => Ran::Cut,
        }
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        let _ = self.cut();
    }
}

/// End a run without its answer: SIGKILL to the group `child` leads (its pid,
/// which no other group can take while it is unreaped) and to `child` itself,
/// in case it left that group, then reap it.
fn cut(child: &mut std::process::Child) -> Ran {
    if let Ok(group) = libc::pid_t::try_from(child.id()) {
        // SAFETY: a plain syscall; `group` is the pid of this process's own
        // unreaped child, which leads the group it was spawned into.
        unsafe { libc::killpg(group, libc::SIGKILL) };
    }
    let _ = child.kill();
    let _ = child.wait();
    Ran::Cut
}

/// Whether the unreaped child `pid` has ended, looked at without reaping it
/// (`WNOWAIT`), so its pid stays its own until it is reaped. `None` where
/// the look failed.
fn exited_unreaped(pid: u32) -> Option<bool> {
    let pid = libc::pid_t::try_from(pid).ok()?;
    let id = libc::id_t::try_from(pid).ok()?;
    loop {
        let mut info = std::mem::MaybeUninit::<libc::siginfo_t>::zeroed();
        // SAFETY: `info` is writable siginfo_t storage. WNOWAIT leaves the
        // child's end pending for its reap; WNOHANG never waits.
        let rc = unsafe {
            libc::waitid(
                libc::P_PID,
                id,
                info.as_mut_ptr(),
                libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
            )
        };
        if rc == 0 {
            // SAFETY: zeroed plain-data storage, which the call either left
            // as it was (no end to report: `si_signo` stays 0) or filled.
            let info = unsafe { info.assume_init() };
            return Some(info.si_signo == libc::SIGCHLD);
        }
        if std::io::Error::last_os_error().raw_os_error() != Some(libc::EINTR) {
            return None;
        }
    }
}

/// Whether `pipe` can be read without waiting (output, or its end), waiting
/// for that up to `wait`: `Some(false)` where nothing came in time or the
/// wait was interrupted, `None` where the pipe reports an error.
fn readable(pipe: &impl std::os::fd::AsRawFd, wait: Duration) -> Option<bool> {
    let mut fd = libc::pollfd {
        fd: pipe.as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    };
    let ms = libc::c_int::try_from(wait.as_millis()).unwrap_or(libc::c_int::MAX);
    // SAFETY: `fd` is one valid pollfd for the call's duration, naming the
    // pipe `pipe` keeps open.
    let n = unsafe { libc::poll(&raw mut fd, 1, ms) };
    if n < 0 {
        return (std::io::Error::last_os_error().raw_os_error() == Some(libc::EINTR))
            .then_some(false);
    }
    if n == 0 {
        return Some(false);
    }
    if fd.revents & (libc::POLLIN | libc::POLLHUP) != 0 {
        Some(true)
    } else {
        None
    }
}

#[cfg(test)]
#[path = "procs_run_tests.rs"]
mod run_tests;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_exec_between_kernel_observations_cannot_keep_the_old_image() {
        let expected = ChildIdentity {
            pid: 700,
            parent: std::process::id(),
            pgid: 600,
            session: 500,
            birth: Birth::Linux(123),
            executable: Executable {
                path: PathBuf::from("/old-image"),
                device: 1,
                inode: 2,
            },
        };
        let kernel = |_: u32| {
            Some(KernelIdentity {
                pid: expected.pid,
                parent: expected.parent,
                pgid: Some(expected.pgid),
                session: Some(expected.session),
                birth: expected.birth.clone(),
                state: ChildState::Live,
            })
        };
        assert_eq!(
            observe_child_using(
                &expected,
                kernel,
                |_| Some(expected.executable.clone()),
                |_| true
            ),
            Some(ChildObservation {
                state: ChildState::Live,
                pgid: Some(expected.pgid),
                session: Some(expected.session)
            })
        );
        let mut images = 0;
        let changed = observe_child_using(
            &expected,
            kernel,
            |_| {
                images += 1;
                let mut image = expected.executable.clone();
                if images == 2 {
                    image.inode += 1;
                }
                Some(image)
            },
            |_| true,
        );
        assert_eq!(changed, None);
        assert_eq!(
            images, 2,
            "the image after the last identity read is required"
        );
    }

    #[test]
    fn terminal_observation_keeps_unknown_facts_without_weakening_live_proof() {
        let expected = ChildIdentity {
            pid: 700,
            parent: std::process::id(),
            pgid: 600,
            session: 500,
            birth: Birth::Linux(123),
            executable: Executable {
                path: PathBuf::from("/old-image"),
                device: 1,
                inode: 2,
            },
        };
        let terminal = KernelIdentity {
            pid: expected.pid,
            parent: expected.parent,
            pgid: None,
            session: None,
            birth: expected.birth.clone(),
            state: ChildState::Unreaped,
        };
        let no_image =
            |_: u32| -> Option<Executable> { panic!("an unreaped child has no live image") };
        assert_eq!(
            observe_child_using(&expected, |_| Some(terminal.clone()), no_image, |_| true),
            Some(ChildObservation {
                state: ChildState::Unreaped,
                pgid: None,
                session: None
            })
        );
        assert_eq!(
            observe_child_using(&expected, |_| Some(terminal.clone()), no_image, |_| false),
            None,
            "birth without wait ownership cannot dispatch an end"
        );

        let mut reused = terminal.clone();
        reused.birth = Birth::Linux(124);
        assert_eq!(
            observe_child_using(&expected, |_| Some(reused.clone()), no_image, |_| true),
            None,
            "an owned reused pid is not the captured child"
        );
        for state in [ChildState::Live, ChildState::Stopped] {
            let mut incomplete = terminal.clone();
            incomplete.state = state;
            incomplete.pgid = Some(expected.pgid);
            assert_eq!(
                observe_child_using(&expected, |_| Some(incomplete.clone()), no_image, |_| true),
                None,
                "missing session cannot prove {state:?}"
            );
        }
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    struct NativeCat {
        child: std::process::Child,
        reader: Option<std::thread::JoinHandle<()>>,
        reaped: bool,
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    impl Drop for NativeCat {
        fn drop(&mut self) {
            if !self.reaped {
                let _ = self.child.kill();
                let _ = self.child.wait();
            }
            if let Some(reader) = self.reader.take() {
                let _ = reader.join();
            }
        }
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    impl NativeCat {
        fn start() -> Self {
            use std::io::Write as _;
            const PROOF: &[u8] = b"native-cat-identity-proof\n";
            let mut cat = Self {
                child: Command::new("/bin/cat")
                    .stdin(Stdio::piped())
                    .stdout(Stdio::piped())
                    .stderr(Stdio::null())
                    .spawn()
                    .expect("native cat in place"),
                reader: None,
                reaped: false,
            };
            let mut stdout = cat.child.stdout.take().expect("cat stdout");
            let (send, receive) = std::sync::mpsc::channel();
            cat.reader = Some(std::thread::spawn(move || {
                let mut proof = [0u8; PROOF.len()];
                let answer = stdout.read_exact(&mut proof).map(|_| proof);
                let _ = send.send(answer);
            }));
            cat.child
                .stdin
                .as_mut()
                .expect("cat stdin")
                .write_all(PROOF)
                .expect("write proof");
            let proof = receive
                .recv_timeout(Duration::from_secs(60))
                .expect("cat output hang detector")
                .expect("read proof");
            assert_eq!(proof.as_slice(), PROOF);
            cat
        }

        fn wait_event(&mut self, phase: &str) -> crate::sys::ChildEvent {
            let event = native_wait(phase, || {
                crate::sys::wait_child(libc::pid_t::try_from(self.child.id()).expect("child pid"))
            });
            if matches!(
                event,
                crate::sys::ChildEvent::Exited(_) | crate::sys::ChildEvent::Signaled(_)
            ) {
                self.reaped = true;
            }
            event
        }
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    fn native_wait<T>(phase: &str, mut ready: impl FnMut() -> Option<T>) -> T {
        let deadline = Instant::now() + Duration::from_secs(60);
        loop {
            if let Some(value) = ready() {
                return value;
            }
            assert!(
                Instant::now() < deadline,
                "native child hang detector: {phase}"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn a_launch_helper_can_pin_only_its_same_lifetime_expected_vendor_after_exec() {
        use std::io::{BufRead as _, Write as _};
        let mut actor = NativeCat {
            child: Command::new("/bin/sh")
                .args([
                    "-c",
                    "printf 'helper-ready\\n'; IFS= read -r gate; exec /bin/cat",
                ])
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .spawn()
                .expect("launch helper"),
            reader: None,
            reaped: false,
        };
        let stdout = actor.child.stdout.take().unwrap();
        let (send, receive) = std::sync::mpsc::channel();
        actor.reader = Some(std::thread::spawn(move || {
            let mut reader = std::io::BufReader::new(stdout);
            for _ in 0..2 {
                let mut line = String::new();
                let answer = reader.read_line(&mut line).map(|_| line);
                let _ = send.send(answer);
            }
        }));
        assert_eq!(
            receive
                .recv_timeout(Duration::from_secs(60))
                .unwrap()
                .unwrap(),
            "helper-ready\n"
        );
        let original = capture_child(actor.child.id()).expect("original helper identity");
        assert_eq!(
            refresh_child_image(&original, Path::new("/bin/cat")),
            None,
            "helper has not execed the vendor"
        );
        actor
            .child
            .stdin
            .as_mut()
            .unwrap()
            .write_all(b"go\nvendor-body-proof\n")
            .unwrap();
        assert_eq!(
            receive
                .recv_timeout(Duration::from_secs(60))
                .unwrap()
                .unwrap(),
            "vendor-body-proof\n"
        );
        let fresh = refresh_child_image(&original, Path::new("/bin/cat"))
            .expect("same child now runs vendor");
        assert_eq!(fresh.pid, original.pid);
        assert_eq!(fresh.birth, original.birth);
        assert_ne!(fresh.executable, original.executable);
        assert!(observe_child(&fresh).is_some());
        assert_eq!(
            observe_child(&original),
            None,
            "the old image remains invalid"
        );
        assert_eq!(
            refresh_child_image(&original, Path::new("/bin/sh")),
            None,
            "an unrelated image is never adopted"
        );
        let mut wrong = original.clone();
        wrong.birth = match original.birth {
            Birth::MacOs {
                seconds,
                microseconds,
            } => Birth::MacOs {
                seconds: seconds + 1,
                microseconds,
            },
            Birth::Linux(ticks) => Birth::Linux(ticks + 1),
        };
        assert_eq!(
            refresh_child_image(&wrong, Path::new("/bin/cat")),
            None,
            "a pid with a different lifetime is never adopted"
        );
        let mut wrong = original;
        wrong.parent += 1;
        assert_eq!(
            refresh_child_image(&wrong, Path::new("/bin/cat")),
            None,
            "changed parentage is never adopted"
        );
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn own_children_lists_a_child_until_it_is_reaped() {
        let mut cat = NativeCat::start();
        let pid = cat.child.id();
        // Sibling tests spawn children too, so only this one child is checked.
        assert!(own_children().expect("child list").contains(&pid), "live");
        assert!(
            !own_children()
                .expect("child list")
                .contains(&std::process::id()),
            "self is not a child"
        );
        assert!(crate::sys::kill(
            libc::pid_t::try_from(pid).expect("pid"),
            libc::SIGKILL
        ));
        native_wait("cat exits unreaped", || {
            kernel_identity(pid)
                .is_some_and(|seen| seen.state == ChildState::Unreaped)
                .then_some(())
        });
        assert!(
            own_children().expect("child list").contains(&pid),
            "an exited child stays listed until it is reaped"
        );
        assert_eq!(
            cat.wait_event("cat end"),
            crate::sys::ChildEvent::Signaled(libc::SIGKILL)
        );
        assert!(
            !own_children().expect("child list").contains(&pid),
            "reaped"
        );
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn native_identity_observes_cat_without_consuming_its_wait_events() {
        use std::os::unix::fs::MetadataExt as _;
        let mut cat = NativeCat::start();
        let pid = cat.child.id();
        let identity = capture_child(pid).expect("proven running cat identity");
        let installed = std::fs::metadata("/bin/cat").expect("cat metadata");
        assert_eq!(
            (identity.executable.device, identity.executable.inode),
            (installed.dev(), installed.ino())
        );
        assert_eq!(
            observe_child(&identity),
            Some(ChildObservation {
                state: ChildState::Live,
                pgid: Some(identity.pgid),
                session: Some(identity.session)
            })
        );
        assert!(
            capture_child(std::process::id()).is_none(),
            "self is not an owned child"
        );
        assert!(
            capture_child(std::os::unix::process::parent_id()).is_none(),
            "parent is not an owned child"
        );
        let mut changed = identity.clone();
        changed.executable.inode ^= 1;
        assert_eq!(
            observe_child(&changed),
            None,
            "a different expected image cannot adopt"
        );
        changed = identity.clone();
        changed.parent ^= 1;
        assert_eq!(
            observe_child(&changed),
            None,
            "a different expected parent cannot adopt"
        );

        let pid = libc::pid_t::try_from(pid).expect("child pid");
        assert!(crate::sys::kill(pid, libc::SIGSTOP));
        native_wait("observe stopped cat after SIGSTOP", || {
            observe_child(&identity).filter(|seen| seen.state == ChildState::Stopped)
        });
        assert!(matches!(
            cat.wait_event("normal stop event after non-consuming observation"),
            crate::sys::ChildEvent::Stopped(libc::SIGSTOP)
        ));
        assert!(crate::sys::kill(pid, libc::SIGCONT));
        native_wait("observe live cat after SIGCONT", || {
            observe_child(&identity).filter(|seen| seen.state == ChildState::Live)
        });
        assert!(matches!(
            cat.wait_event("normal continued event after non-consuming observation"),
            crate::sys::ChildEvent::Continued
        ));

        drop(cat.child.stdin.take());
        let exited = native_wait("observe unreaped cat after stdin EOF", || {
            observe_child(&identity).filter(|seen| seen.state == ChildState::Unreaped)
        });
        #[cfg(target_os = "macos")]
        assert_eq!(exited.session, None, "Darwin's zombie SID remains unknown");
        assert_eq!(
            observe_child(&identity),
            Some(exited),
            "a second observation must not reap"
        );
        assert!(matches!(
            cat.wait_event("normal exit event after two non-consuming observations"),
            crate::sys::ChildEvent::Exited(0)
        ));
        assert_eq!(observe_child(&identity), None, "normal wait reaped it once");
    }

    #[test]
    fn linux_birth_fields_survive_spaces_and_parentheses_in_the_command() {
        let stat =
            "700 (a ) complex (name)) Z 600 500 400 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 123456 0 0\n";
        let identity = parse_linux_identity(stat, 700).expect("unreaped record");
        assert_eq!(identity.parent, 600);
        assert_eq!(identity.pgid, Some(500));
        assert_eq!(identity.session, Some(400));
        assert_eq!(identity.birth, Birth::Linux(123456));
        assert_eq!(identity.state, ChildState::Unreaped);
        assert!(parse_linux_identity(stat, 701).is_none());
        assert!(parse_linux_identity("700 (truncated) S 600 500 400", 700).is_none());
        assert!(parse_linux_identity(&stat.replace(" Z ", " ? "), 700).is_none());
    }

    fn row(pid: u32, ppid: u32, name: &str) -> Row {
        (pid, ppid, name.to_string())
    }

    #[test]
    fn ps_output_is_read_by_base_name() {
        let out = "    1     0 /sbin/launchd\n  700   600 -zsh\n  701   700 /bin/zsh\n  702   701 (trustc)\n garbage\n";
        assert_eq!(
            parse_ps(out),
            [
                row(1, 0, "launchd"),
                row(700, 600, "zsh"),
                row(701, 700, "zsh"),
                row(702, 701, "(trustc)"),
            ]
        );
    }

    #[test]
    fn a_descendant_shell_is_background_work() {
        // The vendor (100) ran a background Bash task: a shell under it, a build under that.
        let t = [
            row(100, 50, "claude"),
            row(101, 100, "zsh"),
            row(102, 101, "cargo"),
            row(200, 1, "zsh"),
        ];
        assert_eq!(background(100, &t), ["zsh"]);
        // A shell anywhere below counts, not only a direct child.
        let t = [
            row(100, 50, "claude"),
            row(101, 100, "node"),
            row(102, 101, "bash"),
        ];
        assert_eq!(background(100, &t), ["bash"]);
    }

    #[test]
    fn the_vendors_own_keep_awake_is_not_work_but_a_nested_one_is() {
        let t = [row(100, 50, "claude"), row(101, 100, "caffeinate")];
        assert!(background(100, &t).is_empty());
        let t = [
            row(100, 50, "claude"),
            row(101, 100, "zsh"),
            row(102, 101, "caffeinate"),
        ];
        assert_eq!(background(100, &t), ["zsh", "caffeinate"]);
    }

    #[test]
    fn an_idle_vendor_with_no_shells_has_no_work() {
        let t = [
            row(100, 50, "claude"),
            row(101, 100, "node"),
            row(102, 100, "(zsh)"),
            row(50, 1, "zsh"),
        ];
        assert!(
            background(100, &t).is_empty(),
            "the parent shell and a zombie (shown in parentheses) are not the vendor's work"
        );
    }

    #[test]
    fn a_cycle_in_the_table_terminates() {
        let t = [row(100, 101, "zsh"), row(101, 100, "zsh")];
        let found = background(100, &t);
        assert!(!found.is_empty() && found.len() <= MAX_WALK);
    }

    /// A private directory of stand-in `ps` programs, removed on drop.
    struct FakePs(PathBuf);

    impl FakePs {
        fn new(name: &str) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "claude-alab-fake-ps-{name}-{}-{}",
                std::process::id(),
                crate::supervise::now_ms()
            ));
            std::fs::create_dir_all(&dir).expect("fake ps dir");
            Self(dir)
        }

        /// A shell script `name` running `body`, started once before it is
        /// returned: a sibling test's fork can briefly hold the descriptor it
        /// was written through, and an exec of it would then be refused as
        /// busy, which the read under test would take for a missing program.
        fn script(&self, name: &str, body: &str) -> String {
            use std::os::unix::fs::PermissionsExt as _;
            let path = self.0.join(name);
            std::fs::write(
                &path,
                format!("#!/bin/sh\n[ \"$1\" = check ] && exit 0\n{body}\n"),
            )
            .expect("fake ps");
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700))
                .expect("fake ps mode");
            let started = Instant::now();
            while !Command::new(&path)
                .arg("check")
                .status()
                .is_ok_and(|status| status.success())
            {
                assert!(started.elapsed() < Duration::from_secs(10), "fake ps");
                std::thread::sleep(Duration::from_millis(5));
            }
            path.to_str().expect("utf-8 temp dir").to_string()
        }

        fn path(&self, name: &str) -> String {
            self.0.join(name).to_str().expect("utf-8").to_string()
        }
    }

    impl Drop for FakePs {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn a_ps_that_timed_out_or_failed_is_not_followed_by_a_second_one() {
        let fakes = FakePs::new("one-deadline");
        // `exec`, so the kill reaches the sleeper and its pipe end closes.
        let hang = fakes.script("hang", "exec /bin/sleep 30");
        let fail = fakes.script("fail", "echo '7 1 /bin/zsh'\nexit 3");
        let good = fakes.script("good", "echo '7 1 /bin/zsh'");
        let limit = Duration::from_millis(300);

        let started = Instant::now();
        assert_eq!(ps_table(&[&hang, &good], limit), None, "timed out");
        let took = started.elapsed();
        assert!(took >= limit, "the deadline was waited for: {took:?}");
        assert!(took < Duration::from_secs(5), "one deadline: {took:?}");

        assert_eq!(ps_table(&[&fail, &good], limit), None, "failed");
        assert_eq!(ps_table(&[&hang, &hang], limit), None);
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "every read is bounded: {:?}",
            started.elapsed()
        );
    }

    #[test]
    fn only_a_ps_that_cannot_be_started_falls_through_to_the_next_path() {
        use std::os::unix::fs::PermissionsExt as _;
        let fakes = FakePs::new("fall-through");
        let good = fakes.script("good", "echo '7 1 /bin/zsh'");
        let missing = fakes.path("missing");
        let plain = fakes.path("plain");
        std::fs::write(&plain, "#!/bin/sh\necho '9 1 sh'\n").expect("plain file");
        std::fs::set_permissions(&plain, std::fs::Permissions::from_mode(0o600))
            .expect("plain mode");
        let limit = Duration::from_secs(5);
        let table = Some("7 1 /bin/zsh\n".to_string());
        assert_eq!(ps_table(&[&good], limit), table);
        assert_eq!(ps_table(&[&missing, &good], limit), table, "not found");
        assert_eq!(ps_table(&[&plain, &good], limit), table, "not executable");
        assert_eq!(ps_table(&[&missing, &plain], limit), None);
        assert_eq!(ps_table(&[], limit), None);
        assert_eq!(ps_output(&missing, Instant::now() + limit), Ps::NotStarted);
    }

    #[test]
    fn this_process_reads_a_live_table_with_its_own_shell_under_it() {
        let mut sh = Command::new("/bin/sh")
            .args(["-c", "sleep 30; :"])
            .spawn()
            .expect("spawn a shell");
        let me = std::process::id();
        let mut found = Vec::new();
        for _ in 0..50 {
            found = live_background(me);
            if !found.is_empty() && !found.iter().any(|n| n == UNREADABLE) {
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        let _ = sh.kill();
        let _ = sh.wait();
        assert!(
            found.iter().any(|n| BACKGROUND.contains(&n.as_str())),
            "a live shell under this process is seen: {found:?}"
        );
        assert!(!found.iter().any(|n| n == UNREADABLE), "{found:?}");
    }
}
