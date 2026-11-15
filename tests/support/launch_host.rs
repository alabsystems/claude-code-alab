// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! A real helper image, used by the managed launch tests. Its log proves each
//! tap/token/update invocation and its executable reaches the body before the
//! scenario's hard links can be removed.

use super::*;

/// This role runs in a fresh process, before this binary's controller installs
/// its own wait policy. No test-runner thread changes signal dispositions.
pub(super) fn signal_parent_entry() {
    let mut args = std::env::args_os().skip(1);
    if args.next().as_deref() != Some(std::ffi::OsStr::new("--launch-signal-parent")) {
        return;
    }
    let mode = args.next().expect("inherited signal mode");
    let owner = PathBuf::from(args.next().expect("actual owner image"));
    let dir = PathBuf::from(std::env::var_os("FAKE_CLAUDE_DIR").expect("private fake"));
    // SAFETY: this role is a fresh single-threaded process with no children.
    let mut action: libc::sigaction = unsafe { std::mem::zeroed() };
    action.sa_sigaction = if mode == "ignored" {
        libc::SIG_IGN
    } else {
        libc::SIG_DFL
    };
    if mode == "no-cldwait" {
        action.sa_flags = libc::SA_NOCLDWAIT;
    } else {
        assert!(mode == "default" || mode == "ignored");
    }
    // SAFETY: initialized native storage and one valid signal disposition.
    assert_eq!(unsafe { libc::sigemptyset(&raw mut action.sa_mask) }, 0);
    assert_eq!(
        // SAFETY: this isolated pre-exec role alone changes its own SIGCHLD policy.
        unsafe { libc::sigaction(libc::SIGCHLD, &raw const action, std::ptr::null_mut()) },
        0
    );
    // SAFETY: initialized writable native mask storage.
    let mut blocked: libc::sigset_t = unsafe { std::mem::zeroed() };
    // SAFETY: initialize and add one valid signal to this role's own mask.
    assert_eq!(unsafe { libc::sigemptyset(&raw mut blocked) }, 0);
    assert_eq!(
        // SAFETY: SIGUSR2 is valid; blocked is initialized writable native storage.
        unsafe { libc::sigaddset(&raw mut blocked, libc::SIGUSR2) },
        0
    );
    assert_eq!(
        // SAFETY: no other thread or child exists; only this role's mask changes.
        unsafe { libc::sigprocmask(libc::SIG_BLOCK, &raw const blocked, std::ptr::null_mut()) },
        0
    );
    record_signal_state(&dir, "driver-signals");
    std::fs::write(
        dir.join("signal-parent-ready"),
        b"signal parent body reached\n",
    )
    .unwrap();
    assert!(
        wait_until(Duration::from_secs(60), || dir
            .join("signal-parent-go")
            .exists()),
        "signal parent gate hang detector"
    );
    let error = Command::new(owner).args(args).exec();
    panic!("actual owner exec refused: {error}");
}

pub(super) fn record_signal_state(dir: &Path, name: &str) {
    if std::env::var_os("ALAB_TEST_SIGNAL_PROOF").is_none() {
        return;
    }
    // SAFETY: exact writable native action/mask query storage.
    let (mut action, mut mask): (libc::sigaction, libc::sigset_t) = unsafe { std::mem::zeroed() };
    assert_eq!(
        // SAFETY: query only this process's valid signal disposition and mask.
        unsafe { libc::sigaction(libc::SIGCHLD, std::ptr::null(), &raw mut action) },
        0
    );
    assert_eq!(
        // SAFETY: null set queries this process's mask without changing it.
        unsafe { libc::sigprocmask(libc::SIG_BLOCK, std::ptr::null(), &raw mut mask) },
        0
    );
    // SAFETY: the queried mask is initialized and SIGUSR2 is valid.
    let usr2 = unsafe { libc::sigismember(&raw const mask, libc::SIGUSR2) };
    let mut blocked = Vec::new();
    for signal in 1..=128 {
        // SAFETY: mask is initialized; unsupported signal numbers report EINVAL.
        if unsafe { libc::sigismember(&raw const mask, signal) } == 1 {
            blocked.push(signal.to_string());
        }
    }
    let disposition = if action.sa_sigaction == libc::SIG_IGN {
        "ignored"
    } else if action.sa_sigaction == libc::SIG_DFL {
        "default"
    } else {
        "caught"
    };
    std::fs::write(
        dir.join(name),
        format!(
            "pid={}\nchld={disposition}\nno-cldwait={}\nusr2-blocked={}\nblocked={}\n",
            std::process::id(),
            u8::from(action.sa_flags & libc::SA_NOCLDWAIT != 0),
            usr2,
            blocked.join(",")
        ),
    )
    .unwrap();
}

