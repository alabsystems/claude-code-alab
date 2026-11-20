// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The two moments the harness asks aterm about before a restart of its own:
//! a child at a usage limit ([`judge_wall_point`]) and a quiet moment under
//! the `relaunch` duty ([`judge_safe_point`]). Each is compared with the
//! upgrade safe point on the same replies, which is the control.

use std::time::Duration;

use super::*;

const ME: &str = "claude-code-alab@4242";

/// A policy as a running aterm states it, the switches an upgrade reads on.
const POLICY: &str = "OK gen=1 effective=on why=enabled contract=2,3 enabled=true \
    autopilot=cruise relaunch=true stall_term=true upgrade=true upgrade_ask=false \
    human_grace_s=120 questions=recommended";

/// A status under this harness's proven claim, quiet in every field.
const STATUS: &str = "OK schema=1 sid=16 observed=true phase=running hold=0 hand=- \
    program=claude agent=idle composer=empty input=clear supervisor_contract=3 \
    supervisor_scope=relaunch,upgrade supervisor_revision=136 \
    supervisor=claude-code-alab@4242 supervisor_proven=1 autopilot=cruise human_ms=- \
    human_seq=0 need=- gen=1.9 seq=9";

/// `reply` with the word for `key` replaced.
fn with(reply: &str, key: &str, value: &str) -> String {
    let prefix = format!("{key}=");
    let words: Vec<String> = reply
        .split_ascii_whitespace()
        .map(|word| {
            if word.starts_with(&prefix) {
                format!("{prefix}{value}")
            } else {
                word.to_string()
            }
        })
        .collect();
    let changed = words.join(" ");
    assert_ne!(
        changed, reply,
        "{key} is not in the reply, or already {value}"
    );
    changed
}

fn policy(changes: &[(&str, &str)]) -> Policy {
    let reply = changes
        .iter()
        .fold(POLICY.to_string(), |reply, (key, value)| {
            with(&reply, key, value)
        });
    Policy::parse(&reply).expect("a policy")
}

fn status(changes: &[(&str, &str)]) -> Status {
    let reply = changes
        .iter()
        .fold(STATUS.to_string(), |reply, (key, value)| {
            with(&reply, key, value)
        });
    Status::parse(&reply).expect("a status")
}

fn word(point: SafePoint) -> String {
    match point {
        SafePoint::Safe => "safe".to_string(),
        SafePoint::Hold(hold) => hold.to_string(),
    }
}

fn wall(policy: &Policy, status: &Status) -> String {
    word(judge_wall_point(policy, status, ME, Duration::ZERO))
}

fn quiet(policy: &Policy, status: &Status, duty: Scope) -> String {
    word(judge_safe_point(policy, status, ME, Duration::ZERO, duty))
}

#[test]
fn a_child_at_a_limit_is_judged_by_who_would_be_interrupted_not_by_its_dialog() {
    // What aterm shows while the vendor's limit dialog is open: the agent is
    // not idle, no composer is drawn (`-`), and the session reads as needing
    // a person. The wall point holds on none of the three.
    let dialog = status(&[("agent", "busy"), ("composer", "-"), ("need", "approval")]);
    assert_eq!(wall(&policy(&[]), &dialog), "safe");
    // CONTROL: the upgrade safe point holds on each of them, on the same reply.
    assert_eq!(
        quiet(&policy(&[]), &dialog, Scope::Upgrade),
        "wait aterm agent=busy"
    );
    for (key, value) in [("composer", "-"), ("need", "approval")] {
        assert_eq!(wall(&policy(&[]), &status(&[(key, value)])), "safe");
        assert_eq!(
            quiet(&policy(&[]), &status(&[(key, value)]), Scope::Upgrade),
            format!("wait aterm {key}={value}")
        );
    }
    // The two switches are a person's word on vendor updates, not on this.
    let no_updates = policy(&[("upgrade", "false"), ("upgrade_ask", "true")]);
    assert_eq!(wall(&no_updates, &dialog), "safe");
    assert_eq!(
        quiet(&no_updates, &status(&[]), Scope::Upgrade),
        "wait aterm upgrade=false"
    );
}

#[test]
fn a_draft_in_the_composer_holds_the_wall_point() {
    // The dialog was pressed (or never drawn): the composer is back, and a
    // person's unsent words are in it. The move would end the child and the
    // resumed session would not have them.
    let typed = status(&[("composer", "draft")]);
    assert_eq!(wall(&policy(&[]), &typed), "wait aterm composer=draft");
    // It holds however long ago the person typed it, and beside every other
    // reading of the dialog's.
    let left = status(&[
        ("composer", "draft"),
        ("agent", "busy"),
        ("need", "approval"),
        ("human_ms", "900000"),
    ]);
    assert_eq!(wall(&policy(&[]), &left), "wait aterm composer=draft");
    // A status that says nothing of the composer holds too.
    let reply = STATUS.replace(" composer=empty", "");
    assert_ne!(reply, STATUS);
    let silent = Status::parse(&reply).expect("a status");
    assert_eq!(wall(&policy(&[]), &silent), "wait aterm composer=absent");
    // CONTROL: an empty composer (the reply's own), its placeholder, and none
    // drawn (the dialog's own `-`) are no draft, on the same replies
    // otherwise.
    assert_eq!(wall(&policy(&[]), &status(&[])), "safe");
    for value in ["placeholder", "-"] {
        assert_eq!(
            wall(&policy(&[]), &status(&[("composer", value)])),
            "safe",
            "{value}"
        );
    }
    assert_eq!(
        wall(
            &policy(&[]),
            &status(&[
                ("agent", "busy"),
                ("need", "approval"),
                ("human_ms", "900000")
            ])
        ),
        "safe"
    );
}

