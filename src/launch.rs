// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Claude Code's managed launch policy. The atpkg launcher supplies only the
//! `run --` or `launch --` envelope and the path of its host; vendor arguments, renderer and
//! host services are selected here so changing them needs no aterm rebuild.

use std::ffi::{OsStr, OsString};
use std::io;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

/// The generic launcher supplies its own aterm executable, never a PATH lookup.
pub const HOST_VAR: &str = "ATERM_HARNESS_HOST";
const BYPASS: &str = "--dangerously-skip-permissions";
const UPDATE_WORDS: &[&str] = &["update", "upgrade", "install"];
const LAUNCH_FLAGS: &[&str] = &["--yolo", "--no-harness", "--no-alab-harness"];
const POSTURE: &[&str] = &[
    BYPASS,
    "--allow-dangerously-skip-permissions",
    "--permission-mode",
    "--permission-prompts",
    "--restricted",
    "--allowedTools",
    "--allowed-tools",
    "--disallowedTools",
    "--disallowed-tools",
    "--tools",
    "--settings",
    "--setting-sources",
    "--resume",
    "--continue",
    "-r",
    "-c",
];

struct Scan {
    flags: Vec<(String, usize)>,
    first: Option<String>,
}

fn scan(args: &[OsString]) -> Option<Scan> {
    let words = std::iter::once(Some(crate::profile::COMMAND.to_string()))
        .chain(
            args.iter()
                .take_while(|word| *word != "--")
                .map(|word| word.to_str().map(str::to_owned)),
        )
        .collect::<Option<Vec<_>>>()?;
    let parsed = crate::profile::launch_words(&words, LAUNCH_FLAGS)?;
    Some(Scan {
        flags: parsed
            .flags
            .into_iter()
            .map(|(name, at)| (name.to_string(), at - 1))
            .collect(),
        first: parsed.first.map(str::to_owned),
    })
}

fn info_flag(name: &str) -> bool {
    matches!(name, "--help" | "-h" | "--version" | "-v")
}

/// Whether the arguments use atpkg's managed envelope after leading harness options.
#[must_use]
pub fn managed(args: &[OsString]) -> bool {
    let at = args
        .iter()
        .take_while(|word| {
            word.to_str()
                .is_some_and(|word| word.starts_with("--alab-"))
        })
        .count();
    args.get(at)
        .is_some_and(|word| word == crate::cli::ENTRY_WORD || word == crate::cli::LAUNCH_ENTRY_WORD)
        && args.get(at + 1).is_some_and(|word| word == "--")
}

/// Vendor help and version queries require neither account services nor launch
/// defaults. A word after a literal `--` is a prompt, not a query.
#[must_use]
pub fn informational(args: &[OsString]) -> bool {
    scan(args).is_some_and(|scan| scan.flags.iter().any(|(name, _)| info_flag(name)))
}

/// Remove launcher-only switches before the literal `--`, leaving its suffix exact.
/// The boolean says that the launch explicitly leaves supervision out.
#[must_use]
pub fn switches(args: Vec<OsString>) -> (Vec<OsString>, bool) {
    let Some(scan) = scan(&args) else {
        return (args, false);
    };
    let mut removed = scan
        .flags
        .into_iter()
        .filter_map(|(name, at)| {
            matches!(name.as_str(), "--no-harness" | "--no-alab-harness").then_some(at)
        })
        .peekable();
    let disabled = removed.peek().is_some();
    let args = args
        .into_iter()
        .enumerate()
        .filter_map(|(at, word)| {
            if removed.peek() == Some(&at) {
                removed.next();
                None
            } else {
                Some(word)
            }
        })
        .collect();
    (args, disabled)
}

/// Apply the managed Claude permission default, preserving explicit posture and
/// unknown flags. `--yolo` expands only before a literal `--` and never duplicates
/// the vendor's own flag. Known option values use the resume grammar's boundaries.
#[must_use]
pub fn permission_args(mut args: Vec<OsString>) -> Vec<OsString> {
    let Some(scan) = scan(&args) else { return args };
    if scan.flags.iter().any(|(name, _)| info_flag(name)) {
        return args;
    }
    let explicit = scan.flags.iter().any(|(name, _)| name == "--yolo");
    if explicit {
        let already = scan.flags.iter().any(|(name, _)| name == BYPASS);
        let mut added = already;
        let mut result = Vec::with_capacity(args.len() + 1);
        let mut yolo = scan
            .flags
            .iter()
            .filter_map(|(name, at)| (name == "--yolo").then_some(*at))
            .peekable();
        for (at, word) in args.into_iter().enumerate() {
            if yolo.peek() == Some(&at) {
                yolo.next();
                if !added {
                    result.push(BYPASS.into());
                    added = true;
                }
            } else {
                result.push(word);
            }
        }
        return result;
    }
    if scan
        .flags
        .iter()
        .any(|(name, _)| POSTURE.contains(&name.as_str()))
    {
        return args;
    }
    if !scan
        .first
        .as_deref()
        .is_some_and(|word| UPDATE_WORDS.contains(&word))
    {
        args.insert(0, BYPASS.into());
    }
    args
}

