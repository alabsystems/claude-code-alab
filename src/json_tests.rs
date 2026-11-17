// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

#[test]
fn reads_a_claude_session_record() {
    let text = r#"{"pid":26750,"sessionId":"7c1e2a90-4b3d-4f6e-9a18-2d5c6b7e8f01","cwd":"/h/u/work","startedAt":1792867292005,"procStart":"Sat Oct 24 18:41:29 2026","version":"2.1.310","peerProtocol":1,"peerFeatures":["notify_idle","artifact_yield"],"kind":"interactive","entrypoint":"cli","status":"busy","updatedAt":1792874003933,"statusUpdatedAt":1792874003933}"#;
    let v = parse(text).expect("parses");
    assert_eq!(v.get("pid").and_then(Value::as_u64), Some(26750));
    assert_eq!(v.get("status").and_then(Value::as_str), Some("busy"));
    assert_eq!(
        v.get("statusUpdatedAt").and_then(Value::as_u64),
        Some(1_792_874_003_933)
    );
    assert!(matches!(v.get("peerFeatures"), Some(Value::Array(a)) if a.len() == 2));
}

#[test]
fn decodes_escapes_and_surrogate_pairs() {
    let v = parse(r#""a\"b\\c\n\u00e9\ud83d\ude00""#).expect("parses");
    assert_eq!(v.as_str(), Some("a\"b\\c\né😀"));
}

#[test]
fn reads_unicode_at_string_and_escape_boundaries() {
    let text = r#"{"κλειδί":["café", "東京😀", "é\"x\\雪\n\uD83D\uDE00", -12.5e+3]}"#;
    let value = parse(text).expect("parses");
    assert_eq!(
        value.get("κλειδί"),
        Some(&Value::Array(vec![
            Value::String("café".into()),
            Value::String("東京😀".into()),
            Value::String("é\"x\\雪\n😀".into()),
            Value::Number("-12.5e+3".into()),
        ]))
    );
    for (end, _) in text.char_indices() {
        assert_eq!(parse(&text[..end]), None, "prefix ending at byte {end}");
    }
    for malformed in [r#""\u123é""#, r#""\u12é""#, r#""\ud83d\u12é""#] {
        assert_eq!(parse(malformed), None, "{malformed:?} must be refused");
    }
}

#[test]
fn refuses_what_is_not_exactly_one_json_value() {
    for bad in [
        "",
        "{",
        "{}x",
        "{\"a\":1,}",
        "[1,]",
        "01",
        "1.",
        "-",
        "\"\\ud800\"",
        "\"\\udc00\"",
        "\"tab\there\"",
        "{\"a\":1,\"a\":2}",
        "nul",
        "{'a':1}",
    ] {
        assert_eq!(parse(bad), None, "{bad:?} must be refused");
    }
}

#[test]
fn refuses_nesting_past_the_bound() {
    for (open, close) in [("[", "]"), ("{\"child\":", "}")] {
        let nested = |levels, leaf: &str| open.repeat(levels) + leaf + &close.repeat(levels);
        assert!(parse(&nested(MAX_DEPTH, "null")).is_some(), "{open}");
        assert_eq!(parse(&nested(MAX_DEPTH + 1, "null")), None, "{open}");
        for leaf in ["[]", "{}"] {
            assert!(
                parse(&nested(MAX_DEPTH - 1, leaf)).is_some(),
                "{open} ending in {leaf} at the bound"
            );
            assert_eq!(
                parse(&nested(MAX_DEPTH, leaf)),
                None,
                "{open} ending in {leaf} past the bound"
            );
        }
    }
}

#[test]
fn every_prefix_of_a_record_is_answered_without_a_panic() {
    // `number` and `string` take checked source slices: every proper prefix
    // ends inside a number, a string or an escape and must be refused.
    let text = r#"{"n":-12.5e+3,"s":"a\"b\u00e9","t":[true,null,0]}"#;
    assert!(parse(text).is_some());
    for end in 0..text.len() {
        if let Some(prefix) = text.get(..end) {
            assert_eq!(parse(prefix), None, "{prefix:?} must be refused");
        }
    }
}

#[test]
fn integers_only_through_as_u64() {
    assert_eq!(parse("12").and_then(|v| v.as_u64()), Some(12));
    assert_eq!(parse("-1").and_then(|v| v.as_u64()), None);
    assert_eq!(parse("1.5").and_then(|v| v.as_u64()), None);
    assert_eq!(parse("1e3").and_then(|v| v.as_u64()), None);
    assert_eq!(parse("99999999999999999999").and_then(|v| v.as_u64()), None);
}
