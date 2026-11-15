// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Original aterm defaults and concurrent-save controls, exercising their new owner.

use super::*;
use std::os::unix::fs::DirBuilderExt as _;
use std::sync::atomic::{AtomicU64, Ordering};

#[test]
fn the_command_requires_both_explicit_homes_and_does_not_capture_vendor_arguments() {
    let words = |values: &[&str]| {
        values
            .iter()
            .map(std::ffi::OsString::from)
            .collect::<Vec<_>>()
    };
    assert!(parse(&words(&["launch", "--", "prime", "settings-defaults"])).is_none());
    for bad in [
        vec!["prime"],
        vec!["prime", "settings-defaults", "--home", "/home"],
        vec![
            "prime",
            "settings-defaults",
            "--home",
            "relative",
            "--config",
            "/config",
        ],
        vec![
            "prime",
            "settings-defaults",
            "--home",
            "/home",
            "--config",
            "relative",
        ],
        vec![
            "prime",
            "settings-defaults",
            "--home",
            "/home",
            "--config",
            "/config",
            "extra",
        ],
    ] {
        assert!(parse(&words(&bad)).unwrap().is_err(), "{bad:?}");
    }
    assert_eq!(
        parse(&words(&[
            "prime",
            "settings-defaults",
            "--home",
            "/chosen/home",
            "--config",
            "/chosen/config"
        ])),
        Some(Ok((
            Operation::SettingsDefaults,
            "/chosen/home".into(),
            "/chosen/config".into()
        )))
    );
}

#[test]
fn the_selected_configuration_uses_only_the_selected_homes_compaction_decision() {
    let home = Temp::new().unwrap();
    let selected = home.path().join("configured");
    std::fs::create_dir(&selected).unwrap();
    std::fs::write(
        home.path().join(".claude.json"),
        r#"{"autoCompactOffer":{"account":{"model":{"resolved":{"choice":"keep"}}}}}"#,
    )
    .unwrap();
    let DefaultsWrite::Added { keys, .. } = settings_defaults(home.path(), &selected).unwrap()
    else {
        panic!("unset defaults were not initialized")
    };
    assert_eq!(keys.len(), 5);
    assert!(!keys.contains(&CLAUDE_COMPACT_WINDOW_KEY));
    assert!(!home.path().join(".claude").exists());
    assert!(selected.join("settings.json").is_file());
}

#[test]
fn an_owned_initializer_blocks_aliases_until_its_lock_is_released() {
    let home = Temp::new().unwrap();
    let config = home.path().join("config");
    let alias = home.path().join("alias");
    std::fs::create_dir(&config).unwrap();
    std::os::unix::fs::symlink(&config, &alias).unwrap();
    let settings = config.join("settings.json");
    let mine = b"{\"model\":\"opus\"}\n";
    std::fs::write(&settings, mine).unwrap();
    let hold = write::Lock::acquire(&settings).unwrap().unwrap();
    let Err(error) = write::Lock::acquire(&alias.join("settings.json")) else {
        panic!("the same target admitted overlapping initializers")
    };
    assert_eq!(error.kind(), std::io::ErrorKind::WouldBlock);
    assert!(settings_defaults(home.path(), &alias).is_err());
    assert_eq!(std::fs::read(&settings).unwrap(), mine);
    assert!(backups(&config).is_empty());
    drop(hold);
    assert!(matches!(
        settings_defaults(home.path(), &alias),
        Ok(DefaultsWrite::Added { .. })
    ));
}

#[test]
fn a_retargeted_settings_alias_does_not_move_the_owned_lock_to_another_file() {
    let home = Temp::new().unwrap();
    let config = home.path().join("config");
    std::fs::create_dir(&config).unwrap();
    let a = home.path().join("a.json");
    let b = home.path().join("b.json");
    let selected = config.join("settings.json");
    std::fs::write(&a, "{\"which\":\"a\"}").unwrap();
    std::fs::write(&b, "{\"which\":\"b\"}").unwrap();
    std::os::unix::fs::symlink(&a, &selected).unwrap();
    let to = b.clone();
    let wrote = saving_after_read(
        move |path| {
            std::fs::remove_file(path).unwrap();
            std::os::unix::fs::symlink(&to, path).unwrap();
        },
        || settings_defaults(home.path(), &config),
    );
    assert!(wrote.is_err());
    assert_eq!(std::fs::read_to_string(&a).unwrap(), "{\"which\":\"a\"}");
    assert_eq!(std::fs::read_to_string(&b).unwrap(), "{\"which\":\"b\"}");
    assert!(backups(&config).is_empty());
}