#[test]
fn a_person_a_driver_a_halt_and_a_claim_that_is_not_ours_hold_the_wall_point() {
    // A person inside the policy's grace, and one whose last key is unread.
    assert_eq!(
        wall(&policy(&[]), &status(&[("human_ms", "1000")])),
        "wait aterm human_ms=1000"
    );
    assert_eq!(
        wall(&policy(&[]), &status(&[("human_ms", "soon")])),
        "wait aterm human_ms=unknown"
    );
    // CONTROL: the same person past the grace does not hold, and the caller's
    // own grace can only lengthen aterm's.
    let aged = status(&[("human_ms", "120000")]);
    assert_eq!(wall(&policy(&[]), &aged), "safe");
    assert_eq!(
        word(judge_wall_point(
            &policy(&[]),
            &aged,
            ME,
            Duration::from_secs(200)
        )),
        "wait aterm human_ms=120000"
    );
    let short = policy(&[("human_grace_s", "1")]);
    assert_eq!(wall(&short, &status(&[("human_ms", "1000")])), "safe");
    // A driver, a halt and unsent input each hold by name.
    for (key, value) in [("hand", "driver"), ("hold", "1"), ("input", "pending")] {
        assert_eq!(
            wall(&policy(&[]), &status(&[(key, value)])),
            format!("wait aterm {key}={value}"),
        );
    }
    // The claim: another holder's, unproven, or one that did not take
    // `relaunch`, the duty the restart is made under.
    assert_eq!(
        wall(&policy(&[]), &status(&[("supervisor", "aterm@1")])),
        "wait aterm supervisor=aterm@1"
    );
    assert_eq!(
        wall(&policy(&[]), &status(&[("supervisor_proven", "0")])),
        "wait aterm supervisor_proven=0"
    );
    let upgrade_only = status(&[("supervisor_scope", "upgrade")]);
    assert_eq!(
        wall(&policy(&[]), &upgrade_only),
        "wait aterm supervisor_scope=upgrade"
    );
    // CONTROL: that narrower claim is still enough for an upgrade.
    assert_eq!(quiet(&policy(&[]), &upgrade_only, Scope::Upgrade), "safe");
    // A policy that is off holds here: the caller's link gives the claim
    // back for an Autopilot that is off before this is ever asked.
    assert_eq!(
        wall(&policy(&[("effective", "off")]), &status(&[])),
        "wait aterm effective=off"
    );
    assert_eq!(
        wall(&policy(&[("contract", "2")]), &status(&[])),
        "wait aterm contract=2"
    );
}

#[test]
fn a_return_is_a_quiet_moment_under_the_relaunch_duty() {
    assert_eq!(quiet(&policy(&[]), &status(&[]), Scope::Relaunch), "safe");
    // The upgrade switches do not hold a restart that moves no build.
    let no_updates = policy(&[("upgrade", "false"), ("upgrade_ask", "true")]);
    assert_eq!(quiet(&no_updates, &status(&[]), Scope::Relaunch), "safe");
    // CONTROL: they hold the upgrade.
    assert_eq!(
        quiet(&no_updates, &status(&[]), Scope::Upgrade),
        "wait aterm upgrade=false"
    );
    let asks = policy(&[("upgrade_ask", "true")]);
    assert_eq!(
        quiet(&asks, &status(&[]), Scope::Upgrade),
        "wait aterm upgrade_ask=true"
    );
    // Everything else a safe point reads holds a return exactly as it holds
    // an upgrade: it is made at a quiet moment, unlike the move at the limit.
    for (key, value) in [
        ("agent", "busy"),
        ("composer", "draft"),
        ("human_ms", "1000"),
        ("hand", "driver"),
        ("hold", "1"),
        ("input", "pending"),
        ("need", "approval"),
    ] {
        let held = status(&[(key, value)]);
        let line = format!("wait aterm {key}={value}");
        assert_eq!(quiet(&policy(&[]), &held, Scope::Relaunch), line);
        assert_eq!(quiet(&policy(&[]), &held, Scope::Upgrade), line);
    }
    // The duty is the claim's to have taken.
    let upgrade_only = status(&[("supervisor_scope", "upgrade")]);
    assert_eq!(
        quiet(&policy(&[]), &upgrade_only, Scope::Relaunch),
        "wait aterm supervisor_scope=upgrade"
    );
    let relaunch_only = status(&[("supervisor_scope", "relaunch")]);
    assert_eq!(quiet(&policy(&[]), &relaunch_only, Scope::Relaunch), "safe");
    assert_eq!(
        quiet(&policy(&[]), &relaunch_only, Scope::Upgrade),
        "wait aterm supervisor_scope=relaunch"
    );
}
