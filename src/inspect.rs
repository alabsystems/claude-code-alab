// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Bounded, read-only defaults findings from the same interpretation as initialization.
//! Findings describe observations and preferences; they grant no process authority.

use std::ffi::OsString;
use std::os::unix::ffi::OsStrExt as _;
use std::path::{Path, PathBuf};

mod files;
mod words;
use crate::prime::Json;

pub(crate) const CAPABILITIES: &[&str] = &["claude:settings-defaults"];
/// Maximum complete serialized inspection document, including its final LF.
pub const MAX_RESPONSE_BYTES: usize = 1024 * 1024;
/// Maximum native path represented losslessly by an inspection.
pub const MAX_PATH_BYTES: usize = 64 * 1024;
/// Maximum number of independently identified findings in a v1 document.
pub const MAX_FINDINGS: usize = 8;

/// What a selected path's bounded read established.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PathStatus {
    /// A bounded regular-file read completed.
    Present,
    /// The selected file or a directory does not exist.
    Missing,
    /// A symbolic-link component has no existing target.
    BrokenLink,
    /// Resolution or the bounded regular-file read could not complete.
    Unavailable,
}
/// Selected and observed physical names; physical names are observations, not authority.
#[derive(Clone, Debug)]
pub struct PathObservation {
    /// Exact explicitly selected path.
    pub selected: PathBuf,
    /// Canonical path when resolution succeeded.
    pub physical: Option<PathBuf>,
    /// Read outcome, distinguishing an absent name from broken indirection.
    pub status: PathStatus,
}
/// Generic finding outcome.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Disposition {
    /// The observation needs no change.
    Ok,
    /// The owner describes a difference from the desired state.
    Change,
    /// The owner could not establish the requested observation.
    Unavailable,
}
/// Existing host presentation classes, with no new authority class.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Class {
    /// A deliberate personal choice or unavailable personal settings observation.
    Person,
    /// An automatic unset-default preference.
    Preference,
}
/// A diagnostic can explain a manual edit, but never executes it.
#[derive(Clone, Debug)]
pub enum Remedy {
    /// No manual action is proposed.
    None,
    /// Explain an edit of the explicitly selected settings file.
    Edit {
        /// Owner-authored single-line explanation.
        instruction: String,
    },
}
/// A complete presentation row requiring no vendor interpretation by its consumer.
#[derive(Clone, Debug)]
pub struct Finding {
    /// Unique, bounded stable ASCII row key.
    pub key: String,
    /// Observed state.
    pub disposition: Disposition,
    /// Preference or deliberate personal choice.
    pub class: Class,
    /// Single-line title.
    pub label: String,
    /// Single-line explanation, with untrusted controls visibly escaped.
    pub detail: String,
    /// Single-line desired value, when relevant.
    pub expected: Option<String>,
    /// Non-executing presentation remedy.
    pub remedy: Remedy,
}
/// One closed v1 response, wholly validated before any stdout write.
#[derive(Clone, Debug)]
pub struct Report {
    /// Selected settings-file observation.
    pub settings: PathObservation,
    /// Selected state-file observation.
    pub state: PathObservation,
    /// Whether automatic initialization is enabled at the caller.
    pub auto_prime: bool,
    /// Complete owner-authored findings.
    pub findings: Vec<Finding>,
}

fn hex(path: &Path) -> Result<String, String> {
    let bytes = path.as_os_str().as_bytes();
    if !path.is_absolute() || bytes.len() > MAX_PATH_BYTES || bytes.contains(&0) {
        return Err("inspection path exceeds its native-byte contract".into());
    }
    use std::fmt::Write as _;
    let mut text = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        write!(text, "{byte:02x}").expect("writing into String");
    }
    Ok(text)
}