#[test]
fn an_alias_aba_cannot_redirect_a_physical_write_or_its_backup_to_an_unlocked_target() {
    let home = Temp::new().unwrap();
    let config = home.path().join("config");
    std::fs::create_dir(&config).unwrap();
    let a = home.path().join("a.json");
    let b = home.path().join("b.json");
    let selected = config.join("settings.json");
    let original_a = "{\"which\":\"a\"}";
    let original_b = "{\"which\":\"b\"}";
    std::fs::write(&a, original_a).unwrap();
    std::fs::write(&b, original_b).unwrap();
    std::os::unix::fs::symlink(&a, &selected).unwrap();
    let restore_alias = selected.clone();
    let restore_to = a.clone();
    let move_to = b.clone();
    let wrote = write::before_final_check(
        move || {
            std::fs::remove_file(&restore_alias).unwrap();
            std::os::unix::fs::symlink(&restore_to, &restore_alias).unwrap();
        },
        || {
            saving_after_read(
                move |alias| {
                    std::fs::remove_file(alias).unwrap();
                    std::os::unix::fs::symlink(&move_to, alias).unwrap();
                },
                || settings_defaults(home.path(), &config),
            )
        },
    );
    let DefaultsWrite::Added {
        backup: Some(backup),
        ..
    } = wrote.unwrap()
    else {
        panic!("the original selected target was not initialized")
    };
    assert_eq!(std::fs::read_link(&selected).unwrap(), a);
    assert_eq!(std::fs::read_to_string(&b).unwrap(), original_b);
    assert_eq!(std::fs::read_to_string(backup).unwrap(), original_a);
    let after = std::fs::read_to_string(&a).unwrap();
    let doc = settings_json::Json::parse(&after).unwrap();
    assert_eq!(
        doc.get("which").and_then(settings_json::Json::as_str),
        Some("a")
    );
    assert!(doc.get("effortLevel").is_some());
}

#[test]
fn large_settings_are_preserved_and_over_budget_settings_are_refused_unchanged() {
    let home = Temp::new().unwrap();
    let config = home.path().join("config");
    std::fs::create_dir(&config).unwrap();
    let path = config.join("settings.json");
    let text = format!("{{\"large\":\"{}\"}}", "x".repeat(2 * 1024 * 1024));
    std::fs::write(&path, &text).unwrap();
    let DefaultsWrite::Added {
        backup: Some(backup),
        ..
    } = settings_defaults(home.path(), &config).unwrap()
    else {
        panic!("an accepted large file was not backed up")
    };
    assert_eq!(std::fs::read_to_string(&backup).unwrap(), text);
    assert!(
        std::fs::read_to_string(&path)
            .unwrap()
            .contains(&"x".repeat(2 * 1024 * 1024))
    );
    let too_big = vec![b'x'; 16 * 1024 * 1024 + 1];
    std::fs::write(&path, &too_big).unwrap();
    let before = backups(&config);
    assert!(settings_defaults(home.path(), &config).is_err());
    assert_eq!(std::fs::read(&path).unwrap(), too_big);
    assert_eq!(backups(&config), before);
}

