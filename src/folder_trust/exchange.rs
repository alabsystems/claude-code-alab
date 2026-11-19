// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Private initialize-only SDK exchange; no prompt, model turn, hooks or MCP servers.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::io::{self, Write as _};
use std::os::unix::fs::OpenOptionsExt as _;
use std::path::Path;
use std::time::{Duration, Instant};

use crate::json::{self, Value};
use crate::sys::{ProbeChild, ProbeSpawnRefusal};

const OUTPUT_MAX: usize = 1024 * 1024;
const SETTLEMENT: Duration = Duration::from_secs(2);

#[derive(Default)]
struct Wire {
    output: Vec<u8>,
    errors: Vec<u8>,
    parsed: usize,
    scanned: usize,
    answer: Option<Option<String>>,
    failed: bool,
}

impl Wire {
    fn poison(&mut self) {
        self.failed = true;
        self.answer = None;
        self.output.clear();
        self.errors.clear();
        self.parsed = 0;
        self.scanned = 0;
    }

    fn read(&mut self, child: &mut ProbeChild, records: &mut Option<Records>) -> io::Result<()> {
        if self.failed {
            // A refused exchange can never admit another answer. Reuse its buffers
            // only to discard bounded chunks while positively retiring the pipes.
            self.output.clear();
            self.errors.clear();
            let _ = child.drain_total(&mut self.output, &mut self.errors, 4096);
            return Ok(());
        }
        let out = self.output.len();
        let err = self.errors.len();
        let drained = child.drain_total(&mut self.output, &mut self.errors, OUTPUT_MAX);
        let saved = records.as_mut().map_or(Ok(()), |records| {
            records.output(&self.output[out..], &self.errors[err..])
        });
        drained.and(saved)
    }

    /// Keep the same aggregate bytes and frame cursor through final EOF. Retirement
    /// may validate trailing frames, but can never admit a first late answer.
    fn parse(&mut self, id: &str, may_answer: bool) -> io::Result<bool> {
        if self.failed {
            return Ok(false);
        }
        let mut fresh = false;
        while let Some(end) = self.output[self.scanned..].iter().position(|b| *b == b'\n') {
            let end = self.scanned + end;
            let line = std::str::from_utf8(&self.output[self.parsed..end])
                .map_err(|_| super::invalid("SDK output is not UTF-8"))?;
            self.parsed = end + 1;
            self.scanned = self.parsed;
            if let Some(reply) = reply(line, id)? {
                if !may_answer || self.answer.is_some() {
                    return Err(super::invalid("early, late or duplicate SDK answer"));
                }
                self.answer = Some(reply);
                fresh = true;
            }
        }
        self.scanned = self.output.len();
        Ok(fresh)
    }

    fn finish(&self) -> io::Result<()> {
        if self.failed || self.parsed != self.output.len() {
            Err(super::invalid("refused or unterminated SDK output"))
        } else {
            Ok(())
        }
    }
}

struct Records {
    input: std::fs::File,
    output: std::fs::File,
    errors: std::fs::File,
    native: std::fs::File,
    remaining: usize,
    valid: bool,
}

impl Records {
    fn create(root: &Path) -> io::Result<Self> {
        let file = |name| {
            std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(root.join(name))
        };
        Ok(Self {
            input: file("request.jsonl")?,
            output: file("stdout.jsonl")?,
            errors: file("stderr.log")?,
            native: file("native.txt")?,
            remaining: OUTPUT_MAX,
            valid: true,
        })
    }

    fn output(&mut self, stdout: &[u8], stderr: &[u8]) -> io::Result<()> {
        let total = stdout
            .len()
            .checked_add(stderr.len())
            .ok_or_else(|| super::invalid("evidence count overflow"))?;
        let result = (|| {
            self.remaining = self
                .remaining
                .checked_sub(total)
                .ok_or_else(|| super::invalid("evidence output exceeded bound"))?;
            self.output.write_all(stdout)?;
            self.errors.write_all(stderr)
        })();
        self.valid &= result.is_ok();
        result
    }
}

pub(super) const ARGUMENTS: &[&str] = &[
    "-p",
    "--input-format",
    "stream-json",
    "--output-format",
    "stream-json",
    "--verbose",
    "--no-session-persistence",
    "--strict-mcp-config",
    "--settings",
    r#"{"disableAllHooks":true}"#,
];

pub(super) fn request(directory: &str, id: &str) -> Vec<u8> {
    let quote = crate::folder_trust::quote;
    format!("{{\"type\":\"control_request\",\"request_id\":{},\"request\":{{\"subtype\":\"initialize\",\"workspaceTrust\":{{\"accepted\":true,\"directory\":{}}}}}}}\n", quote(id),quote(directory)).into_bytes()
}

