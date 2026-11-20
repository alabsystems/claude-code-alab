// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! What the repositories the session works in hold that no remote main has:
//! the read Land's rest waits on ([`crate::duty::Look::repo`]).
//!
//! aterm's engine counts a land only when the agent answers its check
//! `LANDED` and its probe of the session's repository reads clean
//! (`crates/aterm-agent/src/harness/repo_work.rs`, `probe`; its turn-end rule
//! `Landed && reading.repo.is_clean() && !left_held`). This is that probe's
//! core, ported: the same reads, the same counts, the same words, over every
//! repository the session works in ([`probe`]).
//!
//! * The repositories are those `git rev-parse --show-toplevel` names from
//!   each folder [`probe`] is given (the folder the vendor child runs in, the
//!   one the harness started in, and those the transcript shows the session
//!   changing: the folder of a file it edited or wrote, and one a shell
//!   command made git work in, [`session_folders`]), each once; a folder the
//!   session only read or went into is not one, since work there may be
//!   someone else's. Then the superproject each is a submodule of, up to the
//!   outermost, as aterm climbs to it; and the populated submodules of each,
//!   for their commits no remote has. A folder that is gone, is in no
//!   repository, or lies under a folder macOS guards with a consent dialog
//!   ([`guarded`]) is passed over, as in aterm. Where no `HOME` is given, or
//!   no `git` may run here ([`git_program`]), nothing can be read, and the
//!   reading is [`RepoRead::Unread`], never clean: a landing there would
//!   rest on the agent's word alone.
//! * Counted in each repository a folder names, and in each superproject:
//!   changed and untracked paths (`git status --porcelain=v1 -z
//!   --untracked-files=normal`), stash entries made on `HEAD`'s branch
//!   ([`stashes_on`]), commits on `HEAD` no remote-tracking ref has (only where
//!   a remote is configured or tracked: there is nowhere to push otherwise),
//!   and commits on `HEAD` the remote main lacks (`origin/HEAD`'s target, else
//!   `origin/main`, else `origin/master`). In a submodule reached from its
//!   superproject, only its commits no remote has: its `HEAD` is the pin its
//!   superproject records, which need not be its remote's main.
//! * The first repository that holds anything is the finding
//!   ([`RepoRead::Found`]). Where none does, one git refused where it must
//!   answer makes the reading [`RepoRead::Unread`]; one a read of which did
//!   not end within its limit makes it [`Probed::Cut`]: not known yet, to be
//!   read again later, never a reading Land acts on.
//!
//! Not ported (aterm's probe reads them, this one does not): branches the
//! session parked work on, Cargo source baselines, a folder that is gone (aterm
//! names it; here it is passed over), and a status git could not finish
//! because a directory would not open (aterm reads its warning as a cut read;
//! here it counts what git listed).
//!
//! Every read runs `git` with no pager, no hooks and no file-system monitor,
//! in the C locale, with no system configuration, no optional locks and no
//! terminal prompt, each within its own limit from the end of the read before
//! it ([`READ_LIMIT`], aterm's `PROBE_LIMIT`), so a large repository is read
//! whole however long its reads take together ([`crate::procs::run_until`]).
//! It writes nothing. Unlike aterm's probe, which reads the person's global
//! configuration only for its ignore file, the person's global configuration
//! applies to every read (as in their own shell's `git status`), so their
//! global ignore file counts. The probe runs in a process of its own beside
//! the supervisor's loop ([`crate::repo_probe`]).

