// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The phase and agent-name projections under `vendor/` are aterm's: its
//! `targo --unverified verify vendor-out` writes them (each file a library's
//! build reads, byte for byte, and a `Cargo.toml` of its making), with
//! `vendor/VENDORED`, which gives each file's FNV-1a 64 digest and carries a
//! seal of its own. This check holds every projection file to that list
//! and the list to its seal, so a hand edit of a copy, or of a copy and its
//! line in the list together, fails here without aterm. A copy is changed by
//! running vendor-out again in aterm, never by editing it here. The canonical
//! `alab-harness/` tree and its pin are separately checked before their exact
//! paths are reserved. Every other unlisted vendor file remains an error.
//!
//! Std only, and it links none of the copies. It reads the list as aterm's
//! `aterm_seal` (`crates/aterm-seal`) writes it: a seal line in the first 16 lines carries
//! `@generated` and `seal ` before 16 lowercase hex digits, the FNV-1a 64 of
//! the whole file with those digits read as `0`. Every other line that does
//! not start with `#` is `<16 hex digits> <path from vendor/>`, in path order.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

#[path = "support/pin.rs"]
mod pin;
use pin::checkout;

/// The list of digests, in `vendor/`.
const LIST: &str = "VENDORED";

/// The lines of the list's head a seal line may sit in.
const HEAD_LINES: usize = 16;

/// The word a seal line carries.
const MARK: &str = "@generated";

/// The token before a seal's digits.
const SEAL: &str = "seal ";

/// A digest's width in hex digits.
const DIGITS: usize = 16;

const FNV64_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
const FNV64_PRIME: u64 = 0x0000_0100_0000_01b3;

/// FNV-1a 64 of `bytes`, with the bytes in `masked` read as `0`.
fn fnv1a_64(bytes: &[u8], masked: std::ops::Range<usize>) -> u64 {
    bytes
        .iter()
        .enumerate()
        .fold(FNV64_OFFSET, |hash, (at, &byte)| {
            let byte = if masked.contains(&at) { b'0' } else { byte };
            (hash ^ u64::from(byte)).wrapping_mul(FNV64_PRIME)
        })
}

