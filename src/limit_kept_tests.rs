// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The off-Fable state kept beside the journal: where it is kept, the one
//! line it is, and its write, read and removal on disk. Each has a control
//! that reads as nothing kept.

use std::path::PathBuf;

use super::*;

const SID: &str = "7c1e2a90-4b3d-4f6e-9a18-2d5c6b7e8f01";

fn scratch(name: &str) -> PathBuf {
    let root = std::env::temp_dir().join(".ubd").join(format!(
        "claude-alab-kept-{name}-{}-{}.noindex",
        std::process::id(),
        crate::supervise::now_ms()
    ));
    std::fs::create_dir_all(&root).expect("a scratch folder");
    root
}

#[test]
fn the_kept_line_reads_back_and_nothing_else_does() {
    let kept = Kept {
        back_at_ms: 1_794_762_060_000,
        walled_returns: 2,
    };
    assert_eq!(kept.line(), "back_at_ms=1794762060000 walled_returns=2\n");
    assert_eq!(Kept::parse(&kept.line()), Some(kept));
    let zero = Kept {
        back_at_ms: 0,
        walled_returns: 0,
    };
    assert_eq!(Kept::parse(&zero.line()), Some(zero));
    // CONTROLS: every other text is nothing kept.
    for text in [
        "",
        "back_at_ms=1 walled_returns=2",
        "back_at_ms=1 walled_returns=2\n\n",
        "back_at_ms=1  walled_returns=2\n",
        "walled_returns=2 back_at_ms=1\n",
        "back_at_ms=1 walled_returns=2 more=3\n",
        "back_at_ms=01 walled_returns=2\n",
        "back_at_ms=+1 walled_returns=2\n",
        "back_at_ms= walled_returns=2\n",
        "back_at_ms=1 walled_returns=4294967296\n",
        "back_at_ms=18446744073709551616 walled_returns=2\n",
        "back_at_ms=1\n",
        "back_at_msx=1 walled_returns=2\n",
    ] {
        assert_eq!(Kept::parse(text), None, "{text:?}");
    }
}

#[test]
fn a_state_is_kept_beside_the_journal_by_conversation() {
    let journal = std::path::Path::new("/state/claude-code-alab/journal.log");
    assert_eq!(
        kept_path(journal, SID),
        Some(PathBuf::from(format!(
            "/state/claude-code-alab/off-fable/{SID}"
        )))
    );
    // CONTROL: an id that could name another file names none.
    for session in [
        "../journal.log",
        "short",
        "",
        "7c1e2a90/4b3d",
        "7c1e2a90-4b3d-zz",
    ] {
        assert_eq!(kept_path(journal, session), None, "{session:?}");
    }
}

#[test]
fn a_kept_state_is_written_whole_read_back_and_removed() {
    use std::os::unix::fs::PermissionsExt as _;
    let root = scratch("disk");
    let journal = root.join("claude-code-alab").join("journal.log");
    let path = kept_path(&journal, SID).expect("a path");
    // Nothing kept yet, and removing nothing is no error.
    assert_eq!(kept(&path), None);
    forget(&path).expect("nothing to remove");
    let first = Kept {
        back_at_ms: 10,
        walled_returns: 0,
    };
    keep(&path, &first).expect("kept");
    assert_eq!(kept(&path), Some(first));
    let mode = |path: &std::path::Path| {
        std::fs::metadata(path)
            .expect("metadata")
            .permissions()
            .mode()
            & 0o777
    };
    assert_eq!(mode(&path), 0o600);
    assert_eq!(mode(path.parent().expect("its folder")), 0o700);
    // A second write replaces the first whole, and leaves nothing beside it.
    let second = Kept {
        back_at_ms: 20,
        walled_returns: 1,
    };
    keep(&path, &second).expect("kept again");
    assert_eq!(kept(&path), Some(second));
    let names: Vec<_> = std::fs::read_dir(path.parent().expect("its folder"))
        .expect("the folder")
        .map(|entry| entry.expect("an entry").file_name())
        .collect();
    assert_eq!(names, [std::ffi::OsString::from(SID)]);
    forget(&path).expect("removed");
    assert_eq!(kept(&path), None);
    // CONTROLS: a file that holds anything else, and a link to a good one,
    // read as nothing kept.
    std::fs::write(&path, "back_at_ms=10\n").expect("a short file");
    assert_eq!(kept(&path), None);
    let elsewhere = root.join("elsewhere");
    std::fs::write(&elsewhere, first.line()).expect("a good file");
    std::fs::remove_file(&path).expect("removed");
    std::os::unix::fs::symlink(&elsewhere, &path).expect("a link");
    assert_eq!(kept(&path), None);
    let _ = std::fs::remove_dir_all(&root);
}
