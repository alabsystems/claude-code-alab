// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Shared different-image actors and local-provider qualification. The runner takes an
//! independently built old image, its compiled version, and a persistent
//! owned Native world. It never removes a world, executable or unproved path.
//! Its store copies call the real CLI; debug callbacks observe/fault only the
//! production syscall, entry and precommit boundaries. No provider is mocked.

#[cfg(all(debug_assertions, any(target_os = "macos", target_os = "linux")))]
mod native {
    use claude_code_alab::supervise::image::fixture::{self, Event, Facts, Hooks};
    use claude_code_alab::transfer_capsule::{Decoder, Encoder, Phase};
    use claude_code_alab::{cli, procs, store, sys};
    use std::ffi::{CString, OsStr, OsString};
    use std::fs::{File, OpenOptions};
    use std::io::{self, Read as _, Write as _};
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
    use std::os::unix::ffi::{OsStrExt as _, OsStringExt as _};
    use std::os::unix::fs::{
        DirBuilderExt as _, MetadataExt as _, OpenOptionsExt as _, PermissionsExt as _,
    };
    use std::os::unix::process::ExitStatusExt as _;
    use std::path::{Path, PathBuf};
    use std::process::{Command, Stdio};
    use std::sync::OnceLock;
    use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
    use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

    const HANG: Duration = Duration::from_secs(60);
    const POLL: Duration = Duration::from_millis(10);
    const OLD_BUILD: u64 = 11;
    const NEW_BUILD: u64 = 22;
    const VENDOR_BUILD: u64 = 1_000_009_000_009_000_001;
    const CONFIG: &str = ".image-fixture";
    const RAW_ENV: &[u8] = b"image-raw-\xff-\xfe";
    const RAW_KEY: &str = "IMAGE_FIXTURE_RAW_BYTES";
    /// The one vendor argument of the fixture's launch: a flag the resume
    /// grammar keeps, with a value that is more than ASCII.
    const VENDOR_ARG: &str = "--model=opaque-\u{fc}n\u{ef}";
    const SID: &str = "0f0f0f0f-1111-2222-3333-444455556666";

