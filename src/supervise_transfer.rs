// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Lossless bounded runtime data for the same supervisor and already-owned child.
//!
//! This child module never spawns, installs signal handlers, owns inherited
//! signal/authority/gate descriptors, resolves a new journal, saves current
//! terminal modes, or restores modes to the terminal. The caller owns early
//! entry validation, signal attachment, authority reproof, rollback and mask
//! restoration. A decoded payload alone authorizes none of those actions.

use super::{Abandoned, Child, Ending, MAX_ABANDONED, Setup, Supervisor};
use crate::cli::{self, Options};
use crate::journal::Journal;
use crate::model_facts;
use crate::procs::{
    self, Birth, ChildIdentity, ChildObservation, ChildState, Executable, ProcessIdentity,
};
use crate::profile;
use crate::quiet::{RunSnapshot, Snapshot, Tracker};
use crate::store::{self, Build, Store};
use crate::sys::{self, DescriptorIdentity, SignalSnapshot, Termios, TermiosSnapshot};
use crate::transfer_capsule::{Decoder, Encoder, Header, MAX_PAYLOAD_BYTES, Phase};
use std::ffi::{OsStr, OsString};
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::path::{Component, Path, PathBuf};

/// The payload layout. 2: `abandoned` is a bounded ascending list of build
/// numbers, where 1 carried one optional build. 3: the two flags that said
/// whether the session could be moved are gone, because a supervisor exists
/// only for one that can ([`super::live_update_refusal`]). 4: the model
/// reader travels ([`crate::model_facts::Reader::put`]), preserving its
/// conversation-entry facts for bounded replay of changed transcript versions.
pub(super) const SCHEMA: u32 = 4;
const MAX_PATH: usize = 16 * 1024;
const MAX_ARG: usize = 128 * 1024;
const MAX_ARGS: usize = 4096;
const MAX_TEXT: usize = 16 * 1024;
const MAX_ENV: usize = 8;
const MAX_ENV_ENTRY: usize = 256;
const MAX_ABI: usize = 256;
const MAX_CONTROLS: usize = 32;

fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

/// Captured data only; inherited descriptor ownership remains with the caller.
#[derive(Clone, Debug)]
pub(super) struct Saved {
    setup: Setup,
    process: ProcessIdentity,
    cwd_identity: DescriptorIdentity,
    signals: SignalSnapshot,
    termios: TermiosSnapshot,
    journal_path: Option<PathBuf>,
    journal_pid: u32,
    config: PathBuf,
    cwd: Option<PathBuf>,
    child_pid: libc::pid_t,
    child_build: Build,
    child_args: Vec<OsString>,
    child_identity: ChildIdentity,
    tracker: Snapshot,
    spawned_ms: u64,
    stopped: bool,
    ending: u8,
    noted: String,
    seen_target: Option<u64>,
    abandoned: Abandoned,
    background_hold_until: u64,
    background_found: String,
    facts: model_facts::Reader,
}

/// The same child may have stopped or ended during exec. Unreaped is only
/// normal terminal-event dispatch evidence, never live adoption or spawn permission.
pub(super) struct Restored {
    pub(super) supervisor: Supervisor,
    pub(super) child_state: ChildState,
}

fn path(value: &Path) -> io::Result<()> {
    let bytes = value.as_os_str().as_bytes();
    if !value.is_absolute() || bytes.len() > MAX_PATH || bytes.contains(&0) {
        Err(invalid(
            "runtime transfer path is not bounded absolute Unix data",
        ))
    } else {
        Ok(())
    }
}

fn text(value: &str, maximum: usize) -> io::Result<()> {
    if value.len() > maximum || value.contains('\0') {
        Err(invalid(
            "runtime transfer text exceeds its bound or contains NUL",
        ))
    } else {
        Ok(())
    }
}

fn argv(value: &[OsString]) -> io::Result<()> {
    if value.len() > MAX_ARGS {
        return Err(invalid("too many runtime arguments"));
    }
    for arg in value {
        let bytes = arg.as_bytes();
        if bytes.len() > MAX_ARG || bytes.contains(&0) {
            return Err(invalid("invalid runtime argument bytes"));
        }
    }
    Ok(())
}

fn options(value: &Options) -> io::Result<()> {
    let args: Vec<OsString> = [
        format!("--alab-quiet-ms={}", value.quiet_ms),
        format!("--alab-poll-ms={}", value.poll_ms),
        format!("--alab-term-grace-ms={}", value.term_grace_ms),
    ]
    .into_iter()
    .map(OsString::from)
    .collect();
    match cli::parse(args) {
        Ok(cli::Command::Run {
            opts,
            live_update: true,
            args,
        }) if opts == *value && args.is_empty() => Ok(()),
        _ => Err(invalid(
            "runtime settings are not normalized harness options",
        )),
    }
}

fn build_shape(build: &Build, store: &Store) -> io::Result<()> {
    path(&build.dir)?;
    path(&build.exe)?;
    if build.dir != store.program_dir.join(build.number.to_string())
        || build.exe != build.dir.join("bin").join(&store.command)
        || build.env.len() > MAX_ENV
    {
        return Err(invalid(
            "runtime build differs from the captured store shape",
        ));
    }
    if let Some(version) = &build.version {
        text(version, MAX_ENV_ENTRY)?;
        if !store::record_names_build(version, build.number) {
            return Err(invalid("runtime vendor version does not name its build"));
        }
    }
    let mut lines = String::new();
    for (name, value) in &build.env {
        text(name, MAX_ENV_ENTRY)?;
        text(value, MAX_ENV_ENTRY)?;
        if name
            .len()
            .checked_add(value.len())
            .and_then(|n| n.checked_add(1))
            .is_none_or(|n| n > MAX_ENV_ENTRY)
        {
            return Err(invalid("runtime shim environment entry exceeds its bound"));
        }
        lines.push_str(name);
        lines.push('=');
        lines.push_str(value);
        lines.push('\n');
    }
    if store::parse_shim_env(&lines) != build.env {
        return Err(invalid("runtime shim environment is not normalized"));
    }
    Ok(())
}