/// The byte range of the digits of the first seal line in `text`'s head.
fn seal_digits(text: &str) -> Option<std::ops::Range<usize>> {
    let mut start = 0;
    for raw in text.split_inclusive('\n').take(HEAD_LINES) {
        if raw.contains(MARK) {
            for (at, _) in raw.match_indices(SEAL) {
                let from = at + SEAL.len();
                let hex = raw.as_bytes().get(from..from + DIGITS);
                let after = raw.as_bytes().get(from + DIGITS);
                if hex.is_some_and(|h| h.iter().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')))
                    && !after.is_some_and(u8::is_ascii_hexdigit)
                {
                    return Some(start + from..start + from + DIGITS);
                }
            }
        }
        start += raw.len();
    }
    None
}

/// Whether `path` is a plain relative path inside the folder it is read from.
fn inside(path: &str) -> bool {
    !path.is_empty()
        && !path.starts_with('/')
        && path
            .split('/')
            .all(|part| !part.is_empty() && part != "." && part != "..")
}

/// Every entry under `dir` but its folders, by path from it, each with whether
/// it is a regular file (a symbolic link is not followed, and is not one).
fn entries(dir: &Path, prefix: &str, into: &mut BTreeMap<String, bool>) -> Result<(), String> {
    let listed = fs::read_dir(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    for entry in listed {
        let entry = entry.map_err(|e| format!("{}: {e}", dir.display()))?;
        let name = entry.file_name().to_string_lossy().into_owned();
        let path = format!("{prefix}{name}");
        let kind = entry
            .file_type()
            .map_err(|e| format!("{}: {e}", entry.path().display()))?;
        if kind.is_dir() {
            entries(&entry.path(), &format!("{path}/"), into)?;
        } else {
            into.insert(path, kind.is_file());
        }
    }
    Ok(())
}

/// What a check of a `vendor/` folder found.
struct Checked {
    /// The paths the list names, from `vendor/`.
    listed: Vec<String>,
    /// One line per thing wrong, each starting with the `vendor/` path it names.
    problems: Vec<String>,
}

/// Check the copy in `vendor`: the list's seal, each line of it, each file it
/// names and every file under `vendor` it does not name.
fn check(vendor: &Path) -> Checked {
    let mut checked = Checked {
        listed: Vec::new(),
        problems: Vec::new(),
    };
    let root = vendor.parent().expect("vendor has a checkout parent");
    if let Err(why) = pin::hold(root) {
        checked.problems.push(why);
        return checked;
    }
    let problems = &mut checked.problems;
    let text = match fs::symlink_metadata(vendor.join(LIST)) {
        Ok(meta) if meta.is_file() => fs::read(vendor.join(LIST))
            .map_err(|e| e.to_string())
            .and_then(|bytes| String::from_utf8(bytes).map_err(|_| "not UTF-8".to_string())),
        Ok(_) => Err("not a regular file".to_string()),
        Err(e) => Err(e.to_string()),
    };
    let text = match text {
        Ok(text) => text,
        Err(why) => {
            problems.push(format!("vendor/{LIST}: {why}"));
            return checked;
        }
    };
    match seal_digits(&text) {
        None => problems.push(format!(
            "vendor/{LIST}: no `{MARK} … {SEAL}<{DIGITS} hex digits>` line in its first \
             {HEAD_LINES} lines"
        )),
        Some(digits) => {
            let now = format!("{:016x}", fnv1a_64(text.as_bytes(), digits.clone()));
            if text[digits.clone()] != now {
                problems.push(format!(
                    "vendor/{LIST}: its seal says {} and the list digests to {now}: it was \
                     edited after vendor-out wrote it",
                    &text[digits]
                ));
            }
        }
    }
    let mut digests = BTreeMap::new();
    for (index, line) in text.lines().enumerate() {
        if line.starts_with('#') {
            continue;
        }
        let parsed = line.split_once(' ').filter(|(digest, path)| {
            digest.len() == DIGITS
                && digest
                    .bytes()
                    .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
                && inside(path)
                && *path != LIST
                && matches!(
                    path.split('/').next(),
                    Some("aterm-phase" | "aterm-agent-names")
                )
        });
        let Some((digest, path)) = parsed else {
            problems.push(format!(
                "vendor/{LIST}:{}: not `<{DIGITS} hex digits> <path inside vendor/>`",
                index + 1
            ));
            continue;
        };
        if checked
            .listed
            .last()
            .is_some_and(|last| last.as_str() >= path)
        {
            problems.push(format!(
                "vendor/{LIST}:{}: {path} is out of path order or listed twice",
                index + 1
            ));
        }
        checked.listed.push(path.to_string());
        digests.insert(path.to_string(), digest.to_string());
    }
    let mut found = BTreeMap::new();
    if let Err(why) = entries(vendor, "", &mut found) {
        problems.push(format!("vendor/: {why}"));
        return checked;
    }
    for (path, digest) in &digests {
        match found.get(path) {
            None => problems.push(format!("vendor/{path}: listed, and not there")),
            Some(false) => problems.push(format!("vendor/{path}: not a regular file")),
            Some(true) => match fs::read(vendor.join(path)) {
                Err(e) => problems.push(format!("vendor/{path}: {e}")),
                Ok(bytes) => {
                    let now = format!("{:016x}", fnv1a_64(&bytes, 0..0));
                    if now != *digest {
                        problems.push(format!(
                            "vendor/{path}: digests to {now}, and the list says {digest}"
                        ));
                    }
                }
            },
        }
    }
    let canonical_prefix = format!("{}/", pin::DIR);
    for path in found.keys() {
        if path != LIST
            && path != pin::PIN
            && !path.starts_with(&canonical_prefix)
            && !digests.contains_key(path)
        {
            problems.push(format!("vendor/{path}: not in vendor/{LIST}"));
        }
    }
    checked
}

/// A fresh scratch folder of this test's own.
fn scratch(name: &str) -> PathBuf {
    let root = std::env::var_os("CARGO_TARGET_TMPDIR")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join(format!(
            ".ubd/vendored.noindex/{name}-{}",
            std::process::id()
        ));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).expect("scratch folder");
    root
}

/// Copy the tree at `from` to `to`, folders and regular files only.
fn copy_tree(from: &Path, to: &Path) {
    fs::create_dir_all(to).expect("copy folder");
    for entry in fs::read_dir(from).expect("list folder") {
        let entry = entry.expect("folder entry");
        let kind = entry.file_type().expect("entry type");
        let target = to.join(entry.file_name());
        if kind.is_dir() {
            copy_tree(&entry.path(), &target);
        } else {
            assert!(kind.is_file(), "{} is not a file", entry.path().display());
            fs::copy(entry.path(), &target).expect("copy file");
        }
    }
}

#[test]
fn every_copy_matches_the_sealed_list() {
    let checked = check(&checkout().join("vendor"));
    assert!(
        checked.problems.is_empty(),
        "vendor ownership differs: canonical copies change through alab-harness's tools/pin; \
         phase/name projections through aterm's `targo --unverified verify vendor-out \
         --into <this checkout>`. Never edit either copy here:\n{}",
        checked.problems.join("\n")
    );
    for package in ["aterm-phase", "aterm-agent-names"] {
        let manifest = format!("{package}/Cargo.toml");
        assert!(
            checked.listed.contains(&manifest),
            "vendor/{LIST} names no {manifest}, which Cargo.toml's path dependencies build"
        );
    }
}

#[test]
fn a_changed_byte_is_named() {
    let root = scratch("a-changed-byte");
    let vendor = root.join("vendor");
    copy_tree(&checkout().join("vendor"), &vendor);
    let checked = check(&vendor);
    assert!(checked.problems.is_empty(), "{:?}", checked.problems);

    let path = checked
        .listed
        .iter()
        .find(|path| path.ends_with(".rs"))
        .expect("the list names a source file")
        .clone();
    let file = vendor.join(&path);
    let original = fs::read(&file).expect("read the copy");
    let mut changed = original.clone();
    let middle = changed.len() / 2;
    changed[middle] ^= 1;
    fs::write(&file, &changed).expect("change one byte");
    assert_eq!(
        check(&vendor).problems,
        [format!(
            "vendor/{path}: digests to {:016x}, and the list says {:016x}",
            fnv1a_64(&changed, 0..0),
            fnv1a_64(&original, 0..0)
        )],
        "one changed byte of a copy names that copy, and nothing else"
    );

    // The same edit with its line in the list rewritten to match: the seal names it.
    let list = vendor.join(LIST);
    let text = fs::read_to_string(&list).expect("read the list");
    let was = format!("{:016x} {path}\n", fnv1a_64(&original, 0..0));
    let now = format!("{:016x} {path}\n", fnv1a_64(&changed, 0..0));
    assert_eq!(text.matches(&was).count(), 1, "the list names {path} once");
    fs::write(&list, text.replace(&was, &now)).expect("rewrite the list");
    let problems = check(&vendor).problems;
    assert_eq!(problems.len(), 1, "{problems:?}");
    assert!(
        problems[0].starts_with(&format!("vendor/{LIST}: its seal says ")),
        "{problems:?}"
    );

    // A file the list does not name is named.
    fs::write(&list, &text).expect("restore the list");
    fs::write(&file, &original).expect("restore the copy");
    let added = match path.rsplit_once('/') {
        Some((folder, _)) => format!("{folder}/added.rs"),
        None => "added.rs".to_string(),
    };
    fs::write(vendor.join(&added), b"// added here\n").expect("add a file");
    assert_eq!(
        check(&vendor).problems,
        [format!("vendor/{added}: not in vendor/{LIST}")],
        "a file the list does not name is named"
    );

    fs::remove_dir_all(&root).expect("remove the scratch folder");
}

#[test]
fn canonical_drift_fails_before_projection_paths_are_reserved() {
    let root = scratch("canonical-drift");
    let vendor = root.join("vendor");
    copy_tree(&checkout().join("vendor"), &vendor);
    fs::write(root.join(".gitignore"), b"vendor/alab-harness/ignored\n")
        .expect("fixture ignore rule");
    pin::git(&root, &["init", "--quiet", "--object-format=sha1"]).expect("fixture repository");
    // This fixture owns no detached Git housekeeping when its directory is removed.
    pin::git(&root, &["config", "gc.auto", "0"]).expect("disable fixture auto gc");
    pin::git(&root, &["config", "maintenance.auto", "false"])
        .expect("disable fixture auto maintenance");
    pin::git(
        &root,
        &[
            "-c",
            "core.autocrlf=false",
            "add",
            "-f",
            "vendor",
            ".gitignore",
        ],
    )
    .expect("fixture index");
    let commit = [
        "-c",
        "user.name=Fixture",
        "-c",
        "user.email=fixture@example.test",
        "-c",
        "commit.gpgsign=false",
        "commit",
        "--quiet",
        "-m",
        "fixture",
    ];
    pin::git(&root, &commit).expect("fixture commit");
    assert!(check(&vendor).problems.is_empty());
    let file = vendor.join(pin::DIR).join("README.md");
    let original = fs::read(&file).expect("canonical input");
    let mut bytes = original.clone();
    bytes.extend_from_slice(b"\n");
    fs::write(&file, &bytes).expect("change canonical data");
    fs::write(vendor.join("aterm-phase/stray.rs"), b"// stray\n").expect("projection stray");
    let changed = || {
        let problems = check(&vendor).problems;
        assert_eq!(
            problems.len(),
            1,
            "pin failure precedes projection checks: {problems:?}"
        );
        assert!(problems[0].starts_with("canonical copy or pin changed:"));
    };
    changed();
    pin::git(&root, &["add", "vendor/alab-harness/README.md"]).expect("stage canonical drift");
    changed();
    fs::write(&file, &original).expect("restore canonical data");
    pin::git(&root, &["add", "vendor/alab-harness/README.md"]).expect("restore fixture index");
    fs::remove_file(vendor.join("aterm-phase/stray.rs")).expect("remove projection stray");
    assert!(check(&vendor).problems.is_empty());

    let ignored = vendor.join(pin::DIR).join("ignored");
    fs::write(&ignored, b"not part of the pin\n").expect("ignored canonical stray");
    changed();
    fs::remove_file(ignored).expect("remove ignored stray");
    assert!(check(&vendor).problems.is_empty());

    fs::write(&file, bytes).expect("change canonical data again");
    pin::git(&root, &["add", "vendor/alab-harness/README.md"]).expect("stage committed drift");
    pin::git(&root, &commit).expect("commit canonical drift");
    let problems = check(&vendor).problems;
    assert_eq!(problems.len(), 1, "committed drift: {problems:?}");
    assert!(problems[0].starts_with("vendor/alab-harness: committed entry"));
    fs::remove_dir_all(root).expect("fixture cleanup");
}

#[test]
fn malformed_pins_and_projection_overlap_do_not_create_a_vendor_exception() {
    let root = scratch("owner-boundaries");
    let vendor = root.join("vendor");
    copy_tree(&checkout().join("vendor"), &vendor);
    let pin_file = vendor.join(pin::PIN);
    let original_pin = fs::read(&pin_file).expect("pin input");
    fs::write(&pin_file, "tag=v0.9.1\ntree=invalid\n").expect("invalid pin");
    let problems = check(&vendor).problems;
    assert_eq!(problems.len(), 1);
    assert!(problems[0].starts_with("vendor/alab-harness.pin:"));
    fs::write(&pin_file, original_pin).expect("restore pin");

    let list_file = vendor.join(LIST);
    let original_list = fs::read_to_string(&list_file).expect("projection list");
    let at = original_list
        .lines()
        .take_while(|line| line.starts_with('#'))
        .map(|line| line.len() + 1)
        .sum::<usize>();
    for canonical_path in ["alab-harness/Cargo.toml", pin::PIN] {
        let digest = fnv1a_64(&fs::read(vendor.join(canonical_path)).unwrap(), 0..0);
        let mut overlap = original_list.clone();
        overlap.insert_str(at, &format!("{digest:016x} {canonical_path}\n"));
        let digits = seal_digits(&overlap).unwrap();
        let seal = format!("{:016x}", fnv1a_64(overlap.as_bytes(), digits.clone()));
        overlap.replace_range(digits, &seal);
        fs::write(&list_file, overlap).expect("sealed overlapping owner");
        let checked = check(&vendor);
        assert_eq!(
            checked.problems.len(),
            1,
            "a valid seal cannot claim {canonical_path}: {:?}",
            checked.problems
        );
        assert!(!checked.listed.iter().any(|listed| listed == canonical_path));
    }

    fs::write(&list_file, original_list).expect("restore list");
    fs::write(vendor.join("alab-harness-extra"), b"stray\n").expect("similar name");
    assert_eq!(
        check(&vendor).problems,
        ["vendor/alab-harness-extra: not in vendor/VENDORED"]
    );
    fs::remove_dir_all(root).expect("fixture cleanup");
}
