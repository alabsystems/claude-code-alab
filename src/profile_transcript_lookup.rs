// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Transcript discovery for the existing profile reader and bounded model reads.

use std::fs::{self, DirEntry};
use std::io;
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Refused {
    Incomplete,
    Unreadable,
    Ambiguous,
}

pub(super) fn legacy(config: &Path, session: &str) -> Option<PathBuf> {
    locate(config, session, None).ok().flatten()
}

/// Consume at most `remaining` directory-next attempts, including the attempt that proves
/// the enumeration ended. No partial enumeration establishes a unique transcript.
pub(crate) fn bounded(
    config: &Path,
    session: &str,
    remaining: &mut usize,
) -> Result<Option<PathBuf>, Refused> {
    locate(config, session, Some(remaining))
}

fn locate(
    config: &Path,
    session: &str,
    budget: Option<&mut usize>,
) -> Result<Option<PathBuf>, Refused> {
    if !super::is_session_id(session) {
        return Ok(None);
    }
    if budget.as_deref() == Some(&0) {
        return Err(Refused::Incomplete);
    }
    let projects = config.join("projects");
    let entries = match fs::read_dir(&projects) {
        Ok(entries) => entries,
        Err(error)
            if budget.is_some()
                && error.kind() == io::ErrorKind::NotFound
                && fs::symlink_metadata(&projects)
                    .is_err_and(|why| why.kind() == io::ErrorKind::NotFound) =>
        {
            return Ok(None);
        }
        Err(_) => return Err(Refused::Unreadable),
    };
    find(entries, session, budget)
}

fn find(
    mut entries: impl Iterator<Item = io::Result<DirEntry>>,
    session: &str,
    mut budget: Option<&mut usize>,
) -> Result<Option<PathBuf>, Refused> {
    let strict = budget.is_some();
    let mut found = None;
    loop {
        if let Some(remaining) = budget.as_deref_mut() {
            *remaining = remaining.checked_sub(1).ok_or(Refused::Incomplete)?;
        }
        let Some(entry) = entries.next() else {
            return Ok(found);
        };
        let entry = match entry {
            Ok(entry) => entry,
            Err(_) if strict => return Err(Refused::Unreadable),
            Err(_) => continue,
        };
        let path = entry.path().join(format!("{session}.jsonl"));
        let regular = if strict {
            strict_candidate(&entry, &path)?
        } else {
            // Preserve the existing permission/restart lookup's followed-link and skip rules.
            fs::metadata(&path).is_ok_and(|metadata| metadata.is_file())
        };
        if regular {
            if found.is_some() {
                return if strict {
                    Err(Refused::Ambiguous)
                } else {
                    Ok(None)
                };
            }
            found = Some(path);
        }
    }
}

fn strict_candidate(entry: &DirEntry, path: &Path) -> Result<bool, Refused> {
    let kind = entry.file_type().map_err(|_| Refused::Unreadable)?;
    if kind.is_symlink() {
        if !fs::metadata(entry.path())
            .map_err(|_| Refused::Unreadable)?
            .is_dir()
        {
            return Ok(false);
        }
    } else if !kind.is_dir() {
        return Ok(false);
    }
    let named = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(_) => return Err(Refused::Unreadable),
    };
    let metadata = if named.file_type().is_symlink() {
        fs::metadata(path).map_err(|_| Refused::Unreadable)?
    } else {
        named
    };
    if !metadata.is_file() {
        return Err(Refused::Unreadable);
    }
    Ok(true)
}

#[cfg(test)]
#[path = "profile_transcript_lookup_tests.rs"]
mod tests;
