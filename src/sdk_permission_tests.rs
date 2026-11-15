// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

#[test]
fn refused_native_state_keeps_result_containers_bounded() {
    const _: () = assert!(std::mem::size_of::<ProbeSpawnRefusal>() <= 128);
    const _: () = assert!(std::mem::size_of::<StartFailure>() <= 128);
}

fn parsed(text: &str, request: Request) -> io::Result<Option<Reply>> {
    reply(
        &json::parse(text).ok_or_else(|| invalid("strict JSON refused"))?,
        "pending",
        request,
    )
}

#[test]
fn only_an_explicit_control_response_for_the_pending_request_is_an_ack() {
    assert_eq!(parsed(r#"{"type":"control_response","response":{"request_id":"pending","subtype":"success","response":{"current_permission_mode":"plan"}}}"#, Request::Initialize).unwrap(), Some(Reply::CurrentMode(Mode::Plan)));
    assert_eq!(
        parsed(
            r#"{"type":"system","permissionMode":"auto","timestamp":"2099"}"#,
            Request::Initialize
        )
        .unwrap(),
        None
    );
    for text in [
        r#"{"type":"control_response","response":{"request_id":"old","subtype":"success","response":{"current_permission_mode":"auto"}}}"#,
        r#"{"type":"control_response","response":{"request_id":"pending","subtype":"success","response":{"permissionMode":"auto"}}}"#,
        r#"{"type":"permission-mode","permissionMode":"auto","request_id":"pending"}"#,
        r#"{"type":"control_response","response":{"request_id":"pending","subtype":"success","response":{"current_permission_mode":"unknown"}}}"#,
        r#"{"type":"control_response","response":{"request_id":"pending","request_id":"old","subtype":"success","response":{"current_permission_mode":"auto"}}}"#,
    ] {
        assert!(parsed(text, Request::Initialize).is_err(), "{text}");
    }
}

#[test]
fn set_ack_must_confirm_the_requested_value_and_rejections_are_explicit() {
    let success = r#"{"type":"control_response","response":{"request_id":"pending","subtype":"success","response":{"mode":"plan"}}}"#;
    assert_eq!(
        parsed(success, Request::Set(Mode::Plan)).unwrap(),
        Some(Reply::ModeAccepted(Mode::Plan))
    );
    assert!(parsed(success, Request::Set(Mode::Default)).is_err());
    assert!(parsed(success, Request::InvalidMode).is_err());
    let error = r#"{"type":"control_response","response":{"request_id":"pending","subtype":"error","error":"invalid mode","error_code":"invalid_mode"}}"#;
    assert_eq!(
        parsed(error, Request::InvalidMode).unwrap(),
        Some(Reply::Rejected("invalid mode".into()))
    );
    assert!(
        parsed(
            r#"{"type":"control_response","response":{"request_id":"pending","subtype":"error"}}"#,
            Request::InvalidMode
        )
        .is_err()
    );
}

#[test]
fn request_encoding_uses_the_vendor_mode_grammar_and_fresh_os_random_ids() {
    let one = request_id().unwrap();
    let two = request_id().unwrap();
    assert_ne!(one, two);
    for id in [&one, &two] {
        assert_eq!(id.len(), 74);
        assert!(
            id.strip_prefix("alab-mode-")
                .unwrap()
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit())
        );
        let frame = Request::Set(Mode::DontAsk).frame(id);
        let value = json::parse(std::str::from_utf8(&frame).unwrap()).unwrap();
        assert_eq!(
            value.get("request_id").and_then(Value::as_str),
            Some(id.as_str())
        );
        assert_eq!(
            value
                .get("request")
                .unwrap()
                .get("mode")
                .and_then(Value::as_str),
            Some("dontAsk")
        );
    }
}

#[test]
fn exact_native_observation_never_promotes_birth_and_image_to_an_exec_generation() {
    let identity = procs::capture_exact_exec(std::process::id());
    #[cfg(target_os = "macos")]
    {
        let identity = identity.expect("Darwin exposes native exec generation");
        assert_eq!(identity.process().pid, std::process::id());
        assert!(identity.current());
    }
    #[cfg(not(target_os = "macos"))]
    assert!(identity.is_none());
}

#[test]
fn buffered_control_violations_are_refused_before_another_request() {
    let ack = r#"{"type":"control_response","response":{"request_id":"pending","subtype":"success","response":{"current_permission_mode":"plan"}}}"#;
    assert!(buffered_reply(ack, None, false).is_err());
    assert!(
        buffered_reply(ack, Some(("pending", Request::Initialize)), false)
            .unwrap()
            .is_some()
    );
    assert!(buffered_reply(ack, Some(("pending", Request::Initialize)), true).is_err());
    assert_eq!(
        buffered_reply(r#"{"type":"system","permissionMode":"auto"}"#, None, false).unwrap(),
        None
    );
}
