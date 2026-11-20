// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Fable's limit: which transcript rows are one, what stands, what is
//! decided, and when the session goes back. Every test has a control that
//! shows the same code saying otherwise.

use std::io::Write as _;
use std::path::PathBuf;

use super::*;
use crate::cli;

/// The time of the limit row read off Claude Code 2.1.323.
const ROW_STAMP: &str = "2026-11-13T06:39:26.429Z";
/// [`ROW_STAMP`] as epoch milliseconds.
const ROW_MS: u64 = 1_794_551_966_429;
/// The reset that row names (2026-11-15 17:00 UTC).
const RESETS_S: u64 = 1_794_762_000;

/// A refused request's row in the shape Claude Code 2.1.323 wrote it, with
/// the fields a test varies. The ids are placeholders; every key the real row
/// carries beside the ones read is here, so the reader is shown to pass them.
fn limit_row(stamp: &str, bucket: &str, status: &str, sidechain: bool) -> String {
    format!(
        "{{\"parentUuid\":\"5e5e0483-0000-4000-8000-000000000001\",\"isSidechain\":{sidechain},\
         \"type\":\"assistant\",\"uuid\":\"597479d3-0000-4000-8000-000000000002\",\
         \"timestamp\":\"{stamp}\",\"message\":{{\"diagnostics\":null,\"id\":\"328c5ebe\",\
         \"container\":null,\"model\":\"<synthetic>\",\"role\":\"assistant\",\
         \"stop_details\":null,\"stop_reason\":\"stop_sequence\",\"stop_sequence\":\"\",\
         \"type\":\"message\",\"usage\":{{\"input_tokens\":0,\"output_tokens\":0,\
         \"server_tool_use\":{{\"web_search_requests\":0}},\"service_tier\":null}},\
         \"content\":[{{\"type\":\"text\",\"text\":\"You've hit your Fable limit \u{b7} resets \
         Nov 15 at 9am (America/Los_Angeles)\"}}],\"context_management\":null}},\
         \"requestId\":\"req_0\",\"apiError\":\"usage_limit_reached\",\
         \"apiErrorParams\":{{\"rate_limit_info\":{{\"status\":\"{status}\",\
         \"resetsAt\":{RESETS_S},\"rateLimitType\":\"{bucket}\"}}}},\
         \"quotaLimits\":{{\"status\":\"{status}\",\"resetsAt\":{RESETS_S},\
         \"unifiedRateLimitFallbackAvailable\":false,\"rateLimitType\":\"{bucket}\",\
         \"overageStatus\":\"rejected\",\"overageDisabledReason\":\"org_level_disabled\",\
         \"upgradePaths\":[\"overage\"],\"isUsingOverage\":false}},\"error\":\"rate_limit\",\
         \"isApiErrorMessage\":true,\"apiErrorStatus\":429,\"perTurnEffort\":\"high\",\
         \"failedResponseHadNoToolCall\":true,\"userType\":\"external\",\"entrypoint\":\"cli\",\
         \"cwd\":\"/w\",\"sessionId\":\"s\",\"version\":\"2.1.323\",\"gitBranch\":\"main\"}}"
    )
}

/// The row of Fable's own limit, on the conversation's thread.
fn fable_row(stamp: &str) -> String {
    limit_row(stamp, FABLE_BUCKET, "rejected", false)
}

/// A real answer's row: an `assistant` row that names the model that answered.
fn answer_row(stamp: &str, model: &str, sidechain: bool) -> String {
    format!(
        "{{\"type\":\"assistant\",\"isSidechain\":{sidechain},\"timestamp\":\"{stamp}\",\
         \"message\":{{\"model\":\"{model}\",\"role\":\"assistant\",\"content\":[]}}}}"
    )
}

/// A local command's result row, as one run between turns is recorded.
fn stdout_row(stamp: &str, text: &str) -> String {
    format!(
        "{{\"type\":\"user\",\"isSidechain\":false,\"timestamp\":\"{stamp}\",\
         \"message\":{{\"role\":\"user\",\"content\":\
         \"<local-command-stdout>{text}</local-command-stdout>\"}}}}"
    )
}

/// The row Claude Code writes as it sends a request (2.1.321), from the
/// query `source`, on `model`.
fn request_row(stamp: &str, source: &str, model: &str, sidechain: bool) -> String {
    format!(
        "{{\"type\":\"api-request\",\"isSidechain\":{sidechain},\"timestamp\":\"{stamp}\",\
         \"querySource\":\"{source}\",\"params\":{{\"model\":\"{model}\",\
         \"output_config\":{{\"effort\":\"high\"}}}}}}"
    )
}

/// What `rows` say, read oldest first from `floor_ms`.
fn standing(rows: &[String], floor_ms: u64) -> Sight {
    let mut standing = Standing::default();
    for row in rows {
        standing.line(row.as_bytes(), floor_ms);
    }
    standing.sight()
}

/// The wall the real row states.
fn real_wall() -> Wall {
    Wall {
        at_ms: ROW_MS,
        resets_at_s: Some(RESETS_S),
        bucket: Some(FABLE_BUCKET.to_string()),
    }
}

