// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;
use crate::prime::tests::Temp;
use std::os::unix::fs::{PermissionsExt as _, symlink};

const FIXTURE: &str = r#"{
  "model": "opus",
  "env": {"SECRET_OF_THE_HUMAN": "never-touched"},
  "statusLine": {"type": "command", "command": "sh '/s/harness/claude-harness/plugin/hooks/bridge.sh' statusline StatusLine usage-hud # aterm-harness", "refreshInterval": 60},
  "hooks": {
    "SessionStart": [{"hooks": [{"type": "command", "command": "/opt/aterm/aterm link hook run session-start --state /s"}]}],
    "Stop": [
      {"hooks": [{"type": "command", "command": "/opt/aterm/aterm-link hook run stop --report-to @s-1", "timeout": 600}]},
      {"matcher": "", "hooks": [{"type": "command", "command": "/usr/local/bin/their-own-stop-hook"}]},
      {"hooks": [{"type": "command", "command": "mytool hook run x"}]}
    ],
    "Notification": [{"hooks": [{"type": "command", "command": "'/h/u/Library/Application Support/aterm/pkg/bin/aterm' link hook run notification"}]}],
    "PermissionRequest": [{"hooks": [{"type": "command", "command": "sh '/s/harness/claude-harness/plugin/hooks/bridge.sh' decide PermissionRequest rm-approve # aterm-harness"}]}],
    "PreToolUse": [
      {"matcher": "Bash", "hooks": [{"type": "command", "command": "/usr/local/bin/their-lint"}]},
      {"hooks": [{"type": "command", "command": "sh '/s/harness/claude-harness/plugin/hooks/bridge.sh' decide PreToolUse rm-approve # aterm-harness"}]}
    ]
  },
  "permissions": {"allow": ["Bash(ls:*)"]}
}
"#;

fn backups(config: &Path) -> Vec<PathBuf> {
    let mut paths: Vec<_> = std::fs::read_dir(config)
        .unwrap()
        .map(|entry| entry.unwrap().path().canonicalize().unwrap())
        .filter(|path| {
            path.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("settings.json.bak-")
        })
        .collect();
    paths.sort();
    paths
}

fn statusline(config: &Path) -> Option<Json> {
    Json::parse(&std::fs::read_to_string(config.join("settings.json")).unwrap())
        .unwrap()
        .get("statusLine")
        .cloned()
}

fn original(command: &str, padding: usize) -> String {
    format!(
        r#"{{"statusLine":{{"type":"command","command":{},"padding":{padding}}}}}"#,
        super::super::settings_json::string(command)
    )
}

#[test]
fn only_the_native_hook_verb_or_exact_retired_installer_mark_is_removed() {
    for ours in [
        "/Applications/aterm.app/Contents/MacOS/aterm link hook run stop --state /h/u/.local/state/aterm-link --wake-budget 6/1 --timeout 15",
        "aterm link hook run notification",
        "/opt/aterm/aterm-link hook run stop --report-to @s-1",
        "'/h/u/Library/Application Support/aterm/pkg/bin/aterm' link hook run stop",
        "'/tmp/it'\\''s/aterm-link' hook run stop",
        "/h/u/Library/Application Support/aterm/pkg/bin/aterm link hook run stop --state /h/u/.local/state/aterm-link",
        "/h/u/My Tools/aterm hook run stop",
        "/Applications/aterm.app/Contents/MacOS/aterm-cli link hook run stop",
        "aterm-cli link hook run notification",
        "sh '/s/plugin/hooks/bridge.sh' statusline StatusLine usage-hud # aterm-harness  ",
    ] {
        assert!(is_aterm_hook_command(ours), "{ours}");
    }
    for theirs in [
        "mytool hook run x",
        "/usr/local/bin/mytool link hook run stop",
        "/opt/aterm/aterm-foo link hook run stop",
        "/opt/aterm/aterm link hook runner",
        "/opt/aterm/aterm ctl ls",
        "echo aterm link hook run stop",
        "'/unterminated/aterm link hook run stop",
        "echo /h/u/Library/Application Support/aterm/pkg/bin/aterm link hook run stop",
        "/usr/bin/tee /x | /h/u/App Support/aterm link hook run stop",
        "/usr/bin/env FOO=$(/x/aterm link hook run stop)",
        "/h/u/bin/my aterm link hook run stop",
        "Library/Application Support/aterm link hook run stop",
        "/h/u/Application Support/aterm link hook runner",
        "sh x.sh # aterm-harness-but-not",
        "sh x.sh #aterm-harness",
        "# aterm-harness",
    ] {
        assert!(!is_aterm_hook_command(theirs), "{theirs}");
    }
}

