// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

#[test]
fn pre_spawn_barriers_require_both_unambiguous_capabilities() {
    assert!(child_start_supported(
        "OK native-act child-proof=1 child-start=1"
    ));
    for info in [
        "OK child-proof=1",
        "OK child-start=1",
        "ERR child-proof=1 child-start=1",
        "OK child-proof=1 child-start=0",
        "OK child-proof=1 child-start=1 child-start=1",
        "OK child-proof=1 child-proof=1 child-start=1",
    ] {
        assert!(!child_start_supported(info), "{info}");
    }
}

#[test]
fn a_start_claim_never_guesses_the_child_tuple() {
    let scopes = &[Scope::Relaunch, Scope::Upgrade];
    let sid = "s-0000000000004d7a2a97";
    let base = claim_verb(sid, 4242, scopes, "0.3.0", Some("2.1.325")).unwrap();
    assert_eq!(
        claim_child_start_verb(sid, 4242, scopes, "0.3.0", Some("2.1.325"), 17),
        Some(format!("{base} child-start=17"))
    );
}

#[test]
fn a_claimed_start_needs_proven_pending_status_without_a_previous_child() {
    let status = "OK schema=1 supervisor=claude-code-alab@4242 supervisor_proven=1 supervisor_scope=relaunch,upgrade supervisor_revision=7 supervisor_child_start=1 supervisor_child=-";
    assert!(child_start_pending(status, "claude-code-alab@4242", 1));
    assert!(!child_start_pending(status, "claude-code-alab@4242", 2));
    for changed in [
        status.replace("supervisor_proven=1", "supervisor_proven=0"),
        status.replace("supervisor_child=-", "supervisor_child=123:456"),
        status.replace("supervisor_child_start=1", "supervisor_child_start=0"),
        status.replace("supervisor_revision=7", "supervisor_revision=0"),
        format!("{status} supervisor_child_start=1"),
        status.replace(" supervisor_child=-", ""),
    ] {
        assert!(
            !child_start_pending(&changed, "claude-code-alab@4242", 1),
            "{changed}"
        );
    }
    assert!(!child_start_pending(status, "claude-code-alab@4243", 1));
}
