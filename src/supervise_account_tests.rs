// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! aterm's account moves in the supervisor: a request for an account waits
//! for a relaunch already under way and is made by it, and a start onto
//! another account is the session's move, said once.

use super::model_tests::{SID, events, launched, pinned, scratch};
use super::tests::{ask, crashed};
use super::*;
use crate::account::LOGIN_ENV;
use crate::aterm::Account;

/// Two accounts' keys, as aterm names them in a restart's `to=`.
const WORK: &str = "ac-0123456789abcdef";
const HOME: &str = "ac-fedcba9876543210";

fn work(root: &Path) -> Account {
    Account {
        key: WORK.to_string(),
        home: Some(root.join("login-work")),
    }
}

fn onto_account(account: &Account) -> Restart {
    Restart {
        cause: "account".to_string(),
        model: None,
        account: Some(account.clone()),
    }
}

/// The start that moved `sup`'s session onto `account` for `cause`, its
/// vendor seen running by `record`. Whether it was seen.
fn started_on(
    sup: &mut Supervisor,
    root: &Path,
    account: &Account,
    cause: &str,
    record: &SessionRecord,
) -> bool {
    let before = pinned(root, vec!["--resume".to_string(), SID.to_string()]);
    sup.account_admitting(account.clone(), before, &Cause::Asked(cause.to_string()));
    sup.account_seen(Some(record))
}

/// Write the child's own session record where `sup` reads it: this
/// conversation of the one child, in `status` since `status_at`.
fn recorded(sup: &Supervisor, root: &Path, status: &str, status_at: u64) {
    let path = profile::record_path(&sup.config, 0);
    std::fs::create_dir_all(path.parent().expect("a sessions folder")).expect("sessions");
    std::fs::write(
        &path,
        format!(
            r#"{{"pid":0,"sessionId":"{SID}","cwd":"{}","startedAt":1,"version":"9.9.1","kind":"{}","status":"{status}","statusUpdatedAt":{status_at}}}"#,
            root.display(),
            profile::KIND_INTERACTIVE,
        ),
    )
    .expect("a session record");
    assert!(sup.record().is_some(), "the record is the child's");
}

/// A supervisor on an open contract 4 stream of a quiet fake aterm, its
/// claim taking the account moves and its launch reaching a token helper,
/// over a child with an idle record of its own and a build that can be
/// started again. The child cannot be signalled (pid 0), so a restart that
/// gets as far as its signal ends nothing and is journalled `ignored
/// signal-failed`: that line says the request was made, and no other does.
fn at_the_signal(name: &str) -> (alab_wire::fake::FakeAterm, Supervisor, PathBuf) {
    let (fake, mut sup, _, root) =
        super::link::tests::streaming(name, &["--dangerously-skip-permissions"]);
    sup.link.accounts_granted = true;
    sup.link.token_helper = true;
    sup.child.build = pinned(&root, Vec::new()).target.build.clone();
    recorded(&sup, &root, profile::STATUS_IDLE, 1);
    (fake, sup, root)
}

/// The last journal line of `root`.
fn last_event(root: &Path) -> String {
    events(root).pop().unwrap_or_default()
}

/// What waits in `sup`: the request's cause and why it waits.
fn waiting(sup: &Supervisor) -> Option<(&str, &str)> {
    sup.restart_deferred
        .as_ref()
        .map(|deferred| (deferred.restart.cause.as_str(), deferred.why.as_str()))
}

/// The value `launch` gives the login directory variable, if any.
fn login_of(launch: &vendor::Launch) -> Option<&OsString> {
    launch
        .environment
        .iter()
        .find(|(key, _)| key.as_os_str() == std::ffi::OsStr::new(LOGIN_ENV))
        .map(|(_, value)| value)
}

#[test]
fn an_account_restart_that_meets_a_crash_waits_and_is_made_by_its_relaunch() {
    let root = scratch("account-joins");
    let (mut sup, record) = launched(&root, &["--dangerously-skip-permissions"]);
    let plan = pinned(&root, sup.resume_plan(&record).unwrap());
    let account = work(&root);
    // CONTROL: no account named leaves the pinned plan as it was.
    assert_eq!(plan.clone().with_account(None), plan);
    sup.ending = crashed(ChildEvent::Exited(7));
    // CONTROL: a request that names neither model nor account is the
    // standing ending's to answer alone.
    sup.restart_asked(&ask("stall"));
    assert_eq!(sup.restart_deferred, None);
    // CONTROL: a claim that does not take `accounts` makes no account move.
    sup.restart_asked(&onto_account(&account));
    assert_eq!(sup.restart_deferred, None);
    sup.link.accounts_granted = true;
    sup.link.token_helper = true;
    sup.restart_asked(&onto_account(&account));
    sup.restart_retry();
    assert_eq!(
        sup.restart_deferred
            .as_ref()
            .map(|deferred| (deferred.restart.clone(), deferred.why.as_str())),
        Some((onto_account(&account), "crash-pending"))
    );
    let (joined, moved) = sup.model_joins(plan.clone(), &Cause::Crash, None);
    assert_eq!(moved, None, "an account names no model");
    assert_eq!(sup.restart_deferred, None);
    for launch in [&joined.old, &joined.target] {
        assert_eq!(launch.account.as_ref(), Some(&account));
        assert_eq!(
            login_of(launch),
            Some(&root.join("login-work").into_os_string())
        );
    }
    assert_eq!(joined.resume, plan.resume, "the conversation's own line");
    let said = events(&root);
    assert_eq!(
        said[..3],
        [
            "aterm restart cause=stall ignored crash-pending".to_string(),
            "aterm restart cause=account to=ac-0123456789abcdef ignored account-assignment-unsupported"
                .to_string(),
            "aterm restart cause=account to=ac-0123456789abcdef deferred crash-pending".to_string(),
        ]
    );
    assert!(
        said[3]
            .starts_with("aterm restart cause=account to=ac-0123456789abcdef joined crash argv="),
        "{said:?}"
    );
    assert!(
        said.iter().all(|line| !line.contains("login-work")),
        "{said:?}"
    );
    assert!(said.iter().any(|line| line.contains(SID)), "{said:?}");
    let _ = std::fs::remove_dir_all(&root);
}

