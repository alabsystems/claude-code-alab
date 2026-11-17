// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Checkout-local half of the canonical pin check. Upstream's `tools/check-pin`
//! also compares this tree with the named tag. An archive has no committed tree
//! to compare: its pin syntax and top-level kinds are checked, but content custody
//! still requires the release check in the original checkout.

use std::path::{Path, PathBuf};
use std::process::Command;

/// The canonical tree under vendor.
pub const DIR: &str = "alab-harness";
/// Its independently owned tag/tree pin under vendor.
pub const PIN: &str = "alab-harness.pin";

/// The actual checkout supplied to this test run, independent of build-cache paths.
pub fn checkout() -> PathBuf {
    std::env::var_os("CARGO_MANIFEST_DIR")
        .map(PathBuf::from)
        .expect("cargo sets CARGO_MANIFEST_DIR for the tests it runs")
}

/// The two canonical fields the pin producer writes.
pub fn fields(text: &str) -> Result<(&str, &str), String> {
    let mut lines = text.lines();
    let tag = lines.next().and_then(|line| line.strip_prefix("tag="));
    let tree = lines.next().and_then(|line| line.strip_prefix("tree="));
    let (Some(tag), Some(tree)) = (tag, tree) else {
        return Err("expected tag=vX.Y.Z followed by tree=<40 lowercase hex digits>".into());
    };
    let version = tag.strip_prefix('v').ok_or("tag is not vX.Y.Z")?;
    let parts: Vec<_> = version.split('.').collect();
    if parts.len() != 3
        || parts.iter().any(|part| {
            part.is_empty()
                || (part.len() > 1 && part.starts_with('0'))
                || !part.bytes().all(|byte| byte.is_ascii_digit())
                || part.parse::<u64>().is_err()
        })
    {
        return Err("tag is not canonical vX.Y.Z".into());
    }
    if tree.len() != 40
        || !tree
            .bytes()
            .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
    {
        return Err("tree is not 40 lowercase hex digits".into());
    }
    if lines.next().is_some() || !text.ends_with('\n') || text.contains('\r') {
        return Err("pin must be exactly two LF-terminated lines".into());
    }
    Ok((tag, tree))
}

/// Run an explicit Git operation in the checkout or a test-owned fixture.
pub fn git(root: &Path, args: &[&str]) -> Result<String, String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .env("GIT_NO_LAZY_FETCH", "1")
        .env("GIT_OPTIONAL_LOCKS", "0")
        .output()
        .map_err(|error| format!("cannot run git: {error}"))?;
    if !out.status.success() {
        return Err(format!(
            "git failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    String::from_utf8(out.stdout)
        .map(|text| text.trim().to_string())
        .map_err(|_| "git returned non-UTF-8 output".into())
}

/// Validate the canonical owner before reserving its paths from projections.
pub fn hold(root: &Path) -> Result<(), String> {
    let vendor = root.join("vendor");
    let pin_path = vendor.join(PIN);
    let pin_meta =
        std::fs::symlink_metadata(&pin_path).map_err(|error| format!("vendor/{PIN}: {error}"))?;
    if !pin_meta.is_file() || pin_meta.len() > 256 {
        return Err(format!("vendor/{PIN}: not a bounded regular pin file"));
    }
    let text =
        std::fs::read_to_string(&pin_path).map_err(|error| format!("vendor/{PIN}: {error}"))?;
    let (tag, tree) = fields(&text).map_err(|why| format!("vendor/{PIN}: {why}"))?;
    if !std::fs::symlink_metadata(vendor.join(DIR)).is_ok_and(|meta| meta.is_dir()) {
        return Err(format!("vendor/{DIR}: not a directory"));
    }
    if !root.join(".git").exists() {
        eprintln!(
            "archive at {}: pin {} is parsed, but canonical content comparison requires its git checkout",
            root.display(),
            tag
        );
        return Ok(());
    }
    let entry = git(
        root,
        &[
            "ls-tree",
            "--full-tree",
            "HEAD",
            "--",
            "vendor/alab-harness",
        ],
    )?;
    let expected = format!("040000 tree {tree}\tvendor/alab-harness");
    if entry != expected {
        return Err(format!(
            "vendor/{DIR}: committed entry {entry:?} is not tree {tree} ({tag})"
        ));
    }
    if git(root, &["cat-file", "-t", tree])? != "tree" {
        return Err(format!("vendor/{DIR}: {tree} is not a tree object"));
    }
    let changed = git(
        root,
        &[
            "--no-optional-locks",
            "status",
            "--porcelain",
            "--untracked-files=all",
            "--ignored=matching",
            "--",
            "vendor/alab-harness",
            "vendor/alab-harness.pin",
        ],
    )?;
    if !changed.is_empty() {
        return Err(format!("canonical copy or pin changed:\n{changed}"));
    }
    Ok(())
}
