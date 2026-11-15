// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! BSD state survives an unavailable later session read; live identity does not.

use super::*;

fn snapshot() -> libc::proc_bsdinfo {
    libc::proc_bsdinfo {
        pbi_pid: 42,
        pbi_ppid: std::process::id(),
        pbi_pgid: 42,
        pbi_start_tvsec: 123,
        pbi_start_tvusec: 456,
        pbi_status: libc::SRUN,
        ..libc::proc_bsdinfo::default()
    }
}

fn expected() -> ChildIdentity {
    ChildIdentity {
        pid: 42,
        parent: std::process::id(),
        pgid: 42,
        session: 24,
        birth: Birth::MacOs {
            seconds: 123,
            microseconds: 456,
        },
        executable: Executable {
            path: "/unexecuted-native-fixture".into(),
            device: 1,
            inode: 2,
        },
    }
}

#[test]
fn a_missing_later_session_keeps_the_original_bsd_lifetime_and_state() {
    let read = kernel_identity_from_bsd(&snapshot(), |pid| {
        assert_eq!(pid, 42);
        None
    })
    .expect("missing optional session cannot erase the BSD observation");
    assert_eq!(read.pid, 42);
    assert_eq!(read.parent, std::process::id());
    assert_eq!(read.birth, expected().birth);
    assert_eq!(read.state, ChildState::Live);
    assert_eq!(read.pgid, Some(42));
    assert_eq!(read.session, None);
}

#[test]
fn a_missing_session_still_refuses_live_child_identity() {
    let expected = expected();
    for session in [None, Some(0), Some(25)] {
        let read = kernel_identity_from_bsd(&snapshot(), |_| session).unwrap();
        assert!(!read.matches(&expected));
        assert!(
            observe_child_using(
                &expected,
                |_| Some(read.clone()),
                |_| Some(expected.executable.clone()),
                |_| true,
            )
            .is_none()
        );
    }
    let read = kernel_identity_from_bsd(&snapshot(), |_| Some(expected.session)).unwrap();
    assert!(read.matches(&expected));
    assert!(
        observe_child_using(
            &expected,
            |_| Some(read.clone()),
            |_| Some(expected.executable.clone()),
            |_| true,
        )
        .is_some()
    );
}

#[test]
fn an_unreaped_bsd_snapshot_never_replaces_its_unknown_session() {
    let mut info = snapshot();
    info.pbi_status = libc::SZOMB;
    let read = kernel_identity_from_bsd(&info, |_| {
        panic!("an exited BSD observation must not query a live session")
    })
    .unwrap();
    assert_eq!(read.state, ChildState::Unreaped);
    assert_eq!(read.session, None);
    info.pbi_status = u32::MAX;
    assert!(kernel_identity_from_bsd(&info, |_| Some(24)).is_none());
}
