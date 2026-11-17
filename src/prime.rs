// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Claude-owned settings initialization and legacy adapter retirement. No lifecycle authority.

use std::path::{Path, PathBuf};

mod legacy_hooks;
pub use legacy_hooks::Removal as LegacyWrite;
mod inspection;
mod settings_json;
mod write;
pub(crate) use inspection::{DefaultState, Meaning, Setting, settings_state, would_add};
pub(crate) use settings_json::Json;

/// Metadata words for implemented settings operations, independent of launch ownership.
pub(crate) const CAPABILITIES: &[&str] = &["claude:settings-defaults", "claude:legacy-hooks"];
/// Maximum raw native backup path in a legacy result; no path is truncated or made lossy.
pub const MAX_BACKUP_PATH_BYTES: usize = 64 * 1024;
/// Complete legacy machine row, including a hex path and fixed header/count.
pub const MAX_RESULT_BYTES: usize = 2 * MAX_BACKUP_PATH_BYTES + 128;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Operation {
    SettingsDefaults,
    LegacyHooks,
}

fn parse(words: &[std::ffi::OsString]) -> Option<Result<(Operation, PathBuf, PathBuf), String>> {
    if words.first().is_none_or(|word| word != "prime") {
        return None;
    }
    Some(match words {
        [_, action, home_flag, home, config_flag, config]
            if home_flag == "--home" && config_flag == "--config" &&
                (action == "settings-defaults" || action == "legacy-hooks") => {
            let home = PathBuf::from(home);
            let config = PathBuf::from(config);
            if home.is_absolute() && config.is_absolute() {
                let operation = if action == "settings-defaults" { Operation::SettingsDefaults } else { Operation::LegacyHooks };
                Ok((operation, home, config))
            } else {
                Err("priming requires absolute home and configuration paths".into())
            }
        }
        _ => Err("expected prime <settings-defaults|legacy-hooks> --home <absolute-path> --config <absolute-path>".into()),
    })
}

/// The completed operation whose closed result the native caller receives.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PrimeWrite {
    /// Unset-default initialization.
    SettingsDefaults(DefaultsWrite),
    /// Removal of legacy adapter entries, with the original backup path.
    LegacyHooks(LegacyWrite),
}

pub(crate) fn entry(words: &[std::ffi::OsString]) -> Option<Result<PrimeWrite, String>> {
    parse(words).map(|request| {
        request.and_then(|(operation, home, config)| match operation {
            Operation::SettingsDefaults => {
                settings_defaults(&home, &config).map(PrimeWrite::SettingsDefaults)
            }
            Operation::LegacyHooks => retire_legacy_hooks(&config).map(PrimeWrite::LegacyHooks),
        })
    })
}

/// One versioned ASCII result, with no vendor schema. Default initialization keeps
/// its original three rows. Legacy cleanup includes a lossless native-byte hex path.
/// # Errors
/// A legacy changed result has zero affected entries, a relative backup path, or
/// a backup path containing NUL or exceeding the closed native-byte bound.
pub fn response(effect: &PrimeWrite) -> Result<String, String> {
    let row = match effect {
        PrimeWrite::SettingsDefaults(effect) => match effect {
            DefaultsWrite::Nothing => "OK prime=1 operation=settings-defaults changed=0 backup=0\n",
            DefaultsWrite::Added { backup: None, .. } => {
                "OK prime=1 operation=settings-defaults changed=1 backup=0\n"
            }
            DefaultsWrite::Added {
                backup: Some(_), ..
            } => "OK prime=1 operation=settings-defaults changed=1 backup=1\n",
        },
        PrimeWrite::LegacyHooks(LegacyWrite::Nothing) => {
            "OK prime=1 operation=legacy-hooks affected=0 backup=none\n"
        }
        PrimeWrite::LegacyHooks(LegacyWrite::Removed { removed, backup }) => {
            use std::os::unix::ffi::OsStrExt as _;
            let bytes = backup.as_os_str().as_bytes();
            if *removed == 0
                || !backup.is_absolute()
                || bytes.contains(&0)
                || bytes.len() > MAX_BACKUP_PATH_BYTES
            {
                return Err("legacy cleanup result exceeds its closed backup contract".into());
            }
            let mut row = format!("OK prime=1 operation=legacy-hooks affected={removed} backup=");
            const HEX: &[u8; 16] = b"0123456789abcdef";
            for &byte in bytes {
                row.push(char::from(HEX[usize::from(byte >> 4)]));
                row.push(char::from(HEX[usize::from(byte & 15)]));
            }
            row.push('\n');
            return Ok(row);
        }
    };
    Ok(row.into())
}