use std::cell::Cell;
use std::collections::VecDeque;
use std::ffi::{OsStr, OsString};
use std::path::{Component, Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use crate::json::{self, Value};

use crate::procs::{Ran, run_until};

/// How many changed or untracked paths a finding names.
pub const NAMED_PATHS: usize = 3;

/// The most characters of a path or a repository's name a finding keeps (its
/// end, after `…`).
pub const PATH_CHARS: usize = 72;

/// The most characters of a finding Land's act quotes (aterm's
/// `REPO_SUMMARY_UNITS`).
pub const SUMMARY_CHARS: usize = 160;

/// How long each read of a probe may take, from the end of the read before
/// it (the first, from the probe's start): aterm's `PROBE_LIMIT`, the
/// one-minute hang detector. A read cut by it makes the probe
/// [`Probed::Cut`].
pub const READ_LIMIT: Duration = Duration::from_secs(60);

/// The most folders [`session_folders`] names.
pub const SESSION_FOLDERS: usize = 32;

/// The most repositories one probe reads, superprojects and submodules
/// included: past that the rest are not read.
pub const PROBE_REPOS: usize = 96;

/// The file-changing tools whose `file_path` (`notebook_path` for a
/// notebook) names a folder the session changed ([`session_folders`]).
const CHANGING_TOOLS: &[(&str, &str)] = &[
    ("Edit", "file_path"),
    ("MultiEdit", "file_path"),
    ("Write", "file_path"),
    ("NotebookEdit", "notebook_path"),
];

/// A detached `HEAD`'s branch word: git's own, which its stash lines name.
pub const DETACHED: &str = "(no branch)";

/// The words Land's act opens a finding with (aterm's `REPO_HEAD`).
const REPO_HEAD: &str = "Your repository still holds work no remote main has";

/// The words Land's act asks a finding with (aterm's `REPO_ALL`).
const REPO_ALL: &str = "commit, merge and push it to the remote main first.";

/// What Land's act says of a repository the probe could not read (aterm's
/// `REPO_UNREAD`).
pub const REPO_UNREAD: &str = "Your repository could not be read: show its git status yourself \
                               and land what it holds first.";

/// The per-folder consent services under `$HOME`, by directory name (aterm's
/// `atpkg::protected::HOME_FOLDERS`).
const HOME_FOLDERS: &[&str] = &[
    "Documents",
    "Desktop",
    "Downloads",
    "Pictures",
    "Movies",
    "Music",
];

/// The file-provider domains under `~/Library` (aterm's
/// `atpkg::protected::LIBRARY_DOMAINS`).
const LIBRARY_DOMAINS: &[(&str, &str)] =
    &[("Library", "Mobile Documents"), ("Library", "CloudStorage")];

/// Roots that are guarded wherever `$HOME` is (aterm's
/// `atpkg::protected::ABSOLUTE_ROOTS`).
const ABSOLUTE_ROOTS: &[&str] = &["/Volumes"];

/// The developer directories whose `usr/bin/git` stands behind Apple's
/// `/usr/bin/git` stub, beside `DEVELOPER_DIR` and the `xcode-select` link.
const DEVELOPER_DIRS: &[&str] = &[
    "/Applications/Xcode.app/Contents/Developer",
    "/Library/Developer/CommandLineTools",
];

/// The link `xcode-select` keeps the selected developer directory in.
const XCODE_SELECT_LINK: &str = "/var/db/xcode_select_link";

/// What one repository holds that no remote main has ([`probe`]).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RepoWork {
    /// The repository's top-level directory, as git names it ([`printable`]).
    pub repo: String,
    /// Tracked paths with changes, staged or not.
    pub changed: usize,
    /// Untracked paths git does not ignore.
    pub untracked: usize,
    /// Stash entries made on `HEAD`'s branch.
    pub stashes: usize,
    /// Commits on `HEAD` no remote-tracking ref contains.
    pub unpushed: usize,
    /// Commits on `HEAD` the remote main ([`Self::main_ref`]) does not contain.
    pub unlanded: usize,
    /// The remote main [`Self::unlanded`] counts against (`origin/main`).
    pub main_ref: String,
    /// The first [`NAMED_PATHS`] changed or untracked paths ([`printable`]).
    pub paths: Vec<String>,
}

impl RepoWork {
    /// Whether anything was found.
    #[must_use]
    pub const fn found(&self) -> bool {
        self.changed > 0
            || self.untracked > 0
            || self.stashes > 0
            || self.unpushed > 0
            || self.unlanded > 0
    }

    /// The finding of the repository `repo` from `git status --porcelain=v1
    /// -z` output `status`, the stash lines `stash` made on `HEAD`'s branch
    /// ([`stashes_on`]) and the counts; `None` for nothing.
    #[must_use]
    pub fn of_git(
        repo: &str,
        status: &str,
        stash: &str,
        unpushed: usize,
        unlanded: usize,
        main_ref: &str,
    ) -> Option<Self> {
        let mut work = Self {
            repo: printable(repo.trim_end_matches(['\n', '\r'])),
            stashes: stash.lines().filter(|line| !line.trim().is_empty()).count(),
            unpushed,
            unlanded,
            main_ref: printable(main_ref),
            ..Self::default()
        };
        let mut fields = status.split('\0');
        while let Some(entry) = fields.next() {
            let (Some(code), Some(path)) = (entry.get(..2), entry.get(3..)) else {
                continue;
            };
            if code == "??" {
                work.untracked += 1;
            } else if code == "!!" {
                continue;
            } else {
                work.changed += 1;
                // A rename or a copy names its source in the next field.
                if code.contains(['R', 'C']) {
                    fields.next();
                }
            }
            if work.paths.len() < NAMED_PATHS && !path.is_empty() {
                work.paths.push(printable(path));
            }
        }
        work.found().then_some(work)
    }

    /// The finding in a few words, as aterm's `RepoWork::summary` gives them:
    /// `<repo>: 1 changed, 2 untracked (a, b, c, …), 1 stash, 2 unpushed
    /// commits, 2 commits not on origin/main`.
    #[must_use]
    pub fn summary(&self) -> String {
        let plural = |n: usize, one: &str, many: &str| {
            if n == 1 {
                one.to_string()
            } else {
                many.to_string()
            }
        };
        let mut parts: Vec<String> = Vec::new();
        if self.changed > 0 {
            parts.push(format!("{} changed", self.changed));
        }
        if self.untracked > 0 {
            parts.push(format!("{} untracked", self.untracked));
        }
        let mut text = parts.join(", ");
        if !self.paths.is_empty() {
            let more = if self.changed.saturating_add(self.untracked) > self.paths.len() {
                ", \u{2026}"
            } else {
                ""
            };
            text = format!("{text} ({}{more})", self.paths.join(", "));
        }
        let mut tail: Vec<String> = Vec::new();
        if self.stashes > 0 {
            tail.push(format!(
                "{} {}",
                self.stashes,
                plural(self.stashes, "stash", "stashes")
            ));
        }
        if self.unpushed > 0 {
            tail.push(format!(
                "{} unpushed {}",
                self.unpushed,
                plural(self.unpushed, "commit", "commits")
            ));
        }
        if self.unlanded > 0 {
            tail.push(format!(
                "{} {} not on {}",
                self.unlanded,
                plural(self.unlanded, "commit", "commits"),
                if self.main_ref.is_empty() {
                    "the remote main"
                } else {
                    self.main_ref.as_str()
                }
            ));
        }
        let all: Vec<String> = [text]
            .into_iter()
            .filter(|text| !text.is_empty())
            .chain(tail)
            .collect();
        format!("{}: {}", self.repo, all.join(", "))
    }
}