fn setup_shape(setup: &Setup) -> io::Result<()> {
    path(&setup.home)?;
    path(&setup.store.program_dir)?;
    options(&setup.opts)?;
    argv(&setup.args)?;
    if setup.depth >= cli::MAX_DEPTH
        || setup.store.command != profile::COMMAND
        || setup.store.program_dir.file_name() != Some(OsStr::new(profile::PACKAGE))
        || setup.store.program_dir.parent().and_then(Path::file_name) != Some(OsStr::new("store"))
        || setup
            .store
            .program_dir
            .components()
            .any(|c| c == Component::ParentDir)
    {
        return Err(invalid(
            "runtime setup has an invalid vendor store, command or nesting depth",
        ));
    }
    path(
        setup
            .own_exe
            .as_deref()
            .ok_or_else(|| invalid("runtime setup has no captured own image"))?,
    )?;
    build_shape(&setup.build, &setup.store)
}

fn birth(value: &Birth) -> io::Result<()> {
    if matches!(value, Birth::MacOs { microseconds, .. } if *microseconds >= 1_000_000) {
        Err(invalid("runtime kernel birth microseconds are invalid"))
    } else {
        Ok(())
    }
}

fn executable(value: &Executable) -> io::Result<()> {
    path(&value.path)
}

fn process_shape(value: &ProcessIdentity) -> io::Result<()> {
    if value.pid <= 1 || value.pgid == 0 || value.session == 0 {
        return Err(invalid("runtime process identity is incomplete"));
    }
    birth(&value.birth)?;
    executable(&value.executable)
}

fn child_shape(
    value: &ChildIdentity,
    process: &ProcessIdentity,
    pid: libc::pid_t,
) -> io::Result<()> {
    if u32::try_from(pid).ok() != Some(value.pid)
        || value.pid <= 1
        || value.pid == process.pid
        || value.parent != process.pid
        || value.pgid != process.pgid
        || value.session != process.session
    {
        return Err(invalid(
            "runtime child does not belong to the same process group and supervisor",
        ));
    }
    birth(&value.birth)?;
    executable(&value.executable)
}

fn signal_shape(value: &SignalSnapshot) -> io::Result<()> {
    if !(1..=8).contains(&sys::HANDLED.len()) {
        return Err(invalid("runtime signal layout exceeds its mask width"));
    }
    let bits = u32::try_from(sys::HANDLED.len())
        .ok()
        .and_then(|width| 1_u16.checked_shl(width))
        .and_then(|bit| bit.checked_sub(1))
        .ok_or_else(|| invalid("runtime signal layout exceeds its mask width"))?;
    let chld = sys::MANAGED
        .iter()
        .find_map(|&(signal, bit)| (signal == libc::SIGCHLD).then_some(bit))
        .ok_or_else(|| invalid("runtime signal layout has no SIGCHLD"))?;
    if value.read_fd < 3
        || value.write_fd < 3
        || value.read_fd == value.write_fd
        || u16::from(value.prior_mask) & !bits != 0
        || u16::from(value.ignored) & !bits != 0
        || value.ignored & chld != 0
    {
        return Err(invalid(
            "runtime signal snapshot has invalid roles or mask bits",
        ));
    }
    Ok(())
}

fn termios_shape(value: &TermiosSnapshot) -> io::Result<()> {
    text(&value.abi, MAX_ABI)?;
    if value.abi.is_empty() || value.controls.len() > MAX_CONTROLS {
        return Err(invalid(
            "runtime terminal snapshot has an invalid ABI or control count",
        ));
    }
    Ok(())
}

fn same_cwd() -> io::Result<DescriptorIdentity> {
    let meta = std::fs::metadata(".")?;
    if !meta.is_dir() {
        return Err(invalid("runtime current directory is not a directory"));
    }
    Ok(DescriptorIdentity {
        device: meta.dev(),
        inode: meta.ino(),
        rdev: meta.rdev(),
    })
}

fn same_file(path: &Path, expected: &Executable) -> io::Result<()> {
    let meta = std::fs::metadata(path)?;
    if !meta.is_file()
        || meta.dev() != expected.device
        || meta.ino() != expected.inode
        || std::fs::canonicalize(path)? != expected.path
    {
        return Err(invalid("runtime captured executable file identity changed"));
    }
    Ok(())
}

fn validate_saved(saved: &Saved) -> io::Result<()> {
    setup_shape(&saved.setup)?;
    process_shape(&saved.process)?;
    path(&saved.config)?;
    if let Some(cwd) = &saved.cwd {
        path(cwd)?;
    }
    if let Some(journal) = &saved.journal_path {
        path(journal)?;
    }
    if saved.journal_pid != saved.process.pid
        && !(saved.journal_path.is_none() && saved.journal_pid == 0)
    {
        return Err(invalid(
            "runtime journal writer differs from this supervisor",
        ));
    }
    signal_shape(&saved.signals)?;
    termios_shape(&saved.termios)?;
    child_shape(&saved.child_identity, &saved.process, saved.child_pid)?;
    build_shape(&saved.child_build, &saved.setup.store)?;
    argv(&saved.child_args)?;
    if saved
        .facts
        .pid()
        .is_some_and(|pid| i64::from(pid) != i64::from(saved.child_pid))
    {
        return Err(invalid("the model reader follows another child"));
    }
    if saved.stopped || saved.ending != 0 {
        return Err(invalid(
            "only a live unstopped child with no ending can enter transfer",
        ));
    }
    if Tracker::restore(
        saved.tracker.clone(),
        saved.spawned_ms,
        saved.setup.opts.poll_ms,
        saved.setup.opts.quiet_ms,
    )
    .is_none()
    {
        return Err(invalid(
            "runtime tracker history does not match the original child and settings",
        ));
    }
    text(&saved.noted, MAX_TEXT)?;
    text(&saved.background_found, MAX_TEXT)
}

fn capture_budget(supervisor: &Supervisor) -> io::Result<()> {
    let mut size = 0_usize;
    let mut add = |bytes: usize| {
        size = size
            .checked_add(bytes)
            .and_then(|n| n.checked_add(4))
            .ok_or_else(|| invalid("runtime capture size overflows"))?;
        if size > MAX_PAYLOAD_BYTES {
            return Err(invalid("runtime capture exceeds payload bound"));
        }
        Ok(())
    };
    for path in [
        &supervisor.setup.home,
        &supervisor.setup.store.program_dir,
        &supervisor.config,
    ] {
        add(path.as_os_str().len())?;
    }
    for path in [
        supervisor.setup.own_exe.as_deref(),
        supervisor.cwd.as_deref(),
        supervisor.journal.path(),
    ]
    .into_iter()
    .flatten()
    {
        add(path.as_os_str().len())?;
    }
    add(supervisor.setup.store.command.len())?;
    for build in [&supervisor.setup.build, &supervisor.child.build] {
        add(build.dir.as_os_str().len())?;
        add(build.exe.as_os_str().len())?;
        if let Some(version) = &build.version {
            add(version.len())?;
        }
        for (name, value) in &build.env {
            add(name.len())?;
            add(value.len())?;
        }
    }
    for arg in supervisor.setup.args.iter().chain(&supervisor.child.args) {
        add(arg.len())?;
    }
    add(supervisor.noted.len())?;
    add(supervisor.background_found.len())?;
    add(model_facts::Reader::TRANSFER_BOUND)?;
    Ok(())
}

