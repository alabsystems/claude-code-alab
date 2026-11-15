// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! One cold-launch workspace-trust exchange, owned separately from the usage query.
//! Only the unmodified vendor writes its profile. A host policy answer is no SDK ACK,
//! and neither observation supplies interactive permission or input authority.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::io;
use std::os::unix::fs::{MetadataExt as _, OpenOptionsExt as _};
use std::path::{Path, PathBuf};

mod exchange;
mod gate;
mod keys;
mod signals;

/// The vendor's admission budget; host policy and native settlement have separate bounds.
pub const ADMISSION: std::time::Duration = std::time::Duration::from_secs(5);
/// Native path bytes accepted by the closed host-policy exchange.
pub const MAX_PATH: usize = 64 * 1024;

/// A completed cold-launch observation. No variant is an interactive mode lease.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// This launch needs no attestation, or its policy declined it.
    Skipped(String),
    /// A settled operation could not establish the vendor's recorded trust.
    Refused(String),
    /// A managed termination/stop was caught; native retirement and original
    /// signal-policy restoration precede this result or signal propagation.
    Interrupted {
        /// The first caught cancellation signal.
        signal: i32,
    },
    /// The original native child settled and the exact profile key then held true.
    Recorded {
        /// The host's folder-policy rule, not a vendor setting or input grant.
        rule: String,
        /// The vendor version in the matching private SDK reply.
        version: String,
    },
}

impl Outcome {
    /// A bounded journal description, with no credential or environment values.
    #[must_use]
    pub fn description(&self) -> String {
        match self {
            Self::Skipped(why) => format!("folder-trust skipped {why}"),
            Self::Refused(why) => format!("folder-trust refused {why}"),
            Self::Interrupted { signal } => format!("folder-trust interrupted signal={signal}"),
            Self::Recorded { rule, version } => {
                format!("folder-trust recorded rule={rule} vendor={version}")
            }
        }
    }
}

fn invalid(why: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, why)
}

fn quote(text: &str) -> String {
    crate::prime::Json::Str(text.into()).render()
}

/// Parse an explicitly selected vendor operation before ordinary launch parsing.
/// It preserves the caller's environment; it never selects another managed build.
pub(crate) struct Explicit {
    build: crate::store::Build,
    args: Vec<OsString>,
    terminal: bool,
}

impl Explicit {
    pub(crate) fn run(self) -> Result<Outcome, crate::sys::ProbeSpawnRefusal> {
        settle(&self.build, &self.args, self.terminal)
    }
}

pub(crate) fn entry(words: &[OsString]) -> Option<Result<Explicit, String>> {
    if words.first().is_none_or(|word| word != "trust") {
        return None;
    }
    Some(match words {
        [
            _,
            operation,
            flag,
            vendor,
            tty_flag,
            tty,
            separator,
            args @ ..,
        ] if operation == "folder"
            && flag == "--vendor"
            && tty_flag == "--stdout-tty"
            && separator == "--"
            && (tty == "0" || tty == "1")
            && Path::new(vendor).is_absolute() =>
        {
            let vendor = PathBuf::from(vendor);
            let build = crate::store::Build {
                number: 0,
                dir: vendor.parent().unwrap_or(Path::new("/")).into(),
                exe: vendor,
                version: None,
                env: Vec::new(),
            };
            Ok(Explicit {
                build,
                args: args.to_vec(),
                terminal: tty == "1",
            })
        }
        _ => Err("expected trust folder --vendor ABS --stdout-tty 0|1 -- ARGS".into()),
    })
}

pub(crate) fn response(result: &Outcome) -> String {
    let (status, rule, version, reason) = match result {
        Outcome::Skipped(why) => ("skipped", "null".into(), "null".into(), quote(why)),
        Outcome::Refused(why) => ("refused", "null".into(), "null".into(), quote(why)),
        Outcome::Recorded { rule, version } => {
            ("recorded", quote(rule), quote(version), "null".into())
        }
        Outcome::Interrupted { signal } => (
            "interrupted",
            "null".into(),
            "null".into(),
            quote(&format!("signal={signal}")),
        ),
    };
    format!(
        "{{\"schema\":1,\"operation\":\"folder-trust\",\"status\":{},\"rule\":{rule},\"version\":{version},\"reason\":{reason}}}\n",
        quote(status)
    )
}

