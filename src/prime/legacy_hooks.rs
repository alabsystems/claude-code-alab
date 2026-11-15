// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Retired aterm hook removal through the same transaction as Claude's defaults.

use std::ffi::OsStr;
use std::io::{self, Read as _};
use std::os::unix::fs::OpenOptionsExt as _;
use std::path::{Path, PathBuf};

use super::settings_json::Json;
use super::write;

const LINK_HOOK_FORMS: [&str; 2] = ["link hook run", "hook run"];
const ATERM_EXE_NAMES: [&str; 3] = ["aterm", "aterm-link", "aterm-cli"];
const SH_META: [char; 13] = [
    ';', '|', '&', '$', '`', '\'', '"', '<', '>', '(', ')', '\\', '\n',
];
const RETIRED_INSTALLER_MARK: &str = "# aterm-harness";
const HARNESS_BACKUP_SUFFIX: &str = ".aterm-harness.orig";
const READ_MAX: u64 = 16 * 1024 * 1024;

/// The completed legacy settings cleanup, preserving the original native backup path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Removal {
    /// No marked hook or displaced status-line entry remains.
    Nothing,
    /// Legacy entries were retired with the original settings bytes backed up.
    Removed {
        /// The retired hook and status-line entry count.
        removed: usize,
        /// The native path retaining the original bytes.
        backup: PathBuf,
    },
}

/// Remove only entries written by aterm, preserving foreign settings and the
/// installer's original statusline. No managed content means no lock or write.
pub(super) fn remove(config: &Path) -> Result<Removal, String> {
    remove_with_backup_bound(config, super::MAX_BACKUP_PATH_BYTES)
}

fn remove_with_backup_bound(config: &Path, backup_path_bound: usize) -> Result<Removal, String> {
    if !config.is_absolute() {
        return Err("legacy hook removal requires an absolute configuration path".into());
    }
    let path = config.join("settings.json");
    let before = read_text(&path).map_err(|error| format!("{}: {error}", path.display()))?;
    if !needs_removal(before.as_deref(), &path)? {
        return Ok(Removal::Nothing);
    }
    let Some(lock) = write::Lock::acquire(&path).map_err(|error| error.to_string())? else {
        return Ok(Removal::Nothing);
    };
    if lock.backup_path_bound() > backup_path_bound {
        return Err("legacy backup path exceeds the result bound; settings left unchanged".into());
    }
    let selected = lock
        .selected_auxiliary(&path)
        .map_err(|error| error.to_string())?;
    let backups = Backups {
        lock: &lock,
        selected,
    };
    let changed = write::rewrite(&path, &lock, true, |text| {
        Ok(edit(text, &path, &backups)?
            .map(|(doc, removed)| (format!("{}\n", doc.render()), removed)))
    })
    .map_err(|error| format!("{}: {}", path.display(), error.words()))?;
    match changed {
        Some((removed, Some(backup))) => Ok(Removal::Removed { removed, backup }),
        Some((_, None)) => Err("legacy hook removal did not retain the original settings".into()),
        None => Ok(Removal::Nothing),
    }
}

fn needs_removal(text: Option<&str>, path: &Path) -> Result<bool, String> {
    let Some(text) = text else { return Ok(false) };
    let mut doc = Json::parse(text).map_err(|error| format!("{}: {error}", path.display()))?;
    let removed =
        strip_marked_hooks(&mut doc).map_err(|error| format!("{}: {error}", path.display()))?;
    Ok(removed > 0 || doc.get("statusLine").is_some_and(is_harness_statusline))
}

fn edit(
    text: Option<&str>,
    path: &Path,
    backups: &Backups<'_>,
) -> Result<Option<(Json, usize)>, String> {
    let Some(text) = text else { return Ok(None) };
    let mut doc = Json::parse(text).map_err(|error| format!("{}: {error}", path.display()))?;
    let mut removed =
        strip_marked_hooks(&mut doc).map_err(|error| format!("{}: {error}", path.display()))?;
    if restore_harness_statusline(&mut doc, backups) {
        removed += 1;
    }
    Ok((removed > 0).then_some((doc, removed)))
}

fn read_text(path: &Path) -> io::Result<Option<String>> {
    let file = match std::fs::File::options()
        .read(true)
        .custom_flags(libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(path)
    {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    let metadata = file.metadata()?;
    if !metadata.is_file() || metadata.len() > READ_MAX {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "legacy settings source is not a bounded regular file",
        ));
    }
    let mut text = String::new();
    file.take(READ_MAX + 1).read_to_string(&mut text)?;
    if text.len() as u64 > READ_MAX {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "legacy settings source exceeds its read budget",
        ));
    }
    Ok(Some(text))
}

fn is_aterm_hook_command(command: &str) -> bool {
    let command = command.trim();
    if is_harness_statusline_command(command) || bare_path_hook(command) {
        return true;
    }
    let Some((exe, rest)) = split_first_word(command) else {
        return false;
    };
    let name = exe.rsplit('/').next().unwrap_or(&exe);
    ATERM_EXE_NAMES.contains(&name) && is_link_hook_form(rest)
}

fn is_link_hook_form(rest: &str) -> bool {
    LINK_HOOK_FORMS.iter().any(|form| {
        rest.strip_prefix(form)
            .is_some_and(|tail| tail.is_empty() || tail.starts_with(' '))
    })
}