pub(super) fn entry() {
    let Some(host) = std::env::var_os("ALAB_TEST_HOST_IMAGE") else {
        return;
    };
    if std::env::current_exe().ok().as_deref() != Some(Path::new(&host)) {
        return;
    }
    let args: Vec<_> = std::env::args().skip(1).collect();
    if std::env::var_os("ALAB_TEST_HOST_MUST_NOT_RUN").is_some() {
        panic!("information query invoked the host: {args:?}");
    }
    let dir = PathBuf::from(std::env::var_os("FAKE_CLAUDE_DIR").expect("fake"));
    let mut log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join("host-log"))
        .expect("host log");
    writeln!(log, "{}", args.join("\t")).expect("host invocation");
    match args
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>()
        .as_slice()
    {
        ["renderer-probe"] => {
            let mut command = Command::new("vendor-must-not-run");
            claude_code_alab::launch::renderer(&mut command);
            let chosen = command
                .get_envs()
                .find(|(key, _)| *key == "CLAUDE_CODE_NO_FLICKER")
                .and_then(|(_, value)| value)
                .and_then(|value| value.to_str());
            println!("{}", chosen.unwrap_or("-"));
            std::process::exit(0);
        }
        ["harness", "tap", "--launch", "--", ..] => {
            record_signal_state(&dir, "tap-signals");
            match std::env::var("ALAB_TEST_TAP_MODE").ok().as_deref() {
                Some("empty") => std::process::exit(0),
                Some("failed") => std::process::exit(1),
                Some("reap-parent") => {
                    std::fs::write(dir.join("tap-pid"), std::process::id().to_string()).unwrap();
                    println!(
                        "{{\"statusLine\":{{\"type\":\"command\",\"command\":\"fake-tap\"}}}}"
                    );
                    std::io::stdout().flush().unwrap();
                    let driver = std::fs::read_to_string(dir.join("driver-signals")).unwrap();
                    let expected: i32 = driver
                        .lines()
                        .next()
                        .unwrap()
                        .strip_prefix("pid=")
                        .unwrap()
                        .parse()
                        .unwrap();
                    // The outer controller retains this parent's terminal wait until
                    // reaper-result, so this positive PID cannot be reused here.
                    // SAFETY: getppid is a native read and expected is the pinned parent.
                    assert_eq!(unsafe { libc::getppid() }, expected);
                    // SAFETY: the fixture's unreaped original parent pins expected.
                    assert_eq!(unsafe { libc::kill(expected, libc::SIGUSR1) }, 0);
                    assert!(
                        wait_until(Duration::from_secs(60), || std::fs::metadata(
                            dir.join("reaper-entered")
                        )
                        .is_ok_and(|meta| meta.len() >= 8)),
                        "reaper entry hang detector"
                    );
                    std::process::exit(0);
                }
                Some("sleep") => std::thread::sleep(Duration::from_secs(5)),
                Some("oversized") => {
                    if let Err(error) = std::io::stdout().write_all(&vec![b'x'; 64 * 1024 + 1]) {
                        std::fs::write(dir.join("oversized-write-error"), error.to_string())
                            .unwrap();
                        std::process::exit(91);
                    }
                    std::fs::write(dir.join("oversized-written.pending"), "65537\n").unwrap();
                    std::fs::rename(
                        dir.join("oversized-written.pending"),
                        dir.join("oversized-written"),
                    )
                    .unwrap();
                    std::process::exit(0);
                }
                Some("invalid-utf8") => {
                    std::io::stdout().write_all(&[0xff]).unwrap();
                    std::process::exit(0);
                }
                Some("nul") => {
                    std::io::stdout().write_all(b"{}\0").unwrap();
                    std::process::exit(0);
                }
                _ => {}
            }
            println!("{{\"statusLine\":{{\"type\":\"command\",\"command\":\"fake-tap\"}}}}");
            std::process::exit(0);
        }
        [
            "harness",
            "accounts",
            "token-run",
            "--auto",
            "--",
            vendor,
            rest @ ..,
        ] => {
            record_signal_state(&dir, "token-signals");
            std::fs::write(
                dir.join("renderer"),
                std::env::var("CLAUDE_CODE_NO_FLICKER").unwrap_or_default(),
            )
            .expect("renderer");
            if std::env::var_os("ALAB_TEST_HOST_PAUSE_TOKEN").is_some() {
                let pid = std::process::id();
                std::fs::write(dir.join(format!("host-wait-{pid}")), b"host body reached\n")
                    .expect("host body proof");
                assert!(
                    wait_until(Duration::from_secs(60), || dir
                        .join(format!("host-go-{pid}"))
                        .exists()),
                    "token exec gate hang detector"
                );
            }
            let error = Command::new(vendor).args(rest).exec();
            panic!("cannot exec fake vendor: {error}");
        }
        ["pkg", "update", "claude", "--wait-lock", "1800"] => std::process::exit(23),
        _ => panic!("unexpected host request: {args:?}"),
    }
}

fn helper(s: &Scenario) -> PathBuf {
    let path = s.root.join("aterm-host");
    std::fs::hard_link(std::env::current_exe().expect("test image"), &path).expect("host image");
    path
}

fn command(s: &Scenario, host: &Path, args: &[&str]) -> Command {
    let mut command = s.command(args);
    command
        .env("ATERM_HARNESS_HOST", host)
        .env("ALAB_TEST_HOST_IMAGE", host)
        .env("ATERM_PARENT_SESSION_ID", "scenario")
        .env_remove("CLAUDE_CODE_NO_FLICKER")
        .env_remove("CLAUDE_CODE_DISABLE_ALTERNATE_SCREEN");
    command
}

fn signal_launch(s: &Scenario, host: &Path, mode: &str, tap: &str) -> std::process::Output {
    signal_launch_entry(s, host, mode, tap, None)
}