struct Selected {
    file: std::fs::File,
    original: PathBuf,
    path: PathBuf,
    stamp: (u64, u64, u64, i64, i64, i64, i64),
}

impl Selected {
    fn open(path: &Path) -> io::Result<Self> {
        if !path.is_absolute() || path.as_os_str().len() > MAX_PATH {
            return Err(invalid("selected vendor path exceeds the native contract"));
        }
        let original = path.to_path_buf();
        let path = path.canonicalize()?;
        if !path.is_absolute() || path.as_os_str().len() > MAX_PATH {
            return Err(invalid("selected vendor path exceeds the native contract"));
        }
        let file = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(&path)?;
        let meta = file.metadata()?;
        if !meta.is_file() || meta.mode() & 0o111 == 0 {
            return Err(invalid("selected vendor is not an executable regular file"));
        }
        let selected = Self {
            file,
            original,
            path,
            stamp: stamp(&meta),
        };
        selected.current()?;
        Ok(selected)
    }

    fn current(&self) -> io::Result<()> {
        if stamp(&self.file.metadata()?) != self.stamp
            || stamp(&std::fs::symlink_metadata(&self.path)?) != self.stamp
            || self.original.canonicalize()? != self.path
            || stamp(&std::fs::metadata(&self.original)?) != self.stamp
        {
            return Err(invalid("original selected vendor changed"));
        }
        Ok(())
    }
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

fn environment(build: &crate::store::Build) -> BTreeMap<OsString, OsString> {
    let mut env: BTreeMap<_, _> = std::env::vars_os().collect();
    env.extend(build.env.iter().map(|(k, v)| (k.into(), v.into())));
    env.extend(
        crate::profile::HARNESS_ENV
            .iter()
            .map(|(k, v)| ((*k).into(), (*v).into())),
    );
    env
}

fn interactive(args: &[OsString]) -> bool {
    let Some(words) = args
        .iter()
        .map(|word| word.to_str().map(str::to_owned))
        .collect::<Option<Vec<_>>>()
    else {
        return false;
    };
    let argv = std::iter::once(crate::profile::COMMAND.to_owned())
        .chain(words)
        .collect::<Vec<_>>();
    let Ok(_) = crate::profile::resume_argv(&argv, "00000000-0000-0000-0000-000000000001") else {
        return false;
    };
    let Some(parsed) = crate::profile::launch_words(&argv, &[]) else {
        return false;
    };
    !parsed.flags.iter().any(|(name, _)| *name == "--bare")
        && parsed.first.is_none_or(|word| {
            word.is_empty()
                || !word
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        })
}

/// Settle cold workspace trust for the already selected vendor and launch environment.
/// Standard-output terminal state belongs to the original caller, before any private pipe.
/// # Errors
/// Original selected-image custody changed, or native child/group or signal-mask custody
/// remains unsettled. Retain the error and do not start the vendor.
pub fn settle(
    build: &crate::store::Build,
    args: &[OsString],
    stdout_terminal: bool,
) -> Result<Outcome, crate::sys::ProbeSpawnRefusal> {
    settle_inner(build, args, stdout_terminal, None)
}

/// The same operation retaining its private wire bytes in an existing empty evidence directory.
/// Files are create-new; failed evidence cannot produce a recorded result.
/// # Errors
/// As [`settle`]. Evidence is observation data and grants no runtime authority.
pub fn settle_recorded(
    build: &crate::store::Build,
    args: &[OsString],
    stdout_terminal: bool,
    records: &Path,
) -> Result<Outcome, crate::sys::ProbeSpawnRefusal> {
    settle_inner(build, args, stdout_terminal, Some(records))
}

fn settle_inner(
    build: &crate::store::Build,
    args: &[OsString],
    stdout_terminal: bool,
    records: Option<&Path>,
) -> Result<Outcome, crate::sys::ProbeSpawnRefusal> {
    if !stdout_terminal || !interactive(args) {
        return Ok(Outcome::Skipped("not-interactive".into()));
    }
    let env = environment(build);
    if env
        .get(std::ffi::OsStr::new("ATERM_NO_HARNESS"))
        .is_some_and(|word| word == "1")
    {
        return Ok(Outcome::Skipped("no-harness".into()));
    }
    let Some(host) = crate::launch::host() else {
        return Ok(Outcome::Skipped("no-host".into()));
    };
    let Some(session) = env
        .get(std::ffi::OsStr::new("ATERM_PARENT_SESSION_ID"))
        .and_then(|v| v.to_str())
        .filter(|v| !v.is_empty() && v.len() <= 64)
    else {
        return Ok(Outcome::Skipped("no-session".into()));
    };
    let setup = (|| -> io::Result<_> {
        let cwd = std::env::current_dir()?.canonicalize()?;
        let home = crate::store::home_dir(env.get(std::ffi::OsStr::new("HOME")).cloned())
            .ok_or_else(|| invalid("selected home is unavailable"))?;
        if keys::same_dir(&home, &cwd) {
            return Ok(None);
        }
        let profile =
            keys::profile(&home, &env).ok_or_else(|| invalid("selected profile is unavailable"))?;
        let key = keys::trust_key(&cwd);
        if keys::recorded(&profile, &key) {
            return Ok(None);
        }
        Ok(Some((cwd, home, profile, key)))
    })();
    let (cwd, home, profile, key) = match setup {
        Ok(Some(setup)) => setup,
        Ok(None) => return Ok(Outcome::Skipped("home-or-already-recorded".into())),
        Err(_) => return Ok(Outcome::Refused("launch-context-unread".into())),
    };
    let selected = Selected::open(&build.exe)?;
    let request = match gate::Request::new(&cwd, &selected.path, session, stdout_terminal) {
        Ok(request) => request,
        Err(_) => return Ok(Outcome::Refused("policy-request-unavailable".into())),
    };
    let Some(mut scope) = signals::Scope::install()? else {
        return Ok(Outcome::Refused(
            "original-native-wait-policy-unavailable".into(),
        ));
    };
    let observed = (|| -> Result<Outcome, crate::sys::ProbeSpawnRefusal> {
        if scope.cancelled() {
            return Ok(Outcome::Skipped("cancelled-before-policy".into()));
        }
        let changes = env
            .iter()
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect::<Vec<_>>();
        let Some(rule) = gate::query(&host, &request, &changes, records)? else {
            return Ok(Outcome::Refused("policy-refused-or-unread".into()));
        };
        if scope.cancelled() {
            return Ok(Outcome::Skipped("cancelled-before-attestation".into()));
        }
        selected.current()?;
        if std::env::current_dir().ok().as_ref() != Some(&cwd) {
            return Ok(Outcome::Refused("selected-context-changed".into()));
        }
        let reply = exchange::attest(&selected, &cwd, &env, records, &mut || scope.cancelled())?;
        // Known changed payloads cannot continue through the changed pathname.
        selected.current()?;
        if let Some(version) = reply
            && keys::trust_key(&cwd) == key
            && keys::profile(&home, &env).as_ref() == Some(&profile)
            && keys::recorded(&profile, &key)
        {
            return Ok(Outcome::Recorded { rule, version });
        }
        Ok(Outcome::Refused(
            "vendor-did-not-establish-recorded-trust".into(),
        ))
    })();
    match observed {
        Err(failure) => Err(scope.retain(failure)),
        Ok(outcome) => match scope.finish()? {
            Some(signal) => Ok(Outcome::Interrupted { signal }),
            None => Ok(outcome),
        },
    }
}

#[cfg(test)]
#[path = "folder_trust_tests.rs"]
mod tests;
