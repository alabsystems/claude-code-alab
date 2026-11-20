// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Land's repository read beside the supervisor's loop: the probe
//! ([`crate::repo::probe`]) runs in a process of its own, this harness's own
//! executable started with [`ENTRY`], and the loop looks at it without waiting
//! ([`Probing::poll`]), so git's reads of a large repository, each given its
//! minute ([`crate::repo::READ_LIMIT`]), never hold the loop that keeps the
//! claim, follows the child and answers its boxes.
//!
//! * **What it is given.** The person's home folder, the conversation's
//!   transcript where the supervisor found it, and the folders it knows the
//!   session works in: the one the vendor child runs in and the one the
//!   harness started in. The process adds the folders the transcript shows
//!   the session changing: a file it edited or wrote, or git work a shell
//!   command made there ([`crate::repo::session_folders`], over its last
//!   [`TRANSCRIPT_SCAN_BYTES`]).
//! * **What it answers.** One line ([`crate::repo::Probed::line`]) on its
//!   standard output, then it exits 0. Anything else (another exit, a line
//!   not in that grammar, a process that runs past [`PROBE_CAP`]) is
//!   [`Probed::Cut`]: nothing known yet.
//! * **What it leaves.** It runs in a process group of its own with every
//!   `git` it starts; a probe dropped before its end is killed with its group
//!   and reaped ([`crate::procs::Running`]).

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use crate::procs::{Ran, Running};
use crate::repo::{self, Probed};

/// The hidden first word that runs the probe in place of the harness:
/// `claude-alab --alab-repo-probe --home <folder> [--transcript <file>] --
/// <folder>…`.
pub const ENTRY: &str = "--alab-repo-probe";

/// How much of the transcript's end the probe reads for the folders the
/// session worked in.
pub const TRANSCRIPT_SCAN_BYTES: u64 = 8 * 1024 * 1024;

/// How long a probe may run before it is cut whatever it is doing: past every
/// read's own minute, a bound on the whole.
pub const PROBE_CAP: Duration = Duration::from_secs(15 * 60);

/// How many reads of its pipe one look makes at most.
const POLL_READS: usize = 8;

/// A probe to run in this process ([`entry`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Request {
    /// The person's home folder.
    pub home: PathBuf,
    /// The conversation's transcript.
    pub transcript: Option<PathBuf>,
    /// The folders the session is known to work in.
    pub folders: Vec<PathBuf>,
}

impl Request {
    /// Run the probe over the folders given and those the transcript names,
    /// the given first, each once.
    #[must_use]
    pub fn run(&self) -> Probed {
        let mut folders = self.folders.clone();
        if let Some(tail) = self.transcript.as_deref().and_then(transcript_tail) {
            for folder in repo::session_folders(&tail) {
                if !folders.contains(&folder) {
                    folders.push(folder);
                }
            }
        }
        repo::probe(&folders, Some(self.home.as_path()), repo::READ_LIMIT)
    }
}

/// The last [`TRANSCRIPT_SCAN_BYTES`] of the transcript at `path`, read
/// lossily; its first line is dropped where the read starts inside one.
fn transcript_tail(path: &Path) -> Option<String> {
    use std::io::{Read as _, Seek as _, SeekFrom};
    let (mut file, meta) = crate::profile::open_transcript(path)?;
    let from = meta.len().saturating_sub(TRANSCRIPT_SCAN_BYTES);
    file.seek(SeekFrom::Start(from)).ok()?;
    let mut bytes = Vec::new();
    file.take(TRANSCRIPT_SCAN_BYTES)
        .read_to_end(&mut bytes)
        .ok()?;
    let text = String::from_utf8_lossy(&bytes);
    Some(if from > 0 {
        text.split_once('\n')
            .map_or_else(String::new, |(_, rest)| rest.to_string())
    } else {
        text.into_owned()
    })
}

