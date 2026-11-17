// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Claude Code's facts: names, its own session record, and its resume grammar.
//!
//! Every vendor fact this harness acts on lives here, compiled in (aterm's rule
//! R1, "every vendor fact compiled"). The resume grammar is a port of aterm's
//! measured Claude relaunch lane, cited per item against aterm's main branch
//! of 2026-10-24 (`crates/aterm-agent/src/harness/upgrade.rs`).

use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use crate::json::{self, Value};

/// The harness repository, which `--version` names.
pub const REPO: &str = "claude-code-alab";

/// The harness's own command. Never the vendor's (`claude`): that name is the
/// vendor row's shim and the atpkg `agents/` twin.
pub const TOOL: &str = "claude-alab";

/// The atpkg program whose store holds the vendor build (`store/<PACKAGE>/current`).
pub const PACKAGE: &str = "claude";

/// The vendor's command inside a store build (`<build>/bin/<COMMAND>`).
pub const COMMAND: &str = "claude";

/// Environment variables the harness always sets on the vendor child, matching
/// atpkg's `claude` row (`crates/atpkg/src/vendor_direct/table.rs`: `shim_env`),
/// so Claude Code's own auto-updater and feedback survey stay off even when the
/// build's `.shim-env` sidecar is absent or unreadable.
pub const HARNESS_ENV: &[(&str, &str)] = &[
    ("DISABLE_AUTOUPDATER", "1"),
    ("CLAUDE_CODE_DISABLE_FEEDBACK_SURVEY", "1"),
];

/// The one status in Claude Code's record that means "no turn is running".
pub const STATUS_IDLE: &str = "idle";

/// The record kind of an interactive (TUI) session; print-mode runs are never relaunched.
pub const KIND_INTERACTIVE: &str = "interactive";

/// Bound for reading Claude Code's record (it is well under 1 KiB measured).
pub const MAX_RECORD_BYTES: u64 = 64 * 1024;

/// Claude Code's config directory: `$CLAUDE_CONFIG_DIR` when set to an absolute
/// path, else `<home>/.claude`.
#[must_use]
pub fn config_dir(home: &Path, configured: Option<&std::ffi::OsStr>) -> PathBuf {
    match configured.map(Path::new) {
        Some(dir) if dir.is_absolute() => dir.to_path_buf(),
        _ => home.join(".claude"),
    }
}

/// Claude Code's record of the running interactive process `pid`:
/// `<config>/sessions/<pid>.json` (aterm `upgrade.rs:85-129`, `SessionFile`).
#[must_use]
pub fn record_path(config: &Path, pid: u32) -> PathBuf {
    config.join("sessions").join(format!("{pid}.json"))
}

/// The fields of Claude Code's own session record this harness reads.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionRecord {
    /// The process the record describes.
    pub pid: u32,
    /// The conversation `--resume` names.
    pub session_id: String,
    /// The directory the conversation was started in (`--resume` looks it up there).
    pub cwd: String,
    /// `idle`, `busy`, `shell`, `waiting`, … ([`STATUS_IDLE`]).
    pub status: String,
    /// When `status` last changed, epoch milliseconds.
    pub status_updated_at_ms: u64,
    /// When the process started, epoch milliseconds.
    pub started_at_ms: u64,
    /// `interactive` for a TUI session ([`KIND_INTERACTIVE`]).
    pub kind: String,
    /// The vendor version that wrote it.
    pub version: String,
}

impl SessionRecord {
    /// Parse a record, refusing one that lacks any field this harness relies on.
    ///
    /// # Errors
    /// The text is not JSON, or a field is missing or of the wrong type.
    pub fn parse(text: &str) -> Result<Self, String> {
        let v = json::parse(text).ok_or("not JSON")?;
        let s = |k: &str| -> Result<String, String> {
            v.get(k)
                .and_then(Value::as_str)
                .map(str::to_owned)
                .ok_or_else(|| format!("no string `{k}`"))
        };
        let n = |k: &str| -> Result<u64, String> {
            v.get(k)
                .and_then(Value::as_u64)
                .ok_or_else(|| format!("no number `{k}`"))
        };
        Ok(Self {
            pid: u32::try_from(n("pid")?).map_err(|_| "pid out of range".to_string())?,
            session_id: s("sessionId")?,
            cwd: s("cwd")?,
            status: s("status")?,
            status_updated_at_ms: n("statusUpdatedAt")?,
            started_at_ms: n("startedAt")?,
            kind: s("kind")?,
            version: s("version")?,
        })
    }

