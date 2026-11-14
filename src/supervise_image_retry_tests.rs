// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Pure transaction controls: dispatch recording never executes an image, and
//! retry observations advance an explicit Instant without sleeping.

use super::*;

fn envelope() -> Envelope {
    Envelope {
        prefix: "/private/prefix".into(),
        old_build: 11,
        new_build: 22,
        deadline_ns: 987_654_321_123,
        revisions: [[1, 2, 3, 4, 5], [6, 7, 8, 9, 10]],
        retry_step: 0,
        context: HostContext {
            sid: "s-abcdef".into(),
            binding: vec![0, 255, 1],
        },
        original: vec![0, 255, 10, 0, 13, 128],
    }
}

#[test]
fn returned_frame_rollback_cannot_dispatch_again_after_safe_recovery() {
    let original = envelope();
    let bytes = original.encode().unwrap();
    let mut sent = Vec::new();
    let mut frame_attempt = RollbackAttempt::for_entry(Phase::Forward);
    assert!(
        frame_attempt.consume(),
        "the forward entry may make its sole rollback attempt"
    );
    sent.push((original.deadline_ns, original.original.clone()));
    assert!(!frame_attempt.consume(), "the attempt is consumed once");

    // This is the same Frame -> Runtime recovery construction used after the
    // returned rollback exec, retaining the consumed attempt and original data.
    let mut recovery = Recovery::from_entry(original, Phase::Forward, None, frame_attempt);
    for decision in [
        CommitOutcome::Unknown,
        CommitOutcome::ProvenNotCommittedRollbackSafe,
        CommitOutcome::ProvenNotCommittedRollbackSafe,
    ] {
        assert!(
            recovery
                .dispatch_rollback(decision, |envelope, _| {
                    sent.push((envelope.deadline_ns, envelope.original.clone()));
                    io::Error::from_raw_os_error(libc::ENOENT)
                })
                .is_none(),
            "a later safe reply cannot renew a consumed rollback attempt"
        );
    }
    assert_eq!(sent, [(987_654_321_123, vec![0, 255, 10, 0, 13, 128])]);
    assert_eq!(recovery.envelope.encode().unwrap(), bytes);

    // A fresh forward entry still permits one rollback, but only on the
    // explicit same-operation safe outcome. Unknown consumes no attempt.
    let mut fresh = Recovery::from_entry(
        envelope(),
        Phase::Forward,
        None,
        RollbackAttempt::for_entry(Phase::Forward),
    );
    assert!(
        fresh
            .dispatch_rollback(CommitOutcome::Unknown, |_, _| {
                panic!("Unknown must not dispatch")
            })
            .is_none()
    );
    assert!(!fresh.rollback_attempt.consumed);
    let mut allowed = 0;
    assert!(
        fresh
            .dispatch_rollback(CommitOutcome::ProvenNotCommittedRollbackSafe, |saved, _| {
                allowed += 1;
                assert_eq!(saved.encode().unwrap(), bytes);
                io::Error::from_raw_os_error(libc::ENOENT)
            })
            .is_some()
    );
    assert!(
        fresh
            .dispatch_rollback(CommitOutcome::ProvenNotCommittedRollbackSafe, |_, _| {
                allowed += 1;
                io::Error::from_raw_os_error(libc::ENOENT)
            })
            .is_none()
    );
    assert_eq!(allowed, 1);

    let mut old_entry = Recovery::from_entry(
        envelope(),
        Phase::Rollback,
        None,
        RollbackAttempt::for_entry(Phase::Rollback),
    );
    assert!(old_entry.rollback_attempt.consumed);
    assert!(
        old_entry
            .dispatch_rollback(CommitOutcome::ProvenNotCommittedRollbackSafe, |_, _| {
                panic!("an old-image entry cannot recursively roll back")
            })
            .is_none()
    );
}

#[test]
fn rollback_ready_defers_both_retries_while_forward_ready_stays_normal() {
    let now = Instant::now();
    let mut probe = Backoff::default();
    let mut attempt = Backoff::default();
    defer_rollback_retries(Phase::Rollback, now, &mut probe, &mut attempt);
    for retry in [&probe, &attempt] {
        assert!(!retry.ready(now));
        assert!(!retry.ready(now + RETRY_FIRST - Duration::from_nanos(1)));
        assert!(retry.ready(now + RETRY_FIRST));
    }

    // The recovery path uses the same rule on existing retry state; it does
    // not reset an already growing delay or manufacture a new transaction.
    let later = now + RETRY_FIRST;
    defer_rollback_retries(Phase::Rollback, later, &mut probe, &mut attempt);
    for retry in [&probe, &attempt] {
        assert!(!retry.ready(later + RETRY_FIRST));
        assert!(retry.ready(later + RETRY_FIRST * 2));
    }

    let mut forward_probe = Backoff::default();
    let mut forward_attempt = Backoff::default();
    defer_rollback_retries(
        Phase::Forward,
        now,
        &mut forward_probe,
        &mut forward_attempt,
    );
    assert!(forward_probe.ready(now) && forward_attempt.ready(now));
    assert!(forward_probe.after.is_none() && forward_attempt.after.is_none());
}