fn object(fields: impl IntoIterator<Item = (&'static str, Json)>) -> Json {
    Json::Object(
        fields
            .into_iter()
            .map(|(key, value)| (key.into(), value))
            .collect(),
    )
}
fn string(text: &str) -> Json {
    Json::Str(text.into())
}
fn optional(text: Option<&str>) -> Json {
    text.map_or(Json::Null, string)
}
fn single_line(text: &str) -> bool {
    !text
        .chars()
        .any(|c| c.is_control() || matches!(c, '\u{2028}' | '\u{2029}'))
}

impl PathObservation {
    fn json(&self) -> Result<Json, String> {
        if matches!(self.status, PathStatus::Present) && self.physical.is_none()
            || matches!(self.status, PathStatus::Missing | PathStatus::BrokenLink)
                && self.physical.is_some()
        {
            return Err("inconsistent inspection path observation".into());
        }
        Ok(object([
            ("selected_hex", string(&hex(&self.selected)?)),
            (
                "physical_hex",
                self.physical
                    .as_deref()
                    .map(hex)
                    .transpose()?
                    .as_deref()
                    .map_or(Json::Null, string),
            ),
            (
                "status",
                string(match self.status {
                    PathStatus::Present => "present",
                    PathStatus::Missing => "missing",
                    PathStatus::BrokenLink => "broken-link",
                    PathStatus::Unavailable => "unavailable",
                }),
            ),
        ]))
    }
}

impl Report {
    /// Encode the closed response, refusing duplicate/invalid keys, controls or excess output.
    /// # Errors
    /// A path, row, presentable field or complete output exceeds the v1 contract.
    pub fn response(&self) -> Result<String, String> {
        if self.findings.len() > MAX_FINDINGS {
            return Err("too many inspection findings".into());
        }
        let mut keys = std::collections::BTreeSet::new();
        let mut rows = Vec::new();
        for row in &self.findings {
            if row.key.is_empty()
                || row.key.len() > 128
                || !row
                    .key
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
                || !keys.insert(row.key.as_str())
            {
                return Err("invalid or duplicate inspection row key".into());
            }
            let instruction = match &row.remedy {
                Remedy::None => None,
                Remedy::Edit { instruction } => Some(instruction.as_str()),
            };
            if [&row.label, &row.detail]
                .into_iter()
                .map(String::as_str)
                .chain(row.expected.as_deref())
                .chain(instruction)
                .any(|text| !single_line(text) || text.len() > MAX_RESPONSE_BYTES)
            {
                return Err("inspection field exceeds its presentation contract".into());
            }
            rows.push(object([
                ("key", string(&row.key)),
                (
                    "disposition",
                    string(match row.disposition {
                        Disposition::Ok => "ok",
                        Disposition::Change => "change",
                        Disposition::Unavailable => "unavailable",
                    }),
                ),
                (
                    "class",
                    string(match row.class {
                        Class::Person => "person",
                        Class::Preference => "preference",
                    }),
                ),
                ("label", string(&row.label)),
                ("detail", string(&row.detail)),
                ("expected", optional(row.expected.as_deref())),
                (
                    "remedy",
                    object([
                        (
                            "kind",
                            string(if instruction.is_some() {
                                "edit"
                            } else {
                                "none"
                            }),
                        ),
                        ("target", optional(instruction.map(|_| "settings"))),
                        ("instruction", optional(instruction)),
                    ]),
                ),
            ]));
        }
        let doc = object([
            ("schema", Json::Number("1".into())),
            ("operation", string("settings-defaults")),
            ("auto_prime", Json::Bool(self.auto_prime)),
            (
                "paths",
                object([
                    ("settings", self.settings.json()?),
                    ("state", self.state.json()?),
                ]),
            ),
            ("findings", Json::Array(rows)),
        ]);
        let text = format!("{}\n", doc.render());
        if text.len() > MAX_RESPONSE_BYTES {
            return Err("inspection response exceeds its output budget".into());
        }
        Ok(text)
    }
}

/// Inspect only the explicitly selected files. No lock, backup, directory or file is created.
/// # Errors
/// A selected path is relative, contains NUL or exceeds the native-byte bound.
pub fn settings_defaults(home: &Path, config: &Path, auto_prime: bool) -> Result<Report, String> {
    let settings = config.join("settings.json");
    let state = home.join(".claude.json");
    hex(&settings)?;
    hex(&state)?;
    let observed = files::observe(settings, 1024 * 1024);
    let state_observed = files::observe(state, 2 * 1024 * 1024);
    let offer = match state_observed.text.as_ref() {
        Ok(None) => None,
        Ok(Some(text)) => match crate::prime::compact_offers_kept(text) {
            Ok(models) if models.is_empty() => None,
            Ok(models) => Some(format!(
                "the autocompact offer was answered Keep ({})",
                Json::Array(models.into_iter().map(Json::Str).collect()).render()
            )),
            Err(error) => Some(format!(
                "the autocompact offer record cannot be read ({error})"
            )),
        },
        Err(error) => Some(format!(
            "the autocompact offer record cannot be read ({error})"
        )),
    };
    let findings = match observed.text {
        Ok(None) => Vec::new(),
        Err(error) => vec![words::unavailable("settings", &format!("the selected settings file cannot be read ({error})"))],
        Ok(Some(text)) => match Json::parse(&text).and_then(|doc| {
            let states = crate::prime::settings_state(&doc, offer.is_some())?;
            if auto_prime && crate::prime::would_add(&doc, offer.is_some())
                && !observed.path.physical.as_deref().is_some_and(files::parent_writable) {
                return Err("the selected settings file's physical directory is not writable; automatic initialization cannot add defaults".into());
            }
            Ok(states)
        }) {
            Ok(states) => states.into_iter().map(|state| words::setting(state, auto_prime, offer.as_deref())).collect(),
            Err(error) => vec![words::unavailable("settings", &format!("the selected settings file cannot take defaults ({error})"))],
        },
    };
    Ok(Report {
        settings: observed.path,
        state: state_observed.path,
        auto_prime,
        findings,
    })
}

pub(crate) fn entry(words: &[OsString]) -> Option<Result<Report, String>> {
    if words.first().is_none_or(|word| word != "inspect") {
        return None;
    }
    Some(match words {
        [_, operation, h, home, c, config, auto] if operation == "settings-defaults" && h == "--home" && c == "--config" && (auto == "--auto-prime=0" || auto == "--auto-prime=1") => settings_defaults(Path::new(home), Path::new(config), auto == "--auto-prime=1"),
        _ => Err("expected inspect settings-defaults --home <absolute-path> --config <absolute-path> --auto-prime=0|1".into()),
    })
}

#[cfg(test)]
#[path = "inspect_tests.rs"]
mod tests;
