// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! A restart event's model, and what `status` says aterm confirmed of the
//! agent image (`supervisor_act=`), which decides whether it types the
//! claim's writes.

use super::*;

fn parse(tail: &str) -> Option<Restart> {
    Restart::parse(tail.split_ascii_whitespace()).ok()
}

fn moved(cause: &str, model: &str) -> Option<Restart> {
    Some(Restart {
        cause: cause.to_string(),
        model: Some(model.to_string()),
        account: None,
    })
}

#[test]
fn a_restart_event_names_the_model_to_resume_on() {
    // aterm's own line for a model move (`record_scoped_restart`).
    assert_eq!(
        parse("cause=model model=claude-opus-5-5"),
        moved("model", "claude-opus-5-5")
    );
    // Every byte aterm's rule admits, and the account words passed over
    // wherever they stand.
    for (tail, model) in [
        ("cause=model model=opus", "opus"),
        ("cause=model model=opus[1m]", "opus[1m]"),
        (
            "cause=model model=us.anthropic/claude_x:1@a-b",
            "us.anthropic/claude_x:1@a-b",
        ),
        ("cause=account to=work home=%2Fh model=fable", "fable"),
        ("cause=model model=fable a_later_key=1 bare", "fable"),
    ] {
        assert_eq!(
            parse(tail).and_then(|restart| restart.model).as_deref(),
            Some(model),
            "{tail}"
        );
    }
    // The reserved word is carried as it came: the holder reads it as the
    // launch's own model, never as a model's name.
    assert_eq!(MODEL_LAUNCH, "launch");
    assert_eq!(
        parse("cause=model model=launch"),
        moved("model", MODEL_LAUNCH)
    );
    // CONTROL: a restart that names no model moves none.
    for tail in ["cause=asked", "cause=model", "cause=asked models=opus"] {
        assert_eq!(
            parse(tail).map(|restart| restart.model),
            Some(None),
            "{tail}"
        );
    }
    let longest = "m".repeat(256);
    assert_eq!(
        parse(&format!("cause=model model={longest}")),
        moved("model", &longest)
    );
}

#[test]
fn a_restart_whose_model_cannot_be_a_command_line_word_is_not_acted_on() {
    for tail in [
        // Empty, an option to the vendor, a byte outside aterm's rule.
        "cause=model model=",
        "cause=model model=-opus",
        "cause=model model=--dangerously-skip-permissions",
        "cause=model model=opus;rm",
        "cause=model model=a%20b",
        "cause=model model=opus$HOME",
        "cause=model model=op\u{fc}s",
        // Named twice, the same word or not.
        "cause=model model=opus model=fable",
        "cause=model model=opus model=opus",
    ] {
        assert_eq!(parse(tail), None, "{tail:?}");
    }
    assert_eq!(
        parse(&format!("cause=model model={}", "m".repeat(257))),
        None
    );
    // The model must follow a cause: it never leads the event.
    assert_eq!(parse("model=opus cause=model"), None);
}

#[test]
fn a_model_word_is_aterms_own_rule() {
    for word in ["a", "claude-opus-5-5", "opus[1m]", "a.b_c-d:e@f/g", "9"] {
        assert!(model_word(word), "{word}");
    }
    for word in ["", "-a", "a b", "a=b", "a,b", "a\tb", "a\"b", "a'b", "a\\b"] {
        assert!(!model_word(word), "{word:?}");
    }
    assert!(model_word(&"a".repeat(256)) && !model_word(&"a".repeat(257)));
}

#[test]
fn the_engines_act_is_one_known_word_of_an_ok_status() {
    let status =
        |act: &str| format!("OK schema=1 supervisor=x supervisor_act={act} autopilot=cruise");
    assert_eq!(engine_act(&status("on")), Some(EngineAct::On));
    assert_eq!(engine_act(&status("observe")), Some(EngineAct::Observe));
    assert_eq!(engine_act(&status("-")), Some(EngineAct::Unread));
    for reply in [
        status("On"),
        status(""),
        status("acting"),
        format!("{} supervisor_act=on", status("on")),
        "OK schema=1 supervisor=x".to_string(),
        "ERR busy supervisor_act=observe".to_string(),
        String::new(),
    ] {
        assert_eq!(engine_act(&reply), None, "{reply:?}");
    }
}
