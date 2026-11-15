// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Single-threaded real-descriptor controls for unwired transport primitives.

use claude_code_alab::inner_pty::{
    AllocationControl, Delivery, Limits, Outer, PtyPair, Relay, Size,
};
use std::io;
use std::os::fd::{AsFd, AsRawFd, BorrowedFd};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

const HANG: Duration = Duration::from_secs(60);
static REAPED: AtomicUsize = AtomicUsize::new(0);

extern "C" fn reap(_: libc::c_int) {
    loop {
        let mut status = 0;
        // SAFETY: this isolated negative-control handler deliberately steals waits.
        let pid = unsafe { libc::waitpid(-1, &raw mut status, libc::WNOHANG) };
        if pid <= 0 {
            break;
        }
        REAPED.fetch_add(1, Ordering::Relaxed);
    }
}

fn pair() -> PtyPair {
    PtyPair::allocate(Size::new(24, 80).unwrap(), Instant::now() + HANG)
        .unwrap_or_else(|failure| panic!("allocation failed: {failure:?}"))
}

fn raw(fd: BorrowedFd<'_>) {
    // SAFETY: this process-isolated test owns a fresh slave and all native storage.
    unsafe {
        let mut modes = std::mem::zeroed();
        assert_eq!(libc::tcgetattr(fd.as_raw_fd(), &raw mut modes), 0);
        libc::cfmakeraw(&raw mut modes);
        assert_eq!(
            libc::tcsetattr(fd.as_raw_fd(), libc::TCSANOW, &raw const modes),
            0
        );
    }
}

fn write(fd: BorrowedFd<'_>, bytes: &[u8]) -> usize {
    // SAFETY: fd is live; bytes remains readable for its exact length.
    let n = unsafe { libc::write(fd.as_raw_fd(), bytes.as_ptr().cast(), bytes.len()) };
    assert!(n >= 0, "write: {}", io::Error::last_os_error());
    n as usize
}

fn read(fd: BorrowedFd<'_>) -> Vec<u8> {
    let mut bytes = [0; 8192];
    // SAFETY: fd is live and nonblocking; bytes is writable bounded storage.
    let n = unsafe { libc::read(fd.as_raw_fd(), bytes.as_mut_ptr().cast(), bytes.len()) };
    if n < 0 {
        assert_eq!(io::Error::last_os_error().kind(), io::ErrorKind::WouldBlock);
        Vec::new()
    } else {
        bytes[..n as usize].to_vec()
    }
}

fn step(
    relay: &mut Relay<&'static str>,
    delivery: Delivery,
) -> claude_code_alab::inner_pty::Step<&'static str> {
    relay
        .poll_step(
            Duration::ZERO,
            &[],
            || "terminal-unattributed",
            |_| delivery,
        )
        .unwrap()
}

fn setup(
    limits: Limits,
) -> (
    PtyPair,
    Relay<&'static str>,
    claude_code_alab::inner_pty::Slave,
) {
    let outer = pair();
    let (master, slave) = outer.fixture_descriptors();
    raw(slave);
    // Allocation made master nonblocking; reopening must preserve slave flags.
    // SAFETY: F_GETFL queries a live borrowed descriptor.
    let before = unsafe { libc::fcntl(slave.as_raw_fd(), libc::F_GETFL) };
    let opened = Outer::reopen(slave, slave).unwrap();
    // SAFETY: same live borrowed descriptor, query only.
    assert_eq!(
        // SAFETY: this query borrows the same live original description.
        unsafe { libc::fcntl(slave.as_raw_fd(), libc::F_GETFL) },
        before
    );
    assert!(before & libc::O_NONBLOCK == 0);
    let _ = master;
    let (relay, inner) = Relay::new(pair(), opened, limits).unwrap();
    raw(inner.as_fd());
    (outer, relay, inner)
}

