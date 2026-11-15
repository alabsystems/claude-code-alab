// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Bounded settings snapshots and guarded, directory-relative backup/replacement.
//! Cooperating writers keep the named lock stable. Checks detect observed outside
//! changes; the final comparison and rename are not atomic CAS against uncooperative savers.

use std::ffi::{OsStr, OsString};
use std::fs::{File, Permissions};
use std::io::{self, Read as _, Write as _};
#[cfg(target_os = "macos")]
use std::os::fd::AsRawFd as _;
use std::os::unix::fs::MetadataExt as _;
use std::path::{Path, PathBuf};

#[path = "directory.rs"]
mod directory;
use directory::{Directory, Identity, identity};

const MAX_SETTINGS_BYTES: u64 = 16 * 1024 * 1024;
const REWRITE_TRIES: usize = 3;
const BACKUP_CANDIDATES: usize = 64;

pub(super) struct Lock {
    directory: Directory,
    file: File,
    id: Identity,
    name: OsString,
    lock_name: OsString,
    target: PathBuf,
}

fn resolve(path: &Path) -> io::Result<Option<PathBuf>> {
    match std::fs::canonicalize(path) {
        Ok(path) => Ok(Some(path)),
        Err(error) => {
            if let Some(why) = dangling_link(path) {
                return Err(io::Error::new(error.kind(), why));
            }
            if error.kind() != io::ErrorKind::NotFound {
                return Err(error);
            }
            let parent = path
                .parent()
                .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "settings parent"))?;
            let parent = match std::fs::canonicalize(parent) {
                Ok(parent) => parent,
                Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
                Err(error) => return Err(error),
            };
            Ok(Some(parent.join(path.file_name().ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidInput, "settings name")
            })?)))
        }
    }
}

fn private_lock(file: &File) -> io::Result<bool> {
    let meta = file.metadata()?;
    // SAFETY: geteuid has no input pointers and observes this process's effective uid.
    let uid = unsafe { libc::geteuid() };
    Ok(meta.is_file() && meta.nlink() == 1 && meta.uid() == uid && meta.mode() & 0o077 == 0)
}

impl Lock {
    pub(super) fn acquire(path: &Path) -> io::Result<Option<Self>> {
        let Some(target) = resolve(path)? else {
            return Ok(None);
        };
        let parent = target
            .parent()
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "resolved parent"))?;
        let directory = Directory::open(parent)?;
        let name = target
            .file_name()
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "resolved name"))?
            .to_owned();
        let mut lock_name = OsString::from(".");
        lock_name.push(&name);
        lock_name.push(".alab-prime.lock");
        let file = directory.lock_file(&lock_name)?;
        if !private_lock(&file)? {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "settings lock is not private and owned",
            ));
        }
        file.try_lock().map_err(io::Error::from)?;
        let lock = Self {
            id: identity(&file.metadata()?),
            file,
            directory,
            name,
            lock_name,
            target,
        };
        if !lock.still(path)? {
            return Err(io::Error::other(
                "settings selection changed during lock acquisition",
            ));
        }
        Ok(Some(lock))
    }

    fn still(&self, selected: &Path) -> io::Result<bool> {
        if !self.directory.is_at_path()? || resolve(selected)?.as_ref() != Some(&self.target) {
            return Ok(false);
        }
        let Some(named) = self.directory.read(&self.lock_name)? else {
            return Ok(false);
        };
        Ok(identity(&named.metadata()?) == self.id
            && private_lock(&self.file)?
            && private_lock(&named)?)
    }

    /// Upper bound of the native path bytes for every backup candidate this
    /// exact held target can create, before any settings mutation is attempted.
    pub(super) fn backup_path_bound(&self) -> usize {
        use std::os::unix::ffi::OsStrExt as _;
        self.target
            .as_os_str()
            .as_bytes()
            .len()
            .saturating_add(format!(".bak-{}-{}", u64::MAX, BACKUP_CANDIDATES - 1).len())
    }

    /// Retain the selected alias's original parent independently of the physical
    /// target parent. This only permits bounded auxiliary reads, never mutation.
    pub(super) fn selected_auxiliary(&self, selected: &Path) -> io::Result<Auxiliary> {
        if !self.still(selected)? {
            return Err(io::Error::other(
                "selected settings changed before auxiliary capture",
            ));
        }
        let parent = selected.parent().ok_or(io::ErrorKind::InvalidInput)?;
        let directory = Directory::open(&std::fs::canonicalize(parent)?)?;
        if !directory.is_at_path()?
            || std::fs::canonicalize(parent)? != directory.path
            || !self.still(selected)?
        {
            return Err(io::Error::other(
                "selected settings parent changed during auxiliary capture",
            ));
        }
        Ok(Auxiliary {
            directory,
            name: selected
                .file_name()
                .ok_or(io::ErrorKind::InvalidInput)?
                .to_owned(),
        })
    }

    pub(super) fn read_auxiliary(&self, suffix: &OsStr) -> io::Result<Option<String>> {
        auxiliary_text(&self.directory, &self.name, suffix)
    }
}