/// What a probe of the session's repositories read ([`probe`]), as Land
/// weighs it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum RepoRead {
    /// Nothing no remote main has, or nothing the probe may read here (no
    /// repository, a guarded folder).
    #[default]
    Clean,
    /// What the first repository that holds anything holds that no remote
    /// main has.
    Found(RepoWork),
    /// Git refused a read that must answer, or could not be asked (no
    /// `HOME`, no `git` that may run here): what the repository holds is not
    /// known.
    Unread,
}

/// What one probe came to ([`probe`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Probed {
    /// A reading Land weighs.
    Read(RepoRead),
    /// A read did not end within its limit, or the probe could not run to its
    /// end: nothing is known yet, and the repositories are read again later.
    Cut,
}

impl Probed {
    /// The probe's answer as one line, as its process writes it
    /// ([`crate::repo_probe`]): `clean`, `unread`, `cut`, or `found` and the
    /// finding's fields, tab-separated (`found\t<repo>\t<changed>\t<untracked>\t
    /// <stashes>\t<unpushed>\t<unlanded>\t<main ref>[\t<path>…]`). Every text
    /// field is [`printable`], so none holds a tab or a line break.
    #[must_use]
    pub fn line(&self) -> String {
        match self {
            Self::Read(RepoRead::Clean) => "clean".to_string(),
            Self::Read(RepoRead::Unread) => "unread".to_string(),
            Self::Cut => "cut".to_string(),
            Self::Read(RepoRead::Found(work)) => {
                let mut fields = vec![
                    "found".to_string(),
                    printable(&work.repo),
                    work.changed.to_string(),
                    work.untracked.to_string(),
                    work.stashes.to_string(),
                    work.unpushed.to_string(),
                    work.unlanded.to_string(),
                    printable(&work.main_ref),
                ];
                fields.extend(
                    work.paths
                        .iter()
                        .take(NAMED_PATHS)
                        .map(|path| printable(path)),
                );
                fields.join("\t")
            }
        }
    }

    /// The answer [`Probed::line`] wrote; `None` for any other line (a
    /// `found` whose counts are not numbers, that names more than
    /// [`NAMED_PATHS`] paths, or that finds nothing).
    #[must_use]
    pub fn of_line(line: &str) -> Option<Self> {
        let line = line.trim_end_matches(['\n', '\r']);
        match line {
            "clean" => return Some(Self::Read(RepoRead::Clean)),
            "unread" => return Some(Self::Read(RepoRead::Unread)),
            "cut" => return Some(Self::Cut),
            _ => {}
        }
        let mut fields = line.strip_prefix("found\t")?.split('\t');
        let repo = printable(fields.next()?);
        let mut count = || fields.next()?.parse::<usize>().ok();
        let (changed, untracked, stashes, unpushed, unlanded) =
            (count()?, count()?, count()?, count()?, count()?);
        let main_ref = printable(fields.next()?);
        let paths: Vec<String> = fields.map(printable).collect();
        let work = RepoWork {
            repo,
            changed,
            untracked,
            stashes,
            unpushed,
            unlanded,
            main_ref,
            paths,
        };
        (work.found() && work.paths.len() <= NAMED_PATHS)
            .then_some(Self::Read(RepoRead::Found(work)))
    }
}

