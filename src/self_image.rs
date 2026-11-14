// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Own-image selection and bounded retention through atpkg's existing GC gate.
//!
//! A version sidecar is display metadata, not executable authentication. The
//! transfer entry must independently compare the newly running image and its
//! compiled version before adopting any state.

use std::fs::{File, OpenOptions};
use std::io;
use std::os::fd::{AsFd, AsRawFd, BorrowedFd};
use std::os::unix::fs::{MetadataExt as _, OpenOptionsExt as _};
use std::path::{Component, Path, PathBuf};

#[path = "self_image_first_exec.rs"]
mod first_exec;
pub use first_exec::FirstExecMark;

/// The store program containing this harness, distinct from the vendor.
pub const PACKAGE: &str = "claude-code-alab";
/// The executable exposed by the harness package.
pub const COMMAND: &str = "claude-alab";

fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

/// One complete store build and its selected executable file identity.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Image {
    /// Canonical package prefix.
    pub prefix: PathBuf,
    /// Native atpkg build number.
    pub build: u64,
    /// Complete build directory, not a mutable `current` link.
    pub directory: PathBuf,
    /// The selected executable and its device/inode.
    pub executable: crate::procs::Executable,
    /// Version recorded beside this exact build; it must match the compiled probe.
    pub version: String,
    /// File length and native modification/change clocks, guarding ordinary in-place changes.
    pub file_revision: [u64; 5],
}

impl Image {
    /// Resolve the actually mapped old image, independently of a newer `current` link.
    ///
    /// # Errors
    /// The mapped image is not this package's complete numbered build, or its
    /// on-disk identity and version differ from the continuing process.
    pub fn running(
        home: &Path,
        process: &crate::procs::ProcessIdentity,
        compiled_version: &str,
    ) -> io::Result<Self> {
        let prefix = crate::store::default_prefix(home).canonicalize()?;
        let program = prefix.join("store").join(PACKAGE);
        let executable = &process.executable;
        let bin = executable
            .path
            .parent()
            .ok_or_else(|| invalid("running image has no bin directory"))?;
        let directory = bin
            .parent()
            .ok_or_else(|| invalid("running image has no build directory"))?;
        if bin.file_name() != Some(std::ffi::OsStr::new("bin"))
            || executable.path.file_name() != Some(std::ffi::OsStr::new(COMMAND))
            || directory.parent() != Some(program.as_path())
        {
            return Err(invalid("running image is not a numbered harness package"));
        }
        let number = directory
            .file_name()
            .and_then(|name| name.to_str())
            .and_then(crate::store::parse_build_name)
            .ok_or_else(|| invalid("running image has no canonical build number"))?;
        let image = Self::build_at(&prefix, number)?;
        if image.executable != *executable || image.version != compiled_version {
            return Err(invalid(
                "running image identity or compiled version differs from its build",
            ));
        }
        Ok(image)
    }

    /// Resolve the current own-package build without running it.
    ///
    /// # Errors
    /// The prefix, selection, readiness, version or executable is absent or invalid.
    pub fn selected(home: &Path) -> io::Result<Self> {
        let prefix = crate::store::default_prefix(home).canonicalize()?;
        let program = prefix.join("store").join(PACKAGE);
        let target = std::fs::read_link(program.join("current"))?;
        if target
            .components()
            .any(|part| matches!(part, Component::ParentDir))
        {
            return Err(invalid("own-image selection contains a parent component"));
        }
        let number = target
            .file_name()
            .and_then(|name| name.to_str())
            .and_then(crate::store::parse_build_name)
            .filter(|number| *number > 0)
            .ok_or_else(|| invalid("own-image selection names no canonical build"))?;
        let directory = if target.is_absolute() {
            target
        } else {
            program.join(target)
        };
        if directory.parent() != Some(program.as_path()) {
            return Err(invalid("own-image selection leaves its program store"));
        }
        Self::build_at(&prefix, number)
    }

