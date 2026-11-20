// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The typed writes' spelling, aterm's answers to them, the screen read and the
//! judge, each against aterm's own words.

use super::*;
use crate::aterm_wire::{TURN_IDLE_MS, TypedFence, key_verb, press_verb, send_verb, turn_verb};

const SID: &str = "t0000000001";
const ME: &str = "claude-code-alab@4242";

fn fence(duty: Scope) -> TypedFence<'static> {
    TypedFence {
        holder: ME,
        revision: 136,
        duty,
        human_seq: 1474,
    }
}

/// The rows of an idle Claude Code 2.1.302 whose composer is empty, as `text`
/// prints them (aterm-phase's `claude-2.1.302-idle-compacted` fixture, cut).
fn composer_rows() -> Vec<String> {
    let rule = "─".repeat(90);
    [
        "⏺ Done.",
        "",
        &rule,
        "❯",
        &rule,
        "  ⏵⏵ bypass permissions on (shift+tab to cycle)",
    ]
    .map(str::to_string)
    .to_vec()
}

/// The rows of a Claude Code 2.1.316 permission box, which draws no composer.
fn box_rows() -> Vec<String> {
    [
        " Do you want to proceed?",
        " ❯ 1. Yes",
        "   2. No",
        "",
        " Esc to cancel · Tab to amend",
    ]
    .map(str::to_string)
    .to_vec()
}

#[test]
fn a_typed_write_names_the_claim_its_revision_its_duty_and_the_persons_count() {
    assert_eq!(
        send_verb(SID, &fence(Scope::TurnEnd), "continue").as_deref(),
        Some(
            "@t0000000001 send if-supervisor=claude-code-alab@4242 if-supervisor-revision=136 \
             harness-duty=turn-end if-human=1474 -- continue"
        )
    );
    // `--` ends the options, so a text that reads as one is typed as text.
    assert_eq!(
        send_verb(SID, &fence(Scope::Walls), "if-human=1 /login").as_deref(),
        Some(
            "@t0000000001 send if-supervisor=claude-code-alab@4242 if-supervisor-revision=136 \
             harness-duty=walls if-human=1474 -- if-human=1 /login"
        )
    );
    assert_eq!(
        key_verb(SID, &fence(Scope::Approvals), "enter").as_deref(),
        Some(
            "@t0000000001 key if-supervisor=claude-code-alab@4242 if-supervisor-revision=136 \
             harness-duty=approvals if-human=1474 enter"
        )
    );
    assert_eq!(
        turn_verb(SID, &fence(Scope::Questions), "^❯\\shello\\s*$", "hello").as_deref(),
        Some(
            format!(
                "@t0000000001 turn if-supervisor=claude-code-alab@4242 \
                 if-supervisor-revision=136 harness-duty=questions \
                 submit=guarded:^❯\\shello\\s*$ if-human=1474 idle={TURN_IDLE_MS} \
                 timeout={TURN_TIMEOUT_MS} -- hello"
            )
            .as_str()
        )
    );
}