#[test]
fn the_row_claude_code_writes_at_fables_limit_is_read_as_fables_wall() {
    let sight = standing(&[fable_row(ROW_STAMP)], 0);
    assert_eq!(sight.fable(), Some(&real_wall()));
    assert!(!sight.answered, "a refusal is no answer");
    // The floor is inclusive: the row of the floor's own millisecond counts.
    assert_eq!(
        standing(&[fable_row(ROW_STAMP)], ROW_MS).fable(),
        Some(&real_wall())
    );
    // CONTROLS, each the same row with one thing changed.
    // A subagent's refusal speaks for no session.
    let side = limit_row(ROW_STAMP, FABLE_BUCKET, "rejected", true);
    assert_eq!(standing(&[side], 0), Sight::default());
    // A row from before the floor is spent.
    assert_eq!(
        standing(&[fable_row(ROW_STAMP)], ROW_MS + 1),
        Sight::default()
    );
    // Another window's limit stands, and is not Fable's.
    for bucket in ["seven_day", "five_hour", "seven_day_opus"] {
        let sight = standing(&[limit_row(ROW_STAMP, bucket, "rejected", false)], 0);
        assert_eq!(
            sight.wall.as_ref().and_then(|wall| wall.bucket.as_deref()),
            Some(bucket)
        );
        assert_eq!(sight.fable(), None, "{bucket}");
    }
    // A quota that was not rejected names no window: a limit, not Fable's.
    let allowed = standing(&[limit_row(ROW_STAMP, FABLE_BUCKET, "allowed", false)], 0);
    assert_eq!(
        allowed.wall,
        Some(Wall {
            at_ms: ROW_MS,
            resets_at_s: None,
            bucket: None
        })
    );
    assert_eq!(allowed.fable(), None);
    // The same row without the error marks is no limit at all, and its
    // synthetic model makes it no answer either.
    let plain = fable_row(ROW_STAMP).replace("\"isApiErrorMessage\":true,", "");
    assert_eq!(standing(&[plain], 0), Sight::default());
    let other =
        fable_row(ROW_STAMP).replace("\"error\":\"rate_limit\"", "\"error\":\"overloaded\"");
    assert_eq!(standing(&[other], 0), Sight::default());
    // A row cut short, a row with no time and bytes that are no text are passed.
    let whole = fable_row(ROW_STAMP);
    let cut = whole[..whole.len() - 9].to_string();
    let untimed = whole.replace(ROW_STAMP, "yesterday");
    assert_eq!(standing(&[cut, untimed], 0), Sight::default());
    let mut standing = Standing::default();
    standing.line(b"\xff\"assistant\"", 0);
    assert_eq!(standing.sight(), Sight::default());
}

#[test]
fn text_that_only_looks_like_the_notice_is_no_limit() {
    let stamp = "2026-11-13T06:40:00.000Z";
    let notice = "You've hit your Fable limit \u{b7} resets Nov 15 at 9am (America/Los_Angeles)";
    // The whole limit row, as a conversation quotes it: JSON inside a string.
    let quoted = fable_row(ROW_STAMP)
        .replace('\\', "\\\\")
        .replace('"', "\\\"");
    let rows = [
        // A person pasted the notice, or the row itself.
        format!(
            "{{\"type\":\"user\",\"isSidechain\":false,\"timestamp\":\"{stamp}\",\
             \"message\":{{\"role\":\"user\",\"content\":\"{notice}\"}}}}"
        ),
        format!(
            "{{\"type\":\"user\",\"isSidechain\":false,\"timestamp\":\"{stamp}\",\
             \"message\":{{\"role\":\"user\",\"content\":\"{quoted}\"}}}}"
        ),
        // A tool read a transcript and its result holds the row.
        format!(
            "{{\"type\":\"user\",\"isSidechain\":false,\"timestamp\":\"{stamp}\",\
             \"message\":{{\"role\":\"user\",\"content\":[{{\"type\":\"tool_result\",\
             \"tool_use_id\":\"t\",\"content\":\"{quoted}\"}}]}},\
             \"toolUseResult\":{{\"stdout\":\"{quoted}\",\"isApiErrorMessage\":true,\
             \"error\":\"rate_limit\"}}}}"
        ),
        // The marks of a refusal on a row that is not the assistant's.
        fable_row(stamp).replace("\"type\":\"assistant\"", "\"type\":\"user\""),
        // The marks one level down, in the message, not on the row.
        format!(
            "{{\"type\":\"assistant\",\"isSidechain\":false,\"timestamp\":\"{stamp}\",\
             \"message\":{{\"model\":\"<synthetic>\",\"isApiErrorMessage\":true,\
             \"error\":\"rate_limit\",\"quotaLimits\":{{\"status\":\"rejected\",\
             \"resetsAt\":{RESETS_S},\"rateLimitType\":\"{FABLE_BUCKET}\"}},\
             \"content\":[{{\"type\":\"text\",\"text\":\"{notice}\"}}]}}}}"
        ),
    ];
    for row in &rows {
        assert!(
            crate::json::parse(row).is_some(),
            "the fixture is a whole row: {row}"
        );
        // No limit and no answer; a pasted prompt may date typed input.
        let sight = standing(std::slice::from_ref(row), 0);
        assert_eq!((sight.wall, sight.answered), (None, false), "{row}");
    }
    // The model saying the words in a real answer is an answer, not a limit.
    let said = format!(
        "{{\"type\":\"assistant\",\"isSidechain\":false,\"timestamp\":\"{stamp}\",\
         \"message\":{{\"model\":\"claude-fable-5-2\",\"role\":\"assistant\",\
         \"content\":[{{\"type\":\"text\",\"text\":\"{notice}\"}}]}}}}"
    );
    assert_eq!(
        standing(&[said], 0),
        Sight {
            wall: None,
            answered: true,
            typed_ms: None,
            chose: None
        }
    );
    // CONTROL: the row itself, at the same time, is Fable's limit.
    assert!(standing(&[fable_row(stamp)], 0).fable().is_some());
}

