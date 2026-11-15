// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Real shipped-CLI entry controls. This scenario starts no vendor and constructs no mode lease.

use super::*;
use std::os::unix::fs::OpenOptionsExt as _;

fn run(s: &Scenario, mut command: Command) -> (Option<Status>, Vec<u8>) {
    let output = s.root.join("prime-stdout");
    command
        .stdin(Stdio::null())
        .stdout(Stdio::from(std::fs::File::create(&output).unwrap()))
        .stderr(Stdio::from(
            std::fs::File::create(s.root.join("prime-stderr")).unwrap(),
        ))
        .process_group(0);
    let child = command.spawn().expect("spawn real priming CLI");
    s.guard_group(child_pid(&child));
    let ended = reap(child, Duration::from_secs(60));
    let mut bytes = Vec::new();
    std::fs::File::open(output)
        .unwrap()
        .take(u64::try_from(claude_code_alab::prime::MAX_RESULT_BYTES).unwrap() + 1)
        .read_to_end(&mut bytes)
        .unwrap();
    (ended, bytes)
}

pub(super) fn retains_native_entry_guards_and_reports_only_its_closed_result() {
    let s = Scenario::new("prime-entry");
    let config = s.root.join("explicit config");
    std::fs::create_dir(&config).unwrap();
    let settings = config.join("settings.json");
    let original = b"{\"model\":\"opus\",\"theme\":\"dark\"}\n";
    std::fs::write(&settings, original).unwrap();
    let args = [
        "prime",
        "settings-defaults",
        "--home",
        s.home.to_str().unwrap(),
        "--config",
        config.to_str().unwrap(),
    ];
    let mut marked = s.command(&args);
    marked.env(
        claude_code_alab::cli::PARENT_VAR,
        std::process::id().to_string(),
    );
    let (ended, _) = run(&s, marked);
    assert_eq!(ended, Some(Status::Exited(127)), "{}", s.report());
    assert_eq!(std::fs::read(&settings).unwrap(), original);
    assert_eq!(std::fs::read_dir(&config).unwrap().count(), 1);
    assert!(s.starts().is_empty());
    let mut nested = s.command(&args);
    nested.env(claude_code_alab::cli::DEPTH_VAR, u32::MAX.to_string());
    assert_eq!(run(&s, nested).0, Some(Status::Exited(127)));
    assert_eq!(std::fs::read(&settings).unwrap(), original);

    // Metadata stays read-only even under the own-vendor marker.
    let mut query = s.command(&["--alab-primes=claude:settings-defaults"]);
    query.env(
        claude_code_alab::cli::PARENT_VAR,
        std::process::id().to_string(),
    );
    assert_eq!(run(&s, query).0, Some(Status::Exited(0)));
    assert_eq!(std::fs::read_dir(&config).unwrap().count(), 1);
    for malformed in [
        vec!["prime"],
        vec![
            "prime",
            "settings-defaults",
            "--home",
            s.home.to_str().unwrap(),
        ],
        vec![
            "prime",
            "settings-defaults",
            "--home",
            "relative",
            "--config",
            config.to_str().unwrap(),
        ],
    ] {
        assert_eq!(run(&s, s.command(&malformed)).0, Some(Status::Exited(2)));
        assert_eq!(std::fs::read(&settings).unwrap(), original);
    }

    // A separately owned lock makes the real child refuse instead of overlapping a writer.
    let hold = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open(config.join(".settings.json.alab-prime.lock"))
        .unwrap();
    hold.try_lock().unwrap();
    assert_eq!(run(&s, s.command(&args)).0, Some(Status::Exited(2)));
    assert_eq!(std::fs::read(&settings).unwrap(), original);
    drop(hold);

    let (ended, reply) = run(&s, s.command(&args));
    assert_eq!(ended, Some(Status::Exited(0)));
    assert_eq!(
        reply,
        b"OK prime=1 operation=settings-defaults changed=1 backup=1\n"
    );
    let (ended, reply) = run(&s, s.command(&args));
    assert_eq!(ended, Some(Status::Exited(0)));
    assert_eq!(
        reply,
        b"OK prime=1 operation=settings-defaults changed=0 backup=0\n"
    );
    let text = std::fs::read_to_string(&settings).unwrap();
    let document = claude_code_alab::json::parse(&text).unwrap();
    assert_eq!(
        document
            .get("model")
            .and_then(claude_code_alab::json::Value::as_str),
        Some("opus")
    );
    assert_eq!(
        document
            .get("effortLevel")
            .and_then(claude_code_alab::json::Value::as_str),
        Some("xhigh")
    );
    assert_eq!(
        document
            .get("autoCompactWindow")
            .and_then(claude_code_alab::json::Value::as_u64),
        Some(400000)
    );
    assert!(!s.home.join(".claude/settings.json").exists());
    assert!(s.starts().is_empty());
}