/// Retire only legacy aterm hook/status-line adapters in the selected configuration.
/// A clean or absent settings file creates no lock or backup. The same held-directory
/// transaction and original-permission snapshot protect this operation and defaults.
/// # Errors
/// The configuration is relative, settings are invalid or exceed the read bound,
/// the transaction is busy, auxiliary custody changes, or a guarded write cannot finish.
pub fn retire_legacy_hooks(config: &Path) -> Result<LegacyWrite, String> {
    legacy_hooks::remove(config)
}

#[cfg(test)]
#[path = "prime_response_tests.rs"]
mod response_tests;

/// The settings initializer's effect, after its guarded write completed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DefaultsWrite {
    /// No settings directory exists, or each admitted default is already set.
    Nothing,
    /// Unset values were added, retaining the original bytes in a separate backup.
    Added {
        /// The keys added by this operation.
        keys: Vec<&'static str>,
        /// The original file, when there was one.
        backup: Option<PathBuf>,
    },
}

/// Initialize Claude's unset defaults in the explicitly selected configuration home.
/// `home` supplies `.claude.json`; `config` supplies `settings.json`. Neither is inferred
/// from the environment. A missing configuration directory remains absent.
///
/// # Errors
/// Either path is relative, another initializer owns the target, the settings are invalid,
/// a bounded read or guarded replacement fails, or repeated concurrent saves defeat the write.
pub fn settings_defaults(home: &Path, config: &Path) -> Result<DefaultsWrite, String> {
    if !home.is_absolute() || !config.is_absolute() {
        return Err("priming requires absolute home and configuration paths".into());
    }
    let path = config.join("settings.json");
    let Some(lock) = write::Lock::acquire(&path).map_err(|e| e.to_string())? else {
        return Ok(DefaultsWrite::Nothing);
    };
    let hold_window = !claude_compact_offers_kept_in(home).is_ok_and(|kept| kept.is_empty());
    add_defaults(&path, &lock, hold_window)
}

fn add_defaults(
    path: &Path,
    lock: &write::Lock,
    hold_window: bool,
) -> Result<DefaultsWrite, String> {
    write::rewrite(path, lock, true, |text| {
        defaults_edit(path, text, hold_window)
            .map(|edit| edit.map(|(doc, keys)| (format!("{}\n", doc.render()), keys)))
    })
    .map(|written| match written {
        Some((keys, backup)) => DefaultsWrite::Added { keys, backup },
        None => DefaultsWrite::Nothing,
    })
    .map_err(|e| format!("{}: {}", path.display(), e.words()))
}

#[cfg(test)]
#[path = "prime_tests.rs"]
mod tests;

