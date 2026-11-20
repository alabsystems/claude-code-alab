// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! A restart that names a model: what waits, what the resume line says, and
//! what is kept for every later relaunch.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::Instant;

use super::tests::{ask, supervisor_of_an_unsignallable_child};
use super::*;

pub(super) const SID: &str = "7c1e2a90-4b3d-4f6e-9a18-2d5c6b7e8f01";

/// A fresh scratch folder for one test: its journal's state home, and the
/// working directory and Claude config its supervisor reads.
pub(super) fn scratch(name: &str) -> PathBuf {
    let root = std::env::temp_dir().join(".ubd").join(format!(
        "claude-alab-model-{name}-{}-{}.noindex",
        std::process::id(),
        now_ms()
    ));
    std::fs::create_dir_all(&root).expect("a scratch folder");
    root
}

/// The events of the journal under `state`, without their time and pid.
pub(super) fn events(state: &Path) -> Vec<String> {
    std::fs::read_to_string(state.join(profile::REPO).join("journal.log"))
        .unwrap_or_default()
        .lines()
        .map(|line| line.split_once(" pid=").map_or(line, |(_, rest)| rest))
        .map(|rest| rest.split_once(' ').map_or(rest, |(_, event)| event))
        .map(str::to_string)
        .collect()
}

/// A restart aterm asks for `cause`, onto `model`.
pub(super) fn onto(cause: &str, model: &str) -> Restart {
    Restart {
        cause: cause.to_string(),
        model: Some(model.to_string()),
        account: None,
    }
}

/// A supervisor launched with `args` in `root`, and its child's record there.
pub(super) fn launched(root: &Path, args: &[&str]) -> (Supervisor, SessionRecord) {
    let mut sup = supervisor_of_an_unsignallable_child(root);
    sup.setup.args = args.iter().map(OsString::from).collect();
    sup.cwd = Some(root.to_path_buf());
    sup.config = root.join("config");
    let record = SessionRecord {
        pid: 1,
        session_id: SID.to_string(),
        cwd: root.to_string_lossy().into_owned(),
        status: profile::STATUS_IDLE.to_string(),
        status_updated_at_ms: 1,
        started_at_ms: 1,
        kind: profile::KIND_INTERACTIVE.to_string(),
        version: "9.9.1".to_string(),
    };
    (sup, record)
}

fn line(words: &[&str]) -> Vec<String> {
    words.iter().map(|word| (*word).to_string()).collect()
}