fn signal_launch_entry(
    s: &Scenario,
    host: &Path,
    mode: &str,
    tap: &str,
    entry: Option<&str>,
) -> std::process::Output {
    let vendor = s.store.join(BUILD_1.to_string()).join("bin/claude");
    let direct_args = [
        "harness",
        "accounts",
        "token-run",
        "--auto",
        "--",
        vendor.to_str().unwrap(),
        "--dangerously-skip-permissions",
        "-p",
        "a task",
    ];
    let prepared = command(
        s,
        host,
        if matches!(entry, Some("direct-no-cldwait" | "direct-inherited")) {
            &direct_args
        } else {
            &["launch", "--", "-p", "a task"]
        },
    );
    let owner = if entry.is_some() {
        std::env::current_exe().unwrap()
    } else {
        std::env::var_os("CLAUDE_ALAB_TEST_OWNER")
            .map_or_else(|| PathBuf::from(HARNESS), PathBuf::from)
    };
    assert!(
        owner.is_absolute() && owner.is_file(),
        "explicit actual owner image"
    );
    eprintln!("actual launch owner: {}", owner.display());
    let mut driver = Command::new(std::env::current_exe().unwrap());
    driver
        .args([
            std::ffi::OsStr::new("--launch-signal-parent"),
            std::ffi::OsStr::new(mode),
            owner.as_os_str(),
        ])
        .args(prepared.get_args());
    driver.current_dir(prepared.get_current_dir().unwrap());
    for (key, value) in prepared.get_envs() {
        if let Some(value) = value {
            driver.env(key, value);
        } else {
            driver.env_remove(key);
        }
    }
    if let Some(entry) = entry {
        driver
            .env("ALAB_TEST_QUERY_ENTRY_IMAGE", &owner)
            .env("ALAB_TEST_QUERY_ENTRY_KIND", entry);
    }
    driver.process_group(0);
    let stdout = s.root.join("signal-launch.stdout");
    let stderr = s.root.join("signal-launch.stderr");
    driver
        .env("ALAB_TEST_SIGNAL_PROOF", "1")
        .env("ALAB_TEST_TAP_MODE", tap)
        .stdin(Stdio::null())
        .stdout(std::fs::File::create(&stdout).unwrap())
        .stderr(std::fs::File::create(&stderr).unwrap());
    let mut child = driver.spawn().expect("isolated signal parent");
    assert!(
        wait_until(Duration::from_secs(60), || s
            .fake
            .join("signal-parent-ready")
            .exists()),
        "signal parent body hang detector{}",
        s.report()
    );
    s.guard_group(child.id() as i32);
    std::fs::write(s.fake.join("signal-parent-go"), b"go").unwrap();
    if entry == Some("foreign-reaper") {
        // Keep the parent's native terminal event unconsumed for the entire
        // helper-to-parent signal transaction, including a parent that exits.
        assert!(
            wait_until(Duration::from_secs(60), || s
                .fake
                .join("reaper-result")
                .exists()),
            "foreign wait completion hang detector{}",
            s.report()
        );
    }
    let mut status = None;
    let settled = wait_until(Duration::from_secs(60), || {
        status = child.try_wait().expect("owned signal parent wait");
        status.is_some()
    });
    if !settled {
        s.with_owned_groups(|leader| {
            // SAFETY: the original native lifetime plus non-consuming wait pins this group.
            unsafe { libc::kill(-(leader.pid as i32), libc::SIGKILL) };
        });
        if !wait_until(Duration::from_secs(60), || {
            child.try_wait().is_ok_and(|status| status.is_some())
        }) {
            eprintln!(
                "unsettled original signal controller; retained {}",
                s.root.display()
            );
            std::mem::forget(child);
            std::process::exit(70);
        }
        panic!("actual launch exceeded its hang detector{}", s.report());
    }
    let pid = child.id() as i32;
    let output = std::process::Output {
        status: status.unwrap(),
        stdout: std::fs::read(stdout).unwrap(),
        stderr: std::fs::read(stderr).unwrap(),
    };
    if entry != Some("foreign-reaper") {
        assert_eq!(
            s.starts().first().map(|start| start.pid),
            Some(pid),
            "same-PID vendor entry{}",
            s.report()
        );
    }
    output
}