/// Each refusal of an account move is named, the first that holds first,
/// and none of them waits or ends anything.
#[test]
fn an_account_move_that_is_not_the_harnesss_to_make_is_refused_by_name_in_order() {
    let root = scratch("account-refusals");
    let (mut sup, _) = launched(&root, &["--dangerously-skip-permissions"]);
    let account = work(&root);
    let labelled = Account {
        key: "work".to_string(),
        home: account.home.clone(),
    };
    // No grant: whatever else is wrong with it, the move is aterm's own,
    // one that names no account at all among them.
    sup.restart_asked(&ask("account-ahead"));
    sup.restart_asked(&onto_account(&labelled));
    // Granted: a word that is no key names no account here.
    sup.link.accounts_granted = true;
    sup.restart_asked(&onto_account(&labelled));
    // A key, and no token helper to start the vendor on it.
    sup.restart_asked(&onto_account(&account));
    assert_eq!(sup.restart_deferred, None);
    // The account the session runs on already, key and directory both.
    sup.link.token_helper = true;
    sup.account = Some(account.clone());
    sup.restart_asked(&onto_account(&account));
    assert_eq!(sup.restart_deferred, None);
    assert_eq!(
        events(&root),
        [
            "aterm restart cause=account-ahead ignored account-assignment-unsupported",
            "aterm restart cause=account to=work ignored account-assignment-unsupported",
            "aterm restart cause=account to=work ignored account-word",
            "aterm restart cause=account to=ac-0123456789abcdef ignored account-no-token-helper",
            "aterm restart cause=account to=ac-0123456789abcdef ignored already-on",
        ]
    );
    // CONTROL: the same key on another login directory is another move, and
    // so is another key: each is taken up (no record yet, so it waits).
    for other in [
        Account {
            key: WORK.to_string(),
            home: None,
        },
        Account {
            key: HOME.to_string(),
            home: account.home.clone(),
        },
    ] {
        sup.restart_asked(&onto_account(&other));
        assert_eq!(
            sup.restart_deferred
                .take()
                .map(|deferred| (deferred.restart, deferred.why)),
            Some((onto_account(&other), "no-record".to_string())),
        );
    }
    // CONTROL: a model named beside the account the session runs on is
    // still asked of the conversation.
    let with_model = Restart {
        model: Some("claude-opus-5-5".to_string()),
        ..onto_account(&account)
    };
    sup.restart_asked(&with_model);
    assert_eq!(
        sup.restart_deferred.take().map(|deferred| deferred.restart),
        Some(with_model)
    );
    // A child being ended to start on the account is on its way there.
    let plan =
        pinned(&root, vec!["--resume".to_string(), SID.to_string()]).with_account(Some(&account));
    sup.account = None;
    sup.ending = Ending::ForRestart {
        sent: Instant::now(),
        plan,
        cause: "account".to_string(),
        model: None,
        killed: false,
        own: false,
    };
    sup.restart_asked(&onto_account(&account));
    assert_eq!(sup.restart_deferred, None);
    assert_eq!(
        events(&root).last().map(String::as_str),
        Some("aterm restart cause=account to=ac-0123456789abcdef ignored already-on")
    );
    let _ = std::fs::remove_dir_all(&root);
}

/// The offer of an account move is counted from when it was first read,
/// whatever it waited on since and however often it was asked again; past
/// it the move is not made and joins no relaunch.
#[test]
fn an_account_move_is_made_or_joined_only_while_its_offer_stands() {
    let root = scratch("account-offer");
    let (mut sup, record) = launched(&root, &["--dangerously-skip-permissions"]);
    let account = work(&root);
    let request = onto_account(&account);
    sup.link.accounts_granted = true;
    sup.link.token_helper = true;
    // CONTROL: read just now, it waits for the child's record.
    let before = now_ms();
    sup.restart_asked(&request);
    let read = sup
        .restart_deferred
        .as_ref()
        .map(|deferred| deferred.read_ms);
    assert!(read.is_some_and(|read| (before..=now_ms()).contains(&read)));
    // Asked again, and looked at again, it is the same offer.
    let earlier = before - 1_000;
    sup.restart_deferred.as_mut().expect("waiting").read_ms = earlier;
    sup.restart_asked(&request);
    sup.restart_retry();
    assert_eq!(
        sup.restart_deferred
            .as_ref()
            .map(|deferred| (deferred.read_ms, deferred.why.as_str())),
        Some((earlier, "no-record"))
    );
    // A request that names no account joins it and carries its account: no
    // new offer either. One that names another account is another offer.
    sup.restart_asked(&super::model_tests::onto("model", "claude-opus-5-5"));
    assert_eq!(
        sup.restart_deferred
            .as_ref()
            .map(|deferred| (deferred.read_ms, deferred.restart.account.as_ref())),
        Some((earlier, Some(&account)))
    );
    let other = Account {
        key: HOME.to_string(),
        home: None,
    };
    sup.restart_asked(&onto_account(&other));
    assert!(
        sup.restart_deferred
            .as_ref()
            .is_some_and(|deferred| deferred.read_ms >= before)
    );
    // Past the offer: the next pass ends the wait and makes nothing.
    sup.restart_deferred = None;
    sup.restart_asked(&request);
    sup.restart_deferred.as_mut().expect("waiting").read_ms = now_ms() - ACCOUNT_OFFER_MS;
    sup.restart_retry();
    assert_eq!(sup.restart_deferred, None);
    assert_eq!(sup.ending, Ending::None);
    // A relaunch under way is not made on it either: the plan stays as it
    // was pinned.
    let plan = pinned(&root, sup.resume_plan(&record).unwrap());
    sup.ending = crashed(ChildEvent::Exited(7));
    sup.restart_asked(&request);
    sup.restart_deferred.as_mut().expect("waiting").read_ms = now_ms() - ACCOUNT_OFFER_MS;
    let (joined, moved) = sup.model_joins(plan.clone(), &Cause::Crash, None);
    assert_eq!((joined, moved), (plan, None));
    assert_eq!(sup.restart_deferred, None);
    // A model the same request named has no such bound, and comes back
    // without the account.
    let with_model = Restart {
        model: Some("claude-opus-5-5".to_string()),
        ..request.clone()
    };
    assert_eq!(
        sup.offered(with_model.clone(), now_ms() - ACCOUNT_OFFER_MS),
        Some(Restart {
            account: None,
            ..with_model.clone()
        })
    );
    // CONTROL: inside the offer the request comes back whole.
    assert_eq!(sup.offered(with_model.clone(), now_ms()), Some(with_model));
    let expired: Vec<String> = events(&root)
        .into_iter()
        .filter(|line| line.ends_with(" ignored account-offer-expired"))
        .collect();
    assert_eq!(
        expired,
        [
            "aterm restart cause=account to=ac-0123456789abcdef ignored account-offer-expired",
            "aterm restart cause=account to=ac-0123456789abcdef ignored account-offer-expired",
            "aterm restart cause=account to=ac-0123456789abcdef model=claude-opus-5-5 ignored \
             account-offer-expired",
        ]
    );
    let _ = std::fs::remove_dir_all(&root);
}