/// Capture only a currently live, unstopped owned child. This reads identity,
/// cwd metadata and the original saved termios snapshot, without touching any FD ownership.
pub(super) fn encode(supervisor: &Supervisor, signals: &SignalSnapshot) -> io::Result<Vec<u8>> {
    if supervisor.ending != Ending::None
        || supervisor.restart_deferred.is_some()
        || supervisor.moved.is_some()
        || supervisor.child.stopped
    {
        return Err(invalid("supervisor is ending or stopped"));
    }
    setup_shape(&supervisor.setup)?;
    build_shape(&supervisor.child.build, &supervisor.setup.store)?;
    argv(&supervisor.child.args)?;
    signal_shape(signals)?;
    capture_budget(supervisor)?;
    let process = procs::capture_process(std::process::id())
        .ok_or_else(|| invalid("cannot capture the continuing supervisor identity"))?;
    let child_identity = supervisor
        .child
        .identity
        .as_ref()
        .ok_or_else(|| invalid("child has no original kernel identity"))?;
    child_shape(child_identity, &process, supervisor.child.pid)?;
    if procs::observe_child(child_identity).is_none_or(|seen| seen.state != ChildState::Live) {
        return Err(invalid("child is not live at capture"));
    }
    same_file(
        supervisor
            .setup
            .own_exe
            .as_deref()
            .ok_or_else(|| invalid("no captured own image"))?,
        &process.executable,
    )?;
    same_file(&supervisor.child.build.exe, &child_identity.executable)?;
    let (journal_path, journal_pid) = supervisor.journal.captured();
    let saved = Saved {
        setup: supervisor.setup.clone(),
        process,
        cwd_identity: same_cwd()?,
        signals: *signals,
        termios: supervisor
            .termios
            .as_ref()
            .ok_or_else(|| invalid("original terminal modes are unavailable"))?
            .snapshot()?,
        journal_path,
        journal_pid,
        config: supervisor.config.clone(),
        cwd: supervisor.cwd.clone(),
        child_pid: supervisor.child.pid,
        child_build: supervisor.child.build.clone(),
        child_args: supervisor.child.args.clone(),
        child_identity: child_identity.clone(),
        tracker: supervisor.child.tracker.snapshot(),
        spawned_ms: supervisor.child.spawned_ms,
        stopped: supervisor.child.stopped,
        ending: 0,
        noted: supervisor.noted.clone(),
        seen_target: supervisor.seen_target,
        abandoned: supervisor.abandoned.clone(),
        background_hold_until: supervisor.background_hold_until,
        background_found: supervisor.background_found.clone(),
        // A reader still on an earlier child (no look since a relaunch) would
        // start from nothing at its next look: it crosses as that.
        facts: if supervisor
            .facts
            .pid()
            .is_none_or(|pid| i64::from(pid) == i64::from(supervisor.child.pid))
        {
            supervisor.facts.clone()
        } else {
            model_facts::Reader::default()
        },
    };
    encode_saved(&saved)
}