fn defaults_edit(
    path: &Path,
    text: Option<&str>,
    hold_window: bool,
) -> Result<Option<(settings_json::Json, Vec<&'static str>)>, String> {
    if text.is_none() && !path.parent().is_some_and(Path::is_dir) {
        return Ok(None);
    }
    let mut doc = match text {
        Some(text) => settings_json::Json::parse(text)?,
        None => settings_json::Json::Object(Vec::new()),
    };
    let own_effort = manual_efforts(&doc).next().is_some();
    let own_rate = survey_rate_set_by_hand(&doc).is_some();
    let top = doc
        .as_object_mut()
        .ok_or_else(|| format!("{}: the top level is not a JSON object", path.display()))?;
    let mut keys = Vec::new();
    for (key, value, _) in CLAUDE_DEFAULTS {
        if held_default(key, own_effort, own_rate, hold_window) {
            continue;
        }
        let added = match key.split_once('.') {
            None => {
                let unset = !top.iter().any(|(name, _)| name == key);
                if unset {
                    top.push((key.to_owned(), value.json()));
                }
                unset
            }
            Some((parent, child)) => match top.iter_mut().find(|(name, _)| name == parent) {
                None => {
                    top.push((
                        parent.to_owned(),
                        settings_json::Json::Object(vec![(child.to_owned(), value.json())]),
                    ));
                    true
                }
                Some((_, members)) => {
                    let members = members.as_object_mut().ok_or_else(|| {
                        format!("{}: \"{parent}\" is not a JSON object", path.display())
                    })?;
                    let unset = !members.iter().any(|(name, _)| name == child);
                    if unset {
                        members.push((child.to_owned(), value.json()));
                    }
                    unset
                }
            },
        };
        if added {
            keys.push(key);
        }
    }
    Ok((!keys.is_empty()).then_some((doc, keys)))
}

fn held_default(key: &str, own_effort: bool, own_rate: bool, hold_window: bool) -> bool {
    (own_effort && CLAUDE_EFFORT_DEFAULTS.contains(&key))
        || (own_rate && key == CLAUDE_SURVEY_SWITCH)
        || (hold_window && key == CLAUDE_COMPACT_WINDOW_KEY)
}

/// Claude Code's settings key for its top-effort mode, as the vendor spells it.
const CLAUDE_TOP_EFFORT_KEY: &str = concat!("ultra", "code");

/// THE CLAUDE CODE DEFAULTS.
const CLAUDE_DEFAULTS: [(&str, ClaudeDefault, Meaning); 6] = [
    (
        CLAUDE_TOP_EFFORT_KEY,
        ClaudeDefault::Bool(true),
        Meaning::TopEffort,
    ),
    (
        "effortLevel",
        ClaudeDefault::Str(DEFAULT_EFFORT),
        Meaning::Effort,
    ),
    (
        "feedbackSurveyRate",
        ClaudeDefault::Number("0"),
        Meaning::SessionSurveys,
    ),
    (
        CLAUDE_SURVEY_SWITCH,
        ClaudeDefault::Str("1"),
        Meaning::AllSurveys,
    ),
    (
        CLAUDE_DRAFTS_KEY,
        ClaudeDefault::Str("quiet"),
        Meaning::Drafts,
    ),
    (
        CLAUDE_COMPACT_WINDOW_KEY,
        ClaudeDefault::Number("400000"),
        Meaning::CompactWindow,
    ),
];

/// The setting of Claude Code's drafted-feedback card ("Bug report drafted").
const CLAUDE_DRAFTS_KEY: &str = "feedbackDrafts";

/// Claude Code's "Auto-compact window size" in tokens (Claude Code 2.1.302).
const CLAUDE_COMPACT_WINDOW_KEY: &str = "autoCompactWindow";

const CLAUDE_STATE_FILE: &str = ".claude.json";

pub(crate) fn compact_offers_kept(claude_json: &str) -> Result<Vec<String>, String> {
    use settings_json::Json;
    let doc = Json::parse(claude_json)?;
    let accounts = doc
        .get("autoCompactOffer")
        .and_then(Json::as_object)
        .unwrap_or_default();
    let kept: std::collections::BTreeSet<&str> = accounts
        .iter()
        .flat_map(|(_, models)| models.as_object().unwrap_or_default())
        .filter(|(_, offer)| {
            offer
                .get("resolved")
                .and_then(|answer| answer.get("choice"))
                .and_then(Json::as_str)
                == Some("keep")
        })
        .map(|(model, _)| model.as_str())
        .collect();
    Ok(kept.into_iter().map(str::to_string).collect())
}