pub(super) struct Auxiliary {
    directory: Directory,
    name: OsString,
}

impl Auxiliary {
    pub(super) fn read(&self, suffix: &OsStr) -> io::Result<Option<String>> {
        auxiliary_text(&self.directory, &self.name, suffix)
    }
}

fn auxiliary_text(
    directory: &Directory,
    name: &OsStr,
    suffix: &OsStr,
) -> io::Result<Option<String>> {
    let mut name = name.to_owned();
    name.push(suffix);
    directory
        .read_auxiliary(&name)?
        .as_ref()
        .map(bounded)
        .transpose()
}

impl Drop for Lock {
    fn drop(&mut self) {
        let _ = self.file.unlock();
    }
}

struct AsRead {
    text: Option<String>,
    // Retaining the original descriptor pins its inode until validation/cleanup ends.
    file: Option<File>,
    permissions: Option<Permissions>,
}

fn bounded(file: &File) -> io::Result<String> {
    if file.metadata()?.len() > MAX_SETTINGS_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "settings exceed size limit",
        ));
    }
    let mut text = String::new();
    file.take(MAX_SETTINGS_BYTES + 1)
        .read_to_string(&mut text)?;
    if text.len() as u64 > MAX_SETTINGS_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "settings exceed size limit",
        ));
    }
    Ok(text)
}

impl AsRead {
    fn read(lock: &Lock) -> io::Result<Self> {
        let Some(file) = lock.directory.read(&lock.name)? else {
            return Ok(Self {
                text: None,
                file: None,
                permissions: None,
            });
        };
        let permissions = file.metadata()?.permissions();
        let text = bounded(&file)?;
        Ok(Self {
            text: Some(text),
            file: Some(file),
            permissions: Some(permissions),
        })
    }

    fn still(&self, lock: &Lock) -> io::Result<bool> {
        match (lock.directory.read(&lock.name)?, &self.file, &self.text) {
            (None, None, None) => Ok(true),
            (Some(current), Some(original), Some(text)) => {
                let meta = current.metadata()?;
                if identity(&meta) != identity(&original.metadata()?)
                    || Some(meta.permissions()) != self.permissions
                    || meta.len() != text.len() as u64
                {
                    return Ok(false);
                }
                let same = bounded(&current)? == *text;
                Ok(same && Some(current.metadata()?.permissions()) == self.permissions)
            }
            _ => Ok(false),
        }
    }
}

struct Created {
    name: OsString,
    file: File,
}

impl Created {
    fn remove(&self, directory: &Directory) {
        // Best effort on the original directory and original still-open created inode.
        let _ = directory.remove_owned(&self.name, &self.file);
    }
}

fn new_backup(
    directory: &Directory,
    name: &OsStr,
    stamp: u64,
    fill: impl FnOnce(&mut File) -> io::Result<()>,
) -> Result<Created, String> {
    for n in 0..BACKUP_CANDIDATES {
        let mut candidate = name.to_owned();
        candidate.push(if n == 0 {
            format!(".bak-{stamp}")
        } else {
            format!(".bak-{stamp}-{n}")
        });
        match directory.create(&candidate) {
            Ok(file) => {
                let mut created = Created {
                    name: candidate,
                    file,
                };
                if let Err(error) = fill(&mut created.file) {
                    created.remove(directory);
                    return Err(error.to_string());
                }
                return Ok(created);
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error.to_string()),
        }
    }
    Err("no free backup name".into())
}

fn ordered(file: &File) -> io::Result<()> {
    #[cfg(target_os = "macos")]
    {
        let flush = |operation| loop {
            // SAFETY: the live file owns this descriptor; these fcntl operations take no pointer argument.
            if unsafe { libc::fcntl(file.as_raw_fd(), operation) } == 0 {
                return Ok(());
            }
            let error = io::Error::last_os_error();
            if error.kind() != io::ErrorKind::Interrupted {
                return Err(error);
            }
        };
        match flush(libc::F_BARRIERFSYNC) {
            Err(error)
                if matches!(
                    error.raw_os_error(),
                    Some(libc::ENODEV | libc::EINVAL | libc::ENOTTY | libc::ENOTSUP)
                ) =>
            {
                flush(libc::F_FULLFSYNC)
            }
            result => result,
        }
    }
    #[cfg(not(target_os = "macos"))]
    file.sync_all()
}

fn dangling_link(path: &Path) -> Option<String> {
    let (nearest, link) =
        path.ancestors()
            .find_map(|path| match std::fs::symlink_metadata(path) {
                Err(error) if error.kind() == io::ErrorKind::NotFound => None,
                result => Some((path, result.is_ok_and(|meta| meta.file_type().is_symlink()))),
            })?;
    if !link || std::fs::metadata(nearest).is_ok() {
        return None;
    }
    Some(
        if nearest == path {
            "a symlink whose target does not exist"
        } else {
            "under a symlinked directory whose target does not exist"
        }
        .into(),
    )
}

