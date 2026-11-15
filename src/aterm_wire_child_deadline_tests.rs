// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

std::thread_local! {
    static TRANSPORT_NOW: std::cell::Cell<Option<Instant>> = const { std::cell::Cell::new(None) };
}

fn transport_now() -> Instant {
    TRANSPORT_NOW
        .get()
        .expect("fixture sets the transport clock")
}

#[test]
fn c3_barrier_keeps_its_original_deadline_after_transport_preemption() {
    let budget_read = Instant::now();
    let until = budget_read + Duration::from_millis(500);
    let (stream, mut peer) = UnixStream::pair().unwrap();
    let mut conn = Connection::new(stream, Duration::from_secs(60)).unwrap();
    TRANSPORT_NOW.set(Some(budget_read + Duration::from_millis(700)));
    conn.clock = transport_now;
    peer.write_all(b"OK\n").unwrap();

    let result = crate::supervise::child_start_ask_at(
        &mut conn,
        until,
        budget_read,
        "preemption-control",
        Some("status".into()),
    );
    assert!(
        result.is_err(),
        "an expired original budget cannot be rebased"
    );
    assert!(conn.is_poisoned());
    peer.set_nonblocking(true).unwrap();
    let mut received = [0; 32];
    assert_eq!(
        peer.read(&mut received).unwrap_err().kind(),
        io::ErrorKind::WouldBlock
    );
}

#[test]
fn c3_barrier_accepts_a_reply_within_the_same_original_budget() {
    let budget_read = Instant::now();
    let until = budget_read + Duration::from_millis(500);
    let (stream, mut peer) = UnixStream::pair().unwrap();
    let mut conn = Connection::new(stream, Duration::from_secs(60)).unwrap();
    TRANSPORT_NOW.set(Some(budget_read));
    conn.clock = transport_now;
    peer.write_all(b"OK\n").unwrap();

    let reply = crate::supervise::child_start_ask_at(
        &mut conn,
        until,
        budget_read,
        "within-budget-control",
        Some("status".into()),
    )
    .unwrap();
    assert_eq!(reply, "OK");
    assert!(!conn.is_poisoned());
    peer.set_nonblocking(true).unwrap();
    let mut received = [0; 32];
    let length = peer.read(&mut received).unwrap();
    assert_eq!(&received[..length], b"status\n");
}