/// The child is ended for an account move at a wall only where the harness
/// itself reads it standing at one. aterm's request says nothing of where
/// the child stands: a child that works, and that the harness reads at no
/// limit, is not ended on it.
#[test]
fn an_account_move_takes_a_wall_for_its_safe_point_only_by_the_harnesss_own_witness() {
    let (fake, mut sup, record, root) =
        super::link::tests::streaming("claude-account-point", &["--dangerously-skip-permissions"]);
    let on_aterm = |why: &str| {
        Err(AccountHold {
            why: why.to_string(),
            on_aterm: true,
        })
    };
    // CONTROL: a quiet session is a safe point with no wall read.
    assert_eq!(sup.account_point(&record), Ok(AccountMoment::Quiet));
    // The agent is working, and the harness reads no limit.
    fake.set_status("agent", Some("busy"));
    assert_eq!(
        sup.account_point(&record),
        on_aterm("wait aterm agent=busy")
    );
    // The harness's own witness reads the child at a usage limit: a wall,
    // where the agent is not asked to be idle.
    let at_ms = now_ms();
    sup.limit_source = Box::new(Floored(limit::Wall {
        at_ms,
        resets_at_s: None,
        bucket: Some("five_hour".to_string()),
    }));
    assert_eq!(
        sup.account_point(&record),
        Ok(AccountMoment::Wall { at_ms })
    );
    // At a wall too, what a person is doing holds it.
    fake.set_status("input", Some("unread"));
    assert_eq!(
        sup.account_point(&record),
        on_aterm("wait aterm input=unread")
    );
    // With no claim standing nothing answers, and nothing is ended.
    sup.link = link::Link::absent();
    assert_eq!(sup.account_point(&record), on_aterm("wait aterm unread"));
    let _ = std::fs::remove_dir_all(&root);
}

/// aterm sends the move and is also the one that says the agent is idle.
/// The child's own record is the harness's witness: while it reads a turn
/// in flight the move waits, whatever aterm reads, and it is made once the
/// record is at rest.
#[test]
fn an_account_move_ends_no_child_whose_own_record_reads_a_turn_in_flight() {
    let (_fake, mut sup, root) = at_the_signal("claude-account-busy-record");
    let account = work(&root);
    // aterm reads the tab as quiet (`agent=idle`); the record reads `busy`.
    recorded(&sup, &root, profile::STATUS_BUSY, now_ms());
    sup.restart_asked(&onto_account(&account));
    assert_eq!(waiting(&sup), Some(("account", "wait busy")));
    // It is the harness's own witness that holds: read again on the next
    // pass, with no clock of aterm's on it, and said once.
    assert_eq!(
        sup.restart_deferred
            .as_ref()
            .map(|deferred| deferred.on_aterm_since_ms),
        Some(None)
    );
    sup.restart_retry();
    sup.restart_retry();
    assert_eq!(waiting(&sup), Some(("account", "wait busy")));
    assert_eq!(
        events(&root),
        ["aterm restart cause=account to=ac-0123456789abcdef deferred wait busy"]
    );
    // Any status that is not `idle` is a child at work.
    recorded(&sup, &root, "waiting", now_ms());
    sup.restart_retry();
    assert_eq!(waiting(&sup), Some(("account", "wait busy")));
    // CONTROL: the record at rest, the same request is made. No quiet period
    // is asked of it: aterm decides a move at a turn's end.
    recorded(&sup, &root, profile::STATUS_IDLE, now_ms());
    sup.restart_retry();
    assert_eq!(sup.restart_deferred, None);
    assert_eq!(
        last_event(&root),
        "aterm restart cause=account to=ac-0123456789abcdef ignored signal-failed"
    );
    // The moment is read once more just before the signal: a turn begun, or
    // ended and begun again, since it was judged is not that moment.
    let judged = sup.record().expect("the record");
    assert!(sup.account_still(&judged, AccountMoment::Quiet));
    recorded(
        &sup,
        &root,
        profile::STATUS_IDLE,
        judged.status_updated_at_ms + 1,
    );
    assert!(!sup.account_still(&judged, AccountMoment::Quiet));
    recorded(
        &sup,
        &root,
        profile::STATUS_BUSY,
        judged.status_updated_at_ms,
    );
    assert!(!sup.account_still(&judged, AccountMoment::Quiet));
    let _ = std::fs::remove_dir_all(&root);
}

