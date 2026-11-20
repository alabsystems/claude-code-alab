// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Real native vendor/host roles for the independent owner's cold-launch boundary.
//! Each role is selected only in this test image at its explicit private image path.

use super::*;
use std::io::BufRead as _;

const ROLE_HOST: &str = "ALAB_TEST_FOLDER_HOST";
const ROLE_VENDOR: &str = "ALAB_TEST_FOLDER_VENDOR";
const ROOT: &str = "ALAB_TEST_FOLDER_ROOT";
const DRIVER: &str = "ALAB_TEST_FOLDER_DRIVER";
const HOST_PROOF: &str = "host-proof";
const VENDOR_PROOF: &str = "vendor-proof";

fn mask() -> Vec<i32> {
    // SAFETY: initialized native output storage for this thread's mask.
    let mut mask: libc::sigset_t = unsafe { std::mem::zeroed() };
    assert_eq!(
        // SAFETY: null new mask queries only the calling thread.
        unsafe { libc::sigprocmask(libc::SIG_BLOCK, std::ptr::null(), &raw mut mask) },
        0
    );
    (1..=128)
        .filter(|signal| {
            // SAFETY: mask is initialized; unsupported numbers return EINVAL.
            (unsafe { libc::sigismember(&raw const mask, *signal) }) == 1
        })
        .collect()
}

fn state(root: &Path, name: &str) {
    // SAFETY: initialized action query output.
    let mut action: libc::sigaction = unsafe { std::mem::zeroed() };
    assert_eq!(
        // SAFETY: a null action queries this process's disposition only.
        unsafe { libc::sigaction(libc::SIGTERM, std::ptr::null(), &raw mut action) },
        0
    );
    std::fs::write(
        root.join(name),
        format!(
            "term-ignored={}\nmask={:?}\n",
            action.sa_sigaction == libc::SIG_IGN,
            mask()
        ),
    )
    .unwrap();
}

fn driver(root: &Path) -> ! {
    claude_code_alab::sys::ensure_single_threaded().unwrap();
    let kind = std::env::var(DRIVER).unwrap();
    // SAFETY: isolated single-threaded fixture owns these native action/mask changes.
    unsafe {
        let mut action: libc::sigaction = std::mem::zeroed();
        libc::sigemptyset(&raw mut action.sa_mask);
        action.sa_sigaction = libc::SIG_DFL;
        for signal in [
            libc::SIGINT,
            libc::SIGQUIT,
            libc::SIGHUP,
            libc::SIGTERM,
            libc::SIGTSTP,
            libc::SIGCONT,
            libc::SIGCHLD,
            libc::SIGWINCH,
        ] {
            assert_eq!(
                libc::sigaction(signal, &raw const action, std::ptr::null_mut()),
                0
            );
        }
        if kind == "ignore-term" {
            action.sa_sigaction = libc::SIG_IGN;
            assert_eq!(
                libc::sigaction(libc::SIGTERM, &raw const action, std::ptr::null_mut()),
                0
            );
        }
        let mut current: libc::sigset_t = std::mem::zeroed();
        assert_eq!(
            libc::sigprocmask(libc::SIG_BLOCK, std::ptr::null(), &raw mut current),
            0
        );
        for &signal in claude_code_alab::sys::HANDLED {
            libc::sigdelset(&raw mut current, signal);
        }
        libc::sigaddset(&raw mut current, libc::SIGUSR2);
        if kind == "block-int" {
            libc::sigaddset(&raw mut current, libc::SIGINT);
        }
        assert_eq!(
            libc::sigprocmask(libc::SIG_SETMASK, &raw const current, std::ptr::null_mut()),
            0
        );
    }
    state(root, "signal-before");
    let error = Command::new(HARNESS)
        .args(std::env::args_os().skip(1))
        .exec();
    panic!("actual signal owner entry refused: {error}");
}

fn event(root: &Path, word: &str) {
    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .create(true)
        .open(root.join("events"))
        .unwrap();
    writeln!(file, "{word}\t{}", std::process::id()).unwrap();
}

