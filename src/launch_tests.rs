// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

fn os(words: &[&str]) -> Vec<OsString> {
    words.iter().map(OsString::from).collect()
}

#[test]
fn the_live_launch_scan_can_distinguish_a_separator_value_from_the_literal_tail() {
    let args = os(&[
        "--append-system-prompt",
        "--",
        "--model=sonnet",
        "--resume",
        "conversation",
        "--",
        "--model=literal",
    ]);
    let option_end = args.len();
    assert_eq!(
        override_model(args, "opus".into(), option_end).unwrap(),
        os(&[
            "--model",
            "opus",
            "--append-system-prompt",
            "--",
            "--resume",
            "conversation",
            "--",
            "--model=literal",
        ])
    );
}

#[test]
fn only_the_managed_envelope_selects_launch_policy() {
    let managed = |words| {
        let crate::cli::Command::Run { managed, .. } = crate::cli::parse(os(words)).unwrap() else {
            panic!("expected a vendor launch");
        };
        managed
    };
    assert!(managed(&["run", "--"]));
    assert!(managed(&["run", "--model", "opus", "--"]));
    assert!(managed(&["launch", "--"]));
    assert!(managed(&["--alab-poll-ms=50", "run", "--", "--version"]));
    assert!(!managed(&["run"]));
    assert!(!managed(&["--model", "run", "--"]));
    assert!(!managed(&["run", "a task"]));
    assert!(!managed(&["launch", "a task"]));
    assert!(!managed(&["--", "launch", "--"]));
}

#[test]
fn renderer_settings_do_not_require_home_when_config_is_explicit() {
    assert_eq!(
        renderer_config(Some("/chosen/config".into()), || panic!(
            "explicit config must not consult HOME"
        )),
        Some(PathBuf::from("/chosen/config"))
    );
    assert_eq!(
        renderer_config(Some("".into()), || Some("/a/home".into())),
        Some(PathBuf::from("/a/home/.claude"))
    );
    assert_eq!(renderer_config(None, || None), None);
}

#[test]
fn informational_queries_are_recognized_only_before_the_literal_boundary() {
    for words in [
        &["--version"][..],
        &["-v"],
        &["install", "--help"],
        &["--help", "x"],
        &["-h"],
    ] {
        assert!(informational(&os(words)), "{words:?}");
    }
    for words in [
        &["-p", "a task"][..],
        &["--", "--help"],
        &["--", "--version"],
    ] {
        assert!(!informational(&os(words)), "{words:?}");
    }
}

#[test]
fn bypass_switches_are_removed_only_before_the_literal_boundary() {
    assert_eq!(
        switches(os(&[
            "--no-harness",
            "x",
            "--no-alab-harness",
            "--",
            "--no-harness"
        ])),
        (os(&["x", "--", "--no-harness"]), true)
    );
    assert_eq!(
        switches(os(&["--", "--no-alab-harness"])),
        (os(&["--", "--no-alab-harness"]), false)
    );
}