fn press(key: &'static str, act: Option<KeyAct>) -> Press<'static> {
    Press {
        key,
        generation: "1.90262",
        guard: "^\\x20Do\\x20you\\x20want\\x20to\\x20proceed\\?\\s*$",
        act,
        human_seq: 1474,
        grace: Duration::from_millis(2_500),
    }
}

#[test]
fn a_key_on_a_box_is_fenced_on_the_read_it_was_decided_from() {
    assert_eq!(
        press_verb(
            SID,
            &fence(Scope::Approvals),
            &press("1", Some(KeyAct::Approve))
        )
        .as_deref(),
        Some(
            "@t0000000001 key if-supervisor=claude-code-alab@4242 if-supervisor-revision=136 \
             harness-duty=approvals if-human=1474 if-gen=1.90262 \
             if=^\\x20Do\\x20you\\x20want\\x20to\\x20proceed\\?\\s*$ act=approve 1"
        )
    );
    // A key that only moves the focus means nothing for the box: no `act=`.
    assert_eq!(
        press_verb(SID, &fence(Scope::Questions), &press("down", None)).as_deref(),
        Some(
            "@t0000000001 key if-supervisor=claude-code-alab@4242 if-supervisor-revision=136 \
             harness-duty=questions if-human=1474 if-gen=1.90262 \
             if=^\\x20Do\\x20you\\x20want\\x20to\\x20proceed\\?\\s*$ down"
        )
    );
    // Negative controls: no generation, no guard, a guard of two words, a key
    // no verb spells and a duty that grants no typing are never pressed.
    let duty = fence(Scope::Approvals);
    for generation in ["", "1", "1.", "x.2"] {
        let unfenced = Press {
            generation,
            ..press("1", Some(KeyAct::Approve))
        };
        assert_eq!(press_verb(SID, &duty, &unfenced), None, "{generation:?}");
    }
    for guard in ["", "^a b$", "^a\tb$"] {
        let unguarded = Press {
            guard,
            ..press("1", Some(KeyAct::Approve))
        };
        assert_eq!(press_verb(SID, &duty, &unguarded), None, "{guard:?}");
    }
    assert_eq!(press_verb(SID, &duty, &press("if=x", None)), None);
    assert_eq!(
        press_verb(SID, &fence(Scope::Relaunch), &press("1", None)),
        None
    );
}

#[test]
fn a_write_aterm_could_not_read_as_typed_or_that_grants_no_typing_is_not_spelled() {
    // Relaunch and upgrade grant no typing: aterm refuses them whole.
    for duty in [Scope::Relaunch, Scope::Upgrade] {
        assert_eq!(send_verb(SID, &fence(duty), "x"), None, "{duty:?}");
        assert_eq!(key_verb(SID, &fence(duty), "enter"), None, "{duty:?}");
        assert_eq!(turn_verb(SID, &fence(duty), "^x$", "x"), None, "{duty:?}");
    }
    let duty = fence(Scope::TurnEnd);
    for sid in ["", "t000000000", "@t0000000001", "s-00", "t0000000001 x"] {
        assert_eq!(send_verb(sid, &duty, "x"), None, "{sid:?}");
    }
    for holder in ["", "-", "a b", "a=b", "a\tb"] {
        let fence = TypedFence { holder, ..duty };
        assert_eq!(send_verb(SID, &fence, "x"), None, "{holder:?}");
    }
    for text in ["", "   ", "a\nb", "a\rb", "a\u{1b}[A", "tab\there"] {
        assert_eq!(send_verb(SID, &duty, text), None, "{text:?}");
        assert_eq!(turn_verb(SID, &duty, "^x$", text), None, "{text:?}");
    }
    for key in ["", "ctrl c", "if=x", "--", "\u{1b}", &"x".repeat(33), "é"] {
        assert_eq!(key_verb(SID, &duty, key), None, "{key:?}");
    }
    assert!(key_verb(SID, &duty, &"x".repeat(32)).is_some());
    let longest = "x".repeat(alab_policy::guard::MAX_GUARD_BYTES);
    assert!(turn_verb(SID, &duty, &longest, "x").is_some());
    for guard in ["", "^a b$", "^a\tb$", &format!("{longest}x")] {
        assert_eq!(turn_verb(SID, &duty, guard, "x"), None, "{guard:?}");
    }
}

#[test]
fn a_send_or_keys_answers_read_as_aterm_words_them() {
    for (line, reply) in [
        ("OK seq=90262", Reply::Written { seq: Some(90262) }),
        (
            "OK seq=12 warn=input-unread input_bytes=3 input_wait_ms=1200",
            Reply::Written { seq: Some(12) },
        ),
        ("OK", Reply::Written { seq: None }),
        (
            "OK skipped reason=person seq=13",
            Reply::Person { seq: Some(13) },
        ),
        (
            "OK skipped reason=changed seq=4",
            Reply::Skipped("changed".to_string()),
        ),
        ("OK skipped seq=4", Reply::Skipped("guard".to_string())),
        (
            "ERR busy sink",
            Reply::Busy("ERR%20busy%20sink".to_string()),
        ),
        (
            "ERR busy supervisor input in flight",
            Reply::Busy("ERR%20busy%20supervisor%20input%20in%20flight".to_string()),
        ),
        (
            "ERR busy supervisor_act=observe (the agent's image is unconfirmed: the harness \
             types nothing into it)",
            Reply::Observed,
        ),
        ("ERR denied supervisor input authority", Reply::Denied),
        (
            "ERR write failed partial accepted=3",
            Reply::InDoubt("ERR%20write%20failed%20partial%20accepted=3".to_string()),
        ),
        (
            "ERR busy supervisor=-",
            Reply::Refused("ERR%20busy%20supervisor=-".to_string()),
        ),
        (
            "ERR halted by=person",
            Reply::Refused("ERR%20halted%20by=person".to_string()),
        ),
    ] {
        assert_eq!(Reply::of_write(line), reply, "{line:?}");
    }
    let usage = "ERR usage: typed supervisor input takes if-supervisor=<holder> \
                 if-supervisor-revision=<n> harness-duty=<scope> together, once";
    assert!(matches!(Reply::of_write(usage), Reply::Usage(_)));
    // A sequence outside its grammar is no sequence.
    assert_eq!(Reply::of_write("OK seq=007"), Reply::Written { seq: None });
}

#[test]
fn a_turns_answers_read_as_aterm_words_them() {
    let done = "seq=9 id=3 dur_ms=812 hash=5633b312ee9f9b16";
    for (header, reply) in [
        (
            format!("OK 24 turn submitted=1 status=settled {done} submit_by=composer"),
            Reply::Submitted,
        ),
        (
            format!("OK 24 turn submitted=0 status=settled {done} pressed=3"),
            Reply::Unverified,
        ),
        (
            format!("OK 24 turn submitted=0 status=timeout {done}"),
            Reply::Left("timeout".to_string()),
        ),
        (
            "OK 0 turn skipped reason=person submitted=0 seq=9 id=3".to_string(),
            Reply::Person { seq: Some(9) },
        ),
        (
            "OK 0 turn skipped reason=person typed=0 pasted=5 submitted=0 seq=9 id=3".to_string(),
            Reply::Left("person".to_string()),
        ),
        (
            "OK 0 turn skipped reason=guard submitted=0 seq=9 id=3".to_string(),
            Reply::Left("guard".to_string()),
        ),
        (
            "OK 0 turn skipped reason=changed submitted=0 seq=9 id=3".to_string(),
            Reply::Skipped("changed".to_string()),
        ),
        (
            "OK 0 turn skipped reason=deadline typed=0 pasted=0 submitted=0 seq=9 id=3".to_string(),
            Reply::Skipped("deadline".to_string()),
        ),
        (
            "OK 0 turn skipped reason=halted typed=4 pasted=2 submitted=0 seq=9 id=3".to_string(),
            Reply::Left("halted".to_string()),
        ),
        (
            "ERR key delivery failed sink=busy".to_string(),
            Reply::Left("busy".to_string()),
        ),
        (
            "ERR key delivery failed".to_string(),
            Reply::InDoubt("ERR%20key%20delivery%20failed".to_string()),
        ),
        (
            "ERR paste delivery failed".to_string(),
            Reply::InDoubt("ERR%20paste%20delivery%20failed".to_string()),
        ),
        (
            "ERR denied supervisor input authority".to_string(),
            Reply::Denied,
        ),
        (
            "OK 0 dup=1".to_string(),
            Reply::Refused("OK%200%20dup=1".to_string()),
        ),
    ] {
        assert_eq!(Reply::of_turn(&header), reply, "{header:?}");
    }
}

#[test]
fn a_screen_read_is_taken_only_whole() {
    let body = "{\"rows\":[\"⏺ Done.\",\"❯\"],\"cursor\":{\"row\":1,\"col\":2,\"visible\":true,\
                \"style\":\"steady_block\"},\"dims\":{\"rows\":2,\"cols\":80},\"seq\":90262,\
                \"gen\":\"1.90262\",\"title\":\"\",\"human_ms\":null,\"human_seq\":1474,\
                \"launch\":null}";
    assert_eq!(
        Screen::parse(body),
        Some(Screen {
            rows: vec!["⏺ Done.".to_string(), "❯".to_string()],
            seq: 90262,
            human: Human::Never,
            human_seq: 1474,
            cursor: Some((1, 2)),
            title: Some(String::new()),
            generation: Some("1.90262".to_string()),
        })
    );
    // A generation that is not two runs of digits joined by a dot is none,
    // and the read is still a read.
    for word in ["\"1.\"", "\".5\"", "\"1.2.3\"", "\"a.1\"", "12", "\"1 .2\""] {
        let screen = Screen::parse(&body.replace("\"1.90262\"", word)).expect(word);
        assert_eq!(screen.generation, None, "{word}");
    }
    let screen = Screen::parse(&body.replace("\"gen\":\"1.90262\",", "")).unwrap();
    assert_eq!(screen.generation, None);
    // The cursor and the title are read where they are, and a read without
    // them is still a read; a cursor off the rows is none.
    for (from, to) in [
        ("\"title\":\"\",", ""),
        ("\"title\":\"\"", "\"title\":7"),
        ("\"row\":1,", ""),
        ("\"row\":1", "\"row\":2"),
        ("\"col\":2", "\"col\":-2"),
    ] {
        let screen = Screen::parse(&body.replace(from, to)).expect(to);
        assert_eq!(screen.seq, 90262, "{to:?}");
        assert!(screen.cursor.is_none() || screen.title.is_none(), "{to:?}");
    }
    let titled = body.replace("\"title\":\"\"", "\"title\":\"✳ Working\"");
    assert_eq!(
        Screen::parse(&titled).and_then(|screen| screen.title),
        Some("✳ Working".to_string())
    );
    let typed = body.replace("\"human_ms\":null", "\"human_ms\":250");
    assert_eq!(
        Screen::parse(&typed).map(|screen| screen.human),
        Some(Human::Ago(Duration::from_millis(250)))
    );
    for (from, to) in [
        ("\"human_seq\":1474", "\"human_seq\":-1"),
        ("\"human_seq\":1474,", ""),
        ("\"seq\":90262,", ""),
        ("\"human_ms\":null,", ""),
        ("\"human_ms\":null", "\"human_ms\":\"soon\""),
        ("[\"⏺ Done.\",\"❯\"]", "[\"⏺ Done.\",7]"),
        ("[\"⏺ Done.\",\"❯\"]", "\"❯\""),
    ] {
        assert_eq!(Screen::parse(&body.replace(from, to)), None, "{to:?}");
    }
    assert_eq!(Screen::parse("not json"), None);
}

#[test]
fn a_turns_guard_is_the_composer_row_claudes_reader_finds() {
    let rows = composer_rows();
    assert_eq!(
        composer_guard(&rows, "hello world").as_deref(),
        Some("^❯\\shello\\x20world\\s*$")
    );
    // A text past the guard's whole length is named by its last word's tail.
    let long = "please carry on with the remaining packages of the plan now";
    assert_eq!(
        composer_guard(&rows, long).as_deref(),
        Some("^[❯\\s]\\s.*now\\s*$")
    );
    assert_eq!(
        composer_guard(&rows, long),
        Some(composer_guard_at('❯', 0, long))
    );
    // A box draws no composer, so a turn has no row to land on.
    assert_eq!(composer_guard(&box_rows(), "hello"), None);
    assert_eq!(composer_guard(&[], "hello"), None);
}

/// A policy in effect with a two-minute grace, as a live aterm read it.
fn policy() -> Policy {
    Policy::parse(
        "OK gen=1 effective=on why=enabled contract=2,3 relaunch=true upgrade=true \
         upgrade_ask=false human_grace_s=120",
    )
    .expect("policy")
}

fn status(fields: &str) -> Status {
    Status::parse(&format!(
        "OK schema=1 supervisor=claude-code-alab@4242 supervisor_proven=1 \
         supervisor_scope=relaunch,upgrade,turn-end supervisor_revision=136 human_ms=- \
         human_seq=1474 {fields}"
    ))
    .expect("status")
}

#[test]
fn a_write_is_judged_by_the_claim_the_policy_and_the_persons_grace_in_that_order() {
    let on = policy();
    assert_eq!(
        judge(&on, &status(""), ME, Scope::TurnEnd),
        Ok(Judged {
            revision: 136,
            human_seq: 1474
        })
    );
    let held = |policy: &Policy, fields: &str, duty: Scope| {
        judge(policy, &status(fields), ME, duty)
            .expect_err("a hold")
            .to_string()
    };
    assert_eq!(
        held(&on, "", Scope::Upgrade),
        "wait aterm harness-duty=upgrade"
    );
    assert_eq!(
        held(&on, "supervisor=someone@7", Scope::TurnEnd),
        "wait aterm supervisor=someone@7"
    );
    assert_eq!(
        held(&on, "supervisor=-", Scope::TurnEnd),
        "wait aterm supervisor=-"
    );
    assert_eq!(
        held(&on, "supervisor_proven=0", Scope::TurnEnd),
        "wait aterm supervisor_proven=0"
    );
    assert_eq!(
        held(&on, "", Scope::Approvals),
        "wait aterm supervisor_scope=relaunch,upgrade,turn-end"
    );
    assert_eq!(
        held(&on, "supervisor_revision=x", Scope::TurnEnd),
        "wait aterm supervisor_revision=absent"
    );
    let off = Policy::parse(
        "OK gen=2 effective=off why=hold contract=2,3 relaunch=true upgrade=true \
         upgrade_ask=false human_grace_s=120",
    )
    .expect("policy");
    assert_eq!(held(&off, "", Scope::TurnEnd), "wait aterm effective=off");
    let old = Policy::parse(
        "OK gen=2 effective=on contract=2 relaunch=true upgrade=true upgrade_ask=false \
         human_grace_s=120",
    )
    .expect("policy");
    assert_eq!(
        held(&old, "", Scope::TurnEnd),
        "wait aterm contract=unserved"
    );
    // A key within the grace holds with what is left of it; one past it does not.
    let typed = judge(&on, &status("human_ms=20000"), ME, Scope::TurnEnd).expect_err("grace");
    assert_eq!(typed.to_string(), "wait aterm human_ms=20000");
    assert_eq!(typed.grace_left, Some(Duration::from_secs(100)));
    assert!(judge(&on, &status("human_ms=120000"), ME, Scope::TurnEnd).is_ok());
    // A press's own grace stands in the policy's: the same key 20 s old passes
    // a 2.5 s grace and is held by a 30 s one.
    let approving = status("supervisor_scope=approvals human_ms=20000");
    let short = Duration::from_millis(2_500);
    assert!(judge_within(&on, &approving, ME, Scope::Approvals, short).is_ok());
    let long = judge_within(
        &on,
        &approving,
        ME,
        Scope::Approvals,
        Duration::from_secs(30),
    );
    assert_eq!(
        long.expect_err("grace").grace_left,
        Some(Duration::from_secs(10))
    );
    assert_eq!(
        held(&on, "human_ms=soon", Scope::TurnEnd),
        "wait aterm human_ms=unknown"
    );
    assert_eq!(
        held(&on, "human_seq=x", Scope::TurnEnd),
        "wait aterm human_seq=absent"
    );
}

#[test]
fn a_typist_names_its_own_holder_and_only_a_session_it_can_address() {
    let typist = Typist::new(SID, 4242).expect("typist");
    assert_eq!(typist.holder(), ME);
    assert_eq!(Typist::new("nope", 4242), None);
    assert_eq!(Typist::new(SID, 1), None);
}