fn selected(var: &str) -> bool {
    std::env::var_os(var).is_some_and(|expected| {
        std::env::current_exe()
            .ok()
            .and_then(|path| path.canonicalize().ok())
            .is_some_and(|image| Path::new(&expected).canonicalize().ok().as_ref() == Some(&image))
    })
}

pub(super) fn entry() {
    if std::env::var_os(DRIVER).is_some() && selected("ALAB_TEST_FOLDER_DRIVER_IMAGE") {
        let root = PathBuf::from(std::env::var_os(ROOT).unwrap());
        driver(&root);
    }
    let host = selected(ROLE_HOST);
    if !host && !selected(ROLE_VENDOR) {
        return;
    }
    let root = PathBuf::from(std::env::var_os(ROOT).expect("private folder fixture root"));
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    if args == ["--folder-proof"] {
        let receipt = if host { HOST_PROOF } else { VENDOR_PROOF };
        std::fs::write(root.join(receipt), b"native copied fixture body executed\n").unwrap();
        std::process::exit(0);
    }
    if host {
        if args.starts_with(&["harness".into(), "launch-trust".into(), "gate".into()]) {
            event(&root, "gate");
            if let Some(sidecar) = std::env::var_os("ALAB_TEST_FOLDER_SIDECAR") {
                std::fs::write(
                    sidecar,
                    "ALAB_FOLDER_CAPTURED=ownedB\nALAB_FOLDER_B_ONLY=must-not-leak\n",
                )
                .unwrap();
            }
            let value =
                |flag: &str| args.windows(2).find(|pair| pair[0] == flag).unwrap()[1].clone();
            let status = std::env::var("ALAB_TEST_FOLDER_GATE").unwrap_or("allow".into());
            if status == "unserved" {
                // An aterm from before the verb: `aterm harness` refuses the words it does not
                // know with exit 2 and nothing on its standard output.
                eprintln!("aterm harness: unknown flag --cwd-hex; run `aterm harness --help`");
                std::process::exit(2);
            }
            let (rule, reason) = if status == "allow" {
                ("\"trusted-folder\"", "null")
            } else {
                ("null", "\"fixture-refusal\"")
            };
            println!(
                "{{\"schema\":1,\"operation\":\"folder-trust-gate\",\"status\":\"{status}\",\"cwd_hex\":\"{}\",\"vendor_hex\":\"{}\",\"session\":\"{}\",\"stdout_tty\":{},\"rule\":{rule},\"reason\":{reason}}}",
                value("--cwd-hex"),
                value("--vendor-hex"),
                value("--session"),
                value("--stdout-tty") == "1"
            );
            std::process::exit(0);
        }
        if args.starts_with(&["harness".into(), "tap".into()]) {
            event(&root, "tap");
            std::process::exit(0);
        }
        if args.starts_with(&["harness".into(), "accounts".into(), "token-run".into()]) {
            event(&root, "token");
            let at = args.iter().position(|arg| arg == "--").unwrap() + 1;
            let error = Command::new(&args[at]).args(&args[at + 1..]).exec();
            panic!("private token exec: {error}");
        }
        panic!("unexpected private host command: {args:?}");
    }
    if args.first().is_none_or(|arg| arg != "-p") {
        event(&root, "final-vendor");
        state(&root, "signal-after");
        std::fs::write(root.join("final-argv"), args.join("\n")).unwrap();
        std::process::exit(0);
    }
    event(&root, "native-attestation");
    std::fs::write(root.join("attestation-argv"), args.join("\n")).unwrap();
    std::fs::write(
        root.join("attestation-env"),
        format!(
            "{}|{}|{}",
            std::env::var("ENABLE_CLAUDEAI_MCP_SERVERS").unwrap(),
            std::env::var("DISABLE_AUTOUPDATER").unwrap(),
            std::env::var("CLAUDE_CODE_DISABLE_FEEDBACK_SURVEY").unwrap()
        ),
    )
    .unwrap();
    std::fs::write(
        root.join("captured-env"),
        format!(
            "{}|{}",
            std::env::var("ALAB_FOLDER_CAPTURED").unwrap_or("unset".into()),
            std::env::var("ALAB_FOLDER_B_ONLY").unwrap_or("unset".into())
        ),
    )
    .unwrap();
    let mut input = String::new();
    std::io::stdin()
        .lock()
        .take(1024 * 1024 + 1)
        .read_line(&mut input)
        .unwrap();
    std::fs::write(root.join("attestation-request"), &input).unwrap();
    let request = claude_code_alab::json::parse(&input).expect("one strict initialize request");
    let mode = std::env::var("ALAB_TEST_FOLDER_MODE").unwrap_or("record".into());
    let id = request.get("request_id").unwrap().as_str().unwrap();
    assert_eq!(
        request.get("type").unwrap().as_str(),
        Some("control_request")
    );
    let body = request.get("request").unwrap();
    assert_eq!(body.get("subtype").unwrap().as_str(), Some("initialize"));
    let folder = body
        .get("workspaceTrust")
        .unwrap()
        .get("directory")
        .unwrap()
        .as_str()
        .unwrap();
    if mode == "record"
        || mode == "linger"
        || mode == "fork-writer"
        || mode == "trailing-partial"
        || mode.starts_with("preserve-")
    {
        let profile =
            PathBuf::from(std::env::var_os("CLAUDE_CONFIG_DIR").unwrap()).join(".claude.json");
        // Fixture roots use only native ASCII components; the unit controls cover JSON escaping.
        assert!(!folder.contains(['"', '\\', '\n']));
        std::fs::write(
            profile,
            format!("{{\"projects\":{{\"{folder}\":{{\"hasTrustDialogAccepted\":true}}}}}}"),
        )
        .unwrap();
    }
    if let Some(signal) = mode
        .strip_prefix("interrupt-")
        .or_else(|| mode.strip_prefix("preserve-"))
        .and_then(|signal| signal.parse::<i32>().ok())
    {
        // The owner cannot leave this operation normally until this native child
        // ends, and the controller retains the owner's wait. No reused PID is used.
        // SAFETY: getppid names this fixture's still-owning original native parent.
        let parent = unsafe { libc::getppid() };
        assert!(parent > 1);
        assert_eq!(
            // SAFETY: that original parent remains owned and unreaped by the controller.
            unsafe { libc::kill(parent, signal) },
            0
        );
        if mode.starts_with("interrupt-") {
            // A failed mutant is released by an owned private file, not a stale PID signal.
            while !root.join("release-signal-role").exists() {
                std::thread::park_timeout(Duration::from_millis(10));
            }
            std::process::exit(0);
        }
    }
    if mode == "never" {
        loop {
            std::thread::park_timeout(Duration::from_secs(60));
        }
    }
    if mode == "model" {
        println!("{{\"type\":\"assistant\"}}");
    }
    let id = if mode == "wrong-id" { "stale" } else { id };
    let recorded = mode != "decline";
    let answer = format!(
        "{{\"type\":\"control_response\",\"response\":{{\"subtype\":\"success\",\"request_id\":\"{id}\",\"response\":{{\"workspace_trust_recorded\":{recorded},\"claude_code_version\":\"2.1.325\"}}}}}}"
    );
    println!("{answer}");
    if mode == "trailing-partial" {
        print!("{{");
    }
    if mode == "duplicate" {
        println!("{answer}");
    }
    std::io::stdout().flush().unwrap();
    if mode == "fork-writer" {
        // SAFETY: this native fixture has one thread and forks only the refusing writer role.
        let pid = unsafe { libc::fork() };
        assert!(pid >= 0);
        if pid == 0 {
            loop {
                std::thread::park_timeout(Duration::from_secs(60));
            }
        }
        std::fs::write(root.join("writer-pid"), pid.to_string()).unwrap();
    }
    let mut extra = String::new();
    std::io::stdin().read_to_string(&mut extra).unwrap();
    assert!(extra.is_empty(), "no model prompt or second request");
    event(&root, "input-eof");
    if mode == "linger" || mode == "trailing-partial" {
        loop {
            std::thread::park_timeout(Duration::from_secs(60));
        }
    }
    event(&root, "native-finished");
    std::process::exit(0);
}