pub(super) struct Temp(PathBuf);
impl Temp {
    pub(super) fn new() -> std::io::Result<Self> {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let root = std::env::temp_dir().join(".ubd");
        std::fs::create_dir_all(&root)?;
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(std::io::Error::other)?
            .as_nanos();
        let path = root.join(format!(
            "claude-prime-{}-{stamp}-{}.noindex",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::DirBuilder::new().mode(0o700).create(&path)?;
        Ok(Self(path))
    }
    pub(super) fn path(&self) -> &Path {
        &self.0
    }
}
impl Drop for Temp {
    fn drop(&mut self) {
        if std::thread::panicking() {
            eprintln!("preserved failed priming world: {}", self.0.display());
        } else {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
}

#[test]
fn replacement_of_the_named_lock_cannot_admit_a_write_under_the_old_lock() {
    use std::os::unix::fs::OpenOptionsExt as _;
    let home = Temp::new().unwrap();
    let config = home.path().join("config");
    std::fs::create_dir(&config).unwrap();
    let settings = config.join("settings.json");
    let original = b"{\"model\":\"before\"}";
    std::fs::write(&settings, original).unwrap();
    let lock = config.join(".settings.json.alab-prime.lock");
    let held = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let keep = std::sync::Arc::clone(&held);
    let wrote = write::before_final_check(
        move || {
            let mut keep = keep.lock().unwrap();
            if !keep.is_empty() {
                return;
            }
            std::fs::rename(&lock, lock.with_extension("original-lock")).unwrap();
            let current = std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&lock)
                .unwrap();
            current.try_lock().unwrap();
            keep.push(current);
        },
        || settings_defaults(home.path(), &config),
    );
    assert!(wrote.is_err(), "a retired lock granted a write: {wrote:?}");
    assert_eq!(std::fs::read(&settings).unwrap(), original);
    assert!(backups(&config).is_empty());
    assert_eq!(held.lock().unwrap().len(), 1);
}

#[test]
fn a_replaced_parent_never_receives_staging_or_cleanup_from_the_original_directory() {
    use std::os::unix::fs::OpenOptionsExt as _;
    let home = Temp::new().unwrap();
    let config = home.path().join("config");
    let moved = home.path().join("original-config");
    std::fs::create_dir(&config).unwrap();
    let settings = config.join("settings.json");
    let original = b"{\"model\":\"before\"}";
    std::fs::write(&settings, original).unwrap();
    let swapped = config.clone();
    let saved = moved.clone();
    let current_lock = std::sync::Arc::new(std::sync::Mutex::new(None));
    let keep = std::sync::Arc::clone(&current_lock);
    let inspect = config.clone();
    let wrote = write::before_final_check(
        move || {
            let names: Vec<_> = std::fs::read_dir(&inspect)
                .unwrap()
                .map(|entry| entry.unwrap().file_name())
                .collect();
            assert_eq!(
                names.len(),
                2,
                "staging escaped into the replacement directory: {names:?}"
            );
        },
        || {
            saving_after_read(
                move |_| {
                    if keep.lock().unwrap().is_some() {
                        return;
                    }
                    std::fs::rename(&swapped, &saved).unwrap();
                    std::fs::create_dir(&swapped).unwrap();
                    std::fs::hard_link(saved.join("settings.json"), swapped.join("settings.json"))
                        .unwrap();
                    let lock = std::fs::OpenOptions::new()
                        .read(true)
                        .write(true)
                        .create_new(true)
                        .mode(0o600)
                        .open(swapped.join(".settings.json.alab-prime.lock"))
                        .unwrap();
                    lock.try_lock().unwrap();
                    *keep.lock().unwrap() = Some(lock);
                },
                || settings_defaults(home.path(), &config),
            )
        },
    );
    assert!(wrote.is_err());
    assert_eq!(std::fs::read(&settings).unwrap(), original);
    assert_eq!(
        std::fs::read(moved.join("settings.json")).unwrap(),
        original
    );
    assert_eq!(std::fs::read_dir(config).unwrap().count(), 2);
}

#[test]
fn staged_old_private_bytes_never_borrow_a_replacement_files_public_mode() {
    use std::os::unix::fs::PermissionsExt as _;
    let home = Temp::new().unwrap();
    let config = home.path().join("config");
    std::fs::create_dir(&config).unwrap();
    let settings = config.join("settings.json");
    std::fs::write(
        &settings,
        r#"{"model":"before","private":"old-private-value"}"#,
    )
    .unwrap();
    std::fs::set_permissions(&settings, std::fs::Permissions::from_mode(0o600)).unwrap();
    let inspect = config.clone();
    let mut saved = false;
    let wrote = write::before_final_check(
        move || {
            for entry in std::fs::read_dir(&inspect).unwrap() {
                let entry = entry.unwrap();
                let bytes = std::fs::read(entry.path()).unwrap();
                if String::from_utf8_lossy(&bytes).contains("old-private-value") {
                    assert_eq!(
                        entry.metadata().unwrap().permissions().mode() & 0o077,
                        0,
                        "private bytes became public in {:?}",
                        entry.file_name()
                    );
                }
            }
        },
        || {
            saving_after_read(
                move |path| {
                    if saved {
                        return;
                    }
                    saved = true;
                    let replacement = path.with_extension("replacement");
                    std::fs::write(&replacement, r#"{"model":"after","public":true}"#).unwrap();
                    std::fs::set_permissions(&replacement, std::fs::Permissions::from_mode(0o644))
                        .unwrap();
                    std::fs::rename(replacement, path).unwrap();
                },
                || settings_defaults(home.path(), &config),
            )
        },
    );
    assert!(matches!(wrote, Ok(DefaultsWrite::Added { .. })));
    assert!(
        std::fs::read_to_string(&settings)
            .unwrap()
            .contains("\"after\"")
    );
    assert_eq!(
        std::fs::metadata(settings).unwrap().permissions().mode() & 0o777,
        0o644
    );
}

fn add_claude_defaults_in(home: &Path) -> Result<DefaultsWrite, String> {
    settings_defaults(home, &home.join(".claude"))
}
fn add_claude_defaults(path: &Path, hold: bool) -> Result<DefaultsWrite, String> {
    let lock = write::Lock::acquire(path)
        .map_err(|e| e.to_string())?
        .unwrap();
    add_defaults(path, &lock, hold)
}
fn backups(dir: &Path) -> Vec<String> {
    let mut names: Vec<_> = std::fs::read_dir(dir)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|name| name.starts_with("settings.json.bak-"))
        .collect();
    names.sort();
    names
}
fn saving_after_read<R>(save: impl FnMut(&Path) + 'static, run: impl FnOnce() -> R) -> R {
    write::saving_after_read(save, run)
}
const REWRITE_TRIES: usize = 3;

const SETTINGS_WITH_ATERM_HOOKS: &str = r#"{
  "model": "opus",
  "env": {"SECRET_OF_THE_HUMAN": "never-touched"},
  "statusLine": {"type": "command", "command": "sh '/s/harness/claude-harness/plugin/hooks/bridge.sh' statusline StatusLine usage-hud # aterm-harness", "refreshInterval": 60},
  "hooks": {
    "SessionStart": [
      {"hooks": [{"type": "command", "command": "/opt/aterm/aterm link hook run session-start --state /s"}]}
    ],
    "Stop": [
      {"hooks": [{"type": "command", "command": "/opt/aterm/aterm-link hook run stop --report-to @s-1", "timeout": 600}]},
      {"matcher": "", "hooks": [{"type": "command", "command": "/usr/local/bin/their-own-stop-hook"}]},
      {"hooks": [{"type": "command", "command": "mytool hook run x"}]}
    ],
    "Notification": [
      {"hooks": [{"type": "command", "command": "'/h/u/Library/Application Support/aterm/pkg/bin/aterm' link hook run notification"}]}
    ],
    "PermissionRequest": [
      {"hooks": [{"type": "command", "command": "sh '/s/harness/claude-harness/plugin/hooks/bridge.sh' decide PermissionRequest rm-approve # aterm-harness"}]}
    ],
    "PreToolUse": [
      {"matcher": "Bash", "hooks": [{"type": "command", "command": "/usr/local/bin/their-lint"}]},
      {"hooks": [{"type": "command", "command": "sh '/s/harness/claude-harness/plugin/hooks/bridge.sh' decide PreToolUse rm-approve # aterm-harness"}]}
    ]
  },
  "permissions": {"allow": ["Bash(ls:*)"]}
}
"#;

#[test]
fn claude_defaults_create_only_where_claude_code_is() {
    let home = Temp::new().unwrap();
    let claude = home.path().join(".claude");
    assert_eq!(
        add_claude_defaults_in(home.path()),
        Ok(DefaultsWrite::Nothing)
    );
    assert!(!claude.exists());

    std::fs::create_dir_all(&claude).unwrap();
    assert_eq!(
        add_claude_defaults_in(home.path()),
        Ok(DefaultsWrite::Added {
            keys: CLAUDE_DEFAULTS.map(|(key, _, _)| key).to_vec(),
            backup: None,
        })
    );
    let text = std::fs::read_to_string(claude.join("settings.json")).unwrap();
    let v = settings_json::Json::parse(&text).unwrap();
    for (key, value, _) in CLAUDE_DEFAULTS {
        let got = match key.split_once('.') {
            Some((parent, child)) => v.get(parent).and_then(|o| o.get(child)),
            None => v.get(key),
        };
        assert_eq!(got, Some(&value.json()), "{key}: {text}");
    }
}

#[test]
fn claude_defaults_add_only_what_is_unset_and_not_held_back() {
    use settings_json::Json;
    const TOP: &str = CLAUDE_TOP_EFFORT_KEY;
    const RATE: &str = "feedbackSurveyRate";
    const SWITCH: &str = CLAUDE_SURVEY_SWITCH;
    const DRAFTS: &str = CLAUDE_DRAFTS_KEY;
    const WINDOW: &str = CLAUDE_COMPACT_WINDOW_KEY;
    let home = Temp::new().unwrap();
    let claude = home.path().join(".claude");
    std::fs::create_dir_all(&claude).unwrap();
    let settings = claude.join("settings.json");
    let efforts = format!(r#""{TOP}": true, "effortLevel": "xhigh", "feedbackSurveyRate": 0"#);
    let cases: Vec<(&str, String, Vec<&str>)> = vec![
        (
            "every key set by hand, to any value",
            format!(
                r#"{{"{TOP}": false, "effortLevel": "xhigh", "feedbackSurveyRate": 0,
                        "feedbackDrafts": null, "autoCompactWindow": 300000,
                        "env": {{"CLAUDE_CODE_DISABLE_FEEDBACK_SURVEY": "0"}}}}"#
            ),
            vec![],
        ),
        (
            "no env and no drafts key",
            format!(r#"{{"model": "m", {efforts}}}"#),
            vec![SWITCH, DRAFTS, WINDOW],
        ),
        (
            "an env of the person's own",
            format!(r#"{{{efforts}, "feedbackDrafts": "quiet", "env": {{"B": "2", "A": "1"}}}}"#),
            vec![SWITCH, WINDOW],
        ),
        (
            "a hand-set probability holds the switch back",
            r#"{"feedbackSurveyRate": 0.3}"#.to_string(),
            vec![TOP, "effortLevel", DRAFTS, WINDOW],
        ),
        (
            "a zero probability holds nothing back",
            r#"{"feedbackSurveyRate": 0.0}"#.to_string(),
            vec![TOP, "effortLevel", SWITCH, DRAFTS, WINDOW],
        ),
        (
            "a hand-set effort holds the effort defaults back",
            r#"{"modelSettings": {"claude-opus-5-5": {"effortLevel": "medium"}}}"#.to_string(),
            vec![RATE, SWITCH, DRAFTS, WINDOW],
        ),
        (
            "a hand-set xhigh holds nothing back",
            r#"{"modelSettings": {"claude-opus-5-5": {"effortLevel": "xhigh"}}}"#.to_string(),
            vec![TOP, "effortLevel", RATE, SWITCH, DRAFTS, WINDOW],
        ),
    ];
    let flat = |text: &str| -> Vec<(String, String)> {
        let mut out = Vec::new();
        for (k, v) in Json::parse(text).unwrap().as_object().unwrap() {
            match (k.as_str(), v.as_object()) {
                ("env", Some(env)) => {
                    out.extend(env.iter().map(|(m, v)| (format!("env.{m}"), v.render())))
                }
                _ => out.push((k.clone(), v.render())),
            }
        }
        out
    };
    for (case, text, want) in cases {
        std::fs::write(&settings, &text).unwrap();
        let done = add_claude_defaults_in(home.path());
        let after = std::fs::read_to_string(&settings).unwrap();
        if want.is_empty() {
            assert_eq!(done, Ok(DefaultsWrite::Nothing), "{case}");
            assert_eq!(after, text, "{case}: not a byte touched");
            continue;
        }
        let Ok(DefaultsWrite::Added { keys, .. }) = done else {
            panic!("{case}: {done:?}");
        };
        assert_eq!(keys, want, "{case}");
        let (before, now) = (flat(&text), flat(&after));
        assert_eq!(now[..before.len()], before[..], "{case}: {after}");
    }

    let kept = backups(&claude);
    for bad in [
        "{ not json",
        "[]",
        r#"{"env": "FOO=1"}"#,
        r#"{"env": null}"#,
    ] {
        std::fs::write(&settings, bad).unwrap();
        let done = add_claude_defaults_in(home.path());
        assert!(done.is_err(), "{bad}: {done:?}");
        assert_eq!(std::fs::read_to_string(&settings).unwrap(), bad);
    }
    assert_eq!(backups(&claude), kept, "a refused file leaves no backup");
}

#[test]
fn compact_state_reads_are_bounded_before_defaults_are_chosen() {
    let home = Temp::new().unwrap();
    let state = home.path().join(".claude.json");
    assert_eq!(claude_compact_offers_kept_in(home.path()), Ok(Vec::new()));
    let kept = r#"{"autoCompactOffer":{"account":{"model":{"resolved":{"choice":"keep"}}}}}"#;
    std::fs::write(&state, kept).unwrap();
    assert_eq!(
        claude_compact_offers_kept_in(home.path()),
        Ok(vec!["model".to_string()])
    );
    std::fs::write(&state, [0xff]).unwrap();
    assert!(claude_compact_offers_kept_in(home.path()).is_err());
    let at_limit = format!("{}{{}}", " ".repeat(2 * 1024 * 1024 - 2));
    std::fs::write(&state, &at_limit).unwrap();
    assert_eq!(claude_compact_offers_kept_in(home.path()), Ok(Vec::new()));
    std::fs::write(&state, format!("{at_limit} ")).unwrap();
    assert!(claude_compact_offers_kept_in(home.path()).is_err());
}

#[cfg(unix)]
#[test]
fn compact_state_accepts_regular_symlinks_and_refuses_open_nonregular_files() {
    use std::os::fd::OwnedFd;
    use std::os::unix::{fs::symlink, net::UnixStream};
    let home = Temp::new().unwrap();
    let target = home.path().join("state-target");
    std::fs::write(&target, "{}").unwrap();
    symlink(&target, home.path().join(".claude.json")).unwrap();
    assert_eq!(claude_compact_offers_kept_in(home.path()), Ok(Vec::new()));
    let (stream, _peer) = UnixStream::pair().unwrap();
    stream.set_nonblocking(true).unwrap();
    let file = std::fs::File::from(OwnedFd::from(stream));
    assert_eq!(
        read_compact_state(file).unwrap_err().kind(),
        std::io::ErrorKind::InvalidData
    );
}

#[test]
fn an_oversized_compact_state_holds_only_its_default_back() {
    let home = Temp::new().unwrap();
    let claude = home.path().join(".claude");
    std::fs::create_dir(&claude).unwrap();
    let settings = claude.join("settings.json");
    let state = home.path().join(".claude.json");
    std::fs::write(&state, "{}").unwrap();
    let DefaultsWrite::Added { keys, .. } = add_claude_defaults_in(home.path()).unwrap() else {
        panic!("the ordinary state must admit the defaults");
    };
    assert!(keys.contains(&CLAUDE_COMPACT_WINDOW_KEY));
    std::fs::write(&settings, "{}").unwrap();
    std::fs::write(&state, format!("{}{{}}", " ".repeat(2 * 1024 * 1024 - 1))).unwrap();
    let DefaultsWrite::Added { keys, .. } = add_claude_defaults_in(home.path()).unwrap() else {
        panic!("other unset defaults must still be added");
    };
    assert!(!keys.contains(&CLAUDE_COMPACT_WINDOW_KEY));
    let after = settings_json::Json::parse(&std::fs::read_to_string(&settings).unwrap()).unwrap();
    assert!(after.get(CLAUDE_COMPACT_WINDOW_KEY).is_none());
    assert!(after.get(CLAUDE_TOP_EFFORT_KEY).is_some());
}

#[test]
fn claude_defaults_add_the_compact_window_only_where_unset_and_no_offer_was_kept() {
    const TOP: &str = CLAUDE_TOP_EFFORT_KEY;
    let home = Temp::new().unwrap();
    let claude = home.path().join(".claude");
    std::fs::create_dir_all(&claude).unwrap();
    let settings = claude.join("settings.json");
    let rest = format!(
        "  \"{TOP}\": true,\n  \"effortLevel\": \"xhigh\",\n  \"feedbackSurveyRate\": 0,\n  \
         \"env\": {{\n    \"CLAUDE_CODE_DISABLE_FEEDBACK_SURVEY\": \"1\"\n  }},\n  \
         \"feedbackDrafts\": \"quiet\""
    );
    let run = |text: &str| {
        std::fs::write(&settings, text).unwrap();
        let done = add_claude_defaults_in(home.path());
        (done, std::fs::read_to_string(&settings).unwrap())
    };
    let added = |done: &Result<DefaultsWrite, String>| match done {
        Ok(DefaultsWrite::Added { keys, .. }) => keys.clone(),
        _ => Vec::new(),
    };

    let (done, after) = run(&format!("{{\n{rest}\n}}\n"));
    assert_eq!(added(&done), vec![CLAUDE_COMPACT_WINDOW_KEY]);
    assert_eq!(
        after,
        format!("{{\n{rest},\n  \"autoCompactWindow\": 400000\n}}\n")
    );

    for mine in ["300000", "1000000", "100000", "400000", "\"auto\""] {
        let text = format!("{{\n{rest},\n  \"autoCompactWindow\": {mine}\n}}\n");
        let (done, after) = run(&text);
        assert_eq!(done, Ok(DefaultsWrite::Nothing), "{mine}");
        assert_eq!(after, text, "{mine}");
    }

    for mine in ["400000", "1000000", "300000", "\"auto\""] {
        let model = format!(
            "  \"modelSettings\": {{\n    \"claude-opus-5-5\": {{\n      \
             \"autoCompactWindow\": {mine}\n    }}\n  }}"
        );
        let (done, after) = run(&format!("{{\n{model},\n{rest}\n}}\n"));
        assert_eq!(added(&done), vec![CLAUDE_COMPACT_WINDOW_KEY], "{mine}");
        assert_eq!(
            after,
            format!("{{\n{model},\n{rest},\n  \"autoCompactWindow\": 400000\n}}\n"),
            "{mine}"
        );
    }

    let state = home.path().join(".claude.json");
    let offer = |choice: &str| {
        format!(
            r#"{{"eligibleLoggedAt": 1, "resolved": {{"choice": "{choice}", "tokens": 1000000}}}}"#
        )
    };
    let kept = format!(
        r#"{{"numStartups": 3, "autoCompactOffer": {{
                "acct-a": {{"claude-opus-5-5": {keep}, "claude-sonnet-5": {accept}}},
                "acct-b": {{"claude-opus-5-5": {keep}, "claude-fable-5-1": {{"escCount": 1}},
                            "claude-haiku-5": 7}}}}}}"#,
        keep = offer("keep"),
        accept = offer("accept"),
    );
    assert_eq!(
        compact_offers_kept(&kept),
        Ok(vec!["claude-opus-5-5".to_string()])
    );
    let accepted = format!(
        r#"{{"autoCompactOffer": {{"acct-a": {{"claude-opus-5-5": {}}}}}}}"#,
        offer("accept")
    );
    for none in [accepted.as_str(), "{}", r#"{"autoCompactOffer": []}"#] {
        assert_eq!(compact_offers_kept(none), Ok(Vec::new()), "{none}");
    }
    assert!(compact_offers_kept("{").is_err());
    assert_eq!(
        claude_compact_offers_kept_in(home.path()),
        Ok(Vec::new()),
        "no file"
    );

    let unset = format!("{{\n{rest}\n}}\n");
    std::fs::write(&state, &kept).unwrap();
    let (done, after) = run(&unset);
    assert_eq!(done, Ok(DefaultsWrite::Nothing));
    assert_eq!(after, unset);
    let (done, _) = run("{}");
    assert_eq!(
        added(&done),
        vec![
            TOP,
            "effortLevel",
            "feedbackSurveyRate",
            CLAUDE_SURVEY_SWITCH,
            CLAUDE_DRAFTS_KEY
        ]
    );
    std::fs::write(&state, "{\"autoCompactOffer\": ").unwrap();
    assert_eq!(run(&unset), (Ok(DefaultsWrite::Nothing), unset.clone()));
    std::fs::remove_file(&state).unwrap();
    std::fs::create_dir(&state).unwrap();
    assert!(claude_compact_offers_kept_in(home.path()).is_err());
    assert_eq!(run(&unset), (Ok(DefaultsWrite::Nothing), unset.clone()));
    std::fs::remove_dir(&state).unwrap();
    std::fs::write(&state, &accepted).unwrap();
    let (done, _) = run(&unset);
    assert_eq!(added(&done), vec![CLAUDE_COMPACT_WINDOW_KEY]);
}