fn bare_path_hook(command: &str) -> bool {
    if !command.starts_with('/') {
        return false;
    }
    for (i, _) in command.match_indices('/') {
        if command[..i].contains(SH_META) {
            return false;
        }
        let after = &command[i + 1..];
        if ATERM_EXE_NAMES.iter().any(|name| {
            after
                .strip_prefix(name)
                .and_then(|rest| rest.strip_prefix(' '))
                .is_some_and(|rest| is_link_hook_form(rest.trim_start()))
        }) {
            return true;
        }
    }
    false
}

fn split_first_word(command: &str) -> Option<(String, &str)> {
    if let Some(rest) = command.strip_prefix('\'') {
        let mut word = String::new();
        let mut it = rest.char_indices();
        while let Some((i, c)) = it.next() {
            if c == '\'' {
                if rest[i + 1..].starts_with("\\''") {
                    word.push('\'');
                    it.nth(2);
                    continue;
                }
                return Some((word, rest[i + 1..].trim_start()));
            }
            word.push(c);
        }
        None
    } else {
        match command.split_once(' ') {
            Some((word, rest)) => Some((word.to_string(), rest.trim_start())),
            None => Some((command.to_string(), "")),
        }
    }
}

fn strip_marked_hooks(doc: &mut Json) -> Result<usize, String> {
    let Some(hooks) = doc.get_mut("hooks") else {
        return Ok(0);
    };
    let events = hooks
        .as_object_mut()
        .ok_or_else(|| "`hooks` is not a JSON object".to_string())?;
    let had_events = !events.is_empty();
    let mut removed = 0;
    let mut event_emptied = Vec::with_capacity(events.len());
    for (event, groups) in events.iter_mut() {
        let groups = groups
            .as_array_mut()
            .ok_or_else(|| format!("`hooks.{event}` is not a JSON array"))?;
        let had_groups = !groups.is_empty();
        let mut group_emptied = Vec::with_capacity(groups.len());
        for group in groups.iter_mut() {
            let mut emptied = false;
            if let Some(entries) = group.get_mut("hooks").and_then(Json::as_array_mut) {
                let before = entries.len();
                entries.retain(|entry| {
                    !entry
                        .get("command")
                        .and_then(Json::as_str)
                        .is_some_and(is_aterm_hook_command)
                });
                let taken = before - entries.len();
                removed += taken;
                emptied = taken > 0 && entries.is_empty();
            }
            group_emptied.push(emptied);
        }
        let mut index = 0;
        groups.retain(|_| {
            let drop = group_emptied[index];
            index += 1;
            !drop
        });
        event_emptied.push(had_groups && groups.is_empty());
    }
    let mut index = 0;
    events.retain(|_| {
        let drop = event_emptied[index];
        index += 1;
        !drop
    });
    let emptied = had_events
        && doc
            .get("hooks")
            .and_then(Json::as_object)
            .is_some_and(<[_]>::is_empty);
    if emptied && let Some(top) = doc.as_object_mut() {
        top.retain(|(key, _)| key != "hooks");
    }
    Ok(removed)
}

fn is_harness_statusline_command(command: &str) -> bool {
    command
        .trim()
        .strip_suffix(RETIRED_INSTALLER_MARK)
        .is_some_and(|head| head.ends_with(char::is_whitespace))
}

fn is_harness_statusline(value: &Json) -> bool {
    value
        .get("command")
        .and_then(Json::as_str)
        .is_some_and(is_harness_statusline_command)
}

struct Backups<'a> {
    lock: &'a write::Lock,
    selected: write::Auxiliary,
}

impl Backups<'_> {
    fn original_statusline(&self) -> Option<Json> {
        let statusline = |text: io::Result<Option<String>>| {
            let original = Json::parse(&text.ok()??).ok()?;
            original
                .get("statusLine")
                .filter(|value| !is_harness_statusline(value))
                .cloned()
        };
        statusline(self.selected.read(OsStr::new(HARNESS_BACKUP_SUFFIX)))
            .or_else(|| statusline(self.lock.read_auxiliary(OsStr::new(HARNESS_BACKUP_SUFFIX))))
    }
}

fn restore_harness_statusline(doc: &mut Json, backups: &Backups<'_>) -> bool {
    let Some(harness) = doc
        .get("statusLine")
        .filter(|value| is_harness_statusline(value))
    else {
        return false;
    };
    let state = harness
        .get("command")
        .and_then(Json::as_str)
        .and_then(harness_state_of)
        .filter(|state| state.join("plugin").is_dir());
    let copied = backups.original_statusline();
    let original = match state {
        Some(state) => read_text(&state.join("statusline.user"))
            .ok()
            .flatten()
            .map(|text| text.trim_end_matches('\n').to_string())
            .filter(|command| !command.trim().is_empty() && !is_harness_statusline_command(command))
            .map(|command| match copied {
                Some(value) if value.get("command").and_then(Json::as_str) == Some(&command) => {
                    value
                }
                _ => Json::Object(vec![
                    ("type".into(), Json::Str("command".into())),
                    ("command".into(), Json::Str(command)),
                ]),
            }),
        None => copied,
    };
    let Some(top) = doc.as_object_mut() else {
        return false;
    };
    match original {
        Some(value) => {
            if let Some((_, slot)) = top.iter_mut().find(|(key, _)| key == "statusLine") {
                *slot = value;
            }
        }
        None => top.retain(|(key, _)| key != "statusLine"),
    }
    true
}

fn harness_state_of(command: &str) -> Option<PathBuf> {
    let (bridge, _) = command.trim().strip_prefix("sh '")?.split_once('\'')?;
    bridge
        .strip_suffix("/plugin/hooks/bridge.sh")
        .filter(|state| !state.is_empty())
        .map(PathBuf::from)
}

#[cfg(test)]
#[path = "legacy_hooks_tests.rs"]
mod tests;