fn put_option_u64(encoder: &mut Encoder, value: Option<u64>) -> io::Result<()> {
    encoder.boolean(value.is_some())?;
    if let Some(value) = value {
        encoder.u64(value)?;
    }
    Ok(())
}
fn get_option_u64(decoder: &mut Decoder<'_>) -> io::Result<Option<u64>> {
    if decoder.boolean()? {
        Ok(Some(decoder.u64()?))
    } else {
        Ok(None)
    }
}
/// The whole abandoned set: a count of at most [`MAX_ABANDONED`], then the
/// build numbers ascending.
fn put_abandoned(encoder: &mut Encoder, value: &Abandoned) -> io::Result<()> {
    let builds = value.builds();
    encoder.count(builds.len(), MAX_ABANDONED)?;
    for build in builds {
        encoder.u64(build)?;
    }
    Ok(())
}
/// Only the canonical spelling restores: a bounded count and strictly
/// ascending build numbers, so a repeated or reordered build is refused.
fn get_abandoned(decoder: &mut Decoder<'_>) -> io::Result<Abandoned> {
    let mut builds = Vec::new();
    for _ in 0..decoder.count(MAX_ABANDONED)? {
        builds.push(decoder.u64()?);
    }
    Abandoned::from_ascending(&builds)
        .ok_or_else(|| invalid("abandoned builds are not a bounded ascending set"))
}
fn put_path(encoder: &mut Encoder, value: &Path) -> io::Result<()> {
    encoder.os_string(value.as_os_str())
}
fn get_path(decoder: &mut Decoder<'_>) -> io::Result<PathBuf> {
    Ok(PathBuf::from(decoder.os_string(MAX_PATH)?))
}
fn put_option_path(encoder: &mut Encoder, value: Option<&Path>) -> io::Result<()> {
    encoder.boolean(value.is_some())?;
    if let Some(value) = value {
        put_path(encoder, value)?;
    }
    Ok(())
}
fn get_option_path(decoder: &mut Decoder<'_>) -> io::Result<Option<PathBuf>> {
    if decoder.boolean()? {
        Ok(Some(get_path(decoder)?))
    } else {
        Ok(None)
    }
}
fn put_text(encoder: &mut Encoder, value: &str) -> io::Result<()> {
    encoder.bytes(value.as_bytes())
}
fn get_text(decoder: &mut Decoder<'_>, maximum: usize) -> io::Result<String> {
    String::from_utf8(decoder.bytes(maximum)?.to_vec())
        .map_err(|_| invalid("runtime text is not UTF-8"))
}
fn put_argv(encoder: &mut Encoder, value: &[OsString]) -> io::Result<()> {
    encoder.count(value.len(), MAX_ARGS)?;
    for arg in value {
        encoder.os_string(arg)?;
    }
    Ok(())
}
fn get_argv(decoder: &mut Decoder<'_>) -> io::Result<Vec<OsString>> {
    let mut args = Vec::new();
    for _ in 0..decoder.count(MAX_ARGS)? {
        args.push(decoder.os_string(MAX_ARG)?);
    }
    Ok(args)
}
fn put_birth(encoder: &mut Encoder, value: &Birth) -> io::Result<()> {
    match value {
        Birth::MacOs {
            seconds,
            microseconds,
        } => {
            encoder.u8(1)?;
            encoder.u64(*seconds)?;
            encoder.u32(*microseconds)
        }
        Birth::Linux(ticks) => {
            encoder.u8(2)?;
            encoder.u64(*ticks)
        }
    }
}
fn get_birth(decoder: &mut Decoder<'_>) -> io::Result<Birth> {
    match decoder.u8()? {
        1 => Ok(Birth::MacOs {
            seconds: decoder.u64()?,
            microseconds: decoder.u32()?,
        }),
        2 => Ok(Birth::Linux(decoder.u64()?)),
        _ => Err(invalid("runtime birth kind is unsupported")),
    }
}
pub(super) fn put_executable(encoder: &mut Encoder, value: &Executable) -> io::Result<()> {
    put_path(encoder, &value.path)?;
    encoder.u64(value.device)?;
    encoder.u64(value.inode)
}
pub(super) fn get_executable(decoder: &mut Decoder<'_>) -> io::Result<Executable> {
    Ok(Executable {
        path: get_path(decoder)?,
        device: decoder.u64()?,
        inode: decoder.u64()?,
    })
}
fn put_identity(encoder: &mut Encoder, value: DescriptorIdentity) -> io::Result<()> {
    encoder.u64(value.device)?;
    encoder.u64(value.inode)?;
    encoder.u64(value.rdev)
}
fn get_identity(decoder: &mut Decoder<'_>) -> io::Result<DescriptorIdentity> {
    Ok(DescriptorIdentity {
        device: decoder.u64()?,
        inode: decoder.u64()?,
        rdev: decoder.u64()?,
    })
}
pub(super) fn put_process(encoder: &mut Encoder, value: &ProcessIdentity) -> io::Result<()> {
    for number in [value.pid, value.parent, value.pgid, value.session] {
        encoder.u32(number)?;
    }
    put_birth(encoder, &value.birth)?;
    put_executable(encoder, &value.executable)
}
pub(super) fn get_process(decoder: &mut Decoder<'_>) -> io::Result<ProcessIdentity> {
    Ok(ProcessIdentity {
        pid: decoder.u32()?,
        parent: decoder.u32()?,
        pgid: decoder.u32()?,
        session: decoder.u32()?,
        birth: get_birth(decoder)?,
        executable: get_executable(decoder)?,
    })
}
pub(super) fn put_child(encoder: &mut Encoder, value: &ChildIdentity) -> io::Result<()> {
    for number in [value.pid, value.parent, value.pgid, value.session] {
        encoder.u32(number)?;
    }
    put_birth(encoder, &value.birth)?;
    put_executable(encoder, &value.executable)
}
pub(super) fn get_child(decoder: &mut Decoder<'_>) -> io::Result<ChildIdentity> {
    Ok(ChildIdentity {
        pid: decoder.u32()?,
        parent: decoder.u32()?,
        pgid: decoder.u32()?,
        session: decoder.u32()?,
        birth: get_birth(decoder)?,
        executable: get_executable(decoder)?,
    })
}
fn put_build(encoder: &mut Encoder, value: &Build) -> io::Result<()> {
    encoder.u64(value.number)?;
    put_path(encoder, &value.dir)?;
    put_path(encoder, &value.exe)?;
    encoder.boolean(value.version.is_some())?;
    if let Some(version) = &value.version {
        put_text(encoder, version)?;
    }
    encoder.count(value.env.len(), MAX_ENV)?;
    for (name, value) in &value.env {
        put_text(encoder, name)?;
        put_text(encoder, value)?;
    }
    Ok(())
}
fn get_build(decoder: &mut Decoder<'_>) -> io::Result<Build> {
    let number = decoder.u64()?;
    let dir = get_path(decoder)?;
    let exe = get_path(decoder)?;
    let version = if decoder.boolean()? {
        Some(get_text(decoder, MAX_ENV_ENTRY)?)
    } else {
        None
    };
    let mut env = Vec::new();
    for _ in 0..decoder.count(MAX_ENV)? {
        env.push((
            get_text(decoder, MAX_ENV_ENTRY)?,
            get_text(decoder, MAX_ENV_ENTRY)?,
        ));
    }
    Ok(Build {
        number,
        dir,
        exe,
        version,
        env,
    })
}
fn put_setup(encoder: &mut Encoder, value: &Setup) -> io::Result<()> {
    put_path(encoder, &value.home)?;
    put_path(encoder, &value.store.program_dir)?;
    put_text(encoder, &value.store.command)?;
    put_option_path(encoder, value.own_exe.as_deref())?;
    put_build(encoder, &value.build)?;
    encoder.u64(value.opts.quiet_ms)?;
    encoder.u64(value.opts.poll_ms)?;
    encoder.u64(value.opts.term_grace_ms)?;
    put_argv(encoder, &value.args)?;
    encoder.u32(value.depth)
}
fn get_setup(decoder: &mut Decoder<'_>) -> io::Result<Setup> {
    Ok(Setup {
        home: get_path(decoder)?,
        store: Store {
            program_dir: get_path(decoder)?,
            command: get_text(decoder, MAX_ENV_ENTRY)?,
        },
        own_exe: get_option_path(decoder)?,
        build: get_build(decoder)?,
        opts: Options {
            quiet_ms: decoder.u64()?,
            poll_ms: decoder.u64()?,
            term_grace_ms: decoder.u64()?,
        },
        args: get_argv(decoder)?,
        depth: decoder.u32()?,
    })
}
fn put_signals(encoder: &mut Encoder, value: &SignalSnapshot) -> io::Result<()> {
    encoder.u32(value.read_fd as u32)?;
    encoder.u32(value.write_fd as u32)?;
    put_identity(encoder, value.read_identity)?;
    put_identity(encoder, value.write_identity)?;
    encoder.u8(value.prior_mask)?;
    encoder.u8(value.ignored)
}
fn get_signals(decoder: &mut Decoder<'_>) -> io::Result<SignalSnapshot> {
    Ok(SignalSnapshot {
        read_fd: i32::try_from(decoder.u32()?)
            .map_err(|_| invalid("runtime signal fd does not fit"))?,
        write_fd: i32::try_from(decoder.u32()?)
            .map_err(|_| invalid("runtime signal fd does not fit"))?,
        read_identity: get_identity(decoder)?,
        write_identity: get_identity(decoder)?,
        prior_mask: decoder.u8()?,
        ignored: decoder.u8()?,
    })
}
fn put_termios(encoder: &mut Encoder, value: &TermiosSnapshot) -> io::Result<()> {
    put_text(encoder, &value.abi)?;
    put_identity(encoder, value.terminal)?;
    for flag in value.flags {
        encoder.u64(flag)?;
    }
    encoder.bytes(&value.controls)?;
    encoder.boolean(value.line.is_some())?;
    if let Some(line) = value.line {
        encoder.u8(line)?;
    }
    encoder.boolean(value.raw_speeds.is_some())?;
    if let Some(speeds) = value.raw_speeds {
        encoder.u64(speeds[0])?;
        encoder.u64(speeds[1])?;
    }
    Ok(())
}
fn get_termios(decoder: &mut Decoder<'_>) -> io::Result<TermiosSnapshot> {
    Ok(TermiosSnapshot {
        abi: get_text(decoder, MAX_ABI)?,
        terminal: get_identity(decoder)?,
        flags: [
            decoder.u64()?,
            decoder.u64()?,
            decoder.u64()?,
            decoder.u64()?,
        ],
        controls: decoder.bytes(MAX_CONTROLS)?.to_vec(),
        line: if decoder.boolean()? {
            Some(decoder.u8()?)
        } else {
            None
        },
        raw_speeds: if decoder.boolean()? {
            Some([decoder.u64()?, decoder.u64()?])
        } else {
            None
        },
    })
}
fn put_tracker(encoder: &mut Encoder, value: &Snapshot) -> io::Result<()> {
    encoder.u64(value.spawned_ms)?;
    encoder.u64(value.poll_ms)?;
    encoder.u64(value.quiet_ms)?;
    put_option_u64(encoder, value.last_look_ms)?;
    encoder.boolean(value.run.is_some())?;
    if let Some(run) = value.run {
        encoder.u64(run.status_at)?;
        put_option_u64(encoder, run.input_at)?;
        put_option_u64(encoder, run.clean_before)?;
    }
    put_option_u64(encoder, value.clean_at)
}
fn get_tracker(decoder: &mut Decoder<'_>) -> io::Result<Snapshot> {
    Ok(Snapshot {
        spawned_ms: decoder.u64()?,
        poll_ms: decoder.u64()?,
        quiet_ms: decoder.u64()?,
        last_look_ms: get_option_u64(decoder)?,
        run: if decoder.boolean()? {
            Some(RunSnapshot {
                status_at: decoder.u64()?,
                input_at: get_option_u64(decoder)?,
                clean_before: get_option_u64(decoder)?,
            })
        } else {
            None
        },
        clean_at: get_option_u64(decoder)?,
    })
}