#[cfg(unix)]
#[test]
fn claude_defaults_never_create_over_a_dangling_settings_link() {
    let home = Temp::new().unwrap();
    let claude = home.path().join(".claude");
    std::fs::create_dir_all(&claude).unwrap();
    let settings = claude.join("settings.json");
    let target = home.path().join("dotfiles").join("claude-settings.json");
    std::os::unix::fs::symlink(&target, &settings).unwrap();

    let refused = add_claude_defaults_in(home.path());
    assert!(refused.is_err(), "{refused:?}");
    assert_eq!(
        std::fs::read_link(&settings).unwrap(),
        target,
        "the link is still that link"
    );
    assert!(!target.exists(), "nothing is created at the link's target");
    assert_eq!(
        std::fs::read_dir(&claude).unwrap().count(),
        1,
        "no backup and no temp file beside the link"
    );
}

#[cfg(unix)]
#[test]
fn a_settings_write_that_fails_leaves_no_backup_behind() {
    use std::os::unix::fs::PermissionsExt as _;
    let home = Temp::new().unwrap();
    let claude = home.path().join(".claude");
    std::fs::create_dir_all(&claude).unwrap();
    let store = home.path().join("store");
    std::fs::create_dir_all(&store).unwrap();
    let real = store.join("settings.json");
    std::fs::write(&real, SETTINGS_WITH_ATERM_HOOKS).unwrap();
    std::os::unix::fs::symlink(&real, claude.join("settings.json")).unwrap();

    std::fs::set_permissions(&store, std::fs::Permissions::from_mode(0o555)).unwrap();
    let defaults = add_claude_defaults_in(home.path());
    std::fs::set_permissions(&store, std::fs::Permissions::from_mode(0o755)).unwrap();

    assert!(defaults.is_err(), "{defaults:?}");
    assert_eq!(backups(&claude), Vec::<String>::new());
    assert_eq!(
        std::fs::read_to_string(&real).unwrap(),
        SETTINGS_WITH_ATERM_HOOKS
    );
}