pub(super) fn managed_usage_query_keeps_inherited_wait_policy_and_vendor_signal_state() {
    let fields = |text: String| text.lines().skip(1).collect::<Vec<_>>().join("\n");
    for mode in ["default", "ignored"] {
        let control = signal_scenario(&format!("managed-query-{mode}-direct"));
        let control_host = helper(&control);
        let out = signal_launch_entry(
            &control,
            &control_host,
            mode,
            "fast",
            Some("direct-inherited"),
        );
        assert_eq!(out.status.code(), Some(7), "{out:?}{}", control.report());
        let expected_token =
            fields(std::fs::read_to_string(control.fake.join("token-signals")).unwrap());
        let expected_vendor =
            fields(std::fs::read_to_string(control.fake.join("vendor-signals")).unwrap());
        let s = Scenario::new(&format!("managed-query-{mode}"));
        s.add_build(BUILD_1, Some("9.9.1"));
        s.set_current(BUILD_1);
        s.put("mode", "echo:7");
        let host = helper(&s);
        let output = signal_launch(&s, &host, mode, "fast");
        assert_eq!(output.status.code(), Some(7), "{output:?}{}", s.report());
        let driver = std::fs::read_to_string(s.fake.join("driver-signals")).unwrap();
        assert!(driver.contains("usr2-blocked=1"));
        assert!(driver.contains(&format!(
            "chld={}\n",
            if mode == "ignored" {
                "ignored"
            } else {
                "default"
            }
        )));
        if mode == "default" {
            assert!(driver.contains("no-cldwait=0\n"));
        }
        assert_eq!(
            fields(std::fs::read_to_string(s.fake.join("token-signals")).unwrap()),
            expected_token,
            "token exec matches the direct native transition"
        );
        assert_eq!(
            fields(std::fs::read_to_string(s.fake.join("vendor-signals")).unwrap()),
            expected_vendor,
            "vendor exec matches the direct native transition"
        );
        let log = std::fs::read_to_string(s.fake.join("host-log")).unwrap();
        assert_eq!(
            log.lines().count(),
            if mode == "default" { 2 } else { 1 },
            "unwaitable tap must refuse before spawn: {log}{}",
            s.report()
        );
        assert_eq!(s.fake.join("tap-signals").exists(), mode == "default");
        let args = &s.starts()[0].args;
        assert_eq!(
            args.iter().any(|arg| arg == "--settings"),
            mode == "default"
        );
    }
    let control = signal_scenario("managed-query-no-cldwait-control");
    let host = helper(&control);
    let out = signal_launch_entry(
        &control,
        &host,
        "default",
        "fast",
        Some("direct-no-cldwait"),
    );
    assert_eq!(out.status.code(), Some(7), "{out:?}{}", control.report());
    let entry = std::fs::read_to_string(control.fake.join("entry-signals")).unwrap();
    assert!(entry.contains("no-cldwait=1"));
    let expected_token =
        fields(std::fs::read_to_string(control.fake.join("token-signals")).unwrap());
    let expected_vendor =
        fields(std::fs::read_to_string(control.fake.join("vendor-signals")).unwrap());
    let s = signal_scenario("managed-query-no-cldwait-cli");
    let host = helper(&s);
    let out = signal_launch_entry(&s, &host, "default", "fast", Some("no-cldwait"));
    assert_eq!(out.status.code(), Some(7), "{out:?}{}", s.report());
    assert!(
        std::fs::read_to_string(s.fake.join("entry-signals"))
            .unwrap()
            .contains("no-cldwait=1")
    );
    assert_eq!(
        fields(std::fs::read_to_string(s.fake.join("token-signals")).unwrap()),
        expected_token
    );
    assert_eq!(
        fields(std::fs::read_to_string(s.fake.join("vendor-signals")).unwrap()),
        expected_vendor
    );
    assert_eq!(
        std::fs::read_to_string(s.fake.join("host-log"))
            .unwrap()
            .lines()
            .count(),
        1,
        "a real entry with no wait ownership starts no tap helper"
    );
    assert!(!s.fake.join("tap-signals").exists());
}

fn signal_scenario(name: &str) -> Scenario {
    let s = Scenario::new(name);
    // This actor calls the actual CLI library in the test image. A separate
    // native inode for the vendor preserves the product's self-image refusal.
    let vendor = s.root.join("vendor-image");
    std::fs::copy(std::env::current_exe().unwrap(), &vendor).unwrap();
    s.add_build_of(BUILD_1, Some("9.9.1"), &vendor);
    s.set_current(BUILD_1);
    s.put("mode", "echo:7");
    s
}

pub(super) fn managed_usage_query_retains_a_foreign_native_wait_at_the_executable_boundary() {
    let s = signal_scenario("managed-query-foreign-wait");
    let host = helper(&s);
    let output = signal_launch_entry(&s, &host, "default", "reap-parent", Some("foreign-reaper"));
    assert_eq!(output.status.code(), Some(70), "{output:?}{}", s.report());
    assert!(
        s.starts().is_empty(),
        "no vendor may start with lost helper custody"
    );
    let log = std::fs::read_to_string(s.fake.join("host-log")).unwrap();
    assert_eq!(
        log.lines().count(),
        1,
        "no token route after lost wait: {log}"
    );
    assert!(log.starts_with("harness\ttap\t--launch"));
    let actual = std::fs::read_to_string(s.fake.join("reaper-result")).unwrap();
    let helper = std::fs::read_to_string(s.fake.join("tap-pid")).unwrap();
    assert_eq!(
        actual.trim(),
        helper.trim(),
        "the actual helper's terminal event was consumed by the foreign waiter"
    );
}

pub(super) fn managed_usage_query_is_bounded_in_the_actual_single_threaded_cli() {
    for tap in [
        "fast",
        "empty",
        "failed",
        "sleep",
        "oversized",
        "invalid-utf8",
        "nul",
    ] {
        let s = Scenario::new(&format!("managed-query-output-{tap}"));
        s.add_build(BUILD_1, Some("9.9.1"));
        s.set_current(BUILD_1);
        s.put("mode", "echo:7");
        let host = helper(&s);
        let output = signal_launch(&s, &host, "default", tap);
        assert_eq!(
            output.status.code(),
            Some(7),
            "{tap}: {output:?}{}",
            s.report()
        );
        let log = std::fs::read_to_string(s.fake.join("host-log")).unwrap();
        assert_eq!(log.lines().count(), 2, "one tap and one token exec: {log}");
        let expected: Vec<String> = if tap == "fast" {
            vec![
                "--settings",
                "{\"statusLine\":{\"type\":\"command\",\"command\":\"fake-tap\"}}",
                "--dangerously-skip-permissions",
                "-p",
                "a task",
            ]
        } else {
            vec!["--dangerously-skip-permissions", "-p", "a task"]
        }
        .into_iter()
        .map(str::to_owned)
        .collect();
        assert_eq!(s.starts()[0].args, expected, "{tap}{}", s.report());
        if tap == "oversized" {
            assert!(
                !s.fake.join("oversized-write-error").exists(),
                "the fixture must not silently accept a failed body write"
            );
            if let Ok(written) = std::fs::read_to_string(s.fake.join("oversized-written")) {
                assert_eq!(written, "65537\n");
            }
        }
    }
}

