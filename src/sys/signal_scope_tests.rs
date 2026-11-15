// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Pure data at the actual checked-restoration completion boundary. No handlers change.

use super::*;

#[test]
fn mask_restore_failure_retains_every_caught_byte_until_successful_retry() {
    let mut caught = vec![libc::SIGINT, libc::SIGCHLD, libc::SIGINT];
    let original = caught.clone();
    let failure =
        finish_signal_restore(&mut caught, || Err(io::Error::from_raw_os_error(libc::EIO)));
    assert!(failure.is_err());
    assert_eq!(
        caught, original,
        "failed native restoration cannot consume signal data"
    );
    caught.extend([libc::SIGTERM, libc::SIGCONT]);
    let received = finish_signal_restore(&mut caught, || Ok(())).unwrap();
    assert_eq!(
        received,
        [
            libc::SIGINT,
            libc::SIGCHLD,
            libc::SIGINT,
            libc::SIGTERM,
            libc::SIGCONT
        ]
    );
    assert!(caught.is_empty());
    assert!(
        finish_signal_restore(&mut caught, || Ok(()))
            .unwrap()
            .is_empty(),
        "a successful completion owns the bytes exactly once"
    );
}
