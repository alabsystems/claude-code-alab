// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::super::model_tests;
use super::*;
use alab_wire::fake::{FakeAterm, info_with_rows};

fn host(name: &str, capabilities: &str) -> (FakeAterm, Supervisor) {
    let fake = FakeAterm::for_harness(name, &config());
    fake.serve_contracts(&[3]);
    fake.set_info(&format!(
        "{} {capabilities}",
        info_with_rows(&["claude-code-alab"])
    ));
    let mut supervisor = super::super::tests::supervisor_of_an_unsignallable_child(&fake.xdg);
    supervisor.journal = Journal::none();
    supervisor.link = Link::launch_at(&fake.locator(), None, 0);
    assert!(supervisor.link.judges());
    (fake, supervisor)
}

fn identity(birth: procs::Birth) -> procs::ChildIdentity {
    procs::ChildIdentity {
        pid: 123,
        parent: 42,
        pgid: 42,
        session: 42,
        birth,
        executable: procs::Executable {
            path: "/fixture/claude".into(),
            device: 1,
            inode: 2,
        },
    }
}

#[test]
fn child_birth_conversion_keeps_native_units_and_refuses_invalid_data() {
    assert_eq!(
        native_child(&identity(procs::Birth::MacOs {
            seconds: 42,
            microseconds: 5,
        })),
        NativeChild::from_native(123, 42_000_005)
    );
    assert_eq!(
        native_child(&identity(procs::Birth::Linux(42))),
        NativeChild::from_native(123, 42)
    );
    for birth in [
        procs::Birth::MacOs {
            seconds: u64::MAX,
            microseconds: 0,
        },
        procs::Birth::MacOs {
            seconds: 42,
            microseconds: 1_000_000,
        },
        procs::Birth::Linux(0),
    ] {
        assert_eq!(native_child(&identity(birth)), None);
    }
    let mut invalid = identity(procs::Birth::Linux(42));
    invalid.pid = 1;
    assert_eq!(native_child(&invalid), None);
}

#[test]
fn a_c3_host_keeps_its_lifecycle_and_opens_no_stream() {
    let (fake, supervisor) = host("claude-link-config", "");
    let config = supervisor.link.core.config();
    assert_eq!(config.contracts(), [Contract::Three, Contract::Four]);
    assert_eq!(
        config.scopes(),
        [Scope::Relaunch, Scope::Upgrade, Scope::Accounts]
    );
    assert!(config.streams());
    assert!(config.c3_child_tracking());
    let claim = fake.claim_words().unwrap();
    assert_eq!(claim.contract.as_deref(), Some("3"));
    assert_eq!(claim.scope.as_deref(), Some("relaunch,upgrade"));
    assert!(!fake.verbs().iter().any(|verb| verb.starts_with("stream ")));
    // Contract 3 has no `accounts` word: no account move is this claim's.
    assert!(!supervisor.link.moves_accounts());
}

/// The claim asks for the account moves only from a launch that reaches
/// aterm's token helper: one that could start no account says so by its
/// claim, the only answer aterm reads.
#[test]
fn a_launch_that_reaches_no_token_helper_does_not_claim_the_account_moves() {
    assert_eq!(
        config_for(false).scopes(),
        [Scope::Relaunch, Scope::Upgrade]
    );
    // CONTROL: with a helper the word is asked for, under the same
    // contracts either way.
    assert_eq!(
        config_for(true).scopes(),
        [Scope::Relaunch, Scope::Upgrade, Scope::Accounts]
    );
    for helper in [false, true] {
        let config = config_for(helper);
        assert_eq!(config.contracts(), [Contract::Three, Contract::Four]);
        assert!(config.streams() && config.c3_child_tracking());
    }
    // A contract 4 host answers a launch without one with no account word.
    let fake = FakeAterm::for_harness("claude-link-no-helper", &config_for(false));
    fake.serve_contracts(&[4]);
    fake.serve_stream(true);
    let core = alab_wire::Link::launch_at(
        config_for(false),
        std::process::id(),
        &fake.locator(),
        metadata(None, None, None),
        0,
    );
    assert!(core.streaming() && !core.takes(Scope::Accounts));
    assert_eq!(
        fake.claim_words().unwrap().scope.as_deref(),
        Some("relaunch,upgrade")
    );
}

