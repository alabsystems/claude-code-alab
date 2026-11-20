// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! What is kept of a session off Fable beyond its harness process: one small
//! file per conversation beside the journal,
//! `<journal's folder>/off-fable/<session id>`, written when a move leaves
//! Fable and removed when the session is back on its launch's terms. A new
//! harness process for the same conversation (a tab restored after a restart
//! of the machine, or `claude --resume <id>` run again) takes it up at its
//! first look, where its own launch names no model and so resumes on the
//! conversation's newest model, which is the fallback
//! (`Supervisor::limit_adopt` in [`crate::supervise`]).
//!
//! The file holds one line, `back_at_ms=<n> walled_returns=<n>`. It is
//! written whole under another name and renamed into place, read through one
//! bounded handle that follows no link, and anything else in it reads as
//! nothing kept.

use std::io::Write as _;
use std::path::{Path, PathBuf};

/// The folder beside the journal the files are kept in.
const DIR: &str = "off-fable";

/// The longest file read: one line of two numbers is far shorter.
const MAX_BYTES: u64 = 256;

/// What is kept of a session off Fable.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Kept {
    /// When (epoch ms) the session goes back ([`super::Phase::OffFable`]).
    pub back_at_ms: u64,
    /// How many returns in a row were refused ([`super::State::walled_returns`]).
    pub walled_returns: u32,
}

impl Kept {
    /// The file's one line.
    #[must_use]
    pub fn line(&self) -> String {
        format!(
            "back_at_ms={} walled_returns={}\n",
            self.back_at_ms, self.walled_returns
        )
    }

    /// The file's text read back: exactly the line [`Kept::line`] writes,
    /// each number in its plain decimal spelling. Anything else is `None`.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        let words: Vec<&str> = text.strip_suffix('\n')?.split(' ').collect();
        let [back, walled] = words.as_slice() else {
            return None;
        };
        let number = |word: &str, key: &str| {
            let digits = word.strip_prefix(key)?.strip_prefix('=')?;
            let canonical = !digits.is_empty()
                && digits.bytes().all(|b| b.is_ascii_digit())
                && (digits == "0" || !digits.starts_with('0'));
            canonical.then(|| digits.parse::<u64>().ok()).flatten()
        };
        Some(Self {
            back_at_ms: number(back, "back_at_ms")?,
            walled_returns: u32::try_from(number(walled, "walled_returns")?).ok()?,
        })
    }
}

/// Where the state of the conversation `session` is kept, beside the journal
/// at `journal`; `None` for a session id that could name another file.
#[must_use]
pub fn kept_path(journal: &Path, session: &str) -> Option<PathBuf> {
    if !crate::profile::is_session_id(session) {
        return None;
    }
    Some(journal.parent()?.join(DIR).join(session))
}

/// Keep `kept` at `path`: its folder made private (0700) where it is missing,
/// the line written whole to a new private (0600) file beside it and renamed
/// over `path`.
///
/// # Errors
/// The folder, the file or the rename could not be made.
pub fn keep(path: &Path, kept: &Kept) -> std::io::Result<()> {
    use std::os::unix::fs::{DirBuilderExt as _, OpenOptionsExt as _};
    let dir = path
        .parent()
        .ok_or_else(|| std::io::Error::other("no folder"))?;
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)?;
    let name = path
        .file_name()
        .ok_or_else(|| std::io::Error::other("no name"))?;
    let mut fresh = name.to_os_string();
    fresh.push(format!(".{}.new", std::process::id()));
    let fresh = dir.join(fresh);
    // One this process left behind, cut short.
    let _ = std::fs::remove_file(&fresh);
    let written = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(&fresh)
        .and_then(|mut file| file.write_all(kept.line().as_bytes()))
        .and_then(|()| std::fs::rename(&fresh, path));
    if written.is_err() {
        let _ = std::fs::remove_file(&fresh);
    }
    written
}

/// What is kept at `path`, when a well-formed line is.
#[must_use]
pub fn kept(path: &Path) -> Option<Kept> {
    crate::store::read_bounded(path, MAX_BYTES, false).and_then(|text| Kept::parse(&text))
}

/// Keep nothing at `path` any more. Nothing there already is no error.
///
/// # Errors
/// The file is there and could not be removed.
pub fn forget(path: &Path) -> std::io::Result<()> {
    match std::fs::remove_file(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        other => other,
    }
}

#[cfg(test)]
#[path = "limit_kept_tests.rs"]
mod tests;