#[test]
fn an_answer_a_model_choice_and_a_login_lift_the_wall_and_nothing_else_does() {
    let wall = fable_row(ROW_STAMP);
    let later = "2026-11-13T06:40:00.000Z";
    // A real answer: nothing stands, and the child has answered.
    let sight = standing(
        &[wall.clone(), answer_row(later, "claude-fable-5-2", false)],
        0,
    );
    assert_eq!(
        sight,
        Sight {
            wall: None,
            answered: true,
            typed_ms: None,
            chose: None
        }
    );
    // `/model` chose a model; `/fast on` moved it (its middle dot raw, and
    // escaped as JSON may write it); `/login` changed the account.
    let fast_on = "\u{26a1} Fast mode ON \u{b7} model set to `Opus 5.5`";
    let fast_on_escaped = "\u{26a1} Fast mode ON \\u00b7 model set to `Opus 5.5`";
    // Each move is a choice the rows name no word for; a login is none.
    let later_ms = ROW_MS + 33_571;
    let moved = Some(Choice {
        at_ms: later_ms,
        word: None,
    });
    for (text, chose) in [
        ("Set model to Opus 5.5", moved.clone()),
        (fast_on, moved.clone()),
        (fast_on_escaped, moved.clone()),
        ("Login successful", None),
    ] {
        let sight = standing(&[wall.clone(), stdout_row(later, text)], 0);
        assert_eq!(
            sight,
            Sight {
                wall: None,
                answered: false,
                typed_ms: None,
                chose
            },
            "{text}"
        );
    }
    // The conversation's own thread sends a request on another model: the
    // session runs on it now, however it got there.
    for model in ["claude-opus-5-5", "claude-sonnet-5"] {
        let sight = standing(
            &[
                wall.clone(),
                request_row(later, model_facts::MAIN_THREAD_QUERY, model, false),
            ],
            0,
        );
        assert_eq!(sight.wall, None, "{model}");
        assert!(!sight.answered, "a request is no answer: {model}");
    }
    // The newest limit row is the one that stands.
    let again = "2026-11-13T08:00:00.000Z";
    let sight = standing(
        &[
            wall.clone(),
            answer_row(later, "claude-fable-5-2", false),
            limit_row(again, "five_hour", "rejected", false),
        ],
        0,
    );
    assert_eq!(
        sight.wall.as_ref().map(|wall| wall.bucket.as_deref()),
        Some(Some("five_hour"))
    );
    assert!(sight.answered && sight.fable().is_none());
    // CONTROLS: what follows the wall in the real transcript, and near misses.
    let stands = [
        // The vendor's own dialog result, 1.4 s after the row.
        stdout_row(later, "Claude Code will continue automatically at 9am"),
        // `/model` that kept the model.
        stdout_row(later, "Kept model as Fable 5.2"),
        // `/fast` that moved nothing, and the words of the result too far
        // into the line to be one.
        stdout_row(later, "\u{26a1} Fast mode OFF"),
        stdout_row(later, "Note: Fast mode ON \u{b7} model set to `Opus 5.5`"),
        // A request on Fable, a side query's request on another model, a
        // subagent's, and one that names no model word.
        request_row(
            later,
            model_facts::MAIN_THREAD_QUERY,
            "claude-fable-5-2",
            false,
        ),
        request_row(later, model_facts::MAIN_THREAD_QUERY, "fable[1m]", false),
        request_row(later, "compact", "claude-haiku-5", false),
        request_row(
            later,
            model_facts::MAIN_THREAD_QUERY,
            "claude-opus-5-5",
            true,
        ),
        request_row(later, model_facts::MAIN_THREAD_QUERY, "two words", false),
        // A subagent answered.
        answer_row(later, "claude-haiku-5", true),
        // A synthetic row is not an answer.
        answer_row(later, "<synthetic>", false),
        // The words in a person's own message, not a command's result.
        format!(
            "{{\"type\":\"user\",\"isSidechain\":false,\"timestamp\":\"{later}\",\
             \"message\":{{\"role\":\"user\",\"content\":\"Set model to Opus 5.5\"}}}}"
        ),
        // A turn's end.
        format!(
            "{{\"type\":\"system\",\"subtype\":\"turn_duration\",\"isSidechain\":false,\
             \"timestamp\":\"{later}\",\"durationMs\":5}}"
        ),
        // An answer stamped before the floor.
        answer_row("2020-01-01T00:00:00.000Z", "claude-fable-5-2", false),
    ];
    for row in stands {
        let sight = standing(&[wall.clone(), row.clone()], ROW_MS);
        assert_eq!(sight.fable(), Some(&real_wall()), "{row}");
        assert!(!sight.answered, "{row}");
    }
}

/// A `user` row on the conversation's thread whose `message.content` is
/// `content`, a JSON value written as it stands in the row.
fn user_row(stamp: &str, content: &str, extra: &str) -> String {
    format!(
        "{{\"type\":\"user\",\"isSidechain\":false,{extra}\"timestamp\":\"{stamp}\",\
         \"message\":{{\"role\":\"user\",\"content\":{content}}}}}"
    )
}