/// The limit's row stays the newest until a later turn's first answer is
/// written. A turn begun after the refusal is a turn of its own: at a wall
/// the harness reads, a record that reads `busy` holds the move for the
/// limit's patience, as it holds the harness's own move.
#[test]
fn at_a_wall_a_turn_begun_since_the_refusal_holds_an_account_move() {
    let (fake, mut sup, root) = at_the_signal("claude-account-wall-busy");
    let account = work(&root);
    let wall_at = now_ms() - 1_000;
    sup.limit_source = Box::new(Floored(limit::Wall {
        at_ms: wall_at,
        resets_at_s: None,
        bucket: Some("five_hour".to_string()),
    }));
    // aterm reads the tab as busy, which a wall does not ask about.
    fake.set_status("agent", Some("busy"));
    recorded(&sup, &root, profile::STATUS_BUSY, wall_at + 500);
    sup.restart_asked(&onto_account(&account));
    assert_eq!(waiting(&sup), Some(("account", "wait busy")));
    let judged = sup.record().expect("the record");
    assert!(!sup.account_still(&judged, AccountMoment::Wall { at_ms: wall_at }));
    // CONTROL: the child at rest at the same wall is ended for the move,
    // though aterm reads the tab as busy.
    recorded(&sup, &root, profile::STATUS_IDLE, wall_at + 600);
    let judged = sup.record().expect("the record");
    assert!(sup.account_still(&judged, AccountMoment::Wall { at_ms: wall_at }));
    // Another limit's row since is not the moment that was judged.
    assert!(!sup.account_still(&judged, AccountMoment::Wall { at_ms: wall_at - 1 }));
    sup.restart_retry();
    assert_eq!(sup.restart_deferred, None);
    assert_eq!(
        last_event(&root),
        "aterm restart cause=account to=ac-0123456789abcdef ignored signal-failed"
    );
    // The limit the moment was judged at is read once more just before the
    // signal: where the witness no longer reads it (a later turn has
    // answered while aterm was asked), the child is not ended on it.
    sup.limit_source = Box::new(Lifted(Some(limit::Wall {
        at_ms: wall_at,
        resets_at_s: None,
        bucket: Some("five_hour".to_string()),
    })));
    sup.restart_asked(&onto_account(&account));
    assert_eq!(
        waiting(&sup),
        Some(("account", "wait changed-while-deciding"))
    );
    sup.restart_deferred = None;
    // CONTROL: work that has had the limit's patience is moved over, as the
    // harness's own move at a limit is.
    let old = now_ms() - limit::WALL_PATIENCE_MS - 1_000;
    sup.limit_source = Box::new(Floored(limit::Wall {
        at_ms: old,
        resets_at_s: None,
        bucket: Some("five_hour".to_string()),
    }));
    recorded(&sup, &root, profile::STATUS_BUSY, old - 1);
    sup.restart_asked(&onto_account(&account));
    assert_eq!(sup.restart_deferred, None);
    assert_eq!(
        events(&root)
            .iter()
            .filter(|line| line.ends_with(" ignored signal-failed"))
            .count(),
        2
    );
    let _ = std::fs::remove_dir_all(&root);
}

/// aterm's advice ends no child that could not be started again, and none
/// for an account nothing could start it on any more: the helper is looked
/// for again when a move that waited is made.
#[test]
fn an_account_move_that_waited_is_not_made_where_its_start_could_not_be() {
    let (_fake, mut sup, root) = at_the_signal("claude-account-unstartable");
    let account = work(&root);
    recorded(&sup, &root, profile::STATUS_BUSY, now_ms());
    sup.restart_asked(&onto_account(&account));
    assert_eq!(waiting(&sup), Some(("account", "wait busy")));
    // The helper is gone while the move waits.
    sup.link.token_helper = false;
    recorded(&sup, &root, profile::STATUS_IDLE, now_ms());
    sup.restart_retry();
    assert_eq!(sup.restart_deferred, None);
    assert_eq!(
        last_event(&root),
        "aterm restart cause=account to=ac-0123456789abcdef ignored account-no-token-helper"
    );
    // The running build's executable is gone.
    sup.link.token_helper = true;
    let exe = sup.child.build.exe.clone();
    std::fs::remove_file(&exe).expect("the vendor file");
    // Another key: the helper's absence above is no refusal of this one.
    let other = Account {
        key: HOME.to_string(),
        home: None,
    };
    sup.restart_asked(&onto_account(&other));
    assert_eq!(sup.restart_deferred, None);
    assert_eq!(
        last_event(&root),
        "aterm restart cause=account to=ac-fedcba9876543210 ignored vendor-missing build=1"
    );
    // CONTROL: a restart aterm orders is not held to advice's rule: what
    // becomes of it is the plan's and the signal's to say.
    sup.restart_asked(&ask("asked"));
    let ordered = last_event(&root);
    assert!(
        ordered.starts_with("aterm restart cause=asked ignored ")
            && !ordered.contains("vendor-missing"),
        "{:?}",
        events(&root)
    );
    let _ = std::fs::remove_dir_all(&root);
}