fn encode_saved(saved: &Saved) -> io::Result<Vec<u8>> {
    validate_saved(saved)?;
    encode_fields(saved)
}

fn encode_fields(saved: &Saved) -> io::Result<Vec<u8>> {
    let mut encoder = Encoder::new();
    encoder.u32(SCHEMA)?;
    put_setup(&mut encoder, &saved.setup)?;
    put_process(&mut encoder, &saved.process)?;
    put_identity(&mut encoder, saved.cwd_identity)?;
    put_signals(&mut encoder, &saved.signals)?;
    put_termios(&mut encoder, &saved.termios)?;
    put_option_path(&mut encoder, saved.journal_path.as_deref())?;
    encoder.u32(saved.journal_pid)?;
    put_path(&mut encoder, &saved.config)?;
    put_option_path(&mut encoder, saved.cwd.as_deref())?;
    encoder.u32(saved.child_pid as u32)?;
    put_build(&mut encoder, &saved.child_build)?;
    put_argv(&mut encoder, &saved.child_args)?;
    put_child(&mut encoder, &saved.child_identity)?;
    put_tracker(&mut encoder, &saved.tracker)?;
    encoder.u64(saved.spawned_ms)?;
    encoder.boolean(saved.stopped)?;
    encoder.u8(saved.ending)?;
    put_text(&mut encoder, &saved.noted)?;
    put_option_u64(&mut encoder, saved.seen_target)?;
    put_abandoned(&mut encoder, &saved.abandoned)?;
    encoder.u64(saved.background_hold_until)?;
    put_text(&mut encoder, &saved.background_found)?;
    saved.facts.put(&mut encoder)?;
    Ok(encoder.finish())
}

/// Decode all state with bounded fields and strict trailing-data rejection, without
/// touching descriptor ownership or authorizing runtime actions.
pub(super) fn decode(payload: &[u8]) -> io::Result<Saved> {
    let mut decoder = Decoder::new(payload)?;
    if decoder.u32()? != SCHEMA {
        return Err(invalid("unsupported supervisor runtime payload schema"));
    }
    let saved = Saved {
        setup: get_setup(&mut decoder)?,
        process: get_process(&mut decoder)?,
        cwd_identity: get_identity(&mut decoder)?,
        signals: get_signals(&mut decoder)?,
        termios: get_termios(&mut decoder)?,
        journal_path: get_option_path(&mut decoder)?,
        journal_pid: decoder.u32()?,
        config: get_path(&mut decoder)?,
        cwd: get_option_path(&mut decoder)?,
        child_pid: i32::try_from(decoder.u32()?)
            .map_err(|_| invalid("runtime child PID does not fit"))?,
        child_build: get_build(&mut decoder)?,
        child_args: get_argv(&mut decoder)?,
        child_identity: get_child(&mut decoder)?,
        tracker: get_tracker(&mut decoder)?,
        spawned_ms: decoder.u64()?,
        stopped: decoder.boolean()?,
        ending: decoder.u8()?,
        noted: get_text(&mut decoder, MAX_TEXT)?,
        seen_target: get_option_u64(&mut decoder)?,
        abandoned: get_abandoned(&mut decoder)?,
        background_hold_until: decoder.u64()?,
        background_found: get_text(&mut decoder, MAX_TEXT)?,
        facts: model_facts::Reader::get(&mut decoder)?,
    };
    decoder.finish()?;
    validate_saved(&saved)?;
    Ok(saved)
}