fn allocation_controls() {
    let directory = if cfg!(target_os = "linux") {
        "/proc/self/fd"
    } else {
        "/dev/fd"
    };
    let before = std::fs::read_dir(directory).unwrap().count();
    for fault in [
        AllocationControl::Block,
        AllocationControl::Fail,
        AllocationControl::WrongCount,
        AllocationControl::Truncated,
        AllocationControl::TruncatedSweep,
    ] {
        eprintln!("allocator control: {fault:?}");
        let malformed = matches!(
            fault,
            AllocationControl::WrongCount
                | AllocationControl::Truncated
                | AllocationControl::TruncatedSweep
        );
        let result = PtyPair::fixture_allocate(
            Size::new(24, 80).unwrap(),
            Instant::now()
                + if malformed {
                    HANG
                } else {
                    Duration::from_millis(20)
                },
            fault,
        );
        let mut error = result.expect_err("fault must not allocate a pair");
        if malformed {
            assert_eq!(error.error.kind(), io::ErrorKind::InvalidInput);
        }
        if let Some(receipt) = error.pending.take() {
            (*receipt)
                .settle(Instant::now() + HANG)
                .expect("owned helper must settle");
        }
        assert_eq!(
            std::fs::read_dir(directory).unwrap().count(),
            before,
            "malformed handoff leaked descriptors"
        );
    }
}

fn active_reaper_never_grants_stale_kill() {
    // SAFETY: initialized native action storage, changed on this single thread.
    let mut action: libc::sigaction = unsafe { std::mem::zeroed() };
    // SAFETY: writable output for the previous native action.
    let mut previous = unsafe { std::mem::zeroed() };
    action.sa_sigaction = reap as *const () as usize;
    // SAFETY: construct an empty mask and retain the original disposition.
    unsafe {
        libc::sigemptyset(&raw mut action.sa_mask);
        assert_eq!(
            libc::sigaction(libc::SIGCHLD, &raw const action, &raw mut previous),
            0
        );
    }
    let failure = PtyPair::fixture_allocate(
        Size::new(24, 80).unwrap(),
        Instant::now() + HANG,
        AllocationControl::Reaper,
    )
    .unwrap_err();
    assert_eq!(REAPED.load(Ordering::Relaxed), 1);
    let receipt = failure
        .pending
        .expect("lost wait custody is not a settled success");
    assert!(receipt.ownership_lost());
    assert_eq!(receipt.pid(), None);
    let receipt = (*receipt)
        .settle(Instant::now() + HANG)
        .expect_err("lost custody cannot grant kill or settlement");
    assert!(receipt.ownership_lost());
    // SAFETY: restore exactly the previous process-isolated handler.
    assert_eq!(
        // SAFETY: previous holds the exact native action saved above.
        unsafe { libc::sigaction(libc::SIGCHLD, &raw const previous, std::ptr::null_mut()) },
        0
    );
}

fn stale_and_bounded_input() {
    let (_outer, mut relay, _inner) = setup(Limits {
        input_bytes: 4,
        input_chunks: 1,
        output_bytes: 4,
    });
    let generation = relay.generation();
    let stale = pair().generation();
    let refused = relay.enqueue(stale, "A", b"old".to_vec()).unwrap_err();
    assert_eq!((refused.origin, refused.bytes), ("A", b"old".to_vec()));
    relay.enqueue(generation, "A", b"1234".to_vec()).unwrap();
    let refused = relay.enqueue(generation, "B", b"5".to_vec()).unwrap_err();
    assert_eq!(refused.origin, "B");
    let held = step(&mut relay, Delivery::Hold);
    assert!(held.held);
    assert_eq!(held.written_to_inner, 0);
    assert_eq!(relay.pending(), (4, 0));
    let refused = step(&mut relay, Delivery::Refuse);
    assert_eq!(refused.retired.len(), 1);
    assert_eq!(refused.retired[0].origin, "A");
    assert_eq!(refused.retired[0].written, 0);
    assert_eq!(refused.retired[0].remaining, b"1234");
}

