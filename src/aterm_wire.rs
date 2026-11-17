// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Direct-local transport, typed requests and retained connection custody.
//! Lifecycle claims and reconnects use [`alab_wire::link::Link`] through the supervisor adapter.
//!
//! The framing and discovery rules follow aterm's `control_socket.rs`,
//! `control_verbs.rs` and `RelayClient`, and grok-alab's direct `aterm.rs` client.
//! A connection is opened by this process, never through `aterm ctl`, so the
//! server can inspect this process's kernel peer identity. Opening a connection
//! does not prove that identity, acquire a claim, or enable this harness.
//!
//! Each exchange has one monotonic deadline, including every write and reply
//! fragment. A failed exchange poisons its connection: a late reply can never
//! answer another request. A transport error after a request starts is an
//! unknown outcome, not permission to repeat an act. The socket stays owned
//! until dropped or explicitly exported into bounded, non-writing custody:
//! a claim may have committed before its ACK was lost.
//!
//! It also spells the three typed writes, `send`, `key` and `turn`, each under
//! the fence aterm judges it by ([`TypedFence`]): [`send_verb`], [`key_verb`]
//! and [`turn_verb`]. Spelling one types nothing; [`crate::typing`] sends them.

use std::ffi::OsStr;
use std::io::{self, Read, Write};
use std::net::Shutdown;
use std::os::fd::{AsFd, AsRawFd, BorrowedFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

pub(crate) use alab_wire::wire::session_id;
pub use alab_wire::wire::{Fields, percent_decode};

use crate::aterm::{Restart, Scope};

const MAX_TOKEN_BYTES: usize = 256;
const MAX_GRAPH_BYTES: usize = 16 * 1024;
const MAX_LINE_BYTES: usize = 1024 * 1024;
const EVENT_DRAIN_BYTES: usize = 256 * 1024;
const EVENT_DRAIN_READS: usize = 1024;

/// Maximum one-transaction custody budget, including forward exec and rollback.
pub const MAX_CUSTODY_AGE: Duration = Duration::from_secs(5 * 60);

fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

/// aterm's answer to `subscribe`, when it opens no stream: an `ERR` line, or an
/// acknowledgement this reader cannot parse. It travels as the inner error of
/// the [`io::ErrorKind::InvalidData`] error [`EventReader::connect`] returns
/// ([`Unsubscribed::of`]), so a caller can tell an answer aterm gave apart from
/// a connection that failed and a reply that never came, which carry no such
/// inner error.
#[derive(Debug)]
pub struct Unsubscribed {
    header: String,
    why: &'static str,
}

impl Unsubscribed {
    fn error(header: &str, why: &'static str) -> io::Error {
        io::Error::new(
            io::ErrorKind::InvalidData,
            Self {
                header: header.to_string(),
                why,
            },
        )
    }

    /// The answer `error` carries, when it is one aterm gave.
    #[must_use]
    pub fn of(error: &io::Error) -> Option<&Self> {
        error.get_ref()?.downcast_ref()
    }

    /// The first line of aterm's answer, as it wrote it.
    #[must_use]
    pub fn header(&self) -> &str {
        &self.header
    }

    /// Whether aterm refused the subscription with an `ERR` line, rather than
    /// acknowledging it in words this reader cannot parse.
    #[must_use]
    pub fn refused(&self) -> bool {
        self.header == "ERR" || self.header.starts_with("ERR ")
    }
}

impl std::fmt::Display for Unsubscribed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.why)
    }
}

impl std::error::Error for Unsubscribed {}

fn deadline(timeout: Duration) -> io::Result<Instant> {
    deadline_at(timeout, Instant::now())
}

fn deadline_at(timeout: Duration, now: Instant) -> io::Result<Instant> {
    if timeout.is_zero() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "a control deadline must be positive",
        ));
    }
    now.checked_add(timeout)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "control deadline overflows"))
}

fn remaining(until: Instant) -> io::Result<Duration> {
    remaining_at(until, Instant::now())
}

fn remaining_at(until: Instant, now: Instant) -> io::Result<Duration> {
    until
        .checked_duration_since(now)
        .filter(|left| !left.is_zero())
        .ok_or_else(|| io::Error::new(io::ErrorKind::TimedOut, "control exchange deadline elapsed"))
}

/// A session's rendezvous directory and validated session ID.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Locator {
    directory: PathBuf,
    sid: String,
}

impl Locator {
    /// Resolve the documented environment values without reading the process environment.
    ///
    /// # Errors
    /// A session ID or rendezvous directory is absent or malformed.
    pub fn from_values(
        sid: &str,
        runtime: Option<&OsStr>,
        home: Option<&OsStr>,
    ) -> io::Result<Self> {
        let directory = if let Some(runtime) = runtime.filter(|value| !value.is_empty()) {
            PathBuf::from(runtime).join("aterm")
        } else {
            PathBuf::from(
                home.filter(|value| !value.is_empty())
                    .ok_or_else(|| invalid("neither XDG_RUNTIME_DIR nor HOME is set"))?,
            )
            .join("Library/Application Support/aterm")
        };
        Self::new(directory, sid)
    }

    /// Name an explicit rendezvous directory and session ID.
    ///
    /// # Errors
    /// `sid` is not a canonical current or retained older tab ID.
    pub fn new(directory: PathBuf, sid: &str) -> io::Result<Self> {
        if !session_id(sid) {
            return Err(invalid("the session ID is not a canonical aterm tab ID"));
        }
        Ok(Self {
            directory,
            sid: sid.to_string(),
        })
    }

    /// The validated session ID.
    #[must_use]
    pub fn sid(&self) -> &str {
        &self.sid
    }

    /// Read the current `graph/<sid>` entry again, so a host move is rediscovered.
    ///
    /// # Errors
    /// The graph is missing, nonregular, oversized, invalid UTF-8, or has no unique absolute socket.
    pub fn socket(&self) -> io::Result<PathBuf> {
        let body = read_regular(
            &self.directory.join("graph").join(&self.sid),
            MAX_GRAPH_BYTES,
        )?;
        let mut socket = None;
        for line in body.lines() {
            if let Some(value) = line.strip_prefix("sock ") {
                let value = value.trim();
                if socket.is_some() || value.is_empty() || !Path::new(value).is_absolute() {
                    return Err(invalid("the graph has no unique absolute socket"));
                }
                socket = Some(PathBuf::from(value));
            }
        }
        socket.ok_or_else(|| invalid("the graph names no socket"))
    }
}

fn read_regular(path: &Path, bound: usize) -> io::Result<String> {
    let bound_u64 = u64::try_from(bound).map_err(|_| invalid("invalid control file bound"))?;
    let read_limit = bound_u64
        .checked_add(1)
        .ok_or_else(|| invalid("control file bound leaves no overflow sentinel"))?;
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(path)
        .map_err(|error| {
            if error.raw_os_error() == Some(libc::ELOOP)
                || std::fs::symlink_metadata(path).is_ok_and(|meta| meta.file_type().is_symlink())
            {
                invalid("the control file is a symlink")
            } else {
                error
            }
        })?;
    let meta = file.metadata()?;
    if !meta.is_file() || meta.len() > bound_u64 {
        return Err(invalid(
            "the control file is nonregular or exceeds its bound",
        ));
    }
    let mut bytes = Vec::new();
    file.take(read_limit).read_to_end(&mut bytes)?;
    if bytes.len() > bound {
        return Err(invalid("the control file exceeds its bound"));
    }
    String::from_utf8(bytes).map_err(|_| invalid("the control file is not UTF-8"))
}

fn instance_pid(name: &str) -> Option<u32> {
    let digits = name.strip_prefix("aterm-")?.strip_suffix(".sock")?;
    if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    digits.parse().ok()
}

fn pinned_socket(socket: &Path) -> PathBuf {
    if let (Ok(target), Some(directory)) = (std::fs::read_link(socket), socket.parent())
        && let Some(name) = target.to_str()
        && instance_pid(name).is_some()
    {
        return directory.join(name);
    }
    socket.to_path_buf()
}

fn token_names(name: &str) -> Vec<String> {
    if let Some(pid) = instance_pid(name) {
        return vec![format!("aterm-{pid}.token")];
    }
    let own = if name.ends_with(".sock") {
        format!("{name}.token")
    } else {
        format!("aterm-sock-{name}.token")
    };
    vec![own, "aterm.token".to_string()]
}

fn token_beside(socket: &Path) -> io::Result<String> {
    token_beside_using(socket, |path| read_regular(path, MAX_TOKEN_BYTES))
}

fn token_beside_using(
    socket: &Path,
    mut read: impl FnMut(&Path) -> io::Result<String>,
) -> io::Result<String> {
    let (Some(directory), Some(name)) = (socket.parent(), socket.file_name()) else {
        return Err(invalid("the socket names no parent directory"));
    };
    let target_name = std::fs::read_link(socket)
        .ok()
        .and_then(|target| target.file_name().map(OsStr::to_os_string));
    let name = target_name
        .as_deref()
        .unwrap_or(name)
        .to_str()
        .ok_or_else(|| invalid("the socket filename is not UTF-8"))?;
    let mut missing = None;
    for candidate in token_names(name) {
        match read(&directory.join(candidate)) {
            Ok(raw) => {
                let token = raw.trim();
                if token.is_empty()
                    || token
                        .chars()
                        .any(|character| character.is_whitespace() || character.is_control())
                {
                    return Err(invalid("the token file is not one token"));
                }
                return Ok(token.to_string());
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                missing.get_or_insert(error);
            }
            Err(error) => return Err(error),
        }
    }
    Err(missing.unwrap_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no control token")))
}

fn fd_flags(fd: RawFd) -> io::Result<libc::c_int> {
    // SAFETY: F_GETFD observes this live owned descriptor and takes no pointer argument.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
    if flags < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(flags)
    }
}