pub(super) enum Unwritten {
    Read(io::Error),
    Refused(String),
    Write(io::Error),
    Changed,
}
impl Unwritten {
    pub(super) fn words(self) -> String {
        match self {
            Self::Read(error) | Self::Write(error) => error.to_string(),
            Self::Refused(why) => why,
            Self::Changed => "settings changed during guarded replacement".into(),
        }
    }
}

#[cfg(test)]
type Save = Box<dyn FnMut(&Path)>;
#[cfg(test)]
thread_local! {
    static SAVE_AFTER_READ: std::cell::RefCell<Option<Save>> = const { std::cell::RefCell::new(None) };
    static BEFORE_FINAL_CHECK: std::cell::RefCell<Option<Box<dyn FnMut()>>> = const { std::cell::RefCell::new(None) };
}
#[cfg(test)]
pub(super) fn saving_after_read<R>(
    save: impl FnMut(&Path) + 'static,
    run: impl FnOnce() -> R,
) -> R {
    struct Reset;
    impl Drop for Reset {
        fn drop(&mut self) {
            SAVE_AFTER_READ.with_borrow_mut(|slot| *slot = None);
        }
    }
    SAVE_AFTER_READ.with_borrow_mut(|slot| {
        assert!(slot.is_none());
        *slot = Some(Box::new(save));
    });
    let _reset = Reset;
    run()
}
#[cfg(test)]
pub(super) fn before_final_check<R>(check: impl FnMut() + 'static, run: impl FnOnce() -> R) -> R {
    struct Reset;
    impl Drop for Reset {
        fn drop(&mut self) {
            BEFORE_FINAL_CHECK.with_borrow_mut(|slot| *slot = None);
        }
    }
    BEFORE_FINAL_CHECK.with_borrow_mut(|slot| {
        assert!(slot.is_none());
        *slot = Some(Box::new(check));
    });
    let _reset = Reset;
    run()
}

pub(super) fn rewrite<T>(
    path: &Path,
    lock: &Lock,
    backup: bool,
    mut edit: impl FnMut(Option<&str>) -> Result<Option<(String, T)>, String>,
) -> Result<Option<(T, Option<PathBuf>)>, Unwritten> {
    static WRITES: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    for _ in 0..REWRITE_TRIES {
        if !lock.still(path).map_err(Unwritten::Read)? {
            return Err(Unwritten::Changed);
        }
        let read = AsRead::read(lock).map_err(Unwritten::Read)?;
        #[cfg(test)]
        SAVE_AFTER_READ.with_borrow_mut(|save| save.as_mut().map(|save| save(path)));
        let Some((text, done)) = edit(read.text.as_deref()).map_err(Unwritten::Refused)? else {
            return Ok(None);
        };
        let copy = if backup {
            match &read.text {
                Some(old) => {
                    let stamp = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map_or(0, |d| d.as_secs());
                    Some(
                        new_backup(&lock.directory, &lock.name, stamp, |file| {
                            file.write_all(old.as_bytes())?;
                            ordered(file)?;
                            if let Some(mode) = &read.permissions {
                                file.set_permissions(mode.clone())?;
                            }
                            Ok(())
                        })
                        .map_err(Unwritten::Refused)?,
                    )
                }
                None => None,
            }
        } else {
            None
        };
        let mut temp_name = OsString::from(".");
        temp_name.push(&lock.name);
        temp_name.push(format!(
            ".alab-prime-tmp-{}-{}",
            std::process::id(),
            WRITES.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        let written = (|| {
            let mut temp = Created {
                file: lock.directory.create(&temp_name)?,
                name: temp_name,
            };
            let effect = (|| {
                temp.file.write_all(text.as_bytes())?;
                ordered(&temp.file)?;
                if let Some(mode) = &read.permissions {
                    temp.file.set_permissions(mode.clone())?;
                }
                #[cfg(test)]
                BEFORE_FINAL_CHECK.with_borrow_mut(|check| check.as_mut().map(|check| check()));
                if !lock.still(path)? || !read.still(lock)? {
                    return Ok(false);
                }
                lock.directory.rename(&temp.name, &lock.name)?;
                Ok(true)
            })();
            if !matches!(effect, Ok(true)) {
                temp.remove(&lock.directory);
            }
            effect
        })();
        if !matches!(written, Ok(true))
            && let Some(copy) = &copy
        {
            copy.remove(&lock.directory);
        }
        if written.map_err(Unwritten::Write)? {
            return Ok(Some((
                done,
                copy.map(|copy| lock.directory.path.join(copy.name)),
            )));
        }
    }
    Err(Unwritten::Changed)
}

#[cfg(test)]
#[path = "write_tests.rs"]
mod tests;