pub(super) fn managed_launch_owns_arguments_renderer_and_host_services() {
    for entry in ["run", "launch"] {
        managed_launch_for(entry);
    }
}

fn managed_launch_for(entry: &str) {
    let s = Scenario::new(&format!("managed-{entry}"));
    s.add_build(BUILD_1, Some("9.9.1"));
    s.set_current(BUILD_1);
    s.put("mode", "echo:7");
    let host = helper(&s);
    let output = command(&s, &host, &[entry, "--", "-p", "a task"])
        .stdin(Stdio::null())
        .output()
        .expect("managed launch");
    assert_eq!(output.status.code(), Some(7), "{output:?}{}", s.report());
    let starts = s.starts();
    assert_eq!(starts.len(), 1, "{}", s.report());
    assert_eq!(
        starts[0].args,
        [
            "--settings",
            "{\"statusLine\":{\"type\":\"command\",\"command\":\"fake-tap\"}}",
            "--dangerously-skip-permissions",
            "-p",
            "a task"
        ]
    );
    let log = std::fs::read_to_string(s.fake.join("host-log")).expect("host log");
    assert_eq!(log.lines().count(), 2, "tap and token route: {log}");
    assert!(
        log.starts_with("harness\ttap\t--launch\t--\t--dangerously-skip-permissions"),
        "{log}"
    );
    assert_eq!(
        std::fs::read_to_string(s.fake.join("renderer")).expect("renderer"),
        "1"
    );

    for (environment, settings, expected) in [
        (Some(""), None, ""),
        (Some("0"), None, "0"),
        (None, Some("{\"tui\":\"classic\"}"), ""),
    ] {
        let config = s.home.join(".claude");
        std::fs::create_dir_all(&config).expect("config");
        if let Some(settings) = settings {
            std::fs::write(config.join("settings.json"), settings).expect("settings")
        }
        let mut launch = command(
            &s,
            &host,
            &[entry, "--", "--permission-mode=plan", "-p", "a task"],
        );
        if let Some(value) = environment {
            launch.env("CLAUDE_CODE_NO_FLICKER", value);
        }
        let output = launch
            .stdin(Stdio::null())
            .output()
            .expect("chosen renderer");
        assert_eq!(output.status.code(), Some(7), "{output:?}{}", s.report());
        assert_eq!(
            std::fs::read_to_string(s.fake.join("renderer")).expect("renderer"),
            expected
        );
        assert!(
            !s.starts()
                .last()
                .expect("start")
                .args
                .iter()
                .any(|arg| arg == "--dangerously-skip-permissions")
        );
    }

    let calls = std::fs::read_to_string(s.fake.join("host-log")).expect("log");
    let output = command(&s, &host, &[entry, "--", "--no-harness", "-p", "a task"])
        .stdin(Stdio::null())
        .output()
        .expect("bypass");
    assert_eq!(output.status.code(), Some(7), "{output:?}{}", s.report());
    assert_eq!(
        s.starts().last().expect("start").args,
        ["--dangerously-skip-permissions", "-p", "a task"]
    );
    assert_eq!(
        std::fs::read_to_string(s.fake.join("host-log")).expect("log"),
        calls,
        "bypass never invokes a host service"
    );

    for mode in ["empty", "failed"] {
        let output = command(
            &s,
            &host,
            &[
                entry,
                "--",
                "--settings",
                "{\"custom\":true}",
                "-p",
                "a task",
            ],
        )
        .env("ALAB_TEST_TAP_MODE", mode)
        .stdin(Stdio::null())
        .output()
        .expect("tap refused");
        assert_eq!(output.status.code(), Some(7), "{output:?}{}", s.report());
        assert_eq!(
            s.starts().last().expect("start").args,
            ["--settings", "{\"custom\":true}", "-p", "a task"]
        );
    }
    let output = command(
        &s,
        &host,
        &[
            entry,
            "--",
            "--settings",
            "{\"custom\":true}",
            "-p",
            "a task",
        ],
    )
    .stdin(Stdio::null())
    .output()
    .expect("tap and explicit settings");
    assert_eq!(output.status.code(), Some(7), "{output:?}{}", s.report());
    let last = s.starts().pop().expect("start").args;
    assert_eq!(last[0], "--settings");
    assert_eq!(
        &last[2..],
        ["--settings", "{\"custom\":true}", "-p", "a task"]
    );

    let config = s.root.join("chosen config");
    std::fs::create_dir_all(&config).expect("custom config");
    std::fs::write(config.join("settings.json"), "{\"tui\":\"classic\"}").expect("settings");
    let output = command(&s, &host, &[entry, "--", "-p", "a task"])
        .env("CLAUDE_CONFIG_DIR", &config)
        .stdin(Stdio::null())
        .output()
        .expect("configured renderer");
    assert_eq!(output.status.code(), Some(7), "{output:?}{}", s.report());
    assert_eq!(
        std::fs::read_to_string(s.fake.join("renderer")).expect("renderer"),
        ""
    );
}