    /// Validate an immutable build by number, including a prior rollback image.
    ///
    /// # Errors
    /// The build is incomplete, escapes its package, or lacks valid metadata or executable.
    pub fn build_at(prefix: &Path, build: u64) -> io::Result<Self> {
        if build == 0 || !prefix.is_absolute() {
            return Err(invalid("own-image prefix or build is invalid"));
        }
        let prefix = prefix.canonicalize()?;
        let program = prefix.join("store").join(PACKAGE);
        let directory = program.join(build.to_string());
        let directory_meta = std::fs::symlink_metadata(&directory)?;
        if !directory_meta.is_dir() || directory.canonicalize()?.parent() != Some(program.as_path())
        {
            return Err(invalid(
                "own-image build is not a direct real package directory",
            ));
        }
        let ready =
            crate::store::read_bounded(&program.join(format!("{build}.ready")), 65_536, false)
                .ok_or_else(|| invalid("own-image readiness record is unreadable"))?;
        if !crate::store::ready_text_accepts(&ready, &crate::store::running_platform()) {
            return Err(invalid("own-image build is not ready on this platform"));
        }
        let record =
            crate::store::read_bounded(&program.join(format!("{build}.version")), 256, false)
                .ok_or_else(|| invalid("own-image version record is unreadable"))?;
        let mut lines = record.lines();
        if lines.next() != Some("version v1") {
            return Err(invalid(
                "own-image version record has an unsupported schema",
            ));
        }
        let version = lines
            .next()
            .filter(|version| version_word(version))
            .ok_or_else(|| invalid("own-image version is invalid"))?
            .to_owned();
        if lines.next().is_some() {
            return Err(invalid("own-image version record has trailing data"));
        }
        let path = directory.join("bin").join(COMMAND);
        let metadata = std::fs::symlink_metadata(&path)?;
        let canonical = path.canonicalize()?;
        if !metadata.is_file()
            || metadata.mode() & 0o111 == 0
            || metadata.mode() & 0o6000 != 0
            || metadata.ino() == 0
            || !canonical.starts_with(&directory)
        {
            return Err(invalid(
                "own-image executable is not a real executable in its build",
            ));
        }
        Ok(Self {
            prefix,
            build,
            directory,
            executable: crate::procs::Executable {
                path: canonical,
                device: metadata.dev(),
                inode: metadata.ino(),
            },
            version,
            file_revision: [
                metadata.len(),
                metadata.mtime() as u64,
                metadata.mtime_nsec() as u64,
                metadata.ctime() as u64,
                metadata.ctime_nsec() as u64,
            ],
        })
    }

    /// Revalidate the complete build and executable identity after obtaining retention.
    ///
    /// # Errors
    /// The build, metadata or executable changed since selection.
    pub fn revalidate(&self) -> io::Result<()> {
        if Self::build_at(&self.prefix, self.build)? != *self {
            return Err(invalid("own-image build changed while preparing transfer"));
        }
        Ok(())
    }

    /// Hold the native GC gate shared for this bounded image handoff.
    ///
    /// # Errors
    /// Reclamation is in progress, the gate is unsafe, or the selected build changed.
    pub fn retain(&self) -> io::Result<BuildHold> {
        BuildHold::take(self)
    }
}

fn version_word(version: &str) -> bool {
    let (base, stamp) = version
        .split_once('+')
        .map_or((version, None), |(base, stamp)| (base, Some(stamp)));
    let mut numbers = base.split('.');
    let canonical = |word: &str| {
        !word.is_empty()
            && (word == "0" || !word.starts_with('0'))
            && word.bytes().all(|byte| byte.is_ascii_digit())
            && word.parse::<u32>().is_ok()
    };
    let release = numbers.next().is_some_and(canonical)
        && numbers.next().is_some_and(canonical)
        && numbers.next() == Some("0")
        && numbers.next().is_none();
    release
        && stamp.is_none_or(|stamp| {
            stamp.strip_prefix('g').is_some_and(|sha| {
                (9..=40).contains(&sha.len())
                    && sha
                        .bytes()
                        .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
            })
        })
}

/// A shared atpkg GC gate, held only for a bounded forward/rollback transaction.
///
/// atpkg's `lease::reclaim` must acquire this same gate exclusively before
/// removing the build. An executable descriptor alone would not retain it.
#[derive(Debug)]
pub struct BuildHold {
    file: File,
    path: PathBuf,
    image: Image,
}

/// A refused inherited gate, with its original descriptor still owned by the caller.
/// Retain it while deciding whether the continuing child can safely roll back.
#[derive(Debug)]
pub struct RetentionRefusal {
    /// The same open description offered for adoption; no error path closes it.
    pub file: File,
    /// Why this descriptor could not be adopted as the selected image's gate.
    pub error: io::Error,
}

impl BuildHold {
    /// Adopt the same open GC gate that survived an image transfer.
    ///
    /// This validates the descriptor's native rendezvous and retains its existing
    /// open description. It is not proof of a capsule's origin or of transfer authority.
    ///
    /// # Errors
    /// The descriptor, gate pathname, lock or image does not match the retained build.
    pub fn from_inherited(image: &Image, file: File) -> Result<Self, RetentionRefusal> {
        let path = image
            .prefix
            .join("leases")
            .join(format!("store-{PACKAGE}-{}.gate", image.build));
        let validate = || {
            // Reject unrelated files before changing their lock state.
            Self::validate_file(image, &file, &path)?;
            file.try_lock_shared().map_err(|error| match error {
                std::fs::TryLockError::WouldBlock => {
                    io::Error::new(io::ErrorKind::WouldBlock, "inherited image GC gate is busy")
                }
                std::fs::TryLockError::Error(error) => error,
            })?;
            Self::validate_file(image, &file, &path)
        };
        match validate() {
            Ok(()) => Ok(Self {
                file,
                path,
                image: image.clone(),
            }),
            Err(error) => Err(RetentionRefusal { file, error }),
        }
    }