/// The host executable named by the launcher, if it is an absolute executable file.
#[must_use]
pub fn host() -> Option<PathBuf> {
    host_path(std::env::var_os(HOST_VAR).as_deref())
}

fn host_path(value: Option<&OsStr>) -> Option<PathBuf> {
    let path = Path::new(value?);
    let meta = std::fs::metadata(path).ok()?;
    (path.is_absolute() && meta.is_file() && meta.permissions().mode() & 0o111 != 0)
        .then(|| path.to_path_buf())
}

fn enabled() -> bool {
    std::env::var_os("ATERM_PARENT_SESSION_ID").is_some_and(|value| !value.is_empty())
        && !crate::cli::left_out(
            std::env::var_os(crate::cli::NO_HARNESS_VAR).as_deref(),
            crate::supervise::image::hosted_by_aterm(),
        )
}

/// Intercept a managed vendor update before resolving its current store build.
/// The host remains the package manager; the harness owns when Claude asks it.
pub fn self_update(args: &[OsString]) -> Result<Option<Command>, String> {
    if !update_action(args)? {
        return Ok(None);
    }
    let Some(host) = host() else { return Ok(None) };
    let mut command = Command::new(host);
    command.args(["pkg", "update", "claude", "--wait-lock", "1800"]);
    Ok(Some(command))
}

fn update_action(args: &[OsString]) -> Result<bool, String> {
    let Some(verb) = args
        .first()
        .and_then(|word| word.to_str())
        .filter(|word| UPDATE_WORDS.contains(word))
    else {
        return Ok(false);
    };
    let help = args
        .iter()
        .take_while(|word| *word != "--")
        .any(|word| word == "-h" || word == "--help");
    let mut words = args[1..]
        .iter()
        .filter(|word| !help || (*word != "-h" && *word != "--help"));
    let valid = if verb == "install" {
        let (mut latest, mut force) = (false, false);
        words.all(|word| {
            if word == "latest" && !latest {
                latest = true;
                true
            } else if word == "--force" && !force {
                force = true;
                true
            } else {
                false
            }
        })
    } else {
        words.next().is_none()
    };
    if valid {
        Ok(!help)
    } else {
        Err(format!(
            "`claude {verb}` is managed by atpkg; expected `{verb}`{}; no channel or version target was installed",
            if verb == "install" {
                " with optional `latest` and `--force`"
            } else {
                " with no arguments"
            }
        ))
    }
}

/// Ask the host for the usage settings once at launch, after permission defaults.
/// Explicit launch settings remain after this argument as in the former shim.
/// A settled failure, malformed UTF-8, excess output or the two-second query deadline
/// leaves the arguments intact. Cancellation has a separate two-second settlement
/// grace; it never extends query admission or retries a lost native wait receipt.
/// # Errors
/// Native child or signal-mask ownership remains unsettled. The caller must retain
/// the returned resources through its actual executable failure boundary.
pub fn usage_tap(args: &mut Vec<OsString>) -> Result<(), crate::sys::ProbeSpawnRefusal> {
    if !enabled() {
        return Ok(());
    }
    let Some(host) = host() else { return Ok(()) };
    let words: Vec<_> = ["harness", "tap", "--launch", "--"]
        .into_iter()
        .map(OsString::from)
        .chain(args.iter().cloned())
        .collect();
    if let Some(value) = bounded_output(&host, &words, Duration::from_secs(2))? {
        let value = value.trim_end_matches('\n');
        if !value.is_empty() && !value.contains('\0') {
            args.splice(..0, [OsString::from("--settings"), OsString::from(value)]);
        }
    }
    Ok(())
}

fn settle_tap(mut child: crate::sys::ProbeChild) -> Result<(), crate::sys::ProbeSpawnRefusal> {
    let until = Instant::now() + Duration::from_secs(2);
    let result = (|| {
        if !child.pending() {
            return Ok(());
        }
        child.cancel()?;
        while child.pending() {
            let now = Instant::now();
            if now >= until {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "usage helper settlement deadline",
                ));
            }
            child.try_wait()?;
            if child.pending() {
                std::thread::sleep((until - now).min(Duration::from_millis(10)));
            }
        }
        if Instant::now() >= until {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "usage helper settlement completed after its deadline",
            ));
        }
        Ok(())
    })();
    result.map_err(|error| crate::sys::ProbeSpawnRefusal {
        error,
        child: Some(Box::new(child)),
        signals: None,
    })
}