pub(super) fn managed_token_route_survives_vendor_replacement() {
    let s = Scenario::new("managed-replacement");
    s.add_build(BUILD_1, Some("9.9.1"));
    s.add_build(BUILD_2, Some("9.9.2"));
    s.set_current(BUILD_1);
    let host = helper(&s);
    let pty = Pty::open();
    let command = command(
        &s,
        &host,
        &[
            "--alab-quiet-ms=100",
            "--alab-poll-ms=40",
            "run",
            "--",
            "--model",
            "opus",
        ],
    );
    let (_child, pid) = spawn_on_pty(&s, &pty, command, None);
    assert!(
        wait_until(Duration::from_secs(60), || s
            .log()
            .iter()
            .any(|line| line.starts_with("modes "))),
        "{}",
        s.report()
    );
    s.set_current(BUILD_2);
    assert!(
        wait_until(Duration::from_secs(60), || s.starts().len() >= 2),
        "{}\n{}",
        s.report(),
        pty.output()
    );
    let log = std::fs::read_to_string(s.fake.join("host-log")).expect("host log");
    assert_eq!(
        log.lines()
            .filter(|line| line.starts_with("harness\taccounts\ttoken-run\t"))
            .count(),
        2,
        "{log}"
    );
    assert_eq!(
        log.lines()
            .filter(|line| line.starts_with("harness\ttap\t"))
            .count(),
        1,
        "tap is prepared once: {log}"
    );
    assert_eq!(s.starts()[1].build(), Some(BUILD_2));
    assert!(
        s.starts()[1]
            .args
            .windows(2)
            .any(|words| words == ["--resume", SID])
    );
    s.put("command", "exit:5");
    assert_eq!(
        wait_status(pid, Duration::from_secs(60)),
        Some(Status::Exited(5)),
        "{}",
        s.report()
    );
}

/// Different helper and vendor images share one native process lifetime. A
/// negotiated claim names that lifetime even before exec, and the supervisor
/// pins only the actual vendor for its unchanged own-image transfer witness.
pub(super) fn managed_child_proof_and_image_pinning_follow_the_token_exec() {
    use std::os::unix::fs::MetadataExt as _;
    let s = Scenario::new("managed-child-image");
    s.add_build(BUILD_1, Some("9.9.1"));
    s.add_build(BUILD_2, Some("9.9.2"));
    s.set_current(BUILD_1);
    let host = s.root.join("distinct-aterm-host");
    std::fs::copy(std::env::current_exe().unwrap(), &host).expect("distinct helper image");
    let fake = rostered_aterm(&s);
    fake.set_info(&format!("{INFO_ROSTERED} child-proof=1"));
    let pty = Pty::open();
    let mut launch = command(
        &s,
        &host,
        &[
            "--alab-quiet-ms=100",
            "--alab-poll-ms=40",
            "run",
            "--",
            "--model",
            "opus",
        ],
    );
    launch
        .envs(fake.env())
        .env("ALAB_TEST_HOST_PAUSE_TOKEN", "1");
    let (_child, pid) = spawn_guarded(&s, &pty, launch);
    let finish_exec = |build: u64, vendor: &str, nth: usize| {
        let mut helper_pid = None;
        assert!(
            wait_until(Duration::from_secs(60), || {
                helper_pid = std::fs::read_dir(&s.fake)
                    .unwrap()
                    .filter_map(Result::ok)
                    .filter_map(|entry| {
                        entry
                            .file_name()
                            .to_str()
                            .and_then(|name| name.strip_prefix("host-wait-"))
                            .and_then(|pid| pid.parse::<u32>().ok())
                    })
                    .find(|pid| !s.fake.join(format!("host-go-{pid}")).exists());
                helper_pid.is_some()
            }),
            "helper body never reached{}",
            s.report()
        );
        let helper_pid = helper_pid.unwrap();
        let helper = claude_code_alab::procs::capture_process(helper_pid).expect("live helper");
        let expected = std::fs::metadata(s.store.join(format!("{build}/bin/claude"))).unwrap();
        assert_ne!(
            (helper.executable.device, helper.executable.inode),
            (expected.dev(), expected.ino()),
            "the helper fixture must be a different image"
        );
        let birth = match helper.birth {
            claude_code_alab::procs::Birth::MacOs {
                seconds,
                microseconds,
            } => seconds * 1_000_000 + u64::from(microseconds),
            claude_code_alab::procs::Birth::Linux(ticks) => ticks,
        };
        let claim = format!("{} child={helper_pid}:{birth}", claim_text(pid, vendor));
        assert!(
            wait_until(Duration::from_secs(60), || asked(&fake, &claim) == 1),
            "helper lifetime was not announced{}",
            s.report()
        );
        std::fs::write(
            s.fake.join(format!("host-go-{helper_pid}")),
            b"exec vendor\n",
        )
        .unwrap();
        assert_eq!(
            u32::try_from(await_record(&s, nth)).unwrap(),
            helper_pid,
            "token routing must exec in place"
        );
        let actual =
            claude_code_alab::procs::capture_process(helper_pid).expect("vendor body reached");
        assert_eq!(actual.birth, helper.birth);
        assert_eq!(
            (actual.executable.device, actual.executable.inode),
            (expected.dev(), expected.ino())
        );
        let pinned = format!(
            "vendor-image-pinned pid={helper_pid} device={} inode={}",
            expected.dev(),
            expected.ino()
        );
        assert!(
            wait_until(Duration::from_secs(60), || s.journal().contains(&pinned)),
            "the own-image snapshot witness still names the helper{}",
            s.report()
        );
    };
    finish_exec(BUILD_1, "9.9.1", 0);
    fake.event("restart cause=asked");
    finish_exec(BUILD_1, "9.9.1", 1);
    s.set_current(BUILD_2);
    finish_exec(BUILD_2, "9.9.2", 2);
    assert_eq!(
        claims_sent(&fake),
        4,
        "one initial claim and one per new child"
    );
    let log = std::fs::read_to_string(s.fake.join("host-log")).unwrap();
    assert_eq!(
        log.lines()
            .filter(|line| line.starts_with("harness\taccounts\ttoken-run\t"))
            .count(),
        3,
        "one token route per vendor: {log}"
    );
    assert_eq!(
        log.lines()
            .filter(|line| line.starts_with("harness\ttap\t"))
            .count(),
        1,
        "one tap preparation per launch: {log}"
    );
    assert_eq!(s.starts().len(), 3);
    assert_eq!(s.starts()[2].build(), Some(BUILD_2));
    assert!(
        s.starts()[2]
            .args
            .windows(2)
            .any(|words| words == ["--resume", SID])
    );
    end_session(&s, pid);
}