fn qualify_fixture(s: &Scenario, name: &str, role: &str, receipt: &str) -> PathBuf {
    let image = s.root.join(name);
    copy_fixture(&std::env::current_exe().unwrap(), &image);
    let child = Command::new(&image)
        .arg("--folder-proof")
        .env(ROOT, &s.root)
        .env(role, &image)
        .process_group(0)
        .spawn()
        .expect("spawn native fixture proof");
    s.guard_group(child_pid(&child));
    assert_eq!(
        reap(child, Duration::from_secs(60)),
        Some(Status::Exited(0)),
        "fixture proof for {}",
        image.display()
    );
    assert!(s.root.join(receipt).is_file(), "missing {receipt}");
    image
}

fn setup(name: &str) -> (Scenario, PathBuf, PathBuf) {
    let s = Scenario::new(name);
    // The launch folder is its own repository root. The harness keys trust by the nearest
    // enclosing repository, as the vendor does, and the fake vendor records the folder it was
    // asked to accept; a scenario under a checkout (Cargo's target folder inside the clone, as
    // `CHECK_CMD_DEFAULT` builds) would otherwise key the clone and never read its record.
    std::fs::create_dir(s.work.join(".git")).expect("the launch folder's repository root");
    // These stateless images take their working paths and role inputs from each
    // scenario. Prove each copied image ran before starting the real deadlines.
    static HOST: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
    static VENDOR: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
    let host = HOST.get_or_init(|| qualify_fixture(&s, "native-host", ROLE_HOST, HOST_PROOF));
    let image =
        VENDOR.get_or_init(|| qualify_fixture(&s, "vendor-image", ROLE_VENDOR, VENDOR_PROOF));
    s.add_build_of(BUILD_1, None, image);
    s.set_current(BUILD_1);
    let vendor = s.store.join(BUILD_1.to_string()).join("bin/claude");
    let config = s.root.join("selected-profile");
    std::fs::create_dir(&config).unwrap();
    (s, host.to_path_buf(), vendor)
}

