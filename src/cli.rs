// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The command line: the harness's own options, `--version`, and the hand-off
//! to [`crate::supervise`].
//!
//! Direct launches pass arguments through; managed launches apply [`crate::launch`].
//! The parser consumes:
//! * leading `--alab-<name>[=<value>]` words, which are the harness's own and
//!   are consumed (Claude Code has no `--alab-` flag; an unknown one is refused,
//!   never passed on);
//! * `run --` or `launch --` as the first two words after them: managed
//!   permission, bypass, update and usage policy is applied. `launch --`
//!   always execs the vendor in place, leaving live duties to the host;
//! * a lone `--version` or `-v` outside that envelope, which prints
//!   `claude-code-alab <version> (claude <vendor version>)` from the store's
//!   records and the shared origin line, never by running the vendor.
//!
//! Two kinds of launch run the vendor alone, exec'd in the harness's place
//! ([`pass_through`]: the same pid, no supervision claim):
//! * inside aterm, `ATERM_NO_HARNESS=1` leaves the harness out ([`left_out`]);
//! * a launch the harness could never move onto another build
//!   ([`crate::supervise::live_update_refusal`]: `--alab-no-live-update`, no
//!   terminal on stdin, a one-shot such as `-p` or `--help`, a command line
//!   that cannot be resumed). The supervisor exists only for a session it can
//!   move; in front of any other it would be one more process between the
//!   terminal and the vendor, and inside aterm it would keep aterm from
//!   proving that the tab's foreground leader is the vendor.

use std::ffi::{OsStr, OsString};
use std::io::Write as _;

use alab_policy::ORIGIN_LINE;

use crate::journal::Journal;
use crate::profile::{COMMAND, PACKAGE, REPO, TOOL};
use crate::quiet::QUIET_MS;
use crate::store::{self, Store};
use crate::supervise::TERM_GRACE_MS;

/// The harness version: the workspace's release line, with the publisher's build stamp.
pub const VERSION: &str = match option_env!("ALAB_HARNESS_VERSION") {
    Some(version) => version,
    None => env!("CARGO_PKG_VERSION"),
};

const _: () = assert!(
    version_matches_release(VERSION, env!("CARGO_PKG_VERSION")),
    "ALAB_HARNESS_VERSION must be this package's MAJOR.MINOR.0, optionally +g and 9..40 lowercase hex digits"
);

const fn release_component(bytes: &[u8]) -> Option<&[u8]> {
    let (first, mut rest) = match bytes.split_first() {
        Some((&first, rest)) if first.is_ascii_digit() => (first, rest),
        _ => return None,
    };
    while let Some((&digit, tail)) = rest.split_first() {
        if digit == b'.' {
            return Some(tail);
        }
        if first == b'0' || !digit.is_ascii_digit() {
            return None;
        }
        rest = tail;
    }
    None
}

const fn release_base(bytes: &[u8]) -> bool {
    let Some(after_major) = release_component(bytes) else {
        return false;
    };
    let Some(after_minor) = release_component(after_major) else {
        return false;
    };
    matches!(after_minor, [b'0'])
}

const fn version_matches_release(value: &str, release: &str) -> bool {
    let mut base = release.as_bytes();
    if !release_base(base) {
        return false;
    }
    let mut rest = value.as_bytes();
    while let Some((&expected, tail)) = base.split_first() {
        rest = match rest.split_first() {
            Some((&actual, tail)) if actual == expected => tail,
            _ => return false,
        };
        base = tail;
    }
    if rest.is_empty() {
        return true;
    }
    rest = match rest {
        [b'+', b'g', stamp @ ..] => stamp,
        _ => return false,
    };
    if rest.len() < 9 || rest.len() > 40 {
        return false;
    }
    while let Some((&digit, tail)) = rest.split_first() {
        if !(digit.is_ascii_digit() || matches!(digit, b'a'..=b'f')) {
            return false;
        }
        rest = tail;
    }
    true
}

/// The longest quiet period `--alab-quiet-ms` accepts: one day.
pub const QUIET_MS_MAX: u64 = 24 * 3600 * 1000;

/// How often the harness looks at the store and the child, by default.
pub const POLL_MS: u64 = 250;

/// Exit status when the vendor CLI cannot be found or run (the shell's "not found").
pub const EXIT_VENDOR_MISSING: i32 = 127;

/// Exit status for a harness usage error.
pub const EXIT_USAGE: i32 = 2;

/// Vendor command words this build is qualified to lead through its managed
/// entry. The package manifest must describe this same source declaration.
/// Native qualification is still in progress, so no word is led yet.
pub const LEADING_WORDS: &[&str] = &[];

/// Vendor words whose launch policy this build owns before execing the vendor
/// in place. This grants no lifecycle or terminal supervision capability.
pub const LAUNCH_WORDS: &[&str] = &[COMMAND];

/// A metadata-only capability request. It must be the complete command line;
/// unsupported words and malformed requests refuse without opening any state.
fn capability_query(words: &[OsString]) -> Option<i32> {
    let first = words.first()?;
    for (query, supported) in [
        ("--alab-leads", LEADING_WORDS),
        ("--alab-launches", LAUNCH_WORDS),
        ("--alab-primes", crate::prime::CAPABILITIES),
        ("--alab-inspects", crate::inspect::CAPABILITIES),
    ] {
        let bytes = first.as_encoded_bytes();
        if bytes != query.as_bytes()
            && !bytes
                .strip_prefix(query.as_bytes())
                .is_some_and(|suffix| suffix.starts_with(b"="))
        {
            continue;
        }
        return Some(
            if words.len() == 1
                && first
                    .to_str()
                    .and_then(|first| first.strip_prefix(query)?.strip_prefix('='))
                    .is_some_and(|word| supported.contains(&word))
            {
                0
            } else {
                1
            },
        );
    }
    None
}

