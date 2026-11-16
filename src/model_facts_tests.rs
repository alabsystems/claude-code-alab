// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The reader's tests. Those ported from aterm (`footer_tests.rs` and
//! `claude_model_tests.rs` on branch `s3607/small-type`) keep their names and
//! their rows; where this reader differs from aterm's (no launch flag or
//! launch card stands for a fact, an unanswered `/effort` leaves the key
//! unread) the expectation says so.

use super::*;
use std::sync::atomic::{AtomicU64, Ordering};

#[path = "model_facts_lookup_tests.rs"]
mod lookup_tests;

/// `text` as a JSON string.
fn json_string(text: &str) -> String {
    let mut out = String::from("\"");
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            c if u32::from(c) < 0x20 => out.push_str(&format!("\\u{:04x}", u32::from(c))),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn ms(stamp: &str) -> Option<u64> {
    stamp_ms(stamp)
}

fn is(value: &str) -> Said<String> {
    Said::Is(value.to_owned())
}

fn body(rows: &[&str]) -> String {
    rows.iter().map(|r| format!("{r}\n")).collect()
}

fn answer_at(model: &str, at: &str, effort: Option<&str>) -> String {
    let effort = effort.map_or(String::new(), |e| format!(r#","effort":"{e}""#));
    format!(r#"{{"type":"assistant","timestamp":"{at}"{effort},"message":{{"model":"{model}"}}}}"#)
}

fn command_at(name: &str, at: &str) -> String {
    format!(
        r#"{{"type":"user","timestamp":"{at}","message":{{"role":"user","content":"<command-name>/{name}</command-name>\n            <command-message>{name}</command-message>\n            <command-args></command-args>"}}}}"#
    )
}

fn result_at(at: &str, text: &str) -> String {
    format!(
        r#"{{"type":"user","timestamp":"{at}","message":{{"role":"user","content":{}}}}}"#,
        json_string(&format!(
            "<local-command-stdout>{text}</local-command-stdout>"
        ))
    )
}

/// 2.1.321's request row, in the shape it writes (trimmed of betas and context management).
fn request(source: &str, model: &str, effort: &str) -> String {
    format!(
        r#"{{"type":"api-request","sessionId":"e1b4","timestamp":"2026-11-10T00:11:47.081Z","version":"2.1.321","querySource":"{source}","params":{{"model":"{model}","max_tokens":128000,"thinking":{{"type":"adaptive"}},"output_config":{{"effort":"{effort}"}}}}}}"#
    )
}

/// 2.1.322's row for a command typed while a turn runs: `system`, subtype
/// `local_command`, the text at the top level, stamped with the moment it was typed.
fn mid_turn_command(at: &str, content: &str) -> String {
    format!(
        r#"{{"isSidechain":false,"type":"system","subtype":"local_command","content":{},"level":"info","timestamp":"{at}","isMeta":false,"sessionId":"6397","version":"2.1.322"}}"#,
        json_string(content)
    )
}

/// A 2.1.322 request row at `at`.
fn request_at(at: &str, effort: &str) -> String {
    format!(
        r#"{{"type":"api-request","sessionId":"6397","timestamp":"{at}","version":"2.1.322","querySource":"repl_main_thread","params":{{"model":"claude-opus-5-5","output_config":{{"effort":"{effort}"}}}}}}"#
    )
}

/// A key attachment row as Claude Code 2.1.322 writes one, with a synthetic session id.
fn key_row(kind: &str, at: &str) -> String {
    format!(
        r#"{{"type":"attachment","isSidechain":false,"attachment":{{"type":"ultra_effort_{kind}","reminderType":"full"}},"sessionId":"6397","timestamp":"{at}"}}"#
    )
}

/// The key word as Claude Code's results begin with it: capitalised.
fn capital_key() -> String {
    let mut chars = TOP_EFFORT_KEY.chars();
    let first = chars.next().expect("a key");
    format!("{}{}", first.to_ascii_uppercase(), chars.as_str())
}

const FAST_ON_COLOURED: &str = "\u{1b}[38;2;255;106;0m\u{21AF}\u{1b}[39m Fast mode ON \u{00B7} model set to `Opus 5.5` \u{00B7} $30/$150 per Mtok";

#[test]
fn model_ids_read_as_the_vendor_spells_them() {
    assert_eq!(model_display("claude-opus-5-5"), "Opus 5.5");
    assert_eq!(model_display("claude-opus-5"), "Opus 5");
    assert_eq!(model_display("claude-fable-5-1"), "Fable 5.1");
    assert_eq!(model_display("claude-sonnet-5"), "Sonnet 5");
    assert_eq!(model_display("claude-haiku-4-5-20251001"), "Haiku 4.5");
    assert_eq!(model_display("claude-opus-5-5[1m]"), "Opus 5.5");
    assert_eq!(model_display("gpt-5-codex"), "gpt-5-codex");
}

#[test]
fn a_result_row_names_its_kind_its_model_and_its_effort() {
    let row = |text: &str| format!("<local-command-stdout>{text}</local-command-stdout>");
    assert_eq!(
        model_result_of(&row(
            "Set model to `Sonnet 5` for this session only with `high` effort"
        )),
        Some(ModelResult {
            kind: ModelResultKind::Set,
            display: Some("Sonnet 5".into()),
            effort: Some("high".into()),
        })
    );
    assert_eq!(
        strip_sgr(FAST_ON_COLOURED),
        "\u{21AF} Fast mode ON \u{00B7} model set to `Opus 5.5` \u{00B7} $30/$150 per Mtok"
    );
    assert_eq!(
        model_result_of(&row(FAST_ON_COLOURED)),
        Some(ModelResult {
            kind: ModelResultKind::FastOn,
            display: Some("Opus 5.5".into()),
            effort: None,
        })
    );
    assert_eq!(
        model_result_of("why did it say Set model to `claude-fable-5-1`?"),
        None
    );
}

#[test]
fn stamps_read_to_the_millisecond() {
    assert_eq!(ms("1970-01-01T00:00:00Z"), Some(0));
    assert_eq!(ms("1970-01-01T00:00:01.5Z"), Some(1_500));
    // `date -j -u -f %Y-%m-%dT%H:%M:%S 2026-11-13T04:18:02 +%s` is 1794543482.
    assert_eq!(ms("2026-11-13T04:18:02.796Z"), Some(1_794_543_482_796));
    assert_eq!(
        ms("2026-11-13T04:18:02.796123456Z"),
        Some(1_794_543_482_796)
    );
    assert_eq!(ms("2024-02-29T00:00:00Z"), Some(1_709_164_800_000));
    for bad in [
        "",
        "2026-11-13T04:18:02",
        "2026-11-13T04:18:02.Z",
        "2026-11-13T04:18:02.1234567890Z",
        "2026-11-13 04:18:02Z",
        "2026-13-13T04:18:02Z",
        "2026-11-13T24:18:02Z",
        "2026-11-13T04:18:02+00:00",
        "1969-12-31T23:59:59Z",
        "2026-11-1xT04:18:02Z",
    ] {
        assert_eq!(ms(bad), None, "{bad:?}");
    }
}

#[test]
fn the_journal_line_names_each_fact_or_a_dash() {
    assert_eq!(
        ModelFacts::default().journal_line(),
        "facts model=- effort=- key=-"
    );
    let stated = ModelFacts {
        model: is("Opus 5.5"),
        effort: is("high"),
        key: Said::Is(true),
        ..ModelFacts::default()
    };
    assert_eq!(
        stated.journal_line(),
        "facts model=Opus_5.5 effort=high key=on"
    );
    let unread = ModelFacts {
        model: Said::Unread,
        effort: Said::Unread,
        key: Said::Is(false),
        ..ModelFacts::default()
    };
    assert_eq!(unread.journal_line(), "facts model=- effort=- key=off");
}

#[test]
fn a_main_thread_request_shows_a_switch_before_its_reply() {
    // Measured 2026-11-09 on 2.1.321: the top-effort key turned on mid-turn wrote no command
    // row, and the next request carried `xhigh` minutes before the reply's first row.
    let answer = r#"{"type":"assistant","effort":"medium","message":{"model":"claude-fable-5-2"}}"#;
    let sent = format!(
        "{answer}\n{}\n",
        request("repl_main_thread", "claude-fable-5-2", "xhigh")
    );
    let facts = tail_facts(&sent, None);
    assert_eq!(facts.model, is("Fable 5.2"));
    assert_eq!(facts.effort, is("xhigh"));

    let switched = format!(
        "{answer}\n{}\n",
        request("repl_main_thread", "claude-opus-5-5", "high")
    );
    let facts = tail_facts(&switched, None);
    assert_eq!(facts.model, is("Opus 5.5"));
    assert_eq!(facts.effort, is("high"));

    // A side query on another model and effort says nothing of the conversation's.
    let side = format!(
        "{answer}\n{}\n",
        request("compact", "claude-haiku-4-5", "low")
    );
    let facts = tail_facts(&side, None);
    assert_eq!(facts.model, is("Fable 5.2"));
    assert_eq!(facts.effort, is("medium"));

    // A newer `/effort` result outranks the request before it.
    let set = format!(
        "{sent}{}\n",
        r#"{"type":"user","message":{"role":"user","content":"<local-command-stdout>Set effort level to low</local-command-stdout>"}}"#
    );
    assert_eq!(tail_facts(&set, None).effort, is("low"));
}

#[test]
fn a_command_typed_mid_turn_shows_once_its_rows_land() {
    // Measured 2026-11-11 on 2.1.322 (lab, stub tool mode): `/effort high` typed while the
    // turn's Bash call ran wrote nothing to the transcript then; the turn's second request
    // still went at `low`; once the reply landed, Claude Code wrote the command and its result
    // as two `system` `local_command` rows stamped with the moment it was typed.
    let start = ms("2026-11-11T03:55:00Z");
    let turn = [
        r#"{"type":"user","isSidechain":false,"timestamp":"2026-11-11T03:55:45.011Z","message":{"role":"user","content":"toolstart"}}"#.to_owned(),
        request_at("2026-11-11T03:55:45.021Z", "low"),
        r#"{"type":"assistant","isSidechain":false,"timestamp":"2026-11-11T03:55:45.024Z","effort":"low","message":{"model":"claude-opus-5-5"}}"#.to_owned(),
        request_at("2026-11-11T03:55:57.088Z", "low"),
        r#"{"type":"assistant","isSidechain":false,"timestamp":"2026-11-11T03:55:57.090Z","effort":"low","message":{"model":"claude-opus-5-5"}}"#.to_owned(),
    ];
    let typed = "2026-11-11T03:55:49.135Z";
    let landed = [
        mid_turn_command(
            typed,
            "<command-name>/effort</command-name>\n            <command-message>effort</command-message>\n            <command-args>high</command-args>",
        ),
        mid_turn_command(
            typed,
            "<local-command-stdout>Set effort level to high (saved as your default for new sessions): Comprehensive implementation with extensive testing and documentation</local-command-stdout>",
        ),
        r#"{"type":"system","subtype":"turn_duration","isSidechain":false,"timestamp":"2026-11-11T03:55:57.092Z","durationMs":12081}"#.to_owned(),
    ];
    let read = |rows: &[String]| {
        let rows: Vec<&str> = rows.iter().map(String::as_str).collect();
        tail_facts(&body(&rows), start)
    };
    assert_eq!(
        read(&turn).effort,
        is("low"),
        "CONTROL: before the rows land, the turn's requests say what they were sent with"
    );
    let ended: Vec<String> = turn.iter().chain(&landed).cloned().collect();
    let facts = read(&ended);
    assert_eq!(facts.effort, is("high"), "the switch, at the turn's end");
    assert_eq!(facts.model, is("Opus 5.5"), "the model stands");

    // The next request is the authority once it is sent.
    let mut next = ended.clone();
    next.push(request_at("2026-11-11T03:56:10.000Z", "xhigh"));
    assert_eq!(read(&next).effort, is("xhigh"));

    // The top-effort key turned off mid-turn (measured the same day): its result reads too.
    let key = capital_key();
    let mut off = turn.to_vec();
    off.push(mid_turn_command(
        typed,
        &format!(
            "<command-name>/effort</command-name>\n            <command-message>effort</command-message>\n            <command-args>{TOP_EFFORT_KEY} off</command-args>"
        ),
    ));
    off.push(mid_turn_command(
        typed,
        &format!("<local-command-stdout>{key} off. Effort stays medium.</local-command-stdout>"),
    ));
    let facts = read(&off);
    assert_eq!((facts.effort, facts.key), (is("medium"), Said::Is(false)));

    // Another system row quoting a command result is no command.
    let mut quoted = turn.to_vec();
    quoted.push(
        r#"{"type":"system","subtype":"informational","isSidechain":false,"timestamp":"2026-11-11T03:55:58.000Z","content":"<local-command-stdout>Set effort level to max</local-command-stdout>"}"#.to_owned(),
    );
    assert_eq!(read(&quoted).effort, is("low"));
}

#[test]
fn the_tail_takes_the_newest_main_thread_model_and_effort() {
    let tail = concat!(
        r#"{"type":"assistant","effort":"high","message":{"model":"claude-opus-5"}}"#,
        "\n",
        r#"{"type":"assistant","effort":"medium","message":{"model":"claude-opus-5-5"}}"#,
        "\n",
        r#"{"type":"assistant","isSidechain":true,"effort":"low","message":{"model":"claude-haiku-4-5"}}"#,
        "\n",
        r#"{"type":"assistant","message":{"model":"<synthetic>"}}"#,
        "\n",
        r#"{"type":"user","effort":"xhigh"}"#,
        "\n",
    );
    let facts = tail_facts(tail, None);
    assert_eq!(facts.model, is("Opus 5.5"));
    assert_eq!(facts.effort, is("xhigh"));

    let answered = format!(
        "{tail}{}\n",
        r#"{"type":"assistant","effort":"max","message":{"model":"claude-opus-5-5"}}"#
    );
    assert_eq!(tail_facts(&answered, None).effort, is("max"));
    assert_eq!(
        tail_facts("", None),
        ModelFacts::default(),
        "a fresh session knows neither"
    );
}

#[test]
fn an_unreadable_model_choice_voids_model_and_effort() {
    let answer = r#"{"type":"assistant","timestamp":"2026-09-24T04:13:30.000Z","effort":"xhigh","message":{"model":"claude-opus-5"}}"#;
    let command = r#"{"type":"user","timestamp":"2026-09-24T04:13:31.000Z","message":{"role":"user","content":"<command-name>/model</command-name>\n            <command-message>model</command-message>\n            <command-args></command-args>"}}"#;
    let unread = r#"{"type":"user","timestamp":"2026-09-24T04:13:31.000Z","message":{"role":"user","content":"<local-command-stdout>Model switch to X was blocked by a PreModelSwitch hook</local-command-stdout>"}}"#;
    let start = ms("2026-09-24T04:13:26Z");
    let void = ModelFacts {
        model: Said::Unread,
        effort: Said::Unread,
        ..ModelFacts::default()
    };
    assert_eq!(tail_facts(&format!("{answer}\n{command}\n"), start), void);
    assert_eq!(
        tail_facts(&format!("{answer}\n{command}\n{unread}\n"), start),
        void,
        "a result this build does not read is no model"
    );
    assert_eq!(
        tail_facts(&format!("{answer}\n"), start).model,
        is("Opus 5")
    );
    for quoted in [
        r#"{"type":"assistant","timestamp":"2026-09-24T04:13:31.000Z","message":{"model":"<synthetic>","content":"<command-name>/model</command-name>"}}"#,
        r#"{"type":"user","timestamp":"2026-09-24T04:13:31.000Z","message":{"content":[{"type":"tool_result","content":"<command-name>/model</command-name>"}]}}"#,
        r#"{"type":"user","timestamp":"2026-09-24T04:13:31.000Z","message":{"content":"why does <command-name>/model</command-name> exist"}}"#,
        r#"{"type":"user","timestamp":"2026-09-24T04:13:31.000Z","message":{"content":"it printed <local-command-stdout>Set model to `Haiku 4.5`</local-command-stdout>"}}"#,
        r#"{"type":"assistant","timestamp":"2026-09-24T04:13:31.000Z","message":{"model":"<synthetic>","content":"<local-command-stdout>Set model to `Haiku 4.5`</local-command-stdout>"}}"#,
    ] {
        assert_eq!(
            tail_facts(&format!("{answer}\n{quoted}\n"), start).model,
            is("Opus 5"),
            "{quoted}"
        );
    }
}

#[test]
fn an_unstamped_row_is_not_attributed() {
    let unstamped =
        r#"{"type":"assistant","effort":"xhigh","message":{"model":"claude-opus-5-5"}}"#;
    let stamped = r#"{"type":"assistant","timestamp":"2026-09-24T04:15:32.000Z","effort":"xhigh","message":{"model":"claude-opus-5-5"}}"#;
    let start = ms("2026-09-24T04:13:26Z");
    assert_eq!(
        tail_facts(&format!("{unstamped}\n"), start),
        ModelFacts::default()
    );
    assert_eq!(
        tail_facts(&format!("{stamped}\n"), start).model,
        is("Opus 5.5")
    );
}

#[test]
fn a_model_result_display_is_the_vendors_name() {
    let result = |text: &str| {
        let row = format!(
            r#"{{"type":"user","timestamp":"2026-09-24T04:13:31.000Z","message":{{"content":{}}}}}"#,
            json_string(&format!(
                "<local-command-stdout>{text}</local-command-stdout>"
            ))
        );
        tail_facts(&format!("{row}\n"), None)
    };
    for (text, model) in [
        (
            "Set model to `Opus 5.5 (default)` and saved as your default for new sessions",
            "Opus 5.5",
        ),
        (
            "Set model to `Opus 5 (1M context) (default)` for this session only",
            "Opus 5",
        ),
        ("Set model to `Fable 5.1`", "Fable 5.1"),
        ("Set model to `Fable 5.1 (1M context)`", "Fable 5.1"),
        (
            "Set model to \u{1b}[1mclaude-fable-5-1\u{1b}[22m and saved as your default",
            "Fable 5.1",
        ),
        (
            "\u{21AF} Fast mode ON \u{00B7} model set to `Opus 5.5` \u{00B7} $30/$150 per Mtok",
            "Opus 5.5",
        ),
        (FAST_ON_COLOURED, "Opus 5.5"),
    ] {
        assert_eq!(result(text).model, is(model), "{text:?}");
    }
    let with_effort = result(&format!(
        "Set model to `Opus 5.5` and saved as your default for new sessions with `{TOP_EFFORT_KEY}` effort ({TOP_EFFORT_KEY} applies to this session only)",
    ));
    // The key is no level: the result names the model and says the key is on, and the level
    // waits for a row that states one.
    assert_eq!(
        (with_effort.model, with_effort.effort, with_effort.key),
        (is("Opus 5.5"), Said::Unread, Said::Is(true))
    );
    for text in [
        "Set model to `Opus\u{7}5`",
        "Set model to `Opus\n5`",
        "Set model to `A very long name that no model of anyone's has ever carried at all`",
        "Set model to ``Opus``",
        "Set model to Opus 5.5",
    ] {
        assert_eq!(result(text).model, Said::Unread, "{text:?}");
    }
    for text in [
        "Cloud session couldn't switch to `Opus 5.5`",
        "Model switch to X was blocked by a PreModelSwitch hook",
        "Current model: `Opus 5.5`",
        "Kept Fast mode ON \u{00B7} model set to `Opus 5.5`",
        "\u{1b}[38;2;255;106;0m\u{21AF}\u{1b}[39m Kept Fast mode ON \u{00B7} model set to `Opus 5.5`",
        "Fast mode ON",
        "\u{1b}[38;2;255;106;0m\u{21AF}\u{1b}[39m Fast mode ON \u{00B7} $30/$150 per Mtok",
        "Fast mode OFF",
    ] {
        assert_eq!(result(text).model, Said::Unsaid, "{text:?}");
    }
}

#[test]
fn a_kept_model_leaves_the_facts_as_they_were() {
    const ANSWER: &str = r#"{"parentUuid":"00000000-0000-4000-8000-000000000030","isSidechain":false,"type":"assistant","effort":"xhigh","message":{"model":"claude-fable-5-1","role":"assistant","content":[{"type":"text","text":"Done."}]},"uuid":"00000000-0000-4000-8000-000000000031","timestamp":"2026-09-22T17:05:50.000Z","userType":"external","entrypoint":"cli","cwd":"/h/u/ty","sessionId":"00000000-0000-4000-8000-000000000004","version":"2.1.278"}"#;
    const KEPT_COMMAND: &str = r#"{"parentUuid":"00000000-0000-4000-8000-000000000031","isSidechain":false,"type":"system","subtype":"local_command","content":"<command-name>/model</command-name>\n            <command-message>model</command-message>\n            <command-args></command-args>","level":"info","timestamp":"2026-09-22T17:06:02.248Z","uuid":"00000000-0000-4000-8000-000000000032","isMeta":false,"userType":"external","entrypoint":"cli","cwd":"/h/u/ty","sessionId":"00000000-0000-4000-8000-000000000004","version":"2.1.278"}"#;
    const KEPT_RESULT: &str = r#"{"parentUuid":"00000000-0000-4000-8000-000000000032","isSidechain":false,"type":"system","subtype":"local_command","content":"<local-command-stdout>Kept model as `Fable 5.1`</local-command-stdout>","level":"info","timestamp":"2026-09-22T17:06:02.248Z","uuid":"00000000-0000-4000-8000-000000000033","isMeta":false,"commandRun":{"command":"model","args":""},"userType":"external","entrypoint":"cli","cwd":"/h/u/ty","sessionId":"00000000-0000-4000-8000-000000000004","version":"2.1.278"}"#;
    let start = ms("2026-09-22T17:00:00Z");
    let before = tail_facts(&format!("{ANSWER}\n"), start);
    let kept = tail_facts(&format!("{ANSWER}\n{KEPT_COMMAND}\n{KEPT_RESULT}\n"), start);
    assert_eq!(kept, before, "a kept model changes nothing");
    assert_eq!((kept.model, kept.effort), (is("Fable 5.1"), is("xhigh")));
    let choice = KEPT_COMMAND.replace(
        r#""type":"system","subtype":"local_command","content":"#,
        r#""type":"user","message":{"role":"user","content":"#,
    );
    let choice = choice.replacen(r#"","level""#, r#""},"level""#, 1);
    let voided = tail_facts(&format!("{ANSWER}\n{choice}\n"), start);
    assert_eq!(
        (voided.model, voided.effort),
        (Said::Unread, Said::Unread),
        "{choice}"
    );
}

#[test]
fn a_switch_that_names_no_effort_keeps_it_only_for_the_same_model() {
    let start = ms("2026-09-28T15:00:00Z");
    let opus = answer_at("claude-opus-5-5", "2026-09-28T15:01:00.000Z", Some("xhigh"));
    let switch = |at: &str, to: &str| {
        [
            command_at("model", at),
            result_at(at, &format!("Set model to `{to}` for this session only")),
        ]
    };
    let effort_high = |at: &str| {
        [
            command_at("effort", at),
            result_at(
                at,
                "Set effort level to high (this session only): Deeper reasoning",
            ),
        ]
    };
    let facts = |rows: &[&str]| {
        let facts = tail_facts(&body(rows), start);
        (facts.model, facts.effort)
    };
    let both = |model: &str, effort: Option<&str>| (is(model), effort.map_or(Said::Unread, is));
    let [c, r] = switch("2026-09-28T15:02:00.000Z", "Haiku 4.5");
    let haiku = answer_at(
        "claude-haiku-4-5-20251001",
        "2026-09-28T15:03:00.000Z",
        None,
    );
    assert_eq!(facts(&[&opus, &c, &r]), both("Haiku 4.5", None));
    assert_eq!(
        facts(&[&opus, &c, &r, &haiku]),
        both("Haiku 4.5", None),
        "never Haiku 4.5 xhigh, after any number of Haiku answers"
    );
    let [c, r] = switch("2026-09-28T15:02:00.000Z", "Opus 5.5 (default)");
    assert_eq!(facts(&[&opus, &c, &r]), both("Opus 5.5", Some("xhigh")));
    let [c1, r1] = switch("2026-09-28T15:02:00.000Z", "Sonnet 5");
    let [c2, r2] = switch("2026-09-28T15:02:30.000Z", "Opus 5.5");
    assert_eq!(facts(&[&opus, &c1, &r1, &c2, &r2]), both("Opus 5.5", None));
    let [e1, e2] = effort_high("2026-09-28T15:02:10.000Z");
    assert_eq!(
        facts(&[&opus, &c1, &r1, &e1, &e2]),
        both("Sonnet 5", Some("high"))
    );
    let [e1, e2] = effort_high("2026-09-28T15:01:30.000Z");
    assert_eq!(facts(&[&opus, &e1, &e2, &c1, &r1]), both("Sonnet 5", None));
    let [c, r] = switch("2026-09-28T15:02:00.000Z", "Opus 5.5");
    assert_eq!(
        facts(&[&opus, &e1, &e2, &c, &r]),
        both("Opus 5.5", Some("high"))
    );
    let clause = result_at(
        "2026-09-28T15:02:00.000Z",
        "Set model to `Sonnet 5` for this session only with `high` effort",
    );
    let c = command_at("model", "2026-09-28T15:02:00.000Z");
    assert_eq!(facts(&[&opus, &c, &clause]), both("Sonnet 5", Some("high")));
    let [c, r] = switch("2026-09-28T15:02:00.000Z", "Sonnet 5");
    let alone = tail_facts(&body(&[&c, &r]), start);
    assert_eq!(
        (alone.effort, alone.effort_for.as_deref()),
        (Said::Unsaid, Some("Sonnet 5"))
    );
}

#[test]
fn a_command_pairs_only_with_the_result_directly_under_it() {
    let start = ms("2026-09-28T15:00:00Z");
    let opus = answer_at("claude-opus-5-5", "2026-09-28T15:01:00.000Z", Some("xhigh"));
    let model_cmd = command_at("model", "2026-09-28T15:02:00.000Z");
    let unread = result_at("2026-09-28T15:02:00.000Z", "Set model to Fable 5.1");
    let fast_cmd = command_at("fast", "2026-09-28T15:02:30.000Z");
    let fast = result_at("2026-09-28T15:02:30.000Z", FAST_ON_COLOURED);
    let facts = |rows: &[&str]| {
        let facts = tail_facts(&body(rows), start);
        (facts.model, facts.effort)
    };
    assert_eq!(
        facts(&[&opus, &model_cmd, &unread]),
        (Said::Unread, Said::Unread)
    );
    assert_eq!(
        facts(&[&opus, &model_cmd, &unread, &fast_cmd, &fast]),
        (is("Opus 5.5"), Said::Unread)
    );
    assert_eq!(
        facts(&[&opus, &model_cmd, &fast_cmd, &fast]),
        (is("Opus 5.5"), Said::Unread)
    );
    assert_eq!(
        facts(&[&opus, &fast_cmd, &fast]),
        (is("Opus 5.5"), is("xhigh"))
    );
    let fable = answer_at("claude-fable-5-1", "2026-09-28T15:01:00.000Z", Some("high"));
    assert_eq!(
        facts(&[&fable, &fast_cmd, &fast]),
        (is("Opus 5.5"), Said::Unread)
    );
    let off = result_at("2026-09-28T15:02:30.000Z", "Fast mode OFF");
    assert_eq!(
        facts(&[&fable, &fast_cmd, &off]),
        (is("Fable 5.1"), is("high"))
    );
}

/// aterm's floor is a second; this one is the record's `startedAt` to the
/// millisecond, so a row from earlier in the same second is not the process's.
#[test]
fn the_floor_is_the_processs_start_and_the_first_row_before_it_ends_the_scan() {
    let floor = ms("2026-09-28T15:00:00.500Z");
    let after = answer_at("claude-opus-5-5", "2026-09-28T15:00:00.900Z", Some("high"));
    let same_second = answer_at("claude-fable-5-1", "2026-09-28T15:00:00.100Z", None);
    let above = answer_at("claude-sonnet-5", "2026-09-28T15:10:00.000Z", Some("low"));
    let read = |rows: &[&str]| {
        let (facts, floored) = scan(body(rows).as_bytes(), floor, false, true);
        (facts.model, facts.effort, floored)
    };
    assert_eq!(read(&[&after]), (is("Opus 5.5"), is("high"), false));
    assert_eq!(
        read(&[&above, &same_second]),
        (Said::Unsaid, Said::Unsaid, true)
    );
}

#[test]
fn an_effort_result_names_the_effort_at_once() {
    let start = ms("2026-09-24T04:13:26Z");
    let answer = r#"{"type":"assistant","timestamp":"2026-09-24T04:13:30.000Z","effort":"high","message":{"model":"claude-opus-5-5"}}"#;
    let command = r#"{"type":"user","timestamp":"2026-09-24T04:13:31.000Z","message":{"content":"<command-name>/effort</command-name>\n            <command-message>effort</command-message>\n            <command-args></command-args>"}}"#;
    let effort = |text: Option<&str>| {
        let mut rows = format!("{answer}\n{command}\n");
        if let Some(text) = text {
            rows.push_str(&result_at("2026-09-24T04:13:31.000Z", text));
            rows.push('\n');
        }
        tail_facts(&rows, start)
    };
    let top = format!(
        "Set effort level to {TOP_EFFORT_KEY} (this session only): xhigh + dynamic workflow orchestration"
    );
    // The key's own result names no level, whatever words follow it: it says the key is on.
    let keyed = effort(Some(top.as_str()));
    assert_eq!((keyed.effort, keyed.key), (Said::Unread, Said::Is(true)));
    for (text, level) in [
        (
            "Set effort level to high (saved as your default for new sessions): Deeper reasoning",
            "high",
        ),
        (
            "Current effort level: max (Maximum reasoning; this session only)",
            "max",
        ),
        ("Set effort level to \u{1b}[1mlow\u{1b}[22m: Quick", "low"),
    ] {
        let facts = effort(Some(text));
        assert_eq!(facts.effort, is(level), "{text:?}");
        assert_eq!(facts.model, is("Opus 5.5"), "an effort leaves the model");
    }
    for text in [
        "Effort level set to auto for this session",
        "Set effort level to turbo: new",
    ] {
        assert_eq!(effort(Some(text)).effort, Said::Unread, "{text:?}");
    }
    assert_eq!(
        effort(None).effort,
        Said::Unread,
        "an unreadable result is no effort"
    );
    assert_eq!(tail_facts(&format!("{answer}\n"), start).effort, is("high"));
}

fn effort_after_answer(text: Option<&str>) -> ModelFacts {
    let start = ms("2026-10-19T10:00:00Z");
    let answer = answer_at("claude-opus-5-5", "2026-10-19T10:00:10.000Z", Some("high"));
    let command = command_at("effort", "2026-10-19T10:00:20.000Z");
    let mut rows = vec![answer, command];
    if let Some(text) = text {
        rows.push(result_at("2026-10-19T10:00:20.000Z", text));
    }
    let rows: Vec<&str> = rows.iter().map(String::as_str).collect();
    tail_facts(&body(&rows), start)
}

#[test]
fn a_top_effort_off_result_keeps_the_effort_and_names_the_key() {
    let key = capital_key();
    let facts = effort_after_answer(Some(&format!("{key} off. Effort stays xhigh.")));
    assert_eq!(facts.effort, is("xhigh"));
    assert_eq!(facts.key, Said::Is(false));
    assert_eq!(facts.model, is("Opus 5.5"), "the model stands");
    let bare = effort_after_answer(None);
    assert_eq!(bare.effort, Said::Unread);
    assert_eq!(
        bare.key,
        Said::Unread,
        "an `/effort` with no result may have been the key's: unread (aterm: unstated)"
    );
}

#[test]
fn a_top_effort_on_result_keeps_the_effort() {
    let key = capital_key();
    let on = |stays: &str| {
        effort_after_answer(Some(&format!(
            "{key} on (this session only): dynamic workflows on every task. Effort stays {stays}."
        )))
    };
    let facts = on("xhigh");
    assert_eq!((facts.effort, facts.key), (is("xhigh"), Said::Is(true)));
    let auto = on("auto");
    assert_eq!((auto.effort, auto.key), (Said::Unread, Said::Is(true)));
    let lower = effort_after_answer(Some(&format!("{TOP_EFFORT_KEY} off. Effort stays max.")));
    assert_eq!((lower.effort, lower.key), (is("max"), Said::Is(false)));
    let other = effort_after_answer(Some("Workspace off. Effort stays max."));
    assert_eq!((other.effort, other.key), (Said::Unread, Said::Unread));
}

#[test]
fn a_level_result_suffix_names_the_key() {
    let key = capital_key();
    let set = effort_after_answer(Some(&format!(
        "Set effort level to high (this session only): Comprehensive implementation \u{b7} {key} off"
    )));
    assert_eq!((set.effort, set.key), (is("high"), Said::Is(false)));
    let current = effort_after_answer(Some(&format!(
        "Current effort level: xhigh (Extended reasoning with thorough analysis) \u{b7} {key} on"
    )));
    assert_eq!((current.effort, current.key), (is("xhigh"), Said::Is(true)));
    let auto = effort_after_answer(Some(&format!("Effort level set to auto \u{b7} {key} off")));
    assert_eq!((auto.effort, auto.key), (Said::Unread, Said::Is(false)));
    let plain = effort_after_answer(Some("Set effort level to low (this session only): Quick"));
    assert_eq!((plain.effort, plain.key), (is("low"), Said::Unsaid));
    for tail in [
        "Fast mode off".to_owned(),
        format!("{key} online"),
        format!("{key}s off"),
    ] {
        let said = effort_after_answer(Some(&format!(
            "Set effort level to low (this session only): Quick \u{b7} {tail}"
        )));
        assert_eq!(said.key, Said::Unsaid, "{tail:?}");
    }
}

#[test]
fn a_refused_key_answers_its_command_and_decides_nothing() {
    let key = capital_key();
    for refusal in [
        format!(
            "{key} needs dynamic workflows enabled (see /config). Valid options are: low, medium, high, xhigh, max, auto"
        ),
        format!(
            "{key} isn't available on claude-haiku-4-5. Valid options are: low, medium, high, auto"
        ),
    ] {
        let facts = effort_after_answer(Some(&refusal));
        assert_eq!(facts.effort, is("high"), "{refusal:?}");
        assert_eq!(facts.key, Said::Unsaid, "{refusal:?}");
    }
    let unknown = effort_after_answer(Some("Effort is now something new"));
    assert_eq!((unknown.effort, unknown.key), (Said::Unread, Said::Unread));
}

/// THE 2.1.322 ROWS READ MODEL, LEVEL AND KEY APART: with the key in effect Claude Code writes
/// an `ultra_effort_enter` attachment with the prompt, sends the request at the session's own
/// level, and stamps the reply with it. The level is that word and never the key's, and the key
/// is on; an `ultra_effort_exit` newer than every enter reads it off, the level unchanged.
#[test]
fn the_keys_rows_name_the_key_and_leave_the_level_alone() {
    let start = ms("2026-11-11T03:55:00Z");
    let reply = |at: &str| {
        format!(
            r#"{{"type":"assistant","isSidechain":false,"timestamp":"{at}","effort":"medium","message":{{"model":"claude-fable-5-2"}}}}"#
        )
    };
    let request = request_at("2026-11-11T03:55:45.021Z", "medium")
        .replace("claude-opus-5-5", "claude-fable-5-2");
    let on = [
        key_row("enter", "2026-11-11T03:55:45.011Z"),
        request,
        reply("2026-11-11T03:55:45.024Z"),
    ];
    let rows: Vec<&str> = on.iter().map(String::as_str).collect();
    let facts = tail_facts(&body(&rows), start);
    assert_eq!(facts.model, is("Fable 5.2"));
    assert_eq!(facts.effort, is("medium"));
    assert_eq!(facts.key, Said::Is(true));

    let exit = key_row("exit", "2026-11-11T03:57:00.000Z");
    let after = reply("2026-11-11T03:57:02.000Z");
    let mut rows = rows;
    rows.extend([exit.as_str(), after.as_str()]);
    let facts = tail_facts(&body(&rows), start);
    assert_eq!(facts.effort, is("medium"));
    assert_eq!(
        facts.key,
        Said::Is(false),
        "the newest statement is the exit"
    );

    let none = [reply("2026-11-11T03:55:45.024Z")];
    let rows: Vec<&str> = none.iter().map(String::as_str).collect();
    assert_eq!(
        tail_facts(&body(&rows), start).key,
        Said::Unsaid,
        "rows that never state the key say nothing of it"
    );
}

/// THE KEY IS NO LEVEL (measured on 2.1.325, finding 1): turning it on and off
/// leaves the level the session had, and the key's word where a level stands
/// says the key is on and names no level. CONTROL: a level result in the same
/// place does move the level, so the slot is live.
#[test]
fn the_key_turned_on_and_off_leaves_the_level() {
    let start = ms("2026-11-13T10:00:00Z");
    let key = capital_key();
    let high = answer_at(
        "claude-sonnet-5-5",
        "2026-11-13T10:00:10.000Z",
        Some("high"),
    );
    let on = [
        command_at("effort", "2026-11-13T10:00:20.000Z"),
        result_at(
            "2026-11-13T10:00:20.000Z",
            &format!("{key} on (this session only): dynamic workflows. Effort stays high."),
        ),
    ];
    let off = [
        command_at("effort", "2026-11-13T10:00:30.000Z"),
        result_at(
            "2026-11-13T10:00:30.000Z",
            &format!("{key} off. Effort stays high."),
        ),
    ];
    let read = |rows: &[&str]| {
        let facts = tail_facts(&body(rows), start);
        (facts.effort, facts.key)
    };
    assert_eq!(read(&[&high]), (is("high"), Said::Unsaid));
    assert_eq!(read(&[&high, &on[0], &on[1]]), (is("high"), Said::Is(true)));
    assert_eq!(
        read(&[&high, &on[0], &on[1], &off[0], &off[1]]),
        (is("high"), Said::Is(false)),
        "the level is the one before the key, not the key's"
    );
    // The key's rows alone, over an answer: the level stands, the key moves.
    let enter = key_row("enter", "2026-11-13T10:00:40.000Z");
    let exit = key_row("exit", "2026-11-13T10:00:50.000Z");
    assert_eq!(read(&[&high, &enter]), (is("high"), Said::Is(true)));
    assert_eq!(read(&[&high, &enter, &exit]), (is("high"), Said::Is(false)));
    // A row that writes the key's word as its effort: the key is on, the level unread.
    let keyed = answer_at(
        "claude-sonnet-5-5",
        "2026-11-13T10:01:00.000Z",
        Some(TOP_EFFORT_KEY),
    );
    assert_eq!(read(&[&high, &keyed]), (Said::Unread, Said::Is(true)));
    let facts = tail_facts(&body(&[&high, &keyed]), start);
    assert!(
        !facts.journal_line().contains(TOP_EFFORT_KEY),
        "the key never stands in the level's place: {}",
        facts.journal_line()
    );
    // CONTROL: a level result moves the level.
    let low = [
        command_at("effort", "2026-11-13T10:00:30.000Z"),
        result_at(
            "2026-11-13T10:00:30.000Z",
            "Set effort level to low (this session only): Quick",
        ),
    ];
    assert_eq!(
        read(&[&high, &on[0], &on[1], &low[0], &low[1]]),
        (is("low"), Said::Is(true))
    );
}

/// AN UNKNOWN OR TRUNCATED ROW CHANGES NOTHING: a row type this build does not
/// read, a row cut short, a row that is not JSON and a row that is not UTF-8
/// each leave the facts as the rows under them made them. CONTROL: the same row
/// whole does change them.
#[test]
fn an_unknown_or_truncated_row_changes_nothing() {
    let start = ms("2026-11-13T10:00:00Z");
    let answer = answer_at("claude-opus-5-5", "2026-11-13T10:00:10.000Z", Some("high"));
    let before = tail_facts(&body(&[&answer]), start);
    assert_eq!(
        (before.model.clone(), before.effort.clone()),
        (is("Opus 5.5"), is("high"))
    );
    let choice = result_at("2026-11-13T10:00:20.000Z", "Set model to `Fable 5.1`");
    let cut = &choice[..choice.len() - 3];
    for odd in [
        r#"{"type":"progress","timestamp":"2026-11-13T10:00:20.000Z","effort":"low","data":{"model":"claude-haiku-4-5"}}"#,
        r#"{"type":"assistant","timestamp":"2026-11-13T10:00:20.000Z","effort":"low","message":{"model":"claude-haiku-4-5""#,
        cut,
        r#"assistant effort model claude-haiku-4-5"#,
        r#"{"type":"assistant","timestamp":"2026-11-13T10:00:20.000Z","message":{"model":"claude-haiku-4-5"},"effort":"low","effort":"max"}"#,
    ] {
        let after = tail_facts(&body(&[&answer, odd]), start);
        assert_eq!(after, before, "{odd}");
    }
    let mut not_utf8 = body(&[&answer]).into_bytes();
    not_utf8.extend_from_slice(b"{\"type\":\"assistant\",\"effort\":\"low\",\"x\":\"\xff\"}\n");
    assert_eq!(scan(&not_utf8, start, false, true).0, before, "not UTF-8");
    // CONTROL: the whole rows do.
    assert_eq!(
        tail_facts(&body(&[&answer, &choice]), start).model,
        is("Fable 5.1")
    );
    let whole = r#"{"type":"assistant","timestamp":"2026-11-13T10:00:20.000Z","effort":"low","message":{"model":"claude-haiku-4-5"}}"#;
    assert_eq!(
        tail_facts(&body(&[&answer, whole]), start).effort,
        is("low")
    );
}

// ───────────────────────── the reader over a config directory ─────────────────────────

/// When each test process starts (epoch ms): the floor of most processes below.
const START: &str = "2026-09-28T15:00:00.000Z";
const PID: u32 = 4242;

/// A private Claude Code config directory with one project, removed on drop.
struct World {
    root: PathBuf,
    config: PathBuf,
    project: PathBuf,
}

impl World {
    fn new(tag: &str) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let root = std::env::temp_dir().join(".ubd").join(format!(
            "claude-model-facts-{tag}-{}-{stamp}-{}.noindex",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let config = root.join("claude");
        let project = config.join("projects").join("-w-work");
        std::fs::create_dir_all(&project).expect("project dir");
        Self {
            root,
            config,
            project,
        }
    }

    fn path(&self, session: &str) -> PathBuf {
        self.project.join(format!("{session}.jsonl"))
    }

    fn write(&self, session: &str, rows: &[&str]) {
        std::fs::write(self.path(session), body(rows)).expect("transcript");
    }

    fn append(&self, session: &str, text: &str) {
        use std::io::Write as _;
        std::fs::OpenOptions::new()
            .append(true)
            .open(self.path(session))
            .expect("transcript")
            .write_all(text.as_bytes())
            .expect("append");
    }
}

impl Drop for World {
    fn drop(&mut self) {
        if !std::thread::panicking() {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }
}

fn look_update<'a>(
    reader: &'a mut Reader,
    world: &World,
    process: &Process<'_>,
    now: &str,
) -> Update<'a> {
    reader.look(&world.config, process, ms(now).expect("now"))
}

/// One look by `reader` of process [`PID`], started at `started`, in `session`.
fn look_from(
    reader: &mut Reader,
    world: &World,
    session: &str,
    started: &str,
    pinned: bool,
    now: &str,
) -> Option<String> {
    let pinning = [OsString::from("--model"), OsString::from("opus")];
    let process = Process {
        pid: PID,
        record: Some((session, ms(started).expect("start"))),
        args: if pinned { &pinning } else { &[] },
    };
    look_update(reader, world, &process, now)
        .journal_line
        .map(str::to_owned)
}

/// [`look_from`] for a process started at [`START`].
fn look(
    reader: &mut Reader,
    world: &World,
    session: &str,
    pinned: bool,
    now: &str,
) -> Option<String> {
    look_from(reader, world, session, START, pinned, now)
}

fn words(reader: &Reader) -> (Said<String>, Said<String>, Said<bool>) {
    let facts = reader.facts();
    (facts.model.clone(), facts.effort.clone(), facts.key.clone())
}

fn session(reader: &Reader) -> &SessionRead {
    reader.session.as_ref().expect("a session read")
}

#[test]
fn typed_unknown_state_changes_do_not_need_a_new_journal_line() {
    const S: &str = "00000000-0000-4000-8000-000000000106";
    let world = World::new("typed-unknown");
    let mut reader = Reader::default();
    let process = Process {
        pid: PID,
        record: Some((S, ms(START).unwrap())),
        args: &[],
    };
    let update = reader.look_with_budget(
        &world.config,
        &process,
        ms("2026-09-28T15:01:00Z").unwrap(),
        0,
    );
    assert!(update.facts_changed, "Unsaid and Unread are distinct facts");
    assert_eq!(update.facts.model, Said::Unread);
    assert_eq!(update.facts.effort, Said::Unread);
    assert_eq!(update.facts.key, Said::Unread);
    assert_eq!(update.journal_line, None, "both journal as '-'");

    let update = reader.look_with_budget(
        &world.config,
        &process,
        ms("2026-09-28T15:01:00.500Z").unwrap(),
        0,
    );
    assert!(
        !update.facts_changed,
        "the paced refusal states nothing new"
    );
    assert_eq!(update.journal_line, None);
    assert_eq!(session(&reader).lookups, 1);

    let other = Process {
        pid: PID + 1,
        record: None,
        args: &[],
    };
    let update = look_update(&mut reader, &world, &other, "2026-09-28T15:01:01Z");
    assert!(update.facts_changed, "a new process starts Unsaid");
    assert_eq!(update.facts.model, Said::Unsaid);
    assert_eq!(update.facts.effort, Said::Unsaid);
    assert_eq!(update.facts.key, Said::Unsaid);
    assert_eq!(
        update.journal_line, None,
        "the journal still has no new text"
    );

    let update = look_update(&mut reader, &world, &other, "2026-09-28T15:01:02Z");
    assert!(!update.facts_changed);
    assert_eq!(update.journal_line, None);
}

#[test]
fn typed_model_names_remain_distinct_when_their_journal_words_match() {
    const S: &str = "00000000-0000-4000-8000-000000000107";
    let world = World::new("typed-model-name");
    let mut reader = Reader::default();
    let process = Process {
        pid: PID,
        record: Some((S, ms(START).unwrap())),
        args: &[],
    };
    world.write(
        S,
        &[
            &command_at("model", "2026-09-28T15:00:10Z"),
            &result_at(
                "2026-09-28T15:00:11Z",
                "Set model to `Opus 5.5` with `high` effort",
            ),
        ],
    );
    let update = look_update(&mut reader, &world, &process, "2026-09-28T15:01:00Z");
    assert!(update.facts_changed);
    assert_eq!(update.facts.model, is("Opus 5.5"));
    assert_eq!(
        update.journal_line,
        Some("facts model=Opus_5.5 effort=high key=-")
    );

    world.append(
        S,
        &body(&[
            &command_at("model", "2026-09-28T15:01:10Z"),
            &result_at(
                "2026-09-28T15:01:11Z",
                "Set model to `Opus_5.5` with `high` effort",
            ),
        ]),
    );
    let update = look_update(&mut reader, &world, &process, "2026-09-28T15:01:12Z");
    assert!(update.facts_changed, "the exact stated model changed");
    assert_eq!(update.facts.model, is("Opus_5.5"));
    assert_eq!(update.facts.effort, is("high"));
    assert_eq!(
        update.journal_line, None,
        "space normalization hides this change"
    );

    world.append(
        S,
        &body(&[&answer_at("Opus_5.5", "2026-09-28T15:01:13Z", Some("high"))]),
    );
    let update = look_update(&mut reader, &world, &process, "2026-09-28T15:01:14Z");
    assert!(!update.facts_changed, "a new row can state the same facts");
    assert_eq!(update.journal_line, None);
    assert_eq!(session(&reader).appends, 2);

    let update = look_update(&mut reader, &world, &process, "2026-09-28T15:01:15Z");
    assert!(!update.facts_changed, "an unchanged source changes nothing");
    assert_eq!(update.journal_line, None);
    assert_eq!(session(&reader).appends, 2);
}

#[test]
fn private_model_choice_bookkeeping_is_not_a_typed_fact_change() {
    const S: &str = "00000000-0000-4000-8000-000000000108";
    let world = World::new("typed-choice");
    let mut reader = Reader::default();
    let process = Process {
        pid: PID,
        record: Some((S, ms(START).unwrap())),
        args: &[],
    };
    world.write(
        S,
        &[&answer_at(
            "claude-opus-5-5",
            "2026-09-28T15:00:10Z",
            Some("high"),
        )],
    );
    let update = look_update(&mut reader, &world, &process, "2026-09-28T15:01:00Z");
    assert!(update.facts_changed);
    assert!(!update.facts.chose);
    world.append(
        S,
        &body(&[
            &command_at("model", "2026-09-28T15:01:10Z"),
            &result_at(
                "2026-09-28T15:01:11Z",
                "Set model to `Opus 5.5` with `high` effort",
            ),
        ]),
    );
    let update = look_update(&mut reader, &world, &process, "2026-09-28T15:01:12Z");
    assert!(update.facts.chose, "the choice will affect a later resume");
    assert_eq!(update.facts.model, is("Opus 5.5"));
    assert_eq!(update.facts.effort, is("high"));
    assert!(
        !update.facts_changed,
        "the three public facts stayed the same"
    );
    assert_eq!(update.journal_line, None);
}

/// THE TRANSCRIPT IS READ AGAIN ONLY WHEN IT MOVED: an unchanged file is not
/// read; growth is read as an append; a rewrite of the same size and
/// modification time, a replacement and a shrink are read afresh; a file that
/// is gone is looked up again and read anew. Each change of what the facts
/// show is one journal line, and only that.
#[test]
fn the_transcript_is_read_again_only_when_it_moved() {
    const S: &str = "00000000-0000-4000-8000-000000000101";
    let world = World::new("gate");
    let row = |model: &str| answer_at(model, "2026-09-28T15:01:00.000Z", Some("high"));
    let mut reader = Reader::default();
    world.write(S, &[&row("claude-opus-5")]);
    assert_eq!(
        look(&mut reader, &world, S, false, "2026-09-28T15:01:10Z").as_deref(),
        Some("facts model=Opus_5 effort=high key=-")
    );
    assert_eq!(
        look(&mut reader, &world, S, false, "2026-09-28T15:01:11Z"),
        None
    );
    assert_eq!(session(&reader).reads, 1, "an unchanged file is read once");

    world.append(S, &format!("{}\n", row("claude-opus-6")));
    assert_eq!(
        look(&mut reader, &world, S, false, "2026-09-28T15:01:12Z").as_deref(),
        Some("facts model=Opus_6 effort=high key=-")
    );
    assert_eq!((session(&reader).reads, session(&reader).appends), (2, 1));

    // The same size and modification time, other rows: the change time differs.
    let path = world.path(S);
    let old_modified = std::fs::metadata(&path).unwrap().modified().unwrap();
    let whole = body(&[&row("claude-opus-5"), &row("claude-opus-7")]);
    assert_eq!(whole.len() as u64, std::fs::metadata(&path).unwrap().len());
    std::fs::write(&path, &whole).unwrap();
    std::fs::OpenOptions::new()
        .write(true)
        .open(&path)
        .unwrap()
        .set_times(std::fs::FileTimes::new().set_modified(old_modified))
        .unwrap();
    assert_eq!(
        look(&mut reader, &world, S, false, "2026-09-28T15:01:13Z").as_deref(),
        Some("facts model=Opus_7 effort=high key=-")
    );
    assert_eq!(
        (session(&reader).reads, session(&reader).appends),
        (3, 1),
        "a rewrite in place of the same length is read afresh"
    );

    // A replacement of the same size and modification time.
    let replacement = world.project.join("replacement.tmp");
    std::fs::write(
        &replacement,
        body(&[&row("claude-opus-5"), &row("claude-opus-8")]),
    )
    .unwrap();
    std::fs::OpenOptions::new()
        .write(true)
        .open(&replacement)
        .unwrap()
        .set_times(std::fs::FileTimes::new().set_modified(old_modified))
        .unwrap();
    std::fs::rename(&replacement, &path).unwrap();
    assert_eq!(
        look(&mut reader, &world, S, false, "2026-09-28T15:01:14Z").as_deref(),
        Some("facts model=Opus_8 effort=high key=-")
    );
    assert_eq!((session(&reader).reads, session(&reader).appends), (4, 1));

    // Gone, then back with less in it: looked up again and read afresh.
    std::fs::remove_file(&path).unwrap();
    assert_eq!(
        look(&mut reader, &world, S, false, "2026-09-28T15:01:15Z"),
        None
    );
    assert_eq!(
        words(&reader).0,
        is("Opus 8"),
        "a missing file states nothing new"
    );
    world.write(S, &[&row("claude-opus-9")]);
    assert_eq!(
        look(&mut reader, &world, S, false, "2026-09-28T15:01:16Z").as_deref(),
        Some("facts model=Opus_9 effort=high key=-")
    );
    assert_eq!((session(&reader).reads, session(&reader).appends), (5, 1));
}

/// A transcript not found is looked up at most once per [`LOOKUP_RETRY_MS`]:
/// a lookup reads every project directory. CONTROL: once the retry time has
/// passed it is looked up, found and read.
#[test]
fn a_missing_transcript_is_looked_up_at_most_once_a_second() {
    const S: &str = "00000000-0000-4000-8000-000000000102";
    let world = World::new("lookup");
    let mut reader = Reader::default();
    assert_eq!(
        look(&mut reader, &world, S, false, "2026-09-28T15:01:00.000Z"),
        None
    );
    assert_eq!(session(&reader).lookups, 1);
    world.write(
        S,
        &[&answer_at(
            "claude-opus-5-5",
            "2026-09-28T15:01:00.100Z",
            Some("low"),
        )],
    );
    assert_eq!(
        look(&mut reader, &world, S, false, "2026-09-28T15:01:00.250Z"),
        None
    );
    assert_eq!(
        look(&mut reader, &world, S, false, "2026-09-28T15:01:00.999Z"),
        None
    );
    assert_eq!(
        session(&reader).lookups,
        1,
        "not looked up again within the second"
    );
    assert_eq!(
        look(&mut reader, &world, S, false, "2026-09-28T15:01:01.000Z").as_deref(),
        Some("facts model=Opus_5.5 effort=low key=-")
    );
    assert_eq!(session(&reader).lookups, 2);
}

/// A ROW STILL BEING WRITTEN CHANGES NOTHING UNTIL IT IS WHOLE: the reader
/// stops at the last row end and reads the rest once it ends. CONTROL: the
/// finished row changes the facts.
#[test]
fn a_row_still_being_written_counts_once_it_is_whole() {
    const S: &str = "00000000-0000-4000-8000-000000000103";
    let world = World::new("partial");
    let mut reader = Reader::default();
    world.write(
        S,
        &[&answer_at(
            "claude-opus-5-5",
            "2026-09-28T15:01:00.000Z",
            Some("high"),
        )],
    );
    assert!(look(&mut reader, &world, S, false, "2026-09-28T15:01:10Z").is_some());
    let choice = result_at("2026-09-28T15:01:20.000Z", "Set model to `Fable 5.1`");
    let (head, rest) = choice.split_at(choice.len() / 2);
    world.append(S, head);
    assert_eq!(
        look(&mut reader, &world, S, false, "2026-09-28T15:01:21Z"),
        None
    );
    assert_eq!(
        words(&reader).0,
        is("Opus 5.5"),
        "half a row is no statement"
    );
    world.append(S, &format!("{rest}\n"));
    assert_eq!(
        look(&mut reader, &world, S, false, "2026-09-28T15:01:22Z").as_deref(),
        Some("facts model=Fable_5.1 effort=- key=-"),
        "CONTROL: the whole row is read, and its model names no level"
    );
    assert_eq!(session(&reader).appends, 2, "both were appends");
}

/// A statement that more than a tail of newer rows has pushed back is
/// carried, and every appended row is read: the answer below a 300 KiB row
/// still names the model, and a choice made right before another such row
/// counts. CONTROL: the file's tail alone, read afresh, names no model.
#[test]
fn an_answer_past_the_tail_window_is_carried_for_the_same_process() {
    const S: &str = "00000000-0000-4000-8000-000000000104";
    let world = World::new("carry");
    let image = format!(
        "{{\"type\":\"user\",\"timestamp\":\"2026-09-28T15:18:13.903Z\",\"message\":{{\"content\":[{{\"type\":\"image\",\"source\":\"{}\"}}]}}}}\n",
        "A".repeat(300 * 1024)
    );
    let mut reader = Reader::default();
    world.write(
        S,
        &[&answer_at(
            "claude-opus-5-5",
            "2026-09-28T15:05:00.000Z",
            None,
        )],
    );
    assert!(look(&mut reader, &world, S, false, "2026-09-28T15:05:01Z").is_some());
    world.append(S, &image);
    assert_eq!(
        look(&mut reader, &world, S, false, "2026-09-28T15:18:14Z"),
        None
    );
    assert_eq!(words(&reader).0, is("Opus 5.5"), "carried past the window");
    let file = std::fs::read(world.path(S)).unwrap();
    let tail = &file[file.len() - usize::try_from(profile::TRANSCRIPT_TAIL_BYTES).unwrap()..];
    assert_eq!(
        scan(tail, ms(START), false, true).0.model,
        Said::Unsaid,
        "CONTROL: the tail alone names no model"
    );
    world.append(
        S,
        &format!(
            "{}\n",
            result_at("2026-09-28T15:19:00.000Z", "Set model to `Fable 5.1`")
        ),
    );
    world.append(S, &image);
    assert_eq!(
        look(&mut reader, &world, S, false, "2026-09-28T15:19:01Z").as_deref(),
        Some("facts model=Fable_5.1 effort=- key=-"),
        "every appended byte is read"
    );
    assert_eq!(session(&reader).appends, 2);
}

/// Another process (another pid, or the same pid with another `startedAt`)
/// starts from nothing, and its first line says so.
#[test]
fn another_process_starts_from_nothing() {
    const S: &str = "00000000-0000-4000-8000-000000000105";
    let world = World::new("process");
    let mut reader = Reader::default();
    world.write(
        S,
        &[&answer_at(
            "claude-opus-5-5",
            "2026-09-28T15:01:00.000Z",
            Some("high"),
        )],
    );
    assert!(look(&mut reader, &world, S, false, "2026-09-28T15:01:10Z").is_some());
    let later = "2026-09-28T15:02:00.000Z";
    assert_eq!(
        look_from(&mut reader, &world, S, later, false, "2026-09-28T15:02:10Z").as_deref(),
        Some("facts model=- effort=- key=-"),
        "the rows from before its start are not its own"
    );
    let process = Process {
        pid: PID + 1,
        record: None,
        args: &[],
    };
    assert_eq!(
        reader
            .look(&world.config, &process, ms("2026-09-28T15:02:20Z").unwrap())
            .journal_line,
        None,
        "nothing stated, nothing changed"
    );
    assert_eq!(reader.pid, Some(PID + 1));
}

/// The command line is asked whether it pins the model only at a `/resume`
/// of a process that chose no model: a look in the same conversation, a
/// `/clear` and a process's first conversation never ask. CONTROL: the
/// resume does.
#[test]
fn the_command_line_is_asked_only_at_a_resume() {
    const LEFT: &str = "00000000-0000-4000-8000-000000000091";
    const TAKEN: &str = "00000000-0000-4000-8000-000000000092";
    const NEW: &str = "00000000-0000-4000-8000-000000000093";
    let world = World::new("asked");
    world.write(
        TAKEN,
        &[&answer_at(
            "claude-opus-5-5",
            "2026-09-28T14:00:00.000Z",
            None,
        )],
    );
    world.write(
        LEFT,
        &[&answer_at(
            "claude-opus-5-5",
            "2026-09-28T15:01:00.000Z",
            None,
        )],
    );
    let pinning = [OsString::from("--model"), OsString::from("opus")];
    let look_in = |reader: &mut Reader, session: &str, now: &str| {
        let process = Process {
            pid: PID,
            record: Some((session, ms(START).unwrap())),
            args: &pinning,
        };
        let _ = reader.look(&world.config, &process, ms(now).unwrap());
    };
    let mut reader = Reader::default();
    look_in(&mut reader, LEFT, "2026-09-28T15:01:10Z");
    look_in(&mut reader, LEFT, "2026-09-28T15:01:20Z");
    look_in(&mut reader, NEW, "2026-09-28T15:01:30Z");
    assert_eq!(
        reader.pin_asks, 0,
        "first conversation, same conversation, /clear"
    );
    look_in(&mut reader, TAKEN, "2026-09-28T15:01:40Z");
    assert_eq!(reader.pin_asks, 1, "CONTROL: the resume asks");
}

/// A `/clear` KEEPS THE MODEL, THE LEVEL AND THE KEY (finding 4): the process
/// chose a model and set a level, then began a new conversation (the record
/// names it, and it has no transcript yet). The level was set after the last
/// look, so only a read of the conversation left on the switch finds it.
/// CONTROLS: a reader that never saw the conversation left knows nothing, and
/// a `/resume` instead of the `/clear` leaves the level unread.
#[test]
fn a_clear_keeps_the_model_the_level_and_the_key() {
    const A: &str = "00000000-0000-4000-8000-00000000000a";
    const B: &str = "00000000-0000-4000-8000-00000000000b";
    const OLD: &str = "00000000-0000-4000-8000-00000000000c";
    let choice = [
        command_at("model", "2026-09-28T15:04:17.798Z"),
        result_at(
            "2026-09-28T15:04:17.798Z",
            "Set model to `Fable 5.1` for this session only",
        ),
    ];
    let effort = result_at(
        "2026-09-28T15:04:28.429Z",
        "Set effort level to high (this session only): Deeper reasoning",
    );
    let key_on = key_row("enter", "2026-09-28T15:04:29.000Z");
    for (into, resumed) in [(B, false), (OLD, true)] {
        let world = World::new("clear");
        world.write(
            OLD,
            &[&answer_at(
                "claude-haiku-4-5",
                "2026-09-28T14:00:00.000Z",
                Some("low"),
            )],
        );
        world.write(A, &[&choice[0], &choice[1]]);
        let mut reader = Reader::default();
        assert_eq!(
            look(&mut reader, &world, A, false, "2026-09-28T15:04:20Z").as_deref(),
            Some("facts model=Fable_5.1 effort=- key=-")
        );
        world.write(A, &[&choice[0], &choice[1], &effort, &key_on]);
        look(&mut reader, &world, into, false, "2026-09-28T15:04:30Z");
        if resumed {
            assert_eq!(
                words(&reader),
                (is("Fable 5.1"), Said::Unread, Said::Is(true)),
                "CONTROL: a resume keeps the chosen model and the key, not the level"
            );
        } else {
            assert_eq!(
                words(&reader),
                (is("Fable 5.1"), is("high"), Said::Is(true)),
                "the process runs what it chose and set"
            );
            let mut fresh = Reader::default();
            look(&mut fresh, &world, B, false, "2026-09-28T15:04:30Z");
            assert_eq!(
                words(&fresh),
                (Said::Unsaid, Said::Unsaid, Said::Unsaid),
                "CONTROL: a reader that never saw the conversation left"
            );
        }
    }
}

/// AN IN-REPL `/resume` LEAVES THE MODEL AND THE LEVEL OUT UNTIL ITS
/// CONVERSATION NAMES THEM: Claude Code restores the conversation's model onto
/// a process nothing pinned (no `--model`, no choice) and writes no row to say
/// so, so neither what the process said in the conversation it left nor the
/// conversation's own older rows stand for them; the key, the process's own, is
/// carried. A row of the conversation taken up names them again. CONTROL: a
/// `/clear` into a new conversation keeps what the process last stated.
#[test]
fn a_resume_leaves_the_model_and_the_level_out_until_its_conversation_names_them() {
    const LEFT: &str = "00000000-0000-4000-8000-000000000061";
    const TAKEN: &str = "00000000-0000-4000-8000-000000000062";
    const NEW: &str = "00000000-0000-4000-8000-000000000063";
    let world = World::new("resume-left-out");
    let key_on = key_row("enter", "2026-09-28T15:00:50.000Z");
    let older = answer_at(
        "claude-fable-5-1",
        "2026-09-28T14:00:00.000Z",
        Some("medium"),
    );
    world.write(TAKEN, &[&older]);
    world.write(
        LEFT,
        &[
            &key_on,
            &answer_at("claude-opus-5-5", "2026-09-28T15:01:00.000Z", Some("high")),
        ],
    );
    let mut reader = Reader::default();
    assert_eq!(
        look(&mut reader, &world, LEFT, false, "2026-09-28T15:01:30Z").as_deref(),
        Some("facts model=Opus_5.5 effort=high key=on"),
        "CONTROL: the conversation it left"
    );
    assert_eq!(
        look(&mut reader, &world, TAKEN, false, "2026-09-28T15:02:00Z").as_deref(),
        Some("facts model=- effort=- key=on")
    );
    assert_eq!(
        words(&reader),
        (Said::Unread, Said::Unread, Said::Is(true)),
        "no word of the conversation left, nor of the one taken up's older rows; its key carried"
    );
    assert_eq!(
        look(&mut reader, &world, TAKEN, false, "2026-09-28T15:02:30Z"),
        None
    );
    world.write(
        TAKEN,
        &[
            &older,
            &answer_at(
                "claude-fable-5-1",
                "2026-09-28T15:03:00.000Z",
                Some("medium"),
            ),
        ],
    );
    assert_eq!(
        look(&mut reader, &world, TAKEN, false, "2026-09-28T15:03:30Z").as_deref(),
        Some("facts model=Fable_5.1 effort=medium key=on"),
        "a row of it names them"
    );
    assert_eq!(
        look(&mut reader, &world, NEW, false, "2026-09-28T15:04:00Z"),
        None,
        "CONTROL: a `/clear` keeps them"
    );
    assert_eq!(
        words(&reader),
        (is("Fable 5.1"), is("medium"), Said::Is(true))
    );
}

/// A PINNED PROCESS KEEPS ITS MODEL ACROSS AN IN-REPL `/resume`, NOT ITS LEVEL
/// (finding 2): a `--model`, a `/model` choice (`Set model to …`) and `/fast
/// on`'s move each pin the model, so the model the process said stands (and a
/// `--model` process that said none still has none: no flag stands for a
/// statement); the level is unread until a row names it. A choice is found
/// behind newer rows that state the model, the level and the key. CONTROLS:
/// `Kept model as …` is no choice, and a process nothing pinned shows no model.
#[test]
fn a_pinned_process_keeps_its_model_across_a_resume() {
    const LEFT: &str = "00000000-0000-4000-8000-000000000071";
    const TAKEN: &str = "00000000-0000-4000-8000-000000000072";
    let key_on = key_row("enter", "2026-09-28T15:00:50.000Z");
    let answer = |model: &str| answer_at(model, "2026-09-28T15:01:00.000Z", Some("high"));
    let chose = |command: &str, text: &str, model: &str| {
        vec![
            command_at(command, "2026-09-28T15:00:40.000Z"),
            result_at("2026-09-28T15:00:40.000Z", text),
            key_on.clone(),
            answer(model),
        ]
    };
    let cases = [
        (
            "its `--model`, said",
            true,
            vec![key_on.clone(), answer("claude-opus-5-5")],
            is("Opus 5.5"),
        ),
        (
            "its `--model`, nothing said",
            true,
            Vec::new(),
            Said::Unsaid,
        ),
        (
            "a `/model` choice",
            false,
            chose(
                "model",
                "Set model to `Fable 5.1` for this session only",
                "claude-fable-5-1",
            ),
            is("Fable 5.1"),
        ),
        (
            "a `/fast on` move",
            false,
            chose(
                "fast",
                "\u{21AF} Fast mode ON \u{00B7} model set to `Opus 5.5`",
                "claude-opus-5-5",
            ),
            is("Opus 5.5"),
        ),
        (
            "CONTROL: a choice that kept the model",
            false,
            chose("model", "Kept model as `Opus 5.5`", "claude-opus-5-5"),
            Said::Unread,
        ),
        (
            "CONTROL: nothing pinned",
            false,
            vec![key_on.clone(), answer("claude-opus-5-5")],
            Said::Unread,
        ),
    ];
    for (what, pinned, rows, kept) in cases {
        let world = World::new("resume-pinned");
        world.write(
            TAKEN,
            &[&answer_at(
                "claude-fable-5-2",
                "2026-09-28T14:00:00.000Z",
                Some("medium"),
            )],
        );
        let rows: Vec<&str> = rows.iter().map(String::as_str).collect();
        world.write(LEFT, &rows);
        let mut reader = Reader::default();
        look(&mut reader, &world, LEFT, pinned, "2026-09-28T15:01:30Z");
        for (read, now) in [
            ("the resume's look", "2026-09-28T15:02:00Z"),
            ("the next look", "2026-09-28T15:02:30Z"),
        ] {
            look(&mut reader, &world, TAKEN, pinned, now);
            assert_eq!(words(&reader).0, kept, "{what}: {read}");
            assert_eq!(words(&reader).1, Said::Unread, "{what}: {read}: no level");
        }
    }
}

/// A `/resume` NEVER CARRIES THE EFFORT (finding 3), not even for a process
/// whose model it keeps, and not from the conversation's own older rows: the
/// level is unread until a row of the process names one. CONTROL: the same
/// process's `/clear` carries the level.
#[test]
fn a_resume_never_carries_the_effort() {
    const LEFT: &str = "00000000-0000-4000-8000-000000000081";
    const TAKEN: &str = "00000000-0000-4000-8000-000000000082";
    const NEW: &str = "00000000-0000-4000-8000-000000000083";
    for (into, level) in [(TAKEN, Said::Unread), (NEW, is("xhigh"))] {
        let world = World::new("resume-effort");
        world.write(
            TAKEN,
            &[&answer_at(
                "claude-opus-5-5",
                "2026-09-28T14:00:00.000Z",
                Some("low"),
            )],
        );
        world.write(
            LEFT,
            &[&answer_at(
                "claude-opus-5-5",
                "2026-09-28T15:01:00.000Z",
                Some("xhigh"),
            )],
        );
        let mut reader = Reader::default();
        look(&mut reader, &world, LEFT, true, "2026-09-28T15:01:30Z");
        look(&mut reader, &world, into, true, "2026-09-28T15:02:00Z");
        assert_eq!(
            words(&reader),
            (is("Opus 5.5"), level, Said::Unsaid),
            "into {into}"
        );
    }
}

/// The key's state rides over newer reads that settle the model and the
/// level: an answer after `/effort <key> off` decides both, and the key stays
/// off however many such answers follow.
#[test]
fn the_key_state_rides_over_a_settled_newer_read() {
    const S: &str = "00000000-0000-4000-8000-000000001019";
    let world = World::new("key-rides");
    let key = capital_key();
    world.write(
        S,
        &[
            &answer_at("claude-opus-5-5", "2026-09-28T15:00:10.000Z", Some("xhigh")),
            &command_at("effort", "2026-09-28T15:00:20.000Z"),
            &result_at(
                "2026-09-28T15:00:20.000Z",
                &format!("{key} off. Effort stays xhigh."),
            ),
        ],
    );
    let mut reader = Reader::default();
    assert_eq!(
        look(&mut reader, &world, S, false, "2026-09-28T15:00:30Z").as_deref(),
        Some("facts model=Opus_5.5 effort=xhigh key=off")
    );
    for at in ["2026-09-28T15:01:00.000Z", "2026-09-28T15:02:00.000Z"] {
        world.append(
            S,
            &format!("{}\n", answer_at("claude-opus-5-5", at, Some("xhigh"))),
        );
        assert_eq!(
            look(&mut reader, &world, S, false, "2026-09-28T15:02:10Z"),
            None
        );
        assert_eq!(
            words(&reader).2,
            Said::Is(false),
            "rides over the settled read"
        );
    }
}

// ───────────────────────── reads split across looks, and gaps ─────────────────────────

/// A user row of `kib` KiB (a pasted image) at `at`, that states nothing.
fn image_at(at: &str, kib: usize) -> String {
    format!(
        "{{\"type\":\"user\",\"timestamp\":\"{at}\",\"message\":{{\"content\":[{{\"type\":\"image\",\"source\":\"{}\"}}]}}}}\n",
        "A".repeat(kib * 1024)
    )
}

/// `rows`, each with its row end.
fn rows(rows: &[&str]) -> String {
    body(rows)
}

/// A COMMAND ROW IS READ WITH THE RESULT UNDER IT: a look that falls between an
/// `/effort` or `/model` command row and its result reads neither, and the next
/// look reads both, so the split reads say what one read of the same rows says
/// (the key stays off; a model kept or set again keeps the level). CONTROL: a
/// command with another row under it is read as unanswered, its facts unread.
#[test]
fn a_command_is_read_with_the_result_under_it() {
    const S: &str = "00000000-0000-4000-8000-0000000000d1";
    let world = World::new("split");
    let key = capital_key();
    world.write(
        S,
        &[
            &answer_at("claude-opus-5-5", "2026-09-28T15:00:10.000Z", Some("high")),
            &command_at("effort", "2026-09-28T15:00:20.000Z"),
            &result_at(
                "2026-09-28T15:00:20.000Z",
                &format!("{key} off. Effort stays high."),
            ),
        ],
    );
    let mut reader = Reader::default();
    assert_eq!(
        look(&mut reader, &world, S, false, "2026-09-28T15:00:25Z").as_deref(),
        Some("facts model=Opus_5.5 effort=high key=off")
    );
    let fresh_line = |now: &str| {
        let mut fresh = Reader::default();
        look(&mut fresh, &world, S, false, now)
    };
    let split = |reader: &mut Reader, command: &str, at: &str, result: &str, now: [&str; 2]| {
        world.append(S, &rows(&[&command_at(command, at)]));
        let between = look(reader, &world, S, false, now[0]);
        let held = words(reader);
        world.append(S, &rows(&[&result_at(at, result)]));
        let after = look(reader, &world, S, false, now[1]);
        (between, held, after)
    };
    let (between, held, after) = split(
        &mut reader,
        "effort",
        "2026-09-28T15:00:30.000Z",
        "Set effort level to low (this session only): Quick",
        ["2026-09-28T15:00:31Z", "2026-09-28T15:00:32Z"],
    );
    assert_eq!(between, None, "a command alone waits for its result");
    assert_eq!(held, (is("Opus 5.5"), is("high"), Said::Is(false)));
    assert_eq!(
        after.as_deref(),
        Some("facts model=Opus_5.5 effort=low key=off")
    );
    assert_eq!(
        fresh_line("2026-09-28T15:00:33Z").as_deref(),
        after.as_deref(),
        "the split reads say what one read says"
    );
    for (at, result, now) in [
        (
            "2026-09-28T15:00:40.000Z",
            "Kept model as `Opus 5.5`",
            ["2026-09-28T15:00:41Z", "2026-09-28T15:00:42Z"],
        ),
        (
            "2026-09-28T15:00:50.000Z",
            "Set model to `Opus 5.5` for this session only",
            ["2026-09-28T15:00:51Z", "2026-09-28T15:00:52Z"],
        ),
    ] {
        let (between, held, after) = split(&mut reader, "model", at, result, now);
        assert_eq!((between, after), (None, None), "{result}");
        assert_eq!(
            held,
            (is("Opus 5.5"), is("low"), Said::Is(false)),
            "{result}"
        );
        assert_eq!(
            words(&reader),
            (is("Opus 5.5"), is("low"), Said::Is(false)),
            "{result}: the same model keeps the level"
        );
    }
    assert_eq!(
        fresh_line("2026-09-28T15:00:53Z").as_deref(),
        Some("facts model=Opus_5.5 effort=low key=off")
    );
    // CONTROL: once another row is under it, the command is read unanswered.
    world.append(
        S,
        &rows(&[
            &command_at("effort", "2026-09-28T15:01:00.000Z"),
            r#"{"type":"system","subtype":"turn_duration","isSidechain":false,"timestamp":"2026-09-28T15:01:01.000Z","durationMs":5}"#,
        ]),
    );
    assert_eq!(
        look(&mut reader, &world, S, false, "2026-09-28T15:01:02Z").as_deref(),
        Some("facts model=Opus_5.5 effort=- key=-")
    );
    assert_eq!(
        fresh_line("2026-09-28T15:01:03Z").as_deref(),
        Some("facts model=Opus_5.5 effort=- key=-")
    );
}

/// A GROWTH PAST THE APPEND BOUND LEAVES WHAT IT SKIPPED UNREAD: more than
/// [`MAX_APPEND_BYTES`] between two looks is read as a fresh tail, and a fact
/// that tail does not state is unread (`-`), never the value a skipped row
/// overrode. CONTROLS: the same rows under the bound are read as an append and
/// every statement counts; a fact the tail does state is shown.
#[test]
fn a_growth_past_the_append_bound_leaves_what_it_skipped_unread() {
    const S: &str = "00000000-0000-4000-8000-0000000000d2";
    let key = capital_key();
    let over_kib = usize::try_from(MAX_APPEND_BYTES / 1024).unwrap() + 200;
    let started = |world: &World| {
        world.write(
            S,
            &[
                &answer_at("claude-opus-5-5", "2026-09-28T15:00:10.000Z", Some("high")),
                &key_row("enter", "2026-09-28T15:00:20.000Z"),
            ],
        );
        let mut reader = Reader::default();
        assert_eq!(
            look(&mut reader, world, S, false, "2026-09-28T15:00:25Z").as_deref(),
            Some("facts model=Opus_5.5 effort=high key=on")
        );
        reader
    };
    let choices = rows(&[
        &command_at("effort", "2026-09-28T15:00:30.000Z"),
        &result_at(
            "2026-09-28T15:00:30.000Z",
            &format!("{key} off. Effort stays high."),
        ),
        &command_at("model", "2026-09-28T15:00:31.000Z"),
        &result_at(
            "2026-09-28T15:00:31.000Z",
            "Set model to `Sonnet 5.5` for this session only",
        ),
    ]);
    for (kib, line, appends) in [
        (over_kib, Some("facts model=- effort=- key=-"), 0),
        (300, Some("facts model=Sonnet_5.5 effort=- key=off"), 1),
    ] {
        let world = World::new("bound");
        let mut reader = started(&world);
        world.append(S, &choices);
        world.append(S, &image_at("2026-09-28T15:00:32.000Z", kib));
        assert_eq!(
            look(&mut reader, &world, S, false, "2026-09-28T15:00:40Z").as_deref(),
            line,
            "{kib} KiB"
        );
        assert_eq!(
            (session(&reader).reads, session(&reader).appends),
            (2, appends),
            "{kib} KiB: an append only under the bound"
        );
    }
    // The key's exit, a growth past the bound and an answer: the tail names
    // the model and the level, and the key it does not state is unread.
    let world = World::new("bound-answer");
    let mut reader = started(&world);
    world.append(S, &rows(&[&key_row("exit", "2026-09-28T15:00:30.000Z")]));
    world.append(S, &image_at("2026-09-28T15:00:31.000Z", over_kib));
    world.append(
        S,
        &rows(&[&answer_at(
            "claude-opus-5-5",
            "2026-09-28T15:00:40.000Z",
            Some("high"),
        )]),
    );
    assert_eq!(
        look(&mut reader, &world, S, false, "2026-09-28T15:00:45Z").as_deref(),
        Some("facts model=Opus_5.5 effort=high key=-")
    );
    assert_eq!(words(&reader).2, Said::Unread);
}

/// A CHANGED BYTE BEFORE THE END OF THE LAST READ MEANS A FRESH READ: the
/// guard catches a transcript rewritten in place (the same inode, grown), so
/// its new rows are read, not only what follows the old end. CONTROL: an
/// append over unchanged bytes is read as an append.
#[test]
fn a_changed_byte_before_the_read_end_is_read_afresh() {
    const S: &str = "00000000-0000-4000-8000-0000000000d3";
    let world = World::new("guard");
    let row = |model: &str| answer_at(model, "2026-09-28T15:01:00.000Z", Some("high"));
    let filler = r#"{"type":"user","timestamp":"2026-09-28T15:01:05.000Z","message":{"content":"plain words"}}"#;
    let mut reader = Reader::default();
    world.write(S, &[&row("claude-opus-5")]);
    let inode = |world: &World| {
        use std::os::unix::fs::MetadataExt as _;
        std::fs::metadata(world.path(S)).unwrap().ino()
    };
    let before = inode(&world);
    assert_eq!(
        look(&mut reader, &world, S, false, "2026-09-28T15:01:10Z").as_deref(),
        Some("facts model=Opus_5 effort=high key=-")
    );
    world.write(S, &[&row("claude-opus-7"), filler]);
    assert_eq!(inode(&world), before, "rewritten in place");
    assert_eq!(
        look(&mut reader, &world, S, false, "2026-09-28T15:01:11Z").as_deref(),
        Some("facts model=Opus_7 effort=high key=-")
    );
    assert_eq!(
        (session(&reader).reads, session(&reader).appends),
        (2, 0),
        "the changed bytes are read afresh"
    );
    world.append(S, &rows(&[filler]));
    assert_eq!(
        look(&mut reader, &world, S, false, "2026-09-28T15:01:12Z"),
        None
    );
    assert_eq!(
        (session(&reader).reads, session(&reader).appends),
        (3, 1),
        "CONTROL: unchanged bytes, an append"
    );
}

/// A `/resume` IS TOLD FROM A `/clear` BY ANY ROW FROM BEFORE THE SWITCH, not
/// only by a row whose words can matter: a conversation taken up whose window
/// holds only queue rows from before the switch is a resume (the key carried,
/// model and level unread). One whose window holds no whole row cannot be
/// told, and each fact its window does not state is unread, the key included;
/// never the conversation left's level or model. CONTROLS: the conversation
/// taken up with its older answer in the window, and a `/clear`.
#[test]
fn a_resume_is_told_by_any_row_from_before_the_switch() {
    const LEFT: &str = "00000000-0000-4000-8000-0000000000d4";
    const TAKEN: &str = "00000000-0000-4000-8000-0000000000d5";
    const NEW: &str = "00000000-0000-4000-8000-0000000000d6";
    let older = answer_at("claude-fable-5-1", "2026-09-28T14:00:00.000Z", Some("low"));
    let queue = |n: usize| -> String {
        (0..n)
            .map(|i| {
                format!(
                    "{{\"type\":\"queue-operation\",\"operation\":\"enqueue\",\"timestamp\":\"2026-09-28T14:00:01.000Z\",\"sessionId\":\"{TAKEN}\",\"content\":\"{i:04}{}\"}}\n",
                    "x".repeat(520)
                )
            })
            .collect()
    };
    let cases: [(&str, &str, String, &str); 4] = [
        (
            "queue rows",
            TAKEN,
            queue(600),
            "facts model=- effort=- key=on",
        ),
        (
            "one row past the window",
            TAKEN,
            image_at("2026-09-28T14:00:02.000Z", 300),
            "facts model=- effort=- key=-",
        ),
        (
            "CONTROL: its older answer in the window",
            TAKEN,
            String::new(),
            "facts model=- effort=- key=on",
        ),
        (
            "CONTROL: a /clear",
            NEW,
            String::new(),
            "facts model=Opus_5.5 effort=high key=on",
        ),
    ];
    for (what, into, after, line) in cases {
        let world = World::new("resume-any-row");
        world.write(TAKEN, &[&older]);
        world.append(TAKEN, &after);
        world.write(
            LEFT,
            &[
                &key_row("enter", "2026-09-28T15:00:50.000Z"),
                &answer_at("claude-opus-5-5", "2026-09-28T15:01:00.000Z", Some("high")),
            ],
        );
        let mut reader = Reader::default();
        look(&mut reader, &world, LEFT, false, "2026-09-28T15:01:30Z");
        look(&mut reader, &world, into, false, "2026-09-28T15:02:00Z");
        assert_eq!(reader.facts().journal_line(), line, "{what}");
    }
}

/// THE FLOOR OF A CONVERSATION SWITCHED INTO IS WHEN THE PROCESS WAS LAST SEEN
/// IN THE ONE IT LEFT, not its start: a `/resume` back into a conversation this
/// process wrote in before is still a resume, so it carries no level.
/// CONTROL: the same process's `/clear` into a new conversation keeps it.
#[test]
fn a_resume_back_into_a_conversation_the_process_wrote_in_is_a_resume() {
    const A: &str = "00000000-0000-4000-8000-0000000000d7";
    const B: &str = "00000000-0000-4000-8000-0000000000d8";
    const C: &str = "00000000-0000-4000-8000-0000000000d9";
    for (back, line) in [
        (A, "facts model=- effort=- key=-"),
        (C, "facts model=Opus_5.5 effort=low key=-"),
    ] {
        let world = World::new("resume-back");
        world.write(
            A,
            &[&answer_at(
                "claude-opus-5-5",
                "2026-09-28T15:01:00.000Z",
                Some("high"),
            )],
        );
        let mut reader = Reader::default();
        assert_eq!(
            look(&mut reader, &world, A, false, "2026-09-28T15:01:10Z").as_deref(),
            Some("facts model=Opus_5.5 effort=high key=-")
        );
        world.write(
            B,
            &[&answer_at(
                "claude-opus-5-5",
                "2026-09-28T15:02:00.000Z",
                Some("low"),
            )],
        );
        assert_eq!(
            look(&mut reader, &world, B, false, "2026-09-28T15:02:10Z").as_deref(),
            Some("facts model=Opus_5.5 effort=low key=-")
        );
        look(&mut reader, &world, back, false, "2026-09-28T15:03:10Z");
        assert_eq!(reader.facts().journal_line(), line, "back into {back}");
    }
}

/// The reader as an own-image move carries it: written into a capsule's
/// payload and read back by the new image.
fn moved(reader: &Reader) -> Reader {
    let mut encoder = crate::transfer_capsule::Encoder::new();
    reader.put(&mut encoder).expect("put");
    let bytes = encoder.finish();
    let mut decoder = crate::transfer_capsule::Decoder::new(&bytes).expect("payload");
    let moved = Reader::get(&mut decoder).expect("get");
    decoder.finish().expect("the whole reader");
    let mut again = crate::transfer_capsule::Encoder::new();
    moved.put(&mut again).expect("put again");
    assert_eq!(again.finish(), bytes, "it crosses as itself");
    moved
}

/// AN OWN-IMAGE MOVE CARRIES THE READER: the child starts in X (Sonnet 5.5,
/// high, the key entered), `/clear`s into Y (the key off, `/model` Opus 5.5,
/// `/effort low`) and `/resume`s X. The new image says what the old one says,
/// reads nothing again, and goes on with an append where the old one stopped.
/// CONTROL: a reader started afresh in the new image takes X's rows from
/// before the `/resume` for the child's, and names all three wrongly, the key
/// for good.
#[test]
fn an_own_image_move_carries_the_reader() {
    const X: &str = "00000000-0000-4000-8000-0000000000e1";
    const Y: &str = "00000000-0000-4000-8000-0000000000e2";
    let world = World::new("move");
    let key = capital_key();
    world.write(
        X,
        &[
            &answer_at(
                "claude-sonnet-5-5",
                "2026-09-28T15:00:10.000Z",
                Some("high"),
            ),
            &key_row("enter", "2026-09-28T15:00:20.000Z"),
            &answer_at(
                "claude-sonnet-5-5",
                "2026-09-28T15:00:30.000Z",
                Some("high"),
            ),
        ],
    );
    let mut old = Reader::default();
    assert_eq!(
        look(&mut old, &world, X, false, "2026-09-28T15:00:40Z").as_deref(),
        Some("facts model=Sonnet_5.5 effort=high key=on")
    );
    world.write(
        Y,
        &[
            &command_at("effort", "2026-09-28T15:01:10.000Z"),
            &result_at(
                "2026-09-28T15:01:10.000Z",
                &format!("{key} off. Effort stays high."),
            ),
            &command_at("model", "2026-09-28T15:01:20.000Z"),
            &result_at(
                "2026-09-28T15:01:20.000Z",
                "Set model to `Opus 5.5` for this session only",
            ),
            &command_at("effort", "2026-09-28T15:01:30.000Z"),
            &result_at(
                "2026-09-28T15:01:30.000Z",
                "Set effort level to low (this session only): Quick",
            ),
        ],
    );
    assert_eq!(
        look(&mut old, &world, Y, false, "2026-09-28T15:01:40Z").as_deref(),
        Some("facts model=Opus_5.5 effort=low key=off")
    );
    assert_eq!(
        look(&mut old, &world, X, false, "2026-09-28T15:02:00Z").as_deref(),
        Some("facts model=Opus_5.5 effort=- key=off")
    );
    let mut new = moved(&old);
    assert_eq!(new.facts(), old.facts());
    assert_eq!(
        look(&mut new, &world, X, false, "2026-09-28T15:02:30Z"),
        None
    );
    assert_eq!(
        session(&new).reads,
        0,
        "an unchanged transcript is not read"
    );
    let mut afresh = Reader::default();
    assert_eq!(
        look(&mut afresh, &world, X, false, "2026-09-28T15:02:30Z").as_deref(),
        Some("facts model=Sonnet_5.5 effort=high key=on"),
        "CONTROL: a reader started afresh"
    );
    world.append(
        X,
        &rows(&[&answer_at(
            "claude-opus-5-5",
            "2026-09-28T15:03:00.000Z",
            Some("low"),
        )]),
    );
    let next = "2026-09-28T15:03:10Z";
    assert_eq!(
        look(&mut new, &world, X, false, next).as_deref(),
        Some("facts model=Opus_5.5 effort=low key=off")
    );
    assert_eq!(
        (session(&new).reads, session(&new).appends),
        (1, 1),
        "an append from where the old image stopped"
    );
    assert_eq!(
        look(&mut old, &world, X, false, next).as_deref(),
        Some("facts model=Opus_5.5 effort=low key=off")
    );
    assert_eq!(
        look(&mut afresh, &world, X, false, next).as_deref(),
        Some("facts model=Opus_5.5 effort=low key=on"),
        "CONTROL: the afresh reader's key stays wrong"
    );
}

/// A carried reader restores only what [`Reader::put`] could have written: a
/// truncated reader, a state no fact has, a conversation id no transcript has
/// and a guard longer than the rows read are refused. CONTROL: the smallest
/// reader restores.
#[test]
fn a_carried_reader_restores_only_what_put_writes() {
    use crate::transfer_capsule::{Decoder, Encoder};
    let read = |bytes: &[u8]| {
        let mut decoder = Decoder::new(bytes).unwrap();
        Reader::get(&mut decoder).and_then(|reader| decoder.finish().map(|()| reader))
    };
    let facts = |model: u8, key: u8| -> Vec<u8> {
        let mut encoder = Encoder::new();
        for byte in [model, 0, key, 0, 0] {
            encoder.u8(byte).unwrap();
        }
        encoder.finish()
    };
    // No pid, no floor, no conversation.
    let none = [0_u8, 0, 0];
    let with = |head: &[u8], tail: &[u8]| [head, tail].concat();
    let smallest = read(&with(&none, &facts(0, 0))).expect("CONTROL: the smallest reader");
    assert_eq!(smallest.facts(), &ModelFacts::default());
    assert_eq!(smallest.line, ModelFacts::default().journal_line());
    let whole = with(&none, &facts(1, 3));
    assert_eq!(
        read(&whole).unwrap().facts().journal_line(),
        "facts model=- effort=- key=on"
    );
    for cut in 0..whole.len() {
        assert!(read(&whole[..cut]).is_err(), "cut at {cut}");
    }
    assert!(read(&with(&none, &facts(3, 0))).is_err(), "no fourth state");
    assert!(
        read(&with(&none, &facts(0, 4))).is_err(),
        "no fifth key state"
    );
    let session = |id: &str, read_to: u64, guard: &[u8]| -> Vec<u8> {
        let mut encoder = Encoder::new();
        for flag in [false, false, true] {
            encoder.boolean(flag).unwrap();
        }
        encoder.bytes(id.as_bytes()).unwrap();
        encoder.u64(1).unwrap();
        encoder.u64(2).unwrap();
        encoder.boolean(false).unwrap();
        encoder.boolean(true).unwrap();
        for value in [3, 4, read_to] {
            encoder.u64(value).unwrap();
        }
        encoder.bytes(guard).unwrap();
        [encoder.finish(), facts(0, 0)].concat()
    };
    const ID: &str = "00000000-0000-4000-8000-0000000000e3";
    assert!(read(&session(ID, 8, b"row end\n")).is_ok(), "CONTROL");
    assert!(read(&session("../escape", 8, b"row end\n")).is_err());
    assert!(read(&session(ID, 4, b"row end\n")).is_err());
    assert!(read(&session(ID, 900, &[b'x'; GUARD_BYTES + 1])).is_err());
}
