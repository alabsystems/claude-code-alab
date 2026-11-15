// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Claude's owned, headless stream-JSON SDK permission channel.
//!
//! A control response must answer one unpredictable pending request on the private input
//! channel while the same exact native executable remains observable. System output,
//! transcript rows and timestamps supply no ACK. These results are SDK observations, not
//! TUI mode leases or transition authority. Future input/moves still need exclusive controller,
//! quiescence and final native/context admission in their own scope.

use std::collections::BTreeSet;
use std::ffi::OsString;
use std::fs::{File, OpenOptions};
use std::io::{self, Read as _, Write as _};
use std::os::unix::fs::{MetadataExt as _, OpenOptionsExt as _};
use std::path::Path;
use std::process::ExitStatus;
use std::time::{Duration, Instant};

use crate::json::{self, Value};
use crate::permission::Mode;
use crate::procs::{self, ExecIdentity};
use crate::sys::{ProbeChild, ProbeSpawnRefusal};

/// Bound on all SDK output retained by one owned session.
pub const MAX_OUTPUT: usize = 1 << 20;
/// One qualification's monotonic hang detector, never extended by an ACK or output.
pub const WITHIN: Duration = Duration::from_secs(60);

fn invalid(why: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, why)
}

/// What the SDK explicitly acknowledged on its owned headless channel.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Reply {
    /// An initialize response explicitly states its current mode.
    CurrentMode(Mode),
    /// The SDK accepted this mode request; this is not a later current-mode read.
    ModeAccepted(Mode),
    /// The SDK explicitly rejected the request.
    Rejected(String),
}

/// An original SDK control response. Only an owned session can construct one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SdkObservation {
    request_id: String,
    source: ExecIdentity,
    reply: Reply,
}

impl SdkObservation {
    /// The exact acknowledged request, disclosed only after receipt.
    #[must_use]
    pub fn request_id(&self) -> &str {
        &self.request_id
    }
    /// The complete native executable observation bracketing this exchange.
    #[must_use]
    pub fn source(&self) -> &ExecIdentity {
        &self.source
    }
    /// The SDK's explicit answer, scoped only to this headless channel.
    #[must_use]
    pub fn reply(&self) -> &Reply {
        &self.reply
    }
}

/// Retained progress when a write or ACK becomes uncertain. It never permits a retry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pending {
    /// This operation's unpredictable request ID.
    pub request_id: String,
    /// Bytes positively reported written to the owned SDK pipe.
    pub written: usize,
    /// Complete framed request size.
    pub total: usize,
}

struct Records {
    input: File,
    stdout: File,
    stderr: File,
    acknowledgements: File,
}

impl Records {
    fn create(root: &Path) -> io::Result<Self> {
        let file = |name| {
            OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(root.join(name))
        };
        Ok(Self {
            input: file("requests.jsonl")?,
            stdout: file("stdout.jsonl")?,
            stderr: file("stderr.log")?,
            acknowledgements: file("acks.tsv")?,
        })
    }
}

/// A failed launch retains any successfully forked child and unrestored signal mask.
#[must_use = "settle the retained child/mask or retain this failure through terminal exit"]
pub struct StartFailure {
    refusal: ProbeSpawnRefusal,
    evidence: Option<Box<FailureEvidence>>,
}

struct FailureEvidence {
    records: Records,
    output: Vec<u8>,
    errors: Vec<u8>,
}

impl From<ProbeSpawnRefusal> for StartFailure {
    fn from(refusal: ProbeSpawnRefusal) -> Self {
        Self {
            refusal,
            evidence: None,
        }
    }
}

impl From<io::Error> for StartFailure {
    fn from(error: io::Error) -> Self {
        ProbeSpawnRefusal::from(error).into()
    }
}

impl std::fmt::Display for StartFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.refusal.error.fmt(f)
    }
}

impl std::fmt::Debug for StartFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.refusal.fmt(f)
    }
}

