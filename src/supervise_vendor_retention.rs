// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Hold atpkg's actual build GC rendezvous through termination and spawn.
//! This is not first-exec body proof after the spawn syscall returns.

use crate::store::Build;
use std::fs::{File, OpenOptions};
use std::io;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt};
use std::path::PathBuf;
use std::sync::Arc;

#[derive(Clone, Debug)]
pub(super) struct Hold(Arc<Held>);
#[derive(Debug)]
struct Held {
    file: File,
    path: PathBuf,
    prefix: PathBuf,
    entry: PathBuf,
    executable: PathBuf,
    identity: (u64, u64),
    revision: [i128; 5],
}
impl PartialEq for Hold {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}
impl Eq for Hold {}
fn invalid(why: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, why)
}
fn revision(metadata: &std::fs::Metadata) -> [i128; 5] {
    [
        i128::from(metadata.len()),
        i128::from(metadata.mtime()),
        i128::from(metadata.mtime_nsec()),
        i128::from(metadata.ctime()),
        i128::from(metadata.ctime_nsec()),
    ]
}
impl Hold {
    pub fn take(build: &Build) -> io::Result<Self> {
        let program = build
            .dir
            .parent()
            .ok_or_else(|| invalid("vendor build has no program"))?
            .canonicalize()?;
        let store = program
            .parent()
            .ok_or_else(|| invalid("vendor program has no store"))?;
        if program.file_name() != Some(std::ffi::OsStr::new("claude"))
            || store.file_name() != Some(std::ffi::OsStr::new("store"))
        {
            return Err(invalid("vendor build leaves its package"));
        }
        let prefix = store
            .parent()
            .ok_or_else(|| invalid("vendor store has no prefix"))?
            .to_path_buf();
        let leases = prefix.join("leases");
        match std::fs::symlink_metadata(&leases) {
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                if let Err(error) = std::fs::DirBuilder::new().mode(0o700).create(&leases)
                    && error.kind() != io::ErrorKind::AlreadyExists
                {
                    return Err(error);
                }
            }
            Err(error) => return Err(error),
        }
        if !std::fs::symlink_metadata(&leases)?.is_dir() || leases.canonicalize()? != leases {
            return Err(invalid("vendor lease directory is not a real prefix child"));
        }
        let path = leases.join(format!("store-claude-{}.gate", build.number));
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
            .open(&path)?;
        let held = file.metadata()?;
        if !held.is_file()
            || held.nlink() != 1
            || held.mode() & 0o077 != 0
            || held.uid() != std::fs::symlink_metadata(&prefix)?.uid()
        {
            return Err(invalid(
                "vendor GC gate is not a private regular rendezvous",
            ));
        }
        file.try_lock_shared().map_err(|error| match error {
            std::fs::TryLockError::WouldBlock => {
                io::Error::new(io::ErrorKind::WouldBlock, "vendor build GC is in progress")
            }
            std::fs::TryLockError::Error(error) => error,
        })?;
        let executable = build.exe.canonicalize()?;
        let in_build = executable.starts_with(build.dir.canonicalize()?);
        let in_compat = prefix
            .join("compat/claude")
            .join(build.number.to_string())
            .canonicalize()
            .is_ok_and(|root| executable.starts_with(root));
        if !in_build && !in_compat {
            return Err(invalid(
                "vendor image leaves its managed build and execution root",
            ));
        }
        let metadata = std::fs::symlink_metadata(&executable)?;
        if !metadata.is_file()
            || metadata.mode() & 0o111 == 0
            || metadata.mode() & 0o6000 != 0
            || metadata.ino() == 0
        {
            return Err(invalid("vendor image is not executable"));
        }
        let hold = Self(Arc::new(Held {
            file,
            path,
            prefix,
            entry: build.exe.clone(),
            executable,
            identity: (metadata.dev(), metadata.ino()),
            revision: revision(&metadata),
        }));
        hold.revalidate()?;
        Ok(hold)
    }
    pub fn revalidate(&self) -> io::Result<()> {
        let held = &self.0;
        let leases = held.prefix.join("leases");
        if !std::fs::symlink_metadata(&leases)?.is_dir() || leases.canonicalize()? != leases {
            return Err(invalid("vendor GC gate directory changed"));
        }
        let descriptor = held.file.metadata()?;
        let path = std::fs::symlink_metadata(&held.path)?;
        let executable = std::fs::symlink_metadata(&held.executable)?;
        if !descriptor.is_file()
            || descriptor.nlink() != 1
            || descriptor.mode() & 0o077 != 0
            || descriptor.uid() != std::fs::symlink_metadata(&held.prefix)?.uid()
            || !path.is_file()
            || (descriptor.dev(), descriptor.ino()) != (path.dev(), path.ino())
            || held.entry.canonicalize()? != held.executable
            || !executable.is_file()
            || executable.mode() & 0o111 == 0
            || executable.mode() & 0o6000 != 0
            || (executable.dev(), executable.ino()) != held.identity
            || revision(&executable) != held.revision
        {
            return Err(invalid("vendor GC gate or pinned image changed"));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQUENCE: AtomicU64 = AtomicU64::new(0);
    fn fixture_build() -> Build {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let prefix = std::env::temp_dir().join(".ubd").join(format!(
            "claude-vendor-hold-{}-{stamp}-{}.noindex",
            std::process::id(),
            SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        let dir = prefix.join("store/claude/1");
        let exe = dir.join("bin/claude");
        std::fs::create_dir_all(exe.parent().unwrap()).unwrap();
        // Executable metadata only: these fixture bytes are never executed.
        std::fs::write(&exe, b"fixture data, never a launched program\n").unwrap();
        std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o700)).unwrap();
        Build {
            number: 1,
            dir,
            exe,
            version: Some("9.9.1".into()),
            env: Vec::new(),
        }
    }
    #[test]
    fn captured_vendor_and_clones_block_the_actual_gc_subject_until_last_drop() {
        let build = fixture_build();
        let hold = Hold::take(&build).unwrap();
        let copy = hold.clone();
        let gate = OpenOptions::new()
            .read(true)
            .write(true)
            .open(hold.0.prefix.join("leases/store-claude-1.gate"))
            .unwrap();
        assert!(matches!(
            gate.try_lock(),
            Err(std::fs::TryLockError::WouldBlock)
        ));
        drop(hold);
        assert!(matches!(
            gate.try_lock(),
            Err(std::fs::TryLockError::WouldBlock)
        ));
        drop(copy);
        crate::test_support::lock_reclaim_gate(&gate);
        gate.unlock().unwrap();
    }
    #[test]
    fn gate_or_pinned_executable_substitution_cannot_pass_revalidation() {
        let build = fixture_build();
        let hold = Hold::take(&build).unwrap();
        std::fs::rename(&build.exe, build.exe.with_extension("retained-fixture")).unwrap();
        std::fs::write(&build.exe, b"different unexecuted fixture\n").unwrap();
        assert!(hold.revalidate().is_err());
        let build = fixture_build();
        let hold = Hold::take(&build).unwrap();
        std::fs::rename(&hold.0.path, hold.0.path.with_extension("retained-gate")).unwrap();
        let _replacement = OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&hold.0.path)
            .unwrap();
        assert!(hold.revalidate().is_err());
        let build = fixture_build();
        std::fs::set_permissions(&build.exe, std::fs::Permissions::from_mode(0o4700)).unwrap();
        assert!(Hold::take(&build).is_err());
    }
}