fn command(s: &Scenario, host: &Path, vendor: &Path, mode: &str) -> Command {
    let mut cmd = s.command(&["launch", "--", "--model", "opus"]);
    if mode.starts_with("interrupt-") || mode.starts_with("preserve-") {
        let image = std::env::current_exe().unwrap();
        let mut driver = Command::new(&image);
        driver.args(cmd.get_args()).current_dir(&s.work);
        for (key, value) in cmd.get_envs() {
            match value {
                Some(value) => {
                    driver.env(key, value);
                }
                None => {
                    driver.env_remove(key);
                }
            }
        }
        driver
            .env(DRIVER, "normal")
            .env("ALAB_TEST_FOLDER_DRIVER_IMAGE", image);
        cmd = driver;
    }
    cmd.env(ROLE_HOST, host)
        .env(ROLE_VENDOR, vendor)
        .env(ROOT, &s.root)
        .env("ALAB_TEST_FOLDER_MODE", mode)
        .env("ATERM_HARNESS_HOST", host)
        .env("ATERM_PARENT_SESSION_ID", "t0000000000")
        .env("CLAUDE_CONFIG_DIR", s.root.join("selected-profile"))
        .env_remove("ALAB_TEST_FOLDER_GATE")
        .env_remove("CLAUDE_CODE_CUSTOM_OAUTH_URL")
        .stdin(Stdio::null())
        .stderr(Stdio::from(
            std::fs::File::create(s.root.join("stderr")).unwrap(),
        ));
    cmd
}

