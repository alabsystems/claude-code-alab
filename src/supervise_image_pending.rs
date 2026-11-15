// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! An existing prepare is serviced before any ordinary selection/retry skip.
//! These are observations, not another admission or mutation budget.

use super::{Image, PendingAdmission};
use std::io;

pub(super) struct Facts {
    pub selected: io::Result<Image>,
    pub moves_allowed: bool,
    pub child_unchanged: bool,
    pub usable_budget: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Decision {
    ContinueOriginal,
    ReconcileOriginal,
}

pub(super) fn decide(
    original: &PendingAdmission,
    facts: &Facts,
    attempted: bool,
    retained: bool,
) -> Decision {
    if facts.moves_allowed
        && facts.child_unchanged
        && facts.usable_budget
        && !attempted
        && !retained
        && facts
            .selected
            .as_ref()
            .is_ok_and(|selected| selected == &original.target)
    {
        Decision::ContinueOriginal
    } else {
        Decision::ReconcileOriginal
    }
}

#[cfg(test)]
mod tests {
    use super::super::{
        Admission, CommitOutcome, CustodyDeadline, CustodyFd, Host, HostContext, MoveKind, Runtime,
        Supervisor,
    };
    use super::*;
    use crate::transfer_capsule::Phase;
    use crate::{cli, journal, procs, quiet, store, supervise};
    use std::cell::RefCell;
    use std::rc::Rc;
    use std::time::Instant;