#[test]
fn a_save_between_read_and_write_of_the_settings_stands() {
    fn theirs(n: usize) -> String {
        format!("{{\"model\": \"opus\", \"theirs\": {n}}}\n")
    }
    fn save(settings: &Path, n: usize) {
        if n.is_multiple_of(2) {
            let swap = settings.with_extension("json.swp");
            std::fs::write(&swap, theirs(n)).unwrap();
            std::fs::rename(&swap, settings).unwrap();
        } else {
            std::fs::write(settings, theirs(n)).unwrap();
        }
    }
    let home = Temp::new().unwrap();
    let claude = home.path().join(".claude");
    std::fs::create_dir_all(&claude).unwrap();
    let settings = claude.join("settings.json");
    for saves in [REWRITE_TRIES - 1, REWRITE_TRIES] {
        std::fs::write(&settings, "{\"model\": \"opus\"}\n").unwrap();
        for b in backups(&claude) {
            std::fs::remove_file(claude.join(b)).unwrap();
        }
        let mut n = 0;
        let wrote = saving_after_read(
            move |path| {
                if n < saves {
                    save(path, n);
                }
                n += 1;
            },
            || add_claude_defaults(&settings, false),
        );
        let after = std::fs::read_to_string(&settings).unwrap();
        let last = theirs(saves - 1);
        if saves < REWRITE_TRIES {
            let Ok(DefaultsWrite::Added {
                backup: Some(backup),
                ..
            }) = wrote
            else {
                panic!("{wrote:?}")
            };
            let doc = settings_json::Json::parse(&after).unwrap();
            let theirs = settings_json::Json::Number((saves - 1).to_string());
            assert_eq!(doc.get("theirs"), Some(&theirs), "{after}");
            assert!(doc.get("effortLevel").is_some(), "{after}");
            assert_eq!(std::fs::read_to_string(&backup).unwrap(), last);
            assert_eq!(backups(&claude).len(), 1);
        } else {
            assert!(wrote.is_err(), "{wrote:?}");
            assert_eq!(after, last);
            assert_eq!(backups(&claude), Vec::<String>::new());
        }
    }
}
