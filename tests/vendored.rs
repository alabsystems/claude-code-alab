// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The copies under `vendor/` are aterm's (`docs/DECISIONS.md`, 41): aterm's
//! `targo --unverified verify vendor-out` writes them (each file a library's
//! build reads, byte for byte, and a `Cargo.toml` of its making), with
//! `vendor/VENDORED`, which gives each file's FNV-1a 64 digest and carries a
//! seal of its own. This check holds every file under `vendor/` to that list
//! and the list to its seal, so a hand edit of a copy, or of a copy and its
//! line in the list together, fails here without aterm. A copy is changed by
//! running vendor-out again in aterm, never by editing it here.
//!
//! Std only, and it links none of the copies. It reads the list as aterm's
//! `aterm_census::seal` writes it: a seal line in the first 16 lines carries
//! `@generated` and `seal ` before 16 lowercase hex digits, the FNV-1a 64 of
//! the whole file with those digits read as `0`. Every other line that does
//! not start with `#` is `<16 hex digits> <path from vendor/>`, in path order.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

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
    for path in found.keys() {
        if path != LIST && !digests.contains_key(path) {
            problems.push(format!("vendor/{path}: not in vendor/{LIST}"));
        }
    }
    checked
}

/// This checkout, read when the test runs: a test built in another checkout
/// and reused from a shared build cache would otherwise check that one's copy.
fn checkout() -> PathBuf {
    std::env::var_os("CARGO_MANIFEST_DIR")
        .map(PathBuf::from)
        .expect("cargo sets CARGO_MANIFEST_DIR for the tests it runs")
}

/// A fresh scratch folder of this test's own.
fn scratch(name: &str) -> PathBuf {
    let root = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!(
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
        "vendor/ is not as aterm's vendor-out wrote it. Run `targo --unverified verify \
         vendor-out --into <this checkout>` in aterm; never edit a copy here:\n{}",
        checked.problems.join("\n")
    );
    for package in ["alab-policy", "aterm-phase", "aterm-agent-names"] {
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
