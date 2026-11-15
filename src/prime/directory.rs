// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Directory-relative settings effects. Moving the directory's name never redirects an effect.

use std::ffi::{CString, OsStr};
use std::fs::File;
use std::io;
use std::os::fd::{AsRawFd as _, FromRawFd as _};
use std::os::unix::ffi::OsStrExt as _;
use std::os::unix::fs::{MetadataExt as _, OpenOptionsExt as _, PermissionsExt as _};
use std::path::{Component, Path, PathBuf};

pub(super) type Identity = (u64, u64);

pub(super) fn identity(meta: &std::fs::Metadata) -> Identity {
    (meta.dev(), meta.ino())
}

fn entry(name: &OsStr) -> io::Result<CString> {
    let mut parts = Path::new(name).components();
    if !matches!(parts.next(), Some(Component::Normal(_))) || parts.next().is_some() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "expected one entry name",
        ));
    }
    CString::new(name.as_bytes()).map_err(io::Error::other)
}

pub(super) struct Directory {
    file: File,
    id: Identity,
    pub(super) path: PathBuf,
}

impl Directory {
    pub(super) fn open(path: &Path) -> io::Result<Self> {
        #[cfg(target_os = "macos")]
        let access = libc::O_SEARCH;
        #[cfg(target_os = "linux")]
        let access = libc::O_PATH;
        #[cfg(not(any(target_os = "macos", target_os = "linux")))]
        let access = libc::O_RDONLY;
        let file = File::options()
            .read(true)
            .custom_flags(
                access | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC,
            )
            .open(path)?;
        let meta = file.metadata()?;
        if !meta.is_dir() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "settings parent is not a directory",
            ));
        }
        Ok(Self {
            file,
            id: identity(&meta),
            path: path.to_owned(),
        })
    }

    pub(super) fn is_at_path(&self) -> io::Result<bool> {
        match std::fs::metadata(&self.path) {
            Ok(meta) => Ok(meta.is_dir() && identity(&meta) == self.id),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
            Err(error) => Err(error),
        }
    }

    fn open_entry(&self, name: &OsStr, flags: libc::c_int) -> io::Result<File> {
        let name = entry(name)?;
        // SAFETY: the directory and NUL-terminated single component stay owned through openat.
        let fd = unsafe {
            libc::openat(
                self.file.as_raw_fd(),
                name.as_ptr(),
                flags | libc::O_NONBLOCK | libc::O_CLOEXEC,
                0o600,
            )
        };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: openat returned one new descriptor, transferred into File exactly once.
        let file = unsafe { File::from_raw_fd(fd) };
        if !file.metadata()?.is_file() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "settings entry is not a regular file",
            ));
        }
        Ok(file)
    }

    pub(super) fn read(&self, name: &OsStr) -> io::Result<Option<File>> {
        match self.open_entry(name, libc::O_RDONLY | libc::O_NOFOLLOW) {
            Ok(file) => Ok(Some(file)),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error),
        }
    }

    /// Legacy installer copies may be regular-file symlinks. Follow only the
    /// final entry relative to this retained directory, then require a regular file.
    pub(super) fn read_auxiliary(&self, name: &OsStr) -> io::Result<Option<File>> {
        match self.open_entry(name, libc::O_RDONLY) {
            Ok(file) => Ok(Some(file)),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error),
        }
    }

    pub(super) fn create(&self, name: &OsStr) -> io::Result<File> {
        self.open_entry(
            name,
            libc::O_RDWR | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW,
        )
    }

    pub(super) fn lock_file(&self, name: &OsStr) -> io::Result<File> {
        match self.create(name) {
            Ok(file) => {
                file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
                Ok(file)
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                self.open_entry(name, libc::O_RDWR | libc::O_NOFOLLOW)
            }
            Err(error) => Err(error),
        }
    }

    pub(super) fn rename(&self, from: &OsStr, to: &OsStr) -> io::Result<()> {
        let from = entry(from)?;
        let to = entry(to)?;
        // SAFETY: both one-component names and the retained original directory are live.
        let result = unsafe {
            libc::renameat(
                self.file.as_raw_fd(),
                from.as_ptr(),
                self.file.as_raw_fd(),
                to.as_ptr(),
            )
        };
        if result < 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(())
        }
    }

    pub(super) fn remove_owned(&self, name: &OsStr, owned: &File) -> io::Result<bool> {
        let Some(current) = self.read(name)? else {
            return Ok(false);
        };
        // The original open file pins its inode; a reused path or inode cannot become this receipt.
        if identity(&current.metadata()?) != identity(&owned.metadata()?) {
            return Ok(false);
        }
        let name = entry(name)?;
        // SAFETY: the directory and one-component NUL-terminated name remain live for unlinkat.
        let result = unsafe { libc::unlinkat(self.file.as_raw_fd(), name.as_ptr(), 0) };
        if result < 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(true)
        }
    }
}
