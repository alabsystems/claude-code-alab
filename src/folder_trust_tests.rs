// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Exact-key, framing and closed-policy controls ported from Main6116 trust tests.

use super::*;
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT: AtomicU64 = AtomicU64::new(0);

fn world() -> PathBuf {
    let root = PathBuf::from(format!(
        "/tmp/.ubd/folder-trust-{}-{}-{}.noindex",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&root).unwrap();
    root.canonicalize().unwrap()
}

fn worktree(main: &Path, linked: &Path, name: &str) {
    let admin = main.join(".git/worktrees").join(name);
    std::fs::create_dir_all(&admin).unwrap();
    std::fs::create_dir_all(linked).unwrap();
    std::fs::write(
        linked.join(".git"),
        format!("gitdir: {}\n", admin.display()),
    )
    .unwrap();
    std::fs::write(admin.join("commondir"), "../..\n").unwrap();
    std::fs::write(
        admin.join("gitdir"),
        format!("{}\n", linked.join(".git").display()),
    )
    .unwrap();
}

#[test]
fn trust_keys_keep_repository_subfolder_worktree_and_submodule_distinct() {
    let root = world();
    let main = root.join("main");
    let deep = main.join("src/deep");
    std::fs::create_dir_all(main.join(".git")).unwrap();
    std::fs::create_dir_all(&deep).unwrap();
    assert_eq!(keys::trust_key(&deep), main);
    let linked = root.join("linked");
    worktree(&main, &linked, "linked");
    assert_eq!(keys::trust_key(&linked), main);
    let sub = main.join("vendor/sub");
    std::fs::create_dir_all(&sub).unwrap();
    std::fs::create_dir_all(main.join(".git/modules/sub")).unwrap();
    std::fs::write(sub.join(".git"), "gitdir: ../../.git/modules/sub\n").unwrap();
    assert_eq!(keys::trust_key(&sub), sub);
    let plain = root.join("plain");
    std::fs::create_dir_all(&plain).unwrap();
    assert_eq!(keys::trust_key(&plain), plain);
}

#[test]
fn a_forged_git_file_cannot_borrow_another_worktrees_trust() {
    let root = world();
    let main = root.join("main");
    std::fs::create_dir_all(main.join(".git")).unwrap();
    let linked = root.join("linked");
    worktree(&main, &linked, "linked");
    let forged = root.join("forged");
    std::fs::create_dir_all(&forged).unwrap();
    std::fs::write(
        forged.join(".git"),
        format!("gitdir: {}\n", main.join(".git/worktrees/linked").display()),
    )
    .unwrap();
    assert_eq!(keys::trust_key(&linked), main);
    assert_eq!(keys::trust_key(&forged), forged);
}

#[test]
fn selected_original_image_rejects_a_new_named_file_even_with_identical_bytes() {
    use std::os::unix::fs::PermissionsExt as _;
    let root = world();
    let path = root.join("vendor");
    std::fs::write(&path, b"unexecuted metadata fixture").unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
    let selected = Selected::open(&path).unwrap();
    selected.current().unwrap();
    std::fs::rename(&path, root.join("original-retained")).unwrap();
    std::fs::write(&path, b"unexecuted metadata fixture").unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
    assert!(selected.current().is_err());
}

#[test]
fn selected_vendor_alias_cannot_retarget_while_the_proven_file_stays_unchanged() {
    use std::os::unix::fs::{PermissionsExt as _, symlink};
    let root = world();
    let first = root.join("first");
    let second = root.join("second");
    let alias = root.join("selected");
    for path in [&first, &second] {
        std::fs::write(path, b"unexecuted metadata fixture").unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    symlink(&first, &alias).unwrap();
    let selected = Selected::open(&alias).unwrap();
    selected.current().unwrap();
    std::fs::remove_file(&alias).unwrap();
    symlink(&second, &alias).unwrap();
    // Both physical files still exist with their original bytes. Rejection is
    // about the launch's original spelling, not a replacement of the held file.
    assert_eq!(stamp(&selected.file.metadata().unwrap()), selected.stamp);
    assert!(selected.current().is_err());
}

#[test]
fn only_the_exact_true_profile_key_counts_and_other_vendor_fields_remain_opaque() {
    let root = world();
    let child = root.join("child");
    std::fs::create_dir_all(&child).unwrap();
    let profile = root.join("profile");
    for text in [
        "not json".into(),
        "{}".into(),
        format!(
            "{{\"projects\":{{{}:{{\"hasTrustDialogAccepted\":true}}}}}}",
            quote(root.to_str().unwrap())
        ),
        format!(
            "{{\"projects\":{{{}:{{\"hasTrustDialogAccepted\":false}}}}}}",
            quote(child.to_str().unwrap())
        ),
    ] {
        std::fs::write(&profile, text).unwrap();
        assert!(!keys::recorded(&profile, &child));
    }
    std::fs::write(&profile,format!("{{\"unknown\":7,\"projects\":{{{}:{{\"hasTrustDialogAccepted\":true,\"opaque\":[]}}}}}}",quote(child.to_str().unwrap()))).unwrap();
    assert!(keys::recorded(&profile, &child));
    std::fs::write(&profile, "{\"projects\":{},\"projects\":{}}").unwrap();
    assert!(!keys::recorded(&profile, &child));
}

#[test]
fn selected_profile_keeps_explicit_config_custom_oauth_and_legacy_precedence() {
    let root = world();
    let mut env = BTreeMap::new();
    assert_eq!(keys::profile(&root, &env), Some(root.join(".claude.json")));
    env.insert("CLAUDE_CONFIG_DIR".into(), root.join("selected").into());
    std::fs::create_dir_all(root.join("selected")).unwrap();
    assert_eq!(
        keys::profile(&root, &env),
        Some(root.join("selected/.claude.json"))
    );
    env.insert("CLAUDE_CODE_CUSTOM_OAUTH_URL".into(), "custom".into());
    assert_eq!(
        keys::profile(&root, &env),
        Some(root.join("selected/.claude-custom-oauth.json"))
    );
    std::fs::write(root.join("selected/.config.json"), "{}").unwrap();
    assert_eq!(
        keys::profile(&root, &env),
        Some(root.join("selected/.config.json"))
    );
    env.insert("CLAUDE_CONFIG_DIR".into(), "relative".into());
    assert_eq!(keys::profile(&root, &env), None);
}

#[test]
fn interactive_classification_uses_the_same_resume_grammar_and_preserves_settings() {
    let words = |args: &[&str]| args.iter().map(OsString::from).collect::<Vec<_>>();
    for args in [
        &["--model", "opus"][..],
        &["--settings", "{}"][..],
        &["--permission-mode=plan"][..],
    ] {
        assert!(interactive(&words(args)), "{args:?}");
    }
    for args in [
        &["-p", "hello"][..],
        &["--print"][..],
        &["mcp", "list"][..],
        &["doctor"][..],
        &["--bare"][..],
        &["--unknown-flag"][..],
    ] {
        assert!(!interactive(&words(args)), "{args:?}");
    }
}

fn allowed(cwd: &Path, vendor: &Path) -> String {
    format!(
        "{{\"schema\":1,\"operation\":\"folder-trust-gate\",\"status\":\"allow\",\"cwd_hex\":{},\"vendor_hex\":{},\"session\":\"t0000000000\",\"stdout_tty\":true,\"rule\":\"trusted-folder\",\"reason\":null}}\n",
        quote(&gate::hex(cwd).unwrap()),
        quote(&gate::hex(vendor).unwrap())
    )
}

#[test]
fn gate_requires_closed_complete_echoes_and_no_terminal_controls() {
    let cwd = Path::new("/work");
    let vendor = Path::new("/vendor");
    let request = gate::Request::new(cwd, vendor, "t0000000000", true).unwrap();
    let good = allowed(cwd, vendor);
    assert_eq!(request.decode(&good).as_deref(), Some("trusted-folder"));
    for bad in [
        good.trim_end().to_owned(),
        good.replace("\"schema\":1", "\"schema\":2"),
        good.replace("true", "false"),
        good.replace("t0000000000", "t0000000001"),
        good.replace("trusted-folder", "bad\\u001btext"),
        good.replace("\"reason\":null", "\"reason\":null,\"extra\":0"),
        good.replace("\"schema\":1", "\"schema\":1,\"schema\":1"),
        good.replace(&gate::hex(vendor).unwrap(), "2f6f74686572"),
    ] {
        assert_eq!(request.decode(&bad), None, "{bad}");
    }
    assert!(gate::Request::new(cwd, vendor, "bad\nword", true).is_err());
}

#[test]
fn sdk_accepts_only_one_matching_explicit_recorded_answer_without_model_output() {
    let answer = |body: &str| {
        format!(
            "{{\"type\":\"control_response\",\"response\":{{\"subtype\":\"success\",\"request_id\":\"fresh\",\"response\":{body}}}}}"
        )
    };
    assert_eq!(
        exchange::reply(
            &answer("{\"workspace_trust_recorded\":true,\"claude_code_version\":\"2.1.323\"}"),
            "fresh"
        )
        .unwrap(),
        Some(Some("2.1.323".into()))
    );
    for body in [
        "{}",
        "{\"workspace_trust_recorded\":false}",
        "{\"workspace_trust_recorded\":\"true\"}",
    ] {
        assert_eq!(exchange::reply(&answer(body), "fresh").unwrap(), Some(None));
    }
    assert!(exchange::reply(&answer("{}"), "stale").is_err());
    assert!(exchange::reply("{\"type\":\"assistant\"}", "fresh").is_err());
    let frame = String::from_utf8(exchange::request("/work/quote\"\n", "fresh")).unwrap();
    let value = crate::json::parse(&frame).unwrap();
    assert_eq!(
        value
            .get("request")
            .unwrap()
            .get("workspaceTrust")
            .unwrap()
            .get("directory")
            .and_then(crate::json::Value::as_str),
        Some("/work/quote\"\n")
    );
    assert!(exchange::ARGUMENTS.contains(&"--strict-mcp-config"));
    assert!(exchange::ARGUMENTS.contains(&r#"{"disableAllHooks":true}"#));
}
