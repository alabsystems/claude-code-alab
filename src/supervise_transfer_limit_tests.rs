// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Where the session is with respect to Fable's limit crosses an own-image
//! move, and so do what the journal already says of it and the reset grace
//! option.

use super::tests::fixture;
use super::*;

/// The bytes the limit state takes at the end of a payload: a phase tag and
/// its word (one byte and eight, or one and one), then the floor, the two
/// counts and the retry time.
const OFF_FABLE_BYTES: usize = 1 + 8 + 8 + 4 + 4 + 8;
const ON_LAUNCH_BYTES: usize = 1 + 1 + 8 + 4 + 4 + 8;

/// The bytes the fixture's turn-end duty memory takes after the limit state,
/// the payload's last field.
fn duty_bytes() -> usize {
    let mut encoder = Encoder::new();
    Carried::default().put(&mut encoder).unwrap();
    encoder.finish().len()
}

#[test]
fn the_fable_limit_state_crosses_whole() {
    let mut saved = fixture();
    // CONTROL: a session that never met the limit crosses as that.
    let untouched = encode_saved(&saved).unwrap();
    assert_eq!(
        decode(&untouched).unwrap().limit,
        limit::State::new(1000),
        "the fixture"
    );
    // Off Fable, with a return refused twice and a move that failed.
    let off = limit::State {
        floor_ms: 5_000,
        phase: limit::Phase::OffFable {
            back_at_ms: 1_794_762_060_000,
        },
        walled_returns: 2,
        attempts: 3,
        retry_at_ms: 9_000,
    };
    saved.limit = off;
    let moved = encode_saved(&saved).unwrap();
    assert_eq!(
        moved.len(),
        untouched.len() - ON_LAUNCH_BYTES + OFF_FABLE_BYTES
    );
    let restored = decode(&moved).unwrap();
    assert_eq!(restored.limit, off);
    assert_eq!(encode_saved(&restored).unwrap(), moved, "and back again");
    // A return that has yet to answer is not the same as a launch.
    saved.limit.phase = limit::Phase::OnLaunch { returned: true };
    let returned = decode(&encode_saved(&saved).unwrap()).unwrap().limit;
    assert_eq!(returned.phase, limit::Phase::OnLaunch { returned: true });
    assert_ne!(returned, limit::State::new(5_000));
}

#[test]
fn a_limit_state_that_is_cut_short_or_names_no_phase_restores_nothing() {
    let mut saved = fixture();
    saved.limit.phase = limit::Phase::OffFable { back_at_ms: 7 };
    let payload = encode_saved(&saved).unwrap();
    // CONTROL: the bytes as written restore.
    assert!(decode(&payload).is_ok());
    let tag = payload.len() - duty_bytes() - OFF_FABLE_BYTES;
    assert_eq!(payload[tag], 1, "the phase tag of a session off Fable");
    // A phase this build does not know.
    for unknown in [2, 7, u8::MAX] {
        let mut wrong = payload.clone();
        wrong[tag] = unknown;
        assert_eq!(
            decode(&wrong).unwrap_err().to_string(),
            "unknown Fable-limit phase",
            "{unknown}"
        );
    }
    // A flag that is neither zero nor one.
    let mut on_launch = fixture();
    on_launch.limit.phase = limit::Phase::OnLaunch { returned: true };
    let mut wrong = encode_saved(&on_launch).unwrap();
    let flag = wrong.len() - duty_bytes() - ON_LAUNCH_BYTES + 1;
    assert_eq!(wrong[flag], 1);
    wrong[flag] = 2;
    assert!(decode(&wrong).is_err());
    // Cut anywhere inside the state (or the duty's memory after it), or with
    // a byte after it all.
    for cut in 1..=duty_bytes() + OFF_FABLE_BYTES {
        assert!(decode(&payload[..payload.len() - cut]).is_err(), "{cut}");
    }
    let mut longer = payload;
    longer.push(0);
    assert!(decode(&longer).is_err());
}

#[test]
fn the_reset_grace_crosses_with_the_other_options() {
    let mut saved = fixture();
    // CONTROL: the fixture's own grace crosses as itself.
    let before = saved.setup.opts.reset_grace_ms;
    assert_eq!(
        decode(&encode_saved(&saved).unwrap())
            .unwrap()
            .setup
            .opts
            .reset_grace_ms,
        before
    );
    saved.setup.opts.reset_grace_ms = before + 1_234;
    let restored = decode(&encode_saved(&saved).unwrap()).unwrap();
    assert_eq!(restored.setup.opts.reset_grace_ms, before + 1_234);
    // A grace the command line would refuse does not cross: the options
    // are held to the bounds they are parsed with.
    saved.setup.opts.reset_grace_ms = cli::RESET_GRACE_MS_MAX + 1;
    assert!(encode_saved(&saved).is_err());
    assert!(encode_fields(&saved).is_ok_and(|payload| decode(&payload).is_err()));
    saved.setup.opts.reset_grace_ms = cli::RESET_GRACE_MS_MAX;
    assert!(encode_saved(&saved).is_ok());
}

#[test]
fn what_the_journal_already_says_of_the_limit_crosses_so_nothing_is_said_twice() {
    let mut saved = fixture();
    // CONTROL: a session that has said nothing crosses as that.
    assert_eq!(
        decode(&encode_saved(&saved).unwrap()).unwrap().limit_said,
        LimitSaid::default()
    );
    let said = LimitSaid {
        seen: "limit-seen bucket=seven_day_overage_included at=1794551966 \
               resets-at=1794762000 decision=move to=claude-opus-5-5"
            .to_string(),
        due: true,
        wait: "wait aterm human_ms=1000".to_string(),
        over: "busy background-work".to_string(),
    };
    saved.limit_said = said.clone();
    let payload = encode_saved(&saved).unwrap();
    let restored = decode(&payload).unwrap();
    assert_eq!(restored.limit_said, said);
    assert_eq!(encode_saved(&restored).unwrap(), payload, "and back again");
    // Each word is its own: one changed is the only one that differs.
    saved.limit_said.due = false;
    let other = decode(&encode_saved(&saved).unwrap()).unwrap().limit_said;
    assert_eq!(other, LimitSaid { due: false, ..said });
    // A line longer than any the journal writes does not cross.
    saved.limit_said.wait = "w".repeat(MAX_TEXT + 1);
    assert!(encode_saved(&saved).is_err());
    if let Ok(payload) = encode_fields(&saved) {
        assert!(decode(&payload).is_err());
    }
    saved.limit_said.wait = "w".repeat(MAX_TEXT);
    assert!(encode_saved(&saved).is_ok());
}