impl StartFailure {
    /// Attempt explicit bounded reconciliation. A false result retains every unsettled role.
    pub fn settle(&mut self) -> bool {
        if self
            .refusal
            .signals
            .as_mut()
            .is_some_and(|mask| mask.restore().is_err())
        {
            return false;
        }
        self.refusal.signals = None;
        if let Some(child) = self.refusal.child.as_mut() {
            let Some(evidence) = &mut self.evidence else {
                return false;
            };
            if child.cancel().is_err() {
                return false;
            }
            let until = Instant::now() + Duration::from_secs(2);
            while (child.pending() || !child.output_finished()) && Instant::now() < until {
                let out = evidence.output.len();
                let err = evidence.errors.len();
                let drained =
                    child.drain_total(&mut evidence.output, &mut evidence.errors, MAX_OUTPUT);
                if evidence
                    .records
                    .stdout
                    .write_all(&evidence.output[out..])
                    .is_err()
                    || evidence
                        .records
                        .stderr
                        .write_all(&evidence.errors[err..])
                        .is_err()
                {
                    return false;
                }
                if drained.is_err() {
                    return false;
                }
                if child.try_wait().is_err() {
                    return false;
                }
                std::thread::sleep(Duration::from_millis(2));
            }
            if child.pending() || !child.output_finished() {
                return false;
            }
        }
        self.refusal.child = None;
        true
    }
}

#[derive(Clone, Copy)]
enum Request {
    Initialize,
    Set(Mode),
    InvalidMode,
}

impl Request {
    fn frame(self, id: &str) -> Vec<u8> {
        let body = match self {
            Self::Initialize => "{\"subtype\":\"initialize\",\"hooks\":{}}".to_owned(),
            Self::Set(mode) => format!("{{\"subtype\":\"set_permission_mode\",\"mode\":\"{}\"}}", mode.setting()),
            Self::InvalidMode => "{\"subtype\":\"set_permission_mode\",\"mode\":\"__alab_invalid_permission_mode__\"}".to_owned(),
        };
        format!("{{\"type\":\"control_request\",\"request_id\":\"{id}\",\"request\":{body}}}\n")
            .into_bytes()
    }
}

fn reply(value: &Value, id: &str, request: Request) -> io::Result<Option<Reply>> {
    if value.get("type").and_then(Value::as_str) == Some("system") {
        return Ok(None);
    }
    if value.get("type").and_then(Value::as_str) != Some("control_response") {
        return Err(invalid(
            "unexpected non-control SDK output without a prompt",
        ));
    }
    let response = value
        .get("response")
        .ok_or_else(|| invalid("SDK ACK has no response"))?;
    if response.get("request_id").and_then(Value::as_str) != Some(id) {
        return Err(invalid("unknown, duplicate or out-of-order SDK ACK"));
    }
    match response.get("subtype").and_then(Value::as_str) {
        Some("error") => {
            if matches!(request, Request::InvalidMode)
                && response.get("error_code").and_then(Value::as_str) != Some("invalid_mode")
            {
                return Err(invalid(
                    "invalid-mode control did not receive its explicit vendor rejection",
                ));
            }
            let error = response
                .get("error")
                .and_then(Value::as_str)
                .filter(|error| !error.is_empty())
                .ok_or_else(|| invalid("SDK rejection has no error"))?;
            Ok(Some(Reply::Rejected(error.to_owned())))
        }
        Some("success") => {
            let body = response
                .get("response")
                .ok_or_else(|| invalid("successful SDK ACK has no body"))?;
            let field = match request {
                Request::Initialize => "current_permission_mode",
                _ => "mode",
            };
            let mode = body
                .get(field)
                .and_then(Value::as_str)
                .and_then(Mode::of_setting)
                .ok_or_else(|| invalid("successful SDK ACK has no recognized explicit mode"))?;
            match request {
                Request::Initialize => Ok(Some(Reply::CurrentMode(mode))),
                Request::Set(wanted) if mode == wanted => Ok(Some(Reply::ModeAccepted(mode))),
                _ => Err(invalid("SDK ACK does not confirm the requested mode")),
            }
        }
        _ => Err(invalid("SDK ACK has no recognized outcome")),
    }
}