/// A restart aterm orders (a person's, a stall's) that arrives while an
/// account move waits is made as it would have been without the move, at
/// once, and the account goes with it. It is not held to the account's
/// safe point, and where the account's offer has run out it is still made,
/// without the account: aterm has told its asker that it is under way.
#[test]
fn a_restart_aterm_orders_is_not_lost_to_an_account_move_that_waits() {
    let (_fake, mut sup, root) = at_the_signal("claude-account-joined-order");
    let account = work(&root);
    recorded(&sup, &root, profile::STATUS_BUSY, now_ms());
    sup.restart_asked(&onto_account(&account));
    assert_eq!(waiting(&sup), Some(("account", "wait busy")));
    // The child is still at work: the ordered restart is made all the same.
    sup.restart_asked(&ask("stall"));
    assert_eq!(sup.restart_deferred, None);
    assert_eq!(
        last_event(&root),
        "aterm restart cause=stall to=ac-0123456789abcdef ignored signal-failed"
    );
    // It waits for something of its own (the child is stopped) past the
    // account's offer: the account is over, the restart is not.
    sup.restart_asked(&onto_account(&account));
    sup.child.stopped = true;
    sup.restart_asked(&ask("asked"));
    assert_eq!(waiting(&sup), Some(("asked", "child-stopped")));
    sup.restart_deferred.as_mut().expect("waiting").read_ms = now_ms() - ACCOUNT_OFFER_MS;
    sup.child.stopped = false;
    sup.restart_retry();
    assert_eq!(sup.restart_deferred, None);
    let said = events(&root);
    assert_eq!(
        said[said.len() - 2..],
        [
            "aterm restart cause=asked to=ac-0123456789abcdef ignored account-offer-expired",
            "aterm restart cause=asked ignored signal-failed",
        ],
        "{said:?}"
    );
    // The same by the offer's own rule, and its CONTROL: a move that is
    // only aterm's advice is over with its offer.
    let joined = Restart {
        account: Some(account.clone()),
        ..ask("stall")
    };
    assert_eq!(
        sup.offered(joined.clone(), now_ms() - ACCOUNT_OFFER_MS),
        Some(ask("stall"))
    );
    assert_eq!(
        sup.offered(onto_account(&account), now_ms() - ACCOUNT_OFFER_MS),
        None
    );
    // Waiting for a relaunch under way past the offer, it asks for nothing
    // more than that relaunch makes, and puts no account on it.
    let record = sup.record().expect("the record");
    let plan = pinned(&root, sup.resume_plan(&record).unwrap());
    sup.ending = crashed(ChildEvent::Exited(7));
    sup.restart_asked(&onto_account(&account));
    sup.restart_deferred = sup.restart_deferred.take().map(|deferred| Deferred {
        restart: joined.clone(),
        read_ms: now_ms() - ACCOUNT_OFFER_MS,
        ..deferred
    });
    assert_eq!(
        sup.model_joins(plan.clone(), &Cause::Crash, None),
        (plan, None)
    );
    assert_eq!(sup.restart_deferred, None);
    assert_eq!(
        last_event(&root),
        "aterm restart cause=stall ignored child-replaced"
    );
    let _ = std::fs::remove_dir_all(&root);
}

/// An account's cause is no order to end the child: a request with one
/// that names no account asks for a move onto nothing and is passed over,
/// and so is one whose login directory no child could be given.
#[test]
fn an_account_cause_that_names_no_startable_account_ends_nothing() {
    use std::os::unix::ffi::OsStringExt as _;
    let (_fake, mut sup, root) = at_the_signal("claude-account-unnamed");
    for cause in ["account", "account-ahead"] {
        sup.restart_asked(&ask(cause));
    }
    // A model beside it does not make it an order either.
    sup.restart_asked(&super::model_tests::onto("account", "claude-opus-5-5"));
    // A login directory with a NUL in it, which the shared link reads and
    // no environment can carry; and one that is not absolute.
    for home in [
        PathBuf::from(OsString::from_vec(b"/tmp/a\0b".to_vec())),
        PathBuf::from("relative/login"),
    ] {
        sup.restart_asked(&onto_account(&Account {
            key: WORK.to_string(),
            home: Some(home),
        }));
    }
    assert_eq!(sup.restart_deferred, None);
    assert_eq!(sup.ending, Ending::None);
    assert_eq!(
        events(&root),
        [
            "aterm restart cause=account ignored account-word",
            "aterm restart cause=account-ahead ignored account-word",
            "aterm restart cause=account model=claude-opus-5-5 ignored account-word",
            "aterm restart cause=account to=ac-0123456789abcdef ignored account-home",
            "aterm restart cause=account to=ac-0123456789abcdef ignored account-home",
        ]
    );
    // CONTROL: a restart aterm orders that names no account is made.
    sup.restart_asked(&ask("stall"));
    assert_eq!(
        last_event(&root),
        "aterm restart cause=stall ignored signal-failed"
    );
    let _ = std::fs::remove_dir_all(&root);
}

/// A child that crashes while an account move waits for it is started
/// again by the crash's relaunch, which the move joins, as it joins one it
/// arrives during. A restart that names nothing has nothing left to
/// restart.
#[test]
fn a_crash_keeps_the_account_move_that_was_waiting_for_its_relaunch() {
    let (_fake, mut sup, root) = at_the_signal("claude-account-crash-keeps");
    let account = work(&root);
    // CONTROL: a plain restart that waited is over with the child.
    sup.child.stopped = true;
    sup.restart_asked(&ask("stall"));
    assert_eq!(waiting(&sup), Some(("stall", "child-stopped")));
    assert!(sup.crash_begins(ChildEvent::Exited(7)));
    assert_eq!(sup.restart_deferred, None);
    assert_eq!(
        events(&root)[1],
        "aterm restart cause=stall ignored child-ended"
    );
    // The account move that waited stays, and the crash's relaunch is made
    // on its account.
    sup.ending = Ending::None;
    recorded(&sup, &root, profile::STATUS_BUSY, now_ms());
    sup.restart_asked(&onto_account(&account));
    assert_eq!(waiting(&sup), Some(("account", "wait busy")));
    assert!(sup.crash_begins(ChildEvent::Exited(7)));
    assert_eq!(waiting(&sup), Some(("account", "wait busy")));
    let record = sup.record().expect("the record");
    let plan = pinned(&root, sup.resume_plan(&record).unwrap());
    let (joined, _) = sup.model_joins(plan, &Cause::Crash, None);
    assert_eq!(joined.target.account.as_ref(), Some(&account));
    assert_eq!(sup.restart_deferred, None);
    assert!(
        events(&root)
            .iter()
            .all(|line| !line.contains("to=ac-0123456789abcdef ignored")),
        "{:?}",
        events(&root)
    );
    let _ = std::fs::remove_dir_all(&root);
}