fn partial_authority_and_death() {
    let (_outer, mut relay, _inner) = setup(Limits {
        input_bytes: 1_048_576,
        ..Limits::default()
    });
    relay
        .enqueue(
            relay.generation(),
            "original-full-proof",
            vec![b'x'; 1_048_576],
        )
        .unwrap();
    let first = step(&mut relay, Delivery::Allow);
    assert!(first.written_to_inner > 0 && first.written_to_inner < 1_048_576);
    let held = step(&mut relay, Delivery::Hold);
    assert!(held.held && held.written_to_inner == 0);
    let refused = step(&mut relay, Delivery::Refuse);
    assert_eq!(refused.retired[0].origin, "original-full-proof");
    assert_eq!(refused.retired[0].written, first.written_to_inner);
    assert_eq!(
        refused.retired[0].remaining.len() + refused.retired[0].written,
        1_048_576
    );
    relay
        .enqueue(relay.generation(), "undeliverable", b"pending".to_vec())
        .unwrap();
    let dead = relay.target_died();
    assert_eq!(dead[0].remaining, b"pending");
    assert!(
        relay
            .enqueue(relay.generation(), "new", b"no".to_vec())
            .is_err()
    );
}

fn output_hup_drains_and_input_eof_retires() {
    let (outer, mut relay, inner) = setup(Limits {
        output_bytes: 8,
        ..Limits::default()
    });
    relay
        .enqueue(relay.generation(), "owed", b"do-not-deliver".to_vec())
        .unwrap();
    // The actual fixed writer writes and exits BEFORE the first pump. The
    // relay's passive anchor must preserve its unread Darwin kernel output.
    let writer = inner.fixture_writer(b"abcdefgh").unwrap();
    let proof = writer.finish(Instant::now() + HANG).unwrap();
    let mut retired = relay.producer_finished(proof).unwrap();
    let mut observed = Vec::new();
    let mut drained = None;
    let deadline = Instant::now() + HANG;
    while !relay.drained() {
        let result = relay
            .poll_step(Duration::from_millis(10), &[], || "raw", |_| Delivery::Hold)
            .unwrap();
        retired.extend(result.retired);
        drained = result.output_drained.or(drained);
        observed.extend(read(outer.fixture_descriptors().0));
        assert!(Instant::now() < deadline, "EOF/drain hang");
    }
    observed.extend(read(outer.fixture_descriptors().0));
    assert_eq!(observed, b"abcdefgh");
    assert_eq!(drained.unwrap().generation(), relay.generation());
    assert_eq!(retired.len(), 1);
    assert_eq!(retired[0].origin, "owed");
    assert_eq!(retired[0].written, 0);
    assert_eq!(retired[0].remaining, b"do-not-deliver");
}

fn settlement_is_generation_bound() {
    let (_one, mut relay_one, inner_one) = setup(Limits::default());
    let (_two, mut relay_two, inner_two) = setup(Limits::default());
    let proof = inner_one
        .fixture_writer(b"")
        .unwrap()
        .finish(Instant::now() + HANG)
        .unwrap();
    let proof = relay_two.producer_finished(proof).unwrap_err();
    assert!(!relay_one.drained() && !relay_two.drained());
    relay_one.producer_finished(proof).unwrap();
    let proof_two = inner_two
        .fixture_writer(b"")
        .unwrap()
        .finish(Instant::now() + HANG)
        .unwrap();
    relay_two.producer_finished(proof_two).unwrap();
}

fn terminal_error_keeps_original_receipts() {
    let (outer, mut relay, inner) = setup(Limits::default());
    write(inner.as_fd(), b"reply");
    step(&mut relay, Delivery::Allow);
    relay
        .enqueue(relay.generation(), "refused-before-poll", b"old".to_vec())
        .unwrap();
    drop(outer);
    let error = relay
        .poll_step(Duration::ZERO, &[], || "raw", |_| Delivery::Refuse)
        .unwrap_err();
    assert_eq!(error.step.retired.len(), 1);
    assert_eq!(error.step.retired[0].origin, "refused-before-poll");
    assert_eq!(error.step.retired[0].remaining, b"old");

    let (outer, mut relay, inner) = setup(Limits::default());
    write(inner.as_fd(), b"response");
    step(&mut relay, Delivery::Allow);
    assert_eq!(relay.pending().1, 8);
    relay
        .enqueue(relay.generation(), "completed-before-error", b"x".to_vec())
        .unwrap();
    let mut outer = Some(outer);
    let mut checks = 0;
    let error = relay
        .poll_step(
            Duration::ZERO,
            &[],
            || "raw",
            |_| {
                checks += 1;
                if checks == 2 {
                    outer.take();
                }
                Delivery::Allow
            },
        )
        .unwrap_err();
    assert_eq!(error.step.written.len(), 1);
    assert_eq!(error.step.written[0].origin, "completed-before-error");
    assert_eq!(error.step.written[0].bytes, 1);
    assert_eq!(relay.pending(), (0, 8));
}