    fn invalid(why: impl Into<String>) -> io::Error {
        io::Error::new(io::ErrorKind::InvalidData, why.into())
    }
    fn require(ok: bool, why: &str) -> io::Result<()> {
        if ok { Ok(()) } else { Err(invalid(why)) }
    }
    fn until<T>(phase: &str, mut read: impl FnMut() -> io::Result<Option<T>>) -> io::Result<T> {
        let end = Instant::now() + HANG;
        loop {
            if let Some(value) = read()? {
                return Ok(value);
            }
            if Instant::now() >= end {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    format!("60s hang detector: {phase}"),
                ));
            }
            std::thread::sleep(POLL);
        }
    }
    fn monotonic_ns() -> io::Result<u64> {
        // SAFETY: initialized writable native timespec, valid monotonic clock.
        let mut now: libc::timespec = unsafe { std::mem::zeroed() };
        // SAFETY: now is writable for exactly one timespec.
        if unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &raw mut now) } != 0 {
            return Err(io::Error::last_os_error());
        }
        u64::try_from(now.tv_sec)
            .ok()
            .and_then(|s| s.checked_mul(1_000_000_000))
            .and_then(|s| {
                u64::try_from(now.tv_nsec)
                    .ok()
                    .and_then(|n| s.checked_add(n))
            })
            .ok_or_else(|| invalid("invalid monotonic clock"))
    }
    fn epoch_ms() -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64
    }
    fn append(path: &Path, body: &str) -> io::Result<()> {
        let mut file = OpenOptions::new()
            .append(true)
            .create(true)
            .mode(0o600)
            .open(path)?;
        file.write_all(body.as_bytes())?;
        file.write_all(b"\n")?;
        file.sync_data()
    }
    fn contents(path: &Path) -> io::Result<String> {
        match std::fs::read_to_string(path) {
            Ok(s) => Ok(s),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(String::new()),
            Err(e) => Err(e),
        }
    }
    fn digest(bytes: &[u8]) -> u64 {
        bytes.iter().fold(0xcbf29ce484222325, |h, b| {
            (h ^ u64::from(*b)).wrapping_mul(0x100000001b3)
        })
    }
    fn phase(p: Phase) -> &'static str {
        match p {
            Phase::Forward => "forward",
            Phase::Rollback => "rollback",
        }
    }

    #[derive(Clone, Copy, PartialEq, Eq)]
    enum Case {
        Busy,
        ExecFailure,
        Rollback,
        Expired,
        Held,
        HeldRoles,
        Malformed,
    }
    impl Case {
        fn word(self) -> &'static str {
            match self {
                Self::Busy => "busy-forward",
                Self::ExecFailure => "exec-failure",
                Self::Rollback => "one-rollback",
                Self::Expired => "expired-entry",
                Self::Held => "held-handoff",
                Self::HeldRoles => "held-roles",
                Self::Malformed => "malformed-adoption",
            }
        }
        fn number(self) -> u8 {
            match self {
                Self::Busy => 1,
                Self::ExecFailure => 2,
                Self::Rollback => 3,
                Self::Expired => 4,
                Self::Held => 5,
                Self::HeldRoles => 6,
                Self::Malformed => 7,
            }
        }
        fn parse(n: u8) -> io::Result<Self> {
            match n {
                1 => Ok(Self::Busy),
                2 => Ok(Self::ExecFailure),
                3 => Ok(Self::Rollback),
                4 => Ok(Self::Expired),
                5 => Ok(Self::Held),
                6 => Ok(Self::HeldRoles),
                7 => Ok(Self::Malformed),
                _ => Err(invalid("unknown image fixture case")),
            }
        }
    }
    struct Config {
        case: Case,
        root: PathBuf,
    }
    impl Config {
        fn write(&self, bin: &Path) -> io::Result<()> {
            let mut e = Encoder::new();
            e.u32(1)?;
            e.u8(self.case.number())?;
            e.os_string(self.root.as_os_str())?;
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(bin.join(CONFIG))?;
            file.write_all(&e.finish())?;
            file.sync_all()
        }
        fn read(exe: &Path) -> io::Result<Self> {
            let bytes = std::fs::read(
                exe.parent()
                    .ok_or_else(|| invalid("fixture executable has no parent"))?
                    .join(CONFIG),
            )?;
            let mut d = Decoder::new(&bytes)?;
            require(d.u32()? == 1, "fixture configuration schema differs")?;
            let value = Self {
                case: Case::parse(d.u8()?)?,
                root: d.os_string(16 * 1024)?.into(),
            };
            d.finish()?;
            require(value.root.is_absolute(), "fixture world is not absolute")?;
            Ok(value)
        }
    }
    static ACTIVE: OnceLock<Config> = OnceLock::new();
    static PIPE_POLICY: OnceLock<(libc::sighandler_t, libc::c_int)> = OnceLock::new();
    fn config() -> &'static Config {
        ACTIVE
            .get()
            .expect("custom fixture entry installed its private configuration")
    }
    fn mask(sig: i32) -> io::Result<bool> {
        // SAFETY: initialized native storage, filled by a query-only sigprocmask.
        let mut set: libc::sigset_t = unsafe { std::mem::zeroed() };
        // SAFETY: null new set means a query into writable initialized set.
        if unsafe { libc::sigprocmask(libc::SIG_BLOCK, std::ptr::null(), &raw mut set) } != 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: initialized set and a valid signal.
        Ok(unsafe { libc::sigismember(&raw const set, sig) == 1 })
    }
    fn pending(sig: i32) -> io::Result<bool> {
        // SAFETY: initialized native storage, filled by sigpending.
        let mut set: libc::sigset_t = unsafe { std::mem::zeroed() };
        // SAFETY: set has the correct native writable layout.
        if unsafe { libc::sigpending(&raw mut set) } != 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: initialized set and a valid signal.
        Ok(unsafe { libc::sigismember(&raw const set, sig) == 1 })
    }
    fn require_controller_wait_ownership() -> io::Result<()> {
        sys::ensure_single_threaded()?;
        // SAFETY: writable native action storage initialized by the query below.
        let mut action: libc::sigaction = unsafe { std::mem::zeroed() };
        // SAFETY: query only the controller's SIGCHLD disposition.
        if unsafe { libc::sigaction(libc::SIGCHLD, std::ptr::null(), &raw mut action) } != 0 {
            return Err(io::Error::last_os_error());
        }
        require(
            action.sa_sigaction == libc::SIG_DFL && action.sa_flags & libc::SA_NOCLDWAIT == 0,
            "fixture requires SIGCHLD=DFL without SA_NOCLDWAIT and no other reaper before any fork",
        )
    }
    fn pipe_policy() -> io::Result<(libc::sighandler_t, libc::c_int)> {
        // SAFETY: writable native action output initialized by sigaction below.
        let mut action: libc::sigaction = unsafe { std::mem::zeroed() };
        // SAFETY: query only the unmanaged SIGPIPE policy of this private process.
        if unsafe { libc::sigaction(libc::SIGPIPE, std::ptr::null(), &raw mut action) } != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok((action.sa_sigaction, action.sa_flags))
    }
    fn raw_environment_is_exact() -> io::Result<()> {
        observer_check(
            std::env::var_os(RAW_KEY)
                .as_deref()
                .is_some_and(|value| value.as_bytes() == RAW_ENV),
            "raw-environment",
        )
    }
    fn fact_line(kind: &str, f: &Facts<'_>) -> String {
        format!(
            "{kind} version={} phase={} deadline={} saved={:016x} prior={}",
            f.version,
            phase(f.phase),
            f.deadline_ns,
            digest(f.original),
            f.signals.prior_mask
        )
    }
    #[derive(Debug)]
    struct ObserverCheck(&'static str);
    impl std::fmt::Display for ObserverCheck {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str(self.0)
        }
    }
    impl std::error::Error for ObserverCheck {}
    fn observer_check(ok: bool, check: &'static str) -> io::Result<()> {
        if ok {
            Ok(())
        } else {
            Err(io::Error::new(
                io::ErrorKind::InvalidData,
                ObserverCheck(check),
            ))
        }
    }
    fn original_unmanaged_mask() -> io::Result<()> {
        // The local controller deliberately seeds SIGUSR1 as blocked. Check
        // that original bit; never recapture it after exec.
        observer_check(mask(libc::SIGUSR1)?, "original-unmanaged-mask")
    }
    fn record_observation(
        root: &Path,
        event: &'static str,
        result: io::Result<()>,
    ) -> io::Result<()> {
        if let Err(error) = &result {
            let check = error
                .get_ref()
                .and_then(|error| error.downcast_ref::<ObserverCheck>())
                .map_or("io", |check| check.0);
            append(
                &root.join("observer-failures"),
                &format!("schema=1 event={event} check={check}"),
            )?;
            // Make the first diagnostic durable before a controller consumes
            // its PTY error line. Neither a raw reply nor an OS error is logged.
            File::open(root)?.sync_all()?;
        }
        result
    }
    fn observe(event: &Event<'_>) -> io::Result<()> {
        let name = match event {
            Event::Entry(_) => "entry",
            Event::BeforeExec(_, _) => "before-exec",
            Event::ExecReturned(_, _) => "exec-returned",
            Event::DescriptorsRestored(_, _) => "descriptors-restored",
            Event::SignalsRestored(_, _) => "signals-restored",
            Event::Resumed(_, _) => "resumed",
        };
        record_observation(&config().root, name, observe_inner(event))
    }
    fn observe_inner(event: &Event<'_>) -> io::Result<()> {
        let cfg = config();
        let log = cfg.root.join("events");
        match event {
            Event::Entry(f) => {
                raw_environment_is_exact()?;
                observer_check(sys::managed_signals_blocked()?, "entry-managed-mask")?;
                append(&log, &fact_line("entry", f))?;
                // This is a private test barrier installed by Rust code, with no
                // public CLI/env switch. The controller owns both ends of it.
                until("controller release of actual entry", || {
                    Ok(cfg.root.join("release-entry").exists().then_some(()))
                })?;
                observer_check(pending(libc::SIGHUP)?, "entry-pending-hup")?;
                append(&log, "entry-hup-pending")?;
            }
            Event::BeforeExec(f, originals) => {
                raw_environment_is_exact()?;
                observer_check(
                    Some(&pipe_policy()?) == PIPE_POLICY.get(),
                    "exec-sigpipe-policy",
                )?;
                original_unmanaged_mask()?;
                observer_check(sys::managed_signals_blocked()?, "exec-managed-mask")?;
                for &(fd, old) in *originals {
                    observer_check(
                        sys::descriptor_flags(fd)? == old & !libc::FD_CLOEXEC,
                        "exec-descriptor-flags",
                    )?;
                }
                append(&log, &fact_line("before-exec", f))?;
                if cfg.case == Case::ExecFailure {
                    observer_check(
                        f.signals.prior_mask != 0 && mask(libc::SIGUSR1)?,
                        "failure-original-mask",
                    )?;
                    observer_check(
                        sys::kill(std::process::id() as i32, libc::SIGHUP),
                        "failure-queue-hup",
                    )?;
                    observer_check(pending(libc::SIGHUP)?, "failure-pending-hup")?;
                }
            }
            Event::ExecReturned(p, errno) => {
                raw_environment_is_exact()?;
                observer_check(
                    Some(&pipe_policy()?) == PIPE_POLICY.get(),
                    "returned-sigpipe-policy",
                )?;
                append(
                    &log,
                    &format!(
                        "exec-return phase={} errno={} sigpipe=unchanged raw-env=exact",
                        phase(*p),
                        errno.unwrap_or(-1)
                    ),
                )?;
            }
            Event::DescriptorsRestored(f, originals) => {
                observer_check(sys::managed_signals_blocked()?, "descriptor-managed-mask")?;
                for &(fd, old) in *originals {
                    let pipe = fd == f.signals.read_fd || fd == f.signals.write_fd;
                    observer_check(
                        sys::descriptor_flags(fd)?
                            == if pipe { old & !libc::FD_CLOEXEC } else { old },
                        "restored-descriptor-flags",
                    )?;
                }
                append(&log, &fact_line("fd-restored-before-mask", f))?;
            }
            Event::SignalsRestored(f, originals) => {
                raw_environment_is_exact()?;
                observer_check(
                    Some(&pipe_policy()?) == PIPE_POLICY.get(),
                    "restored-sigpipe-policy",
                )?;
                for &(fd, old) in *originals {
                    observer_check(sys::descriptor_flags(fd)? == old, "restored-pipe-flags")?;
                }
                for (index, &sig) in sys::HANDLED.iter().enumerate() {
                    observer_check(
                        mask(sig)? == (f.signals.prior_mask & (1 << index) != 0),
                        "restored-managed-mask",
                    )?;
                }
                original_unmanaged_mask()?;
                append(&log, &fact_line("signal-mask-restored", f))?;
            }
            Event::Resumed(f, ready) => {
                raw_environment_is_exact()?;
                for (index, &sig) in sys::HANDLED.iter().enumerate() {
                    observer_check(
                        mask(sig)? == (f.signals.prior_mask & (1 << index) != 0),
                        "adopted-managed-mask",
                    )?;
                }
                original_unmanaged_mask()?;
                append(&log, &format!("{} ready={ready}", fact_line("resumed", f)))?
            }
        }
        Ok(())
    }
    fn exec_path(p: Phase, path: &Path) -> io::Result<PathBuf> {
        if config().case == Case::ExecFailure && p == Phase::Forward {
            Ok(config().root.join("deliberately-absent-exec"))
        } else {
            Ok(path.to_owned())
        }
    }
    fn precommit(p: Phase) -> io::Result<()> {
        if config().case == Case::Rollback && p == Phase::Forward {
            Err(invalid("fixture refuses precommit before any final commit"))
        } else {
            Ok(())
        }
    }
    /// Change the retained old build's version record after the old image
    /// handed off and before this new image adopts, so that adoption finds the
    /// retained pair changed and holds the handoff with the vendor still alive.
    fn hold_handoff(exe: &Path) -> io::Result<()> {
        let build = exe
            .parent()
            .and_then(Path::parent)
            .ok_or_else(|| invalid("held fixture image has no build directory"))?;
        require(
            build.file_name() == Some(OsStr::new(&NEW_BUILD.to_string())),
            "held fixture rewrite runs only in the new image",
        )?;
        let own = build
            .parent()
            .ok_or_else(|| invalid("held fixture image has no package directory"))?;
        std::fs::write(
            own.join(format!("{OLD_BUILD}.version")),
            format!("version v1\n{}\n", cli::VERSION),
        )?;
        append(&config().root.join("events"), "held old-version-rewritten")?;
        queue_caught_signal(libc::SIGHUP)?;
        append(&config().root.join("events"), "held hup-queued")
    }
    /// Close the inherited retention gate of the old build after the old image
    /// handed off and before this new image adopts, then queue a caught HUP.
    /// Taking the inherited roles then fails after the saved state is proved,
    /// so adoption holds, and must still drain the self-pipe and follow its
    /// child.
    fn close_old_gate(exe: &Path) -> io::Result<()> {
        let build = exe
            .parent()
            .and_then(Path::parent)
            .ok_or_else(|| invalid("held fixture image has no build directory"))?;
        require(
            build.file_name() == Some(OsStr::new(&NEW_BUILD.to_string())),
            "held fixture gate close runs only in the new image",
        )?;
        let prefix = build
            .parent()
            .and_then(Path::parent)
            .and_then(Path::parent)
            .ok_or_else(|| invalid("held fixture image has no prefix"))?;
        let gate = std::fs::metadata(
            prefix
                .join("leases")
                .join(format!("store-claude-code-alab-{OLD_BUILD}.gate")),
        )?;
        let mut found = Vec::new();
        for fd in 3..1024 {
            // SAFETY: F_GETFD only queries this descriptor number.
            if unsafe { libc::fcntl(fd, libc::F_GETFD) } < 0 {
                continue;
            }
            // SAFETY: the descriptor is open, and this single-threaded entry
            // closes nothing while the borrow lives.
            let open = unsafe { std::os::fd::BorrowedFd::borrow_raw(fd) };
            let meta = std::fs::File::from(open.try_clone_to_owned()?).metadata()?;
            if meta.dev() == gate.dev() && meta.ino() == gate.ino() {
                found.push(fd);
            }
        }
        let [fd] = found[..] else {
            return Err(invalid("held fixture found no single inherited old gate"));
        };
        // SAFETY: this private fixture entry closes the one descriptor it found;
        // no Rust owner exists for inherited roles before adoption.
        let closed = unsafe { libc::close(fd) };
        require(closed == 0, "held fixture could not close the old gate")?;
        append(&config().root.join("events"), "held-roles old-gate-closed")?;
        queue_caught_signal(libc::SIGHUP)?;
        append(&config().root.join("events"), "held hup-queued")
    }
    /// Point the capsule marker at a descriptor no capsule can be, after the
    /// old image handed off and before this new image adopts. Adoption then
    /// refuses the entry before any saved state is proved, with the managed
    /// signals still blocked by the old image's exec guard, so it holds with
    /// no recorded child and must find the inherited vendor itself.
    fn break_entry_marker() -> io::Result<()> {
        sys::ensure_single_threaded()?;
        // SAFETY: this private fixture entry is single-threaded (checked
        // above) and changes its own environment before the CLI reads it.
        unsafe { std::env::set_var("CLAUDE_ALAB_IMAGE_FD", "2") };
        append(
            &config().root.join("events"),
            "malformed entry-marker-broken",
        )
    }
    /// Queue `sig` on the inherited signal self-pipe as the old image's handler
    /// does for a signal caught just before its exec: one byte, the signal
    /// number. The pipe's write end is the only inherited write-only FIFO.
    fn queue_caught_signal(sig: i32) -> io::Result<()> {
        let mut found = Vec::new();
        for fd in 3..1024 {
            // SAFETY: F_GETFL only queries this descriptor number.
            let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
            if flags < 0 || flags & libc::O_ACCMODE != libc::O_WRONLY {
                continue;
            }
            // SAFETY: zeroed plain native storage, filled by a successful fstat.
            let mut st: libc::stat = unsafe { std::mem::zeroed() };
            // SAFETY: query only this open descriptor into writable storage.
            if unsafe { libc::fstat(fd, &raw mut st) } == 0
                && st.st_mode & libc::S_IFMT == libc::S_IFIFO
            {
                found.push(fd);
            }
        }
        let [fd] = found[..] else {
            return Err(invalid(
                "held fixture found no single inherited signal pipe",
            ));
        };
        let byte = [u8::try_from(sig).map_err(|_| invalid("signal number exceeds a byte"))?];
        // SAFETY: one initialized byte written to the found pipe write end.
        let n = unsafe { libc::write(fd, byte.as_ptr().cast(), 1) };
        require(n == 1, "held fixture could not queue the caught signal")
    }
    fn custom_harness(exe: &Path, factory: claude_code_alab::supervise::image::HostFactory) -> ! {
        let cfg = Config::read(exe).expect("read private immutable fixture configuration");
        let failure = cfg.case == Case::ExecFailure;
        ACTIVE
            .set(cfg)
            .unwrap_or_else(|_| panic!("fixture configuration installed twice"));
        PIPE_POLICY
            .set(pipe_policy().expect("read original unmanaged SIGPIPE"))
            .unwrap_or_else(|_| panic!("SIGPIPE fixture policy installed twice"));
        if config().case == Case::Held
            && std::env::var_os("CLAUDE_ALAB_IMAGE_FD").is_some()
            && std::env::var_os("CLAUDE_ALAB_IMAGE_PROBE").is_none()
        {
            hold_handoff(exe).expect("change the retained old image before adoption");
        }
        if config().case == Case::HeldRoles
            && std::env::var_os("CLAUDE_ALAB_IMAGE_FD").is_some()
            && std::env::var_os("CLAUDE_ALAB_IMAGE_PROBE").is_none()
        {
            close_old_gate(exe).expect("close the inherited old gate before adoption");
        }
        if config().case == Case::Malformed
            && std::env::var_os("CLAUDE_ALAB_IMAGE_FD").is_some()
            && std::env::var_os("CLAUDE_ALAB_IMAGE_PROBE").is_none()
        {
            break_entry_marker().expect("break the capsule marker before adoption");
        }
        if failure
            && std::env::var_os("CLAUDE_ALAB_IMAGE_FD").is_none()
            && std::env::var_os("CLAUDE_ALAB_IMAGE_PROBE").is_none()
        {
            // SAFETY: this genuinely single-threaded fixture owns its mask; it
            // deliberately supplies nonzero original bits to the production CLI.
            let mut blocked: libc::sigset_t = unsafe { std::mem::zeroed() };
            // SAFETY: all pointers name initialized writable native mask storage.
            unsafe {
                libc::sigemptyset(&raw mut blocked);
                libc::sigaddset(&raw mut blocked, libc::SIGQUIT);
                libc::sigaddset(&raw mut blocked, libc::SIGUSR1);
            }
            // SAFETY: add only the fixture's two deliberate original mask bits.
            assert_eq!(
                // SAFETY: initialized native mask, one private native thread.
                unsafe {
                    libc::sigprocmask(libc::SIG_BLOCK, &raw const blocked, std::ptr::null_mut())
                },
                0
            );
        }
        fixture::install(Hooks {
            observe,
            exec_path,
            precommit,
        })
        .expect("single-threaded fixture hook installation");
        match cli::main_with_image_host(factory) {
            Ok(never) => match never {},
            Err(retained) => {
                std::mem::forget(retained);
                std::process::exit(70);
            }
        }
    }

    static TERM: AtomicBool = AtomicBool::new(false);
    static HUPS: AtomicU32 = AtomicU32::new(0);
    extern "C" fn vendor_signal(sig: i32) {
        if sig == libc::SIGTERM {
            TERM.store(true, Ordering::Relaxed);
        } else if sig == libc::SIGHUP {
            HUPS.fetch_add(1, Ordering::Relaxed);
        }
    }
    fn vendor(exe: &Path) -> io::Result<()> {
        let cfg = Config::read(exe)?;
        println!(
            "image-fixture-vendor-body pid={} version={}",
            std::process::id(),
            cli::VERSION
        );
        std::io::stdout().flush()?;
        if std::env::args_os().nth(1).as_deref() == Some(OsStr::new("--fixture-proof")) {
            return Ok(());
        }
        for sig in [libc::SIGTERM, libc::SIGHUP] {
            // SAFETY: POD action fields with an initialized mask below.
            let mut action: libc::sigaction = unsafe { std::mem::zeroed() };
            action.sa_sigaction = vendor_signal as extern "C" fn(i32) as libc::sighandler_t;
            action.sa_flags = libc::SA_RESTART;
            // SAFETY: action is initialized, the handler only touches lock-free atomics.
            unsafe {
                libc::sigemptyset(&raw mut action.sa_mask);
            }
            // SAFETY: install only in this private vendor process, not the test controller.
            if unsafe { libc::sigaction(sig, &raw const action, std::ptr::null_mut()) } != 0 {
                return Err(io::Error::last_os_error());
            }
        }
        require_controller_wait_ownership()?;
        let mut descendant = Command::new("/bin/cat")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()?;
        let mut cat_in = descendant
            .stdin
            .take()
            .ok_or_else(|| invalid("cat stdin missing"))?;
        let mut cat_out = descendant
            .stdout
            .take()
            .ok_or_else(|| invalid("cat stdout missing"))?;
        cat_in.write_all(b"image-fixture-descendant-ran\n")?;
        cat_in.flush()?;
        let mut proof = Vec::new();
        until("in-place signed cat body proof", || {
            let mut poll = libc::pollfd {
                fd: cat_out.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            };
            // SAFETY: one initialized pollfd, zero timeout, no borrowed native memory escapes.
            if unsafe { libc::poll(&raw mut poll, 1, 0) } < 0 {
                return Err(io::Error::last_os_error());
            }
            if poll.revents & libc::POLLIN != 0 {
                let mut bytes = [0; 128];
                let n = cat_out.read(&mut bytes)?;
                proof.extend_from_slice(&bytes[..n]);
            }
            Ok(proof.ends_with(b"\n").then_some(()))
        })?;
        require(
            proof == b"image-fixture-descendant-ran\n",
            "cat body proof differs",
        )?;
        let actual = procs::capture_process(std::process::id())
            .ok_or_else(|| invalid("vendor identity unavailable"))?;
        let cat = procs::capture_process(descendant.id())
            .ok_or_else(|| invalid("proved descendant identity unavailable"))?;
        let mut e = Encoder::new();
        e.u32(actual.pid)?;
        e.u32(cat.pid)?;
        let args: Vec<_> = std::env::args_os().skip(1).collect();
        e.count(args.len(), 4096)?;
        for arg in &args {
            e.os_string(arg)?;
        }
        std::fs::write(cfg.root.join("vendor-state"), e.finish())?;
        let mut observation = format!(
            "schema=1 pid={} descendant={} argc={}\n",
            actual.pid,
            cat.pid,
            args.len()
        );
        const HEX: &[u8; 16] = b"0123456789abcdef";
        for (index, arg) in args.iter().enumerate() {
            observation.push_str(&format!("arg={index} hex="));
            for byte in arg.as_bytes() {
                observation.push(char::from(HEX[usize::from(byte >> 4)]));
                observation.push(char::from(HEX[usize::from(byte & 15)]));
            }
            observation.push('\n');
        }
        std::fs::write(cfg.root.join("vendor-observation"), observation)?;
        let sessions = cfg.root.join("home/.claude/sessions");
        std::fs::create_dir_all(&sessions)?;
        std::fs::write(
            sessions.join(format!("{}.json", actual.pid)),
            format!(
                "{{\"pid\":{},\"sessionId\":\"{SID}\",\"cwd\":\"{}\",\"status\":\"busy\",\"statusUpdatedAt\":{},\"startedAt\":{},\"kind\":\"interactive\",\"version\":\"9.9.1\"}}",
                actual.pid,
                cfg.root.display(),
                epoch_ms(),
                epoch_ms()
            ),
        )?;
        // SAFETY: query the private controlling terminal into initialized storage.
        let mut raw: libc::termios = unsafe { std::mem::zeroed() };
        // SAFETY: stdin belongs to this fixture's private PTY.
        if unsafe { libc::tcgetattr(0, &raw mut raw) } != 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: cfmakeraw mutates only this initialized local termios.
        unsafe {
            libc::cfmakeraw(&raw mut raw);
        }
        // SAFETY: applies raw mode only to the fixture's private PTY.
        if unsafe { libc::tcsetattr(0, libc::TCSANOW, &raw const raw) } != 0 {
            return Err(io::Error::last_os_error());
        }
        append(
            &cfg.root.join("vendor-events"),
            "busy raw draft=unfinished descendant-proof=ok",
        )?;
        let mut input = Vec::new();
        let mut observed_hups = 0;
        while !TERM.load(Ordering::Relaxed) {
            let hups = HUPS.load(Ordering::Relaxed);
            if hups != observed_hups {
                append(&cfg.root.join("vendor-events"), "hup-forwarded")?;
                observed_hups = hups;
            }
            let mut poll = libc::pollfd {
                fd: 0,
                events: libc::POLLIN,
                revents: 0,
            };
            // SAFETY: one initialized descriptor record, bounded poll in this vendor process.
            let n = unsafe { libc::poll(&raw mut poll, 1, 10) };
            if n < 0 {
                if io::Error::last_os_error().kind() == io::ErrorKind::Interrupted {
                    continue;
                }
                return Err(io::Error::last_os_error());
            }
            if poll.revents & libc::POLLIN != 0 {
                let mut bytes = [0; 256];
                let n = std::io::stdin().read(&mut bytes)?;
                require(n != 0, "vendor stdin unexpectedly closed")?;
                input.extend_from_slice(&bytes[..n]);
                while let Some(end) = input.iter().position(|b| *b == b'\n') {
                    let line: Vec<_> = input.drain(..=end).collect();
                    std::io::stdout().write_all(b"reply ")?;
                    std::io::stdout().write_all(&line)?;
                    std::io::stdout().flush()?;
                }
            }
        }
        append(&cfg.root.join("vendor-events"), "term-forwarded")?;
        drop(cat_in);
        let status = until("exact descendant normal reap", || descendant.try_wait())?;
        require(status.success(), "descendant did not exit normally")?;
        append(&cfg.root.join("vendor-events"), "descendant-reaped-once")?;
        Ok(())
    }

    struct Actor {
        pid: Option<i32>,
        signals: Option<sys::SignalBlock>,
        private_group: bool,
        tracked: Vec<procs::ProcessIdentity>,
        master: File,
        transcript: Vec<u8>,
        /// The terminal modes before the harness first ran.
        cooked: libc::termios,
    }
    impl Actor {
        fn spawn(exe: &Path, root: &Path, home: &Path) -> io::Result<Self> {
            require_controller_wait_ownership()?;
            let mut raw = [-1; 2];
            // SAFETY: two writable outputs, default private PTY attributes and size.
            if unsafe {
                libc::openpty(
                    &raw mut raw[0],
                    &raw mut raw[1],
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                )
            } != 0
            {
                return Err(io::Error::last_os_error());
            }
            // SAFETY: openpty returned these two distinct new descriptors.
            let master = File::from(unsafe { OwnedFd::from_raw_fd(raw[0]) });
            // SAFETY: independently owned slave returned by openpty.
            let slave = File::from(unsafe { OwnedFd::from_raw_fd(raw[1]) });
            let mut ack_raw = [-1; 2];
            // SAFETY: writable outputs for two new independent pipe endpoints.
            if unsafe { libc::pipe(ack_raw.as_mut_ptr()) } != 0 {
                return Err(io::Error::last_os_error());
            }
            // SAFETY: this endpoint was newly allocated by the successful pipe call.
            let ack_read = File::from(unsafe { OwnedFd::from_raw_fd(ack_raw[0]) });
            // SAFETY: the distinct other endpoint was independently allocated.
            let mut ack_write = File::from(unsafe { OwnedFd::from_raw_fd(ack_raw[1]) });
            for fd in raw.into_iter().chain(ack_raw) {
                require(fd >= 3, "private PTY occupied standard descriptors")?;
                // SAFETY: query/set flags on these live newly owned PTY descriptors.
                let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
                // SAFETY: preserve all prior flags and prevent controller descriptor leakage.
                if flags < 0
                    // SAFETY: this descriptor is still owned and valid; preserve prior flags.
                    || unsafe { libc::fcntl(fd, libc::F_SETFD, flags | libc::FD_CLOEXEC) } < 0
                {
                    return Err(io::Error::last_os_error());
                }
            }
            // SAFETY: query/set only the controller's owned master status flags.
            let status = unsafe { libc::fcntl(master.as_raw_fd(), libc::F_GETFL) };
            // SAFETY: the master's reader must be nonblocking for bounded observation.
            if status < 0
                // SAFETY: only this live private master's existing flags are changed.
                || unsafe {
                    libc::fcntl(master.as_raw_fd(), libc::F_SETFL, status | libc::O_NONBLOCK)
                } < 0
            {
                return Err(io::Error::last_os_error());
            }
            let executable =
                CString::new(exe.as_os_str().as_bytes()).map_err(|_| invalid("NUL executable"))?;
            // A launch the harness supervises: a terminal and a command line
            // that can be resumed. One it could never move (an argument that is
            // not UTF-8, as this fixture once sent) runs the vendor in the
            // harness's own place, and there is then no image to move.
            let args = [
                executable.clone(),
                CString::new("--alab-poll-ms=20").unwrap(),
                CString::new(VENDOR_ARG).unwrap(),
            ];
            let mut argv: Vec<_> = args.iter().map(|a| a.as_ptr()).collect();
            argv.push(std::ptr::null());
            let mut environment = std::collections::BTreeMap::new();
            for (key, value) in std::env::vars_os() {
                if !key.as_bytes().starts_with(b"ATERM")
                    && !key.as_bytes().starts_with(b"CLAUDE_ALAB_")
                    && !matches!(
                        key.to_str(),
                        Some("TERM_PROGRAM" | "CLAUDE_CONFIG_DIR" | "XDG_STATE_HOME" | "HOME")
                    )
                {
                    environment.insert(key, value);
                }
            }
            environment.insert("HOME".into(), home.as_os_str().to_owned());
            environment.insert("TERM".into(), "xterm-256color".into());
            environment.insert(RAW_KEY.into(), OsString::from_vec(RAW_ENV.to_vec()));
            environment.insert(
                "CLAUDE_CONFIG_DIR".into(),
                home.join(".claude").into_os_string(),
            );
            environment.insert("XDG_STATE_HOME".into(), root.join("state").into_os_string());
            let environment: Vec<_> = environment
                .into_iter()
                .map(|(key, value)| {
                    let mut word = key;
                    word.push("=");
                    word.push(value);
                    CString::new(word.as_bytes()).map_err(|_| invalid("NUL environment"))
                })
                .collect::<io::Result<_>>()?;
            let mut envp: Vec<_> = environment.iter().map(|a| a.as_ptr()).collect();
            envp.push(std::ptr::null());
            let fd_dir = if cfg!(target_os = "linux") {
                "/proc/self/fd"
            } else {
                "/dev/fd"
            };
            let mut last = 2;
            for item in std::fs::read_dir(fd_dir)? {
                if let Some(fd) = item?
                    .file_name()
                    .to_str()
                    .and_then(|s| s.parse::<i32>().ok())
                {
                    require(fd <= 1_048_575, "fixture descriptor bound exceeded")?;
                    last = last.max(fd);
                }
            }
            let child_slave = slave.as_raw_fd();
            let child_ack = ack_read.as_raw_fd();
            // SAFETY: writable POD native actions/mask, initialized before fork.
            let mut default: libc::sigaction = unsafe { std::mem::zeroed() };
            default.sa_sigaction = libc::SIG_DFL;
            // SAFETY: writable native signal-mask storage.
            let mut child_mask: libc::sigset_t = unsafe { std::mem::zeroed() };
            // SAFETY: initialize action mask and query the controller mask without changing it.
            unsafe {
                libc::sigemptyset(&raw mut default.sa_mask);
            }
            // SAFETY: null new mask queries into initialized storage.
            if unsafe { libc::sigprocmask(libc::SIG_BLOCK, std::ptr::null(), &raw mut child_mask) }
                != 0
            {
                return Err(io::Error::last_os_error());
            }
            for &sig in sys::HANDLED {
                // SAFETY: valid managed signal and initialized writable mask.
                if unsafe { libc::sigdelset(&raw mut child_mask, sig) } != 0 {
                    return Err(io::Error::last_os_error());
                }
            }
            for sig in [libc::SIGQUIT, libc::SIGUSR1] {
                // SAFETY: valid signals and initialized native mask. Every actual
                // handoff carries nonzero original managed and unmanaged bits.
                if unsafe { libc::sigaddset(&raw mut child_mask, sig) } != 0 {
                    return Err(io::Error::last_os_error());
                }
            }
            let block = sys::SignalBlock::managed()?;
            // SAFETY: one native thread, all allocation/pointer graphs prepared;
            // the child branch uses only native calls and never returns to Rust.
            let pid = unsafe { libc::fork() };
            if pid < 0 {
                let error = io::Error::last_os_error();
                let _owner = ProofOwner {
                    child: None,
                    signals: Some(ProofMask::Managed(block)),
                };
                return Err(error);
            }
            if pid == 0 {
                // SAFETY: private child, prebuilt immutable buffers, async-safe native calls only.
                unsafe {
                    if libc::setsid() < 0 || libc::ioctl(child_slave, libc::TIOCSCTTY as _, 0) < 0 {
                        libc::_exit(126);
                    }
                    if libc::tcsetpgrp(child_slave, libc::getpgrp()) != 0 {
                        libc::_exit(126);
                    }
                    for fd in 0..=2 {
                        if libc::dup2(child_slave, fd) < 0 {
                            libc::_exit(126);
                        }
                    }
                    for fd in 3..=last {
                        if fd != child_ack {
                            libc::close(fd);
                        }
                    }
                    for &sig in sys::HANDLED {
                        if libc::sigaction(sig, &raw const default, std::ptr::null_mut()) != 0 {
                            libc::_exit(126);
                        }
                    }
                    // Ownership evidence for this group before exec; this is
                    // deliberately not a copied-image execution body proof.
                    if libc::write(1, c"fixture-private-group-ready\n".as_ptr().cast(), 28) != 28 {
                        libc::_exit(126);
                    }
                    // No vendor or descendant can start until the parent has
                    // published ownership and confirmed this private group.
                    let mut ack = 0_u8;
                    if libc::read(child_ack, (&raw mut ack).cast(), 1) != 1 || ack != 1 {
                        libc::_exit(126);
                    }
                    libc::close(child_ack);
                    if libc::sigprocmask(
                        libc::SIG_SETMASK,
                        &raw const child_mask,
                        std::ptr::null_mut(),
                    ) != 0
                    {
                        libc::_exit(126);
                    }
                    libc::execve(executable.as_ptr(), argv.as_ptr(), envp.as_ptr());
                    libc::write(2, c"fixture exec failed\n".as_ptr().cast(), 20);
                    libc::_exit(127);
                }
            }
            drop(slave);
            drop(ack_read);
            // The child is still blocked before exec, so these are the modes
            // the harness saves before its first child starts.
            let cooked = tty_modes(&master)?;
            let mut actor = Self {
                pid: Some(pid),
                signals: Some(block),
                private_group: false,
                tracked: Vec::new(),
                master,
                transcript: Vec::new(),
                cooked,
            };
            actor
                .signals
                .as_mut()
                .expect("fork mask custody")
                .restore()?;
            actor.signals = None;
            until("owned child establishes its private session/group", || {
                actor.drain()?;
                Ok(actor
                    .transcript
                    .windows(b"fixture-private-group-ready".len())
                    .any(|w| w == b"fixture-private-group-ready")
                    .then_some(()))
            })?;
            // SAFETY: the acknowledged child remains blocked before exec;
            // these queries corroborate the private group marker with kernel facts.
            require(
                // SAFETY: exact positive still-owned child, blocked before exec.
                unsafe { libc::getpgid(pid) == pid && libc::getsid(pid) == pid },
                "private group marker disagrees with kernel group/session",
            )?;
            actor.private_group = true;
            ack_write.write_all(&[1])?;
            drop(ack_write);
            Ok(actor)
        }
        fn drain(&mut self) -> io::Result<()> {
            for _ in 0..32 {
                let mut bytes = [0; 4096];
                match self.master.read(&mut bytes) {
                    Ok(0) => break,
                    Ok(n) => {
                        require(
                            self.transcript.len() + n <= 1024 * 1024,
                            "PTY transcript overflow",
                        )?;
                        self.transcript.extend_from_slice(&bytes[..n]);
                    }
                    Err(e)
                        if e.kind() == io::ErrorKind::WouldBlock
                            || e.raw_os_error() == Some(libc::EIO) =>
                    {
                        break;
                    }
                    Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                    Err(e) => return Err(e),
                }
            }
            Ok(())
        }
        fn ping(&mut self, nonce: &str) -> io::Result<()> {
            self.master.write_all(format!("{nonce}\n").as_bytes())?;
            until("same vendor echoes PTY nonce", || {
                self.drain()?;
                self.reject_observer_failure()?;
                Ok(self
                    .transcript
                    .windows(nonce.len() + 6)
                    .any(|w| w == format!("reply {nonce}").as_bytes())
                    .then_some(()))
            })
        }
        fn reject_observer_failure(&self) -> io::Result<()> {
            require(
                !String::from_utf8_lossy(&self.transcript)
                    .contains("debug image fixture observer failed"),
                "a failed/panicked observer cannot qualify a case",
            )
        }
        fn reap(&mut self) -> io::Result<Option<sys::ChildEvent>> {
            match self.wait_event()? {
                Some(sys::ChildEvent::Stopped(_) | sys::ChildEvent::Continued) => {
                    Err(invalid("owned harness unexpectedly stopped or continued"))
                }
                event => Ok(event),
            }
        }
        /// One owned wait event, including a job-control stop or continue.
        fn wait_event(&mut self) -> io::Result<Option<sys::ChildEvent>> {
            let Some(pid) = self.pid else {
                return Ok(None);
            };
            let mut raw = 0;
            // SAFETY: exact previously owned positive child, writable status;
            // no other controller code waits this PID.
            let waited = unsafe {
                libc::waitpid(
                    pid,
                    &raw mut raw,
                    libc::WNOHANG | libc::WUNTRACED | libc::WCONTINUED,
                )
            };
            if waited == 0 {
                return Ok(None);
            }
            if waited < 0 {
                let error = io::Error::last_os_error();
                if error.kind() == io::ErrorKind::Interrupted {
                    return Ok(None);
                }
                if error.raw_os_error() == Some(libc::ECHILD) {
                    self.pid = None;
                    self.private_group = false;
                    return Err(invalid(
                        "ECHILD: terminal proof was lost; stale PID/group ownership cleared",
                    ));
                }
                return Err(error);
            }
            require(waited == pid, "wait returned an unrelated child")?;
            let status = std::process::ExitStatus::from_raw(raw);
            let event = if let Some(code) = status.code() {
                sys::ChildEvent::Exited(code)
            } else if let Some(sig) = status.signal() {
                sys::ChildEvent::Signaled(sig)
            } else if let Some(sig) = status.stopped_signal() {
                return Ok(Some(sys::ChildEvent::Stopped(sig)));
            } else if status.continued() {
                return Ok(Some(sys::ChildEvent::Continued));
            } else {
                return Err(invalid("owned harness wait status is unknown"));
            };
            self.pid = None;
            self.private_group = false;
            Ok(Some(event))
        }
        fn prove_owned_without_reaping(&mut self) -> io::Result<()> {
            let pid = self
                .pid
                .ok_or_else(|| invalid("no owned actor PID remains"))?;
            // SAFETY: initialized native output; WNOWAIT does not consume an event.
            let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
            // SAFETY: this exact positive PID, no other event or child is consumed.
            if unsafe {
                libc::waitid(
                    libc::P_PID,
                    pid as libc::id_t,
                    &raw mut info,
                    libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
                )
            } != 0
            {
                let error = io::Error::last_os_error();
                if error.raw_os_error() == Some(libc::ECHILD) {
                    self.pid = None;
                    self.private_group = false;
                }
                return Err(error);
            }
            Ok(())
        }
        fn finish(&mut self) -> io::Result<()> {
            let pid = self
                .pid
                .ok_or_else(|| invalid("harness was already reaped"))?;
            self.prove_owned_without_reaping()?;
            require(
                sys::kill(pid, libc::SIGTERM),
                "cannot send TERM to actual supervisor",
            )?;
            let event = until("supervisor forwards TERM and exits after vendor", || {
                self.reap()
            })?;
            require(
                event == sys::ChildEvent::Exited(0),
                "supervisor normal exit differed",
            )?;
            consumed_once(pid)
        }
    }
    fn consumed_once(pid: i32) -> io::Result<()> {
        let mut status = 0;
        // SAFETY: exact previously owned positive child, query-only WNOHANG.
        let again = unsafe { libc::waitpid(pid, &raw mut status, libc::WNOHANG) };
        require(
            again == -1 && io::Error::last_os_error().raw_os_error() == Some(libc::ECHILD),
            "supervisor terminal event was not consumed exactly once",
        )
    }
    impl Drop for Actor {
        fn drop(&mut self) {
            if let Some(pid) = self.pid {
                // The unreaped direct child keeps this PID unavailable for reuse.
                // Only its own verified session/group may be signalled as a group.
                let mut warned = false;
                let detector = Instant::now() + HANG;
                while self.pid.is_some() {
                    match self.prove_owned_without_reaping() {
                        Ok(()) => {
                            // SAFETY: WNOWAIT just confirmed exact child ownership;
                            // the group is independently acknowledged before exec.
                            unsafe {
                                libc::kill(
                                    if self.private_group { -pid } else { pid },
                                    libc::SIGKILL,
                                );
                            }
                            break;
                        }
                        Err(error) if self.pid.is_none() => {
                            eprintln!(
                                "cleanup missing terminal proof, PID ownership relinquished: {error}"
                            );
                            break;
                        }
                        Err(_) => {}
                    }
                    if !warned && Instant::now() >= detector {
                        eprintln!(
                            "cleanup hang detector: cannot prove wait ownership pid={pid}; retaining unknown state without signalling"
                        );
                        warned = true;
                    }
                    std::thread::sleep(POLL);
                }
                while self.pid.is_some() {
                    if let Ok(Some(_)) = self.reap() {
                        break;
                    }
                    if !warned && Instant::now() >= detector {
                        eprintln!(
                            "cleanup hang detector: retained owned harness pid={pid}; all image paths remain in place"
                        );
                        warned = true;
                    }
                    std::thread::sleep(POLL);
                }
            }
            // An unexpected terminal event may have been consumed before an
            // assertion failed. Never reuse its numerical process-group id;
            // individually retire only still-matching fixture descendants.
            for process in &self.tracked {
                if procs::capture_process(process.pid).is_some_and(|now| now.birth == process.birth)
                {
                    let _ = sys::kill(process.pid as i32, libc::SIGKILL);
                }
            }
            let detector = Instant::now() + HANG;
            let mut warned = false;
            while let Some(block) = self.signals.as_mut() {
                if block.restore().is_ok() {
                    self.signals = None;
                    break;
                }
                if !warned && Instant::now() >= detector {
                    eprintln!(
                        "cleanup hang detector: retained original controller signal mask custody"
                    );
                    warned = true;
                }
                std::thread::sleep(POLL);
            }
        }
    }

    fn copy_image(source: &Path, bin: &Path, name: &str) -> io::Result<PathBuf> {
        std::fs::create_dir_all(bin)?;
        let path = bin.join(name);
        require(!path.exists(), "fixture never overwrites an image")?;
        std::fs::copy(source, &path)?;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))?;
        Ok(path)
    }
    enum ProofMask {
        Managed(sys::SignalBlock),
        All(sys::AllSignals),
    }
    impl ProofMask {
        fn restore(&mut self) -> io::Result<()> {
            match self {
                Self::Managed(block) => block.restore(),
                Self::All(block) => block.restore(),
            }
        }
    }
    struct ProofOwner {
        child: Option<sys::ProbeChild>,
        signals: Option<ProofMask>,
    }
    fn spawn_proof_owner(
        path: &Path,
        args: &[OsString],
        changes: &[(OsString, OsString)],
    ) -> io::Result<ProofOwner> {
        let mut owner = ProofOwner {
            child: None,
            signals: None,
        };
        match sys::ProbeChild::spawn(path, args, changes) {
            Ok(child) => owner.child = Some(child),
            Err(refused) => {
                owner.child = refused.child.map(|child| *child);
                owner.signals = refused.signals.map(|block| ProofMask::All(*block));
                return Err(refused.error);
            }
        }
        Ok(owner)
    }
    fn lost_proof_owner(_held: &mut ProofOwner) -> ! {
        // This is the dev test executable's terminal boundary, never a library
        // recovery path. exit skips Drop, retaining child/mask receipt and all
        // immutable worlds through terminal failure without another signal.
        let _ = writeln!(
            io::stderr(),
            "copied-image cleanup refused: original wait custody lost; every world/image retained"
        );
        std::process::exit(70)
    }
    impl Drop for ProofOwner {
        fn drop(&mut self) {
            let detector = Instant::now() + HANG;
            let mut warned = false;
            loop {
                if self
                    .child
                    .as_ref()
                    .is_some_and(sys::ProbeChild::ownership_lost)
                {
                    lost_proof_owner(self);
                }
                if let Some(block) = self.signals.as_mut()
                    && block.restore().is_ok()
                {
                    self.signals = None;
                }
                if let Some(child) = self.child.as_mut()
                    && child.pending()
                {
                    let _ = child.cancel();
                    if !child.ownership_lost() {
                        let _ = child.try_wait();
                    }
                }
                if self
                    .child
                    .as_ref()
                    .is_some_and(sys::ProbeChild::ownership_lost)
                {
                    lost_proof_owner(self);
                }
                if self.signals.is_none()
                    && self.child.as_ref().is_none_or(|child| !child.pending())
                {
                    break;
                }
                if !warned && Instant::now() >= detector {
                    eprintln!(
                        "cleanup hang detector: retained copied-image probe/mask custody; every image path remains in place"
                    );
                    warned = true;
                }
                std::thread::sleep(POLL);
            }
        }
    }
    extern "C" fn steal_probe_wait(_: libc::c_int) {
        let mut status = 0;
        // SAFETY: this private negative-control role deliberately steals its
        // only child's terminal event; it returns no settlement authority.
        unsafe {
            libc::waitpid(-1, &raw mut status, libc::WNOHANG);
        }
    }

    fn lose_proof_owner() -> io::Result<()> {
        require_controller_wait_ownership()?;
        let mut owner = spawn_proof_owner(Path::new("/usr/bin/true"), &[], &[])?;
        until("lost-owner child terminal event", || {
            Ok(sys::child_has_ended(owner.child.as_ref().unwrap().pid().unwrap()).then_some(()))
        })?;
        // SAFETY: initialized writable native action and explicit unmanaged signal
        // in this isolated test role, which terminates through owner cleanup.
        let mut action: libc::sigaction = unsafe { std::mem::zeroed() };
        action.sa_sigaction = steal_probe_wait as *const () as usize;
        // SAFETY: valid action; delivering this process's own signal runs only the
        // deliberate reaper above while its original owner receipt stays held.
        unsafe {
            libc::sigemptyset(&raw mut action.sa_mask);
            if libc::sigaction(libc::SIGUSR1, &raw const action, std::ptr::null_mut()) != 0
                || libc::raise(libc::SIGUSR1) != 0
            {
                return Err(io::Error::last_os_error());
            }
        }
        require(
            owner.child.as_mut().unwrap().try_wait().is_err(),
            "reaper must remove actual wait custody",
        )?;
        drop(owner);
        Err(invalid("lost ProofOwner returned without terminal refusal"))
    }

    fn proof_owner_refuses_lost_wait_at_terminal(exe: &Path) -> io::Result<()> {
        let mut owner = spawn_proof_owner(exe, &["--fixture-lost-proof-owner".into()], &[])?;
        let mut out = Vec::new();
        let mut err = Vec::new();
        let status = until("lost-owner terminal refusal", || {
            let child = owner.child.as_mut().unwrap();
            child.drain_total(&mut out, &mut err, 64 * 1024)?;
            child.try_wait()
        })?;
        owner
            .child
            .as_mut()
            .unwrap()
            .drain_total(&mut out, &mut err, 64 * 1024)?;
        require(
            status.code() == Some(70)
                && String::from_utf8_lossy(&err).contains("original wait custody lost"),
            "lost probe receipt must fail terminally, never report settled success",
        )?;
        println!("image: lost-wait cleanup control passed; terminal refusal=70");
        Ok(())
    }
    fn body_proof(
        path: &Path,
        args: &[OsString],
        expected: &str,
        home: &Path,
        root: &Path,
    ) -> io::Result<()> {
        require_controller_wait_ownership()?;
        let changes = [
            ("HOME".into(), home.as_os_str().to_owned()),
            (
                "CLAUDE_CONFIG_DIR".into(),
                home.join(".claude").into_os_string(),
            ),
            ("XDG_STATE_HOME".into(), root.join("state").into_os_string()),
        ];
        let mut owner = spawn_proof_owner(path, args, &changes)?;
        let child = owner
            .child
            .as_mut()
            .expect("successful spawn transferred child ownership");
        let mut out = Vec::new();
        let mut err = Vec::new();
        let status = until("copied image actual body proof", || {
            child.drain(&mut out, &mut err, 64 * 1024)?;
            child.try_wait()
        })?;
        child.drain(&mut out, &mut err, 64 * 1024)?;
        require(
            status.success(),
            &format!("image body proof failed: {}", String::from_utf8_lossy(&err)),
        )?;
        require(
            String::from_utf8_lossy(&out).contains(expected),
            "copied image did not print its own expected body",
        )?;
        println!(
            "body-proof path={} stdout={}",
            path.display(),
            String::from_utf8_lossy(&out).trim_end()
        );
        Ok(())
    }
    fn select(program: &Path, build: u64) -> io::Result<()> {
        let next = program.join(format!("current-next-{}", std::process::id()));
        require(!next.exists(), "unexpected selection temporary")?;
        std::os::unix::fs::symlink(build.to_string(), &next)?;
        std::fs::rename(next, program.join("current"))
    }
    fn ready(program: &Path, build: u64, command: &str) -> io::Result<()> {
        std::fs::write(
            program.join(format!("{build}.ready")),
            format!(
                "ok\nplatform={}-{}\ncontents=bin:{command}\n",
                std::env::consts::ARCH,
                std::env::consts::OS
            ),
        )
    }
    fn tty_modes(file: &File) -> io::Result<libc::termios> {
        // SAFETY: writable initialized native terminal storage, this owned private PTY.
        let mut term: libc::termios = unsafe { std::mem::zeroed() };
        // SAFETY: query only the fixture-owned master; both PTY ends share these modes.
        if unsafe { libc::tcgetattr(file.as_raw_fd(), &raw mut term) } != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(term)
    }
    fn same_modes(a: &libc::termios, b: &libc::termios) -> bool {
        (a.c_iflag, a.c_oflag, a.c_cflag, a.c_lflag, a.c_cc)
            == (b.c_iflag, b.c_oflag, b.c_cflag, b.c_lflag, b.c_cc)
    }
    fn tty_raw(file: &File) -> io::Result<(u64, u64, u64)> {
        let term = tty_modes(file)?;
        require(
            term.c_lflag & (libc::ICANON | libc::ECHO) == 0,
            "vendor raw terminal modes were restored during image transfer",
        )?;
        let meta = file.metadata()?;
        Ok((meta.dev(), meta.ino(), meta.rdev()))
    }
    /// Whether the kernel reports `pid` stopped by job control.
    fn process_stopped(pid: u32) -> io::Result<bool> {
        #[cfg(target_os = "macos")]
        {
            // SAFETY: zeroed plain native storage, filled by an exact-size read.
            let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
            let size = libc::c_int::try_from(std::mem::size_of::<libc::proc_bsdinfo>())
                .map_err(|_| invalid("process info size"))?;
            let pid = libc::c_int::try_from(pid).map_err(|_| invalid("process id"))?;
            // SAFETY: info is writable for exactly `size` bytes of this flavor.
            let n = unsafe {
                libc::proc_pidinfo(pid, libc::PROC_PIDTBSDINFO, 0, (&raw mut info).cast(), size)
            };
            require(n == size, "cannot read the process state")?;
            Ok(info.pbi_status == libc::SSTOP)
        }
        #[cfg(target_os = "linux")]
        {
            let stat = std::fs::read_to_string(format!("/proc/{pid}/stat"))?;
            let state = stat
                .rsplit_once(')')
                .and_then(|(_, rest)| rest.split_ascii_whitespace().next())
                .ok_or_else(|| invalid("process state is unreadable"))?;
            Ok(state == "T")
        }
    }
    fn gate_is_held(prefix: &Path, build: u64) -> io::Result<()> {
        let image = claude_code_alab::self_image::Image::build_at(prefix, build)?;
        let hold = image.retain()?;
        let path = hold.path().to_owned();
        drop(hold);
        let gate = OpenOptions::new().read(true).write(true).open(path)?;
        match gate.try_lock() {
            Err(std::fs::TryLockError::WouldBlock) => Ok(()),
            Err(std::fs::TryLockError::Error(error)) => Err(error),
            Ok(()) => Err(invalid(
                "actual inherited image gate was not retained during entry",
            )),
        }
    }
    fn run_case(parent: &Path, old_source: &Path, old_version: &str, case: Case) -> io::Result<()> {
        let root = parent.join(format!(
            "{}-{}-{}.noindex",
            case.word(),
            std::process::id(),
            epoch_ms()
        ));
        std::fs::DirBuilder::new().mode(0o700).create(&root)?;
        println!("preserved-world {}", root.display());
        let home = root.join("home");
        std::fs::create_dir_all(&home)?;
        let prefix = store::default_prefix(&home);
        let own = prefix.join("store/claude-code-alab");
        let vendor_store = prefix.join("store/claude");
        let old_bin = own.join(OLD_BUILD.to_string()).join("bin");
        let new_bin = own.join(NEW_BUILD.to_string()).join("bin");
        let old = copy_image(old_source, &old_bin, "claude-alab")?;
        let new = copy_image(&std::env::current_exe()?, &new_bin, "claude-alab")?;
        let vendor_bin = vendor_store.join(VENDOR_BUILD.to_string()).join("bin");
        let vendor_path = copy_image(&std::env::current_exe()?, &vendor_bin, "claude")?;
        let cfg = Config {
            case,
            root: root.clone(),
        };
        cfg.write(&new_bin)?;
        cfg.write(&vendor_bin)?;
        for (build, version) in [(OLD_BUILD, old_version), (NEW_BUILD, cli::VERSION)] {
            ready(&own, build, "claude-alab")?;
            std::fs::write(
                own.join(format!("{build}.version")),
                format!("version v1\n{version}\n"),
            )?;
        }
        ready(&vendor_store, VENDOR_BUILD, "claude")?;
        std::fs::write(
            vendor_store.join(format!("{VENDOR_BUILD}.vendor")),
            "schema = 1\nprogram = \"claude\"\nversion = \"9.9.1\"\nvendor = \"Fake\"\n",
        )?;
        select(&vendor_store, VENDOR_BUILD)?;
        // Every copy is retained even if any following first exec never proves itself.
        body_proof(
            &old,
            &["--version".into()],
            &format!("claude-code-alab {old_version} "),
            &home,
            &root,
        )?;
        if case == Case::Busy {
            body_proof(
                &std::env::current_exe()?,
                &["--fixture-exact-env-hop".into()],
                "image-exact-env-body",
                &home,
                &root,
            )?;
        }
        body_proof(
            &new,
            &["--version".into()],
            &format!("claude-code-alab {} ", cli::VERSION),
            &home,
            &root,
        )?;
        body_proof(
            &vendor_path,
            &["--fixture-proof".into()],
            "image-fixture-vendor-body",
            &home,
            &root,
        )?;
        let failure = case == Case::ExecFailure;
        select(&own, if failure { NEW_BUILD } else { OLD_BUILD })?;
        let mut actor = Actor::spawn(if failure { &new } else { &old }, &root, &home)?;
        let pid = actor.pid.unwrap() as u32;
        let (vendor_pid, cat_pid, original_args) =
            until("busy vendor and proved descendant", || {
                actor.drain()?;
                actor.reject_observer_failure()?;
                if !contents(&root.join("vendor-events"))?.contains("busy raw draft=unfinished") {
                    return Ok(None);
                }
                let bytes = match std::fs::read(root.join("vendor-state")) {
                    Ok(bytes) => bytes,
                    Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
                    Err(e) => return Err(e),
                };
                let mut d = Decoder::new(&bytes)?;
                let child = d.u32()?;
                let cat = d.u32()?;
                let count = d.count(4096)?;
                let mut args = Vec::new();
                for _ in 0..count {
                    args.push(d.os_string(128 * 1024)?);
                }
                d.finish()?;
                Ok(Some((child, cat, args)))
            })?;
        require(
            original_args == [OsString::from(VENDOR_ARG)],
            "vendor arguments were changed",
        )?;
        let before = procs::capture_process(pid)
            .ok_or_else(|| invalid("original harness identity missing"))?;
        let child_before = procs::capture_process(vendor_pid)
            .ok_or_else(|| invalid("original vendor identity missing"))?;
        let cat_before = procs::capture_process(cat_pid)
            .ok_or_else(|| invalid("original proved cat identity missing"))?;
        actor.tracked = vec![child_before.clone(), cat_before.clone()];
        require(
            child_before.parent == pid && cat_before.parent == vendor_pid,
            "vendor/descendant ownership layout differs",
        )?;
        let terminal_before = tty_raw(&actor.master)?;
        actor.ping("before-own-image")?;
        select(&own, if failure { OLD_BUILD } else { NEW_BUILD })?;
        if matches!(case, Case::Held | Case::HeldRoles | Case::Malformed) {
            return held_handoff(
                case,
                actor,
                &root,
                &prefix,
                &new,
                old_version,
                (vendor_pid, &child_before, cat_pid),
            );
        }
        if !failure {
            let deadline = until("actual new-image entry body", || {
                actor.drain()?;
                actor.reject_observer_failure()?;
                let log = contents(&root.join("events"))?;
                Ok(log
                    .lines()
                    .find(|line| line.starts_with("entry "))
                    .and_then(|line| {
                        line.split_ascii_whitespace()
                            .find_map(|w| w.strip_prefix("deadline="))
                    })
                    .and_then(|n| n.parse::<u64>().ok()))
            })?;
            gate_is_held(&prefix, OLD_BUILD)?;
            gate_is_held(&prefix, NEW_BUILD)?;
            require(
                sys::kill(pid as i32, libc::SIGHUP),
                "cannot queue pending HUP during real new-image entry",
            )?;
            actor.ping("during-blocked-entry")?;
            if case == Case::Expired {
                until("original immutable10s deadline expires", || {
                    Ok((monotonic_ns()? > deadline).then_some(()))
                })?;
            }
            std::fs::write(root.join("release-entry"), b"release\n")?;
        }
        let expected = if failure {
            new.clone()
        } else if case == Case::Rollback {
            old.clone()
        } else {
            new.clone()
        };
        until("expected actual image and ordinary HUP forwarding", || {
            actor.drain()?;
            actor.reject_observer_failure()?;
            let image_ok =
                procs::capture_process(pid).is_some_and(|p| p.executable.path == expected);
            let log = contents(&root.join("vendor-events"))?;
            Ok((image_ok && log.contains("hup-forwarded")).then_some(()))
        })?;
        actor.ping("after-own-image")?;
        let after = procs::capture_process(pid)
            .ok_or_else(|| invalid("continuing harness identity missing"))?;
        require(
            (after.pid, &after.birth, after.pgid, after.session)
                == (before.pid, &before.birth, before.pgid, before.session),
            "harness PID/birth/group/session changed",
        )?;
        require(
            procs::capture_process(vendor_pid).as_ref() == Some(&child_before),
            "vendor PID/birth/group/image changed",
        )?;
        require(
            procs::capture_process(cat_pid).as_ref() == Some(&cat_before),
            "live descendant PID/birth/group/image changed",
        )?;
        require(
            tty_raw(&actor.master)? == terminal_before,
            "private terminal identity changed",
        )?;
        let events = contents(&root.join("events"))?;
        if failure {
            require(
                events.contains(&format!("exec-return phase=forward errno={}", libc::ENOENT)),
                "no actual ENOENT returned exec receipt",
            )?;
            let fd = events
                .find("fd-restored-before-mask")
                .ok_or_else(|| invalid("missing actual descriptor restore receipt"))?;
            let mask = events
                .find("signal-mask-restored")
                .ok_or_else(|| invalid("missing actual mask restore receipt"))?;
            require(
                fd < mask,
                "actual rollback mask preceded descriptor restoration",
            )?;
        } else if case == Case::Rollback {
            let entry = events
                .lines()
                .find(|l| l.starts_with("entry "))
                .ok_or_else(|| invalid("new entry receipt missing"))?;
            let rollback: Vec<_> = events
                .lines()
                .filter(|l| l.starts_with("before-exec ") && l.contains("phase=rollback"))
                .collect();
            require(
                rollback.len() == 1,
                "rollback was not exactly one actual attempt",
            )?;
            for key in ["deadline=", "saved="] {
                let field = |s: &str| {
                    s.split_ascii_whitespace()
                        .find(|w| w.starts_with(key))
                        .map(str::to_owned)
                };
                require(
                    field(entry) == field(rollback[0]),
                    "rollback changed original Saved bytes or deadline",
                )?;
            }
        } else if case == Case::Expired {
            require(
                events
                    .lines()
                    .any(|l| l.starts_with("resumed ") && l.contains("ready=false")),
                "expired entry did not resume normal supervision with recovery custody",
            )?;
        }
        actor.finish()?;
        actor.drain()?;
        actor.reject_observer_failure()?;
        let vendor_events = contents(&root.join("vendor-events"))?;
        require(
            vendor_events.matches("descendant-reaped-once").count() == 1,
            "descendant normal reap differs",
        )?;
        require(
            procs::capture_process(vendor_pid).is_none()
                && procs::capture_process(cat_pid).is_none(),
            "owned vendor/descendant remained after normal end",
        )?;
        std::fs::write(root.join("pty-transcript"), &actor.transcript)?;
        println!(
            "PASS {} harness={} child={} descendant={} old={} new={}",
            case.word(),
            pid,
            vendor_pid,
            cat_pid,
            old_version,
            cli::VERSION
        );
        Ok(())
    }

    /// The new image held the handoff with the vendor alive. In `Held` the
    /// retained pair changed; in `HeldRoles` an inherited role could not be
    /// taken after the saved state was proved. Either hold must first pass on
    /// a HUP caught before it began. In `Malformed` the capsule marker named
    /// no capsule, so no saved state was proved: the hold has no recorded
    /// child, no proved self-pipe and no saved modes, and must find the vendor
    /// as the one child the kernel proves it owns. Every hold must then follow
    /// a vendor stop, continue it, and pass on a later HUP. Then the vendor is
    /// killed by a signal: the held harness must end the same way, with the
    /// terminal modes it saved before its first child put back, or, in
    /// `Malformed`, with the modes the child set left in place.
    fn held_handoff(
        case: Case,
        mut actor: Actor,
        root: &Path,
        prefix: &Path,
        new: &Path,
        old_version: &str,
        (vendor_pid, vendor, cat_pid): (u32, &procs::ProcessIdentity, u32),
    ) -> io::Result<()> {
        let pid = actor
            .pid
            .ok_or_else(|| invalid("held harness was already reaped"))?;
        require(
            actor.cooked.c_lflag & (libc::ICANON | libc::ECHO) == libc::ICANON | libc::ECHO,
            "the private terminal did not start canonical",
        )?;
        // Held: the retained pair changed. HeldRoles: the old gate was closed,
        // so the roles cannot be taken. Either way a caught HUP was queued
        // first. Malformed: the capsule marker was broken, nothing was queued.
        let roles = case == Case::HeldRoles;
        let malformed = case == Case::Malformed;
        let (why, markers): (&str, &[&str]) = match case {
            Case::HeldRoles => (
                "image handoff held: inherited roles",
                &["held-roles old-gate-closed", "held hup-queued"],
            ),
            Case::Malformed => (
                "image handoff held: malformed adoption",
                &["malformed entry-marker-broken"],
            ),
            _ => (
                "image handoff held: retained image pair changed",
                &["held old-version-rewritten", "held hup-queued"],
            ),
        };
        until("new image holds the handoff", || {
            actor.drain()?;
            actor.reject_observer_failure()?;
            let held = String::from_utf8_lossy(&actor.transcript).contains(why);
            let events = contents(&root.join("events"))?;
            let prepared = markers.iter().all(|marker| events.contains(marker));
            Ok((held && prepared).then_some(()))
        })?;
        // A HUP the old image caught just before its exec waits on the self-pipe.
        if !malformed {
            until(
                "held harness passes on a HUP caught before the hold",
                || {
                    actor.drain()?;
                    Ok(contents(&root.join("vendor-events"))?
                        .contains("hup-forwarded")
                        .then_some(()))
                },
            )?;
        }
        require(
            !contents(&root.join("events"))?
                .lines()
                .any(|line| line.starts_with("entry ")),
            "a held handoff reached the entry body",
        )?;
        require(
            procs::capture_process(pid as u32).is_some_and(|p| p.executable.path == new),
            "the held handoff is not running the new image",
        )?;
        // A malformed adoption owns no gate: it never took the inherited roles.
        if !roles && !malformed {
            gate_is_held(prefix, OLD_BUILD)?;
        }
        if !malformed {
            gate_is_held(prefix, NEW_BUILD)?;
        }
        actor.ping("during-held-handoff")?;
        tty_raw(&actor.master)?;
        require(
            procs::capture_process(vendor_pid).as_ref() == Some(vendor),
            "vendor PID/birth/group/image changed during the hold",
        )?;
        // A child stop is followed: the hold stops too, and continues the
        // child once it is continued. Only the hold can continue the vendor.
        require(
            sys::kill(vendor_pid as i32, libc::SIGSTOP),
            "cannot stop the held vendor",
        )?;
        let stopped = until("held harness stops with its child", || actor.wait_event())?;
        require(
            stopped == sys::ChildEvent::Stopped(libc::SIGSTOP),
            "the held harness did not stop with its child",
        )?;
        // The vendor stays stopped until the hold itself is continued.
        require(
            process_stopped(vendor_pid)?,
            "the vendor was not stopped while the hold was",
        )?;
        std::thread::sleep(Duration::from_millis(200));
        require(
            process_stopped(vendor_pid)?,
            "the vendor ran again while the hold stayed stopped",
        )?;
        require(
            sys::kill(pid, libc::SIGCONT),
            "cannot continue the held harness",
        )?;
        let continued = until("held harness continues", || actor.wait_event())?;
        require(
            continued == sys::ChildEvent::Continued,
            "the held harness did not continue",
        )?;
        actor.ping("after-held-stop")?;
        // HUP sent to the harness alone is passed on to the child.
        let hups = contents(&root.join("vendor-events"))?
            .matches("hup-forwarded")
            .count();
        require(
            sys::kill(pid, libc::SIGHUP),
            "cannot send HUP to the held harness",
        )?;
        until("held harness passes HUP on", || {
            actor.drain()?;
            Ok((contents(&root.join("vendor-events"))?
                .matches("hup-forwarded")
                .count()
                > hups)
                .then_some(()))
        })?;
        require(
            procs::capture_process(vendor_pid).as_ref() == Some(vendor),
            "vendor PID/birth/group/image changed during the hold",
        )?;
        require(
            sys::kill(vendor_pid as i32, libc::SIGUSR2),
            "cannot signal the held vendor",
        )?;
        let event = until("held harness follows its child's signal death", || {
            actor.drain()?;
            actor.reap()
        })?;
        require(
            event == sys::ChildEvent::Signaled(libc::SIGUSR2),
            "the held harness did not end by its child's signal",
        )?;
        consumed_once(pid)?;
        if malformed {
            require(
                !same_modes(&tty_modes(&actor.master)?, &actor.cooked),
                "a malformed adoption put back modes it never proved",
            )?;
        } else {
            require(
                same_modes(&tty_modes(&actor.master)?, &actor.cooked),
                "the held harness did not restore the terminal modes saved before its first child",
            )?;
        }
        until("held vendor and descendant are gone", || {
            Ok((procs::capture_process(vendor_pid).is_none()
                && procs::capture_process(cat_pid).is_none())
            .then_some(()))
        })?;
        actor.drain()?;
        actor.reject_observer_failure()?;
        std::fs::write(root.join("pty-transcript"), &actor.transcript)?;
        let (caught, modes) = if malformed {
            ("none", "left")
        } else {
            ("passed-on", "restored")
        };
        println!(
            "PASS {} harness={pid} child={vendor_pid} descendant={cat_pid} old={old_version} new={} caught-hup={caught} stop=followed hup=passed-on signal=SIGUSR2 modes={modes}",
            case.word(),
            cli::VERSION
        );
        Ok(())
    }

    pub fn main(factory: claude_code_alab::supervise::image::HostFactory) -> io::Result<()> {
        let exe = std::env::current_exe()?;
        if std::env::args_os().skip(1).collect::<Vec<_>>()
            == [OsString::from("--fixture-body-proof")]
        {
            println!(
                "image-fixture-body pid={} version={}",
                std::process::id(),
                cli::VERSION
            );
            return Ok(());
        }
        match exe.file_name().and_then(OsStr::to_str) {
            Some("claude-alab") => custom_harness(&exe, factory),
            Some("claude") => return vendor(&exe),
            _ => {}
        }
        let private_args: Vec<_> = std::env::args_os().skip(1).collect();
        if private_args == [OsString::from("--fixture-lost-proof-owner")] {
            return lose_proof_owner();
        }
        if private_args == [OsString::from("--fixture-exact-env-hop")] {
            require_controller_wait_ownership()?;
            let _block = sys::SignalBlock::managed()?;
            return Err(sys::exec_exact(
                &exe,
                &["--fixture-exact-env-report".into()],
                &[(RAW_KEY.into(), OsString::from_vec(RAW_ENV.to_vec()))],
            ));
        }
        if private_args == [OsString::from("--fixture-exact-env-report")] {
            raw_environment_is_exact()?;
            println!(
                "image-exact-env-body pid={} raw-env=exact",
                std::process::id()
            );
            return Ok(());
        }
        let (args, requested) = super::qualification_args();
        proof_owner_refuses_lost_wait_at_terminal(&exe)?;
        if !requested {
            println!(
                "NOT RUN: different-image qualification requires OLD_IMAGE OLD_COMPILED_VERSION PERSISTENT_NATIVE_WORLD; no execution proof claimed"
            );
            return Ok(());
        }
        require(
            args.len() == 3,
            "image runner requires OLD_IMAGE OLD_COMPILED_VERSION PERSISTENT_NATIVE_WORLD",
        )?;
        let old = PathBuf::from(&args[0]).canonicalize()?;
        require_controller_wait_ownership()?;
        let old_version = args[1]
            .to_str()
            .ok_or_else(|| invalid("old compiled version is not UTF-8"))?;
        require(
            old_version != cli::VERSION,
            "qualification requires genuinely different compiled version stamps",
        )?;
        let root = PathBuf::from(&args[2]);
        require(
            root.is_absolute()
                && root.components().any(|c| c.as_os_str() == ".ubd")
                && root
                    .file_name()
                    .is_some_and(|n| n.as_bytes().ends_with(b".noindex")),
            "Native world must be an absolute owned .ubd/*.noindex path",
        )?;
        std::fs::create_dir_all(&root)?;
        // Native executable paths resolve aliases such as /tmp to /private/tmp.
        // Derive every fixture path from the same physical world directory.
        let root = root.canonicalize()?;
        for case in [
            Case::Busy,
            Case::ExecFailure,
            Case::Rollback,
            Case::Expired,
            Case::Held,
            Case::HeldRoles,
            Case::Malformed,
        ] {
            run_case(&root, &old, old_version, case)?;
        }
        println!(
            "test result:7 different-image local-provider cases passed; every world/image retained"
        );
        Ok(())
    }
}