    /// Read `pid`'s record under `config`; `None` when absent, unreadable or invalid.
    /// The size and type are checked on the handle the record is read from
    /// ([`crate::store::read_bounded`]), so a record replaced in between is
    /// never read past [`MAX_RECORD_BYTES`].
    #[must_use]
    pub fn read(config: &Path, pid: u32) -> Option<Self> {
        let text = crate::store::read_bounded(&record_path(config, pid), MAX_RECORD_BYTES, true)?;
        Self::parse(&text).ok()
    }
}

/// Whether `id` is a session id Claude Code's `--resume` takes: 8..=64 ASCII
/// hex digits and dashes (aterm `upgrade.rs:131-135`, `is_session_id`).
#[must_use]
pub fn is_session_id(id: &str) -> bool {
    (8..=64).contains(&id.len()) && id.bytes().all(|b| b.is_ascii_hexdigit() || b == b'-')
}

/// Why a launch argv cannot be carried into a `--resume` relaunch
/// (aterm `upgrade.rs:319-333`, `ArgvRefusal`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ArgvRefusal {
    /// A flag the table does not know: carrying it blind could change the session.
    UnknownFlag(String),
    /// A flag whose launch can never be resumed (print mode, help, remote, …).
    NotResumable(String),
}

impl std::fmt::Display for ArgvRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ArgvRefusal::UnknownFlag(fl) => write!(f, "unknown-flag {fl}"),
            ArgvRefusal::NotResumable(fl) => write!(f, "not-resumable {fl}"),
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Arity {
    None,
    One,
    Many,
    Optional,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Fate {
    Keep,
    Drop,
    Refuse,
}

/// Claude Code's launch flags and what a resume does with each: verbatim from
/// aterm `upgrade.rs:350-438` (`FLAGS`) on aterm's main of 2026-10-24, measured there against
/// Claude Code's `--help` by aterm's real-Claude lab.
const FLAGS: &[(&str, Arity, Fate)] = &[
    ("--add-dir", Arity::Many, Fate::Keep),
    ("--agent", Arity::One, Fate::Keep),
    ("--agents", Arity::One, Fate::Keep),
    (
        "--allow-dangerously-skip-permissions",
        Arity::None,
        Fate::Keep,
    ),
    ("--allowedTools", Arity::Many, Fate::Keep),
    ("--allowed-tools", Arity::Many, Fate::Keep),
    ("--append-system-prompt", Arity::One, Fate::Keep),
    ("--append-system-prompt-file", Arity::One, Fate::Keep),
    ("--autocompact", Arity::One, Fate::Keep),
    ("--ax-screen-reader", Arity::None, Fate::Keep),
    ("--bg", Arity::None, Fate::Refuse),
    ("--background", Arity::None, Fate::Refuse),
    ("--bare", Arity::None, Fate::Keep),
    ("--betas", Arity::Many, Fate::Keep),
    ("--brief", Arity::None, Fate::Keep),
    ("--chrome", Arity::None, Fate::Keep),
    ("--cloud", Arity::Optional, Fate::Refuse),
    ("-c", Arity::None, Fate::Drop),
    ("--continue", Arity::None, Fate::Drop),
    ("--dangerously-skip-permissions", Arity::None, Fate::Keep),
    ("-d", Arity::Optional, Fate::Keep),
    ("--debug", Arity::Optional, Fate::Keep),
    ("--debug-file", Arity::One, Fate::Keep),
    ("--disable-slash-commands", Arity::None, Fate::Keep),
    ("--disallowedTools", Arity::Many, Fate::Keep),
    ("--disallowed-tools", Arity::Many, Fate::Keep),
    ("--effort", Arity::One, Fate::Drop),
    ("--environment", Arity::One, Fate::Refuse),
    (
        "--exclude-dynamic-system-prompt-sections",
        Arity::None,
        Fate::Keep,
    ),
    ("--fallback-model", Arity::One, Fate::Keep),
    ("--file", Arity::Many, Fate::Refuse),
    ("--fork-session", Arity::None, Fate::Drop),
    ("--forward-subagent-text", Arity::None, Fate::Keep),
    ("--from-pr", Arity::Optional, Fate::Drop),
    ("-h", Arity::None, Fate::Refuse),
    ("--help", Arity::None, Fate::Refuse),
    ("--ide", Arity::None, Fate::Keep),
    ("--include-hook-events", Arity::None, Fate::Keep),
    ("--include-partial-messages", Arity::None, Fate::Keep),
    ("--input-format", Arity::One, Fate::Refuse),
    ("--json-schema", Arity::One, Fate::Refuse),
    ("--max-budget-usd", Arity::One, Fate::Keep),
    ("--mcp-config", Arity::Many, Fate::Keep),
    ("--model", Arity::One, Fate::Keep),
    ("-n", Arity::One, Fate::Keep),
    ("--name", Arity::One, Fate::Keep),
    ("--no-chrome", Arity::None, Fate::Keep),
    ("--no-session-persistence", Arity::None, Fate::Refuse),
    ("--output-format", Arity::One, Fate::Refuse),
    ("--permission-mode", Arity::One, Fate::Keep),
    ("--permission-prompts", Arity::One, Fate::Keep),
    ("--plugin-dir", Arity::One, Fate::Keep),
    ("--plugin-url", Arity::One, Fate::Keep),
    ("-p", Arity::None, Fate::Refuse),
    ("--print", Arity::None, Fate::Refuse),
    ("--prompt-suggestions", Arity::Optional, Fate::Keep),
    ("--remote-control", Arity::Optional, Fate::Refuse),
    (
        "--remote-control-session-name-prefix",
        Arity::One,
        Fate::Refuse,
    ),
    ("--replay-user-messages", Arity::None, Fate::Refuse),
    ("--restricted", Arity::None, Fate::Keep),
    ("-r", Arity::Optional, Fate::Drop),
    ("--resume", Arity::Optional, Fate::Drop),
    ("--safe-mode", Arity::None, Fate::Keep),
    ("--session-id", Arity::One, Fate::Drop),
    ("--setting-sources", Arity::One, Fate::Keep),
    ("--settings", Arity::One, Fate::Keep),
    ("--strict-mcp-config", Arity::None, Fate::Keep),
    ("--system-prompt", Arity::One, Fate::Keep),
    ("--system-prompt-file", Arity::One, Fate::Keep),
    ("--system-prompt-snapshot", Arity::One, Fate::Keep),
    ("--teleport", Arity::Optional, Fate::Refuse),
    ("--tmux", Arity::Optional, Fate::Refuse),
    ("--tools", Arity::Many, Fate::Keep),
    ("--verbose", Arity::None, Fate::Keep),
    ("-v", Arity::None, Fate::Refuse),
    ("--version", Arity::None, Fate::Refuse),
    ("-w", Arity::Optional, Fate::Refuse),
    ("--worktree", Arity::Optional, Fate::Refuse),
];

fn flag(name: &str) -> Option<(Arity, Fate)> {
    FLAGS
        .iter()
        .find(|(n, _, _)| *n == name)
        .map(|(_, a, f)| (*a, *f))
}

fn maybe_option(tok: &OsStr) -> bool {
    let bytes = tok.as_encoded_bytes();
    bytes.len() > 1 && bytes.starts_with(b"-")
}

/// One option with its original value boundaries and argv position.
struct ParsedFlag<'a, W> {
    name: &'a str,
    fate: Option<Fate>,
    tokens: &'a [W],
    at: usize,
}