/// The variable a harness sets in its vendor child's environment to its own pid.
/// A harness that finds it naming its own parent was started *as the vendor* by
/// a harness (the store's binary is a copy of this harness, which the
/// same-file check cannot see) and refuses at once, so a harness can never
/// recurse into itself.
pub const PARENT_VAR: &str = "CLAUDE_ALAB_PID";

/// Whether `marker` (the value of [`PARENT_VAR`]) names `parent`, this
/// process's parent: this process was started as a harness's vendor child.
#[must_use]
pub fn started_as_vendor(marker: Option<&OsStr>, parent: u32) -> bool {
    marker
        .and_then(|m| m.to_str())
        .and_then(|m| m.parse::<u32>().ok())
        .is_some_and(|pid| pid == parent)
}

/// The variable a harness sets in every vendor child's environment to its own
/// nesting depth plus one. [`PARENT_VAR`] catches a copy of the harness that
/// the harness itself starts; this catches the harness reached again through
/// anything in between (a wrapper script installed as the vendor, which runs
/// `claude-alab` without `exec`), which the parent check cannot see.
pub const DEPTH_VAR: &str = "CLAUDE_ALAB_DEPTH";

/// The most harnesses that may run nested inside one another (a harness started
/// from a session's own shell is one level deeper). Past it, a harness refuses
/// at once, so no chain of wrappers can recurse without bound.
pub const MAX_DEPTH: u32 = 8;

/// This harness's nesting depth from the inherited [`DEPTH_VAR`]: 0 when
/// unset; refused at [`MAX_DEPTH`] or past it, and when the value is not a
/// number (an unreadable depth is never read as "not nested").
///
/// # Errors
/// Why this harness must not run.
pub fn nesting_depth(inherited: Option<&OsStr>) -> Result<u32, String> {
    let Some(value) = inherited else { return Ok(0) };
    let depth = value
        .to_str()
        .and_then(|v| v.parse::<u32>().ok())
        .ok_or_else(|| {
            format!(
                "{DEPTH_VAR}={} is not a nesting depth",
                value.to_string_lossy()
            )
        })?;
    if depth >= MAX_DEPTH {
        return Err(format!(
            "{DEPTH_VAR}={depth}: {TOOL} is already nested {depth} deep (at most {MAX_DEPTH}); \
             the atpkg store's `{COMMAND}` probably runs {TOOL} again"
        ));
    }
    Ok(depth)
}

/// The entry word of atpkg's managed launch. The `agents/` twin of `claude`
/// hands a session to this harness as `claude-alab run -- <the launch's
/// arguments>` (aterm `crates/atpkg/src/platform/mod.rs`, `sh_harness_prelude`),
/// as it does for the sibling harnesses.
pub const ENTRY_WORD: &str = "run";

/// Managed launch preparation followed by same-PID vendor exec, without a
/// supervisor, claim, signal handler or terminal-mode change.
pub const LAUNCH_ENTRY_WORD: &str = "launch";

/// The variable that leaves the harness out of a launch: the twin sets it to
/// `1` for `--no-harness` and `--no-alab-harness` and then runs the vendor
/// alone, and a person may set it themselves.
pub const NO_HARNESS_VAR: &str = "ATERM_NO_HARNESS";

/// Whether this launch leaves the harness out: `value` (of [`NO_HARNESS_VAR`])
/// is exactly `1`, the one value the twin's hand-off reads, and the launch is
/// inside aterm (`in_aterm`). Outside aterm the variable is not read, as in the
/// sibling harnesses.
#[must_use]
pub fn left_out(value: Option<&OsStr>, in_aterm: bool) -> bool {
    in_aterm && value.is_some_and(|v| v == "1")
}

/// The two variables every process this harness starts as the vendor carries:
/// [`PARENT_VAR`] (this harness's pid) and [`DEPTH_VAR`] (`depth + 1`).
#[must_use]
pub fn child_marks(depth: u32) -> [(&'static str, String); 2] {
    [
        (PARENT_VAR, std::process::id().to_string()),
        (DEPTH_VAR, depth.saturating_add(1).to_string()),
    ]
}

/// Write one line to stderr; a closed or broken stderr is ignored (`eprintln!`
/// would panic, and a panic mid-relaunch would lose the session).
pub fn say(line: &str) {
    let _ = writeln!(std::io::stderr(), "{line}");
}

/// Journal a launch that was refused before supervision began, as
/// `refused <reason>`: the journal `run` would have written to
/// (`$XDG_STATE_HOME` when absolute, else under the home, [`Journal::new`]),
/// which exists only once a launch is accepted. With neither place there is
/// nowhere to write and the refusal stays on stderr alone. Journalling never
/// changes what is said or the exit status.
pub fn journal_refusal(reason: &str) {
    Journal::new(
        std::env::var_os("XDG_STATE_HOME")
            .map(std::path::PathBuf::from)
            .as_deref(),
        store::home_dir(std::env::var_os("HOME")).as_deref(),
        std::process::id(),
    )
    .note(&format!("refused {reason}"));
}

/// Write `text` to stdout; a closed or broken stdout is ignored.
fn put(text: &str) {
    let mut out = std::io::stdout();
    let _ = out.write_all(text.as_bytes());
    let _ = out.flush();
}

/// The supervisor's settings.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Options {
    /// The quiet period of the safe point ([`QUIET_MS`]).
    pub quiet_ms: u64,
    /// How often the harness looks ([`POLL_MS`]).
    pub poll_ms: u64,
    /// How long after its SIGTERM the harness waits for the child to end before
    /// it abandons that update ([`TERM_GRACE_MS`]).
    pub term_grace_ms: u64,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            quiet_ms: QUIET_MS,
            poll_ms: POLL_MS,
            term_grace_ms: TERM_GRACE_MS,
        }
    }
}

