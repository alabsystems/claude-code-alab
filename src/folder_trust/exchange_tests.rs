// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

fn answer(id: &str) -> Vec<u8> {
    format!(
        "{{\"type\":\"control_response\",\"response\":{{\"subtype\":\"success\",\"request_id\":\"{id}\",\"response\":{{\"workspace_trust_recorded\":true,\"claude_code_version\":\"2.1.325\"}}}}}}\n"
    )
    .into_bytes()
}

#[test]
fn an_admitted_answer_keeps_its_unfinished_tail_until_final_eof() {
    let mut wire = Wire {
        output: answer("fresh"),
        ..Wire::default()
    };
    wire.output.extend_from_slice(b"{\"type\":\"system\"");
    assert!(wire.parse("fresh", true).unwrap());
    assert!(wire.finish().is_err());
    // The same incomplete frame may legitimately complete during retirement.
    wire.output.extend_from_slice(b"}\n");
    assert!(!wire.parse("fresh", false).unwrap());
    wire.finish().unwrap();
    assert_eq!(wire.answer, Some(Some("2.1.325".into())));
}

#[test]
fn retirement_never_admits_a_first_late_answer() {
    let mut wire = Wire {
        output: answer("fresh"),
        ..Wire::default()
    };
    assert!(wire.parse("fresh", false).is_err());
    wire.poison();
    assert!(wire.finish().is_err());
    assert!(wire.answer.is_none());
}

#[test]
fn trailing_duplicate_unsolicited_and_malformed_frames_refuse_the_earlier_answer() {
    for tail in [
        answer("fresh"),
        answer("stale"),
        b"{\"type\":\"assistant\"}\n".to_vec(),
        b"not-json\n".to_vec(),
        vec![0xff, b'\n'],
    ] {
        let mut wire = Wire {
            output: answer("fresh"),
            ..Wire::default()
        };
        assert!(wire.parse("fresh", true).unwrap());
        wire.output.extend_from_slice(&tail);
        assert!(wire.parse("fresh", false).is_err());
        wire.poison();
        assert!(wire.finish().is_err());
        assert!(wire.answer.is_none());
    }
}