pub(super) fn managed_update_needs_no_installed_vendor() {
    for entry in ["run", "launch"] {
        managed_update_for(entry);
    }
}

fn managed_update_for(entry: &str) {
    let s = Scenario::new("managed-update");
    let host = helper(&s);
    let output = command(&s, &host, &[entry, "--", "upgrade"])
        .stdin(Stdio::null())
        .output()
        .expect("update");
    assert_eq!(output.status.code(), Some(23), "{output:?}{}", s.report());
    assert_eq!(
        std::fs::read_to_string(s.fake.join("host-log")).expect("log"),
        "pkg\tupdate\tclaude\t--wait-lock\t1800\n"
    );
    assert!(s.starts().is_empty());
}

pub(super) fn managed_information_queries_never_invoke_host_services() {
    for entry in ["run", "launch"] {
        managed_information_for(entry);
    }
}

fn managed_information_for(entry: &str) {
    let s = Scenario::new("managed-information");
    s.add_build(BUILD_1, Some("9.9.1"));
    s.set_current(BUILD_1);
    s.put("mode", "echo:7");
    let host = helper(&s);
    for words in [
        &["--version"][..],
        &["-v"],
        &["--help"],
        &["-h"],
        &["install", "--help"],
    ] {
        let mut args = vec![entry, "--"];
        args.extend_from_slice(words);
        let output = command(&s, &host, &args)
            .env("ALAB_TEST_HOST_MUST_NOT_RUN", "1")
            .stdin(Stdio::null())
            .output()
            .expect("information query");
        if words == ["--version"] {
            assert_eq!(output.status.code(), Some(0), "{output:?}{}", s.report());
            assert_eq!(output.stdout, b"9.9.9 (Claude Code)\n");
        } else {
            assert_eq!(output.status.code(), Some(7), "{output:?}{}", s.report());
            assert_eq!(s.starts().last().expect("vendor start").args, words);
        }
        assert!(!s.fake.join("host-log").exists(), "host was invoked");
        assert!(
            !s.fake.join("renderer").exists(),
            "renderer helper was invoked"
        );
    }
}

pub(super) fn leading_word_metadata_opens_no_host_vendor_or_store_state() {
    let s = Scenario::new("leading-metadata");
    let host = helper(&s);
    // No store build is present. Even malformed environment must not divert
    // the metadata query through launch machinery.
    let missing_home = s.root.join("absent-home");
    for args in [
        &["--alab-leads=claude"][..],
        &["--alab-leads=muse"],
        &["--alab-leads=unknown"],
        &["--alab-leads="],
        &["--alab-leads"],
        &["--alab-leads=claude", "run", "--"],
        &["--alab-launches=claude"],
        &["--alab-launches=muse"],
        &["--alab-launches=unknown"],
        &["--alab-launches="],
        &["--alab-launches"],
        &["--alab-launches=claude", "launch", "--"],
        &["--alab-launches=claude", "--alab-leads=claude"],
    ] {
        let output = command(&s, &host, args)
            .env("HOME", &missing_home)
            .env(claude_code_alab::cli::DEPTH_VAR, "invalid")
            .env("ALAB_TEST_HOST_MUST_NOT_RUN", "1")
            .stdin(Stdio::null())
            .output()
            .expect("metadata request");
        let expected = if args == ["--alab-launches=claude"] {
            0
        } else {
            1
        };
        assert_eq!(output.status.code(), Some(expected), "{args:?}: {output:?}");
        assert!(
            output.stdout.is_empty() && output.stderr.is_empty(),
            "{output:?}"
        );
        assert!(!missing_home.exists(), "metadata created state");
        assert!(s.starts().is_empty(), "metadata launched vendor");
        assert!(!s.fake.join("host-log").exists(), "metadata launched host");
    }
}