#[test]
fn a_prompt_or_a_command_a_person_sent_dates_the_input_it_took() {
    let one = "2026-11-13T06:39:27.429Z";
    let two = "2026-11-13T06:39:28.429Z";
    let before = "2026-11-13T06:39:25.429Z";
    let prompt = user_row(one, "\"fix the build\"", "");
    let command = user_row(
        two,
        "\"<command-name>/model</command-name>\\n            \
         <command-message>model</command-message>\\n            \
         <command-args>opus</command-args>\"",
        "",
    );
    let message_first = user_row(
        two,
        "\"<command-message>model</command-message>\\n<command-name>/model</command-name>\"",
        "",
    );
    let shell = user_row(two, "\"<bash-input>ls</bash-input>\"", "");
    // A command typed while a turn ran: written when the turn ends, stamped
    // with the moment it was typed (2.1.322).
    let mid_turn = format!(
        "{{\"isSidechain\":false,\"type\":\"system\",\"subtype\":\"local_command\",\
         \"content\":\"<command-name>/effort</command-name>\",\"level\":\"info\",\
         \"timestamp\":\"{before}\",\"isMeta\":false}}"
    );
    assert_eq!(
        standing(std::slice::from_ref(&prompt), 0).typed_ms,
        Some(ROW_MS + 1_000)
    );
    for row in [&command, &message_first, &shell] {
        assert_eq!(
            standing(std::slice::from_ref(row), 0).typed_ms,
            Some(ROW_MS + 2_000),
            "{row}"
        );
    }
    assert_eq!(
        standing(std::slice::from_ref(&mid_turn), 0).typed_ms,
        Some(ROW_MS - 1_000)
    );
    // The newest stamp stands, whatever order the rows were written in, and
    // a typed row lifts no limit.
    let sight = standing(
        &[fable_row(ROW_STAMP), prompt.clone(), command, mid_turn],
        0,
    );
    assert_eq!(sight.typed_ms, Some(ROW_MS + 2_000));
    assert_eq!(sight.fable(), Some(&real_wall()));
    // CONTROLS: rows the vendor writes itself, and typed words it is not
    // sure of, date nothing.
    let none = [
        // A tool's result.
        user_row(
            two,
            "[{\"type\":\"tool_result\",\"tool_use_id\":\"t\",\"content\":\"ok\"}]",
            "",
        ),
        // The caveat written beside a command, and a compaction's summary.
        user_row(two, "\"Caveat: the messages below\"", "\"isMeta\":true,"),
        user_row(
            two,
            "\"This session is being continued\"",
            "\"isCompactSummary\":true,",
        ),
        // A command's output, a background task's notice.
        stdout_row(two, "Set model to Opus 5.5"),
        user_row(two, "\"<task-notification>done</task-notification>\"", ""),
        // A prompt sent with an image, an interruption's notice, and nothing.
        user_row(
            two,
            "[{\"type\":\"text\",\"text\":\"look\"},{\"type\":\"image\"}]",
            "",
        ),
        user_row(
            two,
            "[{\"type\":\"text\",\"text\":\"[Request interrupted by user]\"}]",
            "",
        ),
        user_row(two, "\"  \"", ""),
        // A subagent's prompt, and an answer.
        prompt.replace("\"isSidechain\":false", "\"isSidechain\":true"),
        answer_row(two, "claude-fable-5-2", false),
    ];
    for row in &none {
        assert!(
            crate::json::parse(row).is_some(),
            "the fixture is a whole row: {row}"
        );
        assert_eq!(
            standing(std::slice::from_ref(row), 0).typed_ms,
            None,
            "{row}"
        );
    }
    // CONTROL: a prompt stamped before the floor is spent.
    assert_eq!(standing(&[prompt], ROW_MS + 1_001).typed_ms, None);
}

/// A local command's own row, as one run between turns is recorded.
fn command_row(stamp: &str, name: &str, args: &str) -> String {
    user_row(
        stamp,
        &format!(
            "\"<command-name>/{name}</command-name>\\n            \
             <command-message>{name}</command-message>\\n            \
             <command-args>{args}</command-args>\""
        ),
        "",
    )
}

/// `text` as the `system` row of a command typed while a turn ran (2.1.322).
fn mid_turn_row(stamp: &str, text: &str) -> String {
    format!(
        "{{\"isSidechain\":false,\"type\":\"system\",\"subtype\":\"local_command\",\
         \"content\":\"{text}\",\"level\":\"info\",\"timestamp\":\"{stamp}\",\"isMeta\":false}}"
    )
}

#[test]
fn a_model_chosen_in_the_session_is_read_with_the_word_its_command_gave() {
    let asked = "2026-11-13T06:40:00.000Z";
    let set = "2026-11-13T06:40:01.000Z";
    let set_ms = ROW_MS + 34_571;
    let chose = |rows: &[String]| standing(rows, 0).chose;
    let choice = |word: Option<&str>| {
        Some(Choice {
            at_ms: set_ms,
            word: word.map(str::to_string),
        })
    };
    // `/model sonnet`: the command's own word, not the result's display name.
    let sonnet = [
        command_row(asked, "model", "sonnet"),
        stdout_row(set, "Set model to `Sonnet 5`"),
    ];
    assert_eq!(chose(&sonnet), choice(Some("sonnet")));
    // The same typed while a turn ran.
    let mid = [
        mid_turn_row(
            asked,
            "<command-name>/model</command-name>\\n<command-args>opus[1m]</command-args>",
        ),
        mid_turn_row(
            set,
            "<local-command-stdout>Set model to `Opus 5.5 (1M context)`</local-command-stdout>",
        ),
    ];
    assert_eq!(chose(&mid), choice(Some("opus[1m]")));
    // The picker: the result names an id in bold, which is a word, or a
    // display name, which is not.
    let picked_id = [
        command_row(asked, "model", ""),
        stdout_row(
            set,
            "Set model to \\u001b[1mclaude-fable-5-1\\u001b[22m and saved as your default",
        ),
    ];
    assert_eq!(chose(&picked_id), choice(Some("claude-fable-5-1")));
    let picked = [
        command_row(asked, "model", ""),
        stdout_row(set, "Set model to `Sonnet 5`"),
    ];
    assert_eq!(chose(&picked), choice(None));
    // An argument that is no model word names nothing.
    let odd = [
        command_row(asked, "model", "the big one"),
        stdout_row(set, "Set model to `Sonnet 5`"),
    ];
    assert_eq!(chose(&odd), choice(None));
    // `/fast on` moves the model and names it by its display name.
    let fast = [stdout_row(
        set,
        "\u{26a1} Fast mode ON \u{b7} model set to `Opus 5.5`",
    )];
    assert_eq!(chose(&fast), choice(None));
    // The newest choice stands.
    let both = [sonnet[0].clone(), sonnet[1].clone(), fast[0].clone()];
    assert_eq!(chose(&both), choice(None));
    // CONTROLS. A `/model` that kept the model is no choice, and ends what
    // its command asked: a `/fast on` after it names no word.
    let kept = [
        command_row(asked, "model", "opus"),
        stdout_row(asked, "Kept model as `Fable 5.2`"),
    ];
    assert_eq!(chose(&kept), None);
    let after_kept = [kept[0].clone(), kept[1].clone(), fast[0].clone()];
    assert_eq!(chose(&after_kept), choice(None));
    // Another command's word is not the model's.
    let effort = [
        command_row(asked, "effort", "high"),
        stdout_row(set, "Set model to `Sonnet 5`"),
    ];
    assert_eq!(chose(&effort), choice(None));
    // A subagent's result, one before the floor, and a login.
    let side = stdout_row(set, "Set model to `Sonnet 5`")
        .replace("\"isSidechain\":false", "\"isSidechain\":true");
    assert_eq!(chose(&[side]), None);
    assert_eq!(standing(&sonnet, set_ms + 1).chose, None);
    assert_eq!(chose(&[stdout_row(set, "Login successful")]), None);
}