/// The folders the transcript `tail` (its last lines, oldest first) shows the
/// session changing, newest first, each once, at most [`SESSION_FOLDERS`]:
/// the folder of each file an `Edit`, `MultiEdit`, `Write` or `NotebookEdit`
/// call changed, and the folders a `Bash` call's command made git work in
/// ([`git_work_folders`]); a subagent's included (it works for the session).
/// A row's own `cwd` is not one: the session may only have gone there to
/// read. Only absolute paths count, a relative one in a `Bash` command taken
/// from its row's `cwd`; a line that is not a whole JSON row is passed over.
#[must_use]
pub fn session_folders(tail: &str) -> Vec<PathBuf> {
    fn add(path: &str, folders: &mut Vec<PathBuf>) {
        let path = Path::new(path);
        if path.is_absolute() && !folders.iter().any(|known| known == path) {
            folders.push(path.to_path_buf());
        }
    }
    let mut folders: Vec<PathBuf> = Vec::new();
    for line in tail.lines().rev() {
        if folders.len() >= SESSION_FOLDERS {
            break;
        }
        if !line.contains("\"tool_use\"") {
            continue;
        }
        let Some(row) = json::parse(line) else {
            continue;
        };
        let cwd = row
            .get("cwd")
            .and_then(Value::as_str)
            .map(Path::new)
            .filter(|cwd| cwd.is_absolute());
        let Some(Value::Array(parts)) = row.get("message").and_then(|m| m.get("content")) else {
            continue;
        };
        for part in parts.iter().rev() {
            if part.get("type").and_then(Value::as_str) != Some("tool_use") {
                continue;
            }
            let name = part.get("name").and_then(Value::as_str);
            if name == Some("Bash") {
                let command = part
                    .get("input")
                    .and_then(|input| input.get("command"))
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                for folder in git_work_folders(command, cwd).iter().rev() {
                    if let Some(folder) = folder.to_str() {
                        add(folder, &mut folders);
                    }
                }
                continue;
            }
            let Some((_, key)) = CHANGING_TOOLS.iter().find(|(tool, _)| Some(*tool) == name) else {
                continue;
            };
            if let Some(folder) = part
                .get("input")
                .and_then(|input| input.get(key))
                .and_then(Value::as_str)
                .map(Path::new)
                .and_then(Path::parent)
                .and_then(Path::to_str)
            {
                add(folder, &mut folders);
            }
        }
    }
    folders.truncate(SESSION_FOLDERS);
    folders
}

