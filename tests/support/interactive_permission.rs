// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use claude_code_alab::{inner_pty, json, permission::Mode, procs, sys};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read as _, Write as _};
use std::os::fd::{AsRawFd as _, FromRawFd as _, OwnedFd};
use std::os::unix::fs::{MetadataExt as _, OpenOptionsExt as _, PermissionsExt as _};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicI32, Ordering};
use std::time::{Duration, Instant};

const LIMIT: usize = 1 << 20;
const WITHIN: Duration = Duration::from_secs(60);
static REAPED: AtomicI32 = AtomicI32::new(0);

extern "C" fn reap_terminal(_: libc::c_int) {
    let mut status = 0;
    // SAFETY: this isolated negative control deliberately steals its only child's wait.
    let pid = unsafe { libc::waitpid(-1, &raw mut status, libc::WNOHANG) };
    REAPED.store(pid, Ordering::SeqCst);
}

fn require(ok: bool, why: &str) -> io::Result<()> {
    if ok {
        Ok(())
    } else {
        Err(io::Error::other(why))
    }
}
fn quoted(text: &str) -> String {
    let mut out = String::from("\"");
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            c if c.is_control() => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}
fn fresh(path: &Path, bytes: &[u8]) -> io::Result<()> {
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?
        .write_all(bytes)
}

fn monotonic_us() -> io::Result<u64> {
    // SAFETY: writable native monotonic clock output storage.
    let mut value: libc::timespec = unsafe { std::mem::zeroed() };
    // SAFETY: CLOCK_MONOTONIC is valid and value has its exact output layout.
    require(
        // SAFETY: value is writable exact-layout output for this monotonic clock.
        unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &raw mut value) } == 0,
        "monotonic clock",
    )?;
    let seconds = u64::try_from(value.tv_sec).map_err(io::Error::other)?;
    let nanos = u64::try_from(value.tv_nsec).map_err(io::Error::other)?;
    require(nanos < 1_000_000_000, "monotonic clock fraction")?;
    seconds
        .checked_mul(1_000_000)
        .and_then(|s| s.checked_add(nanos / 1000))
        .ok_or_else(|| io::Error::other("monotonic clock bound"))
}

fn wait_fd(fd: i32, events: libc::c_short, until: Instant) -> io::Result<()> {
    loop {
        let remaining = until
            .checked_duration_since(Instant::now())
            .ok_or_else(|| io::Error::new(io::ErrorKind::TimedOut, "fixed hook deadline"))?;
        let mut event = libc::pollfd {
            fd,
            events,
            revents: 0,
        };
        // SAFETY: a caller-owned live fd and initialized writable native poll storage.
        let count = unsafe {
            libc::poll(
                &raw mut event,
                1,
                remaining.as_millis().min(i32::MAX as u128) as i32,
            )
        };
        if count < 0 {
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(error);
        }
        require(
            event.revents & libc::POLLNVAL == 0,
            "hook descriptor invalid",
        )?;
        if count > 0 {
            return Ok(());
        }
    }
}

fn read_exact_until(
    channel: &mut UnixStream,
    mut bytes: &mut [u8],
    until: Instant,
) -> io::Result<()> {
    while !bytes.is_empty() {
        require(Instant::now() < until, "fixed hook body deadline")?;
        match channel.read(bytes) {
            Ok(0) => {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "hook body EOF",
                ));
            }
            Ok(n) => bytes = &mut bytes[n..],
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                wait_fd(channel.as_raw_fd(), libc::POLLIN, until)?
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

fn write_all_until(channel: &mut UnixStream, mut bytes: &[u8], until: Instant) -> io::Result<()> {
    while !bytes.is_empty() {
        require(Instant::now() < until, "fixed hook write deadline")?;
        match channel.write(bytes) {
            Ok(0) => {
                return Err(io::Error::new(
                    io::ErrorKind::WriteZero,
                    "hook writer stopped",
                ));
            }
            Ok(n) => bytes = &bytes[n..],
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                wait_fd(channel.as_raw_fd(), libc::POLLOUT, until)?
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

struct ExitWatch {
    fd: OwnedFd,
    pid: u32,
    ended: bool,
}
impl ExitWatch {
    fn new(native: &procs::ExecIdentity) -> io::Result<Self> {
        require(native.current(), "original hook before exit watch")?;
        // SAFETY: kqueue creates one new descriptor with no Rust owner.
        let fd = unsafe { libc::kqueue() };
        require(fd >= 0, "hook native exit watch creation")?;
        // SAFETY: this newly allocated descriptor has exactly one Rust owner.
        let fd = unsafe { OwnedFd::from_raw_fd(fd) };
        // SAFETY: only the newly owned native watch's descriptor flags change.
        require(
            // SAFETY: fd is this function's newly owned kernel queue descriptor.
            unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_SETFD, libc::FD_CLOEXEC) } == 0,
            "private hook native watch",
        )?;
        let change = libc::kevent {
            ident: native.process().pid as usize,
            filter: libc::EVFILT_PROC,
            flags: libc::EV_ADD | libc::EV_ENABLE | libc::EV_ONESHOT,
            fflags: libc::NOTE_EXIT,
            data: 0,
            udata: std::ptr::null_mut(),
        };
        let zero = libc::timespec {
            tv_sec: 0,
            tv_nsec: 0,
        };
        // SAFETY: initialized single registration and no output list; zero timeout is nonblocking.
        require(
            // SAFETY: change/zero are initialized, with no output list and no wait.
            unsafe {
                libc::kevent(
                    fd.as_raw_fd(),
                    &raw const change,
                    1,
                    std::ptr::null_mut(),
                    0,
                    &raw const zero,
                )
            } == 0,
            "hook native watch registration",
        )?;
        require(
            native.current(),
            "original hook changed across native watch",
        )?;
        Ok(Self {
            fd,
            pid: native.process().pid,
            ended: false,
        })
    }
    fn wait(&mut self, until: Instant) -> io::Result<()> {
        while !self.ended {
            wait_fd(self.fd.as_raw_fd(), libc::POLLIN, until)?;
            // SAFETY: zero initializes writable plain-data native event storage.
            let mut event: libc::kevent = unsafe { std::mem::zeroed() };
            let zero = libc::timespec {
                tv_sec: 0,
                tv_nsec: 0,
            };
            // SAFETY: no changes, one writable output, nonblocking native queue read.
            let count = unsafe {
                libc::kevent(
                    self.fd.as_raw_fd(),
                    std::ptr::null(),
                    0,
                    &raw mut event,
                    1,
                    &raw const zero,
                )
            };
            if count < 0 {
                return Err(io::Error::last_os_error());
            }
            if count == 0 {
                continue;
            }
            require(
                event.ident == self.pid as usize
                    && event.filter == libc::EVFILT_PROC
                    && event.flags & libc::EV_ERROR == 0
                    && event.fflags & libc::NOTE_EXIT != 0,
                "hook native exit event mismatch",
            )?;
            self.ended = true;
        }
        Ok(())
    }
}
fn wait_for_settlement(
    started: Instant,
    mut now: impl FnMut() -> Instant,
    mut poll: impl FnMut() -> io::Result<bool>,
    mut pause: impl FnMut(Duration),
) -> io::Result<()> {
    let until = started + WITHIN;
    loop {
        if now() >= until {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "original child settlement one-minute hang detector",
            ));
        }
        let settled = poll()?;
        let remaining = until
            .checked_duration_since(now())
            .filter(|remaining| !remaining.is_zero())
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::TimedOut,
                    "original child settlement one-minute hang detector",
                )
            })?;
        if settled {
            return Ok(());
        }
        pause(remaining.min(Duration::from_millis(10)));
    }
}