#[test]
fn opaque_vendor_words_preserve_launcher_switch_boundaries() {
    use std::os::unix::ffi::OsStringExt as _;

    let opaque = OsString::from_vec(vec![0xff]);
    let opaque_option = OsString::from_vec(b"--unknown-\xff".to_vec());
    let opaque_inline = OsString::from_vec(b"--model=\xff=tail".to_vec());
    let opaque_value = OsString::from_vec(b"--no-harness\xff".to_vec());
    let no = OsString::from("--no-harness");
    let alias = OsString::from("--no-alab-harness");
    let cases = [
        (vec![no.clone(), opaque.clone()], vec![opaque.clone()], true),
        (vec![opaque.clone(), no.clone()], vec![opaque.clone()], true),
        (
            vec!["--system-prompt".into(), opaque.clone(), no.clone()],
            vec!["--system-prompt".into(), opaque.clone()],
            true,
        ),
        (
            vec![
                "--system-prompt".into(),
                no.clone(),
                opaque.clone(),
                alias.clone(),
            ],
            vec!["--system-prompt".into(), no.clone(), opaque.clone()],
            true,
        ),
        (
            vec!["--system-prompt".into(), opaque_value.clone(), no.clone()],
            vec!["--system-prompt".into(), opaque_value.clone()],
            true,
        ),
        (
            vec![
                "--add-dir".into(),
                opaque.clone(),
                "another".into(),
                alias.clone(),
            ],
            vec!["--add-dir".into(), opaque.clone(), "another".into()],
            true,
        ),
        (
            vec![
                "--add-dir".into(),
                no.clone(),
                opaque.clone(),
                alias.clone(),
            ],
            vec!["--add-dir".into(), no.clone(), opaque.clone()],
            true,
        ),
        (
            vec!["--prompt-suggestions".into(), opaque.clone(), alias.clone()],
            vec!["--prompt-suggestions".into(), opaque.clone()],
            true,
        ),
        (
            vec![opaque_inline.clone(), alias.clone()],
            vec![opaque_inline],
            true,
        ),
        (
            vec![
                no.clone(),
                "--".into(),
                opaque.clone(),
                opaque_option.clone(),
                alias.clone(),
            ],
            vec![
                "--".into(),
                opaque.clone(),
                opaque_option.clone(),
                alias.clone(),
            ],
            true,
        ),
    ];
    for (args, expected, disabled) in cases {
        assert_eq!(switches(args.clone()), (expected, disabled), "{args:?}");
    }

    // Unknown names retain the entire input, even after a recognized launcher flag.
    for unknown in [
        OsString::from("--future"),
        opaque_option,
        OsString::from_vec(b"-\xff".to_vec()),
        OsString::from_vec(b"--future=\xff".to_vec()),
        OsString::from_vec(b"--no-harness=\xff".to_vec()),
    ] {
        let args = vec![no.clone(), unknown, opaque.clone()];
        assert_eq!(switches(args.clone()), (args.clone(), false), "{args:?}");
    }
    // A required value does not move the launcher's unconditional literal cut.
    let args = vec!["--system-prompt".into(), "--".into(), no, opaque];
    assert_eq!(switches(args.clone()), (args, false));
}

#[test]
fn managed_permissions_preserve_explicit_posture_and_unknown_arity() {
    for args in [
        vec!["--permission-mode", "plan"],
        vec!["--permission-mode=plan"],
        vec!["--tools", "Read"],
        vec!["--allowedTools=Bash"],
        vec!["-rconversation"],
        vec!["--continue"],
        vec!["--settings", "{}"],
        vec!["--model", "opus", "--permission-mode", "plan"],
        vec!["--model", "opus", "--resume", "conversation"],
        vec!["--model", "opus", "--settings", "{}"],
        vec!["--version"],
        vec!["--future", "value"],
        vec!["a task", "--future"],
        vec!["update"],
        vec!["upgrade"],
        vec!["install"],
    ] {
        assert_eq!(permission_args(os(&args)), os(&args), "{args:?}");
    }
    assert_eq!(permission_args(vec![]), os(&[BYPASS]));
    assert_eq!(
        permission_args(os(&["-p", "a task"])),
        os(&[BYPASS, "-p", "a task"])
    );
    assert_eq!(
        permission_args(os(&["a task", "--", "--permission-mode=plan"])),
        os(&[BYPASS, "a task", "--", "--permission-mode=plan"])
    );
}

#[test]
fn managed_defaults_follow_known_option_values_instead_of_treating_them_as_flags() {
    for args in [
        vec!["--model", "opus"],
        vec!["--model=opus", "a task"],
        vec!["--model", "opus", "--effort", "high", "a task"],
        vec!["--system-prompt", "--permission-mode"],
        vec!["--system-prompt", "update"],
    ] {
        let expected = std::iter::once(BYPASS)
            .chain(args.iter().copied())
            .collect::<Vec<_>>();
        assert_eq!(permission_args(os(&args)), os(&expected), "{args:?}");
    }
    assert_eq!(
        permission_args(os(&["--model", "opus", "update"])),
        os(&["--model", "opus", "update"])
    );
}

#[test]
fn explicit_yolo_is_expanded_once_without_touching_literal_arguments() {
    assert_eq!(
        permission_args(os(&["--yolo", "--model", "opus", "--yolo"])),
        os(&[BYPASS, "--model", "opus"])
    );
    assert_eq!(
        permission_args(os(&["--yolo", BYPASS, "--", "--yolo"])),
        os(&[BYPASS, "--", "--yolo"])
    );
    assert_eq!(
        permission_args(os(&["--permission-mode=plan", "--yolo"])),
        os(&["--permission-mode=plan", BYPASS])
    );
}