fn live_output_backpressure_preserves_bytes() {
    let (outer, mut relay, inner) = setup(Limits {
        output_bytes: 4,
        ..Limits::default()
    });
    assert_eq!(write(inner.as_fd(), b"abcdefgh"), 8);
    let mut observed = Vec::new();
    let deadline = Instant::now() + HANG;
    while observed.len() < 8 {
        relay
            .poll_step(
                Duration::from_millis(10),
                &[],
                || "raw",
                |_| Delivery::Allow,
            )
            .unwrap();
        assert!(relay.pending().1 <= 4);
        observed.extend(read(outer.fixture_descriptors().0));
        assert!(Instant::now() < deadline, "live output backpressure hang");
    }
    assert_eq!(observed, b"abcdefgh");
}

fn mismatched_outer_is_refused() {
    let one = pair();
    let two = pair();
    assert!(Outer::reopen(one.fixture_descriptors().1, two.fixture_descriptors().1).is_err());
    assert!(Outer::reopen(one.fixture_descriptors().0, one.fixture_descriptors().0).is_err());
}

fn full_outer_backpressure_does_not_retire_output() {
    let (outer, mut relay, inner) = setup(Limits {
        output_bytes: 4,
        ..Limits::default()
    });
    let (_, slave) = outer.fixture_descriptors();
    // This is a fresh TEST tty, never inherited stdin. Fill its output driver
    // without a second reader or any production endpoint-adoption seam.
    // SAFETY: query/set flags only on the fixture's owned private description.
    unsafe {
        let flags = libc::fcntl(slave.as_raw_fd(), libc::F_GETFL);
        assert!(flags >= 0);
        assert_eq!(
            libc::fcntl(slave.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK),
            0
        );
    }
    let padding = [b'f'; 8192];
    let mut filled = 0;
    loop {
        // SAFETY: fresh nonblocking tty and exact live readable buffer.
        let n = unsafe { libc::write(slave.as_raw_fd(), padding.as_ptr().cast(), padding.len()) };
        if n < 0 {
            assert_eq!(io::Error::last_os_error().kind(), io::ErrorKind::WouldBlock);
            break;
        }
        assert!(n > 0);
        filled += n as usize;
        assert!(filled <= 1_048_576, "fixture tty did not backpressure");
    }
    relay
        .enqueue(relay.generation(), "dead-input", b"no".to_vec())
        .unwrap();
    let proof = inner
        .fixture_writer(b"abcdefgh")
        .unwrap()
        .finish(Instant::now() + HANG)
        .unwrap();
    let retired = relay.producer_finished(proof).unwrap();
    assert_eq!(retired[0].origin, "dead-input");
    step(&mut relay, Delivery::Allow);
    assert_eq!(relay.pending(), (0, 4));
    let blocked = step(&mut relay, Delivery::Allow);
    assert_eq!(blocked.written_to_outer, 0);
    assert!(!blocked.inner_eof && !relay.drained());
    let mut removed = 0;
    loop {
        let bytes = read(outer.fixture_descriptors().0);
        if bytes.is_empty() {
            break;
        }
        assert!(bytes.iter().all(|byte| *byte == b'f'));
        removed += bytes.len();
    }
    assert_eq!(removed, filled);
    let mut output = Vec::new();
    let deadline = Instant::now() + HANG;
    while !relay.drained() {
        relay
            .poll_step(
                Duration::from_millis(10),
                &[],
                || "raw",
                |_| Delivery::Allow,
            )
            .unwrap();
        output.extend(read(outer.fixture_descriptors().0));
        assert!(Instant::now() < deadline, "backpressure/settlement hang");
    }
    output.extend(read(outer.fixture_descriptors().0));
    assert_eq!(output, b"abcdefgh");
}