/// The positional (non-flag) runner arguments, and whether they request a
/// qualification: three of them, or a first one that is an absolute path.
/// Anything else is a default run, reported as not run.
fn qualification_args() -> (Vec<std::ffi::OsString>, bool) {
    let args: Vec<_> = std::env::args_os()
        .skip(1)
        .filter(|arg| !arg.as_encoded_bytes().starts_with(b"--"))
        .collect();
    let requested = args.len() == 3
        || args
            .first()
            .is_some_and(|arg| std::path::Path::new(arg).is_absolute());
    (args, requested)
}

/// Run the shared original-image fixture with an explicit Rust authority factory.
pub fn main(factory: claude_code_alab::supervise::image::HostFactory) {
    #[cfg(all(debug_assertions, any(target_os = "macos", target_os = "linux")))]
    if let Err(error) = native::main(factory) {
        eprintln!("image qualification FAILED: {error}");
        std::process::exit(101);
    }
    #[cfg(not(all(debug_assertions, any(target_os = "macos", target_os = "linux"))))]
    {
        let _ = factory;
        // A default test run asks for no qualification, so it is reported as
        // not run, by the same rule a debug run uses. A request for one is
        // refused.
        if qualification_args().1 {
            eprintln!("image qualification requires a supported debug Unix build");
            std::process::exit(2);
        }
        println!(
            "NOT RUN: different-image qualification requires a supported debug Unix build; no execution proof claimed"
        );
    }
}