/// The models whose autocompact offer was answered "Keep", read from Claude Code's state file in `home` (`~/.claude.json`).
pub fn claude_compact_offers_kept_in(home: &Path) -> Result<Vec<String>, String> {
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        let nonblocking = if cfg!(target_os = "macos") {
            0x4
        } else if cfg!(target_os = "linux") {
            0x800
        } else {
            return Err("nonblocking Claude state reads are unsupported on this Unix host".into());
        };
        options.custom_flags(nonblocking);
    }
    let file = match options.open(home.join(CLAUDE_STATE_FILE)) {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e.to_string()),
    };
    let text = read_compact_state(file).map_err(|e| e.to_string())?;
    compact_offers_kept(&text)
}

const CLAUDE_STATE_MAX: u64 = 2 * 1024 * 1024;

fn read_compact_state(file: std::fs::File) -> std::io::Result<String> {
    use std::io::{Error, ErrorKind, Read as _};
    let metadata = file.metadata()?;
    if !metadata.is_file() {
        return Err(Error::new(
            ErrorKind::InvalidData,
            "Claude state is not a regular file",
        ));
    }
    if metadata.len() > CLAUDE_STATE_MAX {
        return Err(Error::new(
            ErrorKind::InvalidData,
            "Claude state exceeds the read budget",
        ));
    }
    let mut text =
        String::with_capacity(usize::try_from(metadata.len().saturating_add(1)).unwrap_or(0));
    file.take(CLAUDE_STATE_MAX + 1).read_to_string(&mut text)?;
    if text.len() as u64 > CLAUDE_STATE_MAX {
        return Err(Error::new(
            ErrorKind::InvalidData,
            "Claude state exceeds the read budget",
        ));
    }
    Ok(text)
}

/// The switch that turns every rating survey off, as a default's key.
const CLAUDE_SURVEY_SWITCH: &str = "env.CLAUDE_CODE_DISABLE_FEEDBACK_SURVEY";

const SURVEY_RATE_KEY: &str = "feedbackSurveyRate";

fn survey_rate_set_by_hand(doc: &settings_json::Json) -> Option<&settings_json::Json> {
    let rate = doc.get(SURVEY_RATE_KEY)?;
    let zero =
        matches!(rate, settings_json::Json::Number(n) if n.parse::<f64>().is_ok_and(|x| x == 0.0));
    (!zero).then_some(rate)
}

const DEFAULT_EFFORT: &str = "xhigh";

/// Effort defaults omitted when the person selected an effort.
const CLAUDE_EFFORT_DEFAULTS: [&str; 2] = [CLAUDE_TOP_EFFORT_KEY, "effortLevel"];

/// Manual effort values, top-level first and then models by name, without rendering them.
fn manual_efforts(
    doc: &settings_json::Json,
) -> impl Iterator<Item = (Option<&str>, &settings_json::Json)> {
    use settings_json::Json;
    let top = doc
        .get("effortLevel")
        .into_iter()
        .map(|effort| (None, effort));
    let models = doc
        .get("modelSettings")
        .and_then(Json::as_object)
        .into_iter()
        .flat_map(|models| {
            models
                .iter()
                .map(|(model, settings)| (model.as_str(), settings))
                .collect::<std::collections::BTreeMap<_, _>>()
        })
        .filter_map(|(model, settings)| {
            settings
                .get("effortLevel")
                .map(|effort| (Some(model), effort))
        });
    top.chain(models)
        .filter(|(_, effort)| effort.as_str() != Some(DEFAULT_EFFORT))
}

/// One default's value, as it is written when the key is unset.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ClaudeDefault {
    Bool(bool),
    Str(&'static str),
    Number(&'static str),
}

impl ClaudeDefault {
    fn json(self) -> settings_json::Json {
        match self {
            ClaudeDefault::Bool(b) => settings_json::Json::Bool(b),
            ClaudeDefault::Str(s) => settings_json::Json::Str(s.to_string()),
            ClaudeDefault::Number(n) => settings_json::Json::Number(n.to_string()),
        }
    }
}