pub(super) fn launch_only_executes_the_vendor_in_place_even_inside_a_rostered_aterm() {
    let s = Scenario::new("launch-only-pty");
    s.add_build(BUILD_1, Some("9.9.1"));
    s.add_build(BUILD_2, Some("9.9.2"));
    s.set_current(BUILD_1);
    let host = helper(&s);
    let fake = rostered_aterm(&s);
    let pty = Pty::open();
    let mut launch = command(&s, &host, &["launch", "--", "--model", "opus", "a task"]);
    launch.envs(fake.env());
    let (_child, pid) = spawn_on_pty(&s, &pty, launch, None);
    assert_eq!(
        s.starts()[0].pid,
        pid,
        "the vendor replaces the launcher{}",
        s.report()
    );
    assert_eq!(
        fake.connections(),
        0,
        "launch does not discover or claim the host"
    );
    assert!(
        s.starts()[0]
            .args
            .contains(&"--dangerously-skip-permissions".into())
    );
    let calls = std::fs::read_to_string(s.fake.join("host-log")).expect("host calls");
    assert_eq!(
        calls.lines().count(),
        2,
        "tap setup and same-PID token exec: {calls}"
    );
    s.set_current(BUILD_2);
    s.put("command", "exit:5");
    assert_eq!(
        wait_status(pid, Duration::from_secs(60)),
        Some(Status::Exited(5)),
        "{}",
        s.report()
    );
    assert_eq!(s.starts().len(), 1, "no lifecycle supervisor remains");
    assert_eq!(fake.connections(), 0);
    assert!(!s.journal().contains("aterm claimed"));
}

pub(super) fn renderer_honors_explicit_config_without_home() {
    let s = Scenario::new("renderer-without-home");
    let host = helper(&s);
    let config = s.root.join("config");
    std::fs::create_dir(&config).expect("config");
    for (settings, expected) in [("{\"tui\":\"classic\"}", "-\n"), ("{}", "1\n")] {
        std::fs::write(config.join("settings.json"), settings).expect("settings");
        let output = Command::new(&host)
            .arg("renderer-probe")
            .env("ALAB_TEST_HOST_IMAGE", &host)
            .env("FAKE_CLAUDE_DIR", &s.fake)
            .env("ATERM_PARENT_SESSION_ID", "scenario")
            .env_remove("ATERM_NO_HARNESS")
            .env_remove("CLAUDE_CODE_NO_FLICKER")
            .env_remove("CLAUDE_CODE_DISABLE_ALTERNATE_SCREEN")
            .env_remove("HOME")
            .env("CLAUDE_CONFIG_DIR", &config)
            .output()
            .expect("renderer probe");
        assert!(output.status.success(), "{output:?}");
        assert_eq!(String::from_utf8_lossy(&output.stdout), expected);
    }
}

pub(super) fn managed_option_values_stay_exact_and_keep_host_services() {
    let s = Scenario::new("managed-option-values");
    s.add_build(BUILD_1, Some("9.9.1"));
    s.set_current(BUILD_1);
    s.put("mode", "echo:7");
    let host = helper(&s);
    for (at, value) in [
        "--yolo",
        "--no-harness",
        "--no-alab-harness",
        "--help",
        "--version",
    ]
    .into_iter()
    .enumerate()
    {
        let output = command(
            &s,
            &host,
            &["run", "--", "-p", "a task", "--system-prompt", value],
        )
        .stdin(Stdio::null())
        .output()
        .expect("known option value launch");
        assert_eq!(
            output.status.code(),
            Some(7),
            "{value}: {output:?}{}",
            s.report()
        );
        let starts = s.starts();
        assert_eq!(starts.len(), at + 1);
        assert_eq!(
            starts.last().expect("vendor body").args,
            [
                "--settings",
                "{\"statusLine\":{\"type\":\"command\",\"command\":\"fake-tap\"}}",
                "--dangerously-skip-permissions",
                "-p",
                "a task",
                "--system-prompt",
                value
            ]
        );
        let log = std::fs::read_to_string(s.fake.join("host-log")).expect("host services");
        assert_eq!(
            log.lines().count(),
            (at + 1) * 2,
            "tap and token route remain enabled for {value}: {log}"
        );
    }
}

pub(super) fn managed_host_services_share_the_exact_no_harness_boundary() {
    for entry in ["run", "launch"] {
        managed_no_harness_for(entry);
    }
}

fn managed_no_harness_for(entry: &str) {
    let s = Scenario::new("managed-no-harness-boundary");
    s.add_build(BUILD_1, Some("9.9.1"));
    s.set_current(BUILD_1);
    s.put("mode", "echo:7");
    let host = helper(&s);
    for (at, value) in ["0", "true", "", "1"].into_iter().enumerate() {
        let output = command(&s, &host, &[entry, "--", "-p", "a task"])
            .env("ATERM_CHILD", "1")
            .env("ATERM_NO_HARNESS", value)
            .stdin(Stdio::null())
            .output()
            .expect("managed no-harness value");
        assert_eq!(
            output.status.code(),
            Some(7),
            "{value:?}: {output:?}{}",
            s.report()
        );
        let starts = s.starts();
        assert_eq!(starts.len(), at + 1);
        let mut expected = vec!["--dangerously-skip-permissions", "-p", "a task"];
        if value != "1" {
            expected.splice(
                0..0,
                [
                    "--settings",
                    "{\"statusLine\":{\"type\":\"command\",\"command\":\"fake-tap\"}}",
                ],
            );
        }
        assert_eq!(
            starts.last().expect("vendor body").args,
            expected,
            "{value:?}"
        );
        let log = std::fs::read_to_string(s.fake.join("host-log")).expect("host invocations");
        assert_eq!(
            log.lines().count(),
            (at + 1).min(3) * 2,
            "only1 leaves host services out: {value:?}: {log}"
        );
    }
}