fn run(s: &Scenario, mut cmd: Command, terminal: bool) -> u32 {
    let pty = terminal.then(Pty::open);
    if let Some(pty) = &pty {
        cmd.stdout(Stdio::from(
            pty.slave.try_clone().expect("private terminal"),
        ));
    } else {
        cmd.stdout(Stdio::from(
            std::fs::File::create(s.root.join("stdout")).unwrap(),
        ));
    }
    cmd.process_group(0);
    let child = cmd.spawn().unwrap();
    let pid = child.id();
    s.guard_group(child_pid(&child));
    let deadline = Instant::now() + Duration::from_secs(60);
    assert_eq!(
        wait_noncontinued(child_pid(&child), deadline),
        Some(Status::Exited(0)),
        "{}",
        s.report()
    );
    drop(child);
    pid
}

fn wait_noncontinued(pid: i32, deadline: Instant) -> Option<Status> {
    loop {
        if Instant::now() >= deadline {
            return None;
        }
        let status = wait_status(pid, deadline.saturating_duration_since(Instant::now()));
        if status != Some(Status::Continued) {
            return status;
        }
    }
}

pub(super) fn cold_native_owner_settles_trust_before_usage_and_preserves_in_place_launch() {
    let (s, host, vendor) = setup("cold-native-trust");
    let sidecar = s.store.join(format!("{BUILD_1}.shim-env"));
    std::fs::write(&sidecar, "ALAB_FOLDER_CAPTURED=ownedA\n").unwrap();
    let mut cold = command(&s, &host, &vendor, "record");
    cold.env("ALAB_TEST_FOLDER_SIDECAR", sidecar);
    let pid = run(&s, cold, true);
    let events = std::fs::read_to_string(s.root.join("events")).unwrap();
    let kinds = events
        .lines()
        .map(|line| line.split_once('\t').unwrap().0)
        .collect::<Vec<_>>();
    assert_eq!(
        kinds,
        [
            "gate",
            "native-attestation",
            "input-eof",
            "native-finished",
            "tap",
            "token",
            "final-vendor"
        ],
        "{events}"
    );
    assert!(
        events.ends_with(&format!("final-vendor\t{pid}\n")),
        "same original owner PID reaches vendor body"
    );
    assert!(
        s.journal().contains("folder-trust recorded"),
        "{}",
        s.report()
    );
    assert_eq!(
        std::fs::read_to_string(s.root.join("attestation-env")).unwrap(),
        "false|1|1"
    );
    assert_eq!(
        std::fs::read_to_string(s.root.join("captured-env")).unwrap(),
        "ownedA|unset",
        "the attestation uses the selected manifest snapshot, not the gate-mutated sidecar"
    );
    let args = std::fs::read_to_string(s.root.join("attestation-argv")).unwrap();
    assert!(args.contains("--strict-mcp-config\n--settings\n{\"disableAllHooks\":true}"));
    assert!(args.contains("--no-session-persistence"));
    let final_args = std::fs::read_to_string(s.root.join("final-argv")).unwrap();
    assert_eq!(final_args, "--dangerously-skip-permissions\n--model\nopus");
    // Same exact profile key now skips the cold operation, without losing usage/account service.
    std::fs::write(s.root.join("events"), "").unwrap();
    run(&s, command(&s, &host, &vendor, "record"), true);
    let warm = std::fs::read_to_string(s.root.join("events")).unwrap();
    assert!(
        !warm.contains("gate") && !warm.contains("native-attestation"),
        "{warm}"
    );
}

pub(super) fn cold_native_refusals_never_become_recorded_or_run_in_nonterminal_launches() {
    for mode in ["decline", "lie", "wrong-id", "duplicate", "model"] {
        let (s, host, vendor) = setup(&format!("cold-trust-{mode}"));
        run(&s, command(&s, &host, &vendor, mode), true);
        assert!(
            !s.journal().contains("folder-trust recorded"),
            "{}",
            s.report()
        );
        assert!(
            s.journal().contains("folder-trust refused"),
            "{}",
            s.report()
        );
        assert!(
            std::fs::read_to_string(s.root.join("events"))
                .unwrap()
                .contains("final-vendor")
        );
    }
    for (terminal, gate) in [(false, "allow"), (true, "refused"), (true, "unread")] {
        let (s, host, vendor) = setup("cold-trust-skip");
        let mut cmd = command(&s, &host, &vendor, "record");
        cmd.env("ALAB_TEST_FOLDER_GATE", gate);
        run(&s, cmd, terminal);
        let events = std::fs::read_to_string(s.root.join("events")).unwrap();
        assert!(!events.contains("native-attestation"), "{events}");
        assert!(!s.journal().contains("folder-trust recorded"));
        // A host that answers is obeyed, and its refusal is journaled as one.
        assert_eq!(
            s.journal().contains("folder-trust refused"),
            terminal,
            "{gate}{}",
            s.report()
        );
    }
}