#[test]
fn only_fable_is_fable() {
    for model in [
        "fable",
        "Fable",
        "FABLE",
        "claude-fable-5",
        "claude-fable-5-2",
        "claude-fable-5-2-20261001",
        "claude-fable-5-20261001",
        "claude-fable-5-2[1m]",
        "claude-fable-5[1M]",
        // The alias with the million-token window is the same model.
        "fable[1m]",
        "Fable[1m]",
        "FABLE[1M]",
    ] {
        assert!(is_fable(model), "{model}");
        assert!(admits_fallback(Some(model)), "{model}");
        // The launch's own line names it: nothing is put in its place.
        assert_eq!(back_to(Some(model)), None, "{model}");
    }
    // CONTROL: every other word, the fallback itself among them.
    for model in [
        "",
        "opus",
        FALLBACK,
        "claude-opus-5-5",
        "sonnet",
        "claude-fable",
        "claude-fable-",
        "claude-fable-x",
        "claude-fable-5-2-1",
        "claude-fablex-5",
        "fable-5",
        "fable[1m]x",
        "fable[2m]",
        "[1m]",
        "fable[1m][1m]",
        "opus[1m]",
        "claude-fable-[1m]",
        "us.anthropic.claude-fable-5-2",
    ] {
        assert!(!is_fable(model), "{model}");
        assert!(!admits_fallback(Some(model)), "{model}");
        assert_eq!(back_to(Some(model)), None, "{model}");
    }
    // A launch that names no model is Claude Code's own choice: it is moved,
    // and it is named on the way back, since a line that names none would
    // stay on the model it was moved onto.
    assert!(admits_fallback(None));
    assert_eq!(back_to(None), Some(FABLE));
}

#[test]
fn only_a_fable_display_name_is_fable() {
    for display in ["Fable 5.2", "Fable", "fable 5", "FABLE 6.1"] {
        assert!(is_fable_display(display), "{display}");
    }
    // CONTROL: other models, and names that only start like it.
    for display in [
        "Opus 5.5",
        "",
        "Fables 1",
        "Fable-5",
        "Sonnet Fable",
        "fable[1m]",
    ] {
        assert!(!is_fable_display(display), "{display}");
    }
}

#[test]
fn the_session_goes_back_after_the_named_reset_or_up_the_rungs() {
    let now = ROW_MS + 5_000;
    let wall = real_wall();
    let reset_ms = RESETS_S * 1_000;
    // The reset the row names, and the grace after it.
    assert_eq!(back_at_ms(Some(&wall), 0, now, 60_000), reset_ms + 60_000);
    assert_eq!(back_at_ms(Some(&wall), 0, now, 0), reset_ms);
    // CONTROL: a row that names none, no row at all, and a reset already
    // over all take the first rung from now.
    let unnamed = Wall {
        resets_at_s: None,
        ..wall.clone()
    };
    let half_hour = now + 30 * 60 * 1_000;
    assert_eq!(back_at_ms(Some(&unnamed), 0, now, 60_000), half_hour);
    assert_eq!(back_at_ms(None, 0, now, 60_000), half_hour);
    assert_eq!(
        back_at_ms(Some(&wall), 0, reset_ms, 60_000),
        reset_ms + 30 * 60 * 1_000
    );
    // Each return that was refused climbs a rung; the last is kept.
    assert_eq!(back_at_ms(None, 1, now, 0), now + 2 * 3_600 * 1_000);
    assert_eq!(back_at_ms(None, 2, now, 0), now + 6 * 3_600 * 1_000);
    assert_eq!(back_at_ms(None, 9, now, 0), now + 6 * 3_600 * 1_000);
    assert_eq!(back_at_ms(None, u32::MAX, now, 0), now + 6 * 3_600 * 1_000);
    // A named reset that did not lift the limit is not believed at once
    // again: never sooner than the rung of the refusals so far.
    let soon = Wall {
        resets_at_s: Some(now / 1_000 + 60),
        ..wall.clone()
    };
    let soon_ms = (now / 1_000 + 60) * 1_000;
    assert_eq!(back_at_ms(Some(&soon), 0, now, 0), soon_ms, "the control");
    assert_eq!(back_at_ms(Some(&soon), 1, now, 0), half_hour);
    assert_eq!(back_at_ms(Some(&soon), 2, now, 0), now + 2 * 3_600 * 1_000);
    // A later named reset still wins over the rung.
    assert_eq!(back_at_ms(Some(&wall), 1, now, 0), reset_ms);
    // A reset further off than any weekly window is held to the bound.
    let far = Wall {
        resets_at_s: Some(now / 1_000 + 30 * 24 * 3_600),
        ..wall
    };
    assert_eq!(back_at_ms(Some(&far), 0, now, 60_000), now + MAX_AWAY_MS);
}