fn extra_wake_bound_preserves_queued_origin() {
    let (_outer, mut relay, _inner) = setup(Limits::default());
    let (wake, _peer) = std::os::unix::net::UnixStream::pair().unwrap();
    let too_many = vec![wake.as_fd(); 33];
    relay
        .enqueue(relay.generation(), "preserved", b"pending".to_vec())
        .unwrap();
    let error = relay
        .poll_step(Duration::ZERO, &too_many, || "raw", |_| Delivery::Refuse)
        .unwrap_err();
    assert!(error.step.retired.is_empty());
    assert_eq!(relay.pending(), (7, 0));
    assert_eq!(
        step(&mut relay, Delivery::Refuse).retired[0].origin,
        "preserved"
    );
}

fn driver_receipt_precedes_fixture_consumption() {
    let (outer, mut relay, inner) = setup(Limits::default());
    assert_eq!(write(outer.fixture_descriptors().0, b"typed"), 5);
    let read = relay
        .poll_step(HANG, &[], || "terminal-unattributed", |_| Delivery::Allow)
        .unwrap();
    assert_eq!(read.read_from_outer, 5);
    assert_eq!(read.written_to_inner, 0);
    let delivered = step(&mut relay, Delivery::Allow);
    assert_eq!(delivered.written[0].origin, "terminal-unattributed");
    assert_eq!(delivered.written[0].bytes, 5);
    let mut unread = 0;
    // SAFETY: FIONREAD writes one native integer for this live tty; this is a
    // test fact, deliberately NOT a native process/consumption proof.
    assert_eq!(
        // SAFETY: live tty and writable integer, as described above.
        unsafe { libc::ioctl(inner.as_fd().as_raw_fd(), libc::FIONREAD, &raw mut unread) },
        0
    );
    assert_eq!(
        unread, 5,
        "driver completion still has unconsumed fixture input"
    );
    let mut bytes = [0u8; 5];
    // SAFETY: this sole fixture reader owns the live slave; FIONREAD above
    // established all5 bytes queued, with no other reader or thread present.
    assert_eq!(
        // SAFETY: sole reader and all5 queued bytes, as established above.
        unsafe {
            libc::read(
                inner.as_fd().as_raw_fd(),
                bytes.as_mut_ptr().cast(),
                bytes.len(),
            )
        },
        5
    );
    assert_eq!(&bytes, b"typed");
}

fn stale_native_lifetime_cannot_kill_or_reap_replacement() {
    let (_outer_a, _relay_a, inner_a) = setup(Limits::default());
    let old = inner_a.fixture_writer(b"").unwrap();
    let (_outer_b, _relay_b, inner_b) = setup(Limits::default());
    let mut replacement = inner_b.fixture_paused_writer().unwrap();
    assert!(replacement.alive());
    let stale = old
        .stale_receipt_against(&replacement, Instant::now() + HANG)
        .unwrap();
    let stale = stale
        .settle(Instant::now() + HANG)
        .expect_err("a stale native birth cannot acquire replacement wait/kill authority");
    assert!(stale.ownership_lost());
    assert_eq!(stale.pid(), None);
    assert!(
        replacement.alive(),
        "stale receipt killed or reaped the replacement"
    );
    replacement
        .kill(Instant::now() + HANG)
        .expect("genuine replacement owner settles exact child");
}

