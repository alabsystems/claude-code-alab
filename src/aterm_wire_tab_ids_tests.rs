// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::{Connection, Locator, session_id};
use crate::aterm::{Scope, claim_verb, info_verb, policy_verb, release_verb, status_verb};
use std::io::{BufRead as _, BufReader, Write as _};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::time::{Duration, Instant};

#[test]
fn current_and_retained_tab_ids_locate_claim_and_subscribe_to_the_exact_tab() {
    for sid in ["tqn5r6e0148", "t0000000000", "s-0000000000004d7a2a97"] {
        let locator = Locator::new(PathBuf::from("/private-runtime"), sid).unwrap();
        assert_eq!(locator.sid(), sid);
        for request in [
            claim_verb(sid, 4242, &[Scope::Upgrade], "0.3.0", None),
            release_verb(sid, 4242),
            policy_verb(sid),
            status_verb(sid),
            info_verb(sid),
        ] {
            assert!(request.unwrap().starts_with(&format!("@{sid} ")));
        }

        let (client, mut peer) = UnixStream::pair().unwrap();
        let bound = Duration::from_secs(60);
        peer.set_read_timeout(Some(bound)).unwrap();
        peer.set_write_timeout(Some(bound)).unwrap();
        let server = std::thread::spawn(move || {
            let mut request = String::new();
            BufReader::new(&mut peer).read_line(&mut request).unwrap();
            assert_eq!(request, format!("subscribe @{sid} events\n"));
            writeln!(peer, "OK subscribe 1\nsub 7 {sid}").unwrap();
        });
        let reader = Connection::new(client, bound)
            .unwrap()
            .subscribe_until(sid, Instant::now() + bound)
            .unwrap();
        drop(reader);
        server.join().unwrap();
    }
}

#[test]
fn malformed_ids_never_become_a_locator_or_an_addressed_request() {
    for sid in [
        "",
        "tqn5r6e014",
        "tqn5r6e01488",
        "Tqn5r6e0148",
        "tqn5R6e0148",
        "tqn5r6e014i",
        "tqn5r6e014l",
        "tqn5r6e014o",
        "tqn5r6e014u",
        "s-4d7a2a97",
        "s-0000000000004D7a2a97",
        "s-00000000000004d7a2a97",
        "@tqn5r6e0148",
        "../tqn5r6e0148",
        "tqn5r6e0148 status",
        "tqn5r6e0148\n@t0000000000",
    ] {
        assert!(!session_id(sid), "{sid:?}");
        assert!(Locator::new(PathBuf::from("/private-runtime"), sid).is_err());
        assert!(claim_verb(sid, 4242, &[Scope::Upgrade], "0.3.0", None).is_none());
        assert!(release_verb(sid, 4242).is_none());
        assert!(policy_verb(sid).is_none());
        assert!(status_verb(sid).is_none());
        assert!(info_verb(sid).is_none());
    }
}