// Outer Some means this is the one matched answer. Inner Some means true was
// explicitly acknowledged. A matching decline is complete, never guessed success.
pub(super) fn reply(line: &str, id: &str) -> io::Result<Option<Option<String>>> {
    let value = json::parse(line).ok_or_else(|| super::invalid("SDK output is not strict JSON"))?;
    if value.get("type").and_then(Value::as_str) == Some("system") {
        return Ok(None);
    }
    if value.get("type").and_then(Value::as_str) != Some("control_response") {
        return Err(super::invalid(
            "unexpected SDK output without a model request",
        ));
    }
    let outer = value
        .get("response")
        .ok_or_else(|| super::invalid("SDK answer lacks response"))?;
    if outer.get("request_id").and_then(Value::as_str) != Some(id) {
        return Err(super::invalid("unsolicited SDK answer"));
    }
    let body = outer.get("response");
    let recorded = outer.get("subtype").and_then(Value::as_str) == Some("success")
        && body
            .and_then(|body| body.get("workspace_trust_recorded"))
            .and_then(Value::as_bool)
            == Some(true);
    let version = body
        .and_then(|body| body.get("claude_code_version"))
        .and_then(Value::as_str)
        .unwrap_or("");
    if version.len() > 4096 || version.chars().any(char::is_control) {
        return Err(super::invalid("invalid SDK version text"));
    }
    Ok(Some(recorded.then(|| version.to_owned())))
}

fn native(
    child: &mut ProbeChild,
    selected: &super::Selected,
    previous: Option<&crate::procs::ChildIdentity>,
) -> io::Result<crate::procs::ChildIdentity> {
    if !child.exec_observed()? {
        return Err(io::Error::from(io::ErrorKind::WouldBlock));
    }
    let seen = child
        .observe_native(crate::procs::capture_child)?
        .ok_or_else(|| {
            if previous.is_none() {
                io::Error::from(io::ErrorKind::WouldBlock)
            } else {
                super::invalid("original native SDK image is unavailable")
            }
        })?;
    if seen.executable.device != selected.stamp.0
        || seen.executable.inode != selected.stamp.1
        || seen.pgid != seen.pid
        || previous.is_some_and(|previous| previous != &seen)
    {
        return Err(super::invalid("original native SDK image changed"));
    }
    selected.current()?;
    Ok(seen)
}

fn wait(child: &ProbeChild, until: Instant, writable: bool) -> io::Result<()> {
    let left = until.saturating_duration_since(Instant::now());
    if left.is_zero() {
        // The phase loop handles expiry. Returning an I/O error here would
        // discard a reply that admission already validated.
        return Ok(());
    }
    let mut fds = child
        .output_fds()
        .map(|fd| libc::pollfd {
            fd,
            events: libc::POLLIN,
            revents: 0,
        })
        .collect::<Vec<_>>();
    if let Some(fd) = child.input_fd().filter(|_| writable) {
        fds.push(libc::pollfd {
            fd,
            events: libc::POLLOUT,
            revents: 0,
        });
    }
    let millis = i32::try_from(
        left.min(Duration::from_millis(10))
            .as_millis()
            .saturating_add(1),
    )
    .unwrap_or(10);
    // SAFETY: all descriptors borrow this same owned child, for one bounded wait.
    let got = unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as libc::nfds_t, millis) };
    if got < 0 && io::Error::last_os_error().kind() != io::ErrorKind::Interrupted {
        return Err(io::Error::last_os_error());
    }
    if fds.iter().any(|fd| fd.revents & libc::POLLNVAL != 0) {
        return Err(super::invalid("SDK pipe became invalid"));
    }
    Ok(())
}

fn cleanup(
    mut child: ProbeChild,
    records: &mut Option<Records>,
    wire: &mut Wire,
    id: &str,
    cancelled: &mut dyn FnMut() -> bool,
) -> Result<bool, ProbeSpawnRefusal> {
    child.close_input();
    let until = Instant::now() + SETTLEMENT;
    let result = (|| {
        child.cancel_cohort()?;
        while Instant::now() < until {
            // Collect cancellation without abandoning native settlement. The original
            // admission buffers and parser remain live until both pipes reach EOF.
            if cancelled() {
                wire.poison();
            }
            if wire.read(&mut child, records).is_err() || wire.parse(id, false).is_err() {
                wire.poison();
            }
            child.try_wait_cohort()?;
            if !child.pending() && child.output_finished() {
                if wire.finish().is_err() {
                    wire.poison();
                }
                return Ok(!wire.failed);
            }
            wait(&child, until, false)?;
        }
        Err(io::Error::new(
            io::ErrorKind::TimedOut,
            "SDK native cohort settlement remains pending",
        ))
    })();
    result.map_err(|error| ProbeSpawnRefusal {
        error,
        child: Some(Box::new(child)),
        signals: None,
        signal_pipe: None,
    })
}