    fn take(image: &Image) -> io::Result<Self> {
        use std::os::unix::fs::DirBuilderExt as _;
        let leases = image.prefix.join("leases");
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
            return Err(invalid(
                "image GC gate directory is not a real prefix child",
            ));
        }
        let path = leases.join(format!("store-{PACKAGE}-{}.gate", image.build));
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
            .open(&path)?;
        let metadata = file.metadata()?;
        if !metadata.is_file()
            || metadata.nlink() != 1
            || metadata.mode() & 0o077 != 0
            || metadata.uid() != std::fs::symlink_metadata(&image.prefix)?.uid()
        {
            return Err(invalid("image GC gate is not a private regular rendezvous"));
        }
        file.try_lock_shared().map_err(|error| match error {
            std::fs::TryLockError::WouldBlock => {
                io::Error::new(io::ErrorKind::WouldBlock, "image GC is in progress")
            }
            std::fs::TryLockError::Error(error) => error,
        })?;
        let held = Self {
            file,
            path,
            image: image.clone(),
        };
        // A sweep can have opened and unlinked this rendezvous before our
        // shared lock succeeded. Prove that GC will still open this inode.
        held.revalidate()?;
        Ok(held)
    }

    /// The exact retained build.
    #[must_use]
    pub fn image(&self) -> &Image {
        &self.image
    }

    /// The native gate's path; it is not a lease record or an executable.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Recheck that the gate's pathname still refers to this held descriptor.
    ///
    /// # Errors
    /// A gate was removed, substituted or made unreadable.
    pub fn revalidate(&self) -> io::Result<()> {
        Self::validate_file(&self.image, &self.file, &self.path)
    }

    fn validate_file(image: &Image, file: &File, path: &Path) -> io::Result<()> {
        let leases = image.prefix.join("leases");
        if !std::fs::symlink_metadata(&leases)?.is_dir() || leases.canonicalize()? != leases {
            return Err(invalid("retention gate directory changed while held"));
        }
        let held = file.metadata()?;
        let current = std::fs::symlink_metadata(path)?;
        if !held.is_file()
            || held.nlink() != 1
            || held.mode() & 0o077 != 0
            || held.uid() != std::fs::symlink_metadata(&image.prefix)?.uid()
            || !current.is_file()
            || (held.dev(), held.ino()) != (current.dev(), current.ino())
        {
            return Err(invalid("retention gate changed while held"));
        }
        image.revalidate()
    }
}

impl AsFd for BuildHold {
    fn as_fd(&self) -> BorrowedFd<'_> {
        self.file.as_fd()
    }
}

impl AsRawFd for BuildHold {
    fn as_raw_fd(&self) -> std::os::fd::RawFd {
        self.file.as_raw_fd()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::lock_reclaim_gate;
    use std::os::unix::fs::{DirBuilderExt as _, PermissionsExt as _};
    use std::sync::atomic::{AtomicU64, Ordering};

    pub(super) fn fixture() -> (PathBuf, Image) {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let base = std::env::temp_dir().join(".ubd");
        std::fs::create_dir_all(&base).unwrap();
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let prefix = base.join(format!(
            "own-image-{}-{stamp}-{}.noindex",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&prefix)
            .unwrap();
        let program = prefix.join("store").join(PACKAGE);
        let bin = program.join("7/bin");
        std::fs::create_dir_all(&bin).unwrap();
        let exe = bin.join(COMMAND);
        // Metadata fixture only: these bytes are never executed or presented as a valid image.
        std::fs::write(&exe, b"metadata-only image fixture").unwrap();
        std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o700)).unwrap();
        std::fs::write(
            program.join("7.ready"),
            format!("ok\nplatform={}\n", crate::store::running_platform()),
        )
        .unwrap();
        std::fs::write(program.join("7.version"), "version v1\n0.1.0+g123456789\n").unwrap();
        let image = Image::build_at(&prefix, 7).unwrap();
        (prefix, image)
    }