/// `path` with each `.` dropped and each `..` taking off the part before it,
/// by the words alone (no link is followed).
fn lexical(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for part in path.components() {
        match part {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// The git subcommands that make work a remote main may lack in the folder
/// they run in: commits, merges and their kin, stash entries, and what a push
/// or a pull leaves ([`git_work_folders`]).
const GIT_WORK: &[&str] = &[
    "am",
    "cherry-pick",
    "commit",
    "merge",
    "pull",
    "push",
    "rebase",
    "revert",
    "stash",
];

/// The folders shell `command` made git work in, in order: the folder each
/// git subcommand of [`GIT_WORK`] ran in (its `git -C <folder>`, else the
/// folder the command's `cd <folder>` words moved to, else `cwd`), and the
/// folder each `git worktree add [<option>…] <folder>` made, read word by
/// word (quotes and the separators `;`, `&&`, `||`, `|`, `(` and `)` set
/// apart). A folder the command only went into, or read with git, is not
/// one. A relative folder is taken from the folder it was named in, and is
/// passed over without one, and `.` and `..` are resolved by their words
/// alone; a word with `$`, `~` or a glob is passed over, since only a shell
/// knows what it names, and so is a `cd` to one, after which a relative
/// folder names nothing.
#[must_use]
pub fn git_work_folders(command: &str, cwd: Option<&Path>) -> Vec<PathBuf> {
    let spaced: String = command
        .chars()
        .map(|c| {
            if matches!(c, ';' | '&' | '|' | '(' | ')') {
                ' '
            } else {
                c
            }
        })
        .collect();
    let words: Vec<&str> = spaced
        .split_whitespace()
        .map(|word| word.trim_matches(['\'', '"']))
        .collect();
    let folder = |word: &str, from: Option<&Path>| -> Option<PathBuf> {
        if word.is_empty() || word.starts_with('-') || word.contains(['$', '~', '*', '?', '`']) {
            return None;
        }
        let path = Path::new(word);
        if path.is_absolute() {
            Some(lexical(path))
        } else {
            from.map(|from| lexical(&from.join(path)))
        }
    };
    let mut here = cwd.map(Path::to_path_buf);
    let mut found = Vec::new();
    let mut at = 0;
    while let Some(word) = words.get(at).copied() {
        at += 1;
        if word == "cd" {
            here = words
                .get(at)
                .copied()
                .and_then(|next| folder(next, here.as_deref()));
            continue;
        }
        if word != "git" && !word.ends_with("/git") {
            continue;
        }
        // git's own options before its subcommand: `-C <folder>` moves it,
        // and `-c <name>=<value>` takes a word.
        let mut there = here.clone();
        while let Some(option) = words.get(at).copied().filter(|word| word.starts_with('-')) {
            at += 1;
            match option {
                "-C" => {
                    there = words
                        .get(at)
                        .copied()
                        .and_then(|next| folder(next, there.as_deref()));
                    at += 1;
                }
                "-c" => at += 1,
                _ => {}
            }
        }
        let Some(subcommand) = words.get(at).copied() else {
            break;
        };
        at += 1;
        if GIT_WORK.contains(&subcommand) {
            found.extend(there);
        } else if subcommand == "worktree" && words.get(at).copied() == Some("add") {
            at += 1;
            while let Some(word) = words.get(at).copied() {
                at += 1;
                match word {
                    "-b" | "-B" | "--reason" => at += 1,
                    option if option.starts_with('-') => {}
                    word => {
                        found.extend(folder(word, there.as_deref()));
                        break;
                    }
                }
            }
        }
    }
    found
}

impl RepoRead {
    /// Whether a land may be counted over this reading: only a clean one.
    #[must_use]
    pub const fn is_clean(&self) -> bool {
        matches!(self, Self::Clean)
    }

    /// What Land's act says of the reading where it is not clean (aterm's
    /// `repo_work_suffix` and `REPO_UNREAD`): the finding, cut to
    /// [`SUMMARY_CHARS`], and the ask to land it all; `None` for a clean one.
    #[must_use]
    pub fn ask(&self) -> Option<String> {
        match self {
            Self::Clean => None,
            Self::Found(work) => Some(format!(
                "{REPO_HEAD} ({}): {REPO_ALL}",
                cut(&work.summary(), SUMMARY_CHARS)
            )),
            Self::Unread => Some(REPO_UNREAD.to_string()),
        }
    }

    /// The reading as journal words: `clean`, `unread`, or `found <summary>`.
    #[must_use]
    pub fn words(&self) -> String {
        match self {
            Self::Clean => "clean".to_string(),
            Self::Found(work) => format!("found {}", work.summary()),
            Self::Unread => "unread".to_string(),
        }
    }
}

/// `text` with every control character as `?`, kept to [`PATH_CHARS`]
/// characters (its end, after `…`).
#[must_use]
pub fn printable(text: &str) -> String {
    let clean: String = text
        .chars()
        .map(|c| if c.is_control() { '?' } else { c })
        .collect();
    let n = clean.chars().count();
    if n <= PATH_CHARS {
        return clean;
    }
    let tail: String = clean.chars().skip(n - (PATH_CHARS - 1)).collect();
    format!("\u{2026}{tail}")
}

/// `text` kept to `chars` characters, its cut end marked `…`.
fn cut(text: &str, chars: usize) -> String {
    if text.chars().count() <= chars {
        return text.to_string();
    }
    let kept: String = text.chars().take(chars.saturating_sub(1)).collect();
    format!("{kept}\u{2026}")
}

/// The entries of `list` (`git stash list --format=%gs`) stashed on `branch`
/// (`None`: a detached `HEAD`, [`DETACHED`]), one per line.
#[must_use]
pub fn stashes_on(list: &str, branch: Option<&str>) -> String {
    let name = branch.unwrap_or(DETACHED);
    let (wip, on) = (format!("WIP on {name}: "), format!("On {name}: "));
    list.lines()
        .filter(|line| line.starts_with(&wip) || line.starts_with(&on))
        .map(|line| format!("{line}\n"))
        .collect()
}

/// Whether two path components name one folder before any look: equal, or,
/// on macOS, equal but for ASCII case (its volumes reserve case aliases).
fn component_eq(left: &OsStr, right: &OsStr) -> bool {
    if left == right {
        return true;
    }
    cfg!(target_os = "macos")
        && left
            .as_encoded_bytes()
            .eq_ignore_ascii_case(right.as_encoded_bytes())
}

/// The components of `path` past `root`, where `path` is `root` or under it.
fn under<'a>(root: &Path, path: &'a Path) -> Option<std::path::Components<'a>> {
    let mut parts = path.components();
    for expected in root.components() {
        if !component_eq(expected.as_os_str(), parts.next()?.as_os_str()) {
            return None;
        }
    }
    Some(parts)
}

/// Whether `path` lies at or under a folder macOS guards with a consent
/// dialog (aterm's `atpkg::protected::under_protected_root`, lexically: it
/// reads no file): `/Volumes`, the per-folder services under `home`
/// ([`HOME_FOLDERS`]) and its file-provider domains ([`LIBRARY_DOMAINS`]). A
/// path that climbs out of `home` (`..`) is taken as guarded.
#[must_use]
pub fn guarded(home: &Path, path: &Path) -> bool {
    if ABSOLUTE_ROOTS
        .iter()
        .any(|root| under(Path::new(root), path).is_some())
    {
        return true;
    }
    let Some(rest) = under(home, path) else {
        return false;
    };
    if rest
        .clone()
        .any(|part| matches!(part, Component::ParentDir))
    {
        return true;
    }
    let mut names = rest.filter_map(|part| match part {
        Component::Normal(name) => Some(name),
        _ => None,
    });
    let Some(first) = names.next() else {
        return false;
    };
    if HOME_FOLDERS
        .iter()
        .any(|folder| component_eq(first, OsStr::new(folder)))
    {
        return true;
    }
    let Some(second) = names.next() else {
        return false;
    };
    LIBRARY_DOMAINS.iter().any(|(library, domain)| {
        component_eq(first, OsStr::new(library)) && component_eq(second, OsStr::new(domain))
    })
}

/// Whether `path` is a non-empty file someone may execute.
fn executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::metadata(path).is_ok_and(|meta| {
        meta.is_file() && meta.len() > 0 && meta.permissions().mode() & 0o111 != 0
    })
}