#[test]
fn a_move_that_could_not_be_made_backs_off_and_never_to_nothing() {
    assert_eq!(retry_after_ms(0), 60_000);
    assert_eq!(retry_after_ms(1), 120_000);
    assert_eq!(retry_after_ms(2), 240_000);
    assert_eq!(retry_after_ms(4), 960_000);
    // From the fifth failure on, the longest pause, however many follow.
    for attempts in [5, 6, 15, 16, 40, 60, 63, 64, 65, u32::MAX] {
        assert_eq!(retry_after_ms(attempts), RETRY_MAX_MS, "{attempts}");
    }
    let mut state = State::new(1);
    let mut now = 1_000_000;
    let mut pauses = Vec::new();
    for _ in 0..8 {
        let pause = state.failed(now);
        assert_eq!(state.retry_at_ms, now + pause);
        pauses.push(pause);
        now = state.retry_at_ms;
    }
    assert_eq!(
        pauses,
        [
            60_000, 120_000, 240_000, 480_000, 960_000, 1_800_000, 1_800_000, 1_800_000
        ]
    );
    assert_eq!(state.attempts, 8);
    // CONTROL: a move that was made owes no attempt.
    state.on_launch(7);
    assert_eq!(
        (state.attempts, state.retry_at_ms, state.floor_ms),
        (0, 0, 7)
    );
}

#[test]
fn only_a_session_on_fable_is_moved_and_only_before_the_reset() {
    let wall = real_wall();
    let now = ROW_MS + 5_000;
    let on_launch = State::new(1);
    let moved = Decision::Move;
    assert_eq!(decide(&on_launch, &wall, None, None, now), moved);
    assert_eq!(decide(&on_launch, &wall, None, Some("fable"), now), moved);
    assert_eq!(
        decide(&on_launch, &wall, None, Some("claude-fable-5-2"), now),
        moved
    );
    assert_eq!(
        seen_line(&wall, &moved),
        format!(
            "limit-seen bucket=seven_day_overage_included at={} resets-at={RESETS_S} \
             decision=move to=claude-opus-5-5",
            ROW_MS / 1_000
        )
    );
    // CONTROLS: each reason to stay, in the order they are asked.
    let stay = |why: &str| Decision::Stay(why.to_string());
    let off = State {
        phase: Phase::OffFable {
            back_at_ms: now + 1,
        },
        ..on_launch
    };
    assert_eq!(
        decide(&off, &wall, None, Some(FALLBACK), now),
        stay("on-fallback")
    );
    assert_eq!(decide(&off, &wall, None, None, now), stay("on-fallback"));
    assert_eq!(
        decide(&on_launch, &wall, None, Some(FALLBACK), now),
        stay("on-fallback")
    );
    assert_eq!(
        decide(&on_launch, &wall, None, Some("opus"), now),
        stay("model=opus")
    );
    assert_eq!(
        decide(&on_launch, &wall, None, Some("sonnet"), now),
        stay("model=sonnet")
    );
    // The reset is over on its own second, and not the millisecond before.
    let reset_ms = RESETS_S * 1_000;
    assert_eq!(decide(&on_launch, &wall, None, None, reset_ms - 1), moved);
    assert_eq!(
        decide(&on_launch, &wall, None, None, reset_ms),
        stay("reset-passed")
    );
    // A session on another model is told so before the reset is read.
    assert_eq!(
        decide(&on_launch, &wall, None, Some("opus"), reset_ms),
        stay("model=opus")
    );
    // A refusal written at or after the reset it names was made by a window
    // that reset did not lift: no reset passed, the session is moved, and it
    // goes back on the first rung, not on the reset already over.
    for written in [reset_ms, reset_ms + 1, reset_ms + 3_600_000] {
        let late = Wall {
            at_ms: written,
            ..wall.clone()
        };
        assert!(!late.reset_passed(written + 5_000), "{written}");
        assert_eq!(
            decide(&on_launch, &late, None, None, written + 5_000),
            moved,
            "{written}"
        );
        assert_eq!(
            back_at_ms(Some(&late), 0, written + 5_000, 60_000),
            written + 5_000 + 30 * 60 * 1_000,
            "{written}"
        );
    }
    // CONTROL: the same row written the millisecond before its reset is over
    // at the reset, as the real row (written two days before) is.
    let early = Wall {
        at_ms: reset_ms - 1,
        ..wall.clone()
    };
    assert!(early.reset_passed(reset_ms));
    assert_eq!(
        decide(&on_launch, &early, None, None, reset_ms),
        stay("reset-passed")
    );
    // A launch that asked for another model is never moved, whatever a
    // restart moved it onto since; a Fable launch moved onto another Fable
    // name is, and one moved onto another model is not.
    assert_eq!(
        decide(&on_launch, &wall, Some("sonnet"), Some("fable"), now),
        stay("model=sonnet")
    );
    assert_eq!(
        decide(&on_launch, &wall, Some("sonnet"), Some("sonnet"), now),
        stay("model=sonnet")
    );
    assert_eq!(
        decide(
            &on_launch,
            &wall,
            Some("fable"),
            Some("claude-fable-5-2"),
            now
        ),
        moved
    );
    assert_eq!(
        decide(&on_launch, &wall, Some("fable"), Some("sonnet"), now),
        stay("model=sonnet")
    );
    let unnamed = Wall {
        resets_at_s: None,
        ..wall
    };
    assert_eq!(decide(&on_launch, &unnamed, None, None, u64::MAX), moved);
    assert_eq!(
        seen_line(&unnamed, &stay("model=opus")),
        format!(
            "limit-seen bucket=seven_day_overage_included at={} resets-at=- \
             decision=stay model=opus",
            ROW_MS / 1_000
        )
    );
}

