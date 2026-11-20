// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! What a resume line may change of the launch: its permission mode, only
//! within what the launch's own command line asked for, and its model.

use super::*;

fn words(words: &[&str]) -> Vec<String> {
    words.iter().map(|word| (*word).to_string()).collect()
}

/// The resume flags of the launch `launch` (without `argv[0]`).
fn resumed(launch: &[&str]) -> Vec<String> {
    let argv: Vec<String> = std::iter::once("claude")
        .chain(launch.iter().copied())
        .map(str::to_string)
        .collect();
    resume_argv(&argv, "ID").expect("a resumable launch")
}

#[test]
fn a_resume_is_never_wider_than_the_launch_asked_for() {
    // The reproduced defect: an explicit `default` launch whose conversation's
    // history ends in a wider mode. The launch's own flags stand.
    for launch in [
        &["--permission-mode", "default"][..],
        &["--permission-mode=default"],
        &["--permission-mode", "manual"],
    ] {
        for history in ["auto", "dontAsk", "acceptEdits"] {
            assert_eq!(
                with_permission_mode(resumed(launch), Some(history)),
                resumed(launch),
                "{launch:?} after a historical {history}"
            );
        }
    }
    // Each mode against each launch: carried only when it is the launch's own
    // mode, a narrower one, or `plan`.
    const MODES: [&str; 5] = ["plan", "default", "acceptEdits", "auto", "dontAsk"];
    let carried = |launch: &str, history: &str| {
        let flags = resumed(&["--permission-mode", launch]);
        let out = with_permission_mode(flags.clone(), Some(history));
        if out == flags && launch != history {
            false
        } else {
            assert_eq!(
                out,
                words(&["--permission-mode", history, "--resume", "ID"]),
                "{launch} resumed in {history}"
            );
            true
        }
    };
    for (launch, within) in [
        ("plan", &["plan"][..]),
        ("default", &["plan", "default"]),
        ("acceptEdits", &["plan", "default", "acceptEdits"]),
        ("auto", &["plan", "default", "acceptEdits", "auto"]),
        ("dontAsk", &["plan", "dontAsk"]),
    ] {
        for history in MODES {
            assert_eq!(
                carried(launch, history),
                within.contains(&history),
                "launch {launch}, history {history}"
            );
        }
    }
    // The last `--permission-mode` is the launch's, as Claude Code reads it.
    let twice = resumed(&["--permission-mode", "auto", "--permission-mode=default"]);
    assert_eq!(
        with_permission_mode(twice.clone(), Some("auto")),
        twice,
        "the later, narrower mode is the launch's"
    );
}

#[test]
fn a_launch_that_named_no_mode_is_resumed_as_launched_or_in_plan() {
    // The vendor's settings chose the mode, and this harness does not read
    // them: only `plan` is known not to widen it.
    for launch in [
        &[][..],
        &["--model", "opus"],
        &["--allowedTools", "Bash", "Read"],
        &["--settings", "/tap.json"],
    ] {
        for history in ["default", "acceptEdits", "auto", "dontAsk"] {
            assert_eq!(
                with_permission_mode(resumed(launch), Some(history)),
                resumed(launch),
                "{launch:?} after a historical {history}"
            );
        }
        let mut in_plan = resumed(launch);
        let at = in_plan.len() - 2;
        in_plan.splice(at..at, words(&["--permission-mode", "plan"]));
        assert_eq!(
            with_permission_mode(resumed(launch), Some("plan")),
            in_plan,
            "{launch:?} in plan"
        );
    }
}

#[test]
fn a_launch_that_started_in_bypass_is_resumed_in_the_conversations_mode() {
    // CONTROL for the tests around this one: bypass was the mode the launch
    // started in, so every mode is within it, and it stays reachable.
    for launch in [
        &["--dangerously-skip-permissions"][..],
        &[
            "--dangerously-skip-permissions",
            "--allow-dangerously-skip-permissions",
        ],
        &["--permission-mode", "bypassPermissions"],
        &["--permission-mode=bypassPermissions"],
        &[
            "--permission-mode",
            "plan",
            "--permission-mode",
            "bypassPermissions",
        ],
    ] {
        for history in ["plan", "default", "acceptEdits", "auto", "dontAsk"] {
            assert_eq!(
                with_permission_mode(resumed(launch), Some(history)),
                words(&[
                    "--allow-dangerously-skip-permissions",
                    "--permission-mode",
                    history,
                    "--resume",
                    "ID"
                ]),
                "{launch:?} after a historical {history}"
            );
        }
        assert_eq!(
            with_permission_mode(resumed(launch), Some("bypassPermissions")),
            resumed(launch),
            "still in bypass: as launched"
        );
        assert_eq!(
            with_permission_mode(resumed(launch), None),
            resumed(launch),
            "no mode read: as launched"
        );
    }
}

