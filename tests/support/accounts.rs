// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! aterm's account moves, end to end: the real harness in the tab of the
//! shared fake aterm (`alab_wire::fake`, contract 4 with its stream), its
//! vendor started through the host's token helper (`launch_host`), which is
//! what names an account to anyone.
//!
//! A claim made under contract 4 from a launch that reaches the token helper
//! takes `accounts`. aterm's `restart cause=account|account-ahead to=<key>
//! home=<dir|->` starts the child again on the same conversation, at a
//! moment the harness's own readings of the child allow: the token helper is
//! named the key once, with the account's login directory and no credential
//! variable, and every later start keeps the directory and names no key. The
//! claim names the account (`login=<key>`) only once the vendor is seen
//! running. A start the helper will not make is unmade, and the session
//! stays where it was. The harness asks aterm nothing about a limit: aterm is
//! the one decider.
//!
//! A change of route (the vault's token for a stored login, or back) is
//! aterm's too, and is told bare: `restart cause=route` names no account, so
//! it is a plain restart of the same conversation, which `relaunch` covers
//! and the token helper judges at its `--auto` start.
//!
//! Each wait for something that must happen is a one-minute hang detector.

use super::limit::{OPUS, append, epoch_ms, limit_row, transcript};
use super::*;
use alab_wire::fake::FakeAterm as Aterm;
use alab_wire::{ChildProcess, Config, Scope};

/// The key of the account aterm moves the session onto.
const KEY: &str = "ac-0123456789abcdef";

/// The key of another account.
const OTHER: &str = "ac-fedcba9876543210";

/// How long a thing that must happen may take before the scenario reads it
/// as a hang.
const HANG: Duration = Duration::from_secs(60);

/// A fake aterm that serves contract 4 and its stream, with this harness on
/// its roster and a quiet tab.
fn aterm(name: &str) -> Aterm {
    let config = Config::new(
        "claude-code-alab",
        "claude",
        &[Scope::Relaunch, Scope::Upgrade, Scope::Accounts],
        VERSION,
    )
    .expect("config");
    let fake = Aterm::for_harness(name, &config);
    fake.serve_contracts(&[4]);
    fake.serve_stream(true);
    fake
}

/// Start the harness in `fake`'s tab, its vendor behind the host's token
/// helper. The launch's own environment carries a credential variable and no
/// login directory.
fn start(s: &Scenario, pty: &Pty, fake: &Aterm) -> (std::process::Child, i32) {
    s.add_build(BUILD_1, Some("9.9.1"));
    s.set_current(BUILD_1);
    let host = launch_host::helper(s);
    let mut cmd = launch_host::command(
        s,
        &host,
        &["--alab-quiet-ms=600", "--alab-poll-ms=40", "run", "--"],
    );
    cmd.envs(fake.env())
        .env("ANTHROPIC_API_KEY", "placeholder-not-a-key")
        .env_remove("CLAUDE_SECURESTORAGE_CONFIG_DIR");
    spawn_on_pty(s, pty, cmd, None)
}

/// Wait for the fake vendor's start number `nth` (from 0) and return its pid.
fn started(s: &Scenario, nth: usize) -> i32 {
    assert!(
        wait_until(HANG, || s.starts().len() > nth),
        "start {nth} was never made{}",
        s.report()
    );
    s.starts()[nth].pid
}

/// [`started`], once that start wrote its session record.
fn recorded(s: &Scenario, nth: usize) -> i32 {
    let pid = started(s, nth);
    assert!(
        wait_until(HANG, || record_of(s, pid).exists()),
        "start {nth} never wrote its record{}",
        s.report()
    );
    pid
}

/// Wait for the standing claim to name the vendor `pid` as its child, and
/// return the account it names (`login=`).
fn claimed(s: &Scenario, fake: &Aterm, pid: i32) -> Option<String> {
    claimed_for(s, fake, pid, "relaunch,upgrade,accounts")
}

/// [`claimed`], for a claim that asks for the duties `scope`.
fn claimed_for(s: &Scenario, fake: &Aterm, pid: i32, scope: &str) -> Option<String> {
    let child = ChildProcess::of(u32::try_from(pid).expect("pid"))
        .expect("the vendor is running")
        .word();
    assert!(
        wait_until(HANG, || fake.child() == child),
        "no claim named the child {child:?}: {:?}{}",
        fake.claim_words(),
        s.report()
    );
    let words = fake.claim_words().expect("claim");
    assert_eq!(words.contract.as_deref(), Some("4"), "{words:?}");
    assert_eq!(words.scope.as_deref(), Some(scope), "{words:?}");
    words.login
}

/// Wait for the journal of `s` to hold `line`, whole.
fn journalled(s: &Scenario, line: &str) {
    assert!(
        wait_until(HANG, || s.journal().contains(line)),
        "the journal never said {line:?}{}",
        s.report()
    );
}