#[test]
fn a_model_restart_resumes_on_the_named_model_and_every_later_resume_keeps_it() {
    let root = scratch("kept");
    let (mut sup, record) = launched(
        &root,
        &["--dangerously-skip-permissions", "--model", "fable"],
    );
    // As launched: the launch's own model.
    assert_eq!(
        sup.resume_plan(&record).unwrap(),
        line(&[
            "--dangerously-skip-permissions",
            "--model",
            "fable",
            "--resume",
            SID
        ])
    );
    // The restart's own resume line names the model before anything is kept.
    let on_opus = line(&[
        "--dangerously-skip-permissions",
        "--model",
        "claude-opus-5-5",
        "--resume",
        SID,
    ]);
    assert_eq!(
        sup.resume_plan_on(&record, Some("claude-opus-5-5"))
            .unwrap(),
        on_opus
    );
    assert_eq!(sup.model, None, "nothing is kept before the child is moved");
    // Moved: a crash relaunch, a vendor update and an asked restart all write
    // their resume line through `resume_plan`, which now names the model.
    sup.model_moved("claude-opus-5-5".to_string(), "model");
    assert_eq!(sup.model.as_deref(), Some("claude-opus-5-5"));
    assert_eq!(sup.resume_plan(&record).unwrap(), on_opus);
    // Another model restart names a different one.
    sup.model_moved("sonnet".to_string(), "model");
    assert_eq!(
        sup.resume_plan(&record).unwrap(),
        line(&[
            "--dangerously-skip-permissions",
            "--model",
            "sonnet",
            "--resume",
            SID
        ])
    );
    // Naming the launch's own model again restores the launch's line.
    sup.model_moved("fable".to_string(), "model");
    assert_eq!(sup.model, None);
    assert_eq!(
        sup.resume_plan(&record).unwrap(),
        line(&[
            "--dangerously-skip-permissions",
            "--model",
            "fable",
            "--resume",
            SID
        ])
    );
    assert_eq!(
        events(&root),
        [
            "model-move to=claude-opus-5-5 from=fable cause=model kept=switched back-at=-",
            "model-move to=sonnet from=claude-opus-5-5 cause=model kept=switched back-at=-",
            "model-move to=fable from=sonnet cause=model kept=launch back-at=-",
        ]
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_launch_that_named_no_model_keeps_the_one_it_is_moved_onto() {
    let root = scratch("unnamed");
    let (mut sup, record) = launched(&root, &["--dangerously-skip-permissions"]);
    sup.model_moved("claude-opus-5-5".to_string(), "model");
    assert_eq!(
        sup.resume_plan(&record).unwrap(),
        line(&[
            "--dangerously-skip-permissions",
            "--model",
            "claude-opus-5-5",
            "--resume",
            SID
        ])
    );
    assert_eq!(
        events(&root),
        ["model-move to=claude-opus-5-5 from=- cause=model kept=switched back-at=-"]
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_waiting_model_restart_is_journalled_with_its_model_and_survives_a_later_plain_one() {
    let root = scratch("deferred");
    let mut sup = supervisor_of_an_unsignallable_child(&root);
    // No record yet: the request waits, named with its model.
    sup.restart_asked(&onto("model", "claude-opus-5-5"));
    assert_eq!(
        sup.restart_deferred.as_ref().map(|d| d.restart.clone()),
        Some(onto("model", "claude-opus-5-5"))
    );
    sup.restart_retry();
    // A later request that names no model replaces the cause and keeps the
    // model: the one restart then answers both.
    sup.restart_asked(&ask("asked"));
    assert_eq!(
        sup.restart_deferred.as_ref().map(|d| d.restart.clone()),
        Some(onto("asked", "claude-opus-5-5"))
    );
    // A later model replaces an earlier one.
    sup.restart_asked(&onto("model", "fable"));
    assert_eq!(
        sup.restart_deferred.as_ref().map(|d| d.restart.clone()),
        Some(onto("model", "fable"))
    );
    // The child is replaced: the model was asked of the conversation, so the
    // request stays for the child that runs now.
    sup.restart_moot();
    assert_eq!(
        sup.restart_deferred.as_ref().map(|d| d.restart.clone()),
        Some(onto("model", "fable"))
    );
    sup.restart_retry();
    assert_eq!(
        sup.model, None,
        "a restart that was never made moves nothing"
    );
    assert_eq!(
        events(&root),
        [
            "aterm restart cause=model model=claude-opus-5-5 deferred no-record",
            "aterm restart cause=asked model=claude-opus-5-5 deferred no-record",
            "aterm restart cause=model model=fable deferred no-record",
        ]
    );
    // CONTROL: a request that names no model was for the child that is gone.
    sup.restart_deferred = None;
    sup.restart_asked(&ask("stall"));
    sup.restart_moot();
    assert_eq!(sup.restart_deferred, None);
    assert_eq!(
        events(&root)[3..],
        [
            "aterm restart cause=stall deferred no-record",
            "aterm restart cause=stall ignored child-replaced",
        ]
    );
    let _ = std::fs::remove_dir_all(&root);
}

/// A plan pinned for the build under `root`, resuming with `resume`: the
/// build is fixture data that is never run.
pub(super) fn pinned(root: &Path, resume: Vec<String>) -> vendor::Plan {
    use std::os::unix::fs::PermissionsExt as _;
    let dir = root.join("prefix/store/claude/1");
    let exe = dir.join("bin/claude");
    std::fs::create_dir_all(exe.parent().expect("a bin folder")).expect("a build folder");
    std::fs::write(&exe, b"fixture data, never a launched program\n").expect("a vendor file");
    std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o700)).expect("executable");
    let build = Build {
        number: 1,
        dir,
        exe,
        version: Some("9.9.1".to_string()),
        env: Vec::new(),
    };
    vendor::Plan::capture(build.clone(), build, SID.to_string(), resume, 0, None)
        .expect("a pinned plan")
}

#[test]
fn a_model_restart_that_meets_another_ending_waits_for_the_relaunch_it_makes() {
    let root = scratch("meets");
    let (mut sup, record) = launched(&root, &["--dangerously-skip-permissions"]);
    let plan = pinned(&root, sup.resume_plan(&record).unwrap());
    let endings = [
        (
            Ending::ForUpdate {
                sent: Instant::now(),
                plan: plan.clone(),
            },
            "update-ending",
        ),
        (
            Ending::ForRestart {
                sent: Instant::now(),
                plan,
                cause: "asked".to_string(),
                model: None,
                killed: false,
                own: false,
            },
            "restart-ending",
        ),
        (
            super::tests::crashed(ChildEvent::Exited(7)),
            "crash-pending",
        ),
    ];
    let mut expected = Vec::new();
    for (ending, why) in endings {
        sup.ending = ending.clone();
        // CONTROL: a request that names no model is the standing ending's to
        // answer alone, as before.
        sup.restart_asked(&ask("stall"));
        assert_eq!(sup.restart_deferred, None, "{why}");
        expected.push(format!("aterm restart cause=stall ignored {why}"));
        // One that names a model waits, journalled once however often the
        // loop looks at it again, and the ending stands untouched.
        sup.restart_asked(&onto("model", "claude-opus-5-5"));
        sup.restart_retry();
        sup.restart_retry();
        assert_eq!(
            sup.restart_deferred.as_ref().map(|d| (
                d.restart.clone(),
                d.why.as_str(),
                d.on_aterm_since_ms
            )),
            Some((onto("model", "claude-opus-5-5"), why, None)),
            "{why}"
        );
        assert_eq!(sup.ending, ending, "{why}");
        expected.push(format!(
            "aterm restart cause=model model=claude-opus-5-5 deferred {why}"
        ));
        sup.restart_deferred = None;
    }
    // CONTROL: a session that is itself ending makes no relaunch to wait for.
    sup.ending = Ending::Passed(libc::SIGTERM);
    sup.restart_asked(&onto("model", "claude-opus-5-5"));
    assert_eq!(sup.restart_deferred, None);
    expected
        .push("aterm restart cause=model model=claude-opus-5-5 ignored session-ending".to_string());
    assert_eq!(events(&root), expected);
    assert_eq!(sup.model, None, "nothing is kept before a child runs on it");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn the_relaunch_another_ending_makes_is_made_on_the_model_that_waited() {
    let root = scratch("joins");
    let (mut sup, record) = launched(&root, &["--dangerously-skip-permissions"]);
    let as_launched = sup.resume_plan(&record).unwrap();
    let on_opus = line(&[
        "--dangerously-skip-permissions",
        "--model",
        "claude-opus-5-5",
        "--resume",
        SID,
    ]);
    let waiting = |restart: Restart, why: &str| Deferred {
        restart,
        record: None,
        why: why.to_string(),
        on_aterm_since_ms: None,
        retry_at_ms: 0,
        read_ms: 0,
    };
    // CONTROL: with nothing waiting the plan is the one that was pinned, and
    // the model its own line carries is the one to keep.
    let plan = pinned(&root, as_launched.clone());
    let own = Some(("fable".to_string(), "model".to_string()));
    let (same, moved) = sup.model_joins(plan.clone(), &Cause::Update, own.clone());
    assert_eq!((&same, &moved), (&plan, &own));
    // CONTROL: a waiting request that names no model joins nothing, and is
    // dropped with the child it was for.
    sup.restart_deferred = Some(waiting(ask("stall"), "no-record"));
    let (same, moved) = sup.model_joins(plan.clone(), &Cause::Update, None);
    assert_eq!((&same, &moved), (&plan, &None));
    sup.restart_moot();
    assert_eq!(sup.restart_deferred, None);
    // A waiting model request is made by the relaunch: both launches of the
    // plan resume on the model, every pin kept, and the request is over.
    sup.restart_deferred = Some(waiting(onto("model", "claude-opus-5-5"), "update-ending"));
    let (joined, moved) = sup.model_joins(plan.clone(), &Cause::Update, None);
    assert_eq!(joined.resume, on_opus);
    for launch in [&joined.old, &joined.target] {
        assert_eq!(
            launch.args,
            on_opus.iter().map(OsString::from).collect::<Vec<_>>()
        );
        assert_eq!(launch.environment, plan.target.environment);
    }
    joined
        .revalidate_all()
        .expect("the pins are the plan's own");
    assert_eq!(
        moved,
        Some(("claude-opus-5-5".to_string(), "model".to_string()))
    );
    assert_eq!(sup.restart_deferred, None);
    // It replaces the model the plan's own restart named, and keeps the
    // cause of a later request that named none.
    sup.restart_deferred = Some(waiting(onto("asked", "sonnet"), "restart-ending"));
    let (joined, moved) = sup.model_joins(joined, &Cause::Asked("model: fable".to_string()), own);
    assert_eq!(
        joined.resume,
        line(&[
            "--dangerously-skip-permissions",
            "--model",
            "sonnet",
            "--resume",
            SID
        ])
    );
    assert_eq!(moved, Some(("sonnet".to_string(), "asked".to_string())));
    // Kept from there like any model the child was moved onto.
    sup.model_moved("sonnet".to_string(), "asked");
    assert_eq!(sup.model.as_deref(), Some("sonnet"));
    // The journal writes the argv separator escaped.
    let sep = "\\u{1f}";
    assert_eq!(
        events(&root),
        [
            "aterm restart cause=stall ignored child-replaced".to_string(),
            format!(
                "aterm restart cause=model model=claude-opus-5-5 joined update argv={}",
                on_opus.join(sep)
            ),
            format!(
                "aterm restart cause=asked model=sonnet joined restart argv={}",
                joined.resume.join(sep)
            ),
            "model-move to=sonnet from=- cause=asked kept=switched back-at=-".to_string(),
        ]
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_restart_read_while_the_child_is_replaced_keeps_only_its_model() {
    let root = scratch("replaced");
    let mut sup = supervisor_of_an_unsignallable_child(&root);
    // CONTROL: a request that names no model was for the child that ended,
    // and one that was waiting before it is left as it was.
    sup.restart_asked(&ask("asked"));
    sup.restart_for_replaced(&ask("stall"));
    assert_eq!(
        sup.restart_deferred.as_ref().map(|d| d.restart.clone()),
        Some(ask("asked"))
    );
    // One that names a model is kept for the conversation.
    sup.restart_for_replaced(&onto("model", "claude-opus-5-5"));
    assert_eq!(
        sup.restart_deferred
            .as_ref()
            .map(|d| (d.restart.clone(), d.why.as_str())),
        Some((onto("model", "claude-opus-5-5"), "child-replaced"))
    );
    assert_eq!(
        events(&root),
        [
            "aterm restart cause=asked deferred no-record",
            "aterm restart cause=stall ignored child-replaced",
            "aterm restart cause=model model=claude-opus-5-5 deferred child-replaced",
        ]
    );
    let _ = std::fs::remove_dir_all(&root);
}

/// Write `mode` as the newest `permission-mode` line of the conversation's
/// transcript under the supervisor's config.
pub(super) fn history(sup: &Supervisor, mode: &str) {
    let dir = sup.config.join("projects").join("-w");
    std::fs::create_dir_all(&dir).expect("a project folder");
    std::fs::write(
        dir.join(format!("{SID}.jsonl")),
        format!(
            "{{\"type\":\"permission-mode\",\"permissionMode\":\"plan\",\"sessionId\":\"x\"}}\n\
             {{\"type\":\"permission-mode\",\"permissionMode\":\"{mode}\",\"sessionId\":\"x\"}}\n"
        ),
    )
    .expect("a transcript");
}

#[test]
fn a_relaunch_never_widens_an_explicit_launch_from_the_transcripts_history() {
    let root = scratch("ceiling");
    // The reproduced defect: launched in `default`, with a transcript whose
    // newest mode line (from before this launch, for all the harness knows)
    // says `auto` or `dontAsk`.
    let (sup, record) = launched(&root, &["--permission-mode", "default", "--model", "fable"]);
    let as_launched = line(&[
        "--permission-mode",
        "default",
        "--model",
        "fable",
        "--resume",
        SID,
    ]);
    assert_eq!(sup.resume_plan(&record).unwrap(), as_launched, "unread");
    for wider in ["auto", "dontAsk", "acceptEdits"] {
        history(&sup, wider);
        assert_eq!(sup.resume_plan(&record).unwrap(), as_launched, "{wider}");
        // A model restart's line is held to the same launch.
        assert_eq!(
            sup.resume_plan_on(&record, Some("claude-opus-5-5"))
                .unwrap(),
            line(&[
                "--permission-mode",
                "default",
                "--model",
                "claude-opus-5-5",
                "--resume",
                SID
            ]),
            "{wider}"
        );
    }
    // CONTROL: a narrower mode is the conversation's own and is carried.
    history(&sup, "plan");
    assert_eq!(
        sup.resume_plan(&record).unwrap(),
        line(&[
            "--model",
            "fable",
            "--permission-mode",
            "plan",
            "--resume",
            SID
        ])
    );
    // CONTROL: the same history under a launch that started in bypass is
    // carried: every mode is within bypass.
    let (bypass, record) = launched(&root, &["--dangerously-skip-permissions"]);
    history(&bypass, "auto");
    assert_eq!(
        bypass.resume_plan(&record).unwrap(),
        line(&[
            "--allow-dangerously-skip-permissions",
            "--permission-mode",
            "auto",
            "--resume",
            SID
        ])
    );
    // A launch that left bypass reachable and named a narrower mode is held
    // to the mode it named, on a model restart's line as on any other.
    let (reachable, record) = launched(
        &root,
        &[
            "--allow-dangerously-skip-permissions",
            "--permission-mode",
            "plan",
        ],
    );
    for wider in ["auto", "acceptEdits", "default"] {
        history(&reachable, wider);
        assert_eq!(
            reachable.resume_plan(&record).unwrap(),
            line(&[
                "--allow-dangerously-skip-permissions",
                "--permission-mode",
                "plan",
                "--resume",
                SID
            ]),
            "{wider}"
        );
        assert_eq!(
            reachable
                .resume_plan_on(&record, Some("claude-opus-5-5"))
                .unwrap(),
            line(&[
                "--allow-dangerously-skip-permissions",
                "--permission-mode",
                "plan",
                "--model",
                "claude-opus-5-5",
                "--resume",
                SID
            ]),
            "{wider}"
        );
    }
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_restart_onto_the_launchs_model_drops_the_switched_one_and_pins_its_words_until_answered() {
    let root = scratch("launch-word");
    // The commonest launch: no `--model`, so Claude Code's settings chose.
    let (mut sup, record) = launched(&root, &["--dangerously-skip-permissions"]);
    let as_launched = line(&["--dangerously-skip-permissions", "--resume", SID]);
    let on_fable = line(&[
        "--dangerously-skip-permissions",
        "--model",
        "fable",
        "--resume",
        SID,
    ]);
    let back = onto("model", MODEL_LAUNCH);
    sup.model_moved("claude-opus-5-5".to_string(), "model");
    assert_eq!(
        sup.model_for(&ask("asked")).as_deref(),
        Some("claude-opus-5-5")
    );
    // The word asks for the launch's own model whatever the session is on.
    // The launch named none, and a line that names none takes up the
    // conversation's newest model, the one it was moved onto: the line
    // names the alias.
    assert_eq!(sup.model_for(&back).as_deref(), Some("fable"));
    assert_eq!(
        sup.resume_plan_on(&record, sup.model_for(&back).as_deref())
            .unwrap(),
        on_fable
    );
    sup.model_moved(MODEL_LAUNCH.to_string(), "model");
    assert_eq!(sup.model, None, "the word is never kept as a model");
    assert_eq!(sup.limit.phase, limit::Phase::OnLaunch { returned: true });
    // Until the child answers on it, the conversation's newest model is still
    // the one it was moved off: a vendor update's or a crash relaunch's line,
    // and a plain restart's, name the alias too.
    assert_eq!(sup.resume_plan(&record).unwrap(), on_fable);
    assert_eq!(sup.model_for(&ask("asked")).as_deref(), Some("fable"));
    // A restart onto it, or onto the launch's line, asks for nothing new.
    assert!(sup.already_on(Some("fable")));
    assert!(sup.already_on(Some(MODEL_LAUNCH)));
    // Once it has answered, the launch's own line is reproduced exactly.
    sup.limit.answered();
    assert_eq!(sup.resume_plan(&record).unwrap(), as_launched);
    assert_eq!(sup.model_for(&ask("asked")), None);
    // CONTROL: a move onto the launch's line from the launch's line itself
    // was no return, and pins nothing.
    sup.model_moved(MODEL_LAUNCH.to_string(), "model");
    assert_eq!(sup.limit.phase, limit::Phase::OnLaunch { returned: false });
    assert_eq!(sup.resume_plan(&record).unwrap(), as_launched);
    // CONTROL: the alias aterm had to send before the word existed is a
    // model like any other, and pins a flag the launch never had.
    sup.model_moved("claude-opus-5-5".to_string(), "model");
    sup.model_moved("fable".to_string(), "model");
    assert_eq!(sup.model.as_deref(), Some("fable"));
    assert_eq!(
        sup.resume_plan(&record).unwrap(),
        line(&[
            "--dangerously-skip-permissions",
            "--model",
            "fable",
            "--resume",
            SID
        ])
    );
    // CONTROL: a restart with no `model=` keeps what the session is on.
    assert_eq!(sup.model_for(&ask("asked")).as_deref(), Some("fable"));
    // A launch that named a model gets its own words back, and the journal
    // tells the word from a move onto that model by name.
    let named = scratch("launch-word-named");
    let (mut sup, record) = launched(&named, &["--model", "fable[1m]"]);
    sup.model_moved("claude-opus-5-5".to_string(), "model");
    sup.model_moved(MODEL_LAUNCH.to_string(), "model");
    assert_eq!(sup.model, None);
    // CONTROL: a return that has yet to answer, with nothing to pin: the
    // launch's own line names its model.
    assert_eq!(sup.limit.phase, limit::Phase::OnLaunch { returned: true });
    assert_eq!(sup.launch_pin(), None);
    assert_eq!(
        sup.resume_plan(&record).unwrap(),
        line(&["--model", "fable[1m]", "--resume", SID])
    );
    // The word joins a relaunch like a model: the plan goes back to the
    // launch's own line.
    sup.model_moved("claude-opus-5-5".to_string(), "model");
    let plan = pinned(&named, sup.resume_plan(&record).unwrap());
    sup.restart_deferred = Some(Deferred {
        restart: back.clone(),
        record: None,
        why: "update-ending".to_string(),
        on_aterm_since_ms: None,
        retry_at_ms: 0,
        read_ms: 0,
    });
    let (joined, moved) = sup.model_joins(plan, &Cause::Update, None);
    assert_eq!(
        joined.resume,
        line(&["--model", "fable[1m]", "--resume", SID])
    );
    assert_eq!(moved, Some((MODEL_LAUNCH.to_string(), "model".to_string())));
    assert_eq!(
        events(&root),
        [
            "model-move to=claude-opus-5-5 from=- cause=model kept=switched back-at=-",
            "model-move to=launch launch-model=- from=claude-opus-5-5 cause=model kept=launch \
             back-at=-",
            "model-move to=launch launch-model=- from=- cause=model kept=launch back-at=-",
            "model-move to=claude-opus-5-5 from=- cause=model kept=switched back-at=-",
            "model-move to=fable from=claude-opus-5-5 cause=model kept=switched back-at=-",
        ]
    );
    assert_eq!(
        events(&named)[..2],
        [
            "model-move to=claude-opus-5-5 from=fable[1m] cause=model kept=switched back-at=-",
            "model-move to=launch launch-model=fable[1m] from=claude-opus-5-5 cause=model \
             kept=launch back-at=-",
        ]
    );
    let _ = std::fs::remove_dir_all(&root);
    let _ = std::fs::remove_dir_all(&named);
}
