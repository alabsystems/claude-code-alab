// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Process-isolated controls for the signal-transfer foundation, which the
//! own-image move of a default launch outside aterm uses.
//!
//! This executable only execs itself IN PLACE. It never copies an image, starts
//! a provider, or authenticates a production adoption entry. Its bounded test
//! payload supplies facts which each isolated fixture checks against the kernel.

#[cfg(any(target_os = "macos", target_os = "linux"))]
mod native {
    use claude_code_alab::procs::{
        self, Birth, ChildIdentity, ChildState, Executable, ProcessIdentity,
    };
    use claude_code_alab::sys::{self, DescriptorIdentity, SignalPipe, SignalSnapshot};
    use claude_code_alab::transfer_capsule::{self as capsule, AnonymousData, Decoder, Encoder};
    use std::collections::BTreeSet;
    use std::ffi::OsString;
    use std::fs::File;
    use std::io;
    use std::os::fd::{AsFd, AsRawFd, BorrowedFd, FromRawFd, IntoRawFd, OwnedFd, RawFd};
    use std::os::unix::fs::{FileTypeExt, MetadataExt};
    use std::os::unix::process::CommandExt;
    use std::process::{Child, Command, Stdio};
    use std::time::{Duration, Instant};

    const HANG: Duration = Duration::from_secs(60);
    const POLL: Duration = Duration::from_millis(10);
    const BEFORE: &[u8] = b"transfer-cat-ran-before-capture\n";
    const AFTER: &[u8] = b"same-cat-ran-after-transfer\n";

    fn invalid(why: &str) -> io::Error {
        io::Error::new(io::ErrorKind::InvalidData, why)
    }

    fn require(ok: bool, why: &str) -> io::Result<()> {
        if ok { Ok(()) } else { Err(invalid(why)) }
    }

