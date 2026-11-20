// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The record half of a safe point made at a turn's end ([`record_turn_end`]):
//! the moment a session Autopilot continues at every turn end has,
//! compared with the quiet period's own rule ([`record_idle`]) on the same
//! readings, which is the control.

use super::*;

const NOW: u64 = 1_000_000;

#[test]
fn a_record_just_gone_idle_is_a_turns_end_before_the_continuation() {
    let idle = |ago: u64| Some((crate::profile::STATUS_IDLE, NOW - ago));
    // Inside the window: a turn's end, before a continuation is typed.
    for ago in [
        TURN_END_SETTLE_MS,
        TURN_END_SETTLE_MS + 1,
        1_000,
        TURN_END_WITHIN_MS,
    ] {
        assert_eq!(record_turn_end(idle(ago), NOW, QUIET_MS), Ok(()), "{ago}");
        // CONTROL: the quiet period's own rule waits on the same reading.
        assert_eq!(
            record_idle(idle(ago), NOW, QUIET_MS),
            Err(Wait::Settling),
            "{ago}"
        );
    }
    // Too soon to call the turn over, and past the moment a continuation may
    // be typed: both wait, up to the quiet period.
    for ago in [
        0,
        TURN_END_SETTLE_MS - 1,
        TURN_END_WITHIN_MS + 1,
        2_000,
        QUIET_MS - 1,
    ] {
        assert_eq!(
            record_turn_end(idle(ago), NOW, QUIET_MS),
            Err(Wait::Settling),
            "{ago}"
        );
    }
    // A record idle for the quiet period passes either way.
    for ago in [QUIET_MS, 10 * QUIET_MS] {
        assert_eq!(record_turn_end(idle(ago), NOW, QUIET_MS), Ok(()), "{ago}");
        assert_eq!(record_idle(idle(ago), NOW, QUIET_MS), Ok(()), "{ago}");
    }
    // A status clock ahead of this one has ended nothing.
    assert_eq!(
        record_turn_end(
            Some((crate::profile::STATUS_IDLE, NOW + 1_000)),
            NOW,
            QUIET_MS
        ),
        Err(Wait::Settling)
    );
    // A turn still running, or no record, is never a turn's end, however
    // its clock reads.
    for status in ["busy", "shell", "waiting", ""] {
        assert_eq!(
            record_turn_end(Some((status, NOW - 1_000)), NOW, QUIET_MS),
            Err(Wait::Busy),
            "{status:?}"
        );
    }
    assert_eq!(record_turn_end(None, NOW, QUIET_MS), Err(Wait::NoRecord));
}