fn set_fd_flags(fd: RawFd, flags: libc::c_int) -> io::Result<()> {
    // SAFETY: F_SETFD changes flags on this live owned descriptor and takes an integer argument.
    if unsafe { libc::fcntl(fd, libc::F_SETFD, flags) } < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

#[cfg(target_os = "macos")]
fn peer_fully_closed(stream: &UnixStream) -> bool {
    let mut probe = libc::pollfd {
        fd: stream.as_raw_fd(),
        events: libc::POLLOUT,
        revents: 0,
    };
    // SAFETY: probe is one initialized live descriptor, and a zero timeout never waits.
    let ready = unsafe { libc::poll(&raw mut probe, 1, 0) };
    ready > 0 && probe.revents & (libc::POLLHUP | libc::POLLERR | libc::POLLNVAL) != 0
}

#[cfg(target_os = "macos")]
fn arm_after_peer_close(stream: &UnixStream, armed: io::Result<()>) -> io::Result<()> {
    match armed {
        // As in aterm-agent's accept_closed_arm: Darwin can reject timeout changes
        // after full peer closure. Actual I/O cannot wait then, and must still read
        // queued reply bytes or expose EOF/the socket error. A half-close is insufficient.
        Err(error)
            if matches!(error.raw_os_error(), Some(libc::EINVAL | libc::ENOTCONN))
                && peer_fully_closed(stream) =>
        {
            Ok(())
        }
        result => result,
    }
}

/// A Unix socket address with every field zero, built field by field where
/// the platform's fields are all public.
#[cfg(any(target_os = "macos", target_os = "ios"))]
const fn blank_unix_address() -> libc::sockaddr_un {
    libc::sockaddr_un {
        sun_len: 0,
        sun_family: 0,
        sun_path: [0; 104],
    }
}

/// A Unix socket address with every field zero.
#[cfg(not(any(target_os = "macos", target_os = "ios")))]
fn blank_unix_address() -> libc::sockaddr_un {
    // SAFETY: the all-zero representation initializes sockaddr_un's scalar/byte fields.
    unsafe { std::mem::zeroed() }
}

/// Socket-address storage with every field zero.
#[cfg(any(target_os = "macos", target_os = "ios"))]
fn blank_address_storage() -> libc::sockaddr_storage {
    libc::sockaddr_storage::default()
}

/// Socket-address storage with every field zero.
#[cfg(not(any(target_os = "macos", target_os = "ios")))]
fn blank_address_storage() -> libc::sockaddr_storage {
    // SAFETY: zero initializes the storage's family, length and padding bytes.
    unsafe { std::mem::zeroed() }
}

/// Reuse the bounded native Unix connection for an isolated protocol fixture,
/// without reading a host token or sending authentication words.
/// # Errors
/// The path, socket preparation, connection or fixed deadline fails.
#[cfg(debug_assertions)]
#[doc(hidden)]
pub fn fixture_connect_until(path: &Path, until: Instant) -> io::Result<UnixStream> {
    connect_until(path, until)
}

fn connect_until(path: &Path, until: Instant) -> io::Result<UnixStream> {
    let bytes = path.as_os_str().as_bytes();
    let mut address = blank_unix_address();
    if bytes.is_empty() || bytes.contains(&0) || bytes.len() >= address.sun_path.len() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid Unix socket path",
        ));
    }
    address.sun_family = libc::AF_UNIX as libc::sa_family_t;
    for (target, byte) in address.sun_path.iter_mut().zip(bytes) {
        *target = *byte as libc::c_char;
    }
    let length = std::mem::offset_of!(libc::sockaddr_un, sun_path) + bytes.len() + 1;
    #[cfg(any(
        target_os = "macos",
        target_os = "ios",
        target_os = "freebsd",
        target_os = "openbsd",
        target_os = "netbsd"
    ))]
    {
        address.sun_len =
            u8::try_from(length).map_err(|_| invalid("Unix socket address too long"))?;
    }
    #[cfg(target_os = "linux")]
    let kind = libc::SOCK_STREAM | libc::SOCK_CLOEXEC;
    #[cfg(not(target_os = "linux"))]
    let kind = libc::SOCK_STREAM;
    // SAFETY: socket has no pointer arguments; a successful descriptor is owned below.
    let raw = unsafe { libc::socket(libc::AF_UNIX, kind, 0) };
    if raw < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: socket returned a new, uniquely owned descriptor of a Unix stream socket.
    let stream = unsafe { UnixStream::from_raw_fd(raw) };
    set_fd_flags(raw, fd_flags(raw)? | libc::FD_CLOEXEC)?;
    #[cfg(target_vendor = "apple")]
    {
        // Adopting a raw descriptor bypasses std's Apple socket constructor.
        let enabled: libc::c_int = 1;
        // SAFETY: enabled is a live, correctly sized socket-option value.
        if unsafe {
            libc::setsockopt(
                raw,
                libc::SOL_SOCKET,
                0x1022,
                (&raw const enabled).cast(),
                std::mem::size_of_val(&enabled) as libc::socklen_t,
            )
        } < 0
        {
            return Err(io::Error::last_os_error());
        }
    }
    stream.set_nonblocking(true)?;
    remaining(until)?;
    // SAFETY: address is initialized and length covers only its family and initialized path.
    let connected =
        unsafe { libc::connect(raw, (&raw const address).cast(), length as libc::socklen_t) };
    if connected < 0 {
        let error = io::Error::last_os_error();
        // AF_UNIX EAGAIN is a full backlog, with no pending connection to poll.
        if !matches!(error.raw_os_error(), Some(libc::EINPROGRESS | libc::EINTR)) {
            return Err(error);
        }
        loop {
            let left = remaining(until)?;
            let millis = left
                .as_millis()
                .saturating_add(1)
                .min(libc::c_int::MAX as u128) as libc::c_int;
            let mut poll = libc::pollfd {
                fd: raw,
                events: libc::POLLOUT,
                revents: 0,
            };
            // SAFETY: poll points at one initialized live descriptor and a bounded count.
            let ready = unsafe { libc::poll(&raw mut poll, 1, millis) };
            if ready < 0 {
                let error = io::Error::last_os_error();
                if error.kind() == io::ErrorKind::Interrupted {
                    continue;
                }
                return Err(error);
            }
            remaining(until)?;
            if ready == 0 {
                continue;
            }
            if let Some(error) = stream.take_error()? {
                return Err(error);
            }
            match stream.peer_addr() {
                Ok(_) => break,
                Err(error) if error.raw_os_error() == Some(libc::ENOTCONN) => continue,
                Err(error) => return Err(error),
            }
        }
    }
    stream.peer_addr()?;
    remaining(until)?;
    stream.set_nonblocking(false)?;
    Ok(stream)
}

/// One authenticated, nonclonable, in-process request connection.
#[derive(Debug)]
pub struct Connection {
    stream: UnixStream,
    pending: Vec<u8>,
    timeout: Duration,
    poisoned: bool,
    version: String,
    clock: fn() -> Instant,
}

impl Connection {
    /// Connect directly, read the sibling token after connecting, and authenticate with `version`.
    ///
    /// # Errors
    /// Discovery, connection, authentication, framing, or the total opening deadline fails.
    pub fn connect(socket: &Path, timeout: Duration) -> io::Result<Self> {
        let until = deadline(timeout)?;
        Self::open_until(socket, timeout, until, Instant::now)
    }

    fn open_until(
        socket: &Path,
        timeout: Duration,
        until: Instant,
        clock: fn() -> Instant,
    ) -> io::Result<Self> {
        let socket = pinned_socket(socket);
        let stream = connect_until(&socket, until)?;
        let token = token_beside(&socket)?;
        Self::authenticate(stream, &token, timeout, until, clock)
    }

    fn new(stream: UnixStream, timeout: Duration) -> io::Result<Self> {
        set_fd_flags(
            stream.as_raw_fd(),
            fd_flags(stream.as_raw_fd())? | libc::FD_CLOEXEC,
        )?;
        stream.set_nonblocking(false)?;
        Ok(Self {
            stream,
            pending: Vec::new(),
            timeout,
            poisoned: false,
            version: String::new(),
            clock: Instant::now,
        })
    }

    fn authenticate(
        stream: UnixStream,
        token: &str,
        timeout: Duration,
        until: Instant,
        clock: fn() -> Instant,
    ) -> io::Result<Self> {
        let mut connection = Self::new(stream, timeout)?;
        connection.clock = clock;
        let result = connection
            .write_until(format!("AUTH {token}\nversion\n").as_bytes(), until)
            .and_then(|()| connection.line_until(until))
            .and_then(|line| {
                line.strip_prefix("OK ")
                    .filter(|body| !body.is_empty())
                    .map(str::to_string)
                    .ok_or_else(|| {
                        io::Error::new(
                            io::ErrorKind::PermissionDenied,
                            "aterm refused authentication/version",
                        )
                    })
            });
        match connection.finish(result, until) {
            Ok(version) => {
                connection.version = version;
                Ok(connection)
            }
            Err(error) => {
                connection.poison();
                Err(error)
            }
        }
    }

    /// The host's uninterpreted `version` response body, for diagnostics only.
    #[must_use]
    pub fn version(&self) -> &str {
        &self.version
    }

    /// Bound every later exchange on this connection by `timeout` in place of the
    /// one it was opened with. A caller that opens with a generous bound and then
    /// asks from a loop that must not stall narrows it here; an exchange that
    /// outlives the bound poisons the connection like any other failure.
    pub fn set_timeout(&mut self, timeout: Duration) {
        self.timeout = timeout;
    }

    /// The bound each exchange on this connection has now ([`Connection::set_timeout`]).
    #[must_use]
    pub fn timeout(&self) -> Duration {
        self.timeout
    }

    /// Whether an earlier exchange made this connection unusable.
    #[must_use]
    pub fn is_poisoned(&self) -> bool {
        self.poisoned
    }

    fn poison(&mut self) {
        self.poisoned = true;
    }

    fn begin(&mut self, request: &str) -> io::Result<Instant> {
        self.prepare(request)?;
        deadline_at(self.timeout, (self.clock)())
    }