/// A start on an account is made through aterm's token helper or not at
/// all. The helper that admitted the move may be gone when the old child
/// has ended: the plan then goes back to what was pinned before the
/// account, and the move is journalled as not made.
#[test]
fn a_start_on_an_account_with_no_token_helper_left_goes_back_to_the_plan_before_it() {
    let root = scratch("account-startable");
    let (mut sup, record) = launched(&root, &["--dangerously-skip-permissions"]);
    let before = pinned(&root, sup.resume_plan(&record).unwrap());
    let account = work(&root);
    let moved = before.clone().with_account(Some(&account));
    // CONTROL: with the helper the plan is started as it stands.
    sup.link.token_helper = true;
    assert_eq!(sup.account_startable(moved.clone()), (moved.clone(), false));
    assert_eq!(events(&root), Vec::<String>::new());
    sup.link.token_helper = false;
    assert_eq!(sup.account_startable(moved), (before.clone(), true));
    assert_eq!(
        events(&root),
        ["account-move unmade to=ac-0123456789abcdef why=no-token-helper"]
    );
    // CONTROL: a plan that names no account needs no helper, the later
    // starts of a session on an account among them.
    let later = before.clone().on_account(Some(&account));
    assert_eq!(sup.account_startable(later.clone()), (later, false));
    assert_eq!(sup.account_startable(before.clone()), (before, false));
    assert_eq!(events(&root).len(), 1);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_start_onto_another_account_is_one_move_and_a_start_on_the_same_is_none() {
    let root = scratch("account-moved");
    let (mut sup, _) = launched(&root, &["--dangerously-skip-permissions"]);
    let account = work(&root);
    assert!(sup.account_moved(account.clone(), "account"));
    assert_eq!(sup.account.as_ref(), Some(&account));
    // A start on it again, whatever it was made for, moves nothing.
    assert!(!sup.account_moved(account.clone(), "crash"));
    assert_eq!(sup.account.as_ref(), Some(&account));
    // The default login is another account.
    let home = Account {
        key: HOME.to_string(),
        home: None,
    };
    assert!(sup.account_moved(home.clone(), "account-ahead"));
    assert_eq!(sup.account, Some(home));
    assert_eq!(
        events(&root),
        [
            "account-move to=ac-0123456789abcdef from=- cause=account login=directory",
            "account-move to=ac-fedcba9876543210 from=ac-0123456789abcdef cause=account-ahead login=default",
        ]
    );
    let _ = std::fs::remove_dir_all(&root);
}

/// A start made on an account is the session's move only once its vendor is
/// seen running: until then nothing is booked, no claim names the account,
/// and the same move asked again ends nothing.
#[test]
fn a_start_on_an_account_is_the_sessions_only_once_its_vendor_is_seen_running() {
    let (fake, mut sup, record, root) = super::link::tests::streaming(
        "claude-account-admitting",
        &["--dangerously-skip-permissions"],
    );
    let child = super::link::tests::named_child(&mut sup);
    let logins = |fake: &alab_wire::fake::FakeAterm| {
        fake.stream_records()
            .into_iter()
            .filter_map(|record| match record.body {
                alab_wire::Body::Claim {
                    child: named,
                    login,
                    ..
                } if named == Some(child) => Some(login),
                _ => None,
            })
            .collect::<Vec<_>>()
    };
    let account = work(&root);
    let before = pinned(&root, sup.resume_plan(&record).unwrap());
    sup.limit.floor_ms = 1;
    sup.child.spawned_ms = 5_000;
    sup.account_admitting(
        account.clone(),
        before.clone(),
        &Cause::Asked("account".to_string()),
    );
    // The rows before the new child are spent at once, and nothing else is
    // the account's yet.
    assert_eq!(sup.limit.floor_ms, 5_000);
    assert_eq!(sup.account, None);
    // No record and no terminal taken: however often it is looked at, and
    // however long ago the child started, nothing is seen.
    sup.child.spawned_ms = 1;
    assert!(!sup.account_seen(None));
    sup.link_restate();
    assert!(sup.admitting.is_some());
    // The same move asked meanwhile is the start under way.
    sup.link.accounts_granted = true;
    sup.link.token_helper = true;
    sup.restart_asked(&onto_account(&account));
    assert_eq!(sup.restart_deferred, None);
    // CONTROL: another account is another move, and waits its turn.
    let other = Account {
        key: HOME.to_string(),
        home: None,
    };
    sup.restart_asked(&onto_account(&other));
    assert_eq!(
        sup.restart_deferred.take().map(|deferred| deferred.restart),
        Some(onto_account(&other))
    );
    assert!(sup.admitting.is_some(), "no record: still not seen");
    // The vendor's record is read: the move is made and the claim says so.
    assert!(sup.account_seen(Some(&record)));
    sup.link_restate();
    assert_eq!(sup.account.as_ref(), Some(&account));
    assert_eq!(sup.admitting, None);
    let by = Instant::now() + Duration::from_secs(60);
    loop {
        let said = logins(&fake);
        if said.last() == Some(&Some(WORK.to_string())) {
            // Every claim before it named the child on no account.
            assert!(
                said[..said.len() - 1].iter().all(Option::is_none) && said.len() >= 3,
                "{said:?}"
            );
            break;
        }
        assert!(Instant::now() < by, "no claim named the account: {said:?}");
        std::thread::sleep(Duration::from_millis(10));
    }
    let said = events(&root);
    assert_eq!(
        said.iter()
            .filter(|line| line.starts_with("account-move ")
                || line.contains(" ignored already-on"))
            .collect::<Vec<_>>(),
        [
            "aterm restart cause=account to=ac-0123456789abcdef ignored already-on",
            "account-move to=ac-0123456789abcdef from=- cause=account login=directory",
        ],
        "{said:?}"
    );
    // A second sight books nothing twice.
    assert!(!sup.account_seen(Some(&record)));
    let _ = std::fs::remove_dir_all(&root);
}

/// Only the start a restart of aterm's admitted names the account to the
/// token helper. Every later start of the session keeps the account's login
/// directory and names none, so the helper is told `--auto`.
#[test]
fn a_later_start_keeps_the_accounts_login_directory_and_names_no_account() {
    let root = scratch("account-later");
    let (mut sup, record) = launched(&root, &["--dangerously-skip-permissions"]);
    let mut plan = pinned(&root, sup.resume_plan(&record).unwrap());
    for launch in [&mut plan.old, &mut plan.target] {
        launch
            .environment
            .push(("ANTHROPIC_API_KEY".into(), "placeholder".into()));
    }
    let account = work(&root);
    let directory = root.join("login-work").into_os_string();
    let has_credential = |launch: &vendor::Launch| {
        launch
            .environment
            .iter()
            .any(|(key, _)| key.as_os_str() == std::ffi::OsStr::new("ANTHROPIC_API_KEY"))
    };
    let later = plan.clone().on_account(Some(&account));
    for launch in [&later.old, &later.target] {
        assert_eq!(launch.account, None, "no account is named: `--auto`");
        assert_eq!(login_of(launch), Some(&directory));
        assert!(!has_credential(launch));
    }
    // CONTROL: the admitted move names it, on the same environment.
    let admitted = plan.clone().with_account(Some(&account));
    assert_eq!(admitted.target.account.as_ref(), Some(&account));
    assert_eq!(admitted.target.environment, later.target.environment);
    // CONTROL: a session on no account starts as it was pinned.
    assert_eq!(plan.clone().on_account(None), plan);
    assert!(has_credential(&plan.target));
    // A move made from a later start goes back, when it is not made, to the
    // account the session runs on, never to the launch's own.
    let home = Account {
        key: HOME.to_string(),
        home: None,
    };
    assert_eq!(later.clone().with_account(Some(&home)).unmade(), later);
    // The plans the supervisor itself pins once the session runs on the
    // account: a restart, a crash relaunch and a model move all start here.
    sup.child.build = plan.target.build.clone();
    sup.account = Some(account);
    let own = sup
        .same_build_plan(SID.to_string(), plan.resume.clone())
        .expect("a pinned plan");
    assert_eq!(own.target.account, None);
    assert_eq!(login_of(&own.target), Some(&directory));
    let _ = std::fs::remove_dir_all(&root);
}

/// A plan with an account on it goes back to exactly what was pinned: the
/// start a refused account move is made again from.
#[test]
fn a_plan_taken_off_its_account_is_the_plan_that_was_pinned() {
    let root = scratch("account-unmade-plan");
    let (sup, record) = launched(&root, &["--dangerously-skip-permissions"]);
    let mut plan = pinned(&root, sup.resume_plan(&record).unwrap());
    // The launch's own credential and login directory, which a move takes
    // out.
    for launch in [&mut plan.old, &mut plan.target] {
        launch
            .environment
            .push(("ANTHROPIC_API_KEY".into(), "placeholder".into()));
        launch
            .environment
            .push((LOGIN_ENV.into(), "/launch/own".into()));
    }
    let home = Account {
        key: HOME.to_string(),
        home: None,
    };
    // One account, then another over it: the plan before both comes back.
    let moved = plan
        .clone()
        .with_account(Some(&work(&root)))
        .with_account(Some(&home));
    assert_ne!(moved, plan);
    assert_eq!(moved.target.account.as_ref(), Some(&home));
    assert_eq!(login_of(&moved.target), None);
    assert_eq!(moved.unmade(), plan);
    // CONTROL: a plan that never carried one is left as it is.
    assert_eq!(plan.clone().unmade(), plan);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_move_off_a_limit_owes_the_resume_whoever_made_it_and_no_other_restart_does() {
    // The harness's own move off Fable's limit, and aterm's move onto another
    // account at a limit, with or without a model named.
    for cause in [
        Cause::Own {
            cause: limit::CAUSE_LIMIT.to_string(),
            model: "Opus".to_string(),
        },
        Cause::Asked("account".to_string()),
        Cause::Asked("account: opus".to_string()),
        Cause::Asked("account: the launch's own".to_string()),
    ] {
        assert!(owes_resume(&cause), "{cause:?}");
    }
    // Negative control: a move ahead of a limit, the return at Fable's
    // reset, a model restart aterm asked for and every other relaunch owe
    // none.
    for cause in [
        Cause::Asked("account-ahead".to_string()),
        Cause::Asked("account-ahead: opus".to_string()),
        Cause::Own {
            cause: limit::CAUSE_RESET.to_string(),
            model: "Fable".to_string(),
        },
        Cause::Asked("model: opus".to_string()),
        Cause::Asked("stall".to_string()),
        Cause::Asked("asked".to_string()),
        Cause::Wall(crate::walls::CAUSE_MEMORY.to_string()),
        Cause::Update,
        Cause::Crash,
    ] {
        assert!(!owes_resume(&cause), "{cause:?}");
    }
}

/// Fable's limit is an account's: a session off Fable that a start moves
/// onto another account, with or without a model, goes back at the next
/// point a return may be made, never at the old account's reset.
#[test]
fn an_account_move_off_fable_goes_back_at_once_and_never_at_the_old_accounts_reset() {
    let root = scratch("account-off-fable");
    let (mut sup, record) = launched(&root, &["--dangerously-skip-permissions"]);
    let far = now_ms() + 3 * 24 * 3_600 * 1_000;
    sup.limit.phase = limit::Phase::OffFable { back_at_ms: far };
    sup.limit.walled_returns = 2;
    sup.model = Some(limit::FALLBACK.to_string());
    let onto_fallback = || Some((limit::FALLBACK.to_string(), "account".to_string()));
    // CONTROL: a move onto the fallback again on the same account keeps the
    // reset it waits for.
    sup.limit_moves(onto_fallback(), SID);
    assert_eq!(sup.limit.phase, limit::Phase::OffFable { back_at_ms: far });
    assert_eq!(sup.limit.walled_returns, 2);
    // A start that joined an account move and a model move.
    let before = now_ms();
    let account = work(&root);
    sup.limit_moves(onto_fallback(), SID);
    assert!(started_on(
        &mut sup,
        &root,
        &account,
        "account: opus",
        &record
    ));
    let after = now_ms();
    assert_eq!(sup.account.as_ref(), Some(&account));
    let limit::Phase::OffFable { back_at_ms } = sup.limit.phase else {
        panic!("still off Fable: {:?}", sup.limit.phase);
    };
    assert!((before..=after).contains(&back_at_ms), "{back_at_ms}");
    assert_eq!(sup.limit.walled_returns, 0);
    // An account move with no model move goes back at once too.
    sup.limit.phase = limit::Phase::OffFable { back_at_ms: far };
    let home = Account {
        key: HOME.to_string(),
        home: None,
    };
    assert!(started_on(&mut sup, &root, &home, "account", &record));
    assert_eq!(sup.account, Some(home));
    assert!(matches!(
        sup.limit.phase,
        limit::Phase::OffFable { back_at_ms } if back_at_ms <= now_ms()
    ));
    // CONTROL: on the launch's terms, an account move moves no limit.
    sup.limit.phase = limit::Phase::OnLaunch { returned: false };
    assert!(started_on(&mut sup, &root, &account, "account", &record));
    assert_eq!(sup.account, Some(account));
    assert_eq!(sup.limit.phase, limit::Phase::OnLaunch { returned: false });
    let said = events(&root);
    let rebased: Vec<&String> = said
        .iter()
        .filter(|line| line.starts_with("limit-account "))
        .collect();
    assert_eq!(rebased.len(), 2, "{said:?}");
    assert!(
        rebased[0].starts_with("limit-account to=ac-0123456789abcdef back-at="),
        "{said:?}"
    );
    assert!(
        rebased[1].starts_with("limit-account to=ac-fedcba9876543210 back-at="),
        "{said:?}"
    );
    let _ = std::fs::remove_dir_all(&root);
}

/// A witness that reports `wall` while the floor lets its row count, as the
/// transcript does: a row stamped before the floor is spent.
struct Floored(limit::Wall);

impl limit::Source for Floored {
    fn look(&mut self, at: &limit::At<'_>) -> limit::Sight {
        limit::Sight {
            wall: (self.0.at_ms >= at.floor_ms).then(|| self.0.clone()),
            ..limit::Sight::default()
        }
    }
}

/// A witness that reports its wall to the first look only: the limit was
/// lifted before the next.
struct Lifted(Option<limit::Wall>);

impl limit::Source for Lifted {
    fn look(&mut self, _: &limit::At<'_>) -> limit::Sight {
        limit::Sight {
            wall: self.0.take(),
            ..limit::Sight::default()
        }
    }
}

/// aterm's move onto another account and onto the fallback, made at Fable's
/// limit in a session on its launch's terms, keeps the return to Fable: the
/// start books the model move, which reads the wall the child that ended
/// stood at, before the account move spends that child's rows. The new
/// account's Fable answers for itself, so the return is due at once.
#[test]
fn an_account_move_onto_the_fallback_at_fables_limit_keeps_the_return_to_fable() {
    let root = scratch("account-fallback-at-limit");
    let wall_at = now_ms();
    let wall = limit::Wall {
        at_ms: wall_at,
        resets_at_s: Some(wall_at / 1_000 + 3 * 24 * 3_600),
        bucket: Some(limit::FABLE_BUCKET.to_string()),
    };
    // A Fable session whose child stood at Fable's limit, and whose new
    // child started after it.
    let at_the_wall = || {
        let (mut sup, record) = launched(&root, &["--model", "fable"]);
        sup.limit_source = Box::new(Floored(wall.clone()));
        sup.child.spawned_ms = wall_at + 1_000;
        (sup, record)
    };
    let cause = format!("account: {}", limit::FALLBACK);
    let (mut sup, record) = at_the_wall();
    assert_eq!(sup.limit.phase, limit::Phase::OnLaunch { returned: false });
    let before = now_ms();
    sup.limit_moves(
        Some((limit::FALLBACK.to_string(), "account".to_string())),
        SID,
    );
    assert!(started_on(&mut sup, &root, &work(&root), &cause, &record));
    let after = now_ms();
    let limit::Phase::OffFable { back_at_ms } = sup.limit.phase else {
        panic!("no return to Fable is kept: {:?}", sup.limit.phase);
    };
    assert!((before..=after).contains(&back_at_ms), "{back_at_ms}");
    assert_eq!(sup.limit.floor_ms, wall_at + 1_000);
    assert_eq!(sup.account, Some(work(&root)));
    assert_eq!(sup.model.as_deref(), Some(limit::FALLBACK));
    let said = events(&root);
    let model_move = said
        .iter()
        .position(|line| line.starts_with("model-move to=claude-opus-5-5 from=fable "));
    let account_move = said
        .iter()
        .position(|line| line.starts_with("account-move to=ac-0123456789abcdef "));
    assert!(
        model_move.is_some() && model_move < account_move,
        "{said:?}"
    );
    // Negative control: booked the other way round, the account move spends
    // the wall's row first, and the move onto the fallback keeps no return.
    let (mut other, record) = at_the_wall();
    assert!(started_on(&mut other, &root, &work(&root), &cause, &record));
    other.model_moved(limit::FALLBACK.to_string(), "account");
    assert_eq!(
        other.limit.phase,
        limit::Phase::OnLaunch { returned: false }
    );
    let _ = std::fs::remove_dir_all(&root);
}