fn settlement_error(child: &sys::ProbeChild, phase: &str, error: io::Error) -> io::Error {
    io::Error::new(
        error.kind(),
        format!(
            "original child settlement {phase}: {error}; pending={} ownership_lost={} native={child:?}",
            child.pending(),
            child.ownership_lost(),
        ),
    )
}

fn settle_child(child: &mut sys::ProbeChild) -> io::Result<()> {
    if !child.pending() {
        return Ok(());
    }
    let started = Instant::now();
    child
        .cancel()
        .map_err(|error| settlement_error(child, "cancellation refused", error))?;
    if child.pending() {
        let _ = writeln!(
            io::stderr(),
            "original child settlement awaits terminal wait: pending={} ownership_lost={} retained_native={child:?}",
            child.pending(),
            child.ownership_lost(),
        );
    }
    let mut polls = 0;
    let result = wait_for_settlement(
        started,
        Instant::now,
        || {
            polls += 1;
            child.try_wait().map(|_| !child.pending())
        },
        std::thread::sleep,
    );
    result.map_err(|error| {
        settlement_error(
            child,
            &format!(
                "wait refused or expired after {polls} protected polls, elapsed_ms={}",
                started.elapsed().as_millis()
            ),
            error,
        )
    })
}

#[derive(Default)]
struct Cleanup {
    outcome: Option<Result<(), (io::ErrorKind, String)>>,
}
impl Cleanup {
    fn once(&mut self, action: impl FnOnce() -> io::Result<()>) -> io::Result<()> {
        if let Some(outcome) = &self.outcome {
            return outcome
                .as_ref()
                .copied()
                .map_err(|(kind, message)| io::Error::new(*kind, message.clone()));
        }
        let result = action();
        self.outcome = Some(
            result
                .as_ref()
                .copied()
                .map_err(|error| (error.kind(), error.to_string())),
        );
        result
    }
}
fn line(until: Instant) -> io::Result<Vec<u8>> {
    let mut out = Vec::new();
    loop {
        require(Instant::now() < until, "qualification handshake deadline")?;
        let mut fd = libc::pollfd {
            fd: 0,
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: this qualifier owns stdin and bounded writable native poll storage.
        let n = unsafe { libc::poll(&raw mut fd, 1, 100) };
        if n < 0 {
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(error);
        }
        if n == 0 {
            continue;
        }
        let mut byte = [0];
        // SAFETY: readiness on stdin, one writable byte, no buffered overread.
        require(
            // SAFETY: readiness established on stdin and byte is writable storage.
            unsafe { libc::read(0, byte.as_mut_ptr().cast(), 1) } == 1,
            "handshake EOF",
        )?;
        out.push(byte[0]);
        require(out.len() <= 32, "handshake bound")?;
        if byte[0] == b'\n' {
            return Ok(out);
        }
    }
}

fn hook(root: &Path, event: &str, deadline: &str) -> io::Result<()> {
    let deadline = deadline.parse::<u64>().map_err(io::Error::other)?;
    let remaining = deadline
        .checked_sub(monotonic_us()?)
        .filter(|remaining| *remaining <= WITHIN.as_micros() as u64)
        .ok_or_else(|| io::Error::other("hook absolute deadline"))?;
    let until = Instant::now() + Duration::from_micros(remaining);
    let mut channel = claude_code_alab::aterm_wire::fixture_connect_until(&socket(root)?, until)?;
    // SAFETY: only this isolated helper's stdin status flags change.
    unsafe {
        let flags = libc::fcntl(0, libc::F_GETFL);
        require(
            flags >= 0 && libc::fcntl(0, libc::F_SETFL, flags | libc::O_NONBLOCK) >= 0,
            "hook nonblocking input",
        )?;
    }
    let mut bytes = Vec::new();
    loop {
        wait_fd(0, libc::POLLIN, until)?;
        let mut chunk = [0; 4096];
        // SAFETY: this helper owns stdin; chunk is bounded writable byte storage.
        let n = unsafe { libc::read(0, chunk.as_mut_ptr().cast(), chunk.len()) };
        if n == 0 {
            break;
        }
        if n < 0 {
            let error = io::Error::last_os_error();
            if matches!(
                error.kind(),
                io::ErrorKind::Interrupted | io::ErrorKind::WouldBlock
            ) {
                continue;
            }
            return Err(error);
        }
        require(bytes.len() + n as usize <= LIMIT, "hook input bound")?;
        bytes.extend_from_slice(&chunk[..n as usize]);
    }
    let input = String::from_utf8(bytes).map_err(io::Error::other)?;
    require(input.len() <= LIMIT, "hook input bound")?;
    let body = json::parse(&input).ok_or_else(|| io::Error::other("strict hook JSON"))?;
    require(
        body.get("hook_event_name").and_then(json::Value::as_str) == Some(event),
        "hook event differs",
    )?;
    write_all_until(&mut channel, &(input.len() as u32).to_be_bytes(), until)?;
    write_all_until(&mut channel, input.as_bytes(), until)?;
    let mut release = [1];
    read_exact_until(&mut channel, &mut release, until)?;
    require(release == [0], "hook release refused")
}

fn socket(root: &Path) -> io::Result<PathBuf> {
    let run = root
        .parent()
        .and_then(Path::parent)
        .ok_or_else(|| io::Error::other("private qualifier root"))?;
    Ok(run.join("scratch/sc/interactive-hook.sock"))
}

struct Hook {
    channel: UnixStream,
    native: procs::ExecIdentity,
    event: String,
    mode: Option<Mode>,
    body: String,
    watch: ExitWatch,
    until: Instant,
    exit_evidence: Option<PathBuf>,
}
impl Hook {
    fn release(&mut self) -> io::Result<()> {
        require(
            self.native.current(),
            "original hook image changed before release",
        )?;
        write_all_until(&mut self.channel, &[0], self.until)?;
        self.watch.wait(self.until)?;
        if let Some(path) = &self.exit_evidence {
            fresh(
                path,
                format!("native-note-exit {}\n", self.native.diagnostic()).as_bytes(),
            )?;
        }
        Ok(())
    }
}
impl Drop for Hook {
    fn drop(&mut self) {
        if !self.watch.ended {
            let _ = self.channel.shutdown(std::net::Shutdown::Both);
            if let Err(error) = self.watch.wait(self.until) {
                let _ = writeln!(
                    io::stderr(),
                    "unsettled original hook {}: {error}; every path retained",
                    self.native.diagnostic()
                );
                std::process::exit(70);
            }
        }
    }
}

struct World {
    child: sys::ProbeChild,
    cleanup: Cleanup,
    pair: inner_pty::PtyPair,
    master: File,
    listener: UnixListener,
    output: File,
    inputs: File,
    bytes: Vec<u8>,
    root: PathBuf,
    source: procs::ExecIdentity,
    until: Instant,
    hook_count: usize,
}
impl World {
    fn collect(&mut self) -> io::Result<()> {
        let mut bytes = [0; 8192];
        for _ in 0..32 {
            match self.master.read(&mut bytes) {
                Ok(0) => break,
                Ok(n) => {
                    require(self.bytes.len() + n <= LIMIT, "terminal output bound")?;
                    self.bytes.extend_from_slice(&bytes[..n]);
                    self.output.write_all(&bytes[..n])?;
                }
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(e),
            }
        }
        Ok(())
    }
    fn current(&mut self) -> io::Result<()> {
        require(
            Instant::now() < self.until,
            "interactive experiment one-minute deadline",
        )?;
        require(
            self.child
                .observe_native(procs::capture_exact_exec)?
                .as_ref()
                == Some(&self.source),
            "original vendor image changed",
        )
    }
    fn input(&mut self, bytes: &[u8]) -> io::Result<()> {
        let hex: String = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
        let mut written = 0;
        while written < bytes.len() {
            self.current()?;
            match self.master.write(&bytes[written..]) {
                Ok(0) => return Err(io::Error::other("private terminal input stopped")),
                Ok(count) => {
                    written += count;
                    writeln!(
                        self.inputs,
                        "{}\t{hex}\twritten={written}/{}",
                        self.source.diagnostic(),
                        bytes.len()
                    )?;
                }
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(2))
                }
                Err(error) => return Err(error),
            }
        }
        Ok(())
    }
    fn accept(&mut self) -> io::Result<Option<Hook>> {
        self.current()?;
        self.collect()?;
        require(self.hook_count < 16, "hook count bound")?;
        let (channel, _) = match self.listener.accept() {
            Ok(pair) => pair,
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => return Ok(None),
            Err(e) => return Err(e),
        };
        channel.set_nonblocking(true)?;
        let mut peer: libc::pid_t = 0;
        let mut size = std::mem::size_of_val(&peer) as libc::socklen_t;
        // SAFETY: live private accepted socket and exact native peer-PID output storage.
        require(
            // SAFETY: channel is live and peer/size have exact native output layouts.
            unsafe {
                libc::getsockopt(
                    channel.as_raw_fd(),
                    libc::SOL_LOCAL,
                    libc::LOCAL_PEERPID,
                    (&raw mut peer).cast(),
                    &raw mut size,
                )
            } == 0,
            "hook peer unavailable",
        )?;
        require(
            size as usize == std::mem::size_of_val(&peer) && peer > 1,
            "hook native peer shape",
        )?;
        let native = procs::capture_exact_exec(peer as u32)
            .ok_or_else(|| io::Error::other("hook native image unread"))?;
        let expected = fs::metadata(std::env::current_exe()?)?;
        require(
            native.process().parent == self.source.process().pid
                && native.process().executable.device == expected.dev()
                && native.process().executable.inode == expected.ino(),
            "hook is not this exact vendor's direct native helper",
        )?;
        let watch = ExitWatch::new(&native)?;
        let mut hook = Hook {
            channel,
            native,
            watch,
            until: self.until,
            event: String::new(),
            mode: None,
            body: String::new(),
            exit_evidence: None,
        };
        let mut count = [0; 4];
        read_exact_until(&mut hook.channel, &mut count, self.until)?;
        let count = u32::from_be_bytes(count) as usize;
        require(count <= LIMIT, "hook body bound")?;
        let mut bytes = vec![0; count];
        read_exact_until(&mut hook.channel, &mut bytes, self.until)?;
        let body = String::from_utf8(bytes).map_err(io::Error::other)?;
        let parsed = json::parse(&body).ok_or_else(|| io::Error::other("hook JSON malformed"))?;
        let event = parsed
            .get("hook_event_name")
            .and_then(json::Value::as_str)
            .ok_or_else(|| io::Error::other("hook event absent"))?
            .to_owned();
        require(
            event == "Stop" || event == "UserPromptSubmit",
            "unrequested hook event",
        )?;
        let mode = parsed
            .get("permission_mode")
            .and_then(json::Value::as_str)
            .and_then(Mode::of_setting);
        self.current()?;
        require(hook.native.current(), "hook changed during observation")?;
        self.hook_count += 1;
        hook.exit_evidence = Some(self.root.join(format!("hook-{}.exit", self.hook_count)));
        fresh(
            &self.root.join(format!("hook-{}.json", self.hook_count)),
            body.as_bytes(),
        )?;
        fresh(
            &self.root.join(format!("hook-{}.native", self.hook_count)),
            format!(
                "vendor={}\nhelper={}\nparent={}\n",
                self.source.diagnostic(),
                hook.native.diagnostic(),
                hook.native.process().parent
            )
            .as_bytes(),
        )?;
        hook.event = event;
        hook.mode = mode;
        hook.body = body;
        Ok(Some(hook))
    }
    fn stop(&mut self) -> io::Result<Hook> {
        loop {
            if let Some(mut hook) = self.accept()? {
                require(
                    hook.mode.is_some(),
                    "actual hook omitted current permission mode",
                )?;
                if hook.event == "Stop" {
                    return Ok(hook);
                }
                hook.release()?;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    fn cleanup(&mut self) -> io::Result<()> {
        self.cleanup.once(|| settle_child(&mut self.child))
    }
}
impl Drop for World {
    fn drop(&mut self) {
        if let Err(error) = self.cleanup() {
            let _ = writeln!(
                io::stderr(),
                "interactive qualifier retains original child after one settlement attempt: {error}; all paths retained"
            );
            std::process::exit(70);
        }
    }
}

fn start(vendor: &Path, root: &Path, until: Instant) -> io::Result<World> {
    let exe = std::env::current_exe()?;
    let cwd = std::env::current_dir()?;
    let home = PathBuf::from(
        std::env::var_os("HOME").ok_or_else(|| io::Error::other("private HOME required"))?,
    );
    let scratch = root
        .parent()
        .ok_or_else(|| io::Error::other("private scratch root missing"))?;
    require(
        home.canonicalize()? == scratch.join("home").canonicalize()?
            && cwd == scratch.join("work").canonicalize()?,
        "qualifier HOME/cwd must be this run's private scratch directories",
    )?;
    const DUMMY_KEY: &str = "protocol-proof-placeholder";
    require(
        std::env::var("ANTHROPIC_API_KEY").as_deref() == Ok(DUMMY_KEY),
        "only the private runner's placeholder API key is admitted",
    )?;
    require(
        std::env::var("ANTHROPIC_BASE_URL")
            .ok()
            .and_then(|url| {
                url.strip_prefix("http://127.0.0.1:")
                    .and_then(|port| port.parse::<u16>().ok())
            })
            .is_some_and(|port| port > 0),
        "only the private loopback API stub is admitted",
    )?;
    // Frozen 2.1.325's native key-status input is the trimmed last20 bytes.
    // This approves only the runner's public placeholder inside its private HOME.
    let approval = &DUMMY_KEY[DUMMY_KEY.len() - 20..];
    let config = format!(
        "{{\"hasCompletedOnboarding\":true,\"lastOnboardingVersion\":\"99.0.0\",\"hasSeenAutoDefaultNotice\":true,\"hasSeenAutoDefaultNudge\":true,\"customApiKeyResponses\":{{\"approved\":[{}],\"rejected\":[]}},\"theme\":\"dark\",\"projects\":{{{}:{{\"hasTrustDialogAccepted\":true}}}}}}",
        quoted(approval),
        quoted(&cwd.to_string_lossy())
    );
    fresh(&home.join(".claude.json"), config.as_bytes())?;
    let deadline = monotonic_us()?
        .checked_add(until.saturating_duration_since(Instant::now()).as_micros() as u64)
        .ok_or_else(|| io::Error::other("hook deadline overflow"))?;
    let handler = |event: &str| {
        format!(
            "{{\"hooks\":[{{\"type\":\"command\",\"command\":{},\"args\":[\"--hook\",{},{},\"{deadline}\"],\"timeout\":40}}]}}",
            quoted(&exe.to_string_lossy()),
            quoted(&root.to_string_lossy()),
            quoted(event)
        )
    };
    let settings = format!(
        "{{\"hooks\":{{\"Stop\":[{}],\"UserPromptSubmit\":[{}]}}}}",
        handler("Stop"),
        handler("UserPromptSubmit")
    );
    let settings_path = root.join("settings.json");
    fresh(&settings_path, settings.as_bytes())?;
    let socket_path = socket(root)?;
    let listener = UnixListener::bind(&socket_path).map_err(|error| {
        io::Error::new(
            error.kind(),
            format!(
                "bind private hook listener {}: {error}",
                socket_path.display()
            ),
        )
    })?;
    listener.set_nonblocking(true)?;
    let pair =
        inner_pty::PtyPair::allocate(inner_pty::Size::new(32, 120)?, until).map_err(|failure| {
            if failure.pending.is_some() {
                let _ = writeln!(io::stderr(), "unsettled private PTY allocation {failure:?}");
                let _ = writeln!(
                    io::stderr(),
                    "terminal control allocation refusal {failure:?}"
                );
                std::mem::forget(failure);
                std::process::exit(70);
            }
            io::Error::other(format!("allocate private PTY: {}", failure.error))
        })?;
    let (master, slave) = pair.fixture_descriptors();
    let master = File::from(master.try_clone_to_owned()?);
    // SAFETY: query/set only this private master, preserving its existing flags.
    unsafe {
        let flags = libc::fcntl(master.as_raw_fd(), libc::F_GETFL);
        require(
            flags >= 0
                && libc::fcntl(master.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK) >= 0,
            "private master nonblocking",
        )?;
    }
    let args = [
        "--permission-mode".into(),
        "default".into(),
        "--model".into(),
        "claude-sonnet-4-6".into(),
        "--settings".into(),
        settings_path.into_os_string(),
        "--setting-sources".into(),
        "".into(),
    ];
    let image = fs::metadata(vendor)?;
    let output = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(root.join("terminal.vt"))?;
    let inputs = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(root.join("inputs.tsv"))?;
    let mut child = match sys::ProbeChild::fixture_spawn_terminal(
        vendor,
        &args,
        &[
            ("TERM".into(), "dumb".into()),
            ("DISABLE_TELEMETRY".into(), "1".into()),
            (
                "CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC".into(),
                "1".into(),
            ),
        ],
        slave,
    ) {
        Ok(child) => child,
        Err(refused) => {
            let _ = writeln!(io::stderr(), "terminal spawn refusal {refused:?}");
            if refused.child.is_some() || refused.signals.is_some() {
                std::mem::forget(refused);
                std::process::exit(70);
            }
            return Err(refused.error);
        }
    };
    let source = (|| -> io::Result<procs::ExecIdentity> {
        loop {
            if child.exec_observed()?
                && let Some(source) = child.observe_native(procs::capture_exact_exec)?
            {
                require(
                    source.process().executable.device == image.dev()
                        && source.process().executable.inode == image.ino(),
                    "expected actual vendor image",
                )?;
                return Ok(source);
            }
            require(Instant::now() < until, "vendor exec hang detector")?;
            std::thread::sleep(Duration::from_millis(2));
        }
    })();
    let source = match source {
        Ok(source) => source,
        Err(error) => {
            if let Err(settlement) = settle_child(&mut child) {
                let _ = writeln!(io::stderr(), "vendor startup failed: {error}; {settlement}");
                std::mem::forget(child);
                std::process::exit(70);
            }
            return Err(error);
        }
    };
    Ok(World {
        child,
        cleanup: Cleanup::default(),
        pair,
        master,
        listener,
        output,
        inputs,
        bytes: Vec::new(),
        root: root.to_owned(),
        source,
        until,
        hook_count: 0,
    })
}

fn probe(vendor: &Path, root: &Path) -> io::Result<()> {
    require(
        root.is_absolute() && vendor.is_absolute(),
        "absolute qualification inputs required",
    )?;
    let run = root
        .parent()
        .and_then(Path::parent)
        .ok_or_else(|| io::Error::other("private root layout"))?;
    require(
        root.file_name().is_some_and(|name| name == "observations")
            && root
                .parent()
                .and_then(Path::file_name)
                .is_some_and(|name| name == "scratch")
            && run
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| {
                    name.starts_with("protocol-proof-") && name.ends_with(".noindex")
                }),
        "explicit generic private qualification root required",
    )?;
    let until = Instant::now() + WITHIN;
    println!("alab-qualifier-ready-v1");
    io::stdout().flush()?;
    require(line(until)? == b"GO\n", "initial qualification handshake")?;
    fs::set_permissions(root, fs::Permissions::from_mode(0o700))?;
    let mut world = start(vendor, root, until)?;
    loop {
        world.current()?;
        world.collect()?;
        if String::from_utf8_lossy(&world.bytes).contains('❯') {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    world.input(b"First local Stop observation.\r")?;
    let mut baseline = world.stop()?;
    let baseline_mode = baseline.mode.unwrap();
    baseline.release()?;
    world.input(b"\x1b[Z")?;
    world.input(b"Second local Stop observation.\r")?;
    let mut before = world.stop()?;
    let before_mode = before.mode.unwrap();
    require(
        before_mode != baseline_mode,
        "pre-turn mode change did not reach actual Stop input",
    )?;
    let before_native = before.native.clone();
    let offset = world.bytes.len();
    world.input(b"\x1b[Z")?;
    world.input(b"Third local Stop observation.\r")?;
    let observation_until = (Instant::now() + Duration::from_secs(3)).min(until);
    let mut overlap = None;
    while Instant::now() < observation_until {
        if let Some(mut hook) = world.accept()? {
            if hook.event == "UserPromptSubmit" && hook.mode != Some(before_mode) {
                overlap = hook.mode;
            }
            hook.release()?;
        }
        require(
            before_native.current(),
            "Stop hook ended during explicit hold",
        )?;
        std::thread::sleep(Duration::from_millis(10));
    }
    world.collect()?;
    fresh(&root.join("while-held.vt"), &world.bytes[offset..])?;
    fresh(&root.join("held-stop.json"), before.body.as_bytes())?;
    before.release()?;
    let summary = format!(
        "{{\"baseline_mode\":{},\"before_turn_mode\":{},\"different_mode_prompt_while_stop_held\":{},\"native_ancestry_proved\":true,\"private_transport\":true,\"state_barrier\":{},\"tui_authority\":false,\"full_lead\":false,\"inner_generation\":{}}}\n",
        quoted(baseline_mode.setting()),
        quoted(before_mode.setting()),
        overlap.map_or("null".into(), |mode| quoted(mode.setting())),
        if overlap.is_some() { "false" } else { "null" },
        world.pair.generation().get()
    );
    fresh(
        &root.join("interactive-observations.json"),
        summary.as_bytes(),
    )?;
    world.cleanup()?;
    println!("alab-qualifier-complete-v1");
    io::stdout().flush()?;
    require(
        line(until)? == b"RELEASE\n",
        "final qualification handshake",
    )
}

fn terminal_body() -> io::Result<()> {
    // SAFETY: these are read-only native terminal/session queries in the child.
    require(
        // SAFETY: read-only terminal/session queries for this child's stdio and identity.
        unsafe {
            libc::isatty(0) == 1
                && libc::isatty(1) == 1
                && libc::isatty(2) == 1
                && libc::getsid(0) == libc::getpid()
        },
        "private terminal/session remap",
    )?;
    // SAFETY: read-only native query of this child's own controlling terminal.
    require(
        // SAFETY: read-only group queries of this child's controlling terminal.
        unsafe { libc::tcgetpgrp(0) == libc::getpgrp() },
        "private terminal foreground group",
    )?;
    println!("private-terminal-body-ran");
    io::stdout().flush()?;
    let mut input = [0; 32];
    require(
        io::stdin().read(&mut input)? > 0,
        "private terminal input EOF",
    )?;
    println!("private-terminal-input-read");
    Ok(())
}

fn terminal_control(reaper: bool) -> io::Result<()> {
    let until = Instant::now() + WITHIN;
    let pair =
        inner_pty::PtyPair::allocate(inner_pty::Size::new(24, 80)?, until).map_err(|failure| {
            if failure.pending.is_some() {
                std::mem::forget(failure);
                std::process::exit(70);
            }
            io::Error::other(failure.error)
        })?;
    let (master, slave) = pair.fixture_descriptors();
    let mut master = File::from(master.try_clone_to_owned()?);
    // SAFETY: only this private master endpoint's status flags change.
    unsafe {
        let flags = libc::fcntl(master.as_raw_fd(), libc::F_GETFL);
        require(
            flags >= 0
                && libc::fcntl(master.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK) >= 0,
            "control master nonblocking",
        )?;
    }
    let exe = std::env::current_exe()?;
    let mut child = match sys::ProbeChild::fixture_spawn_terminal(
        &exe,
        &["--terminal-body".into()],
        &[],
        slave,
    ) {
        Ok(child) => child,
        Err(refusal) => {
            if refusal.child.is_some() || refusal.signals.is_some() {
                let _ = writeln!(io::stderr(), "terminal control spawn refusal {refusal:?}");
                std::mem::forget(refusal);
                std::process::exit(70);
            }
            return Err(refusal.error);
        }
    };
    let result = (|| -> io::Result<()> {
        let mut output = Vec::new();
        while !String::from_utf8_lossy(&output).contains("private-terminal-body-ran") {
            require(Instant::now() < until, "terminal body proof hang detector")?;
            let mut bytes = [0; 1024];
            match master.read(&mut bytes) {
                Ok(n) => output.extend_from_slice(&bytes[..n]),
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => {}
                Err(e) => return Err(e),
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        require(
            child.exec_observed()?,
            "private terminal exact exec completion",
        )?;
        let observed = child
            .observe_native(procs::capture_exact_exec)?
            .ok_or_else(|| io::Error::other("private terminal original native observation"))?;
        require(
            observed.process().pid == child.pid().unwrap() as u32,
            "terminal native child mismatch",
        )?;
        let mut exit = ExitWatch::new(&observed)?;
        master.write_all(b"finish\n")?;
        if reaper {
            while !sys::child_has_ended(child.pid().unwrap()) {
                require(Instant::now() < until, "terminal reaper end hang detector")?;
                let mut bytes = [0; 1024];
                match master.read(&mut bytes) {
                    Ok(n) => output.extend_from_slice(&bytes[..n]),
                    Err(error)
                        if error.kind() == io::ErrorKind::WouldBlock
                            || error.raw_os_error() == Some(libc::EIO) => {}
                    Err(error) => return Err(error),
                }
                std::thread::sleep(Duration::from_millis(2));
            }
            let pid = child.pid().unwrap();
            // SAFETY: initialized native action storage for this isolated control.
            let mut action: libc::sigaction = unsafe { std::mem::zeroed() };
            // SAFETY: writable exact-layout native output for restoration below.
            let mut previous: libc::sigaction = unsafe { std::mem::zeroed() };
            action.sa_sigaction = reap_terminal as *const () as usize;
            // SAFETY: valid native action, this process's own unmanaged signal only.
            unsafe {
                libc::sigemptyset(&raw mut action.sa_mask);
                require(
                    libc::sigaction(libc::SIGUSR1, &raw const action, &raw mut previous) == 0,
                    "terminal reaper installation",
                )?;
                require(libc::raise(libc::SIGUSR1) == 0, "terminal reaper delivery")?;
                require(
                    libc::sigaction(libc::SIGUSR1, &raw const previous, std::ptr::null_mut()) == 0,
                    "terminal reaper restoration",
                )?;
            }
            require(
                REAPED.load(Ordering::SeqCst) == pid,
                "actual terminal child wait stolen",
            )?;
            require(
                child.try_wait().is_err()
                    && child.pending()
                    && child.ownership_lost()
                    && child.cancel().is_err(),
                "terminal lost receipt refuses every later signal",
            )?;
            let error = settle_child(&mut child)
                .expect_err("lost original wait cannot become a settlement witness");
            require(
                error.to_string().contains("cancellation refused")
                    && error
                        .to_string()
                        .contains("pending=true ownership_lost=true"),
                "lost terminal settlement must retain its precise native refusal",
            )?;
            writeln!(io::stderr(), "{error}")?;
            exit.wait(until)?;
            let mut next =
                match sys::ProbeChild::spawn(Path::new("/bin/sleep"), &["30".into()], &[]) {
                    Ok(child) => child,
                    Err(refusal) => {
                        std::mem::forget(refusal);
                        std::process::exit(70);
                    }
                };
            let replacement = next
                .try_wait()
                .map(|status| child.cancel().is_err() && status.is_none());
            if let Err(error) = settle_child(&mut next) {
                let _ = writeln!(io::stderr(), "replacement control settlement: {error}");
                std::mem::forget(next);
                std::process::exit(70);
            }
            require(
                replacement?,
                "stale terminal receipt cannot signal a later child",
            )?;
            let _ = writeln!(
                io::stderr(),
                "terminal unmanaged reaper proved lost wait; original receipt retained through exit70"
            );
            std::process::exit(70);
        }
        loop {
            require(Instant::now() < until, "terminal native wait hang detector")?;
            if let Some(status) = child.try_wait()? {
                require(status.success(), "terminal body failed")?;
                break;
            }
            let mut bytes = [0; 1024];
            match master.read(&mut bytes) {
                Ok(n) => output.extend_from_slice(&bytes[..n]),
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => {}
                Err(e) if e.raw_os_error() == Some(libc::EIO) => {}
                Err(e) => return Err(e),
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        exit.wait(until)?;
        Ok(())
    })();
    if let Err(error) = settle_child(&mut child) {
        let _ = writeln!(
            io::stderr(),
            "terminal control retained {child:?}; result {result:?}; settlement: {error}"
        );
        std::mem::forget(child);
        std::process::exit(70);
    }
    result
}

fn terminal_reaper_control() -> io::Result<()> {
    let mut child = match sys::ProbeChild::spawn(
        &std::env::current_exe()?,
        &["--terminal-reaper".into()],
        &[],
    ) {
        Ok(child) => child,
        Err(refusal) => {
            std::mem::forget(refusal);
            std::process::exit(70);
        }
    };
    let result = (|| -> io::Result<()> {
        let until = Instant::now() + WITHIN;
        let mut out = Vec::new();
        let mut err = Vec::new();
        loop {
            require(
                Instant::now() < until,
                "isolated terminal reaper hang detector",
            )?;
            child.drain_total(&mut out, &mut err, LIMIT)?;
            if let Some(status) = child.try_wait()? {
                child.drain_total(&mut out, &mut err, LIMIT)?;
                require(
                    status.code() == Some(70)
                        && String::from_utf8_lossy(&err)
                            .contains("terminal unmanaged reaper proved lost wait")
                        && String::from_utf8_lossy(&err)
                            .contains("pending=true ownership_lost=true"),
                    "terminal negative control must refuse at its actual executable boundary",
                )?;
                return Ok(());
            }
            std::thread::sleep(Duration::from_millis(2));
        }
    })();
    if let Err(error) = settle_child(&mut child) {
        let _ = writeln!(io::stderr(), "terminal reaper settlement: {error}");
        std::mem::forget(child);
        std::process::exit(70);
    }
    result
}

fn expired_transport_keeps_bytes_unconsumed() -> io::Result<()> {
    let (mut first, mut second) = UnixStream::pair()?;
    first.set_nonblocking(true)?;
    second.set_nonblocking(true)?;
    second.write_all(b"a")?;
    let mut byte = [0];
    require(
        read_exact_until(&mut first, &mut byte, Instant::now()).is_err(),
        "expired read must refuse queued bytes",
    )?;
    read_exact_until(&mut first, &mut byte, Instant::now() + WITHIN)?;
    require(
        byte == *b"a",
        "expired read must leave original bytes intact",
    )?;
    require(
        write_all_until(&mut first, b"b", Instant::now()).is_err(),
        "expired write must remain unsent",
    )?;
    require(
        second
            .read(&mut byte)
            .is_err_and(|error| error.kind() == io::ErrorKind::WouldBlock),
        "expired write reached its private peer",
    )?;
    Ok(())
}

fn settlement_controls() -> io::Result<()> {
    use std::cell::Cell;

    let started = Instant::now();
    let elapsed = Cell::new(Duration::ZERO);
    let polls = Cell::new(0);
    wait_for_settlement(
        started,
        || started + elapsed.get(),
        || {
            elapsed.set(elapsed.get() + Duration::from_secs(3));
            polls.set(polls.get() + 1);
            Ok(polls.get() == 3)
        },
        |pause| {
            assert!(pause <= Duration::from_millis(10));
            elapsed.set(elapsed.get() + pause);
        },
    )?;
    require(
        polls.get() == 3 && elapsed.get() > Duration::from_secs(2),
        "a delayed positive terminal receipt must outlive the former two-second detector",
    )?;

    let called = Cell::new(false);
    let expired = wait_for_settlement(
        started,
        || started + WITHIN,
        || {
            called.set(true);
            Ok(true)
        },
        |_| panic!("an expired transaction cannot sleep"),
    )
    .expect_err("expired transaction cannot admit a terminal receipt");
    require(
        expired.kind() == io::ErrorKind::TimedOut && !called.get(),
        "expired settlement must refuse before any native operation",
    )?;

    let elapsed = Cell::new(Duration::ZERO);
    let pending = wait_for_settlement(
        started,
        || started + elapsed.get(),
        || {
            elapsed.set(elapsed.get() + Duration::from_secs(31));
            Ok(false)
        },
        |_| {},
    )
    .expect_err("pending original child is not a terminal receipt");
    require(
        pending.kind() == io::ErrorKind::TimedOut,
        "unknown native settlement must retain a deadline refusal",
    )?;

    let elapsed = Cell::new(Duration::ZERO);
    let late = wait_for_settlement(
        started,
        || started + elapsed.get(),
        || {
            elapsed.set(WITHIN);
            Ok(true)
        },
        |_| panic!("late terminal receipt cannot sleep"),
    )
    .expect_err("a receipt observed after the deadline cannot qualify the run");
    require(
        late.kind() == io::ErrorKind::TimedOut,
        "late native settlement",
    )?;

    let mut failed = Cleanup::default();
    let attempts = Cell::new(0);
    let refusal = failed
        .once(|| {
            attempts.set(attempts.get() + 1);
            Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "original lost native receipt",
            ))
        })
        .expect_err("unsettled cleanup cannot succeed");
    let retained = failed
        .once(|| panic!("Drop must not retry the refused native transaction"))
        .expect_err("the original cleanup refusal remains retained");
    require(
        attempts.get() == 1
            && refusal.kind() == retained.kind()
            && refusal.to_string() == retained.to_string(),
        "cleanup must retain the original refusal without another cancellation",
    )?;

    let mut settled = Cleanup::default();
    settled.once(|| Ok(()))?;
    settled.once(|| panic!("Drop must not cancel a positively settled child"))?;
    Ok(())
}

pub fn run() -> io::Result<()> {
    sys::ensure_single_threaded()?;
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    match args.as_slice() {
        [role] if role == "--terminal-body" => terminal_body(),
        [role] if role == "--terminal-reaper" => terminal_control(true),
        [hook_arg, root, event, deadline] if hook_arg == "--hook" => hook(
            Path::new(root),
            event
                .to_str()
                .ok_or_else(|| io::Error::other("hook event UTF-8"))?,
            deadline
                .to_str()
                .ok_or_else(|| io::Error::other("hook deadline UTF-8"))?,
        ),
        [probe_arg, vendor_key, vendor, root_key, root]
            if probe_arg == "--probe" && vendor_key == "--vendor" && root_key == "--root" =>
        {
            probe(Path::new(vendor), Path::new(root))
        }
        [] => {
            require(
                json::parse(&quoted("quote\"slash\\\n"))
                    .and_then(|value| value.as_str().map(str::to_owned))
                    .as_deref()
                    == Some("quote\"slash\\\n"),
                "settings JSON escaping",
            )?;
            terminal_control(false)?;
            terminal_reaper_control()?;
            expired_transport_keeps_bytes_unconsumed()?;
            settlement_controls()?;
            println!(
                "PASS settings framing; NOT RUN real interactive permission observation without explicit private qualifier inputs"
            );
            Ok(())
        }
        _ => Err(io::Error::other("unsupported interactive qualifier role")),
    }
}