/// Put the fake vendor's own session record in `status`, and wait for it.
fn record_reads(s: &Scenario, pid: i32, status: &str) {
    s.put("status", status);
    let said = format!(r#""status":"{status}""#);
    assert!(
        wait_until(HANG, || std::fs::read_to_string(record_of(s, pid))
            .is_ok_and(|record| record.contains(&said))),
        "the record never read {status}{}",
        s.report()
    );
}

/// Wait for the standing claim to name the account `key`.
fn claims_login(s: &Scenario, fake: &Aterm, key: &str) {
    assert!(
        wait_until(HANG, || fake
            .claim_words()
            .and_then(|words| words.login)
            .as_deref()
            == Some(key)),
        "no claim named login={key}: {:?}{}",
        fake.claim_words(),
        s.report()
    );
}

/// Each vendor start's `login` line: its login directory and whether a
/// credential variable reached it.
fn logins(s: &Scenario) -> Vec<String> {
    s.log()
        .into_iter()
        .filter(|line| line.starts_with("login\t"))
        .collect()
}

fn login_line(dir: Option<&Path>, credential: bool) -> String {
    format!(
        "login\tCLAUDE_SECURESTORAGE_CONFIG_DIR={}\tANTHROPIC_API_KEY={}",
        dir.map_or_else(|| "-".to_string(), |dir| dir.display().to_string()),
        if credential { "set" } else { "-" }
    )
}

/// What the token helper was asked to start, one line a request: the word
/// (`--auto` or a key), the login directory, and whether a credential
/// variable reached it.
fn token_starts(s: &Scenario) -> Vec<String> {
    std::fs::read_to_string(s.fake.join("token-log"))
        .unwrap_or_default()
        .lines()
        .map(str::to_string)
        .collect()
}

fn token_line(word: &str, dir: Option<&Path>, credential: bool) -> String {
    format!(
        "{word}\t{}\t{}",
        dir.map_or_else(|| "-".to_string(), |dir| dir.display().to_string()),
        if credential { "set" } else { "-" }
    )
}

/// How many children were ended by SIGTERM.
fn terms(s: &Scenario) -> usize {
    s.log().iter().filter(|line| *line == "signal TERM").count()
}

/// The login directory of the account [`KEY`] in `s`.
fn login_dir(s: &Scenario) -> PathBuf {
    let dir = s.root.join("logins-work-dir");
    std::fs::create_dir_all(&dir).expect("login directory");
    dir
}

/// End the session from inside and expect the harness to end with it.
fn ended(s: &Scenario, pid: i32) {
    s.put("command", "exit:0");
    assert_eq!(
        wait_status(pid, HANG),
        Some(Status::Exited(0)),
        "{}",
        s.report()
    );
}

/// aterm moves the session onto the account [`KEY`], and says so twice, as it
/// may. The harness ends the child once and starts the same build on the same
/// conversation through the token helper, which is named the key, given the
/// account's login directory and no credential variable. It says so in one
/// journal line that names keys only, and its claim names the account once
/// the vendor runs. The same move asked again ends nothing. A crash
/// afterwards comes back on the account's login directory, and names the
/// helper no key: the account is the session's.
pub(super) fn aterm_an_account_move_resumes_on_its_login_and_every_later_start_keeps_it() {
    let s = Scenario::new("aterm-account-move");
    let fake = aterm("claude-account-move");
    let pty = Pty::open();
    let (_child, pid) = start(&s, &pty, &fake);
    let first = recorded(&s, 0);
    assert_eq!(claimed(&s, &fake, first), None, "{}", s.report());
    let login = login_dir(&s);
    fake.send_account_restart("account", KEY, Some(&login));
    fake.send_account_restart("account", KEY, Some(&login));
    let second = recorded(&s, 1);
    claims_login(&s, &fake, KEY);
    assert_eq!(
        claimed(&s, &fake, second).as_deref(),
        Some(KEY),
        "{}",
        s.report()
    );
    assert_eq!(s.starts()[1].build(), Some(BUILD_1), "{}", s.report());
    assert!(
        s.starts()[1]
            .args
            .windows(2)
            .any(|words| words == ["--resume", SID]),
        "{}",
        s.report()
    );
    assert_eq!(
        logins(&s),
        [login_line(None, true), login_line(Some(&login), false)],
        "{}",
        s.report()
    );
    assert_eq!(
        token_starts(&s),
        [
            token_line("--auto", None, true),
            token_line(KEY, Some(&login), false)
        ],
        "{}",
        s.report()
    );
    let journal = s.journal();
    assert!(
        journal.contains(&format!(
            " aterm restart cause=account to={KEY} SIGTERM pid={first} session={SID} \
             build={BUILD_1} argv="
        )) && journal.contains(&format!(
            " account-move to={KEY} from=- cause=account login=directory\n"
        )),
        "{}",
        s.report()
    );
    // Keys only: neither the login directory nor the credential's value.
    assert!(
        !journal.contains("logins-work-dir") && !journal.contains("placeholder-not-a-key"),
        "{}",
        s.report()
    );
    // The same move again: answered by the claim, and nothing is ended.
    let passed = |s: &Scenario| s.journal().matches(" ignored already-on\n").count();
    let before = passed(&s);
    fake.send_account_restart("account", KEY, Some(&login));
    assert!(
        wait_until(HANG, || passed(&s) > before),
        "the repeated move was not passed over{}",
        s.report()
    );
    assert_eq!(terms(&s), 1, "{}", s.report());
    assert_eq!(s.starts().len(), 2, "{}", s.report());
    assert_eq!(
        fake.claim_words().and_then(|words| words.login).as_deref(),
        Some(KEY),
        "{}",
        s.report()
    );
    // The account stays the session's: a crash is started again on its login
    // directory, and the helper is named no key for it.
    s.put("command", "crash:7");
    let third = recorded(&s, 2);
    assert!(
        wait_until(HANG, || logins(&s).len() >= 3
            && token_starts(&s).len() >= 3),
        "{}",
        s.report()
    );
    assert_eq!(
        logins(&s)[2],
        login_line(Some(&login), false),
        "{}",
        s.report()
    );
    assert_eq!(
        token_starts(&s)[2],
        token_line("--auto", Some(&login), false),
        "{}",
        s.report()
    );
    assert_eq!(
        claimed(&s, &fake, third).as_deref(),
        Some(KEY),
        "{}",
        s.report()
    );
    assert_eq!(
        s.journal().matches(" account-move ").count(),
        1,
        "a crash on the same account is no move{}",
        s.report()
    );
    ended(&s, pid);
}

/// The child stands at its account's five-hour limit and aterm, which reads
/// the tab as busy, names the account to resume it on. The harness's own
/// reading of the limit is what makes the moment safe: the child is ended and
/// started on the account. Its claim names the new child as soon as the
/// vendor's process stands, and names the account only once the vendor is
/// seen running: here by the terminal leaving the modes the harness saved,
/// with no session record yet.
pub(super) fn aterm_a_walled_child_is_resumed_on_the_account_aterm_names() {
    let s = Scenario::new("aterm-account-wall");
    let transcript = transcript(&s);
    let fake = aterm("claude-account-wall");
    let pty = Pty::open();
    let (_child, pid) = start(&s, &pty, &fake);
    let first = recorded(&s, 0);
    assert_eq!(claimed(&s, &fake, first), None, "{}", s.report());
    append(
        &transcript,
        &limit_row(epoch_ms(), "five_hour", epoch_ms() / 1_000 + 3_600, false),
    );
    fake.set_status("agent", Some("busy"));
    // The next vendor gets no further than its start: no record, and the
    // terminal as it found it.
    s.put("record-hold", "");
    s.put("raw-hold", "");
    let login = login_dir(&s);
    fake.send_account_restart("account", KEY, Some(&login));
    let second = started(&s, 1);
    assert_eq!(
        claimed(&s, &fake, second),
        None,
        "a start that is not seen running is no account's{}",
        s.report()
    );
    // Many looks later it is still no account's.
    std::thread::sleep(Duration::from_millis(600));
    assert_eq!(
        fake.claim_words().and_then(|words| words.login),
        None,
        "{}",
        s.report()
    );
    assert!(!s.journal().contains(" account-move "), "{}", s.report());
    assert!(!record_of(&s, second).exists(), "{}", s.report());
    // The vendor takes the terminal: it runs, and the account is the
    // session's.
    std::fs::remove_file(s.fake.join("raw-hold")).expect("release the terminal");
    claims_login(&s, &fake, KEY);
    assert_eq!(
        claimed(&s, &fake, second).as_deref(),
        Some(KEY),
        "{}",
        s.report()
    );
    assert!(
        s.journal().contains(&format!(
            " account-move to={KEY} from=- cause=account login=directory\n"
        )),
        "{}",
        s.report()
    );
    assert_eq!(terms(&s), 1, "{}", s.report());
    assert_eq!(
        logins(&s),
        [login_line(None, true), login_line(Some(&login), false)],
        "{}",
        s.report()
    );
    assert!(
        s.starts()[1]
            .args
            .windows(2)
            .any(|words| words == ["--resume", SID]),
        "{}",
        s.report()
    );
    std::fs::remove_file(s.fake.join("record-hold")).expect("release the record");
    recorded(&s, 1);
    ended(&s, pid);
}

/// aterm names an account the token helper will not start (the login
/// directory does not resolve to its key): the helper exits 75 having started
/// nothing. The harness reads that as the move not made, never as a crash:
/// one journal line, the conversation started again as it was before the move
/// (the launch's own login and credentials, no key named), no claim that ever
/// named the refused account, and the session goes on.
///
/// The same holds however long the helper takes to say so, past the grace a
/// build's failed start is read in: a start nobody saw run is not made, and
/// the session does not end with it. The limit the child stood at is then
/// still the session's: the same move asked again, to a tab aterm reads as
/// busy, is made by the harness's own reading of that limit.
///
/// And where the helper is gone when a move is asked, nothing is ended.
pub(super) fn aterm_an_account_the_token_helper_will_not_start_is_unmade_and_the_session_stays() {
    let s = Scenario::new("aterm-account-unmade");
    let transcript = transcript(&s);
    let fake = aterm("claude-account-unmade");
    let pty = Pty::open();
    let (_child, pid) = start(&s, &pty, &fake);
    let first = recorded(&s, 0);
    assert_eq!(claimed(&s, &fake, first), None, "{}", s.report());
    s.put("token-refuse", KEY);
    let login = login_dir(&s);
    fake.send_account_restart("account", KEY, Some(&login));
    let second = recorded(&s, 1);
    assert!(
        s.journal().contains(&format!(
            " account-move unmade to={KEY} why=not-started child=Exited(75)\n"
        )),
        "{}",
        s.report()
    );
    assert_eq!(
        token_starts(&s),
        [
            token_line("--auto", None, true),
            token_line(KEY, Some(&login), false),
            token_line("--auto", None, true),
        ],
        "{}",
        s.report()
    );
    assert_eq!(
        logins(&s),
        [login_line(None, true), login_line(None, true)],
        "{}",
        s.report()
    );
    assert!(
        s.starts()[1]
            .args
            .windows(2)
            .any(|words| words == ["--resume", SID]),
        "{}",
        s.report()
    );
    assert_eq!(
        claimed(&s, &fake, second),
        None,
        "the refused account was claimed{}",
        s.report()
    );
    assert!(
        fake.verbs()
            .iter()
            .all(|sent| !sent.contains(&format!("login={KEY}"))),
        "{:?}",
        fake.verbs()
    );
    let journal = s.journal();
    assert!(
        !journal.contains(&format!(" account-move to={KEY} from="))
            && !journal.contains(" crash child="),
        "{}",
        s.report()
    );
    assert_eq!(terms(&s), 1, "{}", s.report());
    assert!(
        wait_until(HANG, || pty.output().contains(
            "was not started on the account aterm named; resuming conversation"
        )),
        "the terminal was not told{}\n{}",
        s.report(),
        pty.output()
    );
    // The child stands at its account's limit, and the helper takes longer
    // to refuse than the grace a failed start of a build is read in.
    append(
        &transcript,
        &limit_row(epoch_ms(), "five_hour", epoch_ms() / 1_000 + 3_600, false),
    );
    fake.set_status("agent", Some("busy"));
    let slow = claude_code_alab::supervise::STARTUP_GRACE_MS + 1_000;
    s.put("token-refuse-after-ms", &slow.to_string());
    fake.send_account_restart("account", KEY, Some(&login));
    let third = recorded(&s, 2);
    let unmade = format!(" account-move unmade to={KEY} why=not-started child=Exited(75)\n");
    let journal = s.journal();
    assert_eq!(journal.matches(&unmade).count(), 2, "{}", s.report());
    assert!(
        !journal.contains(" crash") && !journal.contains(&format!(" account-move to={KEY} from=")),
        "{}",
        s.report()
    );
    // Each move so far is two starts: on the account, and back off it.
    let refused = journal_times(&s, " account-move unmade ");
    let started = journal_times(&s, " relaunch child=");
    assert!(
        refused[1] - started[2] > claude_code_alab::supervise::STARTUP_GRACE_MS,
        "the helper refused inside the grace: {refused:?} {started:?}{}",
        s.report()
    );
    assert_eq!(
        token_starts(&s)[3..],
        [
            token_line(KEY, Some(&login), false),
            token_line("--auto", None, true),
        ],
        "{}",
        s.report()
    );
    assert_eq!(claimed(&s, &fake, third), None, "{}", s.report());
    assert_eq!(terms(&s), 2, "{}", s.report());
    // The move asked again, the helper willing: aterm still reads the tab
    // as busy, so it is the limit the harness reads, the old account's and
    // still the session's, that lets the child be ended.
    std::fs::remove_file(s.fake.join("token-refuse")).expect("the helper serves the key");
    fake.send_account_restart("account", KEY, Some(&login));
    let fourth = recorded(&s, 3);
    claims_login(&s, &fake, KEY);
    assert_eq!(
        claimed(&s, &fake, fourth).as_deref(),
        Some(KEY),
        "{}",
        s.report()
    );
    assert!(
        s.journal().contains(&format!(
            " account-move to={KEY} from=- cause=account login=directory\n"
        )),
        "{}",
        s.report()
    );
    assert_eq!(terms(&s), 3, "{}", s.report());
    // The helper is gone: another account's move is refused by name, and no
    // child is ended for a start nothing could make.
    std::fs::remove_file(s.root.join("aterm-host")).expect("remove the helper");
    fake.send_account_restart("account", OTHER, None);
    journalled(
        &s,
        &format!(" aterm restart cause=account to={OTHER} ignored account-no-token-helper\n"),
    );
    assert_eq!(terms(&s), 3, "{}", s.report());
    assert_eq!(s.starts().len(), 4, "{}", s.report());
    ended(&s, pid);
}

/// aterm's restart names an account by a label, not by its key. A label
/// names an account only to the aterm that wrote it: the harness journals the
/// request as passed over and ends nothing. Nor is the move that names the
/// key made while the child's own record reads a turn in flight, though aterm
/// reads the tab as quiet: it waits for the whole of its offer and is over,
/// with nothing ended. CONTROL: the same move, sent ahead of any limit to a
/// child at rest, is made.
pub(super) fn aterm_an_account_restart_that_names_no_key_is_never_made() {
    let s = Scenario::new("aterm-account-word");
    let fake = aterm("claude-account-word");
    let pty = Pty::open();
    let (_child, pid) = start(&s, &pty, &fake);
    let first = recorded(&s, 0);
    assert_eq!(claimed(&s, &fake, first), None, "{}", s.report());
    let login = login_dir(&s);
    fake.send_account_restart("account", "work", Some(&login));
    assert!(
        wait_until(HANG, || s.journal().contains(
            " aterm restart cause=account to=work ignored account-word\n"
        )),
        "the label was not passed over{}",
        s.report()
    );
    assert_eq!(terms(&s), 0, "{}", s.report());
    assert_eq!(s.starts().len(), 1, "{}", s.report());
    assert_eq!(
        token_starts(&s),
        [token_line("--auto", None, true)],
        "{}",
        s.report()
    );
    // The child works: its record says so, and aterm's status does not.
    record_reads(&s, first, "busy");
    fake.send_account_restart("account-ahead", KEY, Some(&login));
    journalled(
        &s,
        &format!(" aterm restart cause=account-ahead to={KEY} deferred wait busy\n"),
    );
    journalled(
        &s,
        &format!(" aterm restart cause=account-ahead to={KEY} ignored account-offer-expired\n"),
    );
    assert_eq!(terms(&s), 0, "{}", s.report());
    assert_eq!(s.starts().len(), 1, "{}", s.report());
    assert!(!s.journal().contains(" SIGTERM "), "{}", s.report());
    let waited = journal_times(&s, " ignored account-offer-expired")[0]
        - journal_times(&s, " deferred wait busy")[0];
    assert!(
        waited + 1_000 >= claude_code_alab::supervise::ACCOUNT_OFFER_MS,
        "the move was over before its offer: {waited} ms{}",
        s.report()
    );
    record_reads(&s, first, "idle");
    fake.send_account_restart("account-ahead", KEY, Some(&login));
    recorded(&s, 1);
    claims_login(&s, &fake, KEY);
    assert_eq!(
        token_starts(&s),
        [
            token_line("--auto", None, true),
            token_line(KEY, Some(&login), false)
        ],
        "{}",
        s.report()
    );
    assert!(
        s.journal().contains(&format!(
            " account-move to={KEY} from=- cause=account-ahead login=directory\n"
        )),
        "{}",
        s.report()
    );
    ended(&s, pid);
}

/// The child stands at its account's five-hour limit. The harness asks aterm
/// nothing about it and ends nothing of its own: which account a session
/// runs on is aterm's to decide. That it read the limit is shown by the move
/// aterm then names, onto an account whose login is the default one: it is
/// made though aterm reads the tab as busy, which only the harness's own
/// reading of a limit allows.
pub(super) fn aterm_the_accounts_own_limit_asks_aterm_nothing() {
    let s = Scenario::new("aterm-account-no-ask");
    let transcript = transcript(&s);
    let fake = aterm("claude-account-no-ask");
    let pty = Pty::open();
    let (_child, pid) = start(&s, &pty, &fake);
    let first = recorded(&s, 0);
    assert_eq!(claimed(&s, &fake, first), None, "{}", s.report());
    append(
        &transcript,
        &limit_row(epoch_ms(), "five_hour", epoch_ms() / 1_000 + 3_600, false),
    );
    let asks_nothing = |s: &Scenario| {
        assert!(
            fake.verbs().iter().all(|sent| !sent.contains("advise"))
                && fake
                    .requests()
                    .iter()
                    .all(|(_, sent)| !sent.contains("advise")),
            "aterm was asked about the limit: {:?}",
            fake.requests()
        );
        assert!(!s.journal().contains("account-ask"), "{}", s.report());
    };
    // Many looks at the limit later.
    std::thread::sleep(Duration::from_millis(800));
    asks_nothing(&s);
    assert_eq!(terms(&s), 0, "{}", s.report());
    assert_eq!(s.starts().len(), 1, "{}", s.report());
    fake.set_status("agent", Some("busy"));
    fake.send_account_restart("account", KEY, None);
    recorded(&s, 1);
    claims_login(&s, &fake, KEY);
    assert!(
        s.journal().contains(&format!(
            " account-move to={KEY} from=- cause=account login=default\n"
        )),
        "{}",
        s.report()
    );
    assert_eq!(
        logins(&s),
        [login_line(None, true), login_line(None, false)],
        "{}",
        s.report()
    );
    assert_eq!(
        token_starts(&s),
        [
            token_line("--auto", None, true),
            token_line(KEY, None, false)
        ],
        "{}",
        s.report()
    );
    asks_nothing(&s);
    ended(&s, pid);
}

/// The person's cap leaves the claim without `accounts`. An account move
/// aterm sends all the same is its own engine's to make: the harness
/// journals it as passed over and ends nothing. CONTROL: a model move, which
/// `relaunch` covers, is made on the same claim.
///
/// A launch that reaches no token helper could start no account, and does
/// not ask for the word at all ([`a_launch_with_no_token_helper`]).
pub(super) fn aterm_an_account_move_without_the_grant_is_not_made() {
    a_launch_with_no_token_helper();
    let s = Scenario::new("aterm-account-ungranted");
    let fake = aterm("claude-account-ungranted");
    fake.set_cap(Some(&[Scope::Relaunch, Scope::Upgrade]));
    let pty = Pty::open();
    let (_child, pid) = start(&s, &pty, &fake);
    let first = recorded(&s, 0);
    assert_eq!(claimed(&s, &fake, first), None, "{}", s.report());
    let login = login_dir(&s);
    fake.send_account_restart("account", KEY, Some(&login));
    assert!(
        wait_until(HANG, || s.journal().contains(&format!(
            " aterm restart cause=account to={KEY} ignored account-assignment-unsupported\n"
        ))),
        "the ungranted move was not passed over{}",
        s.report()
    );
    assert_eq!(terms(&s), 0, "{}", s.report());
    assert_eq!(s.starts().len(), 1, "{}", s.report());
    fake.send_model_restart("model", OPUS);
    recorded(&s, 1);
    assert!(
        s.starts()[1]
            .args
            .windows(2)
            .any(|words| words == ["--model", OPUS]),
        "{}",
        s.report()
    );
    assert_eq!(
        token_starts(&s),
        [
            token_line("--auto", None, true),
            token_line("--auto", None, true)
        ],
        "{}",
        s.report()
    );
    assert!(!s.journal().contains(" account-move "), "{}", s.report());
    ended(&s, pid);
}

/// The harness is started in aterm's tab with no host named, as when it is
/// run by hand: its vendor is started directly, and nothing could start it on
/// an account. Its claim, the only answer aterm reads, asks for `relaunch`
/// and `upgrade` alone, so aterm commits no move for it; one sent all the
/// same is passed over and ends nothing.
fn a_launch_with_no_token_helper() {
    let s = Scenario::new("aterm-account-no-helper");
    let fake = aterm("claude-account-no-helper");
    let pty = Pty::open();
    s.add_build(BUILD_1, Some("9.9.1"));
    s.set_current(BUILD_1);
    let mut cmd = s.command(&["--alab-quiet-ms=600", "--alab-poll-ms=40", "run", "--"]);
    cmd.envs(fake.env());
    let (_child, pid) = spawn_on_pty(&s, &pty, cmd, None);
    let first = recorded(&s, 0);
    assert_eq!(
        claimed_for(&s, &fake, first, "relaunch,upgrade"),
        None,
        "{}",
        s.report()
    );
    assert_eq!(token_starts(&s), Vec::<String>::new(), "{}", s.report());
    let login = login_dir(&s);
    fake.send_account_restart("account", KEY, Some(&login));
    journalled(
        &s,
        &format!(" aterm restart cause=account to={KEY} ignored account-assignment-unsupported\n"),
    );
    assert_eq!(terms(&s), 0, "{}", s.report());
    assert_eq!(s.starts().len(), 1, "{}", s.report());
    ended(&s, pid);
}

/// aterm decided the session's route must change (the vault's token for a
/// stored login, or back) and tells the claim holder so bare: `restart
/// cause=route`, on the stream the account moves come on, with no `to=`, no
/// `home=` and no `model=`. To the harness that is a restart of the child as
/// it is: one SIGTERM, the same build on the same conversation, started
/// through the token helper, which is told `--auto` again and so judges the
/// route at that exec. The helper is named no key, the login directory and
/// the launch's credential variable are what they were, no account or model
/// move is booked, and the claim names the new child and no login.
///
/// It is made when it is read: the child's own record reads `busy`, which
/// holds an account move to its safe point and holds this restart to
/// nothing. aterm picked the moment before it told.
///
/// CONTROLS: the request needs no `accounts` grant
/// ([`a_route_restart_needs_no_accounts_grant`]) and no token helper
/// ([`a_route_restart_with_no_token_helper_starts_the_vendor_itself`]); it is
/// made the same where only the event carries it
/// ([`a_route_restart_told_as_an_event_alone_is_made_the_same`]); and one
/// that names an account is no request at all
/// ([`a_route_restart_that_names_an_account_is_not_read`]).
pub(super) fn aterm_a_route_restart_resumes_the_same_conversation_through_the_token_helper() {
    let s = Scenario::new("aterm-route-restart");
    let fake = aterm("claude-route-restart");
    let pty = Pty::open();
    let (_child, pid) = start(&s, &pty, &fake);
    let first = recorded(&s, 0);
    assert_eq!(claimed(&s, &fake, first), None, "{}", s.report());
    // The child is mid-turn by its own record: an account move would wait
    // (`deferred wait busy`), and this is made all the same.
    record_reads(&s, first, "busy");
    fake.send_restart("route");
    let second = route_restarted(&s, first);
    assert_eq!(
        claimed(&s, &fake, second),
        None,
        "a route move names no account{}",
        s.report()
    );
    assert_eq!(
        token_starts(&s),
        [
            token_line("--auto", None, true),
            token_line("--auto", None, true)
        ],
        "{}",
        s.report()
    );
    assert_eq!(
        logins(&s),
        [login_line(None, true), login_line(None, true)],
        "{}",
        s.report()
    );
    assert!(
        wait_until(HANG, || pty.output().contains(&format!(
            "restarted at aterm's request (route); resuming conversation {SID}"
        ))),
        "the terminal was not told{}\n{}",
        s.report(),
        pty.output()
    );
    assert!(
        fake.verbs().iter().all(|sent| !sent.contains("login=")),
        "{:?}",
        fake.verbs()
    );
    ended(&s, pid);
    a_route_restart_needs_no_accounts_grant();
    a_route_restart_with_no_token_helper_starts_the_vendor_itself();
    a_route_restart_told_as_an_event_alone_is_made_the_same();
    a_route_restart_that_names_an_account_is_not_read();
}

/// The child of `s` that ran as `first` was restarted once for aterm's
/// `restart cause=route`: the new child's pid. One SIGTERM, the same build,
/// `--resume` of the same conversation and no model named; one journal line
/// for the request and one relaunch, with nothing passed over, no crash and
/// no account or model move booked.
fn route_restarted(s: &Scenario, first: i32) -> i32 {
    let second = recorded(s, 1);
    assert_ne!(second, first, "{}", s.report());
    let starts = s.starts();
    assert_eq!(starts.len(), 2, "{}", s.report());
    assert_eq!(starts[1].build(), Some(BUILD_1), "{}", s.report());
    assert!(
        starts[1]
            .args
            .windows(2)
            .any(|words| words == ["--resume", SID])
            && starts[1].args.iter().all(|word| word != "--model"),
        "{}",
        s.report()
    );
    assert_eq!(terms(s), 1, "exactly one SIGTERM{}", s.report());
    let journal = s.journal();
    assert_eq!(
        journal
            .matches(&format!(
                " aterm restart cause=route SIGTERM pid={first} session={SID} build={BUILD_1} \
                 argv="
            ))
            .count(),
        1,
        "{}",
        s.report()
    );
    assert_eq!(
        journal.matches(" relaunch child=").count(),
        1,
        "{}",
        s.report()
    );
    assert!(
        !journal.contains(" restart cause=route ignored ")
            && !journal.contains(" restart cause=route deferred ")
            && !journal.contains(" crash")
            && !journal.contains(" account-move ")
            && !journal.contains(" model-move "),
        "{}",
        s.report()
    );
    second
}

/// The person's cap leaves the claim without `accounts`: an account move is
/// passed over there, and `restart cause=route` is made all the same, through
/// the token helper's `--auto`. A route move is no account move and falls
/// under `relaunch`.
fn a_route_restart_needs_no_accounts_grant() {
    let s = Scenario::new("aterm-route-ungranted");
    let fake = aterm("claude-route-ungranted");
    fake.set_cap(Some(&[Scope::Relaunch, Scope::Upgrade]));
    let pty = Pty::open();
    let (_child, pid) = start(&s, &pty, &fake);
    let first = recorded(&s, 0);
    assert_eq!(claimed(&s, &fake, first), None, "{}", s.report());
    // The grant is absent: the move that needs it is not made.
    let login = login_dir(&s);
    fake.send_account_restart("account", KEY, Some(&login));
    journalled(
        &s,
        &format!(" aterm restart cause=account to={KEY} ignored account-assignment-unsupported\n"),
    );
    assert_eq!(terms(&s), 0, "{}", s.report());
    assert_eq!(s.starts().len(), 1, "{}", s.report());
    fake.send_restart("route");
    let second = route_restarted(&s, first);
    assert_eq!(claimed(&s, &fake, second), None, "{}", s.report());
    assert_eq!(
        token_starts(&s),
        [
            token_line("--auto", None, true),
            token_line("--auto", None, true)
        ],
        "{}",
        s.report()
    );
    ended(&s, pid);
}

/// The harness is started in aterm's tab with no host named, so its claim
/// asks for `relaunch` and `upgrade` alone. aterm offers such a launch no
/// route move; one sent all the same is the plain restart every bare cause
/// is, and the vendor is started as that launch starts it, directly: no
/// token helper is asked anything.
fn a_route_restart_with_no_token_helper_starts_the_vendor_itself() {
    let s = Scenario::new("aterm-route-no-helper");
    let fake = aterm("claude-route-no-helper");
    let pty = Pty::open();
    s.add_build(BUILD_1, Some("9.9.1"));
    s.set_current(BUILD_1);
    let mut cmd = s.command(&["--alab-quiet-ms=600", "--alab-poll-ms=40", "run", "--"]);
    cmd.envs(fake.env());
    let (_child, pid) = spawn_on_pty(&s, &pty, cmd, None);
    let first = recorded(&s, 0);
    assert_eq!(
        claimed_for(&s, &fake, first, "relaunch,upgrade"),
        None,
        "{}",
        s.report()
    );
    fake.send_restart("route");
    let second = route_restarted(&s, first);
    assert_eq!(
        claimed_for(&s, &fake, second, "relaunch,upgrade"),
        None,
        "{}",
        s.report()
    );
    assert_eq!(token_starts(&s), Vec::<String>::new(), "{}", s.report());
    ended(&s, pid);
}

/// The scenario above is told on both carriers at once, as aterm tells it:
/// an `EVENT restart` and a `restart` record of the stream, which the link
/// makes one request of, and once the stream has brought a record of
/// aterm's the link passes its restart events over. Here the event is the
/// only carrier, as from an aterm whose stream brings none of its own
/// records and from one that serves no stream at all: the same one restart,
/// through the token helper's `--auto`, with no account named.
fn a_route_restart_told_as_an_event_alone_is_made_the_same() {
    for (name, stream) in [("route-event", true), ("route-no-stream", false)] {
        let s = Scenario::new(&format!("aterm-{name}"));
        let fake = aterm(&format!("claude-{name}"));
        if stream {
            fake.set_stream_own_records(false);
        } else {
            fake.serve_stream(false);
        }
        let pty = Pty::open();
        let (_child, pid) = start(&s, &pty, &fake);
        let first = recorded(&s, 0);
        assert_eq!(claimed(&s, &fake, first), None, "{}", s.report());
        assert!(
            wait_until(HANG, || fake.subscriptions() > 0
                && (!stream || fake.streaming())),
            "{name}: aterm was never listened to{}",
            s.report()
        );
        fake.send_restart("route");
        let second = route_restarted(&s, first);
        assert_eq!(claimed(&s, &fake, second), None, "{name}{}", s.report());
        assert_eq!(
            token_starts(&s),
            [
                token_line("--auto", None, true),
                token_line("--auto", None, true)
            ],
            "{name}{}",
            s.report()
        );
        // The carrier was the event: no stream brought a record of aterm's.
        assert_eq!(fake.stream_numbers().1, 0, "{name}{}", s.report());
        assert_eq!(fake.streaming(), stream, "{name}{}", s.report());
        ended(&s, pid);
    }
}

/// `to=` and `home=` belong to an account's cause alone. Under `route` they
/// make no bare restart and no account move, on either carrier: the shared
/// link reads no request in the line. A record of the stream is refused
/// (`to=malformed`); the event ends no child either, and the subscription
/// that carried it is given up as faulted. A direction word for a route move
/// would have to be one the link passes over, never one of these.
fn a_route_restart_that_names_an_account_is_not_read() {
    let s = Scenario::new("aterm-route-named");
    let fake = aterm("claude-route-named");
    let pty = Pty::open();
    let (_child, pid) = start(&s, &pty, &fake);
    let first = recorded(&s, 0);
    assert_eq!(claimed(&s, &fake, first), None, "{}", s.report());
    assert!(
        wait_until(HANG, || fake.subscriptions() > 0 && fake.streaming()),
        "aterm was never listened to{}",
        s.report()
    );
    // The fake writes no such record of its own: its encoder refuses one.
    let (_, sent) = fake.stream_numbers();
    let seq = sent + 1;
    assert!(
        fake.send_line(&format!(
            r#"{{"k":"restart","seq":{seq},"ms":0,"cause":"route","to":"{KEY}","home":"-"}}"#
        )),
        "{}",
        s.report()
    );
    journalled(
        &s,
        &format!(" aterm stream-unreadable k=restart in={seq} to=malformed\n"),
    );
    assert!(
        wait_until(HANG, || fake.refused()
            == [(seq, "to=malformed".to_string())]),
        "{:?}{}",
        fake.refused(),
        s.report()
    );
    fake.send_account_restart("route", KEY, None);
    journalled(&s, " aterm events-lost restart event has no single cause\n");
    ended(&s, pid);
    assert_eq!(terms(&s), 0, "{}", s.report());
    assert_eq!(s.starts().len(), 1, "{}", s.report());
    assert!(
        !s.journal().contains(" aterm restart cause=route"),
        "{}",
        s.report()
    );
}