/// The `git` a probe runs: the first executable `git` in an absolute entry of
/// `path`, by its absolute path. On macOS Apple's `/usr/bin/git` stub raises
/// the developer tools' install dialog where they are missing, so a `git`
/// that is that stub runs only where a developer directory carries its own
/// `usr/bin/git` (`DEVELOPER_DIR`, the `xcode-select` link, Xcode, the
/// Command Line Tools); `None` where no `git` may run.
#[must_use]
pub fn git_program(path: &OsStr) -> Option<PathBuf> {
    let candidate = std::env::split_paths(path)
        .filter(|dir| dir.is_absolute())
        .map(|dir| dir.join("git"))
        .find(|candidate| executable(candidate))?;
    if !cfg!(target_os = "macos") {
        return Some(candidate);
    }
    let stub = Path::new("/usr/bin/git");
    let is_stub = match (candidate.canonicalize(), stub.canonicalize()) {
        (Ok(selected), Ok(stub)) => selected == stub,
        (Ok(_), Err(_)) => false,
        (Err(_), _) => return None,
    };
    if !is_stub {
        return Some(candidate);
    }
    let developer = std::env::var_os("DEVELOPER_DIR").map(PathBuf::from);
    let selected = std::fs::read_link(XCODE_SELECT_LINK).ok();
    let carried = developer
        .into_iter()
        .chain(selected)
        .chain(DEVELOPER_DIRS.iter().map(PathBuf::from))
        .filter(|dir| dir.is_absolute())
        .map(|dir| dir.join("usr/bin/git"))
        .filter(|git| {
            git.canonicalize()
                .is_ok_and(|real| !real.starts_with("/usr/bin"))
        })
        .any(|git| executable(&git));
    carried.then_some(candidate)
}

/// What one read gave.
enum Got {
    /// Git answered: its output, read lossily.
    Out(String),
    /// Git refused (it exited with a failure).
    Refused,
    /// The read did not start, did not end by the deadline, or a signal ended
    /// it.
    Cut,
}

/// Why a read the probe needed did not answer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Unanswered {
    /// Git refused it: [`RepoRead::Unread`].
    Refused,
    /// It did not start, did not end within its limit, or a signal ended it:
    /// [`Probed::Cut`].
    Cut,
}

/// The probe's `git`, with what each read runs under.
struct Git<'a> {
    /// The program ([`git_program`]).
    program: PathBuf,
    /// The `PATH` it runs with.
    path: OsString,
    /// The person's home folder, as `HOME`.
    home: &'a Path,
    /// `XDG_CONFIG_HOME`, where set to an absolute folder: git's own ignore
    /// file is read from it.
    xdg: Option<PathBuf>,
    /// `DEVELOPER_DIR`, where set: Apple's `git` stub runs the developer
    /// directory it names, the one [`git_program`] judged.
    developer: Option<OsString>,
    /// How long each read may take, from the end of the one before it.
    each: Duration,
    /// When the next read must end: [`Git::each`] from the end of the read
    /// before it, the first from the probe's start.
    deadline: Cell<Instant>,
}

/// How a repository is read ([`Git::repo`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    /// One a folder names, or a superproject: everything counted.
    Worked,
    /// A submodule reached from its superproject: only its commits no remote
    /// has.
    Submodule,
}