fn probe_custody_excludes_arbitrary_reapers_and_never_becomes_settled_when_lost() {
    use claude_code_alab::sys;
    use std::path::Path;
    REAPED.store(0, Ordering::Relaxed);
    // SAFETY: process-isolated initialized native action storage.
    let mut action: libc::sigaction = unsafe { std::mem::zeroed() };
    // SAFETY: native output for the exact disposition restored below.
    let mut previous: libc::sigaction = unsafe { std::mem::zeroed() };
    action.sa_sigaction = reap as *const () as usize;
    // SAFETY: single-threaded negative control deliberately installs an unmanaged reaper.
    unsafe {
        libc::sigemptyset(&raw mut action.sa_mask);
        assert_eq!(
            libc::sigaction(libc::SIGUSR1, &raw const action, &raw mut previous),
            0
        );
    }
    let mut first = sys::ProbeChild::spawn(Path::new("/usr/bin/true"), &[], &[]).unwrap();
    let until = Instant::now() + HANG;
    while !sys::child_has_ended(first.pid().unwrap()) {
        assert!(Instant::now() < until, "probe terminal hang detector");
        std::thread::sleep(Duration::from_millis(2));
    }
    // SAFETY: send only to this isolated process's explicitly installed handler.
    unsafe {
        libc::raise(libc::SIGUSR1);
    }
    assert_eq!(REAPED.load(Ordering::Relaxed), 1);
    assert!(first.try_wait().is_err());
    assert!(first.cancel().is_err());
    assert!(
        first.pending(),
        "stolen wait is never a successful settlement"
    );
    assert!(first.observe_native(|_| true).is_err());
    // SAFETY: restore the exact original native disposition before the positive child.
    unsafe {
        assert_eq!(
            libc::sigaction(libc::SIGUSR1, &raw const previous, std::ptr::null_mut()),
            0
        );
    }
    let mut second = sys::ProbeChild::spawn(Path::new("/bin/sleep"), &["30".into()], &[]).unwrap();
    assert!(first.cancel().is_err());
    while !second.exec_observed().unwrap() {
        assert!(
            Instant::now() < until,
            "positive probe exec-completion hang detector"
        );
        std::thread::sleep(Duration::from_millis(2));
    }
    assert_eq!(
        second
            .observe_native(claude_code_alab::procs::capture_process)
            .unwrap()
            .unwrap()
            .pid,
        second.pid().unwrap() as u32
    );
    assert!(
        second.try_wait().unwrap().is_none(),
        "stale probe signalled a later child"
    );
    second.cancel().unwrap();
    while second.try_wait().unwrap().is_none() {
        assert!(Instant::now() < until, "positive native wait hang detector");
        std::thread::sleep(Duration::from_millis(2));
    }
    assert!(!second.pending());
}

fn mask() -> libc::sigset_t {
    // SAFETY: initialized native mask storage and a query on this test's thread.
    let mut value = unsafe { std::mem::zeroed() };
    // SAFETY: null input only fills value with the current thread's mask.
    assert_eq!(
        // SAFETY: value is initialized writable native mask storage for this thread.
        unsafe { libc::pthread_sigmask(libc::SIG_BLOCK, std::ptr::null(), &raw mut value) },
        0
    );
    value
}

fn same_mask(before: libc::sigset_t, after: libc::sigset_t) {
    for signal in 1..=128 {
        // SAFETY: successful mask queries initialized both sets; invalid signal
        // numbers return -1 in both without inspecting native padding bytes.
        unsafe {
            assert_eq!(
                libc::sigismember(&raw const before, signal),
                libc::sigismember(&raw const after, signal)
            );
        }
    }
}

fn closed_standard_input_refuses_before_fork_and_restores_the_mask() {
    use claude_code_alab::sys;
    use std::os::fd::{FromRawFd as _, OwnedFd};
    let before = mask();
    // SAFETY: preserve this isolated process's stdin as a private >=3 descriptor.
    let saved = unsafe { libc::fcntl(0, libc::F_DUPFD_CLOEXEC, 3) };
    assert!(saved >= 3);
    // SAFETY: this new descriptor has exactly one Rust owner.
    let saved = unsafe { OwnedFd::from_raw_fd(saved) };
    // SAFETY: query the live stdin before closing only this test process's descriptor.
    let flags = unsafe { libc::fcntl(0, libc::F_GETFD) };
    // SAFETY: saved owns the same open file description for exact restoration.
    assert_eq!(unsafe { libc::close(0) }, 0);
    let refused =
        sys::ProbeChild::spawn(std::path::Path::new("/usr/bin/true"), &[], &[]).unwrap_err();
    // SAFETY: restore the saved stdin and precisely its original descriptor flags.
    unsafe {
        assert_eq!(libc::dup2(saved.as_raw_fd(), 0), 0);
        assert_eq!(libc::fcntl(0, libc::F_SETFD, flags), 0);
    }
    assert!(
        refused.child.is_none() && refused.signals.is_none(),
        "closed stdio must refuse and settle before fork"
    );
    same_mask(before, mask());
}