    fn prepare(&mut self, request: &str) -> io::Result<()> {
        if request.is_empty() || request.contains(['\r', '\n']) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "a request is one nonempty line",
            ));
        }
        if request.len() > MAX_LINE_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "request exceeds its line bound",
            ));
        }
        if self.poisoned {
            return Err(io::Error::new(
                io::ErrorKind::NotConnected,
                "an earlier exchange failed; request not sent",
            ));
        }
        if !self.pending.is_empty() {
            self.poison();
            return Err(invalid("unexpected unread reply bytes; request not sent"));
        }
        Ok(())
    }

    fn arm(&self, until: Instant) -> io::Result<()> {
        let left = remaining_at(until, (self.clock)())?;
        let armed = self
            .stream
            .set_read_timeout(Some(left))
            .and_then(|()| self.stream.set_write_timeout(Some(left)));
        #[cfg(target_os = "macos")]
        {
            arm_after_peer_close(&self.stream, armed)
        }
        #[cfg(not(target_os = "macos"))]
        {
            armed
        }
    }

    fn write_until(&mut self, mut bytes: &[u8], until: Instant) -> io::Result<()> {
        while !bytes.is_empty() {
            self.arm(until)?;
            match self.stream.write(bytes) {
                Ok(0) => {
                    return Err(io::Error::new(
                        io::ErrorKind::WriteZero,
                        "aterm accepted no request bytes",
                    ));
                }
                Ok(written) => bytes = &bytes[written..],
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) => return Err(error),
            }
            remaining_at(until, (self.clock)())?;
        }
        Ok(())
    }

    fn line_until(&mut self, until: Instant) -> io::Result<String> {
        loop {
            remaining_at(until, (self.clock)())?;
            if let Some(end) = self.pending.iter().position(|byte| *byte == b'\n') {
                if end > MAX_LINE_BYTES {
                    return Err(invalid("reply line exceeds its bound"));
                }
                let mut bytes: Vec<u8> = self.pending.drain(..=end).collect();
                bytes.pop();
                if bytes.last() == Some(&b'\r') {
                    bytes.pop();
                }
                return String::from_utf8(bytes).map_err(|_| invalid("reply line is not UTF-8"));
            }
            if self.pending.len() > MAX_LINE_BYTES {
                return Err(invalid("reply line exceeds its bound"));
            }
            self.arm(until)?;
            let mut bytes = [0_u8; 4096];
            let room = bytes.len().min(MAX_LINE_BYTES + 1 - self.pending.len());
            match self.stream.read(&mut bytes[..room]) {
                Ok(0) => {
                    return Err(io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        "aterm closed before a complete reply",
                    ));
                }
                Ok(read) => self.pending.extend_from_slice(&bytes[..read]),
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) => return Err(error),
            }
        }
    }

    fn finish<T>(&mut self, result: io::Result<T>, until: Instant) -> io::Result<T> {
        let result = result.and_then(|value| remaining_at(until, (self.clock)()).map(|_| value));
        if result.is_err() {
            self.poison();
        }
        result
    }

    /// Send one raw protocol line and read one `OK`/`ERR` reply line.
    ///
    /// # Errors
    /// Invalid local input is unsent. Any exchange failure poisons the connection;
    /// the server may already have committed the request, even when no ACK arrived.
    pub fn request_line(&mut self, request: &str) -> io::Result<String> {
        let until = self.begin(request)?;
        let result = self.exchange_line(request, until);
        self.finish(result, until)
    }

    fn exchange_line(&mut self, request: &str, until: Instant) -> io::Result<String> {
        self.write_until(format!("{request}\n").as_bytes(), until)
            .and_then(|()| self.line_until(until))
            .and_then(|line| {
                if line == "OK"
                    || line.starts_with("OK ")
                    || line == "ERR"
                    || line.starts_with("ERR ")
                {
                    Ok(line)
                } else {
                    Err(invalid("reply has no OK/ERR header"))
                }
            })
    }

    /// Read an `OK` field reply with strict duplicate-key validation.
    ///
    /// # Errors
    /// As `request_line`, or the reply is not a unique-key `OK` field reply. Malformed replies poison the connection.
    pub fn request_fields(&mut self, request: &str) -> io::Result<Fields> {
        let until = self.begin(request)?;
        let result = self
            .exchange_line(request, until)
            .and_then(|line| Fields::parse(&line));
        self.finish(result, until)
    }

    /// Read an `OK <count>` reply and its exact counted UTF-8 body lines.
    /// The whole exchange shares one deadline and one aggregate byte bound.
    /// This reads observations only; a body grants no native mutation authority.
    ///
    /// # Errors
    /// Invalid bounds or local requests are unsent. A refused or malformed reply,
    /// excessive count or bytes, incomplete body or elapsed deadline poisons the
    /// connection, so a late body can never answer a later request.
    pub fn request_body(&mut self, request: &str, max_lines: usize) -> io::Result<Vec<String>> {
        self.counted(request, max_lines, false)
            .map(|(_, body)| body)
    }

    /// Send one request whose `OK` reply counts the lines after it, as `turn`'s
    /// does (`OK <n> turn submitted=… seq=…`, then the `<n>` rows it settled on),
    /// and read the header and exactly those lines. An `ERR` header is a reply
    /// with no body. The whole exchange shares one deadline and one aggregate
    /// byte bound, as [`Connection::request_body`]'s.
    ///
    /// # Errors
    /// As [`Connection::request_body`], except that an `ERR` header and words
    /// after the count are answers, not errors.
    pub fn request_counted(
        &mut self,
        request: &str,
        max_lines: usize,
    ) -> io::Result<(String, Vec<String>)> {
        self.counted(request, max_lines, true)
    }

    /// The one reader of a counted reply. `worded` admits an `ERR` header with
    /// no body and words after an `OK` header's count; without it the header
    /// is exactly `OK <count>`.
    fn counted(
        &mut self,
        request: &str,
        max_lines: usize,
        worded: bool,
    ) -> io::Result<(String, Vec<String>)> {
        if max_lines == 0 || max_lines > 1024 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "a counted reply needs a positive bounded line count",
            ));
        }
        let until = self.begin(request)?;
        let result = (|| {
            let header = self.exchange_line(request, until)?;
            if worded && (header == "ERR" || header.starts_with("ERR ")) {
                return Ok((header, Vec::new()));
            }
            let count = header
                .strip_prefix("OK ")
                .and_then(|rest| {
                    let (count, words) = rest.split_once(' ').unwrap_or((rest, ""));
                    (worded || words.is_empty()).then_some(count)
                })
                .and_then(|count| {
                    count
                        .parse::<usize>()
                        .ok()
                        .filter(|parsed| parsed.to_string() == count)
                })
                .filter(|count| *count <= max_lines)
                .ok_or_else(|| invalid("reply has no supported canonical body count"))?;
            let mut body = Vec::with_capacity(count);
            let mut bytes = 0usize;
            for _ in 0..count {
                let line = self.line_until(until)?;
                bytes = bytes
                    .checked_add(line.len() + 1)
                    .filter(|bytes| *bytes <= MAX_LINE_BYTES)
                    .ok_or_else(|| invalid("counted reply exceeds its aggregate byte bound"))?;
                body.push(line);
            }
            if !self.pending.is_empty() {
                return Err(invalid("reply exceeds its declared body count"));
            }
            Ok((header, body))
        })();
        self.finish(result, until)
    }

    // Called only on the fresh connection EventReader::connect creates, never on authority.
    fn subscribe_until(mut self, sid: &str, until: Instant) -> io::Result<EventReader> {
        if !session_id(sid) {
            return Err(invalid("invalid subscription session ID"));
        }
        let request = format!("subscribe @{sid} events");
        self.prepare(&request)?;
        let result = (|| {
            self.write_until(format!("{request}\n").as_bytes(), until)?;
            // From the header on, aterm has answered: what does not open the
            // stream is its word ([`Unsubscribed`]), unless the rest of the
            // acknowledgement never arrives.
            let header = self.line_until(until)?;
            let answer = |why| Unsubscribed::error(&header, why);
            if header == "ERR" || header.starts_with("ERR ") {
                return Err(answer("subscription refused"));
            }
            if header != "OK subscribe 1" {
                return Err(answer("subscription ACK must name exactly one target"));
            }
            let row = self.line_until(until)?;
            let mut words = row.split_ascii_whitespace();
            if words.next() != Some("sub") {
                return Err(answer("subscription ACK has no sub row"));
            }
            let local = words
                .next()
                .ok_or_else(|| answer("subscription ACK has no local ID"))?;
            canonical_u64(local)
                .map_err(|_| answer("subscription ACK local ID is not canonical decimal"))?;
            if words.next() != Some(sid) {
                return Err(answer("subscription ACK names another target"));
            }
            // aterm ends the row with what it knows of the session at that
            // moment (`role=- attention=- user_title=-`, and `log_seq=…` on a
            // log watch). The harness reads none of it, and a later aterm may
            // add to it, so any `key=value` words are let through.
            if words.any(|word| !word.contains('=')) {
                return Err(answer(
                    "subscription ACK row has a word that is not key=value",
                ));
            }
            Ok(local.to_string())
        })();
        let local = self.finish(result, until)?;
        self.stream.set_nonblocking(true)?;
        Ok(EventReader {
            stream: self.stream,
            pending: self.pending,
            local,
            ended: false,
        })
    }

    /// Consume this connection into bounded, non-writing custody, including an ambiguous failed exchange.
    /// Pending reply bytes are deliberately never interpreted by custody. The owner must
    /// call `CustodyFd::retain_until_deadline` on its normal supervision tick or drop it sooner.
    ///
    /// # Errors
    /// `maximum_age` is zero, exceeds `MAX_CUSTODY_AGE`, or overflows the monotonic clock. The error returns
    /// this connection intact, including a potentially current ambiguous claim socket.
    pub fn into_custody(self, maximum_age: Duration) -> Result<CustodyFd, CustodyRefusal> {
        let until = match CustodyDeadline::after(maximum_age) {
            Ok(until) => until,
            Err(error) => {
                return Err(CustodyRefusal {
                    connection: self,
                    error,
                });
            }
        };
        self.into_custody_until(until)
    }

    /// Retain this connection under an existing absolute transaction deadline.
    /// Forward transfer and rollback keep the same deadline, without adding another budget.
    /// # Errors
    /// The deadline has elapsed or its clock cannot be read. Every error returns this
    /// connection intact, including an ambiguous authority connection.
    pub fn into_custody_until(self, until: CustodyDeadline) -> Result<CustodyFd, CustodyRefusal> {
        if let Err(error) = until.remaining() {
            return Err(CustodyRefusal {
                connection: self,
                error,
            });
        }
        Ok(CustodyFd {
            fd: self.stream.into(),
            until,
            ambiguous: self.poisoned || !self.pending.is_empty(),
        })
    }
}

fn canonical_u64(value: &str) -> io::Result<u64> {
    if value.is_empty()
        || (value.len() > 1 && value.starts_with('0'))
        || !value.bytes().all(|byte| byte.is_ascii_digit())
    {
        return Err(invalid("expected canonical unsigned decimal"));
    }
    value
        .parse()
        .map_err(|_| invalid("unsigned decimal overflows"))
}

/// What a typed write names: the claim it is made under and the person's count it
/// was judged at. aterm judges all four again when it makes the write: the claim
/// under the session's meta lock, the count under the terminal lock the write is
/// made under (`aterm/crates/aterm-gui/src/control_input.rs`,
/// `take_leading_options` and `input_if_fenced_observed`; `session_timeline.rs`,
/// `SupervisorInputFence::authorize`):
///
/// * `if-supervisor=<holder>`, `if-supervisor-revision=<n>` and
///   `harness-duty=<scope>`, together and once each, or the whole write is
///   `ERR usage: typed supervisor input takes …`. The holder must be the
///   standing contract-3 claim's, the revision its `status supervisor_revision=`,
///   the duty one the claim takes and a typing duty ([`Scope::types`]), and the
///   write must come on the connection that made the claim; otherwise it is
///   `ERR denied supervisor input authority`, nothing written.
/// * `if-human=<n>`, the `human_seq=` the write was judged at: a person's
///   gesture since skips it (`OK skipped reason=person seq=<n>`, or a turn's
///   `OK 0 turn skipped reason=person … submitted=0 …`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TypedFence<'a> {
    /// The claim's holder, `claude-code-alab@<pid>` ([`crate::aterm::holder`]).
    pub holder: &'a str,
    /// The claim's `supervisor_revision=` as `status` read it.
    pub revision: u64,
    /// The typing duty the write performs.
    pub duty: Scope,
    /// The `human_seq=` the write was judged at.
    pub human_seq: u64,
}

impl TypedFence<'_> {
    /// The leading words that name the claim, in the order aterm's own engine
    /// writes them (`aterm/crates/aterm-agent/src/harness/upgrade_drive.rs`,
    /// `authority_words`). `None` for a duty that grants no typing or a holder
    /// that is not one wire word.
    fn authority(&self) -> Option<String> {
        let holder_ok = !self.holder.is_empty()
            && self.holder != "-"
            && self
                .holder
                .bytes()
                .all(|byte| byte.is_ascii_graphic() && byte != b'=');
        (self.duty.types() && holder_ok).then(|| {
            format!(
                "if-supervisor={} if-supervisor-revision={} harness-duty={}",
                self.holder,
                self.revision,
                self.duty.word()
            )
        })
    }
}

/// The quiet that settles a typed turn's screen, `idle=`: what aterm's own engine
/// gives its turns (`aterm/crates/aterm-agent/src/supervise/run.rs`, `TURN_IDLE`
/// and `TURN_TIMEOUT`).
pub const TURN_IDLE_MS: u64 = 600;
/// The whole turn's bound, `timeout=` ([`TURN_IDLE_MS`]).
pub const TURN_TIMEOUT_MS: u64 = 2_500;

/// Whether `text` can be typed as one line: not empty or blank, and no control
/// character (a request is one line, and `turn` drops a control character where
/// `send` would write it as typed).
fn typable(text: &str) -> bool {
    !text.trim().is_empty() && !text.chars().any(char::is_control)
}

/// `@<sid> send <fence> if-human=<n> -- <text>`: `text` written as typed, under
/// `fence`. aterm answers `OK seq=<n>`; `send` writes a trailing `\n` (the two
/// characters) as Enter (`aterm/crates/aterm-types/src/control_verbs.rs`, the
/// `send` entry), and `--` ends the options, so a text that looks like one is
/// typed as text. `None`, nothing to send, when `sid` is not a session ID,
/// `fence` names no typing authority, or `text` is not [`typable`].
#[must_use]
pub fn send_verb(sid: &str, fence: &TypedFence<'_>, text: &str) -> Option<String> {
    if !session_id(sid) || !typable(text) {
        return None;
    }
    let authority = fence.authority()?;
    Some(format!(
        "@{sid} send {authority} if-human={} -- {text}",
        fence.human_seq
    ))
}

/// `@<sid> key <fence> if-human=<n> <name>`: one named key (`enter`, `esc`,
/// `1`, `ctrl+c`) pressed under `fence`. aterm answers `OK seq=<n>`. `None`
/// when `sid` is not a session ID, `fence` names no typing authority, or `key`
/// is not one word of at most 32 ASCII graphic bytes that no option reads as
/// its own (no `=`, not `--`).
#[must_use]
pub fn key_verb(sid: &str, fence: &TypedFence<'_>, key: &str) -> Option<String> {
    let key_ok = !key.is_empty()
        && key.len() <= 32
        && key != "--"
        && key
            .bytes()
            .all(|byte| byte.is_ascii_graphic() && byte != b'=');
    if !session_id(sid) || !key_ok {
        return None;
    }
    let authority = fence.authority()?;
    Some(format!(
        "@{sid} key {authority} if-human={} {key}",
        fence.human_seq
    ))
}

/// `@<sid> turn <fence> submit=guarded:<guard> if-human=<n> idle=<ms> timeout=<ms> -- <text>`:
/// `text` typed and submitted with Enter, the Enter pressed only while the
/// cursor's row matches `guard` and no person has made a gesture since
/// `if-human=`, both judged under the terminal lock that press is written under
/// (`aterm/crates/aterm-types/src/verb_page/turn.rs`, "submit" and "fences").
/// The fence words are the turn's leading run and the rest its options
/// (`control_verbs.rs`, `TURN_OPTIONS`), which `--` ends. aterm answers
/// `OK <n> turn submitted=<0|1> … seq=<n> id=<n> …` and the `<n>` rows it
/// settled on, `OK 0 turn skipped reason=<why> …`, or an `ERR`.
///
/// `None` when `sid` is not a session ID, `fence` names no typing authority,
/// `text` is not [`typable`], or `guard` is not one nonempty word of at most
/// [`alab_policy::guard::MAX_GUARD_BYTES`].
#[must_use]
pub fn turn_verb(sid: &str, fence: &TypedFence<'_>, guard: &str, text: &str) -> Option<String> {
    let guard_ok = !guard.is_empty()
        && guard.len() <= alab_policy::guard::MAX_GUARD_BYTES
        && !guard.chars().any(|c| c.is_whitespace() || c.is_control());
    if !session_id(sid) || !typable(text) || !guard_ok {
        return None;
    }
    let authority = fence.authority()?;
    Some(format!(
        "@{sid} turn {authority} submit=guarded:{guard} if-human={} idle={TURN_IDLE_MS} \
         timeout={TURN_TIMEOUT_MS} -- {text}",
        fence.human_seq
    ))
}