fn parsed(argv: &[String]) -> Vec<ParsedFlag<'_, String>> {
    parsed_with(argv, &[], |_| {}).expect("String arguments always have UTF-8 option names")
}

fn parsed_with<'a, W: AsRef<OsStr>>(
    argv: &'a [W],
    extra_flags: &[&str],
    mut positional: impl FnMut(&'a W),
) -> Option<Vec<ParsedFlag<'a, W>>> {
    let mut out = Vec::new();
    let mut i = 1;
    while i < argv.len() {
        let tok = argv[i].as_ref();
        if tok == "--" {
            break;
        }
        if !maybe_option(tok) {
            positional(&argv[i]);
            i += 1;
            continue;
        }
        // Only a long option's name needs text; its inline value may be opaque.
        let bytes = tok.as_encoded_bytes();
        let inline_at = if bytes.starts_with(b"--") {
            bytes.iter().position(|byte| *byte == b'=')
        } else {
            None
        };
        let (name, inline) = match inline_at {
            Some(at) => (std::str::from_utf8(&bytes[..at]).ok()?, true),
            None => (tok.to_str()?, false),
        };
        let known = flag(name).or_else(|| {
            (!inline && extra_flags.contains(&name)).then_some((Arity::None, Fate::Keep))
        });
        let start = i;
        i += 1;
        if !inline {
            match known.map_or(Arity::None, |(arity, _)| arity) {
                Arity::None => {}
                Arity::One => {
                    if i < argv.len() {
                        i += 1;
                    }
                }
                Arity::Optional => {
                    if i < argv.len() && !maybe_option(argv[i].as_ref()) {
                        i += 1;
                    }
                }
                Arity::Many => {
                    // The first value is required, so Claude's parser takes it
                    // whatever it looks like (`--add-dir -c`).
                    if i < argv.len() {
                        i += 1;
                    }
                    while i < argv.len() && !maybe_option(argv[i].as_ref()) {
                        i += 1;
                    }
                }
            }
        }
        out.push(ParsedFlag {
            name,
            fate: known.map(|(_, fate)| fate),
            tokens: &argv[start..i],
            at: start,
        });
    }
    Some(out)
}