fn bounded_output(
    executable: &Path,
    args: &[OsString],
    timeout: Duration,
) -> Result<Option<String>, crate::sys::ProbeSpawnRefusal> {
    let mut child = match crate::sys::ProbeChild::spawn(executable, args, &[]) {
        Ok(child) => child,
        Err(mut refusal) => {
            if let Some(mask) = refusal.signals.as_mut() {
                if let Err(error) = mask.restore() {
                    refusal.error = error;
                    return Err(refusal);
                }
                refusal.signals = None;
            }
            if let Some(child) = refusal.child.take() {
                settle_tap(*child)?;
            }
            return Ok(None);
        }
    };
    let result = (|| {
        let deadline = Instant::now() + timeout;
        let mut output = Vec::new();
        let mut errors = Vec::new();
        let mut exited = None;
        loop {
            if Instant::now() >= deadline {
                return None;
            }
            child
                .drain_total(&mut output, &mut errors, 64 * 1024)
                .ok()?;
            if Instant::now() >= deadline {
                return None;
            }
            if exited.is_none()
                && let Some(status) = child.try_wait().ok()?
            {
                if !status.success() {
                    return None;
                }
                exited = Some(status);
            }
            if exited.is_some() && child.output_finished() {
                return (Instant::now() < deadline)
                    .then(|| String::from_utf8(output).ok())
                    .flatten();
            }
            let remaining = deadline.checked_duration_since(Instant::now())?;
            let mut polls: Vec<_> = child
                .output_fds()
                .map(|fd| libc::pollfd {
                    fd,
                    events: libc::POLLIN,
                    revents: 0,
                })
                .collect();
            let wait = if polls.is_empty() {
                remaining.min(Duration::from_millis(10))
            } else {
                remaining
            };
            let millis = i32::try_from(wait.as_millis().saturating_add(1)).ok()?;
            // SAFETY: every descriptor borrows this owned probe, with one bounded native wait.
            let polled =
                unsafe { libc::poll(polls.as_mut_ptr(), polls.len() as libc::nfds_t, millis) };
            if polled < 0 && io::Error::last_os_error().kind() != io::ErrorKind::Interrupted
                || polls.iter().any(|poll| poll.revents & libc::POLLNVAL != 0)
            {
                return None;
            }
        }
    })();
    settle_tap(child)?;
    Ok(result)
}

/// Construct the vendor launch, carrying account-token routing through a host
/// exec that preserves the child's pid. No token is returned to the harness.
/// Bypass launches call this with `policy=false` and contact no host service.
pub fn vendor_command(exe: &Path, args: &[OsString], policy: bool) -> Command {
    let host = policy.then(host).flatten().filter(|_| enabled());
    vendor_command_for(exe, args, host.as_deref())
}

fn vendor_command_for(exe: &Path, args: &[OsString], host: Option<&Path>) -> Command {
    let mut command = match host {
        Some(host) => {
            let mut command = Command::new(host);
            command
                .args(["harness", "accounts", "token-run", "--auto", "--"])
                .arg(exe);
            command
        }
        None => Command::new(exe),
    };
    command.args(args);
    command
}

/// Apply Claude's fullscreen renderer only when neither environment nor the
/// user's settings name a renderer. Settings reads are bounded and use JSON,
/// so a `tui` word inside another setting cannot accidentally disable the default.
pub fn renderer(command: &mut Command) {
    if !enabled() {
        return;
    }
    let config = renderer_config(std::env::var_os("CLAUDE_CONFIG_DIR"), || {
        crate::store::home_dir(std::env::var_os("HOME"))
    });
    let settings = config.and_then(|config| {
        crate::store::read_bounded(&config.join("settings.json"), 1024 * 1024, true)
    });
    if renderer_default(
        std::env::var_os("CLAUDE_CODE_NO_FLICKER").as_deref(),
        std::env::var_os("CLAUDE_CODE_DISABLE_ALTERNATE_SCREEN").as_deref(),
        settings.as_deref(),
    ) {
        command.env("CLAUDE_CODE_NO_FLICKER", "1");
    }
}

fn renderer_config(
    configured: Option<OsString>,
    home: impl FnOnce() -> Option<PathBuf>,
) -> Option<PathBuf> {
    configured
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or_else(|| home().map(|home| home.join(".claude")))
}

fn renderer_default(
    flicker: Option<&OsStr>,
    alternate: Option<&OsStr>,
    settings: Option<&str>,
) -> bool {
    flicker.is_none()
        && alternate.is_none()
        && settings
            .and_then(crate::json::parse)
            .is_none_or(|settings| settings.get("tui").is_none())
}

#[cfg(test)]
#[path = "launch_tests.rs"]
mod tests;
