// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

#[test]
fn dimensions_and_serials_do_not_admit_zero_or_reuse() {
    assert!(Size::new(0, 80).is_err());
    assert!(Size::new(24, 0).is_err());
    assert!(Size::new(24, 80).is_ok());
    assert_ne!(next_generation().unwrap(), next_generation().unwrap());
}

#[test]
fn poll_deadline_already_elapsed_is_nonblocking() {
    let before = Instant::now();
    relay::poll_until(&mut [], before).unwrap();
    assert!(before.elapsed() < std::time::Duration::from_secs(60));
}

#[test]
fn interruptions_consume_one_monotonic_budget_and_clear_spurious_events() {
    let start = Instant::now();
    let mut ticks = [0, 10, 20, 30].into_iter();
    let mut timeouts = Vec::new();
    let mut fds = [libc::pollfd {
        fd: -1,
        events: 0,
        revents: 0,
    }];
    relay::poll_until_with(
        &mut fds,
        start + std::time::Duration::from_millis(30),
        || start + std::time::Duration::from_millis(ticks.next().unwrap()),
        |fds, timeout| {
            timeouts.push(timeout);
            fds[0].revents = libc::POLLIN;
            Err(io::Error::from(io::ErrorKind::Interrupted))
        },
    )
    .unwrap();
    assert_eq!(timeouts, [30, 10]);
    assert_eq!(fds[0].revents, 0);
}

#[test]
fn multithreaded_allocation_refuses_and_restores_the_callers_whole_signal_mask() {
    fn mask() -> Vec<i32> {
        // SAFETY: native storage is initialized by the pure mask query below.
        let mut native = unsafe { std::mem::zeroed() };
        // SAFETY: null input queries this thread's mask into valid storage.
        assert_eq!(
            // SAFETY: pure query into initialized writable native storage.
            unsafe { libc::pthread_sigmask(libc::SIG_BLOCK, std::ptr::null(), &raw mut native) },
            0
        );
        (1..=64)
            .map(|signal| {
                // SAFETY: initialized mask, integer signal query; out-of-range
                // numbers return -1 identically on both sides of the transaction.
                unsafe { libc::sigismember(&raw const native, signal) }
            })
            .collect()
    }
    std::thread::scope(|scope| {
        let (hold, held) = std::sync::mpsc::channel::<()>();
        scope.spawn(move || {
            let _ = held.recv();
        });
        let before = mask();
        let failure = PtyPair::allocate(
            Size::new(24, 80).unwrap(),
            Instant::now() + std::time::Duration::from_secs(60),
        )
        .unwrap_err();
        let after = mask();
        drop(hold);
        assert!(failure.pending.is_none());
        assert_eq!(before, after);
    });
}