/// Known option names/positions and the first positional word, using the same
/// value boundaries as resume parsing. Launcher-only switches may add zero-arity
/// names; vendor arities still have one source. Unknown arity grants no scan.
/// Positional words and option values remain native; only option names need UTF-8.
pub(crate) struct LaunchWords<'a, W> {
    pub flags: Vec<(&'a str, usize)>,
    pub first: Option<&'a W>,
}

pub(crate) fn launch_words<'a, W: AsRef<OsStr>>(
    argv: &'a [W],
    extra_flags: &[&str],
) -> Option<LaunchWords<'a, W>> {
    let mut first = None;
    let flags = parsed_with(argv, extra_flags, |word| {
        if first.is_none() {
            first = Some(word);
        }
    })?;
    let names = flags
        .into_iter()
        .map(|flag| flag.fate.map(|_| (flag.name, flag.at)))
        .collect::<Option<Vec<_>>>()?;
    Some(LaunchWords {
        flags: names,
        first,
    })
}

/// The flags a resume carries, or why none can (aterm `upgrade.rs:490-503`,
/// `carried`). The managed usage tap is attached only when constructing the
/// child command; resume parsing sees the person's settings and keeps them.
fn carried(argv: &[String]) -> Result<Vec<(&str, &[String])>, ArgvRefusal> {
    let mut out = Vec::new();
    for ParsedFlag {
        name, fate, tokens, ..
    } in parsed(argv)
    {
        match fate {
            None => return Err(ArgvRefusal::UnknownFlag(name.to_string())),
            Some(Fate::Refuse) => return Err(ArgvRefusal::NotResumable(name.to_string())),
            Some(Fate::Keep) => out.push((name, tokens)),
            Some(Fate::Drop) => {}
        }
    }
    Ok(out)
}

/// The argv (without `argv[0]`) that resumes `session_id` with the launch's own
/// flags: every kept flag in launch order, then `--resume <session_id>`
/// (aterm `upgrade.rs:454-463`, `rewrite_argv`). Positional words (a first
/// prompt) and everything after `--` are not carried.
///
/// # Errors
/// [`ArgvRefusal`] when a flag is unknown or makes the launch unresumable.
pub fn resume_argv(argv: &[String], session_id: &str) -> Result<Vec<String>, ArgvRefusal> {
    let mut out: Vec<String> = carried(argv)?
        .into_iter()
        .flat_map(|(_, tokens)| tokens.iter().cloned())
        .collect();
    out.push("--resume".to_string());
    out.push(session_id.to_string());
    Ok(out)
}

/// How much of a transcript's end is read for its last permission mode (aterm
/// `upgrade_drive.rs:6753`, `TAIL_BYTES`). Claude Code writes a
/// `permission-mode` line with every turn, so the last one is in any tail.
pub const TRANSCRIPT_TAIL_BYTES: u64 = 262_144;

/// The transcript of `session` under `config`: the one
/// `<config>/projects/<dir>/<session>.jsonl`; `None` when there is none or more
/// than one (aterm `upgrade_drive.rs:6762-6777`, `transcript_in`).
#[must_use]
pub fn transcript_in(config: &Path, session: &str) -> Option<PathBuf> {
    transcript_lookup::legacy(config, session)
}

#[path = "profile_transcript_lookup.rs"]
mod transcript_lookup;
pub(crate) use transcript_lookup::bounded as transcript_in_bounded;

/// The transcript at `path` opened for reading, with the handle's own
/// metadata. A link is followed; the open never blocks, and only a handle that
/// is a regular file is returned, so a FIFO or device (named directly or
/// through a link) gives `None`.
#[must_use]
pub fn open_transcript(path: &Path) -> Option<(std::fs::File, std::fs::Metadata)> {
    use std::os::unix::fs::OpenOptionsExt as _;
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK)
        .open(path)
        .ok()?;
    let meta = file.metadata().ok()?;
    meta.is_file().then_some((file, meta))
}

/// The last [`TRANSCRIPT_TAIL_BYTES`] of `path` as text (a cut first line is
/// harmless: it does not parse, so it is skipped), opened as
/// [`open_transcript`] opens it.
#[must_use]
pub fn transcript_tail(path: &Path) -> Option<String> {
    use std::io::{Read as _, Seek as _, SeekFrom};
    let (mut file, meta) = open_transcript(path)?;
    let len = meta.len();
    file.seek(SeekFrom::Start(len.saturating_sub(TRANSCRIPT_TAIL_BYTES)))
        .ok()?;
    let mut bytes = Vec::new();
    file.take(TRANSCRIPT_TAIL_BYTES)
        .read_to_end(&mut bytes)
        .ok()?;
    Some(String::from_utf8_lossy(&bytes).into_owned())
}