impl Saved {
    pub(super) fn home(&self) -> &Path {
        &self.setup.home
    }
    pub(super) fn signals(&self) -> &SignalSnapshot {
        &self.signals
    }
    pub(super) fn process(&self) -> &ProcessIdentity {
        &self.process
    }
    pub(super) fn child(&self) -> &ChildIdentity {
        &self.child_identity
    }
    /// The terminal modes the original harness saved before its first child started.
    pub(super) fn termios(&self) -> &TermiosSnapshot {
        &self.termios
    }

    fn validate_context(
        &self,
        header: &Header,
        current: &ProcessIdentity,
        observation: ChildObservation,
        cwd: DescriptorIdentity,
    ) -> io::Result<ChildState> {
        validate_saved(self)?;
        let image = match header.phase {
            Phase::Forward => &header.new_image,
            Phase::Rollback => &header.old_image,
        };
        if header.self_pid != self.process.pid
            || header.self_birth != self.process.birth
            || header.old_image.executable != self.process.executable
            || current.pid != self.process.pid
            || current.birth != self.process.birth
            || current.pgid != self.process.pgid
            || current.session != self.process.session
            || current.executable != image.executable
            || image.version != cli::VERSION
            || cwd != self.cwd_identity
        {
            return Err(invalid(
                "runtime context differs from the continuing process, image or cwd object",
            ));
        }
        if self.signals.read_fd != header.fds.signal_read
            || self.signals.write_fd != header.fds.signal_write
        {
            return Err(invalid(
                "runtime signal roles differ from the capsule header",
            ));
        }
        // The parent is diagnostic: the original shell may have exited while signals were blocked.
        if observation
            .pgid
            .is_some_and(|pgid| pgid != self.child_identity.pgid)
            || observation
                .session
                .is_some_and(|session| session != self.child_identity.session)
            || (observation.state != ChildState::Unreaped
                && (observation.pgid.is_none() || observation.session.is_none()))
        {
            return Err(invalid(
                "runtime live child group or session evidence is incomplete",
            ));
        }
        Ok(observation.state)
    }

