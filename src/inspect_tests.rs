// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;
use std::os::unix::ffi::OsStringExt as _;
use std::os::unix::fs::{PermissionsExt as _, symlink};
use std::sync::atomic::{AtomicU64, Ordering};

fn world() -> PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let root = std::env::temp_dir().join(".ubd");
    std::fs::create_dir_all(&root).unwrap();
    let path = root.join(format!(
        "claude-inspect-{}-{}-{}.noindex",
        std::process::id(),
        crate::supervise::now_ms(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir(&path).unwrap();
    path
}

fn inspect(text: &str, state: Option<&str>, auto: bool) -> (PathBuf, Report) {
    let root = world();
    let home = root.join("home");
    let config = root.join("selected config");
    std::fs::create_dir(&home).unwrap();
    std::fs::create_dir(&config).unwrap();
    std::fs::write(config.join("settings.json"), text).unwrap();
    if let Some(state) = state {
        std::fs::write(home.join(".claude.json"), state).unwrap();
    }
    let report = settings_defaults(&home, &config, auto).unwrap();
    assert_eq!(
        std::fs::read(config.join("settings.json")).unwrap(),
        text.as_bytes()
    );
    assert_eq!(
        std::fs::read_dir(&config).unwrap().count(),
        1,
        "no coordination/backup/temp entries"
    );
    (root, report)
}

#[test]
fn defaults_inspection_and_initialization_share_the_same_omission_decisions() {
    for (settings, state) in [
        ("{}", None),
        (
            r#"{"modelSettings":{"model":{"effortLevel":"medium"}}}"#,
            None,
        ),
        (r#"{"feedbackSurveyRate":0.25}"#, None),
        (
            "{}",
            Some(r#"{"autoCompactOffer":{"account":{"model":{"resolved":{"choice":"keep"}}}}}"#),
        ),
    ] {
        let (_, report) = inspect(settings, state, true);
        let changes: Vec<_> = report
            .findings
            .iter()
            .filter(|r| r.disposition == Disposition::Change)
            .map(|r| r.key.as_str())
            .collect();
        let written = crate::prime::settings_defaults(
            report.state.selected.parent().unwrap(),
            report.settings.selected.parent().unwrap(),
        )
        .unwrap();
        let crate::prime::DefaultsWrite::Added { keys, .. } = written else {
            panic!("expected unset defaults");
        };
        assert_eq!(changes, keys);
    }
}

#[test]
fn manual_effort_survey_and_keep_choices_are_owner_authored_findings() {
    let (_, report) = inspect(
        r#"{"effortLevel":"medium","feedbackSurveyRate":0.2}"#,
        Some(r#"{"autoCompactOffer":{"account":{"model":{"resolved":{"choice":"keep"}}}}}"#),
        true,
    );
    assert!(
        report
            .findings
            .iter()
            .all(|r| r.disposition == Disposition::Ok || r.key == "feedbackDrafts")
    );
    let response = report.response().unwrap();
    assert!(response.contains("set by hand"));
    assert!(response.contains("answered Keep"));
    let (_, report) = inspect(
        &format!(
            r#"{{"{}":true,"effortLevel":"medium"}}"#,
            crate::model_facts::TOP_EFFORT_KEY
        ),
        None,
        true,
    );
    let row = report
        .findings
        .iter()
        .find(|r| r.key == crate::model_facts::TOP_EFFORT_KEY)
        .unwrap();
    assert_eq!(row.disposition, Disposition::Change);
    assert!(matches!(row.remedy, Remedy::Edit { .. }));
    assert_eq!(row.expected.as_deref(), Some("unset"));
}

#[test]
fn inspection_preserves_truthiness_and_disabled_auto_prime() {
    for value in ["1", "true", "YES", " On "] {
        let text = format!(
            r#"{{"env":{{"CLAUDE_CODE_DISABLE_FEEDBACK_SURVEY":{}}}}}"#,
            Json::Str(value.into()).render()
        );
        let (_, report) = inspect(&text, None, false);
        assert!(
            report
                .findings
                .iter()
                .all(|r| r.disposition == Disposition::Ok)
        );
        assert_eq!(
            report
                .findings
                .iter()
                .find(|r| r.key == "feedbackSurveyRate")
                .unwrap()
                .class,
            Class::Preference
        );
    }
    for text in [
        r#"{"env":{"CLAUDE_CODE_DISABLE_FEEDBACK_SURVEY":false}}"#,
        r#"{"feedbackSurveyRate":0e0}"#,
    ] {
        let (_, report) = inspect(text, None, true);
        assert!(report.response().is_ok());
    }
}

#[test]
fn missing_paths_are_distinct_from_broken_symlinks_without_creating_anything() {
    let root = world();
    let home = root.join("home");
    let config = root.join("config");
    let report = settings_defaults(&home, &config, true).unwrap();
    assert_eq!(report.settings.status, PathStatus::Missing);
    assert_eq!(report.state.status, PathStatus::Missing);
    assert!(report.findings.is_empty());
    assert_eq!(std::fs::read_dir(&root).unwrap().count(), 0);
    symlink(root.join("absent"), &config).unwrap();
    let report = settings_defaults(&home, &config, true).unwrap();
    assert_eq!(report.settings.status, PathStatus::BrokenLink);
    assert_eq!(report.findings[0].disposition, Disposition::Unavailable);
    assert_eq!(std::fs::read_dir(&root).unwrap().count(), 1);
}

#[test]
fn selected_symlinks_and_non_utf8_paths_remain_lossless_read_only_observations() {
    let root = world();
    let name = if cfg!(target_os = "linux") {
        b"config-\xff\x1b".as_slice()
    } else {
        b"config-\x1b".as_slice()
    };
    let config = root.join(OsString::from_vec(name.to_vec()));
    let physical = root.join("physical");
    std::fs::create_dir(&config).unwrap();
    std::fs::write(&physical, "{}").unwrap();
    symlink(&physical, config.join("settings.json")).unwrap();
    let report = settings_defaults(&root, &config, false).unwrap();
    assert_eq!(report.settings.status, PathStatus::Present);
    assert_eq!(
        report.settings.physical,
        Some(std::fs::canonicalize(&physical).unwrap())
    );
    let doc = crate::json::parse(&report.response().unwrap()).unwrap();
    assert_eq!(
        doc.get("paths")
            .unwrap()
            .get("settings")
            .unwrap()
            .get("selected_hex")
            .unwrap()
            .as_str(),
        Some(hex(&config.join("settings.json")).unwrap().as_str())
    );
    assert_eq!(std::fs::read(&physical).unwrap(), b"{}");
    assert_eq!(std::fs::read_dir(&config).unwrap().count(), 1);
}

#[test]
fn malformed_settings_and_unreadable_keep_state_have_different_observations() {
    for text in ["{", "[]", r#"{"env":false}"#] {
        let (_, report) = inspect(text, None, true);
        assert_eq!(report.findings.len(), 1);
        assert_eq!(report.findings[0].disposition, Disposition::Unavailable);
    }
    let (_, report) = inspect("{}", Some("{"), true);
    assert!(report.findings.iter().any(|r| r.key == "autoCompactWindow"
        && r.disposition == Disposition::Ok
        && r.detail.contains("cannot be read")));
}

#[test]
fn an_unwritable_physical_directory_is_reported_only_when_a_default_would_be_added() {
    let (root, report) = inspect("{}", None, true);
    let config = report.settings.selected.parent().unwrap();
    std::fs::set_permissions(config, std::fs::Permissions::from_mode(0o500)).unwrap();
    let report = settings_defaults(&root, config, true).unwrap();
    assert_eq!(report.findings.len(), 1);
    assert_eq!(report.findings[0].disposition, Disposition::Unavailable);
    let report = settings_defaults(&root, config, false).unwrap();
    assert_eq!(report.findings.len(), 6);
    assert!(
        report
            .findings
            .iter()
            .all(|r| r.disposition == Disposition::Ok)
    );
    std::fs::set_permissions(config, std::fs::Permissions::from_mode(0o700)).unwrap();
}

#[test]
fn actual_read_count_and_special_file_refusals_are_bounded_and_do_not_write() {
    let (_, report) = inspect(&" ".repeat(1024 * 1024 + 1), None, true);
    assert_eq!(report.settings.status, PathStatus::Unavailable);
    assert_eq!(report.findings[0].disposition, Disposition::Unavailable);
    let root = world();
    let path = root.join("settings.json");
    let path_c = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
    // SAFETY: one fresh NUL-terminated private fixture pathname, with no reader/writer.
    assert_eq!(unsafe { libc::mkfifo(path_c.as_ptr(), 0o600) }, 0);
    let report = settings_defaults(&root, &root, true).unwrap();
    assert_eq!(report.settings.status, PathStatus::Unavailable);
    assert_eq!(std::fs::read_dir(&root).unwrap().count(), 1);
}

#[test]
fn response_is_closed_unique_bounded_and_has_no_presentable_terminal_controls() {
    let text = r#"{"modelSettings":{"evil\u001b[31m\n\u0085":{"effortLevel":"low"}}}"#;
    let (_, mut report) = inspect(text, None, true);
    let encoded = report.response().unwrap();
    let parsed = crate::json::parse(&encoded).unwrap();
    assert_eq!(parsed.get("schema").unwrap().as_u64(), Some(1));
    assert!(report.findings.iter().all(|r| single_line(&r.detail)));
    assert!(
        report
            .findings
            .iter()
            .any(|r| r.detail.contains("\\u001b") && r.detail.contains("\\u{85}"))
    );
    report.findings.push(report.findings[0].clone());
    assert!(report.response().is_err());
    report.findings.pop();
    report.findings[0].detail = "\x1b[31m".into();
    assert!(report.response().is_err());
    report.findings[0].detail = "x".repeat(MAX_RESPONSE_BYTES);
    assert!(report.response().is_err());
}

#[test]
fn response_refuses_incoherent_path_outcomes_and_keeps_survey_interpretation_in_the_owner() {
    let (_, mut report) = inspect(
        r#"{"env":{"CLAUDE_CODE_DISABLE_FEEDBACK_SURVEY":"no"}}"#,
        None,
        true,
    );
    let row = report
        .findings
        .iter()
        .find(|row| row.key == "env.CLAUDE_CODE_DISABLE_FEEDBACK_SURVEY")
        .unwrap();
    assert_eq!(row.class, Class::Person);
    assert_eq!(row.disposition, Disposition::Ok);
    assert!(row.detail.contains("surveys") && row.detail.contains("recognizes"));
    report.settings.physical = None;
    assert!(report.response().is_err());
    report.settings.status = PathStatus::Missing;
    assert!(report.response().is_ok());
    report.settings.physical = Some(report.settings.selected.clone());
    assert!(report.response().is_err());
    report.settings.status = PathStatus::BrokenLink;
    assert!(report.response().is_err());
}

#[test]
fn request_grammar_is_exact_and_does_not_fall_back_to_an_ambient_home() {
    for args in [
        vec!["inspect"],
        vec!["inspect", "other"],
        vec![
            "inspect",
            "settings-defaults",
            "--home",
            ".",
            "--config",
            "/tmp",
            "--auto-prime=1",
        ],
    ] {
        assert!(
            entry(&args.into_iter().map(OsString::from).collect::<Vec<_>>())
                .unwrap()
                .is_err()
        );
    }
    assert!(entry(&[OsString::from("a vendor prompt")]).is_none());
}

#[test]
fn native_hex_does_not_require_utf8_or_a_filesystem_that_can_create_the_name() {
    let path = PathBuf::from(OsString::from_vec(
        b"/config-\xff\x1b/settings.json".to_vec(),
    ));
    assert_eq!(
        hex(&path).unwrap(),
        "2f636f6e6669672dff1b2f73657474696e67732e6a736f6e"
    );
    let (_, report) = inspect(
        r#"{"effortLevel":"medium","effortLevel":"xhigh"}"#,
        None,
        true,
    );
    assert_eq!(
        report
            .findings
            .iter()
            .find(|r| r.key == "effortLevel")
            .unwrap()
            .class,
        Class::Preference
    );
}
