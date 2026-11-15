// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The complete local replacement data is pinned before its first SIGTERM.
//! This is not a Native1 grant. External host ordering and pre-exec child
//! registration remain separate requirements before scoped claim activation.

#[path = "supervise_vendor_retention.rs"]
mod retention;

use crate::cli;
use crate::store::Build;
use crate::sys::{self, DescriptorIdentity, TermiosSnapshot};
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::io;
use std::os::unix::fs::MetadataExt;
use std::path::PathBuf;
use std::process::Command;

#[derive(Clone, PartialEq, Eq)]
pub(super) struct Launch {
    pub build: Build,
    pub args: Vec<OsString>,
    pub environment: Vec<(OsString, OsString)>,
    hold: retention::Hold,
}
impl std::fmt::Debug for Launch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Launch")
            .field("build", &self.build.number)
            .field("executable", &self.build.exe)
            .field("args", &self.args)
            .field(
                "environment_keys",
                &self
                    .environment
                    .iter()
                    .map(|(key, _)| key)
                    .collect::<Vec<_>>(),
            )
            .finish()
    }
}
impl Launch {
    fn capture(
        build: Build,
        args: &[OsString],
        base: &[(OsString, OsString)],
        depth: u32,
    ) -> io::Result<Self> {
        let hold = retention::Hold::take(&build)?;
        let mut environment: BTreeMap<OsString, OsString> = base.iter().cloned().collect();
        for (key, value) in &build.env {
            environment.insert(key.into(), value.into());
        }
        for (key, value) in crate::profile::HARNESS_ENV {
            environment.insert((*key).into(), (*value).into());
        }
        for (key, value) in cli::child_marks(depth) {
            environment.insert(key.into(), value.into());
        }
        Ok(Self {
            build,
            args: args.to_vec(),
            environment: environment.into_iter().collect(),
            hold,
        })
    }
    pub fn command(&self) -> Command {
        let mut command = crate::launch::vendor_command(&self.build.exe, &self.args, true);
        command
            .env_clear()
            .envs(self.environment.iter().map(|(k, v)| (k, v)));
        crate::launch::renderer(&mut command);
        // The continuing parent never changes cwd. Inheriting it keeps the
        // original directory object even if its pathname was renamed meanwhile.
        command
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Plan {
    pub old: Launch,
    pub target: Launch,
    pub session_id: String,
    pub resume: Vec<String>,
    pub cwd: PathBuf,
    cwd_device: u64,
    cwd_inode: u64,
    pub stdio: [Option<DescriptorIdentity>; 3],
    pub original_terminal: Option<TermiosSnapshot>,
}
fn stdio() -> io::Result<[Option<DescriptorIdentity>; 3]> {
    let mut roles = [None; 3];
    for (fd, role) in roles.iter_mut().enumerate() {
        match sys::descriptor_identity(fd as libc::c_int) {
            Ok(identity) => *role = Some(identity),
            Err(error) if error.raw_os_error() == Some(libc::EBADF) => {}
            Err(error) => return Err(error),
        }
    }
    Ok(roles)
}
impl Plan {
    pub fn capture(
        old: Build,
        target: Build,
        session_id: String,
        resume: Vec<String>,
        depth: u32,
        original_terminal: Option<TermiosSnapshot>,
    ) -> io::Result<Self> {
        let cwd = std::env::current_dir()?;
        let metadata = std::fs::metadata(".")?;
        let base: Vec<_> = std::env::vars_os().collect();
        let args: Vec<_> = resume.iter().map(OsString::from).collect();
        let value = Self {
            old: Launch::capture(old, &args, &base, depth)?,
            target: Launch::capture(target, &args, &base, depth)?,
            session_id,
            resume,
            cwd,
            cwd_device: metadata.dev(),
            cwd_inode: metadata.ino(),
            stdio: stdio()?,
            original_terminal,
        };
        value.revalidate_all()?;
        Ok(value)
    }
    pub fn revalidate(&self) -> io::Result<()> {
        let cwd = std::fs::metadata(".")?;
        if (cwd.dev(), cwd.ino()) != (self.cwd_device, self.cwd_inode)
            || stdio()? != self.stdio
            || self
                .original_terminal
                .as_ref()
                .is_some_and(|terminal| self.stdio[0] != Some(terminal.terminal))
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "pinned vendor working directory or terminal descriptors changed",
            ));
        }
        Ok(())
    }
    pub fn revalidate_for(&self, launch: &Launch) -> io::Result<()> {
        self.revalidate()?;
        launch.hold.revalidate()
    }
    pub fn revalidate_all(&self) -> io::Result<()> {
        self.revalidate()?;
        self.old.hold.revalidate()?;
        self.target.hold.revalidate()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use std::sync::atomic::{AtomicU64, Ordering};

    static SEQUENCE: AtomicU64 = AtomicU64::new(0);

    fn fixture_build(env: Vec<(String, String)>) -> Build {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let prefix = std::env::temp_dir().join(".ubd").join(format!(
            "claude-vendor-launch-{}-{stamp}-{}.noindex",
            std::process::id(),
            SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        let dir = prefix.join("store/claude/1");
        let exe = dir.join("bin/claude");
        std::fs::create_dir_all(exe.parent().unwrap()).unwrap();
        std::fs::write(&exe, b"fixture data, never a launched program\n").unwrap();
        std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o700)).unwrap();
        Build {
            number: 1,
            dir,
            exe,
            version: Some("9.9.1".into()),
            env,
        }
    }

    #[test]
    fn launch_capture_always_forces_vendor_self_updater_off() {
        let build = fixture_build(vec![("DISABLE_AUTOUPDATER".into(), "0".into())]);
        let launch = Launch::capture(build, &[], &[], 0).unwrap();
        let env: BTreeMap<_, _> = launch.environment.into_iter().collect();
        assert_eq!(
            env.get(std::ffi::OsStr::new("DISABLE_AUTOUPDATER")),
            Some(&OsString::from("1"))
        );
        assert_eq!(
            env.get(std::ffi::OsStr::new("CLAUDE_CODE_DISABLE_FEEDBACK_SURVEY")),
            Some(&OsString::from("1"))
        );
    }
}