    #[derive(Clone, Debug, PartialEq, Eq)]
    enum Call {
        Admit(&'static str, u64, u32, u64),
        Recover(u64, u32),
        Dispatched(&'static str, u64),
    }
    struct Record {
        operation_id: &'static str,
        calls: Vec<Call>,
        ready: bool,
        outcome: CommitOutcome,
    }
    struct RecordingHost(Rc<RefCell<Record>>);
    impl Host for RecordingHost {
        fn ready(&self) -> bool {
            true
        }
        fn admit(&mut self, facts: &Admission<'_>) -> io::Result<bool> {
            let mut record = self.0.borrow_mut();
            let operation_id = record.operation_id;
            record.calls.push(Call::Admit(
                operation_id,
                facts.deadline.monotonic_ns(),
                facts.child.pid,
                facts.target.build,
            ));
            Ok(record.ready)
        }
        fn custody(&mut self, _: CustodyDeadline) -> io::Result<(HostContext, Vec<CustodyFd>)> {
            Err(io::Error::other("recording dispatch has no native FD"))
        }
        fn reprove(
            &mut self,
            _: &HostContext,
            _: &procs::ProcessIdentity,
            _: &procs::ChildIdentity,
            _: Phase,
            _: CustodyDeadline,
            _: &mut Vec<CustodyFd>,
        ) -> io::Result<bool> {
            unreachable!("no image exec in a pending controller control")
        }
        fn commit(
            &mut self,
            _: &HostContext,
            _: &procs::ProcessIdentity,
            _: &procs::ChildIdentity,
            _: Phase,
            _: CustodyDeadline,
            _: &mut Vec<CustodyFd>,
        ) -> io::Result<CommitOutcome> {
            unreachable!("no image exec in a pending controller control")
        }
        fn recover(
            &mut self,
            context: Option<&HostContext>,
            _: &procs::ProcessIdentity,
            child: &procs::ChildIdentity,
            phase: Phase,
            original: u64,
            _: &mut Vec<CustodyFd>,
        ) -> io::Result<CommitOutcome> {
            assert!(context.is_none());
            assert_eq!(phase, Phase::Rollback);
            let mut record = self.0.borrow_mut();
            record.calls.push(Call::Recover(original, child.pid));
            Ok(record.outcome)
        }
    }
    fn image(build: u64) -> Image {
        let prefix = std::path::PathBuf::from("/pending-control");
        let directory = prefix
            .join("store/claude-code-alab")
            .join(build.to_string());
        Image {
            prefix,
            build,
            executable: procs::Executable {
                path: directory.join("bin/claude-alab"),
                device: 1,
                inode: build,
            },
            directory,
            version: cli::VERSION.into(),
            file_revision: [build; 5],
        }
    }
    fn child() -> procs::ChildIdentity {
        procs::ChildIdentity {
            pid: 321,
            parent: std::process::id(),
            pgid: 321,
            session: 321,
            birth: procs::Birth::Linux(123),
            executable: procs::Executable {
                path: "/pending-control/vendor/bin/claude".into(),
                device: 1,
                inode: 123,
            },
        }
    }
    fn supervisor() -> Supervisor {
        let opts = cli::Options::default();
        let build = store::Build {
            number: 1,
            dir: "/pending-control/vendor/1".into(),
            exe: "/pending-control/vendor/1/bin/claude".into(),
            version: None,
            env: Vec::new(),
        };
        Supervisor {
            image: Runtime::default(),
            setup: supervise::Setup {
                home: "/pending-control/home".into(),
                store: store::Store::new(
                    std::path::Path::new("/pending-control"),
                    "claude",
                    "claude",
                ),
                own_exe: None,
                build: build.clone(),
                opts: opts.clone(),
                args: Vec::new(),
                depth: 0,
            },
            journal: journal::Journal::none(),
            config: "/pending-control/home/.claude".into(),
            cwd: None,
            termios: None,
            child: supervise::Child {
                pid: 321,
                build,
                args: Vec::new(),
                identity: Some(child()),
                identity_pending: false,
                tracker: quiet::Tracker::new(1, opts.poll_ms, opts.quiet_ms),
                spawned_ms: 1,
                stopped: false,
            },
            ending: supervise::Ending::None,
            moved: None,
            noted: String::new(),
            seen_target: None,
            abandoned: supervise::Abandoned::default(),
            background_hold_until: 0,
            background_found: String::new(),
            link: supervise::link::Link::Absent,
            ladder: crate::relaunch::Ladder::new(),
            restart_deferred: None,
            facts: crate::model_facts::Reader::default(),
        }
    }
    fn runtime(outcome: CommitOutcome) -> (Runtime, Rc<RefCell<Record>>) {
        let record = Rc::new(RefCell::new(Record {
            operation_id: "original-prepare",
            calls: Vec::new(),
            ready: false,
            outcome,
        }));
        // This exact recorded expired value also proves recovery never replaces
        // a clock merely because the admission attempt can no longer proceed.
        let runtime = Runtime {
            host: Box::new(RecordingHost(record.clone())),
            admission: Some(PendingAdmission {
                target: image(2),
                child: child(),
                deadline: CustodyDeadline::from_recorded_monotonic_ns(1).unwrap(),
            }),
            ..Runtime::default()
        };
        (runtime, record)
    }
    fn valid() -> Facts {
        Facts {
            selected: Ok(image(2)),
            moves_allowed: true,
            child_unchanged: true,
            usable_budget: true,
        }
    }
    fn never_dispatch(_: &mut Runtime, _: &mut Supervisor, _: Image) -> io::Result<()> {
        panic!("invalid original operation dispatched")
    }

    #[test]
    fn selection_current_error_changed_and_custody_failure_reconcile_original_before_skips() {
        for case in 0..7 {
            let (mut runtime, record) = runtime(CommitOutcome::Unknown);
            let mut facts = valid();
            match case {
                0 => facts.selected = Ok(image(1)), // Current is back to the running image.
                1 => facts.selected = Err(io::Error::other("selection unreadable")),
                2 => facts.selected = Ok(image(3)),
                3 => runtime.attempted = Some(image(2)), // custody() previously refused.
                4 => facts.usable_budget = false,
                5 => facts.child_unchanged = false,
                6 => facts.moves_allowed = false,
                _ => unreachable!(),
            }
            runtime.service_pending(&mut supervisor(), facts, never_dispatch);
            assert_eq!(record.borrow().calls, [Call::Recover(1, 321)]);
            assert!(runtime.blocks_vendor_move());
            assert_eq!(
                runtime.admission.as_ref().unwrap().deadline.monotonic_ns(),
                1
            );
            assert_eq!(runtime.admission.as_ref().unwrap().target, image(2));
        }
    }

    #[test]
    fn unknown_keeps_original_operation_but_positive_abort_clears_and_defers() {
        let (mut runtime, record) = runtime(CommitOutcome::Unknown);
        let mut sup = supervisor();
        let mut facts = valid();
        facts.usable_budget = false;
        runtime.service_pending(&mut sup, facts, never_dispatch);
        assert!(runtime.admission.is_some());
        assert!(runtime.blocks_vendor_move());
        record.borrow_mut().outcome = CommitOutcome::ProvenNotCommittedRollbackSafe;
        let before = Instant::now();
        let mut facts = valid();
        facts.selected = Err(io::Error::other("still unreadable"));
        runtime.service_pending(&mut sup, facts, never_dispatch);
        assert_eq!(
            record.borrow().calls,
            [Call::Recover(1, 321), Call::Recover(1, 321)]
        );
        assert!(runtime.admission.is_none());
        assert!(!runtime.blocks_vendor_move());
        assert!(
            runtime
                .attempt_retry
                .after
                .is_some_and(|after| after > before)
        );
    }

    #[test]
    fn unchanged_valid_queued_prepare_retries_same_admit_until_dispatch_without_new_clock() {
        let (mut runtime, record) = runtime(CommitOutcome::Unknown);
        let mut sup = supervisor();
        runtime.admission.as_mut().unwrap().deadline =
            CustodyDeadline::after(super::super::TRANSACTION).unwrap();
        let original_deadline = runtime.admission.as_ref().unwrap().deadline.monotonic_ns();
        // Only dispatch is injected: selection/budget/child observations are
        // readonly controller data, and the real Host interface records every
        // retry against the same original transaction rather than cancelling it.
        for ready in [false, true] {
            record.borrow_mut().ready = ready;
            let witness = record.clone();
            runtime.service_pending(&mut sup, valid(), move |runtime, _, target| {
                let original = runtime.admission.as_ref().unwrap();
                let process = procs::ProcessIdentity {
                    pid: std::process::id(),
                    parent: 1,
                    pgid: 1,
                    session: 1,
                    birth: procs::Birth::Linux(1),
                    executable: image(1).executable,
                };
                if runtime.host.admit(&Admission {
                    process: &process,
                    child: &original.child,
                    target: &target,
                    kind: MoveKind::OwnImage,
                    deadline: original.deadline,
                })? {
                    witness.borrow_mut().calls.push(Call::Dispatched(
                        "original-prepare",
                        original.deadline.monotonic_ns(),
                    ));
                }
                Ok(())
            });
        }
        assert_eq!(
            record.borrow().calls,
            [
                Call::Admit("original-prepare", original_deadline, 321, 2),
                Call::Admit("original-prepare", original_deadline, 321, 2),
                Call::Dispatched("original-prepare", original_deadline)
            ]
        );
        assert_eq!(
            runtime.admission.as_ref().unwrap().deadline.monotonic_ns(),
            original_deadline
        );
    }

    #[test]
    fn a_failure_before_any_admission_defers_the_retry_and_a_pending_one_keeps_its_clock() {
        use super::super::RETRY_FIRST;
        use std::time::Duration;
        let sup = supervisor();
        let refused = io::Error::other("own image is not foreground");
        let now = Instant::now();
        let mut fresh = Runtime::default();
        assert!(fresh.attempt_retry.ready(now));
        fresh.attempt_failed(&sup, &refused, now);
        assert!(fresh.admission.is_none() && !fresh.blocks_vendor_move());
        assert!(!fresh.attempt_retry.ready(now));
        assert!(
            !fresh
                .attempt_retry
                .ready(now + RETRY_FIRST - Duration::from_nanos(1))
        );
        assert!(fresh.attempt_retry.ready(now + RETRY_FIRST));
        // The next failure waits twice as long: the shared doubling schedule.
        let later = now + RETRY_FIRST;
        fresh.attempt_failed(&sup, &refused, later);
        assert!(!fresh.attempt_retry.ready(later + RETRY_FIRST));
        assert!(fresh.attempt_retry.ready(later + RETRY_FIRST * 2));

        // An admission that exists is serviced by the next tick on its own clock.
        let (mut pending, _) = runtime(CommitOutcome::Unknown);
        pending.attempt_failed(&sup, &refused, now);
        assert!(pending.admission.is_some());
        assert!(pending.attempt_retry.after.is_none());
    }

    #[test]
    fn a_host_that_never_admits_holds_with_one_line_before_any_image_is_touched() {
        let state = std::env::temp_dir().join(".ubd").join(format!(
            "claude-never-admits-{}-{}.noindex",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let mut sup = supervisor();
        sup.journal = journal::Journal::new(Some(&state), None, std::process::id());
        let mut unavailable = Runtime::default();
        for _ in 0..3 {
            assert!(unavailable.hold_without_host(&sup));
        }
        assert!(
            unavailable.running.is_none()
                && unavailable.probe.is_none()
                && unavailable.seen.is_none()
                && unavailable.compatible.is_none()
        );
        let text = std::fs::read_to_string(sup.journal.path().unwrap()).unwrap();
        assert_eq!(
            text.matches("harness-image held host-never-admits").count(),
            1,
            "{text}"
        );
        assert_eq!(text.lines().count(), 1, "{text}");

        // A provider that is merely not ready yet, and the local one, go on.
        let (mut recording, _) = runtime(CommitOutcome::Unknown);
        assert!(!recording.hold_without_host(&sup));
        let mut local = Runtime::new(|| {
            Box::new(super::super::LocalOwnSupervisor {
                ticket: None,
                prepared: false,
            })
        });
        assert!(!local.hold_without_host(&sup));
        assert_eq!(
            std::fs::read_to_string(sup.journal.path().unwrap()).unwrap(),
            text
        );
        let _ = std::fs::remove_dir_all(&state);
    }
}
