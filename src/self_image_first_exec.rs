// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Native atpkg first-start inventory for a capability probe that may not reach its code.
//!
//! A permanent, independent stamp survives a failed probe, later exec and process
//! exit. Older atpkg readers retain its nonnumeric schema as an unknown mark.
//! Only output from the exact target can release it; an executable descriptor
//! or a process-table entry is not execution proof.

use super::{Image, PACKAGE, invalid};
use std::fs::{File, OpenOptions};
use std::io::{self, Write as _};
use std::os::unix::fs::{DirBuilderExt as _, MetadataExt as _, OpenOptionsExt as _};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

const PERMANENT_STAMP: &[u8] = b"first-exec permanent v1\n";

fn same_file(file: &File, path: &Path) -> io::Result<bool> {
    let held = file.metadata()?;
    let current = std::fs::symlink_metadata(path)?;
    Ok(current.is_file() && (held.dev(), held.ino()) == (current.dev(), current.ino()))
}

fn gate(program: &Path) -> io::Result<File> {
    let path = program.with_file_name(format!(".{PACKAGE}.first-exec.gate"));
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(&path)?;
    if !file.metadata()?.is_file() {
        return Err(invalid("first-start program gate is not a regular file"));
    }
    file.try_lock().map_err(|error| match error {
        std::fs::TryLockError::WouldBlock => io::Error::new(
            io::ErrorKind::WouldBlock,
            "first-start registration is busy",
        ),
        std::fs::TryLockError::Error(error) => error,
    })?;
    if !same_file(&file, &path)? {
        return Err(invalid("first-start program gate changed while opening"));
    }
    Ok(file)
}

/// One probe's independent first-start hold, using atpkg's existing inventory.
/// Dropping it preserves the mark indefinitely, including across process exit.
#[derive(Debug)]
#[must_use = "release only after the probed image produced execution proof"]
pub struct FirstExecMark {
    program: PathBuf,
    directory: PathBuf,
    directory_id: (u64, u64),
    file: File,
}

impl FirstExecMark {
    /// Register before starting a probe, ordered against native store removal.
    ///
    /// # Errors
    /// Registration is busy, the image changed, or a private mark cannot be persisted.
    pub fn before_probe(image: &Image) -> io::Result<Self> {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let program = image.prefix.join("store").join(PACKAGE);
        let _gate = gate(&program)?;
        image.revalidate()?;
        let marks = program.join(format!("{}.unproven.d", image.build));
        match std::fs::DirBuilder::new().mode(0o700).create(&marks) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error),
        }
        if !std::fs::symlink_metadata(&marks)?.is_dir() || marks.canonicalize()? != marks {
            return Err(invalid(
                "first-start inventory is not a real program directory",
            ));
        }
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|_| invalid("first-start clock precedes its epoch"))?
            .as_nanos();
        let directory = marks.join(format!(
            "start-{}-{now}-{}.noindex",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::DirBuilder::new().mode(0o700).create(&directory)?;
        let mut file = match OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
            .open(directory.join("stamp"))
        {
            Ok(file) => file,
            Err(error) => {
                let _ = std::fs::remove_dir(&directory);
                return Err(error);
            }
        };
        // This named permanent schema is nonnumeric: existing native readers
        // already retain unknown nonempty stamps without an age limit. No new
        // process starts unless every write and persistence step below succeeds.
        file.write_all(PERMANENT_STAMP)?;
        file.sync_all()?;
        File::open(&directory)?.sync_all()?;
        File::open(&marks)?.sync_all()?;
        let metadata = std::fs::symlink_metadata(&directory)?;
        Ok(Self {
            program,
            directory,
            directory_id: (metadata.dev(), metadata.ino()),
            file,
        })
    }

    /// Release only this probe's mark after output from the exact target proved execution.
    ///
    /// # Errors
    /// Registration/removal is busy or a retained pathname changed. Uncertainty keeps the mark.
    pub fn ran(self) -> io::Result<()> {
        let _gate = gate(&self.program)?;
        let metadata = std::fs::symlink_metadata(&self.directory)?;
        let stamp = self.directory.join("stamp");
        if !metadata.is_dir()
            || (metadata.dev(), metadata.ino()) != self.directory_id
            || !same_file(&self.file, &stamp)?
        {
            return Err(invalid("first-start mark changed before release"));
        }
        std::fs::remove_file(stamp)?;
        std::fs::remove_dir(&self.directory)?;
        if let Some(marks) = self.directory.parent() {
            // Other starters keep their independent inventory entries.
            let _ = std::fs::remove_dir(marks);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Wait, at most 2 s, until the gate this test just released is free again. A sibling
    /// test's spawn can hold a copy of its descriptor between the kernel's descriptor copy and
    /// the exec that closes it, so the lock briefly outlives its release (as for atpkg's reclaim
    /// gate, [`crate::test_support::lock_reclaim_gate`]). The harness's own loop spawns nothing
    /// while it holds the gate, so only tests see this.
    fn gate_settled(program: &Path) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        loop {
            match gate(program) {
                Ok(_) => return,
                Err(error)
                    if error.kind() == io::ErrorKind::WouldBlock
                        && std::time::Instant::now() < deadline =>
                {
                    std::thread::sleep(std::time::Duration::from_millis(5));
                }
                Err(error) => panic!("the gate never settled: {error}"),
            }
        }
    }

    #[test]
    fn independent_probe_marks_keep_unproved_starts_and_release_only_the_proven_one() {
        let (_, image) = super::super::tests::fixture();
        let program = image.prefix.join("store").join(PACKAGE);
        let first = FirstExecMark::before_probe(&image).unwrap();
        let first_dir = first.directory.clone();
        gate_settled(&program);
        let second = FirstExecMark::before_probe(&image).unwrap();
        let second_dir = second.directory.clone();
        assert_eq!(
            std::fs::read(first_dir.join("stamp")).unwrap(),
            PERMANENT_STAMP
        );
        drop(first);
        gate_settled(&program);
        second.ran().unwrap();
        assert!(first_dir.join("stamp").is_file());
        assert!(!second_dir.exists());
    }

    #[test]
    fn busy_native_program_gate_refuses_a_probe_before_registration() {
        let (_, image) = super::super::tests::fixture();
        let program = image.prefix.join("store").join(PACKAGE);
        let _removal = gate(&program).unwrap();
        assert_eq!(
            FirstExecMark::before_probe(&image).unwrap_err().kind(),
            io::ErrorKind::WouldBlock
        );
        assert!(!program.join(format!("{}.unproven.d", image.build)).exists());
    }
}