fn request_id() -> io::Result<String> {
    let mut random = [0; 32];
    OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open("/dev/urandom")?
        .read_exact(&mut random)?;
    let mut id = String::from("alab-mode-");
    for byte in random {
        use std::fmt::Write as _;
        let _ = write!(id, "{byte:02x}");
    }
    Ok(id)
}

fn buffered_reply(
    line: &str,
    pending: Option<(&str, Request)>,
    answered: bool,
) -> io::Result<Option<Reply>> {
    let value = json::parse(line).ok_or_else(|| invalid("SDK output is not strict JSON"))?;
    if value.get("type").and_then(Value::as_str) == Some("system") {
        return Ok(None);
    }
    if answered {
        return Err(invalid(
            "duplicate or unsolicited SDK control output after ACK",
        ));
    }
    let (id, request) =
        pending.ok_or_else(|| invalid("unsolicited SDK control output before request"))?;
    reply(&value, id, request)
}

/// A private stdin/stdout SDK session, with sole child wait ownership.
/// Its lifetime must be explicitly completed or cancelled; it is not a TUI attachment.
#[must_use = "complete or cancel the owned SDK child and retain unsettled custody"]
pub struct Session {
    child: ProbeChild,
    source: Option<ExecIdentity>,
    until: Instant,
    records: Records,
    output: Vec<u8>,
    errors: Vec<u8>,
    parsed: usize,
    acknowledged: BTreeSet<String>,
    pending: Option<Pending>,
    poisoned: bool,
    status: Option<ExitStatus>,
}

impl Session {
    /// Start only the unmodified vendor in headless SDK mode, with private pipes.
    /// `records` must be an existing private evidence directory; files are never overwritten.
    /// # Errors
    /// Unsupported exact-exec observation, unsafe signal/quiescence state, preparation or spawn.
    pub fn start(vendor: &Path, records: &Path) -> Result<Self, StartFailure> {
        Self::start_until(vendor, records, Instant::now() + WITHIN)
    }

    /// Start with an earlier caller-owned monotonic deadline. It can narrow,
    /// never extend, the session's one-minute hang detector.
    /// # Errors
    /// As [`Self::start`], or the caller's deadline has already elapsed.
    pub fn start_until(
        vendor: &Path,
        records: &Path,
        until: Instant,
    ) -> Result<Self, StartFailure> {
        Self::start_inner(vendor, records, until.min(Instant::now() + WITHIN))
    }