/// A target that passes its probe and fails after every exec: each round trip
/// (attempt, forward exec, rollback into the old image) starts a new image
/// state, and the attempt schedule must go on from the step the transaction
/// carried, not from the start.
#[test]
fn the_attempt_schedule_keeps_doubling_across_rollback_round_trips() {
    assert_eq!(RETRY_LAST_STEP, 4, "30 s doubles four times to pass 5 min");
    let mut now = Instant::now();
    // What the next transaction's envelope carries: the old image's step.
    let mut carried = Backoff::default().step;
    for wait in [30, 60, 120, 240, 300, 300, 300] {
        let (probe, attempt) = entry_retries(Phase::Rollback, carried, true, now);
        let wait = Duration::from_secs(wait);
        assert!(
            !attempt.ready(now + wait - Duration::from_nanos(1)),
            "{wait:?}"
        );
        assert!(attempt.ready(now + wait), "{wait:?}");
        // The probe is only deferred once: it is the attempt that is paced.
        assert!(!probe.ready(now + RETRY_FIRST - Duration::from_nanos(1)));
        assert!(probe.ready(now + RETRY_FIRST));
        now += wait;
        carried = attempt.step;
        // The step travels in the envelope and comes back as it was sent.
        let mut sent = envelope();
        sent.retry_step = carried;
        assert_eq!(
            Envelope::decode(&sent.encode().unwrap())
                .unwrap()
                .retry_step,
            carried
        );
    }
    assert_eq!(carried, RETRY_LAST_STEP);

    // A rollback whose proof is still pending defers later, from the same step.
    let (probe, mut attempt) = entry_retries(Phase::Rollback, 2, false, now);
    assert!(probe.ready(now) && attempt.ready(now));
    let mut probe = probe;
    defer_rollback_retries(Phase::Rollback, now, &mut probe, &mut attempt);
    assert!(!attempt.ready(now + Duration::from_secs(119)));
    assert!(attempt.ready(now + Duration::from_secs(120)));

    // The image the target now runs in starts both schedules afresh.
    for ready in [false, true] {
        let (probe, attempt) = entry_retries(Phase::Forward, RETRY_LAST_STEP, ready, now);
        assert!(probe.ready(now) && attempt.ready(now));
        assert_eq!((probe.step, attempt.step), (0, 0));
    }
    // A step no schedule reaches is never a shorter wait.
    assert!(Backoff::at_step(RETRY_LAST_STEP + 1).is_none());
    let (_, attempt) = entry_retries(Phase::Rollback, u32::MAX, true, now);
    assert!(!attempt.ready(now + RETRY_MAX - Duration::from_nanos(1)));
    assert!(attempt.ready(now + RETRY_MAX));
}

fn retry_image(build: u64) -> Image {
    let directory = PathBuf::from(format!("/retry/store/claude-code-alab/{build}"));
    Image {
        prefix: "/retry".into(),
        build,
        executable: procs::Executable {
            path: directory.join("bin/claude-alab"),
            device: 1,
            inode: build,
        },
        directory,
        version: cli::VERSION.into(),
        file_revision: [build; 5],
    }
}

/// The carried schedule belongs to the target that earned it: the same target
/// selected again keeps it, and another target starts afresh.
#[test]
fn a_carried_retry_schedule_resets_when_the_selected_target_changes() {
    let now = Instant::now();
    let rolled_back = || {
        let (probe_retry, attempt_retry) = entry_retries(Phase::Rollback, 3, true, now);
        Runtime {
            // As `continue_supervising` leaves it after a rollback.
            seen: Some(retry_image(2)),
            probe_retry,
            attempt_retry,
            ..Runtime::default()
        }
    };
    let mut same = rolled_back();
    same.select(&retry_image(2));
    assert!(!same.attempt_retry.ready(now + Duration::from_secs(239)));
    assert!(same.attempt_retry.ready(now + Duration::from_secs(240)));
    assert!(!same.probe_retry.ready(now));
    assert_eq!(same.attempt_retry.step, 4);

    let mut other = rolled_back();
    other.select(&retry_image(3));
    assert!(other.attempt_retry.ready(now) && other.probe_retry.ready(now));
    assert_eq!((other.attempt_retry.step, other.probe_retry.step), (0, 0));
    assert_eq!(other.seen, Some(retry_image(3)));
}