#[test]
fn cleanup_keeps_foreign_values_order_exact_backup_bytes_and_file_modes() {
    let home = Temp::new().unwrap();
    let path = home.path().join("settings.json");
    std::fs::write(&path, FIXTURE).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o640)).unwrap();
    let Removal::Removed { removed, backup } = remove(home.path()).unwrap() else {
        panic!("legacy hooks were not removed")
    };
    assert_eq!(removed, 6);
    assert_eq!(std::fs::read_to_string(&backup).unwrap(), FIXTURE);
    assert_eq!(
        std::fs::metadata(&backup).unwrap().permissions().mode() & 0o777,
        0o640
    );
    assert_eq!(
        std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o640
    );
    let after = std::fs::read_to_string(&path).unwrap();
    for gone in [
        "link hook run",
        "aterm-link",
        "aterm-harness",
        "statusLine",
        "SessionStart",
        "Notification",
        "PermissionRequest",
    ] {
        assert!(!after.contains(gone), "{gone}: {after}");
    }
    for kept in [
        "/usr/local/bin/their-own-stop-hook",
        "/usr/local/bin/their-lint",
        "\"mytool hook run x\"",
        "\"model\": \"opus\"",
        "never-touched",
        "Bash(ls:*)",
    ] {
        assert!(after.contains(kept), "{kept}: {after}");
    }
    let at = |key: &str| after.find(&format!("\"{key}\"")).unwrap();
    assert!(
        at("model") < at("env")
            && at("env") < at("hooks")
            && at("hooks") < at("permissions")
            && at("Stop") < at("PreToolUse")
    );
    assert_eq!(remove(home.path()), Ok(Removal::Nothing));
    assert_eq!(std::fs::read_to_string(&path).unwrap(), after);
    assert_eq!(backups(home.path()), [backup]);
}

#[test]
fn no_managed_content_creates_no_lock_backup_or_settings_directory() {
    let home = Temp::new().unwrap();
    let missing = home.path().join("absent");
    assert_eq!(remove(&missing), Ok(Removal::Nothing));
    assert!(!missing.exists());
    for untouched in [
        "{}\n",
        r#"{"hooks":{"Stop":[{"hooks":[{"command":"their hook"}]}]},"statusLine":{"command":"their footer"}}"#,
    ] {
        std::fs::write(home.path().join("settings.json"), untouched).unwrap();
        assert_eq!(remove(home.path()), Ok(Removal::Nothing));
        assert_eq!(
            std::fs::read_to_string(home.path().join("settings.json")).unwrap(),
            untouched
        );
        assert_eq!(std::fs::read_dir(home.path()).unwrap().count(), 1);
    }
    assert!(remove(Path::new("relative")).is_err());
}

#[test]
fn invalid_hook_containers_and_bounded_sources_are_refused_without_writes() {
    let home = Temp::new().unwrap();
    let settings = home.path().join("settings.json");
    for invalid in ["{", r#"{"hooks":true}"#, r#"{"hooks":{"Stop":{}}}"#] {
        std::fs::write(&settings, invalid).unwrap();
        assert!(remove(home.path()).is_err(), "{invalid}");
        assert_eq!(std::fs::read_to_string(&settings).unwrap(), invalid);
        assert_eq!(std::fs::read_dir(home.path()).unwrap().count(), 1);
    }
    std::fs::write(&settings, vec![b'x'; READ_MAX as usize + 1]).unwrap();
    assert!(remove(home.path()).is_err());
    assert!(read_text(Path::new("/dev/null")).is_err());
    assert!(backups(home.path()).is_empty());
}

#[test]
fn a_byte_copy_restores_the_original_statusline_at_its_existing_slot() {
    let home = Temp::new().unwrap();
    std::fs::write(home.path().join("settings.json"), FIXTURE).unwrap();
    std::fs::write(
        home.path().join("settings.json.aterm-harness.orig"),
        original("~/bin/my-footer", 0),
    )
    .unwrap();
    remove(home.path()).unwrap();
    let after = std::fs::read_to_string(home.path().join("settings.json")).unwrap();
    let at = |key: &str| after.find(&format!("\"{key}\"")).unwrap();
    assert!(at("env") < at("statusLine") && at("statusLine") < at("hooks"));
    let restored = statusline(home.path()).unwrap();
    assert_eq!(
        restored.get("command").and_then(Json::as_str),
        Some("~/bin/my-footer")
    );
    assert_eq!(restored.get("padding"), Some(&Json::Number("0".into())));
}