fn a_panicking_native_read_restores_mask_and_keeps_child_custody() {
    use claude_code_alab::sys;
    let mut child =
        sys::ProbeChild::spawn(std::path::Path::new("/bin/sleep"), &["30".into()], &[]).unwrap();
    let before = mask();
    let refused = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _: io::Result<()> =
            child.observe_native(|_| panic!("native observation unwind control"));
    }));
    assert!(refused.is_err());
    same_mask(before, mask());
    assert!(child.pending() && child.try_wait().unwrap().is_none());
    child.cancel().unwrap();
    let until = Instant::now() + HANG;
    while child.try_wait().unwrap().is_none() {
        assert!(
            Instant::now() < until,
            "panic-control native wait hang detector"
        );
        std::thread::sleep(Duration::from_millis(2));
    }
    assert!(!child.pending());
}

fn probe_pipe_endpoints_preserve_child_stdio() {
    use claude_code_alab::sys::ProbeChild;
    let mut child = ProbeChild::spawn(
        &std::env::current_exe().unwrap(),
        &["--probe-stdio".into()],
        &[],
    )
    .unwrap();
    let readers_nonblocking = child.output_fds().all(|fd| {
        // SAFETY: these descriptors are borrowed from the still-owned probe.
        let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
        flags >= 0 && flags & libc::O_NONBLOCK != 0
    });
    let until = Instant::now() + HANG;
    let mut output = Vec::new();
    let mut errors = Vec::new();
    let result = (|| -> io::Result<()> {
        let mut ended = None;
        loop {
            child.drain_total(&mut output, &mut errors, 4096)?;
            if ended.is_none() {
                ended = child.try_wait()?;
            }
            if let Some(status) = ended
                && child.output_finished()
            {
                if status.success()
                    && readers_nonblocking
                    && output == b"blocking-probe-stdio\n"
                    && errors.is_empty()
                {
                    return Ok(());
                }
                return Err(io::Error::other("probe pipe endpoint flags differ"));
            }
            if Instant::now() >= until {
                return Err(io::Error::other("probe stdio hang detector"));
            }
            std::thread::sleep(Duration::from_millis(2));
        }
    })();
    if child.pending() {
        let cleanup = Instant::now() + HANG;
        if child.cancel().is_err() {
            std::mem::forget(child);
            std::process::exit(70);
        }
        while child.pending() {
            if child.try_wait().is_err() || Instant::now() >= cleanup {
                std::mem::forget(child);
                std::process::exit(70);
            }
            std::thread::sleep(Duration::from_millis(2));
        }
    }
    result.unwrap();
}

fn main() {
    if std::env::args().nth(1).as_deref() == Some("--probe-stdio") {
        let blocking = [1, 2].into_iter().all(|fd| {
            // SAFETY: native read of this isolated child role's standard output descriptors.
            let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
            flags >= 0 && flags & libc::O_NONBLOCK == 0
        });
        if !blocking {
            std::process::exit(2);
        }
        println!("blocking-probe-stdio");
        return;
    }
    claude_code_alab::sys::ensure_single_threaded().unwrap();
    allocation_controls();
    active_reaper_never_grants_stale_kill();
    stale_and_bounded_input();
    partial_authority_and_death();
    output_hup_drains_and_input_eof_retires();
    live_output_backpressure_preserves_bytes();
    mismatched_outer_is_refused();
    settlement_is_generation_bound();
    terminal_error_keeps_original_receipts();
    full_outer_backpressure_does_not_retire_output();
    extra_wake_bound_preserves_queued_origin();
    driver_receipt_precedes_fixture_consumption();
    stale_native_lifetime_cannot_kill_or_reap_replacement();
    probe_custody_excludes_arbitrary_reapers_and_never_becomes_settled_when_lost();
    closed_standard_input_refuses_before_fork_and_restores_the_mask();
    a_panicking_native_read_restores_mask_and_keeps_child_custody();
    probe_pipe_endpoints_preserve_child_stdio();
    eprintln!("inner_pty: 17 process-isolated controls passed");
}