/// The conversation's historical last permission mode in a transcript tail: the newest
/// `{"type":"permission-mode","permissionMode":…}` line naming a known mode
/// (aterm `relaunch.rs:487-498`, `permission_mode_of`). No writer identity or
/// current-child authority can be inferred from this history.
#[must_use]
pub fn permission_mode_of(tail: &str) -> Option<String> {
    tail.lines()
        .rev()
        .filter(|l| l.contains("\"permission-mode\""))
        .filter_map(json::parse)
        .find(|v| v.get("type").and_then(Value::as_str) == Some("permission-mode"))?
        .get("permissionMode")
        .and_then(Value::as_str)
        .filter(|m| crate::permission::Mode::of_setting(m).is_some())
        .map(str::to_owned)
}

/// `flags` (from [`resume_argv`], the resume pair last) resumed in the
/// conversation's permission `mode`: with no mode, or still in bypass, the launch's own
/// flags stand; any other mode replaces the launch's permission flags with
/// `--permission-mode <mode>`, and a launch that allowed bypass keeps it
/// reachable with `--allow-dangerously-skip-permissions`. A resume never moves
/// a session into bypass that the launch did not allow, and never back into
/// bypass from a mode the person chose inside the session. Option values and a
/// literal `--` suffix keep their original boundaries; unknown arity leaves
/// the input unchanged rather than guessing which words describe posture.
#[must_use]
pub fn with_permission_mode(flags: Vec<String>, mode: Option<&str>) -> Vec<String> {
    let Some(mode) = mode.filter(|m| *m != "bypassPermissions") else {
        return flags;
    };
    let mode = if mode == "manual" { "default" } else { mode };
    let argv: Vec<String> = std::iter::once(COMMAND.to_string()).chain(flags).collect();
    let units = parsed(&argv);
    if units.iter().any(|unit| unit.fate.is_none()) {
        return argv.into_iter().skip(1).collect();
    }
    let mut out = Vec::new();
    let mut insert_at = None;
    let mut bypass_in_cycle = false;
    let mut after = 1;
    for unit in units {
        out.extend_from_slice(&argv[after..unit.at]);
        match unit.name {
            "--dangerously-skip-permissions" | "--allow-dangerously-skip-permissions" => {
                bypass_in_cycle = true;
            }
            "--permission-mode" => {
                bypass_in_cycle |= unit.tokens[0] == "--permission-mode=bypassPermissions"
                    || unit
                        .tokens
                        .get(1)
                        .is_some_and(|mode| mode == "bypassPermissions");
            }
            "--resume" | "-r" => {
                insert_at.get_or_insert(out.len());
                out.extend_from_slice(unit.tokens);
            }
            _ => out.extend_from_slice(unit.tokens),
        }
        after = unit.at + unit.tokens.len();
    }
    let suffix = &argv[after..];
    let insert_at = insert_at.unwrap_or_else(|| {
        out.len()
            + suffix
                .iter()
                .position(|word| word == "--")
                .unwrap_or(suffix.len())
    });
    out.extend_from_slice(suffix);
    let mut posture = Vec::new();
    if bypass_in_cycle {
        posture.push("--allow-dangerously-skip-permissions".to_string());
    }
    posture.push("--permission-mode".to_string());
    posture.push(mode.to_string());
    out.splice(insert_at..insert_at, posture);
    out
}

/// Whether `argv` (`argv[0]` first) names a model with `--model`, an alias or
/// an id alike. That pins Claude Code's main-loop model as a `/model` choice
/// does, so a `/resume` inside the session keeps it (aterm's model reader,
/// `Switch::pinned` in `footer.rs`; [`crate::model_facts`]).
#[must_use]
pub fn pins_model(argv: &[String]) -> bool {
    parsed(argv).iter().any(|flag| flag.name == "--model")
}

const ONE_SHOT: &[&str] = &["-p", "--print", "-h", "--help", "-v", "--version"];