    /// Rebuild the supervisor directly, never through launch or initialization paths.
    /// The returned child state distinguishes ordinary pending terminal dispatch from live adoption.
    pub(super) fn restore(self, header: &Header) -> io::Result<Restored> {
        let current = procs::capture_process(std::process::id())
            .ok_or_else(|| invalid("cannot observe the continuing supervisor"))?;
        let observed = procs::observe_child(&self.child_identity)
            .ok_or_else(|| invalid("the original owned child cannot be proven"))?;
        let child_state = self.validate_context(header, &current, observed, same_cwd()?)?;
        same_file(
            self.setup
                .own_exe
                .as_deref()
                .ok_or_else(|| invalid("no original own image"))?,
            &self.process.executable,
        )?;
        if child_state != ChildState::Unreaped {
            same_file(&self.child_build.exe, &self.child_identity.executable)?;
        }
        let termios = Termios::from_snapshot(0, &self.termios)?;
        let tracker = Tracker::restore(
            self.tracker,
            self.spawned_ms,
            self.setup.opts.poll_ms,
            self.setup.opts.quiet_ms,
        )
        .ok_or_else(|| invalid("runtime tracker restoration refused"))?;
        let journal = Journal::from_captured(self.journal_path, self.journal_pid)?;
        let mut setup = self.setup;
        setup.own_exe = Some(current.executable.path.clone());
        Ok(Restored {
            child_state,
            supervisor: Supervisor {
                image: super::image::Runtime::default(),
                setup,
                journal,
                config: self.config,
                cwd: self.cwd,
                termios: Some(termios),
                child: Child {
                    pid: self.child_pid,
                    build: self.child_build,
                    args: self.child_args,
                    identity: Some(self.child_identity),
                    identity_pending: false,
                    tracker,
                    spawned_ms: self.spawned_ms,
                    stopped: child_state == ChildState::Stopped,
                },
                ending: Ending::None,
                moved: None,
                noted: self.noted,
                seen_target: self.seen_target,
                abandoned: self.abandoned,
                background_hold_until: self.background_hold_until,
                background_found: self.background_found,
                // An own-image move is admitted only outside aterm, where no
                // link exists; nothing of a link is carried in the capsule.
                link: super::link::Link::absent(),
                // Nor is anything of a crash or a restart: a move is made only
                // with neither pending, and the back-off starts over.
                ladder: crate::relaunch::Ladder::new(),
                restart_deferred: None,
                // The model reader goes on where the old image's stopped.
                facts: self.facts,
            },
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transfer_capsule::{FdRoles, Image};
    use std::os::unix::ffi::OsStringExt;

    fn fixture() -> Saved {
        let store = Store::new(Path::new("/owned/pkg"), profile::PACKAGE, profile::COMMAND);
        let build = |number| Build {
            number,
            dir: store.program_dir.join(number.to_string()),
            exe: store
                .program_dir
                .join(number.to_string())
                .join("bin/claude"),
            version: None,
            env: vec![("FOO".into(), "bar".into())],
        };
        let opts = Options::default();
        let initial = build(1);
        let current = build(2);
        let process = ProcessIdentity {
            pid: 4242,
            parent: 100,
            pgid: 4242,
            session: 99,
            birth: Birth::Linux(100),
            executable: Executable {
                path: "/owned/harness-old".into(),
                device: 9,
                inode: 90,
            },
        };
        let child_identity = ChildIdentity {
            pid: 4243,
            parent: 4242,
            pgid: 4242,
            session: 99,
            birth: Birth::Linux(101),
            executable: Executable {
                path: current.exe.clone(),
                device: 8,
                inode: 80,
            },
        };
        let tracker = Snapshot {
            spawned_ms: 1000,
            poll_ms: opts.poll_ms,
            quiet_ms: opts.quiet_ms,
            last_look_ms: Some(1200),
            run: Some(RunSnapshot {
                status_at: 1150,
                input_at: Some(900),
                clean_before: Some(900),
            }),
            clean_at: None,
        };
        Saved {
            setup: Setup {
                home: "/owned/home".into(),
                store,
                own_exe: Some(process.executable.path.clone()),
                build: initial,
                opts,
                args: vec!["--model".into(), OsString::from_vec(b"model-\xff".to_vec())],
                depth: 2,
            },
            process,
            cwd_identity: DescriptorIdentity {
                device: 1,
                inode: 2,
                rdev: 0,
            },
            signals: SignalSnapshot {
                read_fd: 11,
                write_fd: 12,
                read_identity: DescriptorIdentity {
                    device: 3,
                    inode: 4,
                    rdev: 0,
                },
                write_identity: DescriptorIdentity {
                    device: 3,
                    inode: 4,
                    rdev: 0,
                },
                prior_mask: 0,
                ignored: 0,
            },
            termios: TermiosSnapshot {
                abi: "controlled-terminal-layout".into(),
                terminal: DescriptorIdentity {
                    device: 5,
                    inode: 6,
                    rdev: 7,
                },
                flags: [1, 2, 3, 4],
                controls: vec![0, 1, 2],
                line: Some(7),
                raw_speeds: Some([9600, 19200]),
            },
            journal_path: Some("/prior/state/claude-code-alab/journal.log".into()),
            journal_pid: 4242,
            config: "/captured/config".into(),
            cwd: Some("/original/project-name".into()),
            child_pid: 4243,
            child_build: current,
            child_args: vec!["--resume".into(), "conversation".into()],
            child_identity,
            tracker,
            spawned_ms: 1000,
            stopped: false,
            ending: 0,
            noted: "wait abandoned build=77".into(),
            seen_target: Some(78),
            abandoned: Abandoned::from_ascending(&[55, 77]).unwrap(),
            background_hold_until: 5000,
            background_found: "background-work shell".into(),
            facts: model_facts::Reader::default(),
        }
    }

    /// A model reader that has read child `pid`'s transcript once: a
    /// conversation, its floor, a carried read and stated facts.
    fn reader_of(pid: u32) -> model_facts::Reader {
        const SID: &str = "7c1e2a90-4b3d-4e5f-8a9b-0c1d2e3f4a5b";
        let root = std::env::temp_dir().join(".ubd").join(format!(
            "transfer-facts-{}-{pid}.noindex",
            std::process::id()
        ));
        let project = root.join("projects").join("-work");
        std::fs::create_dir_all(&project).unwrap();
        std::fs::write(
            project.join(format!("{SID}.jsonl")),
            "{\"type\":\"assistant\",\"timestamp\":\"2026-11-13T10:00:10.000Z\",\"effort\":\"high\",\"message\":{\"model\":\"claude-opus-5-5\"}}\n",
        )
        .unwrap();
        let mut reader = model_facts::Reader::default();
        let process = model_facts::Process {
            pid,
            record: Some((SID, 1_794_563_200_000)),
            args: &[],
        };
        let line = reader
            .look(&root, &process, 1_794_563_220_000)
            .journal_line
            .map(str::to_owned);
        std::fs::remove_dir_all(&root).unwrap();
        assert_eq!(
            line.as_deref(),
            Some("facts model=Opus_5.5 effort=high key=-")
        );
        reader
    }

    fn header(saved: &Saved) -> Header {
        Header {
            phase: Phase::Forward,
            forward_attempt: 1,
            rollback_attempt: 0,
            self_pid: saved.process.pid,
            self_birth: saved.process.birth.clone(),
            old_image: Image {
                executable: saved.process.executable.clone(),
                version: cli::VERSION.into(),
            },
            new_image: Image {
                executable: Executable {
                    path: "/owned/harness-new".into(),
                    device: 10,
                    inode: 100,
                },
                version: cli::VERSION.into(),
            },
            fds: FdRoles {
                capsule: 10,
                signal_read: 11,
                signal_write: 12,
                authorities: vec![13],
                retention_gates: vec![14, 15],
            },
        }
    }

    #[test]
    fn runtime_roundtrip_keeps_prior_journal_argv_tracker_and_abandoned_targets() {
        let original = fixture();
        let payload = encode_saved(&original).unwrap();
        let saved = decode(&payload).unwrap();
        assert_eq!(encode_saved(&saved).unwrap(), payload);
        assert_eq!(saved.setup.args, original.setup.args);
        assert_eq!(saved.setup.own_exe, original.setup.own_exe);
        assert_eq!(
            saved.setup.store.program_dir,
            original.setup.store.program_dir
        );
        assert_eq!(saved.setup.build, original.setup.build);
        assert_eq!(saved.child_build, original.child_build);
        assert_eq!(saved.child_args, original.child_args);
        assert_eq!(saved.child_identity, original.child_identity);
        assert_eq!(saved.tracker, original.tracker);
        assert_eq!(
            (saved.journal_path, saved.journal_pid),
            (original.journal_path, original.journal_pid)
        );
        assert_eq!(
            (saved.noted, saved.seen_target, saved.abandoned),
            (original.noted, original.seen_target, original.abandoned)
        );
        assert_eq!(
            (saved.background_hold_until, saved.background_found),
            (original.background_hold_until, original.background_found)
        );
        assert_eq!(saved.termios, original.termios);
        assert_eq!(saved.signals, original.signals);
    }

    /// The model reader crosses whole (the new image goes on where the old
    /// one stopped, never reading the child's transcript afresh from its
    /// start), and only one that follows this child restores. CONTROL: the
    /// fixture's unread reader crosses as itself.
    #[test]
    fn the_model_reader_crosses_and_must_follow_the_child() {
        let mut saved = fixture();
        let child = u32::try_from(saved.child_pid).unwrap();
        let unread = decode(&encode_saved(&saved).unwrap()).unwrap();
        assert_eq!(
            (unread.facts.pid(), unread.facts.facts()),
            (None, &model_facts::ModelFacts::default())
        );
        saved.facts = reader_of(child);
        let payload = encode_saved(&saved).unwrap();
        let restored = decode(&payload).unwrap();
        assert_eq!(encode_saved(&restored).unwrap(), payload);
        assert_eq!(restored.facts.pid(), Some(child));
        assert_eq!(restored.facts.facts(), saved.facts.facts());
        assert_eq!(
            restored.facts.facts().journal_line(),
            "facts model=Opus_5.5 effort=high key=-"
        );
        saved.facts = reader_of(child + 1);
        assert!(
            encode_saved(&saved).is_err(),
            "a reader of another child is refused"
        );
    }

    #[test]
    fn the_whole_abandoned_set_crosses_and_only_its_canonical_spelling_restores() {
        let bound = u64::try_from(MAX_ABANDONED).unwrap();
        let mut saved = fixture();
        assert_eq!(saved.abandoned.builds().collect::<Vec<_>>(), [55, 77]);
        saved.abandoned = Abandoned::from_ascending(&(1..=bound).collect::<Vec<_>>()).unwrap();
        let restored = decode(&encode_saved(&saved).unwrap()).unwrap();
        assert_eq!(restored.abandoned, saved.abandoned);
        assert_eq!(restored.abandoned.builds().len(), MAX_ABANDONED);

        let list = |builds: &[u64], count: u32| {
            let mut bytes = count.to_le_bytes().to_vec();
            for build in builds {
                bytes.extend_from_slice(&build.to_le_bytes());
            }
            bytes
        };
        let read = |bytes: &[u8]| {
            let mut decoder = Decoder::new(bytes).unwrap();
            get_abandoned(&mut decoder).and_then(|set| decoder.finish().map(|()| set))
        };
        assert_eq!(read(&list(&[], 0)).unwrap(), Abandoned::default());
        assert_eq!(
            read(&list(&[55, 77], 2))
                .unwrap()
                .builds()
                .collect::<Vec<_>>(),
            [55, 77]
        );
        // Repeated, reordered, truncated, and counts past the bound all refuse.
        assert!(read(&list(&[77, 77], 2)).is_err());
        assert!(read(&list(&[77, 55], 2)).is_err());
        assert!(read(&list(&[55], 2)).is_err());
        let over: Vec<u64> = (1..=bound + 1).collect();
        assert!(read(&list(&over, u32::try_from(over.len()).unwrap())).is_err());
        assert!(read(&list(&[], u32::MAX)).is_err());
    }

    #[test]
    fn malformed_runtime_fields_counts_and_trailing_data_cannot_be_restored() {
        let baseline = fixture();
        let payload = encode_saved(&baseline).unwrap();
        for length in [0, 1, payload.len() - 1] {
            assert!(decode(&payload[..length]).is_err());
        }
        let mut wrong = payload.clone();
        wrong[..4].copy_from_slice(&(SCHEMA + 1).to_le_bytes());
        assert!(decode(&wrong).is_err());
        let mut wrong = payload;
        wrong.push(0);
        assert!(decode(&wrong).is_err());
        assert!(get_argv(&mut Decoder::new(&u32::MAX.to_le_bytes()).unwrap()).is_err());
        let mut cases = Vec::new();
        let mut wrong = baseline.clone();
        wrong.setup.store.command = "sh".into();
        cases.push(wrong);
        let mut wrong = baseline.clone();
        wrong.setup.opts.poll_ms = 0;
        cases.push(wrong);
        let mut wrong = baseline.clone();
        wrong
            .setup
            .args
            .push(OsString::from_vec(b"bad\0arg".to_vec()));
        cases.push(wrong);
        let mut wrong = baseline.clone();
        wrong.child_build.env = vec![("LD_PRELOAD".into(), "bad".into())];
        cases.push(wrong);
        let mut wrong = baseline.clone();
        wrong.child_identity.parent += 1;
        cases.push(wrong);
        let mut wrong = baseline.clone();
        wrong.tracker.spawned_ms += 1;
        cases.push(wrong);
        let mut wrong = baseline.clone();
        wrong.journal_pid += 1;
        cases.push(wrong);
        let mut wrong = baseline.clone();
        wrong.config = "relative".into();
        cases.push(wrong);
        let mut wrong = baseline.clone();
        wrong.stopped = true;
        cases.push(wrong);
        let mut wrong = baseline.clone();
        wrong.ending = 1;
        cases.push(wrong);
        let mut wrong = baseline.clone();
        wrong.signals.write_fd = wrong.signals.read_fd;
        cases.push(wrong);
        for wrong in cases {
            assert!(encode_saved(&wrong).is_err());
            assert!(decode(&encode_fields(&wrong).unwrap()).is_err());
        }
    }

    #[test]
    fn context_allows_parent_change_but_never_loses_live_identity_or_terminal_dispatch_distinction()
    {
        let saved = fixture();
        let header = header(&saved);
        let mut current = saved.process.clone();
        current.parent = 1;
        current.executable = header.new_image.executable.clone();
        let live = ChildObservation {
            state: ChildState::Live,
            pgid: Some(saved.child_identity.pgid),
            session: Some(saved.child_identity.session),
        };
        assert_eq!(
            saved
                .validate_context(&header, &current, live, saved.cwd_identity)
                .unwrap(),
            ChildState::Live
        );
        let stopped = ChildObservation {
            state: ChildState::Stopped,
            ..live
        };
        assert_eq!(
            saved
                .validate_context(&header, &current, stopped, saved.cwd_identity)
                .unwrap(),
            ChildState::Stopped
        );
        let terminal = ChildObservation {
            state: ChildState::Unreaped,
            pgid: None,
            session: None,
        };
        assert_eq!(
            saved
                .validate_context(&header, &current, terminal, saved.cwd_identity)
                .unwrap(),
            ChildState::Unreaped
        );
        assert!(
            saved
                .validate_context(
                    &header,
                    &current,
                    ChildObservation {
                        state: ChildState::Live,
                        ..terminal
                    },
                    saved.cwd_identity
                )
                .is_err()
        );
        assert!(
            saved
                .validate_context(
                    &header,
                    &current,
                    ChildObservation {
                        pgid: Some(1),
                        ..terminal
                    },
                    saved.cwd_identity
                )
                .is_err()
        );
        let mut wrong = current.clone();
        wrong.birth = Birth::Linux(999);
        assert!(
            saved
                .validate_context(&header, &wrong, live, saved.cwd_identity)
                .is_err()
        );
        let mut wrong = current.clone();
        wrong.pgid += 1;
        assert!(
            saved
                .validate_context(&header, &wrong, live, saved.cwd_identity)
                .is_err()
        );
        let mut wrong = current.clone();
        wrong.executable.inode += 1;
        assert!(
            saved
                .validate_context(&header, &wrong, live, saved.cwd_identity)
                .is_err()
        );
        let mut wrong = saved.cwd_identity;
        wrong.inode += 1;
        assert!(
            saved
                .validate_context(&header, &current, live, wrong)
                .is_err()
        );
        let mut wrong = header.clone();
        wrong.fds.signal_read = 99;
        assert!(
            saved
                .validate_context(&wrong, &current, live, saved.cwd_identity)
                .is_err()
        );
        let mut rollback = header;
        rollback.phase = Phase::Rollback;
        rollback.rollback_attempt = 1;
        assert_eq!(
            saved
                .validate_context(&rollback, &saved.process, terminal, saved.cwd_identity)
                .unwrap(),
            ChildState::Unreaped
        );
    }
}