impl Git<'_> {
    /// One read of `git <args>` in `dir`, within [`Git::deadline`]; the next
    /// read's deadline is [`Git::each`] from its end.
    fn read(&self, dir: &Path, args: &[&str]) -> Got {
        let mut command = Command::new(&self.program);
        command
            .current_dir(dir)
            .env_clear()
            .env("PATH", &self.path)
            .env("HOME", self.home)
            .env("LC_ALL", "C")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_OPTIONAL_LOCKS", "0")
            .env("GIT_TERMINAL_PROMPT", "0")
            .args([
                "--no-pager",
                "-c",
                "core.fsmonitor=false",
                "-c",
                "core.hooksPath=/dev/null",
            ])
            .args(args);
        if let Some(xdg) = &self.xdg {
            command.env("XDG_CONFIG_HOME", xdg);
        }
        if let Some(developer) = &self.developer {
            command.env("DEVELOPER_DIR", developer);
        }
        let ran = run_until(&mut command, self.deadline.get());
        if let Some(next) = Instant::now().checked_add(self.each) {
            self.deadline.set(next);
        }
        match ran {
            Ran::Ended { code: Some(0), out } => {
                Got::Out(String::from_utf8_lossy(&out).into_owned())
            }
            Ran::Ended { code: Some(_), .. } => Got::Refused,
            Ran::Ended { code: None, .. } | Ran::NotStarted | Ran::Cut => Got::Cut,
        }
    }

    /// A read that must answer: its output, or why it did not.
    fn answer(&self, dir: &Path, args: &[&str]) -> Result<String, Unanswered> {
        match self.read(dir, args) {
            Got::Out(out) => Ok(out),
            Got::Refused => Err(Unanswered::Refused),
            Got::Cut => Err(Unanswered::Cut),
        }
    }

    /// A count that must answer (`rev-list --count`).
    fn count(&self, dir: &Path, args: &[&str]) -> Result<usize, Unanswered> {
        self.answer(dir, args)?
            .trim()
            .parse()
            .map_err(|_| Unanswered::Refused)
    }

    /// The remote main commits are counted against, in full
    /// (`refs/remotes/origin/main`): `origin/HEAD`'s target, else
    /// `origin/main`, else `origin/master`, the first that names a commit;
    /// `None` where none does.
    fn main_ref(&self, root: &Path) -> Result<Option<String>, Unanswered> {
        let head = match self.read(
            root,
            &["symbolic-ref", "--quiet", "refs/remotes/origin/HEAD"],
        ) {
            Got::Out(target) => {
                Some(target.trim().to_string()).filter(|target| target.starts_with("refs/remotes/"))
            }
            Got::Refused => None,
            Got::Cut => return Err(Unanswered::Cut),
        };
        let candidates = head
            .into_iter()
            .chain(["refs/remotes/origin/main", "refs/remotes/origin/master"].map(str::to_string));
        for full in candidates {
            let commit = format!("{full}^{{commit}}");
            match self.read(root, &["rev-parse", "--verify", "--quiet", &commit]) {
                Got::Out(_) => return Ok(Some(full)),
                Got::Refused => {}
                Got::Cut => return Err(Unanswered::Cut),
            }
        }
        Ok(None)
    }

    /// The top-level folder of the repository `dir` is in, where it is one
    /// the probe reads: `None` for a folder in no repository, or a top level
    /// git names that is not absolute or that lies under a guarded folder.
    fn top(&self, dir: &Path) -> Result<Option<PathBuf>, Unanswered> {
        let top = match self.read(dir, &["rev-parse", "--show-toplevel"]) {
            Got::Out(top) => top.trim_end_matches(['\n', '\r']).to_string(),
            // Not in a repository: nothing to read.
            Got::Refused => return Ok(None),
            Got::Cut => return Err(Unanswered::Cut),
        };
        let root = PathBuf::from(&top);
        Ok((!top.is_empty() && root.is_absolute() && !guarded(self.home, &root)).then_some(root))
    }

    /// The superproject's top-level folder where the repository at `root` is
    /// a submodule of one; `None` where it is none, or git cannot say.
    fn superproject(&self, root: &Path) -> Result<Option<PathBuf>, Unanswered> {
        match self.read(root, &["rev-parse", "--show-superproject-working-tree"]) {
            Got::Out(top) => {
                let top = PathBuf::from(top.trim_end_matches(['\n', '\r']));
                Ok(top.is_absolute().then_some(top))
            }
            Got::Refused => Ok(None),
            Got::Cut => Err(Unanswered::Cut),
        }
    }

    /// The populated submodules of the repository at `root`: the folders its
    /// `.gitmodules` names (`submodule.<name>.path`), relative and climbing
    /// nowhere, that hold a checkout (`.git`). None where it has no
    /// `.gitmodules`, or git cannot read it.
    fn submodules(&self, root: &Path) -> Result<Vec<PathBuf>, Unanswered> {
        let listed = match self.read(
            root,
            &[
                "config",
                "--file",
                ".gitmodules",
                "-z",
                "--get-regexp",
                "^submodule\\..*\\.path$",
            ],
        ) {
            Got::Out(listed) => listed,
            Got::Refused => return Ok(Vec::new()),
            Got::Cut => return Err(Unanswered::Cut),
        };
        Ok(listed
            .split('\0')
            .filter_map(|entry| entry.split_once('\n').map(|(_, path)| Path::new(path)))
            .filter(|path| {
                !path.as_os_str().is_empty()
                    && path
                        .components()
                        .all(|part| matches!(part, Component::Normal(_)))
            })
            .map(|path| root.join(path))
            .filter(|folder| folder.join(".git").exists())
            .collect())
    }

    /// Whether the repository at `root` has anywhere to push: a remote
    /// configured, or a remote-tracking ref (a remote configured but never
    /// fetched from or pushed to has none).
    fn has_remote(&self, root: &Path) -> Result<bool, Unanswered> {
        let tracking = self.answer(
            root,
            &[
                "for-each-ref",
                "--count=1",
                "--format=remote",
                "refs/remotes",
            ],
        )?;
        let configured = self.answer(root, &["remote"])?;
        Ok(!tracking.trim().is_empty() || !configured.trim().is_empty())
    }

    /// Whether `HEAD` of the repository at `root` names a commit (a new one
    /// has none yet).
    fn has_head(&self, root: &Path) -> Result<bool, Unanswered> {
        match self.read(root, &["rev-parse", "--verify", "--quiet", "HEAD"]) {
            Got::Out(head) => Ok(!head.trim().is_empty()),
            Got::Refused => Ok(false),
            Got::Cut => Err(Unanswered::Cut),
        }
    }

    /// The reads of the repository at `root` (its top level, as git named
    /// it), as `kind` reads it.
    fn repo(&self, root: &Path, kind: Kind) -> Result<RepoRead, Unanswered> {
        let top = root.to_string_lossy();
        if kind == Kind::Submodule {
            let unpushed = if self.has_head(root)? && self.has_remote(root)? {
                self.count(root, &["rev-list", "--count", "HEAD", "--not", "--remotes"])?
            } else {
                0
            };
            return Ok(RepoWork::of_git(&top, "", "", unpushed, 0, "")
                .map_or(RepoRead::Clean, RepoRead::Found));
        }
        let status = self.answer(
            root,
            &["status", "--porcelain=v1", "-z", "--untracked-files=normal"],
        )?;
        let branch = match self.read(root, &["symbolic-ref", "--quiet", "--short", "HEAD"]) {
            Got::Out(branch) => Some(branch.trim().to_string()).filter(|b| !b.is_empty()),
            // A detached `HEAD`.
            Got::Refused => None,
            Got::Cut => return Err(Unanswered::Cut),
        };
        let listed = self.answer(root, &["stash", "list", "--format=%gs"])?;
        let stash = stashes_on(&listed, branch.as_deref());
        let remote = self.has_remote(root)?;
        let (mut unpushed, mut unlanded, mut main) = (0, 0, String::new());
        if self.has_head(root)? {
            if remote {
                unpushed =
                    self.count(root, &["rev-list", "--count", "HEAD", "--not", "--remotes"])?;
            }
            if let Some(full) = self.main_ref(root)? {
                unlanded = self.count(root, &["rev-list", "--count", &format!("{full}..HEAD")])?;
                main = full
                    .strip_prefix("refs/remotes/")
                    .unwrap_or(&full)
                    .to_string();
            }
        }
        Ok(
            RepoWork::of_git(&top, &status, &stash, unpushed, unlanded, &main)
                .map_or(RepoRead::Clean, RepoRead::Found),
        )
    }

    /// Every repository `folders` name, with their superprojects and
    /// submodules, at most [`PROBE_REPOS`], each once ([`probe`]).
    fn walk(&self, folders: &[PathBuf]) -> Probed {
        fn heard(why: Unanswered, refused: &mut bool, cut: &mut bool) {
            match why {
                Unanswered::Refused => *refused = true,
                Unanswered::Cut => *cut = true,
            }
        }
        let mut queue: VecDeque<(PathBuf, Kind)> = folders
            .iter()
            .map(|folder| (folder.clone(), Kind::Worked))
            .collect();
        let mut read: Vec<PathBuf> = Vec::new();
        let (mut refused, mut cut) = (false, false);
        while let Some((folder, kind)) = queue.pop_front() {
            if read.len() >= PROBE_REPOS {
                break;
            }
            if !folder.is_absolute() || guarded(self.home, &folder) || !folder.is_dir() {
                continue;
            }
            let root = match self.top(&folder) {
                Ok(Some(root)) => root,
                Ok(None) => continue,
                Err(why) => {
                    heard(why, &mut refused, &mut cut);
                    continue;
                }
            };
            if read.contains(&root) {
                continue;
            }
            read.push(root.clone());
            match self.superproject(&root) {
                Ok(Some(outer)) => queue.push_back((outer, Kind::Worked)),
                Ok(None) => {}
                Err(why) => heard(why, &mut refused, &mut cut),
            }
            match self.repo(&root, kind) {
                Ok(RepoRead::Found(work)) => return Probed::Read(RepoRead::Found(work)),
                Ok(RepoRead::Clean) => {}
                Ok(RepoRead::Unread) => refused = true,
                Err(why) => heard(why, &mut refused, &mut cut),
            }
            match self.submodules(&root) {
                Ok(inner) => queue.extend(inner.into_iter().map(|sub| (sub, Kind::Submodule))),
                Err(why) => heard(why, &mut refused, &mut cut),
            }
        }
        if cut {
            Probed::Cut
        } else if refused {
            Probed::Read(RepoRead::Unread)
        } else {
            Probed::Read(RepoRead::Clean)
        }
    }
}