/// A host that refuses the gate's question as words it does not know (an aterm from before
/// `harness launch-trust gate`: exit 2, nothing on stdout) is no gate: the launch that records
/// the folder everywhere else in this file asks it once, attests nothing, journals nothing
/// about trust, and still reaches the usage tap and the vendor, as a launch whose host was
/// asked nothing. CONTROL: the same launch under a host that answers `refused` is obeyed and
/// journals the refusal, and under one that answers `allow` records the folder.
pub(super) fn a_host_that_does_not_know_the_gate_is_no_gate_and_one_that_answers_is_obeyed() {
    let kinds = |s: &Scenario| {
        std::fs::read_to_string(s.root.join("events"))
            .unwrap()
            .lines()
            .map(|line| line.split_once('\t').unwrap().0.to_owned())
            .collect::<Vec<_>>()
    };
    let (s, host, vendor) = setup("cold-trust-host-serves-no-gate");
    let mut cmd = command(&s, &host, &vendor, "record");
    cmd.env("ALAB_TEST_FOLDER_GATE", "unserved");
    run(&s, cmd, true);
    assert_eq!(
        kinds(&s),
        ["gate", "tap", "token", "final-vendor"],
        "{}",
        s.report()
    );
    assert!(!s.journal().contains("folder-trust"), "{}", s.report());
    let (s, host, vendor) = setup("cold-trust-host-refuses");
    let mut cmd = command(&s, &host, &vendor, "record");
    cmd.env("ALAB_TEST_FOLDER_GATE", "refused");
    run(&s, cmd, true);
    assert_eq!(
        kinds(&s),
        ["gate", "tap", "token", "final-vendor"],
        "{}",
        s.report()
    );
    assert!(
        s.journal()
            .contains("folder-trust refused policy-refused-or-unread"),
        "{}",
        s.report()
    );
    let (s, host, vendor) = setup("cold-trust-host-allows");
    run(&s, command(&s, &host, &vendor, "record"), true);
    assert_eq!(
        kinds(&s)[..2],
        ["gate", "native-attestation"],
        "{}",
        s.report()
    );
    assert!(
        s.journal().contains("folder-trust recorded"),
        "{}",
        s.report()
    );
}

pub(super) fn cold_native_linger_and_inherited_writer_keep_the_owned_cohort_until_retired() {
    for mode in ["linger", "fork-writer", "never"] {
        let (s, host, vendor) = setup(&format!("cold-trust-{mode}"));
        run(&s, command(&s, &host, &vendor, mode), true);
        let events = std::fs::read_to_string(s.root.join("events")).unwrap();
        assert!(events.contains("final-vendor"));
        assert_eq!(
            s.journal().contains("folder-trust recorded"),
            mode != "never",
            "{}",
            s.report()
        );
        if let Ok(pid) = std::fs::read_to_string(s.root.join("writer-pid")) {
            let pid = pid.parse::<i32>().unwrap();
            // SAFETY: signal zero observes only this fixture's recorded child; no control occurs.
            let rc = unsafe { libc::kill(pid, 0) };
            assert!(
                rc < 0 && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH),
                "retained writer must actually be gone"
            );
        }
    }
}

pub(super) fn cold_native_trailing_partial_is_not_a_recorded_exchange() {
    let (s, host, vendor) = setup("cold-trust-trailing-partial");
    run(&s, command(&s, &host, &vendor, "trailing-partial"), true);
    assert!(
        !s.journal().contains("folder-trust recorded"),
        "an unfinished SDK frame after ACK must survive retirement as refusal{}",
        s.report()
    );
    assert!(
        s.journal().contains("folder-trust refused"),
        "{}",
        s.report()
    );
}

