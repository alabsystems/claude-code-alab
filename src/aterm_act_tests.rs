// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The report of where Land rests ([`LandAct`]): its two requests, and their
//! replies read as aterm words them.

use super::*;

const SID: &str = "s-0000000000000000001a";

#[test]
fn the_reply_to_each_report_exchange_is_read_as_aterm_words_it() {
    assert_eq!(
        Lease::parse("OK until_us=1763200000000000 lease_ms=5000"),
        Ok(Lease {
            until_us: 1_763_200_000_000_000
        })
    );
    // Negative controls: a lease out of its grammar is not read.
    for malformed in [
        "OK",
        "OK until_us=0 lease_ms=5000",
        "OK until_us=12 lease_ms=0",
        "OK until_us=12 lease_ms=5001",
        "OK until_us=012 lease_ms=5000",
        "OK lease_ms=5000 until_us=12",
        "OK until_us=12 lease_ms=5000 more",
    ] {
        assert_eq!(
            Lease::parse(malformed),
            Err(Reported::Unserved("prepare lease=malformed".to_string())),
            "{malformed}"
        );
    }
    // An aterm without the lease is unserved; one that refuses it for now is
    // for later.
    for (reply, unserved) in [
        ("ERR unknown verb", true),
        (
            "ERR usage: meta [set <field> <text...> | unset <field>]",
            true,
        ),
        ("ERR unknown meta field", true),
        ("ERR denied", false),
        ("ERR clock unavailable", false),
    ] {
        match Lease::parse(reply) {
            Err(Reported::Unserved(words)) => assert!(unserved, "{reply}: {words}"),
            Err(Reported::Later(words)) => assert!(!unserved, "{reply}: {words}"),
            other => panic!("{reply}: {other:?}"),
        }
    }
    assert_eq!(Reported::parse("OK"), Reported::Recorded);
    assert_eq!(
        Reported::parse("ERR busy harness_act"),
        Reported::Later("act reply=ERR:busy".to_string())
    );
    assert_eq!(
        Reported::parse("ERR expired harness_act"),
        Reported::Later("act reply=ERR:expired".to_string())
    );
    assert!(matches!(
        Reported::parse("ERR usage: meta prepare harness_act | meta set harness_act <act>"),
        Reported::Unserved(_)
    ));
    assert_eq!(
        act_verb(SID, LandAct::Left, 7).as_deref(),
        Some("@s-0000000000000000001a meta set harness_act left-to-person@v1 until_us=7")
    );
    assert_eq!(
        act_prepare_verb(SID).as_deref(),
        Some("@s-0000000000000000001a meta prepare harness_act")
    );
}

#[test]
fn the_acts_are_aterms_own_words_and_a_bad_session_sends_nothing() {
    assert_eq!(LandAct::Landed.word(), "landed@v1");
    assert_eq!(LandAct::Left.word(), "left-to-person@v1");
    assert_eq!(act_prepare_verb("not-a-session"), None);
    assert_eq!(act_verb("not-a-session", LandAct::Landed, 1), None);
}