/// What the command line asks for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Command {
    /// Run the vendor.
    Run {
        /// The supervisor's settings, should the launch be supervised.
        opts: Options,
        /// Whether the session may be moved onto another build: `false` after
        /// `--alab-no-live-update`, which runs the vendor alone
        /// ([`crate::supervise::live_update_refusal`]).
        live_update: bool,
        /// The vendor's arguments, unchanged.
        args: Vec<OsString>,
    },
    /// Print the version headline and shared origin line.
    Version,
    /// Print the harness's own usage.
    Help,
}

/// The harness's usage text.
#[must_use]
pub fn usage() -> String {
    format!(
        "{TOOL} {VERSION}: runs the vendor `{COMMAND}` from the atpkg store as its child.\n\
         {ORIGIN_LINE}\n\
         \n\
         usage: {TOOL} [--alab-<option>…] [{COMMAND} arguments…]\n\
         \x20      {TOOL} [--alab-<option>…] {ENTRY_WORD} -- [{COMMAND} arguments…]\n\
         \x20      {TOOL} [--alab-<option>…] {LAUNCH_ENTRY_WORD} -- [{COMMAND} arguments…]\n\
         \x20      {TOOL} prime <settings-defaults|legacy-hooks> --home <absolute-path> --config <absolute-path>\n\
         \x20      {TOOL} inspect settings-defaults --home <absolute-path> --config <absolute-path> --auto-prime=0|1\n\
         \n\
         Direct launches pass arguments after the leading --alab- options to `{COMMAND}`.\n\
         `{ENTRY_WORD} --` and `{LAUNCH_ENTRY_WORD} --` apply managed permission, usage and update\n\
         defaults. `{LAUNCH_ENTRY_WORD} --` then execs `{COMMAND}` in place, without supervision.\n\
         --no-harness skips supervision, usage and account helpers; --yolo selects bypass.\n\
         `{TOOL} --version` prints the harness and vendor versions and the origin line.\n\
         \n\
         harness options:\n\
         \x20 --alab-no-live-update   never move this session: run `{COMMAND}` alone, in this\n\
         \x20                         program's place\n\
         \x20 --alab-quiet-ms=<ms>    quiet period of the safe point\n\
         \x20                         (default {QUIET_MS}, 100..={QUIET_MS_MAX})\n\
         \x20 --alab-poll-ms=<ms>     how often the store and the child are looked at\n\
         \x20                         (default {POLL_MS}, 20..=5000)\n\
         \x20 --alab-term-grace-ms=<ms>  how long an update waits for {COMMAND} to end after\n\
         \x20                         SIGTERM before it is abandoned (default {TERM_GRACE_MS}, 500..=600000)\n\
         \x20 --alab-help             this text\n\
         \n\
         Inside aterm, {NO_HARNESS_VAR}=1 runs `{COMMAND}` alone, in this program's place.\n\
         So does a launch that can never be moved onto a newer build: no terminal on\n\
         stdin, a one-shot run (-p, --help, --version beside other words), or arguments\n\
         that cannot be resumed (--worktree, an unknown flag).\n"
    )
}

/// Parse the arguments after `argv[0]`.
///
/// The harness's own words lead: `--alab-` options, then, optionally, atpkg's
/// envelope `run --` or `launch --`. The latter always execs the vendor in
/// place. After either envelope nothing is the
/// harness's: an `--alab-` word and a lone `--version` there are the vendor's.
/// Either entry word with any other word after it, or none, is not the envelope and goes to
/// the vendor like every other first vendor word (docs/DECISIONS.md, 37).
///
/// # Errors
/// A malformed or unknown `--alab-` option.
pub fn parse(args: Vec<OsString>) -> Result<Command, String> {
    let mut opts = Options::default();
    let mut live_update = true;
    let mut rest = args.into_iter().peekable();
    while let Some(word) = rest.peek() {
        if !word.as_encoded_bytes().starts_with(b"--alab-") {
            break;
        }
        let text = word
            .to_str()
            .ok_or_else(|| "harness option is not UTF-8".to_string())?;
        let Some(option) = text.strip_prefix("--alab-") else {
            break;
        };
        let (name, value) = match option.split_once('=') {
            Some((n, v)) => (n, Some(v)),
            None => (option, None),
        };
        let number = |lo: u64, hi: u64| -> Result<u64, String> {
            let v = value.ok_or_else(|| format!("--alab-{name} needs =<ms>"))?;
            v.parse::<u64>()
                .ok()
                .filter(|n| (lo..=hi).contains(n))
                .ok_or_else(|| format!("--alab-{name}={v}: expected a number in {lo}..={hi}"))
        };
        match (name, value) {
            ("help", None) => return Ok(Command::Help),
            ("no-live-update", None) => live_update = false,
            ("quiet-ms", _) => opts.quiet_ms = number(100, QUIET_MS_MAX)?,
            ("poll-ms", _) => opts.poll_ms = number(20, 5000)?,
            ("term-grace-ms", _) => opts.term_grace_ms = number(500, 600_000)?,
            _ => return Err(format!("unknown harness option `{text}`")),
        }
        rest.next();
    }
    let mut args: Vec<OsString> = rest.collect();
    if crate::launch::managed(&args) {
        if args[0] == LAUNCH_ENTRY_WORD {
            live_update = false;
        }
        args.drain(..2);
    } else if args.len() == 1 && (args[0] == "--version" || args[0] == "-v") {
        return Ok(Command::Version);
    }
    Ok(Command::Run {
        opts,
        live_update,
        args,
    })
}