pub(super) fn attest(
    selected: &super::Selected,
    cwd: &Path,
    env: &BTreeMap<OsString, OsString>,
    records: Option<&Path>,
    cancelled: &mut dyn FnMut() -> bool,
) -> Result<Option<String>, ProbeSpawnRefusal> {
    let Some(directory) = cwd.to_str() else {
        return Ok(None);
    };
    // Rust's entry policy ignores SIGPIPE. Refuse an embedding caller with another
    // disposition before any child or write, as the existing owned SDK channel does.
    // SAFETY: initialized native output storage for one disposition query.
    let mut action: libc::sigaction = unsafe { std::mem::zeroed() };
    // SAFETY: a null action reads only the caller's current disposition.
    if unsafe { libc::sigaction(libc::SIGPIPE, std::ptr::null(), &raw mut action) } != 0
        || action.sa_sigaction != libc::SIG_IGN
    {
        return Ok(None);
    }
    let id = crate::sdk_permission::request_nonce("alab-trust-")?;
    let mut records = match records.map(Records::create).transpose() {
        Ok(records) => records,
        Err(_) => return Ok(None),
    };
    let input = request(directory, &id);
    let args = ARGUMENTS.iter().map(OsString::from).collect::<Vec<_>>();
    let mut changes = env
        .iter()
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect::<Vec<_>>();
    changes.push(("ENABLE_CLAUDEAI_MCP_SERVERS".into(), "false".into()));
    if cancelled() {
        return Ok(None);
    }
    let until = Instant::now() + super::ADMISSION;
    let mut wire = Wire::default();
    let mut child = match ProbeChild::spawn_duplex_cohort(&selected.path, &args, &changes) {
        Ok(child) => child,
        Err(mut failure) => {
            if let Some(mask) = failure.signals.as_mut() {
                if let Err(error) = mask.restore() {
                    failure.error = error;
                    return Err(failure);
                }
                failure.signals = None;
            }
            if let Some(child) = failure.child.take() {
                cleanup(*child, &mut records, &mut wire, &id, cancelled)?;
            }
            return Ok(None);
        }
    };
    let completed = (|| {
        let mut source = None;
        let mut written = 0;
        let mut ended = false;
        while Instant::now() < until {
            if cancelled() {
                return Err(io::Error::from(io::ErrorKind::Interrupted));
            }
            if source.is_none() {
                match native(&mut child, selected, None) {
                    Ok(seen) => {
                        if let Some(records) = &mut records {
                            writeln!(records.native, "{seen:?}")?;
                        }
                        source = Some(seen);
                    }
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                        wait(&child, until, false)?;
                        continue;
                    }
                    Err(error) => return Err(error),
                }
            }
            if wire.answer.is_none() {
                native(&mut child, selected, source.as_ref())?;
                if written < input.len() {
                    if cancelled() {
                        return Err(io::Error::from(io::ErrorKind::Interrupted));
                    }
                    match child.write_input(&input[written..]) {
                        Ok(0) => return Err(super::invalid("SDK request write stalled")),
                        Ok(n) => {
                            if let Some(records) = &mut records {
                                records.input.write_all(&input[written..written + n])?;
                            }
                            written += n;
                        }
                        Err(error)
                            if matches!(
                                error.kind(),
                                io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                            ) => {}
                        Err(error) => return Err(error),
                    }
                }
            }
            wire.read(&mut child, &mut records)?;
            if wire.parse(&id, written == input.len() && Instant::now() < until)? {
                native(&mut child, selected, source.as_ref())?;
                if cancelled() || Instant::now() >= until {
                    return Err(io::Error::from(io::ErrorKind::TimedOut));
                }
                child.close_input();
            }
            ended |= child.try_wait_cohort()?.is_some();
            if ended {
                if !child.output_finished() {
                    wait(&child, until, false)?;
                    continue;
                }
                wire.finish()?;
                return Ok(());
            }
            wait(&child, until, written < input.len())?;
        }
        // An ACK admitted before the deadline may stand after forced native
        // retirement, exactly as Main's warm exchange; settlement is still mandatory.
        Ok(())
    })();
    if completed.is_err() || cancelled() {
        wire.poison();
    }
    // Actual root/group custody and output EOF are separate: neither substitutes
    // for the other. Unknown retirement returns the real child, never a false result.
    let readable = if child.pending() || !child.output_finished() {
        cleanup(child, &mut records, &mut wire, &id, cancelled)?
    } else {
        true
    };
    Ok(wire.answer.flatten().filter(|_| {
        readable && !wire.failed && records.as_ref().is_none_or(|records| records.valid)
    }))
}

#[cfg(test)]
#[path = "exchange_tests.rs"]
mod tests;