/// Whether `argv` (`argv[0]` first) is a one-shot invocation, which is never
/// relaunched (aterm `upgrade.rs:577-585`, `one_shot`).
#[must_use]
pub fn one_shot(argv: &[String]) -> bool {
    parsed(argv)
        .iter()
        .any(|flag| ONE_SHOT.contains(&flag.name))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(words: &[&str]) -> Vec<String> {
        std::iter::once("claude")
            .chain(words.iter().copied())
            .map(str::to_string)
            .collect()
    }

    const SID: &str = "7c1e2a90-4b3d-4f6e-9a18-2d5c6b7e8f01";

    #[test]
    fn resume_keeps_launch_flags_and_drops_the_prompt() {
        let got = resume_argv(
            &argv(&[
                "--model",
                "opus",
                "--dangerously-skip-permissions",
                "fix the bug",
                "--add-dir",
                "a",
                "b",
            ]),
            SID,
        )
        .expect("resumable");
        assert_eq!(
            got,
            [
                "--model",
                "opus",
                "--dangerously-skip-permissions",
                "--add-dir",
                "a",
                "b",
                "--resume",
                SID
            ]
        );
    }

    #[test]
    fn resume_replaces_the_launch_resume_and_continue() {
        for launch in [
            &["--resume", "0000aaaa-0000"][..],
            &["-r"],
            &["-c"],
            &["--continue", "--fork-session"],
            &["--session-id", "1111bbbb"],
            &["--resume=2222cccc"],
        ] {
            assert_eq!(
                resume_argv(&argv(launch), SID).expect("resumable"),
                ["--resume", SID],
                "{launch:?}"
            );
        }
    }

    #[test]
    fn resume_keeps_inline_values_and_stops_at_double_dash() {
        assert_eq!(
            resume_argv(&argv(&["--model=sonnet", "--", "--print"]), SID).expect("resumable"),
            ["--model=sonnet", "--resume", SID]
        );
    }

    #[test]
    fn resume_refuses_unknown_and_unresumable_flags() {
        assert_eq!(
            resume_argv(&argv(&["--frobnicate"]), SID),
            Err(ArgvRefusal::UnknownFlag("--frobnicate".into()))
        );
        assert_eq!(
            resume_argv(&argv(&["-p", "hi"]), SID),
            Err(ArgvRefusal::NotResumable("-p".into()))
        );
        assert_eq!(
            resume_argv(&argv(&["--worktree"]), SID),
            Err(ArgvRefusal::NotResumable("--worktree".into()))
        );
    }

    #[test]
    fn a_value_that_looks_like_a_flag_is_taken_by_its_flag() {
        // `--model` takes one value whatever it looks like; `--add-dir` takes its first.
        assert_eq!(
            resume_argv(&argv(&["--add-dir", "-c", "--model", "-x"]), SID).expect("resumable"),
            ["--add-dir", "-c", "--model", "-x", "--resume", SID]
        );
    }

    #[test]
    fn one_shot_launches_are_recognized() {
        assert!(one_shot(&argv(&["-p", "hello"])));
        assert!(one_shot(&argv(&["--help"])));
        assert!(one_shot(&argv(&["--model", "x", "--version"])));
        assert!(!one_shot(&argv(&["--model", "x", "hello"])));
        assert!(!one_shot(&argv(&["--", "-p"])));
    }

    #[test]
    fn only_a_model_flag_pins_the_model() {
        assert!(pins_model(&argv(&["--model", "opus"])));
        assert!(pins_model(&argv(&["--model=claude-opus-5-5", "hi"])));
        // CONTROL: the same word as another flag's value, a prompt or past `--`.
        assert!(!pins_model(&argv(&["--add-dir", "--model"])));
        assert!(!pins_model(&argv(&[
            "--fallback-model",
            "opus",
            "--model?"
        ])));
        assert!(!pins_model(&argv(&["--", "--model", "opus"])));
        assert!(!pins_model(&argv(&[])));
    }

    #[test]
    fn session_ids_are_hex_and_dashes_of_bounded_length() {
        assert!(is_session_id(SID));
        assert!(is_session_id("abcdef01"));
        assert!(!is_session_id("abcdef0"));
        assert!(!is_session_id(&"a".repeat(65)));
        assert!(!is_session_id("7c1e2a90 4b3d"));
        assert!(!is_session_id("../../etc"));
    }

    #[test]
    fn record_parse_requires_every_relied_on_field() {
        let full = format!(
            r#"{{"pid":7,"sessionId":"{SID}","cwd":"/w","status":"idle","statusUpdatedAt":5,"startedAt":3,"kind":"interactive","version":"2.1.310","extra":[1]}}"#
        );
        let r = SessionRecord::parse(&full).expect("parses");
        assert_eq!(r.pid, 7);
        assert_eq!(r.session_id, SID);
        assert_eq!(r.status, STATUS_IDLE);
        assert_eq!(r.status_updated_at_ms, 5);
        assert_eq!(r.started_at_ms, 3);
        for key in [
            "pid",
            "sessionId",
            "cwd",
            "status",
            "statusUpdatedAt",
            "startedAt",
            "kind",
            "version",
        ] {
            let pattern = format!("\"{key}\":");
            let without = full.replacen(&pattern, "\"x_renamed\":", 1);
            assert!(
                SessionRecord::parse(&without).is_err(),
                "a record without {key} must be refused"
            );
        }
    }

    #[test]
    fn config_dir_honours_only_an_absolute_override() {
        let home = Path::new("/h");
        assert_eq!(config_dir(home, None), Path::new("/h/.claude"));
        assert_eq!(
            config_dir(home, Some(std::ffi::OsStr::new("/c"))),
            Path::new("/c")
        );
        assert_eq!(
            config_dir(home, Some(std::ffi::OsStr::new("rel"))),
            Path::new("/h/.claude")
        );
        assert_eq!(
            record_path(Path::new("/c"), 42),
            Path::new("/c/sessions/42.json")
        );
    }

    fn args(words: &[&str]) -> Vec<String> {
        words.iter().map(|w| (*w).to_string()).collect()
    }

    // Ported from aterm `relaunch_tests.rs`
    // (`the_conversations_last_permission_mode_is_read_from_its_transcript`).
    #[test]
    fn the_conversations_last_permission_mode_is_read_from_its_transcript() {
        let tail = concat!(
            r#"{"type":"permission-mode","permissionMode":"bypassPermissions","sessionId":"x"}"#,
            "\n",
            r#"{"type":"user","permissionMode":"bypassPermissions","timestamp":"2026-09-24T21:47:16.939Z"}"#,
            "\n",
            r#"{"type":"mode","mode":"normal","sessionId":"x"}"#,
            "\n",
            r#"{"type":"permission-mode","permissionMode":"auto","sessionId":"x"}"#,
            "\n",
            r#"{"type":"user","message":{"content":[{"type":"tool_result","content":"\"permission-mode\""}]}}"#,
            "\n",
        );
        assert_eq!(permission_mode_of(tail).as_deref(), Some("auto"));
        assert_eq!(permission_mode_of(""), None);
        assert_eq!(
            permission_mode_of(r#"{"type":"permission-mode","permissionMode":"yolo; rm"}"#),
            None,
            "a mode this module does not know is not carried"
        );
        // A tail cut mid-line: the cut line does not parse and is skipped.
        assert_eq!(
            permission_mode_of(concat!(
                r#"ode","permissionMode":"auto"}"#,
                "\n",
                r#"{"type":"permission-mode","permissionMode":"plan"}"#
            ))
            .as_deref(),
            Some("plan")
        );
    }

    // Ported from aterm `relaunch_tests.rs`
    // (`a_relaunch_resumes_the_mode_the_conversation_ran_in`).
    #[test]
    fn a_relaunch_resumes_the_mode_the_conversation_ran_in() {
        let carried = args(&[
            "--dangerously-skip-permissions",
            "--model",
            "opus",
            "--resume",
            "ID",
        ]);
        assert_eq!(
            with_permission_mode(carried.clone(), Some("auto")),
            args(&[
                "--model",
                "opus",
                "--allow-dangerously-skip-permissions",
                "--permission-mode",
                "auto",
                "--resume",
                "ID"
            ])
        );
        assert_eq!(with_permission_mode(carried.clone(), None), carried);
        assert_eq!(
            with_permission_mode(carried.clone(), Some("bypassPermissions")),
            carried,
            "still in bypass: as launched"
        );
        for default in ["default", "manual"] {
            assert_eq!(
                with_permission_mode(carried.clone(), Some(default)),
                args(&[
                    "--model",
                    "opus",
                    "--allow-dangerously-skip-permissions",
                    "--permission-mode",
                    "default",
                    "--resume",
                    "ID"
                ]),
                "the default is named too ({default})"
            );
        }
        assert_eq!(
            with_permission_mode(
                args(&[
                    "--permission-mode=plan",
                    "--add-dir",
                    "/w",
                    "--resume",
                    "ID"
                ]),
                Some("acceptEdits")
            ),
            args(&[
                "--add-dir",
                "/w",
                "--permission-mode",
                "acceptEdits",
                "--resume",
                "ID"
            ])
        );
        assert_eq!(
            with_permission_mode(args(&["--resume", "ID"]), Some("bypassPermissions")),
            args(&["--resume", "ID"]),
            "never INTO bypass"
        );
        assert_eq!(
            with_permission_mode(
                args(&["--permission-mode", "bypassPermissions", "--resume", "ID"]),
                Some("plan")
            ),
            args(&[
                "--allow-dangerously-skip-permissions",
                "--permission-mode",
                "plan",
                "--resume",
                "ID"
            ])
        );
    }

    #[test]
    fn a_resume_mode_rewrite_preserves_known_option_values() {
        for option in ["--model", "--append-system-prompt", "--settings", "--tools"] {
            for value in [
                "--dangerously-skip-permissions",
                "--allow-dangerously-skip-permissions",
                "--permission-mode=plan",
                "--resume",
                "--",
            ] {
                let launch = args(&["claude", "--permission-mode", "default", option, value]);
                let flags = resume_argv(&launch, "ID").expect("known value boundary");
                assert_eq!(
                    with_permission_mode(flags, Some("auto")),
                    args(&[option, value, "--permission-mode", "auto", "--resume", "ID"]),
                    "{option} {value} is a value, not permission posture"
                );
            }
        }
    }

    #[test]
    fn a_resume_mode_rewrite_respects_literal_tail_and_real_posture_order() {
        assert_eq!(
            with_permission_mode(
                args(&[
                    "--permission-mode=default",
                    "--model",
                    "opus",
                    "--",
                    "--dangerously-skip-permissions",
                    "--resume",
                    "literal",
                    "--permission-mode=plan",
                ]),
                Some("auto")
            ),
            args(&[
                "--model",
                "opus",
                "--permission-mode",
                "auto",
                "--",
                "--dangerously-skip-permissions",
                "--resume",
                "literal",
                "--permission-mode=plan",
            ])
        );
        assert_eq!(
            with_permission_mode(
                args(&[
                    "--resume",
                    "ID",
                    "--model",
                    "--dangerously-skip-permissions",
                    "--permission-mode",
                    "bypassPermissions",
                ]),
                Some("plan")
            ),
            args(&[
                "--allow-dangerously-skip-permissions",
                "--permission-mode",
                "plan",
                "--resume",
                "ID",
                "--model",
                "--dangerously-skip-permissions",
            ]),
            "an actual bypass stays reachable without changing the model value"
        );
        let unknown = args(&[
            "--future-value",
            "--permission-mode",
            "default",
            "--resume",
            "ID",
        ]);
        assert_eq!(
            with_permission_mode(unknown.clone(), Some("auto")),
            unknown,
            "unknown option arity cannot authorize a rewrite"
        );
    }

    #[test]
    fn the_transcript_is_the_one_file_named_by_the_session() {
        let root = std::env::temp_dir().join(format!(
            "claude-alab-transcript-{}-{}",
            std::process::id(),
            line!()
        ));
        let _ = std::fs::remove_dir_all(&root);
        let one = root.join("projects").join("-w-one");
        std::fs::create_dir_all(&one).expect("mkdir");
        assert_eq!(transcript_in(&root, SID), None, "no transcript yet");
        std::fs::write(one.join(format!("{SID}.jsonl")), "{}\n").expect("write");
        assert_eq!(
            transcript_in(&root, SID),
            Some(one.join(format!("{SID}.jsonl")))
        );
        assert_eq!(transcript_in(&root, "../../etc"), None, "not a session id");
        let two = root.join("projects").join("-w-two");
        std::fs::create_dir_all(&two).expect("mkdir");
        std::fs::write(two.join(format!("{SID}.jsonl")), "{}\n").expect("write");
        assert_eq!(
            transcript_in(&root, SID),
            None,
            "ambiguous: two transcripts"
        );
        let fifo = root.join("fifo.jsonl");
        let c_fifo = std::ffi::CString::new(fifo.as_os_str().as_encoded_bytes()).expect("cstr");
        // SAFETY: `c_fifo` is a valid NUL-terminated path in our private test directory.
        assert_eq!(unsafe { libc::mkfifo(c_fifo.as_ptr(), 0o600) }, 0);
        assert_eq!(transcript_tail(&fifo), None, "fifo");
        let link = root.join("link.jsonl");
        std::os::unix::fs::symlink(one.join(format!("{SID}.jsonl")), &link).expect("symlink");
        assert_eq!(
            transcript_tail(&link).as_deref(),
            Some("{}\n"),
            "a linked transcript is read like its target"
        );
        let fifo_link = root.join("fifo-link.jsonl");
        std::os::unix::fs::symlink(&fifo, &fifo_link).expect("symlink");
        assert_eq!(transcript_tail(&fifo_link), None, "a link to a fifo");
        let device_link = root.join("device-link.jsonl");
        std::os::unix::fs::symlink("/dev/zero", &device_link).expect("symlink");
        assert_eq!(transcript_tail(&device_link), None, "a link to a device");
        // A transcript that is itself a link is found, as aterm finds it.
        std::fs::remove_dir_all(&two).expect("rm two");
        std::fs::remove_file(one.join(format!("{SID}.jsonl"))).expect("rm transcript");
        let stored = root.join(format!("{SID}.stored"));
        std::fs::write(&stored, "{}\n").expect("write");
        std::os::unix::fs::symlink(&stored, one.join(format!("{SID}.jsonl"))).expect("symlink");
        assert_eq!(
            transcript_in(&root, SID),
            Some(one.join(format!("{SID}.jsonl"))),
            "a linked transcript"
        );
        let _ = std::fs::remove_dir_all(&root);
    }
}