/// What a nonblocking event drain observed. Every loss or fault requires fresh policy.
#[derive(Debug, Default)]
pub struct EventUpdate {
    /// A matching `EVENT <local> harness-policy` arrived.
    pub policy_changed: bool,
    /// The host reported a `GAP` for the subscribed target.
    pub gap: bool,
    /// The bounded drain or line budget overflowed; the stream was closed.
    pub overflow: bool,
    /// The event connection ended and must be opened again.
    pub ended: bool,
    /// A read, framing, target or UTF-8 failure, retained alongside preceding events.
    pub fault: Option<io::Error>,
    /// The last `EVENT <local> restart cause=<why>` of this drain: the host asks the
    /// claim holder to restart its child. It is a request, never permission: the
    /// holder still reads fresh policy and status before it acts.
    pub restart: Option<Restart>,
    /// An `agent`, `turn`, `human` or `status` event arrived: what a safe-point read
    /// judges may have moved, so the next look need not wait for its poll.
    pub activity: bool,
}

impl EventUpdate {
    /// Whether cached policy must be invalidated, including event loss and disconnection.
    #[must_use]
    pub fn requires_refresh(&self) -> bool {
        self.policy_changed || self.gap || self.overflow || self.ended || self.fault.is_some()
    }
}

/// A separate subscription connection exposing no request-writing API.
#[derive(Debug)]
pub struct EventReader {
    stream: UnixStream,
    pending: Vec<u8>,
    local: String,
    ended: bool,
}

impl EventReader {
    /// Open a fresh, separate connection and subscribe to exactly this session's events.
    /// No authority connection is consumed and no supervisor claim is requested.
    /// One deadline covers connection, token read, authentication, version and subscription ACK.
    ///
    /// # Errors
    /// Connection/authentication fails, or the session, exchange, count or ACK target is invalid.
    /// Once aterm has answered `subscribe`, an `ERR` line or an acknowledgement that does not
    /// parse carries its answer ([`Unsubscribed::of`]); no other error does.
    pub fn connect(socket: &Path, sid: &str, timeout: Duration) -> io::Result<Self> {
        Self::connect_with_clock(socket, sid, timeout, Instant::now)
    }

    fn connect_with_clock(
        socket: &Path,
        sid: &str,
        timeout: Duration,
        clock: fn() -> Instant,
    ) -> io::Result<Self> {
        let until = deadline_at(timeout, clock())?;
        Connection::open_until(socket, timeout, until, clock)?.subscribe_until(sid, until)
    }

    fn end(&mut self, update: &mut EventUpdate) {
        self.ended = true;
        update.ended = true;
        let _ = self.stream.shutdown(Shutdown::Both);
    }

    fn lines(&mut self, update: &mut EventUpdate) -> io::Result<()> {
        while let Some(end) = self.pending.iter().position(|byte| *byte == b'\n') {
            if end > MAX_LINE_BYTES {
                update.overflow = true;
                return Err(invalid("event line exceeds its bound"));
            }
            let mut bytes: Vec<u8> = self.pending.drain(..=end).collect();
            bytes.pop();
            if bytes.last() == Some(&b'\r') {
                bytes.pop();
            }
            let line = String::from_utf8(bytes).map_err(|_| invalid("event line is not UTF-8"))?;
            let mut words = line.split_ascii_whitespace();
            let kind = words.next();
            if !matches!(kind, Some("EVENT" | "GAP")) || words.next() != Some(self.local.as_str()) {
                return Err(invalid("event line names an unexpected target or stream"));
            }
            if kind == Some("GAP") {
                update.gap = true;
                continue;
            }
            let event = words
                .next()
                .ok_or_else(|| invalid("event line has no event kind"))?;
            match event {
                "harness-policy" => {
                    update.policy_changed = true;
                    let generation = words
                        .next()
                        .and_then(|word| word.strip_prefix("gen="))
                        .ok_or_else(|| invalid("policy event has no generation"))?;
                    canonical_u64(generation)?;
                    // A later host may say more about the move; a second generation
                    // is still two answers to one question.
                    if words.any(|word| word.starts_with("gen=")) {
                        return Err(invalid("policy event names its generation twice"));
                    }
                }
                "restart" => {
                    update.restart = Some(
                        Restart::parse(words)
                            .ok_or_else(|| invalid("restart event has no single cause"))?,
                    );
                }
                "agent" | "turn" | "human" | "status" => update.activity = true,
                _ => {}
            }
        }
        if self.pending.len() > MAX_LINE_BYTES {
            update.overflow = true;
            return Err(invalid("event line exceeds its bound"));
        }
        Ok(())
    }

    /// Drain without waiting, retaining policy invalidation even if a later event is malformed.
    /// A busy stream is closed once this call's byte budget is spent; overflow never silently discards an event.
    #[must_use]
    pub fn drain(&mut self) -> EventUpdate {
        let mut update = EventUpdate::default();
        if self.ended {
            update.ended = true;
            return update;
        }
        let mut received = self.pending.len();
        let mut reads = 0;
        loop {
            if let Err(error) = self.lines(&mut update) {
                update.fault = Some(error);
                self.end(&mut update);
                break;
            }
            if received >= EVENT_DRAIN_BYTES || reads >= EVENT_DRAIN_READS {
                update.overflow = true;
                self.end(&mut update);
                break;
            }
            let mut bytes = [0_u8; 4096];
            let room = bytes.len().min(EVENT_DRAIN_BYTES - received);
            reads += 1;
            match self.stream.read(&mut bytes[..room]) {
                Ok(0) => {
                    self.end(&mut update);
                    break;
                }
                Ok(read) => {
                    // A reader reporting more than it was given is refused, never sliced past.
                    let (Some(total), Some(chunk)) =
                        (received.checked_add(read), bytes.get(..read))
                    else {
                        update.fault = Some(invalid("an event read exceeded its buffer"));
                        self.end(&mut update);
                        break;
                    };
                    received = total;
                    self.pending.extend_from_slice(chunk);
                }
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) => {
                    update.fault = Some(error);
                    self.end(&mut update);
                    break;
                }
            }
        }
        update
    }
}

impl AsRawFd for EventReader {
    /// The subscription's descriptor, for a caller that waits on it beside its own
    /// (`SignalPipe::wait_with`). Readable means [`EventReader::drain`] will not find
    /// the stream idle: an event, the end of the stream, or its error.
    fn as_raw_fd(&self) -> RawFd {
        self.stream.as_raw_fd()
    }
}

/// The old authority socket held only as custody through one exec, never for requests.
#[derive(Debug)]
pub struct CustodyFd {
    fd: OwnedFd,
    until: CustodyDeadline,
    ambiguous: bool,
}

/// An absolute native monotonic deadline that survives same-process exec.
/// It belongs to this boot, and must be carried unchanged through forward and rollback.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CustodyDeadline(u64);

fn native_monotonic_ns() -> io::Result<u64> {
    let mut now = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: now is a writable timespec; CLOCK_MONOTONIC has no caller-owned state.
    if unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &raw mut now) } != 0 {
        return Err(io::Error::last_os_error());
    }
    let seconds = u64::try_from(now.tv_sec).map_err(|_| invalid("negative monotonic clock"))?;
    let nanos = u64::try_from(now.tv_nsec)
        .ok()
        .filter(|n| *n < 1_000_000_000)
        .ok_or_else(|| invalid("invalid monotonic nanoseconds"))?;
    seconds
        .checked_mul(1_000_000_000)
        .and_then(|n| n.checked_add(nanos))
        .ok_or_else(|| invalid("monotonic clock overflows"))
}

impl CustodyDeadline {
    /// Start one positive custody budget. Do not call again when transferring that custody.
    /// # Errors
    /// The budget is zero, exceeds `MAX_CUSTODY_AGE`, overflows, or the native clock is unreadable.
    pub fn after(maximum_age: Duration) -> io::Result<Self> {
        if maximum_age > MAX_CUSTODY_AGE {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "custody age exceeds bound",
            ));
        }
        let age = u64::try_from(maximum_age.as_nanos())
            .ok()
            .filter(|n| *n > 0)
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "invalid custody age"))?;
        native_monotonic_ns()?
            .checked_add(age)
            .map(Self)
            .ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidInput, "custody deadline overflows")
            })
    }

    /// Recover the original absolute deadline from a validated same-process capsule.
    /// # Errors
    /// The deadline is zero, has elapsed, exceeds the maximum remaining span, or the clock is unreadable.
    pub fn from_monotonic_ns(expires_at_ns: u64) -> io::Result<Self> {
        let until = Self::from_recorded_monotonic_ns(expires_at_ns)?;
        until.remaining()?;
        Ok(until)
    }

    /// Read an original deadline for custody reconciliation, including one already elapsed.
    /// This preserves the exact recorded clock value; it grants no new attempt budget.
    /// `remaining` and `allow_one_exec` still refuse an elapsed deadline.
    /// # Errors
    /// The deadline is zero, exceeds the maximum future span, or the native clock is unreadable.
    pub fn from_recorded_monotonic_ns(expires_at_ns: u64) -> io::Result<Self> {
        if expires_at_ns == 0 {
            return Err(invalid("zero custody deadline"));
        }
        let future = expires_at_ns.saturating_sub(native_monotonic_ns()?);
        if Duration::from_nanos(future) > MAX_CUSTODY_AGE {
            return Err(invalid("custody deadline exceeds bound"));
        }
        Ok(Self(expires_at_ns))
    }

    /// The unchanged native value to carry through exec and rollback.
    #[must_use]
    pub fn monotonic_ns(self) -> u64 {
        self.0
    }

    /// Read the remaining part of this budget without renewing it.
    /// # Errors
    /// The deadline elapsed or the native clock cannot be read.
    pub fn remaining(self) -> io::Result<Duration> {
        self.0
            .checked_sub(native_monotonic_ns()?)
            .filter(|n| *n > 0)
            .map(Duration::from_nanos)
            .ok_or_else(|| io::Error::new(io::ErrorKind::TimedOut, "custody deadline elapsed"))
    }
}

/// A local custody-argument refusal retaining ownership of the original connection.
#[derive(Debug)]
pub struct CustodyRefusal {
    /// The unchanged connection, which may still own the host's current claim.
    pub connection: Connection,
    /// Why the requested local custody deadline is invalid.
    pub error: io::Error,
}

/// A refused inherited socket, still owned by the caller for recovery or rollback.
#[derive(Debug)]
pub struct InheritedCustodyRefusal {
    /// The original descriptor; no failed validation closes it or changes its flags.
    pub fd: OwnedFd,
    /// Why adoption into bounded, non-writing custody failed.
    pub error: io::Error,
}

impl AsRawFd for CustodyFd {
    fn as_raw_fd(&self) -> RawFd {
        self.fd.as_raw_fd()
    }
}

impl AsFd for CustodyFd {
    fn as_fd(&self) -> BorrowedFd<'_> {
        self.fd.as_fd()
    }
}

impl CustodyFd {
    /// The original deadline bounding attempts to transfer this custody descriptor.
    #[must_use]
    pub fn deadline(&self) -> CustodyDeadline {
        self.until
    }

    /// Whether an exchange failed or had unread bytes before entering custody.
    #[must_use]
    pub fn outcome_unknown(&self) -> bool {
        self.ambiguous
    }

    /// Keep custody before its deadline, or close it at expiry, without a background thread.
    /// An unreadable clock retains custody until a later tick can establish expiry.
    /// Expiry does not prove authority was released: unresolved authority owners must retain
    /// the descriptor separately until reconciliation, rather than call this expiry helper.
    #[must_use]
    pub fn retain_until_deadline(self) -> Option<Self> {
        if self
            .until
            .remaining()
            .is_err_and(|error| error.kind() == io::ErrorKind::TimedOut)
        {
            None
        } else {
            Some(self)
        }
    }

