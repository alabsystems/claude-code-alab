// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

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
    assert_eq!(config.scopes(), [Scope::Relaunch, Scope::Upgrade]);
    assert!(config.streams());
    assert!(config.c3_child_tracking());
    let claim = fake.claim_words().unwrap();
    assert_eq!(claim.contract.as_deref(), Some("3"));
    assert_eq!(claim.scope.as_deref(), Some("relaunch,upgrade"));
    assert!(!fake.verbs().iter().any(|verb| verb.starts_with("stream ")));
}

#[test]
fn a_c4_host_opening_a_stream_keeps_the_same_lifecycle_scope() {
    let fake = FakeAterm::for_harness("claude-link-stream", &config());
    fake.serve_contracts(&[4]);
    fake.serve_stream(true);

    let link = Link::launch_at(&fake.locator(), None, 0);
    assert!(link.contract_four());
    assert!(link.streaming());
    assert!(link.fds()[1].is_some());
    let claim = fake.claim_words().unwrap();
    assert_eq!(claim.contract.as_deref(), Some("4"));
    assert_eq!(claim.scope.as_deref(), Some("relaunch,upgrade"));
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
    link.core.reannounce(metadata(None, Some(child)), now);
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