pub(super) fn legacy_cleanup_uses_the_same_guards_and_a_lossless_backup_receipt() {
    use std::os::unix::ffi::OsStrExt as _;
    let s = Scenario::new("prime-legacy");
    let config = s.root.join("explicit config");
    std::fs::create_dir(&config).unwrap();
    let settings = config.join("settings.json");
    let original = br#"{"model":"opus","hooks":{"PreToolUse":[{"hooks":[{"type":"command","command":"aterm link hook run"}]}]},"statusLine":{"type":"command","command":"echo my footer"}}"#;
    std::fs::write(&settings, original).unwrap();
    let args = [
        "prime",
        "legacy-hooks",
        "--home",
        s.home.to_str().unwrap(),
        "--config",
        config.to_str().unwrap(),
    ];
    let mut marked = s.command(&args);
    marked.env(
        claude_code_alab::cli::PARENT_VAR,
        std::process::id().to_string(),
    );
    assert_eq!(run(&s, marked).0, Some(Status::Exited(127)));
    assert_eq!(std::fs::read(&settings).unwrap(), original);
    assert_eq!(std::fs::read_dir(&config).unwrap().count(), 1);
    let mut nested = s.command(&args);
    nested.env(claude_code_alab::cli::DEPTH_VAR, u32::MAX.to_string());
    assert_eq!(run(&s, nested).0, Some(Status::Exited(127)));
    assert_eq!(std::fs::read(&settings).unwrap(), original);
    let mut query = s.command(&["--alab-primes=claude:legacy-hooks"]);
    query.env(
        claude_code_alab::cli::PARENT_VAR,
        std::process::id().to_string(),
    );
    assert_eq!(run(&s, query).0, Some(Status::Exited(0)));
    assert_eq!(std::fs::read_dir(&config).unwrap().count(), 1);
    let hold = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open(config.join(".settings.json.alab-prime.lock"))
        .unwrap();
    hold.try_lock().unwrap();
    assert_eq!(run(&s, s.command(&args)).0, Some(Status::Exited(2)));
    assert_eq!(std::fs::read(&settings).unwrap(), original);
    drop(hold);
    let (ended, row) = run(&s, s.command(&args));
    assert_eq!(ended, Some(Status::Exited(0)), "{}", s.report());
    let backups = std::fs::read_dir(&config)
        .unwrap()
        .flatten()
        .filter(|entry| entry.file_name().to_string_lossy().contains(".bak-"))
        .collect::<Vec<_>>();
    assert_eq!(backups.len(), 1);
    let backup = backups[0].path().canonicalize().unwrap();
    assert_eq!(std::fs::read(&backup).unwrap(), original);
    let hex = backup
        .as_os_str()
        .as_bytes()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    assert_eq!(
        row,
        format!("OK prime=1 operation=legacy-hooks affected=1 backup={hex}\n").as_bytes()
    );
    let after = std::fs::read_to_string(&settings).unwrap();
    assert!(after.contains("echo my footer"));
    assert!(after.contains("opus"));
    assert!(!after.contains("aterm link hook run"));
    assert_eq!(
        run(&s, s.command(&args)).1,
        b"OK prime=1 operation=legacy-hooks affected=0 backup=none\n"
    );
    assert_eq!(std::fs::read_to_string(&settings).unwrap(), after);
    assert!(!s.home.join(".claude/settings.json").exists());
    assert!(s.starts().is_empty());
}