    /// Temporarily clear CLOEXEC after managed signals are blocked on a single native thread.
    /// Explicitly restore the returned guard before unblocking after exec failure.
    /// The new image must immediately reconstruct custody with `from_inherited`.
    ///
    /// # Errors
    /// Custody has expired, thread/signal quiescence is absent, or flag access fails.
    pub fn allow_one_exec(&self) -> io::Result<crate::sys::DescriptorExecGuard<'_>> {
        self.until.remaining()?;
        crate::sys::inherit_for_one_exec(self.as_fd())
    }

    /// Take an inherited socket into custody and restore CLOEXEC before any future spawn.
    /// This does not authenticate, adopt a claim, or create a request connection.
    ///
    /// # Errors
    /// The original absolute deadline is invalid or expired, the descriptor is standard,
    /// not a connected Unix stream, or cannot restore CLOEXEC. Every error returns ownership.
    pub fn from_inherited(
        fd: OwnedFd,
        expires_at_ns: u64,
    ) -> Result<Self, InheritedCustodyRefusal> {
        Self::from_inherited_checked(fd, CustodyDeadline::from_monotonic_ns(expires_at_ns))
    }

    /// Retain an inherited socket for same-operation recovery after a slow exec.
    /// Its original deadline may have elapsed; it is never renewed. This remains
    /// non-writing custody, not an authenticated or active request connection.
    /// Expiry still forbids `allow_one_exec`; recovery must observe the original
    /// operation separately without granting another mutation attempt.
    /// # Errors
    /// The recorded deadline is invalid, the descriptor is standard or not a connected
    /// Unix stream, or CLOEXEC cannot be restored. Every error returns the original fd.
    pub fn from_inherited_for_recovery(
        fd: OwnedFd,
        expires_at_ns: u64,
    ) -> Result<Self, InheritedCustodyRefusal> {
        Self::from_inherited_checked(
            fd,
            CustodyDeadline::from_recorded_monotonic_ns(expires_at_ns),
        )
    }

    fn from_inherited_checked(
        fd: OwnedFd,
        deadline: io::Result<CustodyDeadline>,
    ) -> Result<Self, InheritedCustodyRefusal> {
        let validated = (|| {
            let raw = fd.as_raw_fd();
            if raw < 3 {
                return Err(invalid("custody cannot own a standard descriptor"));
            }
            let until = deadline?;
            validate_custody_socket(raw)?;
            set_fd_flags(raw, fd_flags(raw)? | libc::FD_CLOEXEC)?;
            Ok(until)
        })();
        match validated {
            Ok(until) => Ok(Self {
                fd,
                until,
                ambiguous: true,
            }),
            Err(error) => Err(InheritedCustodyRefusal { fd, error }),
        }
    }
}

fn validate_custody_socket(raw: RawFd) -> io::Result<()> {
    let mut kind = 0 as libc::c_int;
    let mut length = std::mem::size_of_val(&kind) as libc::socklen_t;
    // SAFETY: kind and length are initialized, correctly sized writable locals.
    if unsafe {
        libc::getsockopt(
            raw,
            libc::SOL_SOCKET,
            libc::SO_TYPE,
            (&raw mut kind).cast(),
            &raw mut length,
        )
    } < 0
    {
        return Err(io::Error::last_os_error());
    }
    // getpeername writes at most the supplied size into this zeroed storage.
    let mut peer = blank_address_storage();
    let mut length = std::mem::size_of_val(&peer) as libc::socklen_t;
    // SAFETY: peer and length are initialized, correctly sized writable locals.
    if unsafe { libc::getpeername(raw, (&raw mut peer).cast(), &raw mut length) } < 0 {
        return Err(io::Error::last_os_error());
    }
    if kind != libc::SOCK_STREAM || peer.ss_family != libc::AF_UNIX as libc::sa_family_t {
        return Err(invalid("custody is not a connected Unix stream"));
    }
    Ok(())
}