#[test]
fn known_option_values_never_become_launcher_switches_or_information_queries() {
    for value in [
        "--yolo",
        "--no-harness",
        "--no-alab-harness",
        "--help",
        "--version",
        "--dangerously-skip-permissions",
    ] {
        let args = os(&["--system-prompt", value]);
        assert!(!informational(&args), "{value}");
        assert_eq!(switches(args.clone()), (args.clone(), false), "{value}");
        assert_eq!(
            permission_args(args),
            os(&[BYPASS, "--system-prompt", value]),
            "{value}"
        );
    }
    assert_eq!(
        permission_args(os(&["--system-prompt", "--yolo", "--yolo"])),
        os(&["--system-prompt", "--yolo", BYPASS])
    );
    assert_eq!(
        switches(os(&["--system-prompt", "--no-harness", "--no-harness"])),
        (os(&["--system-prompt", "--no-harness"]), true)
    );
    assert!(!informational(&os(&["--future", "--help"])));
    assert_eq!(
        switches(os(&["--future", "--no-harness"])),
        (os(&["--future", "--no-harness"]), false)
    );
    for args in [&["--future", "--yolo"][..], &["--yolo=1"]] {
        assert_eq!(permission_args(os(args)), os(args), "{args:?}");
    }
    assert!(update_action(&os(&["update", "--system-prompt", "--help"])).is_err());
    assert_eq!(
        update_action(&os(&["install", "latest", "--force", "--help"])),
        Ok(false)
    );
}

#[test]
fn renderer_keeps_empty_environment_choices_and_real_settings_keys() {
    assert!(!renderer_default(Some(OsStr::new("")), None, None));
    assert!(!renderer_default(None, Some(OsStr::new("0")), None));
    for settings in [r#"{"tui": "classic"}"#, r#"{"tui":null}"#] {
        assert!(!renderer_default(None, None, Some(settings)));
    }
    for settings in [
        r#"{"message":"tui"}"#,
        r#"{"nested":{"tui":false}}"#,
        "not JSON",
    ] {
        assert!(renderer_default(None, None, Some(settings)));
    }
    assert!(renderer_default(None, None, None));
}

#[test]
fn account_routing_preserves_argument_boundaries_and_execs_the_exact_vendor() {
    let args = os(&["--model", "an opus", "", "a'quote", "--", "--no-harness"]);
    let exe = Path::new("/store/claude/42/bin/claude");
    let command = vendor_command_for(exe, &args, Some(Path::new("/host/aterm")));
    assert_eq!(command.get_program(), "/host/aterm");
    assert_eq!(
        command.get_args().collect::<Vec<_>>(),
        os(&[
            "harness",
            "accounts",
            "token-run",
            "--auto",
            "--",
            "/store/claude/42/bin/claude"
        ])
        .iter()
        .chain(args.iter())
        .map(OsString::as_os_str)
        .collect::<Vec<_>>()
    );
    let plain = vendor_command_for(exe, &args, None);
    assert_eq!(plain.get_program(), exe);
    assert_eq!(
        plain.get_args().collect::<Vec<_>>(),
        args.iter().map(OsString::as_os_str).collect::<Vec<_>>()
    );
}

#[test]
fn host_paths_are_absolute_executable_files() {
    assert_eq!(
        host_path(Some(OsStr::new("/bin/sh"))),
        Some(PathBuf::from("/bin/sh"))
    );
    assert_eq!(host_path(Some(OsStr::new("bin/sh"))), None);
    assert_eq!(host_path(Some(OsStr::new("/bin"))), None);
    assert_eq!(host_path(None), None);
}

#[test]
fn managed_update_rejects_extra_targets_without_silently_dropping_words() {
    for words in [
        &["update"][..],
        &["upgrade"],
        &["install"],
        &["install", "latest", "--force"],
        &["install", "--force", "latest"],
    ] {
        assert_eq!(update_action(&os(words)), Ok(true));
    }
    for words in [
        &["update", "--unexpected"][..],
        &["upgrade", "latest"],
        &["install", "stable"],
        &["install", "2.1.200"],
        &["install", "latest", "latest"],
        &["install", "--force", "--force"],
    ] {
        assert!(update_action(&os(words)).is_err(), "{words:?}");
    }
    for words in [
        &["--model", "opus"][..],
        &["install", "--help"],
        &["update", "-h"],
    ] {
        assert_eq!(update_action(&os(words)), Ok(false));
    }
}
