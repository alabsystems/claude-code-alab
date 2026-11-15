// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;
use std::os::unix::ffi::OsStringExt as _;

#[test]
fn the_legacy_result_keeps_native_bytes_and_has_one_closed_line() {
    assert_eq!(
        response(&PrimeWrite::LegacyHooks(LegacyWrite::Nothing)).unwrap(),
        "OK prime=1 operation=legacy-hooks affected=0 backup=none\n"
    );
    let backup = PathBuf::from(std::ffi::OsString::from_vec(b"/x\xff".to_vec()));
    let result = response(&PrimeWrite::LegacyHooks(LegacyWrite::Removed {
        removed: 2,
        backup,
    }))
    .unwrap();
    assert_eq!(
        result,
        "OK prime=1 operation=legacy-hooks affected=2 backup=2f78ff\n"
    );
}

#[test]
fn legacy_results_refuse_impossible_or_unbounded_backups_without_truncating_them() {
    for (removed, backup) in [
        (0, PathBuf::from("/x")),
        (1, PathBuf::from("relative")),
        (
            1,
            PathBuf::from(std::ffi::OsString::from_vec(b"/x\0".to_vec())),
        ),
        (
            1,
            PathBuf::from(format!("/{}", "x".repeat(MAX_BACKUP_PATH_BYTES))),
        ),
    ] {
        assert!(
            response(&PrimeWrite::LegacyHooks(LegacyWrite::Removed {
                removed,
                backup
            }))
            .is_err()
        );
    }
    let backup = PathBuf::from(format!("/{}", "x".repeat(MAX_BACKUP_PATH_BYTES - 1)));
    let result = response(&PrimeWrite::LegacyHooks(LegacyWrite::Removed {
        removed: usize::MAX,
        backup,
    }))
    .unwrap();
    assert!(result.len() <= MAX_RESULT_BYTES);
    assert!(result.ends_with('\n'));
    assert_eq!(result.bytes().filter(|byte| *byte == b'\n').count(), 1);
}