/// The probe this process was started to run, where its first word is
/// [`ENTRY`]: `None` otherwise.
///
/// # Errors
/// The words after [`ENTRY`] are not `--home <absolute folder>`, an optional
/// `--transcript <absolute file>`, `--`, and absolute folders.
pub fn entry(words: &[OsString]) -> Option<Result<Request, String>> {
    if words.first().is_none_or(|word| word != ENTRY) {
        return None;
    }
    let usage = || format!("usage: {ENTRY} --home <folder> [--transcript <file>] -- <folder>...");
    let absolute = |word: &OsString| Some(PathBuf::from(word)).filter(|path| path.is_absolute());
    let mut rest = words.get(1..).unwrap_or_default().iter();
    let home = match (rest.next(), rest.next()) {
        (Some(flag), Some(home)) if flag == "--home" => absolute(home),
        _ => None,
    };
    let Some(home) = home else {
        return Some(Err(usage()));
    };
    let mut transcript = None;
    match rest.next() {
        Some(flag) if flag == "--transcript" => {
            transcript = rest.next().and_then(absolute);
            if transcript.is_none() || rest.next().is_none_or(|word| word != "--") {
                return Some(Err(usage()));
            }
        }
        Some(separator) if separator == "--" => {}
        _ => return Some(Err(usage())),
    }
    let folders: Option<Vec<PathBuf>> = rest.map(absolute).collect();
    Some(folders.ok_or_else(usage).map(|folders| Request {
        home,
        transcript,
        folders,
    }))
}

/// A probe running in a process of its own, looked at without waiting.
#[derive(Debug)]
pub struct Probing {
    /// The probe's process.
    running: Running,
    /// When it was started.
    started: Instant,
}

impl Probing {
    /// Start the probe of `request` as the harness executable `exe`, with
    /// `PATH`, `XDG_CONFIG_HOME` and `DEVELOPER_DIR` as this process has them
    /// (the probe's `git` is found and run by them) and `HOME` as the
    /// request's.
    ///
    /// # Errors
    /// The process could not be started: journal words.
    pub fn start(exe: &Path, request: &Request) -> Result<Self, String> {
        let mut command = Command::new(exe);
        command
            .env_clear()
            .env("HOME", &request.home)
            .arg(ENTRY)
            .arg("--home")
            .arg(&request.home);
        for key in ["PATH", "XDG_CONFIG_HOME", "DEVELOPER_DIR"] {
            if let Some(value) = std::env::var_os(key) {
                command.env(key, value);
            }
        }
        if let Some(transcript) = &request.transcript {
            command.arg("--transcript").arg(transcript);
        }
        command.arg("--").args(&request.folders);
        Self::of_command(&mut command)
    }

    /// Start `command` as the probe's process.
    ///
    /// # Errors
    /// It could not be started: journal words.
    pub fn of_command(command: &mut Command) -> Result<Self, String> {
        match Running::start(command) {
            Ok(running) => Ok(Self {
                running,
                started: Instant::now(),
            }),
            Err(Ran::NotStarted) => Err("not-started".to_string()),
            Err(_) => Err("output-untaken".to_string()),
        }
    }

    /// One look, never waiting: `None` while the probe runs; its answer once
    /// it has exited 0 with one line in [`Probed::line`]'s grammar; and
    /// [`Probed::Cut`] for any other end, or once it has run for
    /// [`PROBE_CAP`] (it is then killed with its group and reaped).
    pub fn poll(&mut self) -> Option<Probed> {
        for _ in 0..POLL_READS {
            match self.running.poll(Duration::ZERO) {
                Some(Ran::Ended { code: Some(0), out }) => {
                    return Some(
                        std::str::from_utf8(&out)
                            .ok()
                            .and_then(Probed::of_line)
                            .unwrap_or(Probed::Cut),
                    );
                }
                Some(_) => return Some(Probed::Cut),
                None => {}
            }
        }
        if self.started.elapsed() >= PROBE_CAP {
            let _ = self.running.cut();
            return Some(Probed::Cut);
        }
        None
    }
}

#[cfg(test)]
#[path = "repo_probe_tests.rs"]
mod tests;