#[test]
fn the_displaced_user_command_wins_but_matching_backup_metadata_survives() {
    let home = Temp::new().unwrap();
    let state = home.path().join("harness state/claude-harness");
    std::fs::create_dir_all(state.join("plugin/hooks")).unwrap();
    let settings = home.path().join("settings.json");
    let fixture = FIXTURE.replace("/s/harness/claude-harness", state.to_str().unwrap());
    std::fs::write(
        home.path().join("settings.json.aterm-harness.orig"),
        original("~/bin/old-footer", 2),
    )
    .unwrap();
    let user = state.join("statusline.user");
    for (command, padding) in [
        ("~/bin/new-footer --tz \"UTC\"\n", false),
        ("~/bin/old-footer\n", true),
    ] {
        std::fs::write(&settings, &fixture).unwrap();
        std::fs::write(&user, command).unwrap();
        remove(home.path()).unwrap();
        let line = statusline(home.path()).unwrap();
        assert_eq!(
            line.get("command").and_then(Json::as_str),
            Some(command.trim_end_matches('\n'))
        );
        assert_eq!(line.get("padding").is_some(), padding);
    }
    for command in [
        None,
        Some("sh '/s/bridge.sh' # aterm-harness\n"),
        Some("\n"),
    ] {
        std::fs::write(&settings, &fixture).unwrap();
        match command {
            Some(command) => std::fs::write(&user, command).unwrap(),
            None => std::fs::remove_file(&user).unwrap(),
        }
        remove(home.path()).unwrap();
        assert_eq!(statusline(home.path()), None);
        assert_eq!(remove(home.path()), Ok(Removal::Nothing));
    }
}