    fn start_inner(vendor: &Path, records: &Path, until: Instant) -> Result<Self, StartFailure> {
        if Instant::now() >= until {
            return Err(
                io::Error::new(io::ErrorKind::TimedOut, "SDK start deadline expired").into(),
            );
        }
        if procs::capture_exact_exec(std::process::id()).is_none() {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "exact native exec generation is unavailable",
            )
            .into());
        }
        // SAFETY: sigaction is plain native output storage, filled by the query below.
        let mut action: libc::sigaction = unsafe { std::mem::zeroed() };
        // SAFETY: a valid signal query writes this initialized native record.
        if unsafe { libc::sigaction(libc::SIGPIPE, std::ptr::null(), &raw mut action) } != 0 {
            return Err(io::Error::last_os_error().into());
        }
        if action.sa_sigaction != libc::SIG_IGN {
            return Err(invalid("SDK pipe writes require inherited ignored SIGPIPE").into());
        }
        let vendor = vendor.canonicalize()?;
        let image = std::fs::metadata(&vendor)?;
        if !image.is_file() {
            return Err(invalid("SDK vendor image is not regular").into());
        }
        let records = Records::create(records)?;
        let args: Vec<OsString> = [
            "--print",
            "--input-format=stream-json",
            "--output-format=stream-json",
            "--verbose",
            "--permission-mode=default",
            "--settings",
            "{}",
            "--setting-sources",
            "",
        ]
        .into_iter()
        .map(OsString::from)
        .collect();
        let child = match ProbeChild::spawn_duplex(&vendor, &args, &[]) {
            Ok(child) => child,
            Err(refusal) => {
                return Err(StartFailure {
                    refusal,
                    evidence: Some(Box::new(FailureEvidence {
                        records,
                        output: Vec::new(),
                        errors: Vec::new(),
                    })),
                });
            }
        };
        let mut session = Self {
            child,
            source: None,
            until,
            records,
            output: Vec::new(),
            errors: Vec::new(),
            parsed: 0,
            acknowledged: BTreeSet::new(),
            pending: None,
            poisoned: false,
            status: None,
        };
        loop {
            if Instant::now() >= until {
                break;
            }
            let crossed_exec = match session.child.exec_observed() {
                Ok(done) => done,
                Err(_) => break,
            };
            if crossed_exec
                && let Some(identity) = session
                    .child
                    .observe_native(procs::capture_exact_exec)
                    .ok()
                    .flatten()
                && identity.process().executable.device == image.dev()
                && identity.process().executable.inode == image.ino()
            {
                session.source = Some(identity);
                if session.check_deadline().is_ok() {
                    return Ok(session);
                }
                break;
            }
            match session.child.try_wait() {
                Ok(Some(status)) => {
                    session.status = Some(status);
                    break;
                }
                Err(_) => break,
                _ if Instant::now() >= session.until => break,
                _ => std::thread::sleep(Duration::from_millis(2)),
            }
        }
        Err(StartFailure {
            refusal: ProbeSpawnRefusal {
                error: invalid("SDK source did not become the expected native image"),
                child: Some(Box::new(session.child)),
                signals: None,
            },
            evidence: Some(Box::new(FailureEvidence {
                records: session.records,
                output: session.output,
                errors: session.errors,
            })),
        })
    }

    /// Exact retained progress after uncertainty. A poisoned session never sends again.
    #[must_use]
    pub fn pending(&self) -> Option<&Pending> {
        self.pending.as_ref()
    }

    /// Ask the application to state its current SDK permission mode.
    /// # Errors
    /// The original native source changed, the operation became uncertain, or protocol refusal.
    pub fn current_mode(&mut self) -> io::Result<SdkObservation> {
        self.exchange(Request::Initialize)
    }

    /// Ask the SDK to change modes. A success is an ACK of that request, not TUI authority.
    /// # Errors
    /// As [`Self::current_mode`]. A known rejection is returned as an observation.
    pub fn set_mode(&mut self, mode: Mode) -> io::Result<SdkObservation> {
        self.exchange(Request::Set(mode))
    }

    fn exchange(&mut self, request: Request) -> io::Result<SdkObservation> {
        if self.poisoned || self.pending.is_some() {
            return Err(invalid(
                "SDK channel is poisoned; outcome must not be retried",
            ));
        }
        let result = self.exchange_inner(request);
        if result.is_err() {
            self.poisoned = true;
        }
        result
    }

    fn source_current(&mut self) -> io::Result<&ExecIdentity> {
        self.check_deadline()?;
        let current = self.child.observe_native(procs::capture_exact_exec)?;
        let source = self
            .source
            .as_ref()
            .filter(|source| current.as_ref() == Some(source))
            .ok_or_else(|| invalid("SDK source native executable or lifetime changed"))?;
        self.check_deadline()?;
        Ok(source)
    }

    fn check_deadline(&self) -> io::Result<()> {
        if Instant::now() >= self.until {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "SDK operation deadline expired",
            ));
        }
        Ok(())
    }

    /// Consume every available complete frame, including all frames following a
    /// matching ACK in the same read. An incomplete frame must finish before any
    /// new request or successful observation can be admitted.
    fn consume(&mut self, pending: Option<(&str, Request)>) -> io::Result<Option<(Reply, String)>> {
        let mut found = None;
        loop {
            self.check_deadline()?;
            self.drain()?;
            while let Some(end) = self.output[self.parsed..]
                .iter()
                .position(|byte| *byte == b'\n')
            {
                self.source_current()?;
                let end = self.parsed + end;
                let line = std::str::from_utf8(&self.output[self.parsed..end])
                    .map_err(|_| invalid("SDK output is not UTF-8"))?
                    .to_owned();
                let answer = buffered_reply(&line, pending, found.is_some())?;
                self.source_current()?;
                if let Some(answer) = answer {
                    found = Some((answer, line));
                }
                self.parsed = end + 1;
            }
            if self.parsed == self.output.len() {
                return Ok(found);
            }
            if self.child.output_finished() {
                return Err(invalid("SDK output ended with an incomplete frame"));
            }
            self.wait(false)?;
        }
    }

    fn exchange_inner(&mut self, request: Request) -> io::Result<SdkObservation> {
        // Never send over an already-known protocol violation, including a
        // buffered duplicate ACK left after the preceding exchange.
        self.consume(None)?;
        self.source_current()?;
        let id = request_id()?;
        if self.acknowledged.contains(&id) {
            return Err(invalid("SDK random request identity repeated"));
        }
        let frame = request.frame(&id);
        self.pending = Some(Pending {
            request_id: id.clone(),
            written: 0,
            total: frame.len(),
        });
        self.records.input.write_all(&frame)?;
        while self.pending.as_ref().unwrap().written < frame.len() {
            self.source_current()?;
            let offset = self.pending.as_ref().unwrap().written;
            match self.child.write_input(&frame[offset..]) {
                Ok(0) => {
                    return Err(io::Error::new(
                        io::ErrorKind::WriteZero,
                        "SDK input stopped",
                    ));
                }
                Ok(count) => {
                    self.pending.as_mut().unwrap().written += count;
                    self.check_deadline()?;
                }
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => self.wait(true)?,
                Err(error) => return Err(error),
            }
        }
        loop {
            if let Some((reply, line)) = self.consume(Some((&id, request)))? {
                if !self.acknowledged.insert(id.clone()) {
                    return Err(invalid("duplicate SDK ACK"));
                }
                let source = self.source_current()?.clone();
                writeln!(
                    self.records.acknowledgements,
                    "ack\t{}\t{}\t{}",
                    source.diagnostic(),
                    id,
                    line
                )?;
                self.source_current()?;
                self.pending = None;
                return Ok(SdkObservation {
                    request_id: id,
                    source,
                    reply,
                });
            }
            if self.child.output_finished() {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "SDK output ended without its complete ACK",
                ));
            }
            self.wait(false)?;
        }
    }

    fn drain(&mut self) -> io::Result<()> {
        let out = self.output.len();
        let err = self.errors.len();
        let result = self
            .child
            .drain_total(&mut self.output, &mut self.errors, MAX_OUTPUT);
        self.records.stdout.write_all(&self.output[out..])?;
        self.records.stderr.write_all(&self.errors[err..])?;
        result?;
        Ok(())
    }

    fn wait(&self, writing: bool) -> io::Result<()> {
        let mut polls: Vec<_> = self
            .child
            .output_fds()
            .map(|fd| libc::pollfd {
                fd,
                events: libc::POLLIN,
                revents: 0,
            })
            .collect();
        if writing && let Some(fd) = self.child.input_fd() {
            polls.push(libc::pollfd {
                fd,
                events: libc::POLLOUT,
                revents: 0,
            });
        }
        let left = self
            .until
            .checked_duration_since(Instant::now())
            .filter(|left| !left.is_zero())
            .ok_or_else(|| {
                io::Error::new(io::ErrorKind::TimedOut, "SDK operation deadline expired")
            })?;
        let millis = if polls.is_empty() {
            10
        } else {
            i32::try_from(left.as_millis().saturating_add(1)).unwrap_or(i32::MAX)
        };
        // SAFETY: all poll descriptors borrow this owned child for this bounded syscall.
        let result = unsafe { libc::poll(polls.as_mut_ptr(), polls.len() as libc::nfds_t, millis) };
        if result < 0 && io::Error::last_os_error().kind() != io::ErrorKind::Interrupted {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    /// Run the compatibility sequence, with no prompt or model request.
    /// # Errors
    /// Any missing, rejected, mismatched, duplicated or uncertain observation.
    pub fn qualify(&mut self) -> io::Result<Vec<SdkObservation>> {
        let expected = [
            Reply::CurrentMode(Mode::Default),
            Reply::ModeAccepted(Mode::Default),
            Reply::ModeAccepted(Mode::Plan),
            Reply::CurrentMode(Mode::Plan),
            Reply::Rejected(String::new()),
            Reply::ModeAccepted(Mode::Default),
            Reply::CurrentMode(Mode::Default),
        ];
        let requests = [
            Request::Initialize,
            Request::Set(Mode::Default),
            Request::Set(Mode::Plan),
            Request::Initialize,
            Request::InvalidMode,
            Request::Set(Mode::Default),
            Request::Initialize,
        ];
        let mut observed = Vec::new();
        for (request, expected) in requests.into_iter().zip(expected) {
            let value = self.exchange(request)?;
            if value.reply != expected
                && !matches!(
                    (&value.reply, expected),
                    (Reply::Rejected(_), Reply::Rejected(_))
                )
            {
                self.poisoned = true;
                return Err(invalid(
                    "SDK compatibility sequence returned the wrong application observation",
                ));
            }
            observed.push(value);
        }
        Ok(observed)
    }

    /// Close the owned SDK input and retain all remaining output through its terminal event.
    /// # Errors
    /// Pending/poisoned state, trailing unsolicited control output, nonzero exit or timeout.
    pub fn finish(&mut self) -> io::Result<ExitStatus> {
        if self.poisoned || self.pending.is_some() {
            return Err(invalid("SDK outcome remains uncertain"));
        }
        self.child.close_input();
        loop {
            self.check_deadline()?;
            self.drain()?;
            if self.status.is_none() {
                self.status = self.child.try_wait()?;
            }
            if self.status.is_some() && self.child.output_finished() {
                break;
            }
            self.wait(false)?;
        }
        let tail = std::str::from_utf8(&self.output[self.parsed..])
            .map_err(|_| invalid("SDK tail is not UTF-8"))?;
        for line in tail.split_inclusive('\n') {
            self.check_deadline()?;
            if !line.ends_with('\n')
                || json::parse(line)
                    .and_then(|value| value.get("type").and_then(Value::as_str).map(str::to_owned))
                    .as_deref()
                    != Some("system")
            {
                return Err(invalid(
                    "SDK has trailing incomplete or unsolicited control output",
                ));
            }
        }
        let status = self.status.unwrap();
        if !status.success() {
            return Err(invalid("SDK child did not exit successfully"));
        }
        self.check_deadline()?;
        Ok(status)
    }

    /// Explicit bounded cancellation; a false result retains the owning child receipt.
    pub fn cancel(&mut self) -> bool {
        self.child.close_input();
        if self.child.cancel().is_err() {
            return false;
        }
        let until = Instant::now() + Duration::from_secs(2);
        while (self.child.pending() || !self.child.output_finished()) && Instant::now() < until {
            let _ = self.drain();
            match self.child.try_wait() {
                Ok(Some(status)) => self.status = Some(status),
                Err(_) => return false,
                _ => {}
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        !self.child.pending() && self.child.output_finished()
    }
}

#[cfg(test)]
#[path = "sdk_permission_tests.rs"]
mod tests;