#[test]
fn a_c4_host_opening_a_stream_takes_the_account_moves_too_unless_capped() {
    let fake = FakeAterm::for_harness("claude-link-stream", &config());
    fake.serve_contracts(&[4]);
    fake.serve_stream(true);

    let link = Link::launch_at(&fake.locator(), None, 0);
    assert!(link.contract_four());
    assert!(link.streaming());
    assert!(link.fds()[1].is_some());
    let claim = fake.claim_words().unwrap();
    assert_eq!(claim.contract.as_deref(), Some("4"));
    assert_eq!(claim.scope.as_deref(), Some("relaunch,upgrade,accounts"));
    // The account moves are this harness's where the grant says so, and only
    // there: a person's cap that leaves the word out takes them away.
    assert!(link.moves_accounts());
    let capped = FakeAterm::for_harness("claude-link-stream-capped", &config());
    capped.serve_contracts(&[4]);
    capped.serve_stream(true);
    capped.set_cap(Some(&[Scope::Relaunch, Scope::Upgrade]));
    let link = Link::launch_at(&capped.locator(), None, 0);
    assert!(link.contract_four() && link.judges());
    assert!(!link.moves_accounts());
}

#[test]
fn retiring_a_c4_child_reannounces_for_host_reproof() {
    let fake = FakeAterm::for_harness("claude-retire-stream-child", &config());
    fake.serve_contracts(&[4]);
    fake.serve_stream(true);
    let now = now_ms();
    let mut link = Link::launch_at(&fake.locator(), None, now);
    let child = ChildProcess {
        pid: 123,
        start: 42,
    };
    link.process = Some(child);
    link.core.reannounce(metadata(None, Some(child), None), now);
    link.child_ended(&Journal::none(), None);
    assert!(link.process.is_none());
    let by = std::time::Instant::now() + Duration::from_secs(60);
    loop {
        let children = fake
            .stream_records()
            .into_iter()
            .filter_map(|record| match record.body {
                Body::Claim { child, .. } => Some(child),
                _ => None,
            })
            .collect::<Vec<_>>();
        if children == [Some(child), None] {
            break;
        }
        assert!(
            std::time::Instant::now() < by,
            "claims sent for the child and its retirement: {children:?}"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn a_model_only_settles_on_its_own_ack_and_retries_a_transient_drop() {
    let model = Body::Model {
        child: Some(ChildProcess {
            pid: 123,
            start: 42,
        }),
        model: Some("Opus 5.5".to_owned()),
        effort: None,
        key: None,
        offers: Vec::new(),
        permission: None,
    };
    let mut delivery = ModelDelivery {
        opened: 1,
        pending: Some((4, model.clone())),
        ..ModelDelivery::default()
    };
    delivery.ack(3, None, 10);
    assert!(delivery.pending.is_some());
    assert!(delivery.settled.is_none());
    delivery.ack(4, Some(&Dropped::Rate), 10);
    assert!(delivery.pending.is_none());
    assert!(delivery.settled.is_none());
    assert_eq!(delivery.retry_at, 10 + MODEL_RETRY_MS);
    delivery.pending = Some((5, model.clone()));
    delivery.ack(5, None, delivery.retry_at);
    assert_eq!(delivery.settled, Some(model));
    delivery.opening(2);
    assert!(delivery.settled.is_none());
    assert_eq!(delivery.retry_at, 0);
}

#[test]
fn initial_native_completion_is_lazy_and_legacy_claims_are_not_duplicated() {
    for capabilities in ["", "child-proof=1"] {
        let (fake, mut supervisor) = host("claude-link-first-child", capabilities);
        let native = NativeChild::from_native(123, 42).unwrap();
        let observed = std::cell::Cell::new(0);
        let observe = |_| {
            observed.set(observed.get() + 1);
            Some(native)
        };
        let before = fake.verbs().len();
        supervisor.link_tick_with(0, observe);
        if capabilities.is_empty() {
            assert_eq!(observed.get(), 0);
            assert_eq!(fake.verbs().len(), before);
            assert!(fake.child().is_none());
        } else {
            assert_eq!(observed.get(), 1);
            assert_eq!(fake.child().as_deref(), Some("123:42"));
        }
        supervisor.link_tick_with(1, observe);
        assert_eq!(observed.get(), usize::from(!capabilities.is_empty()));
    }
}

#[test]
fn retiring_the_consumed_child_allows_reclaim_without_observing_its_pid() {
    let (fake, mut supervisor) = host("claude-link-retired-child", "child-proof=1");
    let native = NativeChild::from_native(123, 42).unwrap();
    supervisor.link_tick_with(0, |_| Some(native));
    assert_eq!(fake.child().as_deref(), Some("123:42"));
    // The terminal wait event is represented as data; this test creates no OS child.
    supervisor.link.child_ended(&Journal::none(), None);
    supervisor.ending = super::super::tests::crashed(crate::sys::ChildEvent::Exited(7));
    fake.restart();
    supervisor.link_events(false);
    assert!(supervisor.link.lost());
    supervisor.link_tick_with(
        now_ms().saturating_add(alab_wire::link::RECLAIM_EVERY_MS),
        |_| panic!("the reaped child has no native observation"),
    );
    assert!(supervisor.link.judges());
    assert!(fake.child().is_none());
}

/// A supervisor launched with `args` in a scratch folder named for `name`,
/// whose link holds an open contract 4 stream on the returned fake; its
/// child's session record, and the folder, whose journal it writes.
pub(in crate::supervise) fn streaming(
    name: &str,
    args: &[&str],
) -> (
    FakeAterm,
    Supervisor,
    crate::profile::SessionRecord,
    std::path::PathBuf,
) {
    let fake = FakeAterm::for_harness(name, &config());
    fake.serve_contracts(&[4]);
    fake.serve_stream(true);
    let root = model_tests::scratch(name);
    let (mut supervisor, record) = model_tests::launched(&root, args);
    supervisor.child.build.version = Some("9.9.1".to_string());
    supervisor.link = Link::launch_at(&fake.locator(), None, now_ms());
    assert!(supervisor.link.streaming());
    (fake, supervisor, record, root)
}

/// Give `supervisor`'s link a child to name, as a look that observed the
/// vendor does, and announce it: the claims that follow say what they say of
/// that child. The child, which names no process of this test.
pub(in crate::supervise) fn named_child(supervisor: &mut Supervisor) -> ChildProcess {
    let child = ChildProcess {
        pid: 123,
        start: 42,
    };
    supervisor.link.process = Some(child);
    supervisor.link.core.reannounce(
        metadata(
            supervisor.child.build.version.as_deref(),
            Some(child),
            supervisor.link.login.as_deref(),
        ),
        now_ms(),
    );
    child
}

/// The records of `kind` the fake has read on its streams, once at least
/// `n` of them have come.
fn told(fake: &FakeAterm, kind: alab_wire::Kind, n: usize) -> Vec<Body> {
    let by = std::time::Instant::now() + Duration::from_secs(60);
    loop {
        let bodies: Vec<Body> = fake
            .stream_records()
            .into_iter()
            .map(|record| record.body)
            .filter(|body| body.kind() == Some(kind))
            .collect();
        if bodies.len() >= n {
            return bodies;
        }
        assert!(
            std::time::Instant::now() < by,
            "{n} {} records never came: {bodies:?}",
            kind.word()
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// The `session` record of [`model_tests::SID`] in `cwd`, resumed by `resume`.
fn session(resume: &[&str], account: Option<&str>, cwd: &str) -> Body {
    session_of(model_tests::SID, resume, account, cwd)
}

/// The `session` record of `conversation` in `cwd`, resumed by `resume`.
fn session_of(conversation: &str, resume: &[&str], account: Option<&str>, cwd: &str) -> Body {
    Body::Session {
        agent: "claude".to_string(),
        version: VERSION.to_string(),
        vendor: Some("9.9.1".to_string()),
        account: account.map(str::to_string),
        conversation: Some(conversation.to_string()),
        cwd: Some(cwd.to_string()),
        resume: resume.iter().map(|word| (*word).to_string()).collect(),
        mode: Mode::Tui,
        takes: Vec::new(),
    }
}

/// The journal lines under `root` that say a record was put.
fn told_lines(root: &std::path::Path) -> Vec<String> {
    model_tests::events(root)
        .into_iter()
        .filter(|line| line.starts_with("aterm told "))
        .collect()
}

#[test]
fn each_start_tells_aterm_how_its_conversation_resumes() {
    use model_tests::SID;
    let (fake, mut supervisor, mut record, root) =
        streaming("claude-session-starts", &["--dangerously-skip-permissions"]);
    let cwd = record.cwd.clone();
    // CONTROL: before the child writes its record no conversation is known.
    supervisor.session_look(None, now_ms());
    // The launch's child.
    supervisor.session_look(Some(&record), now_ms());
    // CONTROL: the same child on the same conversation says nothing again,
    // busy or at a turn end that moved nothing it says.
    record.status = "busy".to_string();
    supervisor.session_look(Some(&record), now_ms());
    record.status = crate::profile::STATUS_IDLE.to_string();
    record.status_updated_at_ms = 5;
    supervisor.session_look(Some(&record), now_ms());
    // A relaunch on the same conversation: a new child.
    supervisor.child.pid = 2;
    record.pid = 2;
    supervisor.session_look(Some(&record), now_ms());
    // aterm moves the model, then the account; the next child runs on both.
    supervisor.model_moved("claude-opus-5-5".to_string(), "model");
    supervisor.account_moved(
        crate::aterm::Account {
            key: "ac-0000000000000001".to_string(),
            home: Some(root.join("login-work")),
        },
        "account",
    );
    supervisor.child.pid = 3;
    record.pid = 3;
    supervisor.session_look(Some(&record), now_ms());
    let launch = session(
        &["--dangerously-skip-permissions", "--resume", SID],
        None,
        &cwd,
    );
    assert_eq!(
        told(&fake, alab_wire::Kind::Session, 3),
        [
            launch.clone(),
            launch,
            session(
                &[
                    "--dangerously-skip-permissions",
                    "--model",
                    "claude-opus-5-5",
                    "--resume",
                    SID,
                ],
                Some("ac-0000000000000001"),
                &cwd,
            ),
        ]
    );
    let lines = told_lines(&root);
    assert_eq!(lines.len(), 3, "{lines:?}");
    assert!(
        lines[2].ends_with(&format!(
            " conversation={SID} account=ac-0000000000000001 vendor=9.9.1 \
             argv=--dangerously-skip-permissions\\u{{1f}}--model\\u{{1f}}claude-opus-5-5\\u{{1f}}--resume\\u{{1f}}{SID}"
        )),
        "{lines:?}"
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_session_is_put_only_while_a_stream_is_open() {
    use model_tests::SID;
    let (fake, mut supervisor, mut record, root) =
        streaming("claude-session-lost", &["--dangerously-skip-permissions"]);
    supervisor.session_look(Some(&record), now_ms());
    told(&fake, alab_wire::Kind::Session, 1);
    // aterm restarts: the stream goes, and the claim with it.
    fake.restart();
    supervisor.link_events(false);
    assert!(supervisor.link.lost() && !supervisor.link.streaming());
    // A child started meanwhile, on another model: nothing is put.
    supervisor.model_moved("claude-opus-5-5".to_string(), "model");
    supervisor.child.pid = 2;
    record.pid = 2;
    supervisor.session_look(Some(&record), now_ms());
    supervisor.upgrade_told(7, Some("9.9.2".to_string()), UpgradePhase::Seen);
    assert_eq!(told_lines(&root).len(), 1, "{:?}", told_lines(&root));
    // The claim is made again, on a stream whose aterm holds nothing: the
    // next look puts the child's session once, then the move still to make.
    supervisor.link_tick_with(
        now_ms().saturating_add(alab_wire::link::RECLAIM_EVERY_MS),
        |_| None,
    );
    assert!(supervisor.link.streaming());
    supervisor.link_events(false);
    supervisor.session_look(Some(&record), now_ms());
    // The upgrade record is put after the session's: once it has come, every
    // session record put before it has too.
    assert_eq!(
        told(&fake, alab_wire::Kind::Upgrade, 1),
        [Body::Upgrade {
            phase: UpgradePhase::Seen,
            to: "9.9.2".to_string(),
        }]
    );
    // The link itself writes the launch's record again where aterm had not
    // acknowledged it before it went; the new child's is put once, last.
    let launch = session(
        &["--dangerously-skip-permissions", "--resume", SID],
        None,
        &record.cwd,
    );
    let moved = session(
        &[
            "--dangerously-skip-permissions",
            "--model",
            "claude-opus-5-5",
            "--resume",
            SID,
        ],
        None,
        &record.cwd,
    );
    let sessions = told(&fake, alab_wire::Kind::Session, 2);
    let (last, before) = sessions.split_last().unwrap();
    assert_eq!(last, &moved, "{sessions:?}");
    assert!(before.iter().all(|body| *body == launch), "{sessions:?}");
    assert_eq!(told_lines(&root).len(), 3, "{:?}", told_lines(&root));
    // aterm restarts again while nothing changes here: what it forgot is put
    // again, the session first, then the move still to make. (The link writes
    // again, unjournalled, what aterm had not acknowledged; only the
    // supervisor's own puts are journalled.)
    let put = sessions.len();
    fake.restart();
    supervisor.link_events(false);
    assert!(supervisor.link.lost());
    // The link's clock is already past the first reclaim's moment.
    supervisor.link_tick_with(
        now_ms().saturating_add(2 * alab_wire::link::RECLAIM_EVERY_MS),
        |_| None,
    );
    assert!(
        supervisor.link.streaming(),
        "{:?}",
        model_tests::events(&root)
    );
    supervisor.link_events(false);
    supervisor.session_look(Some(&record), now_ms());
    let lines = told_lines(&root);
    assert_eq!(lines.len(), 5, "{lines:?}");
    assert!(
        lines[3].starts_with("aterm told session ")
            && lines[3].ends_with(&format!(
                "argv=--dangerously-skip-permissions\\u{{1f}}--model\\u{{1f}}claude-opus-5-5\\u{{1f}}--resume\\u{{1f}}{SID}"
            ))
            && lines[4].starts_with("aterm told upgrade ")
            && lines[4].ends_with(" phase=seen to=9.9.2"),
        "{lines:?}"
    );
    told(&fake, alab_wire::Kind::Upgrade, 2);
    let sessions = told(&fake, alab_wire::Kind::Session, put + 1);
    assert_eq!(sessions.last(), Some(&moved), "{sessions:?}");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_move_is_told_in_the_order_it_is_made() {
    let (fake, mut supervisor, _record, root) = streaming("claude-upgrade-told", &[]);
    supervisor.setup.store = crate::store::Store::new(&root.join("prefix"), "claude", "claude");
    std::fs::create_dir_all(&supervisor.setup.store.program_dir).unwrap();
    // 9.9.2 and 9.9.3, as the store numbers vendor builds.
    let target = 1_000_000_000_000_000_000 + 9_000_000_000_000 + 9_000_000 + 2;
    std::fs::write(
        supervisor
            .setup
            .store
            .program_dir
            .join(format!("{target}.vendor")),
        "version = \"9.9.2\"\n",
    )
    .unwrap();
    supervisor.upgrade_pending(target);
    supervisor.upgrade_pending(target);
    // CONTROL: a move still waited for is said once.
    supervisor.upgrade_pending(target);
    // CONTROL: a store that names the child's build again has no move the
    // child is ending for to take back.
    supervisor.upgrade_told(target, Some("9.9.2".to_string()), UpgradePhase::Moving);
    supervisor.upgrade_none();
    supervisor.upgrade_told(target, Some("9.9.2".to_string()), UpgradePhase::Done);
    // A build whose provenance states no version, which the store then takes
    // back before it is moved onto.
    supervisor.upgrade_pending(target + 1);
    supervisor.upgrade_none();
    supervisor.upgrade_none();
    let upgrade = |phase, to: &str| Body::Upgrade {
        phase,
        to: to.to_string(),
    };
    assert_eq!(
        told(&fake, alab_wire::Kind::Upgrade, 6),
        [
            upgrade(UpgradePhase::Seen, "9.9.2"),
            upgrade(UpgradePhase::Waiting, "9.9.2"),
            upgrade(UpgradePhase::Moving, "9.9.2"),
            upgrade(UpgradePhase::Done, "9.9.2"),
            upgrade(UpgradePhase::Seen, "-"),
            upgrade(UpgradePhase::Abandoned, "-"),
        ]
    );
    assert_eq!(supervisor.upgrading, None);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_conversation_the_same_child_moves_to_is_told() {
    use model_tests::SID;
    const CLEARED: &str = "5d2c9b1e-7a40-4c3f-8e61-0b9f2a7c4d13";
    let (fake, mut supervisor, mut record, root) = streaming(
        "claude-session-cleared",
        &["--dangerously-skip-permissions"],
    );
    let cwd = record.cwd.clone();
    // Mid-turn throughout, so no turn end moves what the record says.
    record.status = "busy".to_string();
    supervisor.session_look(Some(&record), now_ms());
    // `/clear`, or `/resume` of another conversation: the same child's record
    // names it.
    record.session_id = CLEARED.to_string();
    supervisor.session_look(Some(&record), now_ms());
    // CONTROL: the same child on the same conversation says nothing again.
    supervisor.session_look(Some(&record), now_ms());
    // `/resume` back onto the first.
    record.session_id = SID.to_string();
    supervisor.session_look(Some(&record), now_ms());
    let first = session(
        &["--dangerously-skip-permissions", "--resume", SID],
        None,
        &cwd,
    );
    assert_eq!(
        told(&fake, alab_wire::Kind::Session, 3),
        [
            first.clone(),
            session_of(
                CLEARED,
                &["--dangerously-skip-permissions", "--resume", CLEARED],
                None,
                &cwd
            ),
            first,
        ]
    );
    let lines = told_lines(&root);
    assert_eq!(lines.len(), 3, "{lines:?}");
    assert!(
        lines[1].contains(&format!(" conversation={CLEARED} ")),
        "{lines:?}"
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_turn_end_tells_the_permission_mode_the_conversation_moved_to() {
    use model_tests::SID;
    let (fake, mut supervisor, mut record, root) =
        streaming("claude-session-mode", &["--dangerously-skip-permissions"]);
    let cwd = record.cwd.clone();
    // Launched idle, in bypass.
    supervisor.session_look(Some(&record), now_ms());
    // A turn in which the person moves the conversation to plan mode: its
    // transcript says so.
    record.status = "busy".to_string();
    record.status_updated_at_ms = 5;
    model_tests::history(&supervisor, "plan");
    // CONTROL: mid-turn the words are not made again.
    supervisor.session_look(Some(&record), now_ms());
    assert_eq!(told_lines(&root).len(), 1, "{:?}", told_lines(&root));
    // The turn ends: the words resume the conversation in plan mode, never
    // in the bypass the person left.
    record.status = crate::profile::STATUS_IDLE.to_string();
    record.status_updated_at_ms = 9;
    supervisor.session_look(Some(&record), now_ms());
    assert_eq!(
        told(&fake, alab_wire::Kind::Session, 2),
        [
            session(
                &["--dangerously-skip-permissions", "--resume", SID],
                None,
                &cwd
            ),
            session(
                &[
                    "--allow-dangerously-skip-permissions",
                    "--permission-mode",
                    "plan",
                    "--resume",
                    SID
                ],
                None,
                &cwd
            ),
        ]
    );
    assert_eq!(told_lines(&root).len(), 2, "{:?}", told_lines(&root));
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn the_words_leave_out_the_usage_taps_settings_and_only_those() {
    use model_tests::SID;
    // A managed launch inside aterm: the tap's `--settings <value>` leads,
    // before a person's own `--settings` and the bypass default.
    let launch = [
        "--settings",
        r#"{"statusLine":{"type":"command","command":"aterm harness tap"}}"#,
        "--settings",
        "mine.json",
        "--dangerously-skip-permissions",
    ];
    let (fake, mut supervisor, record, root) = streaming("claude-session-tapped", &launch);
    supervisor.setup.tapped = true;
    supervisor.session_look(Some(&record), now_ms());
    assert_eq!(
        told(&fake, alab_wire::Kind::Session, 1),
        [session(
            &[
                "--settings",
                "mine.json",
                "--dangerously-skip-permissions",
                "--resume",
                SID
            ],
            None,
            &record.cwd
        )]
    );
    // CONTROL: a launch the tap did not lead keeps every `--settings` it has.
    supervisor.setup.tapped = false;
    let mut all: Vec<String> = launch.iter().map(|word| (*word).to_string()).collect();
    all.extend(["--resume".to_string(), SID.to_string()]);
    assert_eq!(supervisor.resume_words(&record), Ok(all));
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn words_aterm_would_refuse_to_type_are_not_told() {
    use model_tests::SID;
    let root = model_tests::scratch("untypable");
    let long = "w".repeat(1_025);
    let dirs: Vec<String> = (0..55).map(|n| format!("/d{n}")).collect();
    let many: Vec<&str> = std::iter::once("--add-dir")
        .chain(dirs.iter().map(String::as_str))
        .collect();
    // Each word of aterm's relaunch line is 1 to 1024 bytes of printable
    // ASCII, at most 64 words in all: a newline, a word that is no ASCII, an
    // empty word, a word too long, and too many words are each refused.
    for (launch, why) in [
        (
            &["--append-system-prompt", "line one\nline two"][..],
            "unrelaunchable word=1",
        ),
        (&["--name", "Café"][..], "unrelaunchable word=1"),
        (&["--name", ""][..], "unrelaunchable word=1"),
        (&["--name", long.as_str()][..], "unrelaunchable word=1"),
        (&many[..], "unrelaunchable words=58"),
    ] {
        let (supervisor, record) = model_tests::launched(&root, launch);
        assert_eq!(
            supervisor.resume_words(&record),
            Err(why.to_string()),
            "{launch:?}"
        );
    }
    // CONTROL: the longest word, a space, and as many words as fit are typed.
    let longest = "w".repeat(1_024);
    let fit = &many[..many.len() - 3];
    for launch in [
        &["--name", longest.as_str()][..],
        &["--append-system-prompt", "one line"][..],
        fit,
    ] {
        let (supervisor, record) = model_tests::launched(&root, launch);
        let mut words: Vec<String> = launch.iter().map(|word| (*word).to_string()).collect();
        words.extend(["--resume".to_string(), SID.to_string()]);
        assert_eq!(supervisor.resume_words(&record), Ok(words), "{launch:?}");
    }
    let _ = std::fs::remove_dir_all(&root);
    // The record of such a launch carries no words, so aterm resumes from its
    // own snapshot of the child, and the journal says why.
    let (fake, mut supervisor, record, root) = streaming(
        "claude-session-untypable",
        &["--append-system-prompt", "line one\nline two"],
    );
    supervisor.session_look(Some(&record), now_ms());
    assert_eq!(
        told(&fake, alab_wire::Kind::Session, 1),
        [session(&[], None, &record.cwd)]
    );
    let events = model_tests::events(&root);
    assert!(
        events.contains(&"session-words unrelaunchable word=1".to_string()),
        "{events:?}"
    );
    let _ = std::fs::remove_dir_all(&root);
}
