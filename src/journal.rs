// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The harness's journal: one line per decision, never on the terminal.
//!
//! The vendor child owns the screen, so the harness writes what it decided
//! (start, an update seen, why it waits, a relaunch, the exit) to
//! `$XDG_STATE_HOME/claude-code-alab/journal.log`, else
//! `~/.local/state/claude-code-alab/journal.log`. A write that fails is
//! dropped: the journal never decides anything.

use std::io::Write as _;
use std::path::{Path, PathBuf};

/// The size past which the journal is rotated to `journal.log.1` (one generation).
pub const MAX_BYTES: u64 = 1024 * 1024;

/// An append-only journal file.
#[derive(Clone, Debug)]
pub struct Journal {
    path: Option<PathBuf>,
    pid: u32,
}

impl Journal {
    /// The journal under `state_home` (or `<home>/.local/state`), for harness `pid`.
    #[must_use]
    pub fn new(state_home: Option<&Path>, home: Option<&Path>, pid: u32) -> Self {
        let base = state_home
            .filter(|p| p.is_absolute())
            .map(Path::to_path_buf)
            .or_else(|| home.map(|h| h.join(".local").join("state")));
        Self {
            path: base.map(|b| b.join(crate::profile::REPO).join("journal.log")),
            pid,
        }
    }

    /// A journal that writes nothing.
    #[must_use]
    pub fn none() -> Self {
        Self { path: None, pid: 0 }
    }

    /// Where the journal is written, when anywhere.
    #[must_use]
    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    /// The resolved journal destination and original writer identity for a transfer capsule.
    #[must_use]
    pub fn captured(&self) -> (Option<PathBuf>, u32) {
        (self.path.clone(), self.pid)
    }

    /// Reconstruct the captured destination without consulting new environment settings.
    ///
    /// # Errors
    /// A destination is relative, or its writer is not this continuing process.
    pub fn from_captured(path: Option<PathBuf>, pid: u32) -> std::io::Result<Self> {
        if path.as_ref().is_some_and(|path| !path.is_absolute())
            || (pid != std::process::id() && !(path.is_none() && pid == 0))
        {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "journal transfer does not name this process's captured destination",
            ));
        }
        Ok(Self { path, pid })
    }

    /// Append `<epoch ms> pid=<pid> <event>` (control bytes in `event` escaped).
    pub fn note(&self, event: &str) {
        let Some(path) = &self.path else { return };
        let now = crate::supervise::now_ms();
        let mut line = format!("{now} pid={} ", self.pid);
        for c in event.chars() {
            if c.is_control() {
                line.push_str(&c.escape_default().to_string());
            } else {
                line.push(c);
            }
        }
        line.push('\n');
        let _ = Self::append(path, line.as_bytes());
    }

    fn append(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
        use std::os::unix::fs::{DirBuilderExt as _, OpenOptionsExt as _};
        if let Some(dir) = path.parent() {
            std::fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(dir)?;
        }
        if std::fs::symlink_metadata(path).is_ok_and(|m| m.is_file() && m.len() > MAX_BYTES) {
            let _ = std::fs::rename(path, path.with_extension("log.1"));
        }
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .mode(0o600)
            .custom_flags(libc::O_NONBLOCK | libc::O_NOFOLLOW)
            .open(path)?;
        if !f.metadata()?.is_file() {
            return Err(std::io::Error::other("journal is not a regular file"));
        }
        f.write_all(bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn journal_path_prefers_an_absolute_state_home() {
        let j = Journal::new(Some(Path::new("/s")), Some(Path::new("/h")), 1);
        assert_eq!(j.path(), Some(Path::new("/s/claude-code-alab/journal.log")));
        let j = Journal::new(Some(Path::new("rel")), Some(Path::new("/h")), 1);
        assert_eq!(
            j.path(),
            Some(Path::new("/h/.local/state/claude-code-alab/journal.log"))
        );
        assert_eq!(Journal::new(None, None, 1).path(), None);
    }

    #[test]
    fn adoption_keeps_the_resolved_journal_and_writer() {
        let original = Journal::new(Some(Path::new("/original/state")), None, std::process::id());
        let (path, pid) = original.captured();
        assert_eq!(
            Journal::from_captured(path.clone(), pid)
                .unwrap()
                .captured(),
            original.captured()
        );
        assert!(Journal::from_captured(path, pid.wrapping_add(1)).is_err());
        assert!(Journal::from_captured(Some(PathBuf::from("relative")), pid).is_err());
        assert_eq!(
            Journal::from_captured(None, 0).unwrap().captured(),
            Journal::none().captured()
        );
    }
}