/// What the version line says when atpkg has no `current` build of the vendor.
pub const NOT_INSTALLED: &str = "not installed";

/// What the version line says when the vendor's version cannot be read: no home to find the
/// store under, a `current` that names no usable build, or a `.vendor` record that is missing,
/// unreadable or names another version than its build directory.
pub const UNKNOWN: &str = "unknown";

/// The version text: a `claude-code-alab <version> (claude <vendor version>)`
/// headline followed by the shared origin line. `<version>` comes from
/// `[workspace.package] version`, and the vendor version from the
/// `.vendor` record of the build `current` names, where it names that very build
/// ([`store::Build::version`]). Otherwise the parenthesis reads
/// `(claude not installed)` (no `current` link) or `(claude unknown)` (every
/// other reason, and `current` is `None` when there is no home to find the store
/// under). The vendor is never run for it, and a version is never decoded from a
/// build number alone.
#[must_use]
pub fn version_line(current: Option<&Result<store::Build, store::Missing>>) -> String {
    let vendor = match current {
        Some(Ok(b)) => b.version.as_deref().unwrap_or(UNKNOWN),
        Some(Err(store::Missing::NotInstalled(_))) => NOT_INSTALLED,
        Some(Err(_)) | None => UNKNOWN,
    };
    format!("{REPO} {VERSION} ({COMMAND} {vendor})\n{ORIGIN_LINE}")
}

/// What the journal calls a launch that leaves the harness out ([`left_out`]).
pub const LEFT_OUT_REASON: &str = "no-harness";

/// Run the vendor in this process's place (`execve`: the same pid, so nothing
/// is left behind when the launcher is killed), for `reason`: the launch leaves
/// the harness out ([`LEFT_OUT_REASON`]), or it could never be moved
/// ([`crate::supervise::live_update_refusal`]'s words). The journal gets one
/// line, `pass <reason> vendor-build=<n> vendor=<version>`. The build's shim
/// environment and the harness's forced variables apply as on a supervised
/// start. [`PARENT_VAR`] is removed, since the vendor is not this harness's
/// child, and [`DEPTH_VAR`] still counts, so a store entry that leads back to
/// this harness stops at [`MAX_DEPTH`]. No supervision claim or signal handling
/// is installed. Managed launches can still use the host's account service,
/// unless `reason` is [`LEFT_OUT_REASON`]. Returns only by exiting: 127 when the vendor cannot
/// be run.
fn pass_through(
    home: &std::path::Path,
    build: &store::Build,
    args: &[OsString],
    depth: u32,
    reason: &str,
) -> ! {
    use std::os::unix::process::CommandExt as _;
    let journal = Journal::new(
        std::env::var_os("XDG_STATE_HOME")
            .map(std::path::PathBuf::from)
            .as_deref(),
        Some(home),
        std::process::id(),
    );
    journal.note(&format!(
        "pass {reason} vendor-build={} vendor={}",
        build.number,
        build.version.as_deref().unwrap_or("-")
    ));
    let policy = reason != LEFT_OUT_REASON && !crate::launch::informational(args);
    let mut command = crate::launch::vendor_command(&build.exe, args, policy);
    command
        .envs(build.env.iter().map(|(k, v)| (k, v)))
        .envs(crate::profile::HARNESS_ENV.iter().copied())
        .env_remove(PARENT_VAR)
        .env(DEPTH_VAR, depth.saturating_add(1).to_string());
    if policy {
        crate::launch::renderer(&mut command);
    } else if reason == LEFT_OUT_REASON {
        command.env(NO_HARNESS_VAR, "1");
    }
    let error = command.exec();
    say(&format!(
        "{TOOL}: cannot run {}: {error}",
        build.exe.display()
    ));
    journal.note(&format!("refused spawn-failed {error}"));
    std::process::exit(EXIT_VENDOR_MISSING);
}

/// The program: parse, resolve the vendor from the store, then run it alone
/// ([`pass_through`]) or supervise it.
/// # Errors
/// Launch preparation retains unsettled native resources. Only the actual executable
/// may terminate while holding this error; it supplies no child-end witness.
pub fn main() -> Result<std::convert::Infallible, crate::sys::ProbeSpawnRefusal> {
    main_with_image_host(crate::supervise::image::default_host)
}