    fn until<T>(phase: &str, mut get: impl FnMut() -> io::Result<Option<T>>) -> io::Result<T> {
        eprintln!("transfer wait: {phase}");
        let deadline = Instant::now() + HANG;
        loop {
            if let Some(value) = get()? {
                return Ok(value);
            }
            if Instant::now() >= deadline {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    format!("60s hang detector: {phase}"),
                ));
            }
            std::thread::sleep(POLL);
        }
    }

    fn flags(fd: RawFd, command: libc::c_int) -> io::Result<libc::c_int> {
        // SAFETY: these integer-only fcntl queries validate the descriptor.
        let value = unsafe { libc::fcntl(fd, command) };
        if value < 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(value)
        }
    }

    fn set_flags(fd: RawFd, command: libc::c_int, value: libc::c_int) -> io::Result<()> {
        // SAFETY: fcntl validates the descriptor; the selected operation takes integer flags.
        if unsafe { libc::fcntl(fd, command, value) } < 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(())
        }
    }

    fn metadata(fd: RawFd) -> io::Result<std::fs::Metadata> {
        // SAFETY: fcntl validates fd and gives this function one new owned duplicate.
        let copy = unsafe { libc::fcntl(fd, libc::F_DUPFD_CLOEXEC, 3) };
        if copy < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: copy is the unique descriptor just allocated above, not the inherited fd.
        let file = File::from(unsafe { OwnedFd::from_raw_fd(copy) });
        file.metadata()
    }

    fn identity(fd: RawFd) -> io::Result<DescriptorIdentity> {
        let meta = metadata(fd)?;
        require(
            meta.file_type().is_fifo(),
            "an inherited pipe role is not a pipe",
        )?;
        Ok(DescriptorIdentity {
            device: meta.dev(),
            inode: meta.ino(),
            rdev: meta.rdev(),
        })
    }

    fn data_identity(fd: RawFd) -> io::Result<DescriptorIdentity> {
        let meta = metadata(fd)?;
        require(meta.is_file(), "capsule role is not regular data")?;
        Ok(DescriptorIdentity {
            device: meta.dev(),
            inode: meta.ino(),
            rdev: meta.rdev(),
        })
    }

    fn mask() -> io::Result<libc::sigset_t> {
        // SAFETY: zero is valid initial sigset storage; successful sigprocmask fills it.
        let mut set = unsafe { std::mem::zeroed() };
        // SAFETY: a null new set only queries the mask into this writable sigset_t.
        if unsafe { libc::sigprocmask(libc::SIG_BLOCK, std::ptr::null(), &raw mut set) } != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(set)
    }

    fn members(set: &libc::sigset_t) -> Vec<i32> {
        // Linux and Darwin signal numbers fit within this bound. Invalid numbers
        // return -1 and are excluded; no native struct padding is compared.
        (1..=128)
            .filter(|&sig| {
                // SAFETY: set is initialized; sigismember validates each signal number.
                unsafe { libc::sigismember(set, sig) == 1 }
            })
            .collect()
    }

    fn pending(sig: i32) -> io::Result<bool> {
        // SAFETY: zero initializes native sigset_t storage which sigpending fills.
        let mut set = unsafe { std::mem::zeroed() };
        // SAFETY: set is writable native signal-set storage.
        if unsafe { libc::sigpending(&raw mut set) } != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(members(&set).contains(&sig))
    }

    fn disposition(sig: i32) -> io::Result<libc::sighandler_t> {
        // SAFETY: zero initializes sigaction output storage, which sigaction fills.
        let mut action: libc::sigaction = unsafe { std::mem::zeroed() };
        // SAFETY: this queries a valid signal into writable native storage.
        if unsafe { libc::sigaction(sig, std::ptr::null(), &raw mut action) } != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(action.sa_sigaction)
    }

    fn set_disposition(sig: i32, handler: libc::sighandler_t) -> io::Result<()> {
        // SAFETY: sigaction is scalar storage; its mask is explicitly initialized.
        let mut action: libc::sigaction = unsafe { std::mem::zeroed() };
        action.sa_sigaction = handler;
        // SAFETY: action's mask is writable native signal storage.
        if unsafe { libc::sigemptyset(&raw mut action.sa_mask) } != 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: action is initialized; each caller names a valid managed signal.
        if unsafe { libc::sigaction(sig, &raw const action, std::ptr::null_mut()) } != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    enum Case {
        Queued,
        Ignored,
        ExecError,
    }

    impl Case {
        const ALL: [Self; 3] = [Self::Queued, Self::Ignored, Self::ExecError];
        fn name(self) -> &'static str {
            match self {
                Self::Queued => "queued-pending",
                Self::Ignored => "inherited-ignored",
                Self::ExecError => "exec-error",
            }
        }
        fn code(self) -> u8 {
            match self {
                Self::Queued => 1,
                Self::Ignored => 2,
                Self::ExecError => 3,
            }
        }
        fn decode(value: u8) -> io::Result<Self> {
            Self::ALL
                .into_iter()
                .find(|case| case.code() == value)
                .ok_or_else(|| invalid("unknown fixture case"))
        }
        fn ignored(self, sig: i32) -> bool {
            self == Self::Ignored
                && [libc::SIGINT, libc::SIGQUIT, libc::SIGHUP, libc::SIGTERM].contains(&sig)
        }
    }

    fn configure(case: Case) -> io::Result<()> {
        sys::ensure_single_threaded()?;
        let mut set = mask()?;
        for &sig in sys::HANDLED {
            let ignored = case.ignored(sig) || (case == Case::Ignored && sig == libc::SIGCHLD);
            set_disposition(
                sig,
                if ignored {
                    libc::SIG_IGN
                } else {
                    libc::SIG_DFL
                },
            )?;
            // SAFETY: set is initialized and each managed signal is valid.
            if unsafe { libc::sigdelset(&raw mut set, sig) } != 0 {
                return Err(io::Error::last_os_error());
            }
        }
        // Keep a nonzero original HANDLED mask and an unmanaged blocked bit.
        // INT and TERM remain unblocked for the queued/pending delivery controls.
        for sig in [libc::SIGQUIT, libc::SIGUSR1] {
            // SAFETY: each signal is valid and set is initialized native storage.
            if unsafe { libc::sigaddset(&raw mut set, sig) } != 0 {
                return Err(io::Error::last_os_error());
            }
        }
        // SAFETY: the initialized mask retains every other inherited membership.
        if unsafe { libc::sigprocmask(libc::SIG_SETMASK, &raw const set, std::ptr::null_mut()) }
            != 0
        {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    #[derive(Clone, Copy)]
    struct End {
        fd: RawFd,
        object: DescriptorIdentity,
        flags: i32,
        status: i32,
    }

    impl End {
        fn capture(fd: RawFd) -> io::Result<Self> {
            Ok(Self {
                fd,
                object: identity(fd)?,
                flags: flags(fd, libc::F_GETFD)?,
                status: flags(fd, libc::F_GETFL)?,
            })
        }
        fn validate(self, direction: i32, inherited: bool) -> io::Result<()> {
            require(
                self.fd >= 3 && self.flags & libc::FD_CLOEXEC != 0,
                "invalid pipe role or original inheritance flags",
            )?;
            require(identity(self.fd)? == self.object, "pipe identity changed")?;
            require(
                flags(self.fd, libc::F_GETFL)? == self.status
                    && self.status & libc::O_ACCMODE == direction
                    && self.status & libc::O_NONBLOCK != 0,
                "pipe access or status changed",
            )?;
            let expected = if inherited {
                self.flags & !libc::FD_CLOEXEC
            } else {
                self.flags
            };
            require(
                flags(self.fd, libc::F_GETFD)? == expected,
                "pipe descriptor flags changed",
            )
        }
    }

    /// Waits only inspect events. The single normal wait happens after Unreaped.
    struct Cat {
        input: Option<File>,
        output: File,
        original: Option<Child>,
        child: Option<ChildIdentity>,
        reaped: bool,
    }

    impl Cat {
        fn start() -> io::Result<Self> {
            let mut child = Command::new("/bin/cat")
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::inherit())
                .spawn()?;
            let input = child
                .stdin
                .take()
                .ok_or_else(|| invalid("cat stdin absent"))?;
            let output = child
                .stdout
                .take()
                .ok_or_else(|| invalid("cat stdout absent"))?;
            // SAFETY: into_raw_fd relinquishes each distinct owned child pipe once.
            let input = unsafe { File::from_raw_fd(input.into_raw_fd()) };
            // SAFETY: stdout relinquishes its independently owned read endpoint once.
            let output = unsafe { File::from_raw_fd(output.into_raw_fd()) };
            let mut cat = Self {
                input: Some(input),
                output,
                original: Some(child),
                child: None,
                reaped: false,
            };
            for fd in [cat.input_fd(), cat.output.as_raw_fd()] {
                set_flags(
                    fd,
                    libc::F_SETFD,
                    flags(fd, libc::F_GETFD)? | libc::FD_CLOEXEC,
                )?;
                set_flags(
                    fd,
                    libc::F_SETFL,
                    flags(fd, libc::F_GETFL)? | libc::O_NONBLOCK,
                )?;
            }
            cat.echo(BEFORE, "cat output proof before identity capture")?;
            let pid = cat.original.as_ref().expect("original cat").id();
            cat.child = Some(
                procs::capture_child(pid)
                    .ok_or_else(|| invalid("cannot capture the cat that printed its proof"))?,
            );
            Ok(cat)
        }
        fn input_fd(&self) -> RawFd {
            self.input.as_ref().expect("live cat input").as_raw_fd()
        }
        fn live(&self) -> io::Result<()> {
            let child = self
                .child
                .as_ref()
                .ok_or_else(|| invalid("cat identity absent"))?;
            let observation = procs::observe_child(child)
                .ok_or_else(|| invalid("same owned cat cannot be proved"))?;
            require(
                observation.state == ChildState::Live
                    && observation.pgid == Some(child.pgid)
                    && observation.session == Some(child.session),
                "cat is not the same live session",
            )
        }
        fn echo(&mut self, bytes: &[u8], phase: &str) -> io::Result<()> {
            let mut sent = 0;
            until(&format!("{phase}: write"), || {
                if sent == bytes.len() {
                    return Ok(Some(()));
                }
                // SAFETY: the live owned pipe and remaining initialized bytes outlive write.
                let n = unsafe {
                    libc::write(
                        self.input_fd(),
                        bytes[sent..].as_ptr().cast(),
                        bytes.len() - sent,
                    )
                };
                match usize::try_from(n) {
                    Ok(0) => return Err(invalid("cat input closed before proof")),
                    Ok(n) => sent += n,
                    Err(_) => {
                        let e = io::Error::last_os_error();
                        if ![io::ErrorKind::WouldBlock, io::ErrorKind::Interrupted]
                            .contains(&e.kind())
                        {
                            return Err(e);
                        }
                    }
                }
                Ok((sent == bytes.len()).then_some(()))
            })?;
            let mut got = vec![0; bytes.len()];
            let mut read = 0;
            until(&format!("{phase}: read"), || {
                if read == got.len() {
                    return Ok(Some(()));
                }
                // SAFETY: the live owned read pipe and remaining writable buffer outlive read.
                let n = unsafe {
                    libc::read(
                        self.output.as_raw_fd(),
                        got[read..].as_mut_ptr().cast(),
                        got.len() - read,
                    )
                };
                match usize::try_from(n) {
                    Ok(0) => return Err(invalid("cat output closed before proof")),
                    Ok(n) => read += n,
                    Err(_) => {
                        let e = io::Error::last_os_error();
                        if ![io::ErrorKind::WouldBlock, io::ErrorKind::Interrupted]
                            .contains(&e.kind())
                        {
                            return Err(e);
                        }
                    }
                }
                Ok((read == got.len()).then_some(()))
            })?;
            require(got == bytes, "cat did not echo its exact output proof")
        }
        fn finish(&mut self) -> io::Result<()> {
            self.live()?;
            drop(self.input.take());
            self.reap("cat EOF remains unreaped for normal wait", 0)
        }
        fn reap(&mut self, phase: &str, expected: i32) -> io::Result<()> {
            let child = self
                .child
                .as_ref()
                .ok_or_else(|| invalid("cat identity absent before reap"))?;
            until(phase, || {
                Ok(procs::observe_child(child).filter(|o| o.state == ChildState::Unreaped))
            })?;
            // Only this normal supervisor wait consumes the observed terminal event.
            let event =
                sys::wait_child(i32::try_from(child.pid).map_err(|_| invalid("invalid cat pid"))?);
            self.reaped = matches!(
                event,
                Some(sys::ChildEvent::Exited(_) | sys::ChildEvent::Signaled(_))
            );
            require(
                event == Some(sys::ChildEvent::Exited(expected)),
                "normal wait did not receive cat's original terminal event",
            )
        }
    }

    impl Drop for Cat {
        fn drop(&mut self) {
            if self.reaped {
                return;
            }
            // The original Child still pins its unreaped pid before capture. Once
            // captured/adopted, fresh wait ownership is required before signaling.
            let pid = if let Some(child) = &self.child {
                if procs::observe_child(child).is_none() {
                    eprintln!(
                        "cat cleanup retains unknown ownership; outer owned group will close the fixture"
                    );
                    return;
                }
                child.pid
            } else if let Some(original) = &self.original {
                original.id()
            } else {
                return;
            };
            if let Ok(pid) = i32::try_from(pid) {
                if terminal_pending(pid).is_err() {
                    eprintln!("cat cleanup retains a pid whose wait ownership is unknown");
                    return;
                }
                let _ = sys::kill(pid, libc::SIGKILL);
                if terminal(pid, "cat panic cleanup").is_ok() {
                    self.reaped = matches!(
                        sys::wait_child(pid),
                        Some(sys::ChildEvent::Exited(_) | sys::ChildEvent::Signaled(_))
                    );
                }
            }
        }
    }

    fn terminal(pid: i32, phase: &str) -> io::Result<()> {
        until(phase, || match terminal_pending(pid) {
            Ok(ready) => Ok(ready.then_some(())),
            Err(e) => {
                if e.kind() == io::ErrorKind::Interrupted {
                    Ok(None)
                } else {
                    Err(e)
                }
            }
        })
    }

    fn terminal_pending(pid: i32) -> io::Result<bool> {
        require(pid > 1, "refusing a non-child wait target")?;
        // SAFETY: zero is valid siginfo_t output storage. WNOWAIT preserves the
        // sole normal wait's event and WNOHANG never waits for a live child.
        let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
        // SAFETY: exact positive child pid, writable output, non-consuming/no-block flags.
        let rc = unsafe {
            libc::waitid(
                libc::P_PID,
                pid as libc::id_t,
                &raw mut info,
                libc::WEXITED | libc::WNOWAIT | libc::WNOHANG,
            )
        };
        if rc != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(info.si_signo == libc::SIGCHLD)
    }

    fn put_identity(e: &mut Encoder, value: DescriptorIdentity) -> io::Result<()> {
        e.u64(value.device)?;
        e.u64(value.inode)?;
        e.u64(value.rdev)
    }
    fn get_identity(d: &mut Decoder<'_>) -> io::Result<DescriptorIdentity> {
        Ok(DescriptorIdentity {
            device: d.u64()?,
            inode: d.u64()?,
            rdev: d.u64()?,
        })
    }
    fn put_process(e: &mut Encoder, p: &ProcessIdentity) -> io::Result<()> {
        for value in [p.pid, p.parent, p.pgid, p.session] {
            e.u32(value)?;
        }
        match p.birth {
            Birth::MacOs {
                seconds,
                microseconds,
            } => {
                e.u8(1)?;
                e.u64(seconds)?;
                e.u32(microseconds)?;
            }
            Birth::Linux(ticks) => {
                e.u8(2)?;
                e.u64(ticks)?;
            }
        }
        e.os_string(p.executable.path.as_os_str())?;
        e.u64(p.executable.device)?;
        e.u64(p.executable.inode)
    }
    fn get_process(d: &mut Decoder<'_>) -> io::Result<ProcessIdentity> {
        let (pid, parent, pgid, session) = (d.u32()?, d.u32()?, d.u32()?, d.u32()?);
        let birth = match d.u8()? {
            1 => Birth::MacOs {
                seconds: d.u64()?,
                microseconds: d.u32()?,
            },
            2 => Birth::Linux(d.u64()?),
            _ => return Err(invalid("fixture birth kind")),
        };
        Ok(ProcessIdentity {
            pid,
            parent,
            pgid,
            session,
            birth,
            executable: Executable {
                path: d.os_string(16 * 1024)?.into(),
                device: d.u64()?,
                inode: d.u64()?,
            },
        })
    }
    fn put_end(e: &mut Encoder, end: End) -> io::Result<()> {
        e.u32(end.fd as u32)?;
        put_identity(e, end.object)?;
        e.u32(end.flags as u32)?;
        e.u32(end.status as u32)
    }
    fn get_end(d: &mut Decoder<'_>) -> io::Result<End> {
        Ok(End {
            fd: number(d.u32()?)?,
            object: get_identity(d)?,
            flags: number(d.u32()?)?,
            status: number(d.u32()?)?,
        })
    }
    fn number(value: u32) -> io::Result<i32> {
        i32::try_from(value).map_err(|_| invalid("fixture integer exceeds native signed range"))
    }

    struct Transfer {
        case: Case,
        data_identity: DescriptorIdentity,
        data_flags: i32,
        process: ProcessIdentity,
        child: ChildIdentity,
        signals: SignalSnapshot,
        pipe_flags: [i32; 2],
        input: End,
        output: End,
        mask: Vec<i32>,
    }

    impl Transfer {
        fn encode(&self) -> io::Result<Vec<u8>> {
            let mut e = Encoder::new();
            e.u8(self.case.code())?;
            put_identity(&mut e, self.data_identity)?;
            e.u32(self.data_flags as u32)?;
            put_process(&mut e, &self.process)?;
            put_process(
                &mut e,
                &ProcessIdentity {
                    pid: self.child.pid,
                    parent: self.child.parent,
                    pgid: self.child.pgid,
                    session: self.child.session,
                    birth: self.child.birth.clone(),
                    executable: self.child.executable.clone(),
                },
            )?;
            e.u32(self.signals.read_fd as u32)?;
            e.u32(self.signals.write_fd as u32)?;
            put_identity(&mut e, self.signals.read_identity)?;
            put_identity(&mut e, self.signals.write_identity)?;
            e.u8(self.signals.prior_mask)?;
            e.u8(self.signals.ignored)?;
            for flags in self.pipe_flags {
                e.u32(flags as u32)?;
            }
            put_end(&mut e, self.input)?;
            put_end(&mut e, self.output)?;
            e.count(self.mask.len(), 128)?;
            for sig in &self.mask {
                e.u32(*sig as u32)?;
            }
            Ok(e.finish())
        }
        fn decode(bytes: &[u8]) -> io::Result<Self> {
            let mut d = Decoder::new(bytes)?;
            let case = Case::decode(d.u8()?)?;
            let data_identity = get_identity(&mut d)?;
            let data_flags = number(d.u32()?)?;
            let process = get_process(&mut d)?;
            let child = get_process(&mut d)?;
            let signals = SignalSnapshot {
                read_fd: number(d.u32()?)?,
                write_fd: number(d.u32()?)?,
                read_identity: get_identity(&mut d)?,
                write_identity: get_identity(&mut d)?,
                prior_mask: d.u8()?,
                ignored: d.u8()?,
            };
            let pipe_flags = [number(d.u32()?)?, number(d.u32()?)?];
            let (input, output) = (get_end(&mut d)?, get_end(&mut d)?);
            let mut mask = Vec::new();
            for _ in 0..d.count(128)? {
                mask.push(number(d.u32()?)?);
            }
            d.finish()?;
            Ok(Self {
                case,
                data_identity,
                data_flags,
                process,
                child: ChildIdentity {
                    pid: child.pid,
                    parent: child.parent,
                    pgid: child.pgid,
                    session: child.session,
                    birth: child.birth,
                    executable: child.executable,
                },
                signals,
                pipe_flags,
                input,
                output,
                mask,
            })
        }
        fn check_mask(&self, blocked: bool) -> io::Result<()> {
            require(
                self.mask.contains(&libc::SIGQUIT)
                    && !self.mask.contains(&libc::SIGINT)
                    && !self.mask.contains(&libc::SIGTERM),
                "nonzero original managed-mask control missing",
            )?;
            let mut expected: BTreeSet<_> = self.mask.iter().copied().collect();
            if blocked {
                expected.extend(sys::HANDLED.iter().copied());
            }
            require(
                members(&mask()?).into_iter().collect::<BTreeSet<_>>() == expected,
                "managed or unmanaged signal membership changed",
            )?;
            require(
                expected.contains(&libc::SIGUSR1),
                "unmanaged SIGUSR1 control missing",
            )
        }
        fn check_pipe(&self) -> io::Result<()> {
            for (fd, object, before) in [
                (
                    self.signals.read_fd,
                    self.signals.read_identity,
                    self.pipe_flags[0],
                ),
                (
                    self.signals.write_fd,
                    self.signals.write_identity,
                    self.pipe_flags[1],
                ),
            ] {
                require(
                    identity(fd)? == object,
                    "original signal endpoint identity changed",
                )?;
                require(
                    before & libc::FD_CLOEXEC != 0 && flags(fd, libc::F_GETFD)? == before,
                    "original signal descriptor flags not restored",
                )?;
            }
            Ok(())
        }
    }

    fn expect_signals(pipe: &SignalPipe, expected: &[i32], phase: &str) -> io::Result<()> {
        let mut seen = Vec::new();
        until(phase, || {
            seen.extend(pipe.wait(0));
            Ok(expected.iter().all(|sig| seen.contains(sig)).then_some(()))
        })
    }

    fn queued(fd: RawFd) -> io::Result<bool> {
        let mut pfd = libc::pollfd {
            fd,
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: this is one initialized owned-pipe poll record; zero never waits.
        let n = unsafe { libc::poll(&raw mut pfd, 1, 0) };
        if n < 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(pfd.revents & libc::POLLIN != 0)
        }
    }

    fn fail_blocked(phase: &str, error: &io::Error) -> ! {
        // Returning/unwinding would run an unmasking guard Drop. Exit keeps
        // signals blocked until this failed process dies; the outer unreaped
        // leader owner then cleans only its own fixture process group.
        eprintln!("{phase}; signals stay blocked: {error}");
        std::process::exit(1)
    }

    fn begin(case: Case) -> io::Result<()> {
        configure(case)?;
        let mut pipe = SignalPipe::install()?;
        for &sig in sys::HANDLED {
            require(
                (disposition(sig)? == libc::SIG_IGN) == case.ignored(sig),
                "fresh inherited-ignore policy differs",
            )?;
        }
        let mut cat = Cat::start()?;
        let process = procs::capture_process(std::process::id())
            .ok_or_else(|| invalid("cannot capture fixture identity"))?;
        require(
            process.pgid == process.pid,
            "fixture does not own its process group",
        )?;
        // Obtain the public descriptor description without reaching into private
        // pipe fields. This preliminary guard is rolled back before any signal.
        let signals = {
            let mut guard = pipe.prepare_exec()?;
            let snapshot = guard.snapshot();
            guard.rollback()?;
            snapshot
        };
        let pipe_flags = [
            flags(signals.read_fd, libc::F_GETFD)?,
            flags(signals.write_fd, libc::F_GETFD)?,
        ];
        let original_mask = members(&mask()?);
        let input = End::capture(cat.input_fd())?;
        let output = End::capture(cat.output.as_raw_fd())?;
        input.validate(libc::O_WRONLY, false)?;
        output.validate(libc::O_RDONLY, false)?;
        if case != Case::Ignored {
            require(
                sys::kill(process.pid as i32, libc::SIGINT),
                "cannot queue before-exec signal",
            )?;
            require(
                queued(signals.read_fd)?,
                "handler did not queue the before-exec signal",
            )?;
        }
        let mut signal_exec = pipe.prepare_exec()?;
        let mut data = AnonymousData::create()?;
        let state = Transfer {
            case,
            data_identity: data_identity(data.as_raw_fd())?,
            data_flags: flags(data.as_raw_fd(), libc::F_GETFD)?,
            process,
            child: cat.child.as_ref().expect("captured cat").clone(),
            signals: signal_exec.snapshot(),
            pipe_flags,
            input,
            output,
            mask: original_mask,
        };
        state.check_mask(true)?;
        if case != Case::Ignored {
            require(
                queued(state.signals.read_fd)?,
                "prepare_exec discarded queued bytes",
            )?;
            require(
                sys::kill(state.process.pid as i32, libc::SIGTERM),
                "cannot pend blocked signal",
            )?;
            require(
                pending(libc::SIGTERM)?,
                "kernel-pending signal missing before exec",
            )?;
        }
        let image = capsule::Image {
            executable: state.process.executable.clone(),
            version: claude_code_alab::cli::VERSION.to_string(),
        };
        let header = capsule::Header {
            phase: capsule::Phase::Forward,
            forward_attempt: 1,
            rollback_attempt: 0,
            self_pid: state.process.pid,
            self_birth: state.process.birth.clone(),
            old_image: image.clone(),
            new_image: image,
            fds: capsule::FdRoles {
                capsule: data.as_raw_fd(),
                signal_read: state.signals.read_fd,
                signal_write: state.signals.write_fd,
                authorities: Vec::new(),
                retention_gates: Vec::new(),
            },
        };
        data.write_capsule(&header, &state.encode()?)?;
        let mut data_exec = sys::inherit_for_one_exec(data.as_fd())
            .unwrap_or_else(|error| fail_blocked("capsule inheritance failed", &error));
        let mut input_exec = sys::inherit_for_one_exec(cat.input.as_ref().expect("input").as_fd())
            .unwrap_or_else(|error| fail_blocked("cat input inheritance failed", &error));
        let mut output_exec = sys::inherit_for_one_exec(cat.output.as_fd())
            .unwrap_or_else(|error| fail_blocked("cat output inheritance failed", &error));
        let mut command = Command::new(&state.process.executable.path);
        command
            .arg("--transfer-adopt")
            .arg(data_exec.as_raw_fd().to_string());
        if case == Case::ExecError {
            // A child path beneath this anonymous regular file is definitively
            // not an executable, without creating, replacing or removing a path.
            command = Command::new(format!(
                "/dev/fd/{}/not-an-executable",
                data_exec.as_raw_fd()
            ));
        }
        let error = command.exec();
        // Do not let guard Drops unmask after a failed borrowed-FD restoration.
        // The outer unreaped group owner cleans this failed fixture instead.
        for result in [
            output_exec.restore(),
            input_exec.restore(),
            data_exec.restore(),
        ] {
            if let Err(error) = result {
                fail_blocked("borrowed descriptor rollback failed", &error);
            }
        }
        let ready = (|| {
            state.input.validate(libc::O_WRONLY, false)?;
            state.output.validate(libc::O_RDONLY, false)?;
            require(
                data_identity(data.as_raw_fd())? == state.data_identity
                    && flags(data.as_raw_fd(), libc::F_GETFD)? == state.data_flags,
                "capsule identity or exact flags changed on returned exec",
            )?;
            state.check_mask(true)
        })();
        if let Err(error) = ready {
            fail_blocked("pre-unmask exec-error validation failed", &error);
        }
        signal_exec.rollback()?;
        drop(output_exec);
        drop(input_exec);
        drop(data_exec);
        drop(signal_exec);
        state.check_pipe()?;
        state.check_mask(false)?;
        require(
            procs::capture_process(std::process::id()).as_ref() == Some(&state.process),
            "returned exec changed fixture process identity",
        )?;
        require(
            case == Case::ExecError
                && matches!(error.raw_os_error(), Some(libc::ENOTDIR | libc::ENOENT)),
            &format!("real self-exec unexpectedly returned: {error}"),
        )?;
        expect_signals(
            &pipe,
            &[libc::SIGINT, libc::SIGTERM],
            "exec-error rollback signal delivery",
        )?;
        cat.live()?;
        cat.echo(AFTER, "same cat after returned exec syscall")?;
        cat.finish()
    }

    fn adopt(raw: i32) -> io::Result<()> {
        sys::ensure_single_threaded()?;
        require(raw >= 3, "invalid fixture capsule fd")?;
        flags(raw, libc::F_GETFD)?;
        // SAFETY: fcntl established this inherited descriptor is open; the
        // single-threaded fixture neither closes nor transfers it while borrowed.
        let decoded = capsule::read_inherited(unsafe { BorrowedFd::borrow_raw(raw) })?;
        let state = Transfer::decode(&decoded.payload)?;
        require(
            state.data_flags & libc::FD_CLOEXEC != 0
                && data_identity(raw)? == state.data_identity
                && flags(raw, libc::F_GETFD)? == state.data_flags,
            "original capsule identity or flags changed across exec",
        )?;
        let header = &decoded.header;
        require(
            state.case != Case::ExecError
                && header.phase == capsule::Phase::Forward
                && header.self_pid == state.process.pid
                && header.self_birth == state.process.birth
                && header.fds.signal_read == state.signals.read_fd
                && header.fds.signal_write == state.signals.write_fd
                && header.fds.authorities.is_empty()
                && header.fds.retention_gates.is_empty(),
            "fixture capsule and payload roles differ",
        )?;
        require(
            header.old_image == header.new_image
                && header.new_image.executable == state.process.executable
                && header.new_image.version == claude_code_alab::cli::VERSION,
            "fixture image binding differs",
        )?;
        let actual = procs::capture_process(std::process::id())
            .ok_or_else(|| invalid("cannot capture adopted fixture"))?;
        require(
            actual == state.process,
            "self-exec changed PID, birth, parent, group, session or mapped image",
        )?;
        let roles = [
            raw,
            state.signals.read_fd,
            state.signals.write_fd,
            state.input.fd,
            state.output.fd,
        ];
        require(
            roles.iter().all(|fd| *fd >= 3)
                && roles.into_iter().collect::<BTreeSet<_>>().len() == roles.len(),
            "fixture descriptor roles alias",
        )?;
        state.check_mask(true)?;
        state.input.validate(libc::O_WRONLY, true)?;
        state.output.validate(libc::O_RDONLY, true)?;
        require(
            procs::observe_child(&state.child).is_some_and(|o| o.state == ChildState::Live),
            "inherited cat is not the same owned live child",
        )?;
        if state.case != Case::Ignored {
            require(
                queued(state.signals.read_fd)?,
                "queued pipe byte lost during exec",
            )?;
            require(
                pending(libc::SIGTERM)?,
                "kernel-pending signal lost during exec",
            )?;
        }
        set_flags(state.input.fd, libc::F_SETFD, state.input.flags)?;
        set_flags(state.output.fd, libc::F_SETFD, state.output.flags)?;
        // SAFETY: the complete validated, unique role graph is from this same
        // fixture's previous image. No Rust owner exists in this fresh image.
        let input = unsafe { File::from_raw_fd(state.input.fd) };
        // SAFETY: independently validated and unique cat output role.
        let output = unsafe { File::from_raw_fd(state.output.fd) };
        // SAFETY: the checked capsule role is distinct and still caller-owned;
        // read_inherited borrowed it and restored CLOEXEC without taking ownership.
        let _data = unsafe { OwnedFd::from_raw_fd(raw) };
        let mut cat = Cat {
            input: Some(input),
            output,
            original: None,
            child: Some(state.child.clone()),
            reaped: false,
        };
        // SAFETY: all endpoints belong to this same single-threaded fixture,
        // inherited mask/dispositions and pipe identities are checked by attachment.
        let (pipe, mut block) = unsafe { SignalPipe::attach_inherited(&state.signals) }?;
        let ready = (|| {
            state.check_pipe()?;
            state.input.validate(libc::O_WRONLY, false)?;
            state.output.validate(libc::O_RDONLY, false)?;
            require(
                flags(raw, libc::F_GETFD)? & libc::FD_CLOEXEC != 0,
                "capsule CLOEXEC not restored",
            )?;
            state.check_mask(true)?;
            for &sig in sys::HANDLED {
                require(
                    (disposition(sig)? == libc::SIG_IGN) == state.case.ignored(sig),
                    "adopted inherited-ignore policy differs",
                )?;
            }
            Ok(())
        })();
        if let Err(error) = ready {
            fail_blocked("pre-unmask adoption validation failed", &error);
        }
        block.restore()?;
        state.check_mask(false)?;
        if state.case == Case::Ignored {
            require(
                sys::kill(state.process.pid as i32, libc::SIGTERM),
                "ignored TERM control failed",
            )?;
            require(
                !pipe.wait(0).contains(&libc::SIGTERM),
                "inherited ignored TERM became caught",
            )?;
        } else {
            expect_signals(
                &pipe,
                &[libc::SIGINT, libc::SIGTERM],
                "queued and kernel-pending signal delivery after exec",
            )?;
        }
        cat.live()?;
        cat.echo(AFTER, "same cat after successful exec")?;
        cat.finish()?;
        expect_signals(
            &pipe,
            &[libc::SIGCHLD],
            "caught SIGCHLD retains child terminal event",
        )
    }

    struct Group {
        child: Child,
        reaped: bool,
    }

    impl Group {
        fn run(case: Case) -> io::Result<()> {
            let child = Command::new(std::env::current_exe()?)
                .arg("--transfer-begin")
                .arg(case.code().to_string())
                .process_group(0)
                .spawn()?;
            let mut group = Self {
                child,
                reaped: false,
            };
            let pid = i32::try_from(group.child.id())
                .map_err(|_| invalid("invalid fixture child pid"))?;
            eprintln!("transfer fixture {} owns group={pid}", case.name());
            terminal(pid, &format!("{} fixture process", case.name()))?;
            // The unreaped leader still pins the group's numeric identity.
            group.kill();
            let status = group.child.wait()?;
            group.reaped = true;
            require(
                status.success(),
                &format!("{} fixture exited {status}", case.name()),
            )
        }
        fn kill(&self) {
            if let Ok(pid) = i32::try_from(self.child.id())
                && pid > 1
            {
                if terminal_pending(pid).is_err() {
                    eprintln!("fixture group cleanup retains unknown leader wait ownership");
                    return;
                }
                // SAFETY: process_group(0) created exactly this owned group. The
                // leader has not been reaped, so its identity cannot be reused.
                unsafe { libc::kill(-pid, libc::SIGKILL) };
            }
        }
    }

    impl Drop for Group {
        fn drop(&mut self) {
            if !self.reaped {
                self.kill();
                if let Ok(pid) = i32::try_from(self.child.id())
                    && terminal(pid, "owned fixture group cleanup").is_ok()
                {
                    self.reaped = self.child.wait().is_ok();
                }
            }
        }
    }

    /// In this single-threaded process, with the managed set blocked: a CONT is
    /// made pending and a TSTP then discards it, an ignored HUP is raised, and
    /// a TERM is raised. While the launch mask blocks TERM, a held handoff must
    /// take exactly TSTP, without waiting for the discarded CONT, and leave
    /// TERM pending. Without that launch block it then takes TERM, and nothing
    /// is left.
    fn pending_takes() -> io::Result<()> {
        sys::ensure_single_threaded()?;
        set_disposition(libc::SIGHUP, libc::SIG_IGN)?;
        sys::block_managed()?;
        for sig in [libc::SIGCONT, libc::SIGTSTP, libc::SIGHUP, libc::SIGTERM] {
            // SAFETY: raise only queues this blocked or ignored signal here.
            require(unsafe { libc::raise(sig) } == 0, "cannot raise a signal")?;
        }
        let term = sys::HANDLED
            .iter()
            .position(|&sig| sig == libc::SIGTERM)
            .ok_or_else(|| invalid("TERM is not managed"))?;
        let launch_blocked = 1_u8 << term;
        let taken = sys::take_pending(launch_blocked)?;
        require(
            taken == [libc::SIGTSTP],
            &format!("took {taken:?} with TERM launch-blocked, expected only TSTP"),
        )?;
        let taken = sys::take_pending(0)?;
        require(
            taken == [libc::SIGTERM],
            &format!("took {taken:?}, expected the TERM left pending"),
        )?;
        require(
            sys::take_pending(0)?.is_empty(),
            "a taken signal stayed pending",
        )?;
        println!("pending-takes ok");
        Ok(())
    }

    /// Open a pipe; a signal self-pipe's ends are nonblocking.
    fn open_pipe(nonblocking: bool) -> io::Result<(OwnedFd, OwnedFd)> {
        let mut fds = [0; 2];
        // SAFETY: pipe writes two new descriptors into this two-element array.
        if unsafe { libc::pipe(fds.as_mut_ptr()) } != 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: pipe succeeded, so both descriptors are new and owned only here.
        let ends = unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) };
        if nonblocking {
            for fd in [ends.0.as_raw_fd(), ends.1.as_raw_fd()] {
                set_flags(
                    fd,
                    libc::F_SETFL,
                    flags(fd, libc::F_GETFL)? | libc::O_NONBLOCK,
                )?;
            }
        }
        Ok(ends)
    }

    /// A hold that owns no inherited role drains the self-pipe only once
    /// `inherited_read_end` accepts it as the recorded read end. The recorded
    /// read end is accepted. Each other descriptor fails exactly one check and
    /// must be refused: another pipe's read end, the recorded pipe's write
    /// end, a blocking read end, a closed descriptor, and the recorded read end
    /// placed on standard input.
    fn read_end_checks() -> io::Result<()> {
        let (read, write) = open_pipe(true)?;
        let recorded = sys::descriptor_identity(read.as_raw_fd())?;
        let write_identity = sys::descriptor_identity(write.as_raw_fd())?;
        let check = |read_fd: RawFd, read_identity: DescriptorIdentity| {
            sys::inherited_read_end(&SignalSnapshot {
                read_fd,
                write_fd: write.as_raw_fd(),
                read_identity,
                write_identity,
                prior_mask: 0,
                ignored: 0,
            })
        };
        require(
            check(read.as_raw_fd(), recorded)? == read.as_raw_fd(),
            "the recorded read end was not returned",
        )?;
        let (other, _other_write) = open_pipe(true)?;
        let (blocking, _blocking_write) = open_pipe(false)?;
        let blocking_identity = sys::descriptor_identity(blocking.as_raw_fd())?;
        let (closed, _closed_write) = open_pipe(true)?;
        let closed_identity = sys::descriptor_identity(closed.as_raw_fd())?;
        let closed_fd = closed.as_raw_fd();
        drop(closed);
        // SAFETY: this fixture process's stdin is null and unused; dup2 makes
        // standard input another descriptor of the recorded read end.
        let placed = unsafe { libc::dup2(read.as_raw_fd(), 0) };
        require(placed == 0, "cannot place the read end on standard input")?;
        // Every row is checked, so the report names each accepted descriptor.
        let accepted: Vec<&str> = [
            ("another pipe's read end", other.as_raw_fd(), recorded),
            ("the write end", write.as_raw_fd(), write_identity),
            (
                "a blocking read end",
                blocking.as_raw_fd(),
                blocking_identity,
            ),
            ("a closed descriptor", closed_fd, closed_identity),
            ("the read end on standard input", 0, recorded),
        ]
        .into_iter()
        .filter(|&(_, fd, identity)| check(fd, identity).is_ok())
        .map(|(what, ..)| what)
        .collect();
        require(
            accepted.is_empty(),
            &format!("accepted {}", accepted.join("; ")),
        )?;
        println!("read-end-checks ok");
        Ok(())
    }

    /// Run a fixture `mode` in a fresh process, failing it if it hangs or does
    /// not print exactly `<name> ok`.
    fn run_isolated(mode: &str, name: &str, phase: &str) -> io::Result<()> {
        let mut child = Command::new(std::env::current_exe()?)
            .arg(mode)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .spawn()?;
        let status = until(phase, || child.try_wait());
        let status = match status {
            Ok(status) => status,
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(error);
            }
        };
        let mut out = String::new();
        if let Some(mut stdout) = child.stdout.take() {
            std::io::Read::read_to_string(&mut stdout, &mut out)?;
        }
        require(
            status.success() && out == format!("{name} ok\n"),
            &format!("{name} failed: {status}"),
        )
    }

    pub fn main() -> io::Result<()> {
        let args: Vec<OsString> = std::env::args_os().skip(1).collect();
        match args.as_slice() {
            [mode] if mode == "--pending-takes" => pending_takes(),
            [mode] if mode == "--read-end-checks" => read_end_checks(),
            [mode, code] if mode == "--transfer-begin" => {
                let case = code
                    .to_str()
                    .and_then(|s| s.parse::<u8>().ok())
                    .ok_or_else(|| invalid("invalid begin case"))?;
                begin(Case::decode(case)?)
            }
            [mode, fd] if mode == "--transfer-adopt" => {
                let fd = fd
                    .to_str()
                    .and_then(|s| s.parse().ok())
                    .ok_or_else(|| invalid("invalid adopt descriptor"))?;
                adopt(fd)
            }
            [] => {
                // The outer process, too, must retain its children until its
                // non-consuming wait and group cleanup have finished.
                set_disposition(libc::SIGCHLD, libc::SIG_DFL)?;
                run_isolated(
                    "--pending-takes",
                    "pending-takes",
                    "pending signals are taken without waiting",
                )?;
                println!("PASS transfer pending-takes");
                run_isolated(
                    "--read-end-checks",
                    "read-end-checks",
                    "inherited read-end checks finish",
                )?;
                println!("PASS transfer read-end-checks");
                let mut failed = 0;
                for case in Case::ALL {
                    match Group::run(case) {
                        Ok(()) => println!("PASS transfer {}", case.name()),
                        Err(error) => {
                            eprintln!("FAIL transfer {}: {error}", case.name());
                            failed += 1;
                        }
                    }
                }
                require(
                    failed == 0,
                    &format!("{failed} of 3 transfer fixtures failed"),
                )?;
                println!("3 transfer fixtures passed");
                Ok(())
            }
            _ => Err(invalid(
                "malformed fixture entry; refusing normal-case fallback",
            )),
        }
    }
}

fn main() {
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    if let Err(error) = native::main() {
        eprintln!("transfer fixture: {error}");
        std::process::exit(1);
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    println!(
        "SKIP transfer fixtures: native single-thread/child identity unavailable on this platform"
    );
}
