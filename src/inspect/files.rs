// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Bounded regular-file observations. No directory creation or writer coordination.

use std::ffi::CString;
use std::fs::File;
use std::io::{self, Read as _};
use std::os::unix::ffi::OsStrExt as _;
use std::os::unix::fs::{MetadataExt as _, OpenOptionsExt as _};
use std::path::{Path, PathBuf};

use super::{PathObservation, PathStatus};

pub(super) struct Observed {
    pub path: PathObservation,
    pub text: Result<Option<String>, String>,
}

fn stamp(meta: &std::fs::Metadata) -> (u64, u64, u64, i64, i64, i64, i64) {
    (
        meta.dev(),
        meta.ino(),
        meta.len(),
        meta.mtime(),
        meta.mtime_nsec(),
        meta.ctime(),
        meta.ctime_nsec(),
    )
}

fn read(path: &Path, physical: &Path, cap: u64) -> io::Result<String> {
    let file = File::options()
        .read(true)
        .custom_flags(libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(path)?;
    let before = file.metadata()?;
    if !before.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "not a regular file",
        ));
    }
    if before.len() > cap {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "read budget exceeded",
        ));
    }
    let mut text = String::new();
    (&file).take(cap + 1).read_to_string(&mut text)?;
    if text.len() as u64 > cap {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "read budget exceeded",
        ));
    }
    if stamp(&before) != stamp(&file.metadata()?)
        || stamp(&before) != stamp(&std::fs::metadata(path)?)
        || std::fs::canonicalize(path)? != physical
    {
        return Err(io::Error::other("file changed during inspection"));
    }
    Ok(text)
}

pub(super) fn observe(path: PathBuf, cap: u64) -> Observed {
    let physical = std::fs::canonicalize(&path);
    let (status, physical, text) = match physical {
        Ok(physical) => {
            let text = read(&path, &physical, cap)
                .map(Some)
                .map_err(|e| e.to_string());
            let status = if text.is_ok() {
                PathStatus::Present
            } else {
                PathStatus::Unavailable
            };
            (status, Some(physical), text)
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            // A dangling component is different from an absent file or directory.
            let broken = dangling(&path);
            if broken {
                (
                    PathStatus::BrokenLink,
                    None,
                    Err("selected path contains a broken symbolic link".into()),
                )
            } else {
                (PathStatus::Missing, None, Ok(None))
            }
        }
        Err(error) => (PathStatus::Unavailable, None, Err(error.to_string())),
    };
    Observed {
        path: PathObservation {
            selected: path,
            physical,
            status,
        },
        text,
    }
}

fn dangling(path: &Path) -> bool {
    path.ancestors().any(|path| {
        std::fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_symlink())
            && std::fs::metadata(path).is_err()
    })
}

pub(super) fn parent_writable(path: &Path) -> bool {
    let Some(parent) = path.parent() else {
        return false;
    };
    let Ok(path) = CString::new(parent.as_os_str().as_bytes()) else {
        return false;
    };
    // SAFETY: the owned NUL-terminated path outlives this read-only access check.
    unsafe { libc::access(path.as_ptr(), libc::W_OK) == 0 }
}