#[test]
fn the_state_follows_a_move_a_return_and_a_return_that_was_refused() {
    let wall = real_wall();
    let reset_ms = RESETS_S * 1_000;
    let now = ROW_MS + 5_000;
    let mut state = State::new(100);
    state.failed(now);
    // Off Fable: back after the reset, the rows before the new child spent,
    // the back-off cleared.
    assert_eq!(
        state.left_fable(Some(&wall), now, 200, 60_000),
        reset_ms + 60_000
    );
    assert_eq!(
        state,
        State {
            floor_ms: 200,
            phase: Phase::OffFable {
                back_at_ms: reset_ms + 60_000
            },
            walled_returns: 0,
            attempts: 0,
            retry_at_ms: 0,
        }
    );
    // An answer on the fallback is not the return's.
    state.answered();
    assert!(matches!(state.phase, Phase::OffFable { .. }));
    // Back: a return that has yet to answer.
    state.on_launch(300);
    assert_eq!(state.phase, Phase::OnLaunch { returned: true });
    assert_eq!(state.floor_ms, 300);
    // Refused again before it answered: one walled return, and the rung's
    // pace where the row names a reset that is already here.
    let later = reset_ms + 120_000;
    let again = Wall {
        at_ms: later,
        resets_at_s: Some(later / 1_000 + 60),
        ..wall.clone()
    };
    let back = state.left_fable(Some(&again), later, 400, 60_000);
    assert_eq!(state.walled_returns, 1);
    assert_eq!(back, later + 30 * 60 * 1_000);
    // And once more: the next rung.
    state.on_launch(500);
    let back = state.left_fable(Some(&again), later, 600, 60_000);
    assert_eq!(state.walled_returns, 2);
    assert_eq!(back, later + 2 * 3_600 * 1_000);
    // CONTROL: a return that answered starts the count again, so the next
    // limit is believed as a new one.
    state.on_launch(700);
    state.answered();
    assert_eq!(state.phase, Phase::OnLaunch { returned: false });
    assert_eq!(state.walled_returns, 0);
    let back = state.left_fable(Some(&again), later, 800, 0);
    assert_eq!(state.walled_returns, 0);
    assert_eq!(back, (later / 1_000 + 60) * 1_000);
    // CONTROL: a move from a launch that was never off Fable is no return.
    let mut fresh = State::new(1);
    fresh.on_launch(2);
    assert_eq!(fresh.phase, Phase::OnLaunch { returned: false });
}

#[test]
fn an_account_move_drops_the_old_accounts_reset_and_its_refused_returns() {
    let mut state = State::new(1);
    let back = state.left_fable(None, 1_000, 2, 0);
    assert_eq!(back, 1_000 + 30 * 60 * 1_000);
    state.walled_returns = 3;
    // Onto another account: back at the next point a return may be made,
    // and the old account's refused returns count no more.
    assert_eq!(state.account_moved(5_000), Some(5_000));
    assert_eq!(state.phase, Phase::OffFable { back_at_ms: 5_000 });
    assert_eq!(state.walled_returns, 0);
    assert_eq!(state.floor_ms, 2, "the start's own floor stands");
    // A time to go back that has already come stays as it is.
    assert_eq!(state.account_moved(9_000), Some(5_000));
    // CONTROL: on the launch's terms there is no time to go back to move,
    // and only the refused returns are the old account's.
    let mut on_launch = State::new(1);
    on_launch.phase = Phase::OnLaunch { returned: true };
    on_launch.walled_returns = 2;
    assert_eq!(on_launch.account_moved(5_000), None);
    assert_eq!(on_launch.phase, Phase::OnLaunch { returned: true });
    assert_eq!(on_launch.walled_returns, 0);
}

#[test]
fn a_move_is_owed_at_fables_limit_on_the_launchs_terms_even_while_it_backs_off() {
    let now = ROW_MS + 5_000;
    let at_fable = Sight {
        wall: Some(real_wall()),
        ..Sight::default()
    };
    let mut state = State::new(1);
    assert!(owes_move(&state, &at_fable, None, None, now));
    // A move that could not be made is still owed: the limit stays the
    // move's while it backs off.
    state.failed(now);
    assert!(owes_move(&state, &at_fable, None, None, now));
    // CONTROLS: another window's limit, a session that names another model,
    // a reset that has passed, and a session already off Fable owe none.
    let weekly = Sight {
        wall: Some(Wall {
            bucket: Some("seven_day".to_string()),
            ..real_wall()
        }),
        ..Sight::default()
    };
    assert!(!owes_move(&state, &weekly, None, None, now));
    assert!(!owes_move(&state, &at_fable, Some("sonnet"), None, now));
    assert!(!owes_move(&state, &at_fable, None, None, RESETS_S * 1_000));
    let off = State {
        phase: Phase::OffFable { back_at_ms: now },
        ..state
    };
    assert!(!owes_move(&off, &at_fable, None, None, now));
    assert!(!owes_move(&state, &Sight::default(), None, None, now));
}