/// Run the CLI with one explicit authority factory for fresh starts and inherited
/// image transfers. The caller selects the factory in Rust before any threads or
/// signal handlers start; command-line arguments and environment cannot select it.
/// # Errors
/// As [`main`], retained native launch preparation cannot be settled safely.
#[doc(hidden)]
pub fn main_with_image_host(
    factory: crate::supervise::image::HostFactory,
) -> Result<std::convert::Infallible, crate::sys::ProbeSpawnRefusal> {
    use std::io::IsTerminal as _;
    let original_stdout_terminal = std::io::stdout().is_terminal();
    let words: Vec<_> = std::env::args_os().skip(1).collect();
    if let Some(status) = capability_query(&words) {
        std::process::exit(status);
    }
    crate::supervise::image::early_entry(factory);
    if started_as_vendor(
        std::env::var_os(PARENT_VAR).as_deref(),
        std::os::unix::process::parent_id(),
    ) {
        say(&format!(
            "{TOOL}: refusing to run: started as the vendor `{COMMAND}` by {TOOL} {} \
             (the atpkg store's `{COMMAND}` is a copy of this harness)",
            std::os::unix::process::parent_id()
        ));
        journal_refusal(&format!(
            "started-as-vendor parent={}",
            std::os::unix::process::parent_id()
        ));
        std::process::exit(EXIT_VENDOR_MISSING);
    }
    let depth = match nesting_depth(std::env::var_os(DEPTH_VAR).as_deref()) {
        Ok(d) => d,
        Err(why) => {
            say(&format!("{TOOL}: refusing to run: {why}"));
            journal_refusal(&format!("nesting-depth {why}"));
            std::process::exit(EXIT_VENDOR_MISSING);
        }
    };
    if let Some(result) = crate::folder_trust::entry(&words) {
        let request = match result {
            Ok(request) => request,
            Err(why) => {
                say(&format!("{TOOL}: {why}"));
                std::process::exit(EXIT_USAGE)
            }
        };
        let outcome = request.run()?;
        if let crate::folder_trust::Outcome::Interrupted { signal } = outcome {
            std::process::exit(128 + signal);
        }
        let response = crate::folder_trust::response(&outcome);
        let written = std::io::stdout().lock().write_all(response.as_bytes());
        if written.is_err() {
            std::process::exit(EXIT_USAGE)
        }
        std::process::exit(0);
    }
    if let Some(result) = crate::prime::entry(&words) {
        match result.and_then(|effect| crate::prime::response(&effect)) {
            Ok(row) => {
                put(&row);
                std::process::exit(0);
            }
            Err(error) => {
                say(&format!("{TOOL}: priming unavailable: {error}"));
                std::process::exit(EXIT_USAGE);
            }
        }
    }
    if let Some(result) = crate::inspect::entry(&words) {
        match result.and_then(|report| report.response()) {
            Ok(document) => {
                let written = {
                    let mut out = std::io::stdout().lock();
                    out.write_all(document.as_bytes())
                        .and_then(|()| out.flush())
                };
                if let Err(error) = written {
                    say(&format!(
                        "{TOOL}: cannot write inspection response: {error}"
                    ));
                    std::process::exit(EXIT_USAGE);
                }
                std::process::exit(0);
            }
            Err(error) => {
                say(&format!("{TOOL}: inspection unavailable: {error}"));
                std::process::exit(EXIT_USAGE);
            }
        }
    }
    let managed = crate::launch::managed(&words);
    let command = match parse(words) {
        Ok(c) => c,
        Err(why) => {
            say(&format!("{TOOL}: {why}\n\n{}", usage()));
            journal_refusal(&format!("usage {why}"));
            std::process::exit(EXIT_USAGE);
        }
    };
    let (opts, live_update, args) = match command {
        Command::Help => {
            put(&usage());
            std::process::exit(0);
        }
        Command::Version => {
            // The store is read and the vendor never run: the version is the current
            // build's `.vendor` record, or a word saying why there is none. The
            // harness's own version is known even with no home to find the store under.
            let own_exe = std::env::current_exe().ok();
            let current = store::home_dir(std::env::var_os("HOME")).map(|home| {
                Store::new(&store::default_prefix(&home), PACKAGE, COMMAND)
                    .current(own_exe.as_deref())
            });
            put(&format!("{}\n", version_line(current.as_ref())));
            std::process::exit(0);
        }
        Command::Run {
            opts,
            live_update,
            args,
        } => (opts, live_update, args),
    };
    let (mut args, disabled) = if managed {
        crate::launch::switches(args)
    } else {
        (args, false)
    };
    let disabled = disabled
        || left_out(
            std::env::var_os(NO_HARNESS_VAR).as_deref(),
            crate::supervise::image::hosted_by_aterm(),
        );
    let informational = crate::launch::informational(&args);
    if managed && !informational {
        let update = match crate::launch::self_update(&args) {
            Ok(update) => update,
            Err(why) => {
                say(&format!("{TOOL}: {why}"));
                journal_refusal(&format!("self-update {why}"));
                std::process::exit(EXIT_USAGE);
            }
        };
        if let Some(mut update) = update {
            use std::os::unix::process::CommandExt as _;
            let error = update.exec();
            say(&format!("{TOOL}: cannot update claude: {error}"));
            std::process::exit(EXIT_VENDOR_MISSING);
        }
        args = crate::launch::permission_args(args);
    }
    let home = store::home_dir(std::env::var_os("HOME"));
    let own_exe = std::env::current_exe().ok();
    let Some(home) = home else {
        // The one launch refusal that is not journalled: with no home, the
        // journal an accepted launch would write (under the home) has no place.
        say(&format!(
            "{TOOL}: cannot find your home directory, so cannot find the atpkg store"
        ));
        std::process::exit(EXIT_VENDOR_MISSING);
    };
    let store = Store::new(&store::default_prefix(&home), PACKAGE, COMMAND);
    let build = match store.current(own_exe.as_deref()) {
        Ok(b) => b,
        Err(missing) => {
            say(&format!("{TOOL}: {missing}"));
            journal_refusal(&format!("{} {missing}", missing.word()));
            std::process::exit(EXIT_VENDOR_MISSING);
        }
    };
    if disabled {
        pass_through(&home, &build, &args, depth, LEFT_OUT_REASON);
    }
    if managed && !informational {
        let trust = crate::folder_trust::settle(&build, &args, original_stdout_terminal)?;
        if !matches!(&trust, crate::folder_trust::Outcome::Skipped(_)) {
            Journal::new(
                std::env::var_os("XDG_STATE_HOME")
                    .map(std::path::PathBuf::from)
                    .as_deref(),
                Some(&home),
                std::process::id(),
            )
            .note(&trust.description());
        }
        if let crate::folder_trust::Outcome::Interrupted { signal } = trust {
            std::process::exit(128 + signal);
        }
        crate::launch::usage_tap(&mut args)?;
    }
    // Launch preparation has restored the caller's signal policy. Only a
    // resumable terminal session proceeds to resident supervision below.
    if let Some(reason) =
        crate::supervise::live_update_refusal(live_update, crate::sys::stdin_is_terminal(), &args)
    {
        pass_through(&home, &build, &args, depth, &reason);
    }
    crate::supervise::run_with_image_host(
        &crate::supervise::Setup {
            home,
            store,
            own_exe,
            build,
            opts,
            args,
            depth,
        },
        factory,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn os(words: &[&str]) -> Vec<OsString> {
        words.iter().map(OsString::from).collect()
    }

    /// A launch of the vendor with `words` that may be moved.
    fn run(opts: Options, words: &[&str]) -> Result<Command, String> {
        Ok(Command::Run {
            opts,
            live_update: true,
            args: os(words),
        })
    }

    /// The same launch after `--alab-no-live-update`.
    fn never_moved(opts: Options, words: &[&str]) -> Result<Command, String> {
        Ok(Command::Run {
            opts,
            live_update: false,
            args: os(words),
        })
    }

    #[test]
    fn every_vendor_argument_passes_through() {
        // After the first vendor word an `--alab-` word is the vendor's, and
        // changes nothing about the launch.
        let args = ["--model", "opus", "--alab-no-live-update", "-p", "hi"];
        assert_eq!(parse(os(&args)), run(Options::default(), &args));
        assert_eq!(parse(Vec::new()), run(Options::default(), &[]));
        use std::os::unix::ffi::OsStringExt as _;
        let owned = OsString::from_vec(b"--alab-poll-ms=\xff".to_vec());
        for words in [
            vec![OsString::from_vec(vec![0xff])],
            vec![OsString::from("task"), owned.clone()],
            vec![OsString::from_vec(vec![0xff]), owned.clone()],
            vec![OsString::from("--"), owned],
        ] {
            assert_eq!(
                parse(words.clone()),
                Ok(Command::Run {
                    opts: Options::default(),
                    live_update: true,
                    args: words,
                })
            );
        }
    }

    #[test]
    fn launch_envelope_prepares_but_never_supervises_the_vendor() {
        use std::os::unix::ffi::OsStringExt as _;
        let opaque = OsString::from_vec(b"--alab-poll-ms=\xff".to_vec());
        assert_eq!(
            parse(vec!["launch".into(), "--".into(), opaque.clone()]),
            Ok(Command::Run {
                opts: Options::default(),
                live_update: false,
                args: vec![opaque],
            })
        );
        for vendor in [
            &[][..],
            &["--model", "opus", "a task"],
            &["--version"],
            &["--alab-no-live-update", "run", "--", ""],
        ] {
            let mut args = vec!["launch", "--"];
            args.extend_from_slice(vendor);
            assert_eq!(parse(os(&args)), never_moved(Options::default(), vendor));
        }
        assert_eq!(
            parse(os(&["--alab-poll-ms=50", "launch", "--", "task"])),
            never_moved(
                Options {
                    poll_ms: 50,
                    ..Options::default()
                },
                &["task"]
            )
        );
        for words in [
            &["launch"][..],
            &["launch", "a task"],
            &["--", "launch", "--"],
        ] {
            assert_eq!(parse(os(words)), run(Options::default(), words));
        }
    }

    #[test]
    fn launch_capability_grants_no_leading_word_and_requires_exact_metadata_argv() {
        for (words, status) in [
            (&["--alab-launches=claude"][..], 0),
            (&["--alab-primes=claude:settings-defaults"][..], 0),
            (&["--alab-primes=claude:legacy-hooks"][..], 0),
            (&["--alab-primes=muse:legacy-hooks"][..], 1),
            (&["--alab-primes=claude"][..], 1),
            (&["--alab-primes=muse:settings-defaults"][..], 1),
            (&["--alab-primes=claude:settings-defaults", "prime"][..], 1),
            (&["--alab-leads=claude"][..], 1),
            (&["--alab-launches=muse"][..], 1),
            (&["--alab-launches=unknown"][..], 1),
            (&["--alab-launches="][..], 1),
            (&["--alab-launches"][..], 1),
            (&["--alab-launches=claude", "launch", "--"][..], 1),
        ] {
            assert_eq!(capability_query(&os(words)), Some(status));
        }
        use std::os::unix::ffi::OsStringExt as _;
        assert_eq!(
            capability_query(&[OsString::from_vec(b"--alab-launches=\xff".to_vec())]),
            Some(1)
        );
    }

    #[test]
    fn leading_harness_options_are_consumed() {
        let got = parse(os(&[
            "--alab-no-live-update",
            "--alab-quiet-ms=500",
            "--alab-poll-ms=50",
            "--resume",
            "x",
        ]));
        assert_eq!(
            got,
            never_moved(
                Options {
                    quiet_ms: 500,
                    poll_ms: 50,
                    term_grace_ms: TERM_GRACE_MS,
                },
                &["--resume", "x"]
            )
        );
        // The option is one word: alone it changes only whether the launch
        // may be moved.
        assert_eq!(
            parse(os(&["--alab-no-live-update"])),
            never_moved(Options::default(), &[])
        );
    }

    #[test]
    fn the_twin_envelope_is_dropped_and_everything_after_it_is_the_vendors() {
        use std::os::unix::ffi::OsStringExt as _;
        let opaque = OsString::from_vec(b"--alab-poll-ms=\xff".to_vec());
        assert_eq!(
            parse(vec!["run".into(), "--".into(), opaque.clone()]),
            Ok(Command::Run {
                opts: Options::default(),
                live_update: true,
                args: vec![opaque],
            })
        );
        // What the twin sends: `run --`, then the launch's own arguments.
        assert_eq!(
            parse(os(&["run", "--", "--model", "opus", "fix it"])),
            run(Options::default(), &["--model", "opus", "fix it"])
        );
        assert_eq!(parse(os(&["run", "--"])), run(Options::default(), &[]));
        // Nothing after the envelope is the harness's: not an `--alab-` word
        // (known, unknown or private), not a lone version flag, not a second
        // `run --`, not an empty word.
        for vendor in [
            &["--version"][..],
            &["-v"],
            &["--alab-help"],
            &["--alab-no-live-update", "-c"],
            &["--alab-frob"],
            &["--alab-internal-image-adopt"],
            &["run", "--", "x"],
            &["--", "-p"],
            &["", "a b"],
        ] {
            let mut words = vec!["run", "--"];
            words.extend_from_slice(vendor);
            assert_eq!(
                parse(os(&words)),
                run(Options::default(), vendor),
                "{vendor:?}"
            );
        }
        // The harness's own options still lead, ahead of the envelope.
        assert_eq!(
            parse(os(&[
                "--alab-no-live-update",
                "--alab-poll-ms=50",
                "run",
                "--",
                "--alab-poll-ms=60",
                "--resume",
                "x",
            ])),
            never_moved(
                Options {
                    poll_ms: 50,
                    ..Options::default()
                },
                &["--alab-poll-ms=60", "--resume", "x"]
            )
        );
        assert_eq!(
            parse(os(&["--alab-help", "run", "--", "x"])),
            Ok(Command::Help)
        );
        assert!(parse(os(&["--alab-frob", "run", "--", "x"])).is_err());
    }

    #[test]
    fn run_without_its_dashes_is_the_vendors_word() {
        // `claude run` is a launch with the prompt "run": only `run --` is the envelope.
        for words in [
            &["run"][..],
            &["run", "fix it"],
            &["run", "--version"],
            &["run", "--alab-help"],
            &["run", "-", "-"],
            &["run", "---"],
            &["Run", "--", "x"],
            &["--", "run", "--", "x"],
            &["-p", "run", "--", "x"],
        ] {
            assert_eq!(
                parse(os(words)),
                run(Options::default(), words),
                "{words:?}"
            );
        }
        // A word that is not UTF-8 after `run` is not `--` either.
        use std::os::unix::ffi::OsStringExt as _;
        let odd = vec![OsString::from("run"), OsString::from_vec(vec![0xff])];
        assert_eq!(
            parse(odd.clone()),
            Ok(Command::Run {
                opts: Options::default(),
                live_update: true,
                args: odd
            })
        );
        assert!(usage().contains(&format!("{ENTRY_WORD} -- [")));
    }

    #[test]
    fn only_a_one_inside_aterm_leaves_the_harness_out() {
        use std::ffi::OsStr;
        assert!(left_out(Some(OsStr::new("1")), true));
        // The twin's hand-off reads exactly `1`; nothing else leaves the harness out.
        for value in ["", "0", "true", "yes", "11", " 1", "1 "] {
            assert!(!left_out(Some(OsStr::new(value)), true), "{value:?}");
        }
        assert!(!left_out(None, true));
        // Outside aterm the variable is not read.
        assert!(!left_out(Some(OsStr::new("1")), false));
        assert!(!left_out(None, false));
        assert_eq!(NO_HARNESS_VAR, "ATERM_NO_HARNESS");
        assert!(usage().contains("ATERM_NO_HARNESS=1"));
    }

    #[test]
    fn bad_harness_options_are_refused_not_passed_on() {
        for bad in [
            &["--alab-frob"][..],
            &["--alab-quiet-ms"],
            &["--alab-quiet-ms=5"],
            &["--alab-poll-ms=0"],
            &["--alab-poll-ms=abc"],
            &["--alab-no-live-update=1"],
            &["--alab-term-grace-ms=10"],
            &["--alab-term-grace-ms"],
        ] {
            assert!(parse(os(bad)).is_err(), "{bad:?}");
        }
        use std::os::unix::ffi::OsStringExt as _;
        for bytes in [b"--alab-poll-ms=\xff".as_slice(), b"--alab-\xff".as_slice()] {
            let bad = OsString::from_vec(bytes.to_vec());
            assert!(parse(vec![bad.clone()]).is_err(), "{bad:?}");
            assert!(
                parse(vec!["--alab-quiet-ms=200".into(), bad.clone()]).is_err(),
                "{bad:?}"
            );
        }
    }

    #[test]
    fn usage_states_both_bounds_the_quiet_period_is_parsed_with() {
        assert_eq!(QUIET_MS_MAX, 86_400_000);
        assert!(usage().contains(&format!("(default {QUIET_MS}, 100..=86400000)")));
        let quiet = |ms: u64| parse(os(&[&format!("--alab-quiet-ms={ms}")]));
        let quiet_of = |ms: u64| match quiet(ms) {
            Ok(Command::Run { opts, .. }) => Some(opts.quiet_ms),
            _ => None,
        };
        assert_eq!(quiet_of(100), Some(100));
        assert_eq!(quiet_of(QUIET_MS_MAX), Some(QUIET_MS_MAX));
        assert!(quiet(99).is_err());
        assert_eq!(
            quiet(QUIET_MS_MAX + 1),
            Err("--alab-quiet-ms=86400001: expected a number in 100..=86400000".to_string())
        );
    }

    #[test]
    fn only_a_lone_version_flag_is_the_harness_version() {
        assert_eq!(parse(os(&["--version"])), Ok(Command::Version));
        assert_eq!(parse(os(&["-v"])), Ok(Command::Version));
        assert_eq!(
            parse(os(&["--alab-poll-ms=30", "--version"])),
            Ok(Command::Version)
        );
        assert!(matches!(
            parse(os(&["--version", "--verbose"])),
            Ok(Command::Run { .. })
        ));
        assert_eq!(parse(os(&["--alab-help"])), Ok(Command::Help));
    }

    #[test]
    fn version_prints_the_origin_line() {
        let text = version_line(None);
        assert_eq!(text.lines().nth(1), Some(alab_policy::ORIGIN_LINE));
        assert_eq!(text.lines().count(), 2);
    }

    #[test]
    fn usage_prints_the_origin_line() {
        let text = usage();
        assert_eq!(text.lines().nth(1), Some(alab_policy::ORIGIN_LINE));
        assert_eq!(
            text.lines()
                .filter(|line| *line == alab_policy::ORIGIN_LINE)
                .count(),
            1
        );
    }

    #[test]
    fn version_line_names_repo_harness_and_vendor() {
        use std::path::PathBuf;
        let origin = alab_policy::ORIGIN_LINE;
        let build = store::Build {
            number: 1_000_002_000_001_000_310,
            dir: PathBuf::from("/s/claude/1000002000001000310"),
            exe: PathBuf::from("/s/claude/1000002000001000310/bin/claude"),
            version: Some("2.1.310".to_string()),
            env: Vec::new(),
        };
        assert_eq!(
            version_line(Some(&Ok(build.clone()))),
            format!("claude-code-alab {VERSION} (claude 2.1.310)\n{origin}")
        );
        assert_eq!(
            version_line(Some(&Ok(store::Build {
                version: None,
                ..build
            }))),
            format!("claude-code-alab {VERSION} (claude unknown)\n{origin}")
        );
        assert_eq!(
            version_line(Some(&Err(store::Missing::NotInstalled(PathBuf::from(
                "/s/claude/current"
            ))))),
            format!("claude-code-alab {VERSION} (claude not installed)\n{origin}")
        );
        for missing in [
            store::Missing::Unusable(PathBuf::from("/s/claude/current"), "x".to_string()),
            store::Missing::IsSelf(PathBuf::from("/s/claude/1/bin/claude")),
            store::Missing::Uncompared(PathBuf::from("/s/claude/1/bin/claude"), "x".to_string()),
        ] {
            assert_eq!(
                version_line(Some(&Err(missing.clone()))),
                format!("claude-code-alab {VERSION} (claude unknown)\n{origin}"),
                "{missing:?}"
            );
        }
        assert_eq!(
            version_line(None),
            format!("claude-code-alab {VERSION} (claude unknown)\n{origin}"),
            "no home"
        );
    }

    #[test]
    fn the_term_grace_is_a_harness_option() {
        assert_eq!(
            parse(os(&["--alab-term-grace-ms=1500", "-c"])),
            run(
                Options {
                    term_grace_ms: 1500,
                    ..Options::default()
                },
                &["-c"]
            )
        );
    }

    #[test]
    fn nesting_is_bounded_and_an_unreadable_depth_refuses() {
        use std::ffi::OsStr;
        assert_eq!(nesting_depth(None), Ok(0));
        assert_eq!(nesting_depth(Some(OsStr::new("3"))), Ok(3));
        assert_eq!(
            nesting_depth(Some(OsStr::new(&(MAX_DEPTH - 1).to_string()))),
            Ok(MAX_DEPTH - 1)
        );
        for bad in [
            MAX_DEPTH.to_string(),
            "99".to_string(),
            "x".to_string(),
            "-1".to_string(),
            String::new(),
        ] {
            assert!(nesting_depth(Some(OsStr::new(&bad))).is_err(), "{bad:?}");
        }
        let marks = child_marks(2);
        assert_eq!(marks[0], (PARENT_VAR, std::process::id().to_string()));
        assert_eq!(marks[1], (DEPTH_VAR, "3".to_string()));
    }

    #[test]
    fn only_a_marker_naming_the_parent_means_started_as_vendor() {
        use std::ffi::OsStr;
        assert!(started_as_vendor(Some(OsStr::new("4242")), 4242));
        assert!(!started_as_vendor(Some(OsStr::new("4242")), 1));
        assert!(!started_as_vendor(Some(OsStr::new("x")), 1));
        assert!(!started_as_vendor(None, 4242));
    }

    #[test]
    fn a_published_version_stays_on_its_own_release_line() {
        assert!(version_matches_release(VERSION, env!("CARGO_PKG_VERSION")));
        for version in [
            "0.1.0",
            "0.1.0+g012345678",
            "0.1.0+g0123456789abcdef0123456789abcdef01234567",
        ] {
            assert!(version_matches_release(version, "0.1.0"), "{version}");
        }
        for version in [
            "",
            "0.1",
            "0.1.1",
            "0.2.0",
            "1.1.0",
            "00.1.0",
            "0.01.0",
            "0.1.00",
            "0.1.0-dev.1",
            "0.1.0+g",
            "0.1.0+g01234567",
            "0.1.0+g01234567A",
            "0.1.0+g01234567z",
            "0.1.0+G012345678",
            "0.1.0+g012345678.extra",
            "0.1.0+g0123456789abcdef0123456789abcdef012345678",
            " 0.1.0",
            "0.1.0\n",
        ] {
            assert!(!version_matches_release(version, "0.1.0"), "{version:?}");
        }
    }

    #[test]
    fn the_release_base_is_canonical_major_minor_zero() {
        for base in ["0.0.0", "0.1.0", "1.20.0", "123.456.0"] {
            assert!(version_matches_release(base, base), "{base}");
        }
        for base in [
            "",
            "0",
            "0.1",
            ".1.0",
            "0..0",
            "00.1.0",
            "0.01.0",
            "0.1.1",
            "0.1.0+g012345678",
        ] {
            assert!(!version_matches_release(base, base), "{base}");
        }
    }
}
