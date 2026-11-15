// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Per-fork barrier ordering observed by the fake host independently of the
//! vendor body log. The real host owns native identity and output-floor proof.

use super::*;

fn generation(verb: &str) -> Option<u64> {
    verb.split_ascii_whitespace()
        .find_map(|word| word.strip_prefix("child-start="))
        .and_then(|value| value.parse().ok())
}

fn assert_bound(fake: &FakeAterm, child: i32, expected_generation: u64) {
    let identity = claude_code_alab::procs::capture_process(child as u32).expect("native child");
    let birth = match identity.birth {
        claude_code_alab::procs::Birth::MacOs {
            seconds,
            microseconds,
        } => seconds * 1_000_000 + u64::from(microseconds),
        claude_code_alab::procs::Birth::Linux(ticks) => ticks,
    };
    let tuple = format!("child={child}:{birth}");
    assert!(
        wait_until(Duration::from_secs(60), || fake.verbs().iter().any(
            |verb| verb.split_ascii_whitespace().any(|word| word == tuple)
                && generation(verb) == Some(expected_generation)
        )),
        "native completion {tuple} generation {expected_generation}"
    );
}

pub(super) fn precedes_every_vendor_body_and_keeps_bound_generation_on_reconnect() {
    let s = Scenario::new("child-start-lifetimes");
    let fake = rostered_aterm(&s);
    fake.set_info(fake_aterm::INFO_CURRENT);
    fake.set_body_log(&s.fake.join("log"));
    let pty = Pty::open();
    let (_child, pid) = start_in_aterm(&s, &pty, &fake, 40);
    let first = await_record(&s, 0);
    assert_bound(&fake, first, 1);
    fake.event("restart cause=asked");
    let second = await_record(&s, 1);
    assert_ne!(first, second);
    assert_bound(&fake, second, 2);
    s.set_current(BUILD_2);
    let third = await_record(&s, 2);
    assert_ne!(second, third);
    assert_bound(&fake, third, 3);
    s.put("command", "crash:7");
    let fourth = await_record(&s, 3);
    assert_ne!(third, fourth);
    assert_bound(&fake, fourth, 4);
    let begins = fake
        .verbs_with_body_counts()
        .into_iter()
        .filter(|(verb, _)| {
            generation(verb).is_some()
                && !verb
                    .split_ascii_whitespace()
                    .any(|word| word.starts_with("child="))
        })
        .collect::<Vec<_>>();
    assert_eq!(
        begins
            .iter()
            .map(|(verb, count)| (generation(verb).unwrap(), *count))
            .collect::<Vec<_>>(),
        [(1, 0), (2, 1), (3, 2), (4, 3)],
        "a fresh acknowledged generation precedes each body{}",
        s.report()
    );
    let before = fake.verbs().len();
    fake.restart();
    assert!(
        wait_until(Duration::from_secs(60), || fake
            .verbs()
            .iter()
            .skip(before)
            .any(
                |verb| generation(verb) == Some(4) && verb.contains(&format!(" child={fourth}:"))
            )),
        "the continuing child retains its acknowledged generation after reconnect{}",
        s.report()
    );
    assert_eq!(s.starts().len(), 4, "reconnect never forks a replacement");
    end_session(&s, pid);
}

pub(super) fn a_refused_child_start_drops_authority_and_keeps_vendor_liveness() {
    let s = Scenario::new("child-start-refused");
    let fake = rostered_aterm(&s);
    fake.set_info(&format!("{INFO_ROSTERED} child-proof=1 child-start=1"));
    fake.set_start_reply(Some("ERR beginning temporarily unavailable; retry"));
    fake.set_body_log(&s.fake.join("log"));
    let pty = Pty::open();
    let (_child, pid) = start_in_aterm(&s, &pty, &fake, 40);
    let _ = await_record(&s, 0);
    assert!(
        wait_until(Duration::from_secs(60), || fake.holder().is_none()),
        "failed begin definitively releases its connection"
    );
    let begins = fake
        .verbs_with_body_counts()
        .into_iter()
        .filter(|(verb, _)| generation(verb).is_some())
        .collect::<Vec<_>>();
    assert_eq!(begins.len(), 2, "one bounded retry");
    assert!(
        begins
            .iter()
            .all(|(verb, count)| generation(verb) == Some(1) && *count == 0),
        "a transport retry uses the same pre-fork generation"
    );
    assert!(
        s.journal().contains("aterm child-start unclaimed"),
        "{}",
        s.report()
    );
    assert_eq!(
        s.starts().len(),
        1,
        "the vendor still starts once without asserted authority"
    );
    end_session(&s, pid);
}

pub(super) fn uses_a_fresh_generation_after_an_unannounced_dead_attempt() {
    let s = Scenario::new("child-start-unannounced-fallback");
    let fake = rostered_aterm(&s);
    fake.set_info(&format!("{INFO_ROSTERED} child-proof=1 child-start=1"));
    fake.set_body_log(&s.fake.join("log"));
    let pty = Pty::open();
    let (_child, pid) = start_in_aterm(&s, &pty, &fake, 40);
    let first = await_record(&s, 0);
    assert_bound(&fake, first, 1);
    // This image exits before a session record can announce it. Its pending
    // floor must never become the fallback child's pre-spawn floor.
    s.add_build_script(BUILD_2, "exit 2");
    s.set_current(BUILD_2);
    let fallback = await_record(&s, 1);
    assert_ne!(fallback, first);
    assert_eq!(s.script_runs(), 1, "the refused image actually ran");
    assert_eq!(s.starts()[1].build(), Some(BUILD_1));
    assert_bound(&fake, fallback, 3);
    let begins = fake
        .verbs_with_body_counts()
        .into_iter()
        .filter_map(|(verb, count)| {
            (!verb
                .split_ascii_whitespace()
                .any(|word| word.starts_with("child=")))
            .then(|| generation(&verb).map(|generation| (generation, count)))
            .flatten()
        })
        .collect::<Vec<_>>();
    assert_eq!(
        begins,
        [(1, 0), (2, 1), (3, 1)],
        "each real fork attempt owns a distinct generation{}",
        s.report()
    );
    end_session(&s, pid);
}
