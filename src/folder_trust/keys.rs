// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Claude 2.1.323 trust-key and profile selection, ported from Main6116 launch_trust.

use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::path::{Component, Path, PathBuf};

pub(super) fn same_dir(a: &Path, b: &Path) -> bool {
    use std::os::unix::fs::MetadataExt as _;
    let id = |path: &Path| {
        std::fs::metadata(path)
            .ok()
            .filter(std::fs::Metadata::is_dir)
            .map(|m| (m.dev(), m.ino()))
    };
    id(a).is_some_and(|a| Some(a) == id(b))
}

fn lexical(base: &Path, rel: &Path) -> PathBuf {
    let mut out = if rel.is_absolute() {
        PathBuf::new()
    } else {
        base.to_path_buf()
    };
    for part in rel.components() {
        match part {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            part => out.push(part),
        }
    }
    out
}

fn marks_root(git: &Path) -> bool {
    std::fs::metadata(git).is_ok_and(|meta| meta.is_dir() || meta.is_file())
}

fn small_file(path: &Path) -> Option<String> {
    crate::store::read_bounded(path, 64 * 1024, false).map(|text| text.trim().to_owned())
}

fn main_checkout(root: &Path) -> Option<PathBuf> {
    let text = small_file(&root.join(".git"))?;
    let git_dir = lexical(root, Path::new(text.strip_prefix("gitdir:")?.trim()));
    let common = lexical(
        &git_dir,
        Path::new(&small_file(&git_dir.join("commondir"))?),
    );
    if git_dir.parent()? != common.join("worktrees") {
        return None;
    }
    let back = lexical(&git_dir, Path::new(&small_file(&git_dir.join("gitdir"))?));
    if back.canonicalize().ok()? != root.canonicalize().ok()?.join(".git") {
        return None;
    }
    if common.file_name() != Some(OsStr::new(".git")) {
        return (!marks_root(&common.join(".git"))).then_some(common);
    }
    common.parent().map(Path::to_path_buf)
}

pub(super) fn trust_key(cwd: &Path) -> PathBuf {
    cwd.ancestors()
        .find(|dir| marks_root(&dir.join(".git")))
        .map_or_else(
            || cwd.to_path_buf(),
            |root| main_checkout(root).unwrap_or_else(|| root.to_path_buf()),
        )
}

pub(super) fn profile(home: &Path, env: &BTreeMap<OsString, OsString>) -> Option<PathBuf> {
    let config = env.get(OsStr::new("CLAUDE_CONFIG_DIR"));
    let records = match config {
        Some(path) if Path::new(path).is_absolute() => PathBuf::from(path),
        Some(_) => return None,
        None => home.join(".claude"),
    };
    // Explicit CLAUDE_CONFIG_DIR keeps its profile inside that directory, including
    // an explicitly named default directory. Absent selection uses HOME's plain profile.
    let mut plain = if config.is_some() {
        records.join(".claude.json")
    } else {
        home.join(".claude.json")
    };
    if env
        .get(OsStr::new("CLAUDE_CODE_CUSTOM_OAUTH_URL"))
        .is_some_and(|v| !v.is_empty())
    {
        plain.set_file_name(".claude-custom-oauth.json");
    }
    let legacy = records.join(".config.json");
    if std::fs::metadata(&legacy).is_ok_and(|m| {
        m.is_file() && (m.len() > 0 || (!m.permissions().readonly() && !plain.exists()))
    }) {
        return Some(legacy);
    }
    Some(plain)
}

pub(super) fn recorded(profile: &Path, key: &Path) -> bool {
    let Some(text) = crate::store::read_bounded(profile, 1024 * 1024, true) else {
        return false;
    };
    let Some(value) = crate::json::parse(&text) else {
        return false;
    };
    let Some(crate::json::Value::Object(projects)) = value.get("projects") else {
        return false;
    };
    let Some(spelling) = key.to_str() else {
        return false;
    };
    projects.iter().any(|(folder, entry)| {
        entry
            .get("hasTrustDialogAccepted")
            .and_then(crate::json::Value::as_bool)
            == Some(true)
            && (folder == spelling
                || (!spelling.is_ascii() && !folder.is_ascii() && same_dir(Path::new(folder), key)))
    })
}