#[test]
fn a_launch_that_only_left_bypass_reachable_is_held_to_the_mode_it_named() {
    // The reviewed defect: bypass allowed, a narrower mode named, and a
    // conversation whose history ends in a wider one. Reachable is not asked
    // for: the launch's own flags stand.
    for (launch, wider) in [
        (
            &[
                "--allow-dangerously-skip-permissions",
                "--permission-mode",
                "plan",
            ][..],
            &["default", "acceptEdits", "auto", "dontAsk"][..],
        ),
        (
            &[
                "--allow-dangerously-skip-permissions",
                "--permission-mode=default",
            ],
            &["acceptEdits", "auto", "dontAsk"],
        ),
        (
            &[
                "--permission-mode",
                "acceptEdits",
                "--allow-dangerously-skip-permissions",
            ],
            &["auto", "dontAsk"],
        ),
        // A mode named beside the skip flag is the narrower of the two words.
        (
            &[
                "--dangerously-skip-permissions",
                "--permission-mode",
                "plan",
            ],
            &["default", "acceptEdits", "auto", "dontAsk"],
        ),
        // The last `--permission-mode` is the launch's, a bypass before it
        // or not.
        (
            &[
                "--permission-mode",
                "bypassPermissions",
                "--permission-mode",
                "default",
            ],
            &["acceptEdits", "auto", "dontAsk"],
        ),
        // No mode named: the vendor's settings chose, and only `plan` is
        // known to be within them.
        (
            &["--allow-dangerously-skip-permissions"],
            &["default", "acceptEdits", "auto", "dontAsk"],
        ),
    ] {
        for history in wider {
            assert_eq!(
                with_permission_mode(resumed(launch), Some(history)),
                resumed(launch),
                "{launch:?} after a historical {history}"
            );
        }
    }
    // CONTROL: a mode within the one named is the conversation's own and is
    // carried, with bypass as reachable as the launch left it.
    for (launch, history) in [
        (
            &[
                "--allow-dangerously-skip-permissions",
                "--permission-mode",
                "auto",
            ][..],
            "acceptEdits",
        ),
        (
            &[
                "--allow-dangerously-skip-permissions",
                "--permission-mode",
                "default",
            ],
            "default",
        ),
        (
            &[
                "--dangerously-skip-permissions",
                "--permission-mode",
                "default",
            ],
            "plan",
        ),
        (&["--allow-dangerously-skip-permissions"], "plan"),
    ] {
        assert_eq!(
            with_permission_mode(resumed(launch), Some(history)),
            words(&[
                "--allow-dangerously-skip-permissions",
                "--permission-mode",
                history,
                "--resume",
                "ID"
            ]),
            "{launch:?} resumed in {history}"
        );
    }
    // CONTROL: a launch that could not reach bypass gains no way to it.
    assert_eq!(
        with_permission_mode(
            resumed(&[
                "--permission-mode",
                "bypassPermissions",
                "--permission-mode",
                "auto"
            ]),
            Some("plan")
        ),
        words(&["--permission-mode", "plan", "--resume", "ID"])
    );
}

#[test]
fn a_model_restart_replaces_the_launchs_model_and_nothing_else() {
    let on = |launch: &[&str], model: &str| with_model(resumed(launch), model).expect("placed");
    // No model named at launch: one is put in front of the resume pair.
    assert_eq!(
        on(&[], "claude-opus-5-5"),
        words(&["--model", "claude-opus-5-5", "--resume", "ID"])
    );
    // Both spellings of the launch's model are replaced, wherever they stand,
    // and every other word keeps its place.
    assert_eq!(
        on(
            &[
                "--model",
                "fable",
                "--dangerously-skip-permissions",
                "--model=sonnet",
                "first prompt",
                "--add-dir",
                "a",
                "b",
            ],
            "claude-opus-5-5"
        ),
        words(&[
            "--dangerously-skip-permissions",
            "--add-dir",
            "a",
            "b",
            "--model",
            "claude-opus-5-5",
            "--resume",
            "ID"
        ])
    );
    // CONTROL: `--model` as another option's value, and the fallback model,
    // are not the launch's model.
    assert_eq!(
        on(
            &[
                "--append-system-prompt",
                "--model",
                "--fallback-model",
                "sonnet"
            ],
            "opus"
        ),
        words(&[
            "--append-system-prompt",
            "--model",
            "--fallback-model",
            "sonnet",
            "--model",
            "opus",
            "--resume",
            "ID"
        ])
    );
    // Naming the launch's own model again gives the launch's line back.
    assert_eq!(
        on(&["--model", "fable"], "fable"),
        resumed(&["--model", "fable"])
    );
    // The permission rewrite that follows keeps the model where it is.
    assert_eq!(
        with_permission_mode(
            on(
                &["--dangerously-skip-permissions", "--model", "fable"],
                "opus"
            ),
            Some("plan")
        ),
        words(&[
            "--model",
            "opus",
            "--allow-dangerously-skip-permissions",
            "--permission-mode",
            "plan",
            "--resume",
            "ID"
        ])
    );
    // An option of unknown arity: nothing is guessed.
    assert_eq!(
        with_model(
            words(&["--future", "--model", "x", "--resume", "ID"]),
            "opus"
        ),
        None
    );
}

#[test]
fn the_launchs_model_is_its_last_model_flag() {
    let of = |launch: &[&str]| {
        let argv: Vec<String> = std::iter::once("claude")
            .chain(launch.iter().copied())
            .map(str::to_string)
            .collect();
        model_of(&argv).map(str::to_string)
    };
    assert_eq!(of(&[]), None);
    assert_eq!(of(&["--model", "fable"]).as_deref(), Some("fable"));
    assert_eq!(
        of(&["--model=claude-opus-5-5"]).as_deref(),
        Some("claude-opus-5-5")
    );
    assert_eq!(
        of(&["--model", "fable", "--model=sonnet", "hi"]).as_deref(),
        Some("sonnet")
    );
    // CONTROL: another option's value, a prompt, and a word past `--`.
    assert_eq!(of(&["--add-dir", "--model"]), None);
    assert_eq!(of(&["--fallback-model", "opus"]), None);
    assert_eq!(of(&["--", "--model", "opus"]), None);
}