/// A fresh scratch folder for one test.
fn scratch(name: &str) -> PathBuf {
    let root = std::env::temp_dir().join(".ubd").join(format!(
        "claude-alab-limit-{name}-{}-{}.noindex",
        std::process::id(),
        crate::supervise::now_ms()
    ));
    std::fs::create_dir_all(&root).expect("a scratch folder");
    root
}

fn append(path: &std::path::Path, text: &str) {
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .expect("the transcript")
        .write_all(text.as_bytes())
        .expect("append");
}

#[test]
fn the_transcript_source_keeps_a_wall_standing_however_much_is_written_after_it() {
    let root = scratch("source");
    let path = root.join("t.jsonl");
    let mut source = Transcript::default();
    let at = |floor_ms| At {
        transcript: Some(path.as_path()),
        floor_ms,
    };
    // No transcript yet, and none named: nothing stands.
    assert_eq!(source.look(&at(0)), Sight::default());
    assert_eq!(
        source.look(&At {
            transcript: None,
            floor_ms: 0
        }),
        Sight::default()
    );
    // An answer, then the wall, the wall's row still being written.
    let wall = fable_row(ROW_STAMP);
    let (head, tail) = wall.split_at(wall.len() / 2);
    append(
        &path,
        &format!(
            "{}\n{head}",
            answer_row("2026-11-13T06:00:00.000Z", "claude-fable-5-2", false)
        ),
    );
    assert_eq!(
        source.look(&at(0)),
        Sight {
            wall: None,
            answered: true,
            typed_ms: None,
            chose: None
        }
    );
    // The rest of the row and its newline arrive: the wall stands.
    append(&path, &format!("{tail}\n"));
    assert_eq!(source.look(&at(0)).fable(), Some(&real_wall()));
    // More than a tail's worth of rows that lift nothing follows, in several
    // writes: the wall still stands, because only what was appended is read.
    // They are prompts a person sent, at 06:41:00.000.
    let filler_ms = ROW_MS + 93_571;
    let typed = Sight {
        typed_ms: Some(filler_ms),
        ..Sight::default()
    };
    let filler = format!(
        "{{\"type\":\"user\",\"isSidechain\":false,\"timestamp\":\"2026-11-13T06:41:00.000Z\",\
         \"message\":{{\"role\":\"user\",\"content\":\"{}\"}}}}\n",
        "x".repeat(4_000)
    );
    for _ in 0..4 {
        append(&path, &filler.repeat(20));
        let sight = source.look(&at(0));
        assert_eq!(sight.fable(), Some(&real_wall()));
        assert_eq!(sight.typed_ms, Some(filler_ms));
    }
    let written = std::fs::metadata(&path).expect("the transcript").len();
    assert!(written > profile::TRANSCRIPT_TAIL_BYTES + wall.len() as u64);
    // CONTROL: a source that starts now reads only the tail, which no longer
    // holds the row. (So an own-image move, which starts one, may lose a wall
    // this old; the state it carries is what keeps a session off Fable.)
    assert_eq!(Transcript::default().look(&at(0)), typed);
    // CONTROL: the same rows from a floor after the wall say nothing of it
    // either.
    assert_eq!(source.look(&at(ROW_MS + 1)), typed);
    // Back at the first floor the tail is read afresh, and the wall is gone
    // with the rows that pushed it out.
    assert_eq!(source.look(&at(0)), typed);
    // CONTROL: a floor after the prompts dates no typed input.
    assert_eq!(source.look(&at(filler_ms + 1)), Sight::default());
    // A wall in the tail is found by a fresh source, and an answer lifts it.
    append(
        &path,
        &format!("{}\n", fable_row("2026-11-13T07:00:00.000Z")),
    );
    let mut fresh = Transcript::default();
    assert!(fresh.look(&at(0)).fable().is_some());
    assert!(source.look(&at(0)).fable().is_some());
    append(
        &path,
        &format!(
            "{}\n",
            answer_row("2026-11-13T07:05:00.000Z", "claude-fable-5-2", false)
        ),
    );
    assert_eq!(
        fresh.look(&at(0)),
        Sight {
            wall: None,
            answered: true,
            typed_ms: Some(filler_ms),
            chose: None
        }
    );
    // A file that shrank is another file's worth of rows: read afresh.
    std::fs::write(&path, format!("{}\n", fable_row(ROW_STAMP))).expect("rewrite");
    assert_eq!(fresh.look(&at(0)).fable(), Some(&real_wall()));
    // Gone: nothing stands.
    std::fs::remove_file(&path).expect("remove");
    assert_eq!(fresh.look(&at(0)), Sight::default());
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn the_reset_grace_is_a_harness_option_with_bounds() {
    let grace = |words: &[&str]| {
        let args = words.iter().map(std::ffi::OsString::from).collect();
        match cli::parse(args) {
            Ok(cli::Command::Run { opts, .. }) => Ok(opts.reset_grace_ms),
            Ok(other) => panic!("not a run: {other:?}"),
            Err(why) => Err(why),
        }
    };
    assert_eq!(grace(&[]), Ok(RESET_GRACE_MS));
    assert_eq!(grace(&["--alab-reset-grace-ms=0"]), Ok(0));
    assert_eq!(
        grace(&["--alab-reset-grace-ms=3600000", "-c"]),
        Ok(3_600_000)
    );
    // CONTROL: past the bound, valueless and not a number are refused, and
    // after the first vendor word the same text is the vendor's.
    for bad in [
        "--alab-reset-grace-ms=3600001",
        "--alab-reset-grace-ms",
        "--alab-reset-grace-ms=soon",
        "--alab-reset-grace-ms=-1",
    ] {
        assert!(grace(&[bad]).is_err(), "{bad}");
    }
    assert_eq!(
        grace(&["-c", "--alab-reset-grace-ms=5"]),
        Ok(RESET_GRACE_MS)
    );
}