#[test]
fn preexisting_empty_groups_and_events_survive_removal() {
    let home = Temp::new().unwrap();
    let settings = home.path().join("settings.json");
    std::fs::write(&settings, r#"{"statusLine":{"command":"sh '/s/bridge.sh' # aterm-harness"},"hooks":{"Stop":[{"hooks":[{"command":"aterm link hook run stop"}]},{"matcher":"keep-empty-group","hooks":[]}],"UserPromptSubmit":[]}}"#).unwrap();
    assert!(matches!(
        remove(home.path()),
        Ok(Removal::Removed { removed: 2, .. })
    ));
    let after = std::fs::read_to_string(&settings).unwrap();
    assert!(after.contains("keep-empty-group") && after.contains("\"UserPromptSubmit\": []"));
    std::fs::write(
        &settings,
        r#"{"statusLine":{"command":"sh '/s/bridge.sh' # aterm-harness"},"hooks":{}}"#,
    )
    .unwrap();
    remove(home.path()).unwrap();
    assert!(
        std::fs::read_to_string(&settings)
            .unwrap()
            .contains("\"hooks\": {}")
    );
}

#[test]
fn cleanup_and_defaults_share_the_same_physical_target_lock() {
    let home = Temp::new().unwrap();
    let config = home.path().join("config");
    let alias = home.path().join("alias");
    std::fs::create_dir(&config).unwrap();
    symlink(&config, &alias).unwrap();
    let settings = config.join("settings.json");
    std::fs::write(&settings, FIXTURE).unwrap();
    let held = write::Lock::acquire(&settings).unwrap().unwrap();
    assert!(remove(&alias).is_err());
    assert_eq!(std::fs::read_to_string(&settings).unwrap(), FIXTURE);
    assert!(backups(&config).is_empty());
    drop(held);
    assert!(matches!(remove(&alias), Ok(Removal::Removed { .. })));
    assert!(matches!(
        crate::prime::settings_defaults(home.path(), &config),
        Ok(crate::prime::DefaultsWrite::Added { .. })
    ));
}

#[test]
fn a_concurrent_save_is_reread_and_the_backup_belongs_to_the_final_removed_version() {
    let home = Temp::new().unwrap();
    let settings = home.path().join("settings.json");
    std::fs::write(&settings, FIXTURE).unwrap();
    let saved = FIXTURE.replace("\"model\": \"opus\"", "\"model\": \"changed\"");
    let wanted = saved.clone();
    let mut once = false;
    let removed = write::saving_after_read(
        move |path| {
            if !once {
                once = true;
                let temporary = path.with_extension("saved");
                std::fs::write(&temporary, &saved).unwrap();
                std::fs::rename(temporary, path).unwrap();
            }
        },
        || remove(home.path()),
    )
    .unwrap();
    let Removal::Removed { removed, backup } = removed else {
        panic!("not removed")
    };
    assert_eq!(removed, 6);
    assert_eq!(std::fs::read_to_string(backup).unwrap(), wanted);
    assert!(
        std::fs::read_to_string(settings)
            .unwrap()
            .contains("\"changed\"")
    );
    assert_eq!(backups(home.path()).len(), 1);
}

#[test]
fn a_settings_alias_aba_cannot_import_another_targets_statusline_backup() {
    let home = Temp::new().unwrap();
    let config = home.path().join("config");
    std::fs::create_dir(&config).unwrap();
    let a = home.path().join("a.json");
    let b = home.path().join("b.json");
    let selected = config.join("settings.json");
    for path in [&a, &b] {
        std::fs::write(path, FIXTURE).unwrap();
    }
    std::fs::write(
        home.path().join("a.json.aterm-harness.orig"),
        original("footer-a", 1),
    )
    .unwrap();
    std::fs::write(
        home.path().join("b.json.aterm-harness.orig"),
        original("footer-b", 2),
    )
    .unwrap();
    symlink(&a, &selected).unwrap();
    let retarget = b.clone();
    let back = a.clone();
    let alias = selected.clone();
    let result = write::before_final_check(
        move || {
            std::fs::remove_file(&alias).unwrap();
            symlink(&back, &alias).unwrap();
        },
        || {
            write::saving_after_read(
                move |path| {
                    std::fs::remove_file(path).unwrap();
                    symlink(&retarget, path).unwrap();
                },
                || remove(&config),
            )
        },
    );
    assert!(matches!(result, Ok(Removal::Removed { .. })), "{result:?}");
    let doc = Json::parse(&std::fs::read_to_string(&a).unwrap()).unwrap();
    assert_eq!(
        doc.get("statusLine")
            .unwrap()
            .get("command")
            .and_then(Json::as_str),
        Some("footer-a")
    );
    assert_eq!(std::fs::read_to_string(&b).unwrap(), FIXTURE);
}

#[test]
fn a_selected_parent_aba_cannot_redirect_the_statusline_backup_read() {
    let home = Temp::new().unwrap();
    let config = home.path().join("config");
    let original_config = home.path().join("original-config");
    let foreign_config = home.path().join("foreign-config");
    std::fs::create_dir(&config).unwrap();
    let physical = home.path().join("physical-settings");
    std::fs::write(&physical, FIXTURE).unwrap();
    symlink(&physical, config.join("settings.json")).unwrap();
    std::fs::write(
        config.join("settings.json.aterm-harness.orig"),
        original("selected-a", 1),
    )
    .unwrap();
    let move_config = config.clone();
    let saved_config = original_config.clone();
    let settings_target = physical.clone();
    let restore_config = config.clone();
    let restore_original = original_config.clone();
    let result = write::before_final_check(
        move || {
            std::fs::rename(&restore_config, &foreign_config).unwrap();
            std::fs::rename(&restore_original, &restore_config).unwrap();
        },
        || {
            write::saving_after_read(
                move |_| {
                    std::fs::rename(&move_config, &saved_config).unwrap();
                    std::fs::create_dir(&move_config).unwrap();
                    symlink(&settings_target, move_config.join("settings.json")).unwrap();
                    std::fs::write(
                        move_config.join("settings.json.aterm-harness.orig"),
                        original("selected-b", 2),
                    )
                    .unwrap();
                },
                || remove(&config),
            )
        },
    );
    assert!(matches!(result, Ok(Removal::Removed { .. })), "{result:?}");
    assert_eq!(
        statusline(&config)
            .unwrap()
            .get("command")
            .and_then(Json::as_str),
        Some("selected-a")
    );
}

#[test]
fn the_selected_backup_precedes_the_physical_backup_and_native_paths_are_retained() {
    let home = Temp::new().unwrap();
    let config = home.path().join("config");
    std::fs::create_dir(&config).unwrap();
    let target = home.path().join("physical-settings");
    std::fs::write(&target, FIXTURE).unwrap();
    symlink(&target, config.join("settings.json")).unwrap();
    std::fs::write(
        home.path().join("physical-settings.aterm-harness.orig"),
        original("physical", 1),
    )
    .unwrap();
    std::fs::write(
        config.join("settings.json.aterm-harness.orig"),
        original("selected", 2),
    )
    .unwrap();
    let Removal::Removed { backup, .. } = remove(&config).unwrap() else {
        panic!("not removed")
    };
    assert_eq!(
        statusline(&config)
            .unwrap()
            .get("command")
            .and_then(Json::as_str),
        Some("selected")
    );
    let physical_home = home.path().canonicalize().unwrap();
    assert_eq!(backup.parent(), Some(physical_home.as_path()));
    assert!(
        backup
            .file_name()
            .unwrap()
            .to_str()
            .unwrap()
            .starts_with("physical-settings.bak-")
    );
    assert_eq!(std::fs::read_to_string(backup).unwrap(), FIXTURE);
}

#[test]
fn installer_backup_symlinks_may_restore_regular_files_but_not_device_files() {
    let home = Temp::new().unwrap();
    let settings = home.path().join("settings.json");
    let copy = home.path().join("installer-copy");
    let alias = home.path().join("settings.json.aterm-harness.orig");
    std::fs::write(&copy, original("original-through-alias", 3)).unwrap();
    symlink(&copy, &alias).unwrap();
    std::fs::write(&settings, FIXTURE).unwrap();
    remove(home.path()).unwrap();
    assert_eq!(
        statusline(home.path())
            .unwrap()
            .get("command")
            .and_then(Json::as_str),
        Some("original-through-alias")
    );
    assert_eq!(
        std::fs::read_to_string(&copy).unwrap(),
        original("original-through-alias", 3)
    );
    std::fs::remove_file(&alias).unwrap();
    symlink("/dev/null", &alias).unwrap();
    std::fs::write(&settings, FIXTURE).unwrap();
    remove(home.path()).unwrap();
    assert_eq!(statusline(home.path()), None);
}

#[cfg(target_os = "linux")]
#[test]
fn a_native_non_utf8_physical_target_restores_and_reports_its_exact_backup_path() {
    use std::os::unix::ffi::{OsStrExt as _, OsStringExt as _};
    let home = Temp::new().unwrap();
    let config = home.path().join("config");
    std::fs::create_dir(&config).unwrap();
    let target = home
        .path()
        .join(std::ffi::OsString::from_vec(b"settings-\xff".to_vec()));
    std::fs::write(&target, FIXTURE).unwrap();
    let mut original_copy = target.as_os_str().to_owned();
    original_copy.push(HARNESS_BACKUP_SUFFIX);
    std::fs::write(Path::new(&original_copy), original("native-footer", 4)).unwrap();
    symlink(&target, config.join("settings.json")).unwrap();
    let Removal::Removed { backup, .. } = remove(&config).unwrap() else {
        panic!("not removed")
    };
    assert_eq!(
        statusline(&config)
            .unwrap()
            .get("command")
            .and_then(Json::as_str),
        Some("native-footer")
    );
    assert!(
        backup
            .file_name()
            .unwrap()
            .as_bytes()
            .starts_with(b"settings-\xff.bak-")
    );
    assert_eq!(std::fs::read_to_string(backup).unwrap(), FIXTURE);
}

#[test]
fn a_backup_result_bound_refuses_before_changing_the_original_settings() {
    let home = Temp::new().unwrap();
    let config = home.path().join("config");
    std::fs::create_dir(&config).unwrap();
    let settings = config.join("settings.json");
    std::fs::write(&settings, FIXTURE).unwrap();
    assert!(remove_with_backup_bound(&config, 1).is_err());
    assert_eq!(std::fs::read_to_string(&settings).unwrap(), FIXTURE);
    assert!(backups(&config).is_empty());
    assert!(
        std::fs::read_dir(&config)
            .unwrap()
            .flatten()
            .all(|entry| !entry.file_name().to_string_lossy().contains("tmp"))
    );
    assert!(matches!(remove(&config), Ok(Removal::Removed { .. })));
}

#[test]
fn legacy_hook_removal_never_creates_over_a_dangling_settings_link() {
    let home = Temp::new().unwrap();
    let config = home.path().join(".claude");
    std::fs::create_dir_all(&config).unwrap();
    let settings = config.join("settings.json");
    let target = home.path().join("dotfiles").join("claude-settings.json");
    symlink(&target, &settings).unwrap();
    assert_eq!(remove(&config).unwrap(), Removal::Nothing);
    assert_eq!(std::fs::read_link(&settings).unwrap(), target);
    assert!(!target.exists());
    assert_eq!(std::fs::read_dir(&config).unwrap().count(), 1);
}