    #[test]
    fn own_versions_require_canonical_release_and_build_stamp() {
        for valid in ["0.1.0", "1.20.0+g123456789", "2.0.0+gabcdef123456789"] {
            assert!(version_word(valid), "{valid}");
        }
        for invalid in [
            "",
            "01.0.0",
            "1.0.1",
            "1.0.0-rc1",
            "1.0.0+g1234",
            "1.0.0+gABCDEF123",
            "1.0.0+other",
        ] {
            assert!(!version_word(invalid), "{invalid}");
        }
    }

    #[test]
    fn retained_image_blocks_the_native_gc_gate_until_the_hold_ends() {
        let (_, image) = fixture();
        let hold = image.retain().unwrap();
        let gc_gate = OpenOptions::new()
            .read(true)
            .write(true)
            .open(hold.path())
            .unwrap();
        assert!(
            matches!(gc_gate.try_lock(), Err(std::fs::TryLockError::WouldBlock)),
            "atpkg's exclusive reclaim gate cannot pass the shared image hold"
        );
        hold.revalidate().unwrap();
        drop(hold);
        lock_reclaim_gate(&gc_gate);
        gc_gate.unlock().unwrap();
    }

    #[test]
    fn inherited_retention_uses_the_same_open_gate_and_refuses_an_unrelated_file() {
        let (_, image) = fixture();
        let hold = image.retain().unwrap();
        let inherited = BuildHold::from_inherited(&image, hold.file.try_clone().unwrap()).unwrap();
        drop(hold);
        let gc = OpenOptions::new()
            .read(true)
            .write(true)
            .open(inherited.path())
            .unwrap();
        assert!(matches!(
            gc.try_lock(),
            Err(std::fs::TryLockError::WouldBlock)
        ));
        let unrelated = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&image.executable.path)
            .unwrap();
        let unrelated_fd = unrelated.as_raw_fd();
        let unrelated_probe = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&image.executable.path)
            .unwrap();
        let refused = BuildHold::from_inherited(&image, unrelated).unwrap_err();
        assert_eq!(refused.file.as_raw_fd(), unrelated_fd);
        unrelated_probe
            .try_lock()
            .expect("an unrelated file acquired no shared lock");
        unrelated_probe.unlock().unwrap();
        drop(inherited);
        lock_reclaim_gate(&gc);
        gc.unlock().unwrap();
    }

    #[test]
    fn refused_adoption_preserves_the_only_remaining_retention_description() {
        let (_, image) = fixture();
        let hold = image.retain().unwrap();
        let offered = hold.file.try_clone().unwrap();
        let gc = OpenOptions::new()
            .read(true)
            .write(true)
            .open(hold.path())
            .unwrap();
        std::fs::write(&image.executable.path, b"a changed metadata-only image").unwrap();
        let refused = BuildHold::from_inherited(&image, offered).unwrap_err();
        drop(hold);
        assert!(
            matches!(gc.try_lock(), Err(std::fs::TryLockError::WouldBlock)),
            "the refusal retains the offered lock even after the old wrapper is gone"
        );
        drop(refused);
        lock_reclaim_gate(&gc);
        gc.unlock().unwrap();
    }

    #[test]
    fn selection_metadata_and_image_substitution_refuse_transfer() {
        let (prefix, image) = fixture();
        std::fs::write(&image.executable.path, b"different length").unwrap();
        assert!(
            image.revalidate().is_err(),
            "an in-place image change is not the selected image"
        );
        let record = prefix.join("store").join(PACKAGE).join("7.version");
        std::fs::write(&record, "version v1\n0.1.0\nextra\n").unwrap();
        assert!(Image::build_at(&prefix, 7).is_err());
        std::fs::write(&record, "version v1\n0.1.0\n").unwrap();
        let gate = prefix.join("leases");
        std::fs::create_dir(&gate).unwrap();
        let target = gate.join(format!("store-{PACKAGE}-7.gate"));
        std::os::unix::fs::symlink(&record, target).unwrap();
        let refreshed = Image::build_at(&prefix, 7).unwrap();
        assert!(
            refreshed.retain().is_err(),
            "a substituted rendezvous is not a retention hold"
        );
    }

    #[test]
    fn an_internal_bin_alias_records_the_kernel_resolved_executable_path() {
        let (prefix, image) = fixture();
        let bin = image.directory.join("bin");
        let libexec = image.directory.join("libexec");
        // These are metadata-only data fixtures; no executable assessment was started.
        std::fs::rename(&bin, &libexec).unwrap();
        std::os::unix::fs::symlink("libexec", &bin).unwrap();
        let selected = Image::build_at(&prefix, image.build).unwrap();
        assert_eq!(selected.executable.path, libexec.join(COMMAND));
        assert_eq!(selected.executable.inode, image.executable.inode);
        selected.revalidate().unwrap();
    }
}