/// What the repositories the folders `folders` are in hold that no remote
/// main has, with the superprojects and populated submodules of each, read
/// with `git`, each read within `each` of the end of the read before it (the
/// first, of the call). `home` is the person's home folder (`HOME`), and
/// `git` is looked for on the harness's own `PATH`. Where `home` is not given
/// or not absolute, or no `git` may run there ([`git_program`]), nothing can
/// be read and the reading is [`RepoRead::Unread`], never clean.
#[must_use]
pub fn probe(folders: &[PathBuf], home: Option<&Path>, each: Duration) -> Probed {
    probe_on(folders, home, std::env::var_os("PATH"), each)
}

/// [`probe`], with `git` looked for on `path` ([`git_program`]). Where `home`
/// is not given or not absolute, or no `git` may run on `path`, nothing can
/// be read and the reading is [`RepoRead::Unread`], never clean; a folder
/// that is gone or lies under a folder macOS guards ([`guarded`]) is passed
/// over.
fn probe_on(
    folders: &[PathBuf],
    home: Option<&Path>,
    path: Option<OsString>,
    each: Duration,
) -> Probed {
    let unread = Probed::Read(RepoRead::Unread);
    let Some(home) = home.filter(|home| home.is_absolute()) else {
        return unread;
    };
    let Some(path) = path else {
        return unread;
    };
    let Some(program) = git_program(&path) else {
        return unread;
    };
    let Some(deadline) = Instant::now().checked_add(each) else {
        return unread;
    };
    let git = Git {
        program,
        path,
        home,
        xdg: std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .filter(|xdg| xdg.is_absolute()),
        developer: std::env::var_os("DEVELOPER_DIR"),
        each,
        deadline: Cell::new(deadline),
    };
    git.walk(folders)
}

#[cfg(test)]
#[path = "repo_tests.rs"]
mod tests;