#[cfg(test)]
#[path = "aterm_wire_tab_ids_tests.rs"]
mod tab_id_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    use std::io::{BufRead, BufReader};
    use std::os::unix::fs::{DirBuilderExt, symlink};
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::thread;
    use std::time::{SystemTime, UNIX_EPOCH};

    const END: Duration = Duration::from_secs(60);
    const SID: &str = "tqn5r6e0148";

    fn pair() -> (Connection, UnixStream) {
        let (client, peer) = UnixStream::pair().expect("Unix pair");
        peer.set_read_timeout(Some(END))
            .expect("peer read detector");
        peer.set_write_timeout(Some(END))
            .expect("peer write detector");
        (Connection::new(client, END).expect("connection"), peer)
    }

    fn read_request(peer: &mut UnixStream) -> String {
        let mut request = String::new();
        BufReader::new(peer)
            .read_line(&mut request)
            .expect("request");
        request
    }

    fn accept_peer(listener: &std::os::unix::net::UnixListener) -> UnixStream {
        let until = deadline(END).unwrap();
        let peer = loop {
            match listener.accept() {
                Ok((peer, _)) => break peer,
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    assert!(Instant::now() < until, "accept hang detector");
                    thread::yield_now();
                }
                Err(error) => panic!("accept: {error}"),
            }
        };
        // Darwin inherits the nonblocking listener's flag on accepted sockets.
        peer.set_nonblocking(false).unwrap();
        peer.set_read_timeout(Some(END)).unwrap();
        peer.set_write_timeout(Some(END)).unwrap();
        peer
    }

    fn world() -> PathBuf {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        // Keep the private listener test's filesystem socket below macOS's path bound.
        let parent = Path::new("/tmp/.ubd");
        std::fs::create_dir_all(parent).expect("scratch parent");
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        loop {
            let sequence = NEXT.fetch_add(1, Ordering::Relaxed);
            let path = parent.join(format!(
                "cw-{}-{stamp:x}-{sequence:x}.noindex",
                std::process::id()
            ));
            match std::fs::DirBuilder::new().mode(0o700).create(&path) {
                Ok(()) => return path,
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
                Err(error) => panic!("scratch world: {error}"),
            }
        }
    }

    #[test]
    fn discovery_and_tokens_follow_the_host_rule_and_reject_malformed_files() {
        assert!(Locator::from_values("../bad", None, Some(OsStr::new("/h"))).is_err());
        let locator = Locator::from_values(SID, Some(OsStr::new("/run/u")), None).unwrap();
        assert_eq!(locator.directory, Path::new("/run/u/aterm"));
        let locator =
            Locator::from_values(SID, Some(OsStr::new("")), Some(OsStr::new("/h"))).unwrap();
        assert_eq!(
            locator.directory,
            Path::new("/h/Library/Application Support/aterm")
        );
        assert_eq!(token_names("aterm-00077.sock"), ["aterm-77.token"]);
        assert_eq!(token_names("mine.sock"), ["mine.sock.token", "aterm.token"]);
        assert_eq!(
            token_names("aterm-77"),
            ["aterm-sock-aterm-77.token", "aterm.token"]
        );

        let root = world();
        std::fs::create_dir(root.join("graph")).unwrap();
        let graph = root.join("graph").join(SID);
        let locator = Locator::new(root.clone(), SID).unwrap();
        std::fs::write(&graph, "pid 77\nsock /first.sock\n").unwrap();
        assert_eq!(locator.socket().unwrap(), Path::new("/first.sock"));
        std::fs::write(&graph, "sock /second.sock\n").unwrap();
        assert_eq!(locator.socket().unwrap(), Path::new("/second.sock"));
        std::fs::write(&graph, "sock /a.sock\nsock /b.sock\n").unwrap();
        assert!(locator.socket().is_err());
        std::fs::write(&graph, "sock relative.sock\n").unwrap();
        assert!(locator.socket().is_err());

        let socket = root.join("mine.sock");
        std::fs::write(root.join("aterm.token"), "legacy-token\n").unwrap();
        assert_eq!(token_beside(&socket).unwrap(), "legacy-token");
        symlink(root.join("aterm.token"), root.join("mine.sock.token")).unwrap();
        assert_eq!(
            token_beside(&socket).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
        let oversized = root.join("aterm-78.token");
        std::fs::write(&oversized, vec![b'x'; MAX_TOKEN_BYTES + 1]).unwrap();
        assert!(token_beside(&root.join("aterm-78.sock")).is_err());
        assert!(
            token_beside(&root.join("aterm-79.sock")).is_err(),
            "instance sockets cannot use the legacy token"
        );
        std::fs::write(root.join("aterm-80.token"), "bad\u{0000}token").unwrap();
        assert!(token_beside(&root.join("aterm-80.sock")).is_err());
    }

    #[test]
    fn control_reads_reject_an_unrepresentable_sentinel_bound() {
        let root = world();
        let path = root.join("bounded-control");
        std::fs::write(&path, b"body").unwrap();
        assert_eq!(read_regular(&path, 4).unwrap(), "body");
        assert!(read_regular(&path, 3).is_err());
        assert!(read_regular(&path, usize::MAX).is_err());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn token_fallback_requires_absence_and_never_hides_an_existing_token_read_error() {
        let socket = world().join("private.sock");
        for kind in [io::ErrorKind::PermissionDenied, io::ErrorKind::Other] {
            let mut reads = 0;
            let result = token_beside_using(&socket, |_| {
                reads += 1;
                if reads == 1 {
                    Err(io::Error::new(kind, "controlled token read failure"))
                } else {
                    Ok("legacy-token".to_string())
                }
            });
            assert_eq!(result.unwrap_err().kind(), kind);
            assert_eq!(
                reads, 1,
                "an existing token read failure cannot widen authentication"
            );
        }
        let mut reads = 0;
        let token = token_beside_using(&socket, |_| {
            reads += 1;
            if reads == 1 {
                Err(io::Error::new(io::ErrorKind::NotFound, "absent token"))
            } else {
                Ok("legacy-token".to_string())
            }
        })
        .unwrap();
        assert_eq!(token, "legacy-token");
        assert_eq!(reads, 2);
    }

    #[test]
    fn authentication_has_no_separate_ack_and_fragments_share_one_reply() {
        let (client, mut peer) = UnixStream::pair().unwrap();
        peer.set_read_timeout(Some(END)).unwrap();
        peer.set_write_timeout(Some(END)).unwrap();
        let server = thread::spawn(move || {
            let mut reader = BufReader::new(&mut peer);
            let mut auth = String::new();
            let mut version = String::new();
            reader.read_line(&mut auth).unwrap();
            reader.read_line(&mut version).unwrap();
            assert_eq!(auth, "AUTH test-token\n");
            assert_eq!(version, "version\n");
            drop(reader);
            peer.write_all(b"OK version=test-host\n").unwrap();
            assert_eq!(read_request(&mut peer), format!("@{SID} status\n"));
            let reply = format!("OK schema=1 title={}\n", "é".repeat(8000));
            for fragment in reply.as_bytes().chunks(17) {
                peer.write_all(fragment).unwrap();
            }
        });
        let mut connection = Connection::authenticate(
            client,
            "test-token",
            END,
            deadline(END).unwrap(),
            Instant::now,
        )
        .unwrap();
        assert_eq!(connection.version(), "version=test-host");
        let fields = connection
            .request_fields(&format!("@{SID} status"))
            .unwrap();
        assert_eq!(fields.get("schema"), Some("1"));
        assert_eq!(fields.get("title"), Some("é".repeat(8000).as_str()));
        server.join().unwrap();
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn only_a_fully_closed_peer_can_refuse_timeout_updates_without_losing_queued_bytes() {
        for errno in [libc::EINVAL, libc::ENOTCONN] {
            let (mut near, mut far) = UnixStream::pair().unwrap();
            near.set_read_timeout(Some(END)).unwrap();
            let refusal = || Err(io::Error::from_raw_os_error(errno));
            assert!(
                arm_after_peer_close(&near, refusal()).is_err(),
                "an open peer must not bypass timer failures"
            );
            far.write_all(b"OK schema=1\n").unwrap();
            far.shutdown(Shutdown::Write).unwrap();
            assert!(!peer_fully_closed(&near));
            assert!(
                arm_after_peer_close(&near, refusal()).is_err(),
                "a half-closed peer must not bypass timer failures"
            );
            drop(far);
            let until = deadline(END).unwrap();
            while !peer_fully_closed(&near) {
                assert!(Instant::now() < until, "peer closure hang detector");
                thread::yield_now();
            }
            assert!(
                arm_after_peer_close(&near, Err(io::Error::from_raw_os_error(libc::EPERM)))
                    .is_err()
            );
            arm_after_peer_close(&near, refusal()).unwrap();
            let mut reply = Vec::new();
            near.read_to_end(&mut reply).unwrap();
            assert_eq!(reply, b"OK schema=1\n");
        }
    }

    #[test]
    fn direct_open_reads_its_instance_token_and_uses_a_real_private_unix_listener() {
        let root = world();
        let socket = root.join("aterm-777.sock");
        std::fs::write(root.join("aterm-777.token"), "private-token\n").unwrap();
        let listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
        listener.set_nonblocking(true).unwrap();
        let server = thread::spawn(move || {
            let mut peer = accept_peer(&listener);
            let mut reader = BufReader::new(&mut peer);
            let mut auth = String::new();
            let mut version = String::new();
            reader.read_line(&mut auth).unwrap();
            reader.read_line(&mut version).unwrap();
            assert_eq!(auth, "AUTH private-token\n");
            assert_eq!(version, "version\n");
            drop(reader);
            peer.write_all(b"OK version=private-test\n").unwrap();
        });
        let connection = Connection::connect(&socket, END).unwrap();
        assert_eq!(connection.version(), "version=private-test");
        assert_ne!(
            fd_flags(connection.stream.as_raw_fd()).unwrap() & libc::FD_CLOEXEC,
            0
        );
        server.join().unwrap();
    }

    #[test]
    fn event_opening_spends_one_original_budget_across_authentication_and_subscription() {
        let root = world();
        let socket = root.join("aterm-778.sock");
        std::fs::write(root.join("aterm-778.token"), "private-token\n").unwrap();
        let listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
        listener.set_nonblocking(true).unwrap();
        let server = thread::spawn(move || {
            let mut peer = accept_peer(&listener);
            let mut reader = BufReader::new(&mut peer);
            let mut auth = String::new();
            let mut version = String::new();
            reader.read_line(&mut auth).unwrap();
            reader.read_line(&mut version).unwrap();
            assert_eq!(auth, "AUTH private-token\n");
            assert_eq!(version, "version\n");
            drop(reader);
            peer.write_all(b"OK version=private-test\n").unwrap();
            let request = read_request(&mut peer);
            if !request.is_empty() {
                assert_eq!(request, format!("subscribe @{SID} events\n"));
                let _ = peer.write_all(b"OK subscribe 1\nsub 7 tqn5r6e0148\n");
            }
        });
        reset_clock();
        let error =
            EventReader::connect_with_clock(&socket, SID, END, advancing_clock).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
        server.join().unwrap();
    }

    #[test]
    fn malformed_utf8_headers_fields_and_truncated_replies_poison_without_a_second_write() {
        for reply in [
            b"OK schema=1\xff\n".as_slice(),
            b"OK schema=1".as_slice(),
            b"OK schema=1 schema=1\n".as_slice(),
            b"OKAY schema=1\n".as_slice(),
        ] {
            let (mut connection, mut peer) = pair();
            peer.write_all(reply).unwrap();
            peer.shutdown(Shutdown::Write).unwrap();
            assert!(
                connection
                    .request_fields(&format!("@{SID} status"))
                    .is_err()
            );
            assert!(connection.is_poisoned());
            assert_eq!(
                connection.request_line("status").unwrap_err().kind(),
                io::ErrorKind::NotConnected
            );
            assert_eq!(read_request(&mut peer), format!("@{SID} status\n"));
            peer.set_nonblocking(true).unwrap();
            let mut next = [0_u8; 1];
            assert_eq!(
                peer.read(&mut next).unwrap_err().kind(),
                io::ErrorKind::WouldBlock,
                "poison neither sends again nor closes a possibly-current claim"
            );
        }
        let (mut fresh, mut peer) = pair();
        peer.write_all(b"OK schema=1\n").unwrap();
        assert_eq!(
            fresh.request_fields("status").unwrap().get("schema"),
            Some("1")
        );
    }

    #[test]
    fn fields_and_percent_decoding_are_strict_without_assuming_unknown_fields() {
        let fields = Fields::parse("OK contract=2,3 effective=on continue_text=").unwrap();
        assert_eq!(fields.get("continue_text"), Some(""));
        assert_eq!(fields.get("composer"), None);
        for malformed in [
            "ERR contract=3",
            "OK contract=3 contract=2,3",
            "OK =on",
            "OK bare",
        ] {
            assert!(Fields::parse(malformed).is_err(), "{malformed}");
        }
        assert_eq!(
            percent_decode("claude-code-alab%40123").unwrap(),
            "claude-code-alab@123"
        );
        assert_eq!(percent_decode("%C3%A9").unwrap(), "é");
        for malformed in ["%", "%2", "%gg", "%ff"] {
            assert!(percent_decode(malformed).is_err());
        }
    }

    #[test]
    fn local_injection_is_unsent_and_an_explicit_err_does_not_poison_a_connection() {
        let (mut connection, mut peer) = pair();
        let server = thread::spawn(move || {
            assert_eq!(read_request(&mut peer), "status\n");
            peer.write_all(b"ERR denied\n").unwrap();
            assert_eq!(read_request(&mut peer), "status\n");
            peer.write_all(b"OK schema=1\n").unwrap();
        });
        assert_eq!(
            connection
                .request_line("status\nsend injected")
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidInput
        );
        assert!(!connection.is_poisoned());
        assert_eq!(connection.request_line("status").unwrap(), "ERR denied");
        assert!(!connection.is_poisoned());
        assert_eq!(
            connection.request_fields("status").unwrap().get("schema"),
            Some("1")
        );
        server.join().unwrap();
    }

    #[test]
    fn a_narrowed_timeout_bounds_the_next_exchange_and_poisons_on_expiry() {
        let (mut connection, mut peer) = pair();
        connection.set_timeout(Duration::from_millis(50));
        let started = Instant::now();
        // The peer reads the request and never answers.
        let error = connection.request_line("status").unwrap_err();
        assert!(
            matches!(
                error.kind(),
                io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
            ),
            "{error}"
        );
        assert!(
            started.elapsed() < Duration::from_secs(30),
            "the 60 s bound"
        );
        assert!(connection.is_poisoned());
        assert_eq!(read_request(&mut peer), "status\n");
        // Nothing more is written on a connection whose reply may still arrive.
        assert_eq!(
            connection.request_line("status").unwrap_err().kind(),
            io::ErrorKind::NotConnected
        );
    }

    #[test]
    fn an_oversized_reply_is_bounded_and_requires_a_fresh_connection() {
        let (mut connection, mut peer) = pair();
        let server = thread::spawn(move || {
            assert_eq!(read_request(&mut peer), "status\n");
            let mut reply = b"OK title=".to_vec();
            reply.extend(std::iter::repeat_n(b'x', MAX_LINE_BYTES));
            reply.push(b'\n');
            let _ = peer.write_all(&reply);
        });
        assert_eq!(
            connection.request_line("status").unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
        assert!(connection.is_poisoned());
        assert!(connection.pending.len() <= MAX_LINE_BYTES + 1);
        drop(connection);
        server.join().unwrap();
    }

    #[test]
    fn subscription_ack_requires_one_bound_target_and_preserves_coalesced_events() {
        for reply in [
            "OK subscribe 0\n",
            "OK subscribe 2\nsub 1 tqn5r6e0148\nsub 2 s-456\n",
            "OK subscribe 999999999999999999999999\n",
            "OK subscribe 1\nsub 1 s-456\n",
            "OK subscribe 1\nsub 01 tqn5r6e0148\n",
            "OK subscribe 1\nsub 1 tqn5r6e0148 extra\n",
        ] {
            let (connection, mut peer) = pair();
            peer.write_all(reply.as_bytes()).unwrap();
            assert!(
                connection
                    .subscribe_until(SID, deadline(END).unwrap())
                    .is_err(),
                "{reply}"
            );
        }
        let (connection, mut peer) = pair();
        peer.write_all(b"OK subscribe 1\nsub 7 tqn5r6e0148\nEVENT 7 harness-policy gen=2\nGAP 7 events-dropped=4\n").unwrap();
        let mut events = connection
            .subscribe_until(SID, deadline(END).unwrap())
            .unwrap();
        assert_eq!(
            read_request(&mut peer),
            format!("subscribe @{SID} events\n")
        );
        let update = events.drain();
        assert!(update.policy_changed && update.gap && update.requires_refresh());
        assert!(!update.ended && update.fault.is_none());
    }

    #[test]
    fn a_subscription_aterm_answered_is_told_apart_from_one_it_did_not() {
        let answer = |reply: &[u8]| {
            let (connection, mut peer) = pair();
            peer.write_all(reply).unwrap();
            // The request is still taken; only aterm's side has said its last.
            peer.shutdown(Shutdown::Write).unwrap();
            let error = connection
                .subscribe_until(SID, deadline(END).unwrap())
                .unwrap_err();
            let answered = Unsubscribed::of(&error)
                .map(|answer| (answer.header().to_string(), answer.refused()));
            drop(peer);
            answered
        };
        for (reply, header) in [
            (
                b"ERR subscription capacity busy; retry\n".as_slice(),
                "ERR subscription capacity busy; retry",
            ),
            (
                b"ERR usage: subscribe [@<sel>] <streams>\n".as_slice(),
                "ERR usage: subscribe [@<sel>] <streams>",
            ),
            (b"ERR\n".as_slice(), "ERR"),
        ] {
            assert_eq!(answer(reply), Some((header.to_string(), true)));
        }
        for (reply, header) in [
            (b"OK subscribe 0\n".as_slice(), "OK subscribe 0"),
            (
                b"OK subscribe 1\nsub 01 tqn5r6e0148\n".as_slice(),
                "OK subscribe 1",
            ),
            (
                b"OK subscribe 1\ntab 7 tqn5r6e0148\n".as_slice(),
                "OK subscribe 1",
            ),
            (b"ERRANT\n".as_slice(), "ERRANT"),
        ] {
            assert_eq!(answer(reply), Some((header.to_string(), false)));
        }
        // No header, or no row after it: aterm said nothing that stands.
        for reply in [
            b"".as_slice(),
            b"OK subscribe 1\n".as_slice(),
            b"ERR busy".as_slice(),
        ] {
            assert_eq!(answer(reply), None);
        }
    }

    #[test]
    fn event_faults_and_target_mismatch_retain_preceding_policy_invalidation() {
        for tail in [
            b"EVENT 7 human\xff\n".as_slice(),
            b"EVENT 8 harness-policy gen=3\n".as_slice(),
            b"EVENT 7 harness-policy gen=3 gen=4\n".as_slice(),
        ] {
            let (connection, mut peer) = pair();
            peer.write_all(b"OK subscribe 1\nsub 7 tqn5r6e0148\nEVENT 7 harness-policy gen=2\n")
                .unwrap();
            peer.write_all(tail).unwrap();
            let mut events = connection
                .subscribe_until(SID, deadline(END).unwrap())
                .unwrap();
            let update = events.drain();
            assert!(
                update.policy_changed
                    && update.ended
                    && update.fault.is_some()
                    && update.requires_refresh()
            );
            assert!(events.drain().requires_refresh());
        }
    }

    fn subscribed(lines: &[u8]) -> (EventReader, UnixStream) {
        let (connection, mut peer) = pair();
        // The row as aterm writes it, session facts after the id.
        peer.write_all(b"OK subscribe 1\nsub 7 tqn5r6e0148 role=- attention=- user_title=-\n")
            .unwrap();
        peer.write_all(lines).unwrap();
        let events = connection
            .subscribe_until(SID, deadline(END).unwrap())
            .unwrap();
        (events, peer)
    }

    #[test]
    fn the_subscription_row_may_end_in_fields_and_must_name_this_session() {
        let ack = |row: &str| {
            let (connection, mut peer) = pair();
            peer.write_all(format!("OK subscribe 1\n{row}\n").as_bytes())
                .unwrap();
            connection
                .subscribe_until(SID, deadline(END).unwrap())
                .map(|_| ())
                .map_err(|e| e.to_string())
        };
        for row in [
            "sub 7 tqn5r6e0148",
            "sub 7 tqn5r6e0148 role=- attention=- user_title=-",
            "sub 7 tqn5r6e0148 log_seq=4 log_ep=1 log_base=0 role=agent:x",
            "sub 7 tqn5r6e0148 later=word",
        ] {
            assert_eq!(ack(row), Ok(()), "{row}");
        }
        for (row, why) in [
            ("sub 7 s-999abc role=-", "names another target"),
            ("sub 7 role=- tqn5r6e0148", "names another target"),
            ("sub 7 tqn5r6e0148 s-999abc", "not key=value"),
            ("sub 7 tqn5r6e0148 role=- bare", "not key=value"),
            ("sub x tqn5r6e0148", ""),
            ("sub 7", "names another target"),
            ("tab 7 tqn5r6e0148", "no sub row"),
        ] {
            let got = ack(row).expect_err(row);
            assert!(got.contains(why), "{row}: {got}");
        }
    }

    #[test]
    fn lenient_fields_take_the_last_word_and_skip_bare_ones_but_keep_the_header_strict() {
        let fields = Fields::parse_lenient(
            "OK native-act state=info agent=working =x bare agent=idle a=b=c empty= x%20y=%41",
        )
        .unwrap();
        assert_eq!(fields.get("agent"), Some("idle"));
        assert_eq!(fields.get("state"), Some("info"));
        assert_eq!(fields.get("a"), Some("b=c"));
        assert_eq!(fields.get("empty"), Some(""));
        assert_eq!(fields.get("x%20y"), Some("%41"));
        for absent in ["native-act", "bare", "", "x", "OK"] {
            assert_eq!(fields.get(absent), None, "{absent:?}");
        }
        assert_eq!(Fields::parse_lenient("OK").unwrap().get("OK"), None);
        // The strict reader still refuses the same line, twice over.
        assert!(Fields::parse("OK agent=working agent=idle").is_err());
        assert!(Fields::parse("OK native-act state=info").is_err());
        for reply in ["", "ERR busy", "OKAY a=b", "ok a=b", "a=b", "OK=1 a=b"] {
            assert!(Fields::parse_lenient(reply).is_err(), "{reply:?}");
        }
        let long = format!("OK a={}", "x".repeat(MAX_LINE_BYTES));
        assert!(Fields::parse_lenient(&long).is_err());
    }

    #[test]
    fn a_restart_event_is_carried_and_activity_kinds_are_flagged() {
        let (mut events, _peer) = subscribed(b"EVENT 7 meta field=title value=-\nEVENT 7 render\n");
        let update = events.drain();
        assert!(update.restart.is_none() && !update.activity && !update.requires_refresh());

        for line in [
            b"EVENT 7 agent\n".as_slice(),
            b"EVENT 7 turn 12 submitted=1 status=done dur_ms=5 by=-\n".as_slice(),
            b"EVENT 7 human\n".as_slice(),
            b"EVENT 7 status\n".as_slice(),
        ] {
            let (mut events, _peer) = subscribed(line);
            let update = events.drain();
            assert!(update.activity, "{}", String::from_utf8_lossy(line));
            assert!(update.restart.is_none() && !update.requires_refresh());
        }
        for line in [
            b"EVENT 7 agents\n".as_slice(),
            b"EVENT 7 task\n".as_slice(),
            b"EVENT 7 closing\n".as_slice(),
            b"EVENT 7 act human\n".as_slice(),
            b"EVENT 7 harness-policy gen=2\n".as_slice(),
            b"GAP 7 events-dropped=4\n".as_slice(),
        ] {
            let (mut events, _peer) = subscribed(line);
            assert!(
                !events.drain().activity,
                "{}",
                String::from_utf8_lossy(line)
            );
        }

        let (mut events, mut peer) = subscribed(
            b"EVENT 7 human\nEVENT 7 restart cause=asked\nEVENT 7 harness-policy gen=9\n",
        );
        let update = events.drain();
        assert_eq!(
            update.restart,
            Some(Restart {
                cause: "asked".to_string()
            })
        );
        assert!(update.activity && update.policy_changed && !update.ended);
        assert!(update.fault.is_none());
        // Each drain reports only what it read; the last restart of a drain wins.
        assert!(events.drain().restart.is_none());
        peer.write_all(
            b"EVENT 7 restart cause=stall\nEVENT 7 restart cause=account to=work home=%2Fh\n",
        )
        .unwrap();
        assert_eq!(
            events.drain().restart.map(|restart| restart.cause),
            Some("account".to_string())
        );
    }

    #[test]
    fn a_restart_nobody_can_name_is_a_fault_that_keeps_what_came_before() {
        for tail in [
            b"EVENT 7 restart\n".as_slice(),
            b"EVENT 7 restart cause=\n".as_slice(),
            b"EVENT 7 restart to=work cause=account\n".as_slice(),
            b"EVENT 7 restart cause=asked cause=stall\n".as_slice(),
        ] {
            let (connection, mut peer) = pair();
            peer.write_all(
                b"OK subscribe 1\nsub 7 tqn5r6e0148\nEVENT 7 human\nEVENT 7 restart cause=asked\n",
            )
            .unwrap();
            peer.write_all(tail).unwrap();
            let mut events = connection
                .subscribe_until(SID, deadline(END).unwrap())
                .unwrap();
            let update = events.drain();
            assert!(update.fault.is_some() && update.ended && update.requires_refresh());
            assert!(update.activity);
            assert_eq!(
                update.restart.map(|restart| restart.cause).as_deref(),
                Some("asked")
            );
        }
    }

    #[test]
    fn a_policy_event_may_say_more_but_never_two_generations() {
        for line in [
            b"EVENT 7 harness-policy gen=3 why=config\n".as_slice(),
            b"EVENT 7 harness-policy gen=3 effective=off bare\n".as_slice(),
        ] {
            let (mut events, _peer) = subscribed(line);
            let update = events.drain();
            assert!(update.policy_changed && update.requires_refresh());
            assert!(update.fault.is_none() && !update.ended);
        }
        for line in [
            b"EVENT 7 harness-policy\n".as_slice(),
            b"EVENT 7 harness-policy why=config gen=3\n".as_slice(),
            b"EVENT 7 harness-policy gen=03\n".as_slice(),
            b"EVENT 7 harness-policy gen=3 why=config gen=4\n".as_slice(),
        ] {
            let (mut events, _peer) = subscribed(line);
            let update = events.drain();
            assert!(update.policy_changed && update.fault.is_some() && update.ended);
        }
    }

    #[test]
    fn the_event_descriptor_is_the_streams_and_polls_readable_only_with_something_to_drain() {
        let readable = |fd: RawFd, timeout_ms| {
            let mut probe = libc::pollfd {
                fd,
                events: libc::POLLIN,
                revents: 0,
            };
            // SAFETY: probe is one initialized live descriptor; the timeout only
            // bounds the wait for the peer write or close to become readable.
            let ready = unsafe { libc::poll(&raw mut probe, 1, timeout_ms) };
            ready > 0 && probe.revents & (libc::POLLIN | libc::POLLHUP) != 0
        };
        let (mut events, mut peer) = subscribed(b"");
        let fd = events.as_raw_fd();
        assert_eq!(fd, events.stream.as_raw_fd());
        assert!(fd_flags(fd).unwrap() & libc::FD_CLOEXEC != 0);
        assert!(!readable(fd, 0));
        peer.write_all(b"EVENT 7 human\n").unwrap();
        assert!(readable(fd, 60_000));
        assert!(events.drain().activity);
        assert!(!readable(fd, 0));
        // The end of the stream is readable too, and the drain reports it.
        // A close may finish asynchronously: wait for readiness instead of
        // requiring its notification to be visible in this exact instant.
        drop(peer);
        assert!(readable(fd, 60_000));
        assert!(events.drain().ended);
    }

    #[test]
    fn partial_events_wait_but_overflow_invalidates_and_closes_the_stream() {
        let (connection, mut peer) = pair();
        peer.write_all(b"OK subscribe 1\nsub 7 tqn5r6e0148\nEVENT 7 harness-pol")
            .unwrap();
        let mut events = connection
            .subscribe_until(SID, deadline(END).unwrap())
            .unwrap();
        assert!(!events.drain().requires_refresh());
        peer.write_all(b"icy gen=2\n").unwrap();
        assert!(events.drain().policy_changed);
        let server = thread::spawn(move || {
            // A deliberately unterminated event must not grow memory or lose invalidation.
            let _ = peer.write_all(&vec![b'x'; EVENT_DRAIN_BYTES + 4096]);
        });
        let until = deadline(END).unwrap();
        loop {
            let update = events.drain();
            if update.overflow {
                assert!(update.requires_refresh() && update.ended);
                break;
            }
            assert!(
                !update.ended,
                "stream ended without explicit overflow: {update:?}"
            );
            assert!(Instant::now() < until, "overflow hang detector");
            thread::yield_now();
        }
        server.join().unwrap();
    }

    thread_local! {
        static CLOCK_STEP: Cell<u64> = const { Cell::new(0) };
        static CLOCK_BASE: Cell<Option<Instant>> = const { Cell::new(None) };
    }

    fn reset_clock() {
        CLOCK_BASE.with(|base| base.set(Some(Instant::now())));
        CLOCK_STEP.with(|step| step.set(0));
    }

    fn advancing_clock() -> Instant {
        let base = CLOCK_BASE.with(|base| base.get().expect("controlled clock reset"));
        CLOCK_STEP.with(|step| {
            let now = base + Duration::from_secs(step.get() * 8);
            step.set(step.get() + 1);
            now
        })
    }

    #[test]
    fn fragmented_reply_spends_one_total_budget_and_poison_can_enter_bounded_custody() {
        let (mut connection, mut peer) = pair();
        reset_clock();
        connection.clock = advancing_clock;
        connection.timeout = END;
        let server = thread::spawn(move || {
            assert_eq!(read_request(&mut peer), "status\n");
            let _ = peer.write_all(format!("OK title={}\n", "x".repeat(32000)).as_bytes());
        });
        assert_eq!(
            connection.request_line("status").unwrap_err().kind(),
            io::ErrorKind::TimedOut
        );
        assert!(connection.is_poisoned());
        let custody = connection.into_custody(END).unwrap();
        assert!(custody.outcome_unknown());
        assert!(custody.deadline().remaining().is_ok());
        drop(custody);
        server.join().unwrap();
    }

    #[test]
    fn counted_observations_consume_the_body_before_the_next_request() {
        let (mut connection, mut peer) = pair();
        let server = thread::spawn(move || {
            assert_eq!(read_request(&mut peer), "text --json process-context\n");
            for part in ["OK ", "1\n", "{\"rows\":[\"a\",\"b\"]}", "\n"] {
                peer.write_all(part.as_bytes()).unwrap();
            }
            assert_eq!(read_request(&mut peer), "status\n");
            peer.write_all(b"OK schema=1\n").unwrap();
        });
        assert_eq!(
            connection
                .request_body("text --json process-context", 1)
                .unwrap(),
            ["{\"rows\":[\"a\",\"b\"]}"]
        );
        assert_eq!(connection.request_line("status").unwrap(), "OK schema=1");
        server.join().unwrap();
    }

    #[test]
    fn malformed_or_incomplete_counted_observations_poison_the_connection() {
        for reply in [
            "OK 01\n{}\n",
            "OK -1\n",
            "OK 2\n{}\n{}\n",
            "OK 1 extra\n{}\n",
            "ERR unavailable\n",
            "OK 1\nunfinished",
            "OK 1\n{}\nunsolicited\n",
        ] {
            let (mut connection, mut peer) = pair();
            peer.write_all(reply.as_bytes()).unwrap();
            peer.shutdown(Shutdown::Write).unwrap();
            assert!(
                connection.request_body("text --json", 1).is_err(),
                "{reply:?}"
            );
            assert!(connection.is_poisoned(), "{reply:?}");
            assert_eq!(
                connection.request_line("status").unwrap_err().kind(),
                io::ErrorKind::NotConnected
            );
        }
    }

    #[test]
    fn a_worded_counted_reply_keeps_its_header_and_an_err_has_no_body() {
        let (mut connection, mut peer) = pair();
        let server = thread::spawn(move || {
            assert_eq!(read_request(&mut peer), "turn -- hi\n");
            peer.write_all(b"OK 2 turn submitted=1 status=settled seq=9 id=3\n\xe2\x9d\xaf hi\n\n")
                .unwrap();
            assert_eq!(read_request(&mut peer), "turn -- hi\n");
            peer.write_all(b"ERR key delivery failed\n").unwrap();
            assert_eq!(read_request(&mut peer), "turn -- hi\n");
            peer.write_all(b"OK 0 turn skipped reason=person submitted=0 seq=9 id=4\n")
                .unwrap();
            assert_eq!(read_request(&mut peer), "status\n");
            peer.write_all(b"OK schema=1\n").unwrap();
        });
        assert_eq!(
            connection.request_counted("turn -- hi", 4).unwrap(),
            (
                "OK 2 turn submitted=1 status=settled seq=9 id=3".to_string(),
                vec!["\u{276f} hi".to_string(), String::new()]
            )
        );
        assert_eq!(
            connection.request_counted("turn -- hi", 4).unwrap(),
            ("ERR key delivery failed".to_string(), Vec::new())
        );
        assert_eq!(
            connection.request_counted("turn -- hi", 4).unwrap(),
            (
                "OK 0 turn skipped reason=person submitted=0 seq=9 id=4".to_string(),
                Vec::new()
            )
        );
        assert!(!connection.is_poisoned());
        assert_eq!(connection.request_line("status").unwrap(), "OK schema=1");
        server.join().unwrap();
    }

    #[test]
    fn a_worded_counted_reply_without_a_count_or_over_its_bound_poisons_the_connection() {
        for reply in [
            "OK turn submitted=1\n",
            "OK 01 turn\nx\n",
            "OK 5 turn\n",
            "OK 1 turn\nx\ny\n",
            "OK\n",
        ] {
            let (mut connection, mut peer) = pair();
            peer.write_all(reply.as_bytes()).unwrap();
            peer.shutdown(Shutdown::Write).unwrap();
            assert!(
                connection.request_counted("turn -- hi", 4).is_err(),
                "{reply:?}"
            );
            assert!(connection.is_poisoned(), "{reply:?}");
        }
    }

    #[test]
    fn counted_observations_keep_one_deadline_for_header_and_all_body_lines() {
        let (mut connection, mut peer) = pair();
        reset_clock();
        connection.clock = advancing_clock;
        let server = thread::spawn(move || {
            assert_eq!(read_request(&mut peer), "text --json\n");
            peer.write_all(b"OK 4\none\ntwo\nthree\nfour\n").unwrap();
        });
        assert_eq!(
            connection
                .request_body("text --json", 4)
                .unwrap_err()
                .kind(),
            io::ErrorKind::TimedOut
        );
        assert!(connection.is_poisoned());
        server.join().unwrap();
    }

    #[test]
    fn custody_keeps_an_ambiguous_socket_open_without_adoption() {
        let (mut connection, mut peer) = pair();
        peer.write_all(b"OK").unwrap();
        peer.shutdown(Shutdown::Write).unwrap();
        assert!(connection.request_line("status").is_err());
        assert_eq!(read_request(&mut peer), "status\n");
        let mut custody = connection.into_custody(END).unwrap();
        assert!(custody.outcome_unknown());
        assert_ne!(fd_flags(custody.as_raw_fd()).unwrap() & libc::FD_CLOEXEC, 0);
        peer.set_nonblocking(true).unwrap();
        assert_eq!(
            peer.read(&mut [0]).unwrap_err().kind(),
            io::ErrorKind::WouldBlock
        );
        custody.until = CustodyDeadline(1);
        assert!(custody.retain_until_deadline().is_none());
        assert_eq!(peer.read(&mut [0]).unwrap(), 0);

        let (inherited, _peer) = UnixStream::pair().unwrap();
        let until = CustodyDeadline::after(END).unwrap();
        let custody = CustodyFd::from_inherited(inherited.into(), until.monotonic_ns()).unwrap();
        assert_eq!(custody.deadline(), until);
        assert_ne!(fd_flags(custody.as_raw_fd()).unwrap() & libc::FD_CLOEXEC, 0);
    }

    #[test]
    fn rejected_inherited_custody_retains_the_same_socket_and_flags_for_rollback() {
        let (inherited, mut peer) = UnixStream::pair().unwrap();
        inherited.set_read_timeout(Some(END)).unwrap();
        let raw = inherited.as_raw_fd();
        let flags = fd_flags(raw).unwrap();
        assert_ne!(flags & libc::FD_CLOEXEC, 0);
        let mut fd: OwnedFd = inherited.into();
        for expires_at_ns in [0, 1, u64::MAX] {
            let refused = CustodyFd::from_inherited(fd, expires_at_ns).unwrap_err();
            assert_eq!(refused.fd.as_raw_fd(), raw);
            assert_eq!(fd_flags(raw).unwrap(), flags);
            fd = refused.fd;
            peer.set_nonblocking(true).unwrap();
            assert_eq!(
                peer.read(&mut [0]).unwrap_err().kind(),
                io::ErrorKind::WouldBlock
            );
        }
        let until = CustodyDeadline::after(END).unwrap();
        let custody = CustodyFd::from_inherited(fd, until.monotonic_ns()).unwrap();
        assert_eq!(
            custody.deadline(),
            until,
            "adoption cannot renew the budget"
        );
        peer.write_all(b"x").unwrap();
        let mut borrowed = std::fs::File::from(custody.fd.try_clone().unwrap());
        let mut byte = [0];
        borrowed.read_exact(&mut byte).unwrap();
        assert_eq!(byte, *b"x");
        drop(borrowed);
        drop(custody);
        assert_eq!(peer.read(&mut [0]).unwrap(), 0);
    }

    #[test]
    fn a_wrong_descriptor_is_returned_intact_and_a_second_transfer_keeps_the_deadline() {
        let file = std::fs::File::open("/dev/null").unwrap();
        let raw = file.as_raw_fd();
        let flags = fd_flags(raw).unwrap();
        let until = CustodyDeadline::after(END).unwrap();
        let refused = CustodyFd::from_inherited(file.into(), until.monotonic_ns()).unwrap_err();
        assert_eq!(refused.fd.as_raw_fd(), raw);
        assert_eq!(fd_flags(raw).unwrap(), flags);
        assert!(std::fs::File::from(refused.fd).metadata().is_ok());

        let (socket, _peer) = UnixStream::pair().unwrap();
        let custody = CustodyFd::from_inherited(socket.into(), until.monotonic_ns()).unwrap();
        let original_remaining = custody.deadline().remaining().unwrap();
        let fd = custody.fd;
        let adopted = CustodyFd::from_inherited(fd, until.monotonic_ns()).unwrap();
        assert_eq!(adopted.deadline(), until);
        assert!(adopted.deadline().remaining().unwrap() <= original_remaining);
    }

    #[test]
    fn invalid_custody_ages_return_the_same_live_request_connection() {
        let (mut connection, mut peer) = pair();
        let original = connection.stream.as_raw_fd();
        for age in [
            Duration::ZERO,
            MAX_CUSTODY_AGE + Duration::from_secs(1),
            Duration::MAX,
        ] {
            let refused = connection.into_custody(age).unwrap_err();
            assert_eq!(refused.error.kind(), io::ErrorKind::InvalidInput);
            connection = refused.connection;
            assert_eq!(connection.stream.as_raw_fd(), original);
            assert!(!connection.is_poisoned());
            assert!(fd_flags(original).is_ok());
        }
        let server = thread::spawn(move || {
            assert_eq!(read_request(&mut peer), "status\n");
            peer.write_all(b"OK schema=1\n").unwrap();
        });
        assert_eq!(
            connection.request_fields("status").unwrap().get("schema"),
            Some("1")
        );
        server.join().unwrap();

        let (mut ambiguous, mut peer) = pair();
        peer.write_all(b"OK").unwrap();
        peer.shutdown(Shutdown::Write).unwrap();
        assert!(ambiguous.request_line("status").is_err());
        let refused = ambiguous.into_custody(Duration::ZERO).unwrap_err();
        assert!(refused.connection.is_poisoned());
        assert_eq!(read_request(&mut peer), "status\n");
        peer.set_nonblocking(true).unwrap();
        assert_eq!(
            peer.read(&mut [0]).unwrap_err().kind(),
            io::ErrorKind::WouldBlock,
            "an invalid local custody age cannot close a possibly current claim socket"
        );
    }

    #[test]
    fn an_existing_custody_budget_is_not_renewed_and_expiry_retains_the_connection() {
        let (connection, mut peer) = pair();
        let original = connection.stream.as_raw_fd();
        let flags = fd_flags(original).unwrap();
        let refused = connection
            .into_custody_until(CustodyDeadline(1))
            .unwrap_err();
        assert_eq!(refused.error.kind(), io::ErrorKind::TimedOut);
        assert_eq!(refused.connection.stream.as_raw_fd(), original);
        assert_eq!(fd_flags(original).unwrap(), flags);
        assert!(!refused.connection.is_poisoned());
        peer.set_nonblocking(true).unwrap();
        assert_eq!(
            peer.read(&mut [0]).unwrap_err().kind(),
            io::ErrorKind::WouldBlock
        );
        let until = CustodyDeadline::after(END).unwrap();
        let custody = refused.connection.into_custody_until(until).unwrap();
        assert_eq!(custody.as_raw_fd(), original);
        assert_eq!(custody.deadline(), until);
        assert_eq!(fd_flags(original).unwrap(), flags);
        drop(custody);
        assert_eq!(peer.read(&mut [0]).unwrap(), 0);
    }

    #[test]
    fn expired_inherited_custody_retains_ownership_without_another_attempt() {
        let (socket, mut peer) = UnixStream::pair().unwrap();
        let raw = socket.as_raw_fd();
        let flags = fd_flags(raw).unwrap();
        let refused = CustodyFd::from_inherited(socket.into(), 1).unwrap_err();
        assert_eq!(refused.error.kind(), io::ErrorKind::TimedOut);
        let custody = CustodyFd::from_inherited_for_recovery(refused.fd, 1).unwrap();
        assert_eq!(custody.as_raw_fd(), raw);
        assert_eq!(custody.deadline().monotonic_ns(), 1);
        assert_eq!(
            custody.deadline().remaining().unwrap_err().kind(),
            io::ErrorKind::TimedOut
        );
        assert_eq!(
            custody.allow_one_exec().unwrap_err().kind(),
            io::ErrorKind::TimedOut
        );
        assert_eq!(fd_flags(raw).unwrap(), flags);
        assert!(custody.outcome_unknown());
        peer.set_nonblocking(true).unwrap();
        assert_eq!(
            peer.read(&mut [0]).unwrap_err().kind(),
            io::ErrorKind::WouldBlock
        );
        let custody = CustodyFd::from_inherited_for_recovery(custody.fd, 1).unwrap();
        assert_eq!(custody.deadline().monotonic_ns(), 1);
        drop(custody);
        assert_eq!(peer.read(&mut [0]).unwrap(), 0);
    }

    #[test]
    fn recovery_custody_refusals_keep_invalid_deadlines_and_non_sockets_owned() {
        let (socket, mut peer) = UnixStream::pair().unwrap();
        let raw = socket.as_raw_fd();
        let flags = fd_flags(raw).unwrap();
        let mut fd: OwnedFd = socket.into();
        for expires_at_ns in [0, u64::MAX] {
            let refused = CustodyFd::from_inherited_for_recovery(fd, expires_at_ns).unwrap_err();
            assert_eq!(refused.fd.as_raw_fd(), raw);
            assert_eq!(fd_flags(raw).unwrap(), flags);
            fd = refused.fd;
        }
        peer.set_nonblocking(true).unwrap();
        assert_eq!(
            peer.read(&mut [0]).unwrap_err().kind(),
            io::ErrorKind::WouldBlock
        );
        drop(fd);
        assert_eq!(peer.read(&mut [0]).unwrap(), 0);

        let file = std::fs::File::open("/dev/null").unwrap();
        let raw = file.as_raw_fd();
        let flags = fd_flags(raw).unwrap();
        let refused = CustodyFd::from_inherited_for_recovery(file.into(), 1).unwrap_err();
        assert_eq!(refused.fd.as_raw_fd(), raw);
        assert_eq!(fd_flags(raw).unwrap(), flags);
        assert!(std::fs::File::from(refused.fd).metadata().is_ok());
    }
}