struct ReleaseSignalRole(PathBuf);

impl Drop for ReleaseSignalRole {
    fn drop(&mut self) {
        // A failed mutant can leave the fake SDK child alive after its owner
        // dies. Release its private role without signalling an unpinned PID.
        let _ = std::fs::write(&self.0, b"release\n");
    }
}

fn assert_attestation_group_absent(s: &Scenario) {
    let events = std::fs::read_to_string(s.root.join("events")).unwrap();
    let pid = events
        .lines()
        .find_map(|line| line.strip_prefix("native-attestation\t"))
        .unwrap()
        .parse::<i32>()
        .unwrap();
    // SAFETY: signal zero is observation only, never control of a potentially reused number.
    let result = unsafe { libc::kill(-pid, 0) };
    assert!(
        result < 0 && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH),
        "original SDK group must be absent before interruption propagates: {events}{}",
        s.report()
    );
    assert!(
        !events.contains("tap") && !events.contains("final-vendor"),
        "interrupted launch must not continue: {events}"
    );
}

pub(super) fn cold_native_interrupts_retire_the_sdk_before_original_signal_propagation() {
    for signal in [libc::SIGINT, libc::SIGTERM, libc::SIGTSTP] {
        let (s, host, vendor) = setup(&format!("cold-trust-interrupt-{signal}"));
        let _release = ReleaseSignalRole(s.root.join("release-signal-role"));
        let pty = Pty::open();
        let mut cmd = command(&s, &host, &vendor, &format!("interrupt-{signal}"));
        cmd.stdout(Stdio::from(pty.slave.try_clone().unwrap()))
            .process_group(0);
        let child = cmd.spawn().unwrap();
        let pid = child_pid(&child);
        s.guard_group(pid);
        if signal == libc::SIGTSTP {
            assert_eq!(
                wait_status(pid, Duration::from_secs(60)),
                Some(Status::Stopped),
                "{}",
                s.report()
            );
            assert_attestation_group_absent(&s);
            assert_eq!(
                // SAFETY: the positive stop event is nonterminal, and this test
                // retains the original child's owning wait until its final exit.
                unsafe { libc::kill(pid, libc::SIGCONT) },
                0
            );
            assert_eq!(
                wait_noncontinued(pid, Instant::now() + Duration::from_secs(60)),
                Some(Status::Exited(128 + signal)),
                "{}",
                s.report()
            );
            drop(child);
        } else {
            assert_eq!(
                reap(child, Duration::from_secs(60)),
                Some(Status::Signaled(signal)),
                "{}",
                s.report()
            );
            assert_attestation_group_absent(&s);
        }
        assert!(
            !s.journal().contains("folder-trust recorded"),
            "{}",
            s.report()
        );
    }
}

pub(super) fn cold_native_signal_scope_preserves_ignore_block_resize_and_continue() {
    for (kind, signal) in [
        ("ignore-term", libc::SIGTERM),
        ("block-int", libc::SIGINT),
        ("normal", libc::SIGCONT),
        ("normal", libc::SIGWINCH),
    ] {
        let (s, host, vendor) = setup(&format!("cold-trust-preserve-{kind}-{signal}"));
        let mut cmd = command(&s, &host, &vendor, &format!("preserve-{signal}"));
        cmd.env(DRIVER, kind);
        run(&s, cmd, true);
        assert_eq!(
            std::fs::read(s.root.join("signal-before")).unwrap(),
            std::fs::read(s.root.join("signal-after")).unwrap(),
            "inherited disposition and full mask survive native cleanup and final exec{}",
            s.report()
        );
        assert!(
            s.journal().contains("folder-trust recorded"),
            "{}",
            s.report()
        );
        let events = std::fs::read_to_string(s.root.join("events")).unwrap();
        assert!(
            events.contains("native-finished") && events.contains("final-vendor"),
            "{events}"
        );
    }
}
