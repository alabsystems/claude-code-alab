// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! A restart event that names its cause but cannot be acted on is passed
//! over; only one nobody can name faults the stream.

use super::{Connection, EventReader, deadline};
use std::io::Write as _;
use std::os::unix::net::UnixStream;
use std::time::Duration;

const END: Duration = Duration::from_secs(60);
const SID: &str = "tqn5r6e0148";

/// A subscription to `SID` whose first event lines are `lines`, and the
/// host's end of it, kept open so the stream does not end.
fn subscribed(lines: &[u8]) -> (EventReader, UnixStream) {
    let (client, mut peer) = UnixStream::pair().expect("Unix pair");
    peer.set_read_timeout(Some(END))
        .expect("peer read detector");
    peer.set_write_timeout(Some(END))
        .expect("peer write detector");
    let connection = Connection::new(client, END).expect("connection");
    peer.write_all(b"OK subscribe 1\nsub 7 tqn5r6e0148 role=- attention=- user_title=-\n")
        .unwrap();
    peer.write_all(lines).unwrap();
    let events = connection
        .subscribe_until(SID, deadline(END).unwrap())
        .unwrap();
    (events, peer)
}

#[test]
fn an_account_restart_whose_account_cannot_be_read_is_passed_over_and_the_stream_stands() {
    let (mut events, mut peer) = subscribed(
        b"EVENT 7 restart cause=asked\nEVENT 7 restart cause=account to=work\nEVENT 7 human\n",
    );
    let update = events.drain();
    assert!(update.fault.is_none() && !update.ended && !update.requires_refresh());
    assert_eq!(update.unacted, Some(("account".to_string(), "account")));
    // The readable restart before it stands, and what follows it is read.
    assert_eq!(
        update.restart.map(|restart| restart.cause).as_deref(),
        Some("asked")
    );
    assert!(update.activity);
    for (line, unacted) in [
        (
            b"EVENT 7 restart cause=model model=-x\n".as_slice(),
            ("model", "model"),
        ),
        (
            b"EVENT 7 restart cause=account-ahead to=work to=play home=-\n".as_slice(),
            ("account-ahead", "to"),
        ),
    ] {
        peer.write_all(line).unwrap();
        let update = events.drain();
        assert!(update.fault.is_none() && !update.ended && update.restart.is_none());
        assert_eq!(
            update.unacted,
            Some((unacted.0.to_string(), unacted.1)),
            "{}",
            String::from_utf8_lossy(line)
        );
    }
    peer.write_all(b"EVENT 7 harness-policy gen=4\n").unwrap();
    let later = events.drain();
    assert!(later.policy_changed && later.fault.is_none() && later.unacted.is_none());

    // Negative control: a restart nobody can name still faults the stream.
    let (mut events, _peer) = subscribed(b"EVENT 7 restart to=work cause=account\n");
    let update = events.drain();
    assert!(update.fault.is_some() && update.ended && update.requires_refresh());
    assert!(update.unacted.is_none() && update.restart.is_none());
}
