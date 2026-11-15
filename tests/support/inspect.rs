// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Actual inspection CLI controls. No vendor is started and every world is retained.

use super::*;
use claude_code_alab::json::Value;
use std::os::unix::ffi::{OsStrExt as _, OsStringExt as _};

fn run(s: &Scenario, mut command: Command) -> (Option<Status>, Vec<u8>) {
    let output = s.root.join("inspection-stdout");
    command
        .stdin(Stdio::null())
        .stdout(Stdio::from(std::fs::File::create(&output).unwrap()))
        .stderr(Stdio::from(
            std::fs::File::create(s.root.join("inspection-stderr")).unwrap(),
        ))
        .process_group(0);
    let child = command.spawn().expect("real inspection CLI");
    s.guard_group(child_pid(&child));
    let ended = reap(child, Duration::from_secs(60));
    let mut bytes = Vec::new();
    std::fs::File::open(output)
        .unwrap()
        .take(claude_code_alab::inspect::MAX_RESPONSE_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .unwrap();
    (ended, bytes)
}

fn command(s: &Scenario, home: &Path, config: &Path) -> Command {
    let mut cmd = s.command(&[]);
    cmd.args([
        OsString::from("inspect"),
        "settings-defaults".into(),
        "--home".into(),
        home.into(),
        "--config".into(),
        config.into(),
        "--auto-prime=1".into(),
    ]);
    cmd
}

fn parsed(bytes: &[u8]) -> Value {
    assert_eq!(bytes.last(), Some(&b'\n'));
    assert!(bytes.len() <= claude_code_alab::inspect::MAX_RESPONSE_BYTES);
    claude_code_alab::json::parse(std::str::from_utf8(bytes).unwrap())
        .expect("strict one-document response")
}

pub(super) fn is_read_only_explicit_and_keeps_native_entry_guards() {
    let s = Scenario::new("inspection-entry");
    let name = if cfg!(target_os = "linux") {
        b"selected-\xff\x1b".as_slice()
    } else {
        b"selected-\x1b".as_slice()
    };
    let config = s.root.join(OsString::from_vec(name.to_vec()));
    std::fs::create_dir(&config).unwrap();
    let settings = config.join("settings.json");
    let bytes =
        br#"{"modelSettings":{"chosen":{"effortLevel":"medium"}},"feedbackSurveyRate":0.2}"#;
    std::fs::write(&settings, bytes).unwrap();
    std::fs::write(
        s.home.join(".claude/settings.json"),
        "ambient must not be read",
    )
    .unwrap();
    let mut marked = command(&s, &s.home, &config);
    marked.env(
        claude_code_alab::cli::PARENT_VAR,
        std::process::id().to_string(),
    );
    assert_eq!(run(&s, marked).0, Some(Status::Exited(127)));
    let mut nested = command(&s, &s.home, &config);
    nested.env(claude_code_alab::cli::DEPTH_VAR, u32::MAX.to_string());
    assert_eq!(run(&s, nested).0, Some(Status::Exited(127)));
    for args in [
        vec!["--alab-inspects=claude:settings-defaults"],
        vec!["--alab-inspects=claude:settings-defaults", "extra"],
        vec!["--alab-inspects=claude:legacy-hooks"],
    ] {
        let expected = if args.len() == 1 && args[0].ends_with(":settings-defaults") {
            0
        } else {
            1
        };
        let mut query = s.command(&args);
        query.env(
            claude_code_alab::cli::PARENT_VAR,
            std::process::id().to_string(),
        );
        let (ended, output) = run(&s, query);
        assert_eq!(ended, Some(Status::Exited(expected)));
        assert!(output.is_empty());
    }
    let (ended, output) = run(&s, command(&s, &s.home, &config));
    assert_eq!(ended, Some(Status::Exited(0)), "{}", s.report());
    let doc = parsed(&output);
    let Value::Object(fields) = &doc else {
        panic!("document");
    };
    assert_eq!(
        fields.keys().map(String::as_str).collect::<Vec<_>>(),
        ["auto_prime", "findings", "operation", "paths", "schema"]
    );
    assert_eq!(
        doc.get("operation").unwrap().as_str(),
        Some("settings-defaults")
    );
    assert_eq!(doc.get("schema").unwrap().as_u64(), Some(1));
    let path = doc.get("paths").unwrap().get("settings").unwrap();
    let selected = path.get("selected_hex").unwrap().as_str().unwrap();
    let want: String = settings
        .as_os_str()
        .as_bytes()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    assert_eq!(selected, want);
    let Value::Array(rows) = doc.get("findings").unwrap() else {
        panic!("findings");
    };
    assert_eq!(rows.len(), 6);
    assert!(rows.iter().all(|r| {
        let Value::Object(fields) = r else {
            return false;
        };
        fields.keys().map(String::as_str).collect::<Vec<_>>()
            == [
                "class",
                "detail",
                "disposition",
                "expected",
                "key",
                "label",
                "remedy",
            ]
            && ["label", "detail"].into_iter().all(|key| {
                !r.get(key)
                    .unwrap()
                    .as_str()
                    .unwrap()
                    .chars()
                    .any(char::is_control)
            })
    }));
    assert_eq!(std::fs::read(&settings).unwrap(), bytes);
    assert_eq!(std::fs::read_dir(&config).unwrap().count(), 1);
    assert!(s.starts().is_empty());
}

pub(super) fn reports_closed_bounded_unavailable_observations() {
    let s = Scenario::new("inspection-unavailable");
    let config = s.root.join("config");
    std::fs::create_dir(&config).unwrap();
    let settings = config.join("settings.json");
    for text in ["{", "[]", &"x".repeat(1024 * 1024 + 1)] {
        std::fs::write(&settings, text).unwrap();
        let (ended, output) = run(&s, command(&s, &s.home, &config));
        assert_eq!(ended, Some(Status::Exited(0)));
        let Value::Array(rows) = parsed(&output).get("findings").unwrap().clone() else {
            panic!("findings");
        };
        assert_eq!(rows.len(), 1);
        assert_eq!(
            rows[0].get("disposition").unwrap().as_str(),
            Some("unavailable")
        );
        assert_eq!(std::fs::read(&settings).unwrap(), text.as_bytes());
        assert_eq!(std::fs::read_dir(&config).unwrap().count(), 1);
    }
    std::fs::remove_file(&settings).unwrap();
    std::os::unix::fs::symlink(config.join("absent"), &settings).unwrap();
    let (ended, output) = run(&s, command(&s, &s.home, &config));
    assert_eq!(ended, Some(Status::Exited(0)));
    assert_eq!(
        parsed(&output)
            .get("paths")
            .unwrap()
            .get("settings")
            .unwrap()
            .get("status")
            .unwrap()
            .as_str(),
        Some("broken-link")
    );
    assert!(s.starts().is_empty());
}
