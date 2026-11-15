// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! A stand-in for aterm's control socket, and the proof that it and this crate's
//! client agree.
//!
//! [`FakeAterm`] lays what a harness inside an aterm tab finds: the rendezvous
//! directory's `graph/<sid>` entry, a socket in a directory short enough for a
//! Unix socket path, and the token beside it. It answers the five requests a
//! contract-3 client makes (`harness policy`, `native-act info`, `meta set
//! supervisor`, `meta unset supervisor`, `status`) from state a scenario changes
//! as it goes, and streams `subscribe @<sid> events` with aterm's numeric tags
//! (`sub 1 <sid>`, `EVENT 1 …`), which this crate's strict reader requires.
//!
//! It proves nothing itself. A claim is stored when the settable claim reply is
//! `OK` and no other holder stands, and `status` shows it proven while the
//! settable proven bit is set. As in aterm, a claim is bound to the connection
//! that made it and is dropped when that connection closes, and a halt
//! (`status hold=1`) reads in the policy as `effective=off why=hold`, with no
//! generation move and no event (`harness_policy_verb.rs`, `effective`).
//!
//! The wire text is aterm's at `d29d3887c4d`: the policy line of
//! `crates/aterm-gui/src/harness_policy_verb.rs`, the claim replies of
//! `crates/aterm-gui/src/control_session.rs` (`cmd_meta_supervisor_serving`,
//! `claim_reply`), the status words of `crates/aterm-gui/src/session_status.rs`
//! (`native_supervisor_fields`) and `session_timeline.rs` (`wire_fields`), the
//! info line of `crates/aterm-gui/src/native_act.rs` (`Request::Info`), the
//! subscription ACK of `crates/aterm-gui/src/control.rs`, and the restart event
//! of `crates/aterm-gui/src/restart_act.rs` (`record_restart`).
//!
//! The self-tests at the end drive the fake through `Locator`, `Connection` and
//! `EventReader` with no harness process, so a later scenario that fails is the
//! harness's failure and not a disagreement between the fake and the client.

use std::io::{BufRead as _, BufReader, Read as _, Write as _};
use std::net::Shutdown;
use std::os::fd::AsRawFd as _;
use std::os::unix::fs::DirBuilderExt as _;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use claude_code_alab::aterm::{
    self, Hold, Human, Policy, ReplyError, SafePoint, Scope, Status, judge_safe_point,
};
use claude_code_alab::aterm_wire::{Connection, EventReader, EventUpdate, Locator};

/// The token every connection must present.
const TOKEN: &str = "fake-aterm-token";
/// How long a self-test waits for the fake's threads before it fails.
const HANG: Duration = Duration::from_secs(20);
/// One request's deadline in the self-tests.
const TIMEOUT: Duration = Duration::from_secs(5);

/// A policy body (everything after `OK gen=<n> `) with contract 3 served, the
/// policy in effect and every switch a move or a crash relaunch needs on. The
/// unrelated keys are a sample of the dozens aterm sends.
pub const POLICY_ON: &str = "effective=on why=enabled contract=2,3 enabled=true autopilot=cruise \
    relaunch=true stall_term=true upgrade=true upgrade_ask=false human_grace_s=120 \
    questions=recommended";

/// What `native-act info` answers once aterm's roster carries this harness.
pub const INFO_ROSTERED: &str = "OK native-act state=info epoch=d1a48b482e4c6b391c4715137ce246b6 \
    native-act=1 handoff=blocked clock=monotonic-us \
    own-image=grok-alab:upgrade,muse-alab:upgrade,kimi-alab:upgrade,claude-code-alab:upgrade";

/// The current native host's canonical info grammar on kernels with child proof:
/// `native_act.rs`'s `Request::Info` inserts `harness_claim::child_capabilities()`
/// before the roster. The epoch is synthetic; the field order and capability words are not.
pub const INFO_CURRENT: &str = "OK native-act state=info epoch=d1a48b482e4c6b391c4715137ce246b6 \
    native-act=1 handoff=close clock=monotonic-us child-proof=1 child-start=1 \
    own-image=grok-alab:upgrade,muse-alab:upgrade,kimi-alab:upgrade,claude-code-alab:upgrade";

/// What `native-act info` answers today (aterm `d29d3887c4d`, read from a
/// running aterm on 2026-11-12): no `claude-code-alab` row. A new fake starts
/// here, so a scenario must opt in to the roster, as a real launch cannot.
pub const INFO_UNROSTERED: &str = "OK native-act state=info epoch=d1a48b482e4c6b391c4715137ce246b6 \
    native-act=1 handoff=blocked clock=monotonic-us \
    own-image=grok-alab:upgrade,muse-alab:upgrade,kimi-alab:upgrade";

/// The `status` fields of a quiet Claude Code session nobody is typing into, as
/// aterm's Claude reader reports them. The claim's own fields are added from
/// the fake's claim state.
pub const QUIET_STATUS: &[(&str, &str)] = &[
    ("hold", "0"),
    ("hand", "-"),
    ("program", "claude"),
    ("agent", "idle"),
    ("composer", "empty"),
    ("input", "clear"),
    ("human_ms", "-"),
    ("need", "-"),
];

/// A stored claim.
#[derive(Clone, Debug)]
struct Claim {
    holder: String,
    /// The connection it is bound to.
    connection: u64,
    /// Its `contract=` word, when it named one.
    contract: Option<String>,
    /// Its `scope=` words; every duty for a claim that named none.
    scope: String,
    version: Option<String>,
    vendor: Option<String>,
    child: Option<String>,
    child_generation: Option<u64>,
}

/// What a [`FakeAterm`] answers, and what it was asked.
#[derive(Debug)]
struct State {
    generation: u64,
    policy: String,
    /// A whole reply sent in place of the policy line, when set.
    policy_reply: Option<String>,
    info: String,
    claim_reply: String,
    start_reply: Option<String>,
    proven: bool,
    status: Vec<(String, String)>,
    claim: Option<Claim>,
    revision: u64,
    /// Every addressed request, without its `@<sid> `, in arrival order.
    verbs: Vec<String>,
    subscriptions: usize,
    /// A whole reply sent in place of the subscription's acknowledgement, when
    /// set; the connection is closed after it.
    subscribe_reply: Option<String>,
    /// Every `subscribe` request, answered or refused.
    subscribe_asks: usize,
    /// Every event line pushed so far.
    events: Vec<String>,
    connections: u64,
    /// Every accepted connection, so [`FakeAterm::restart`] can drop them.
    streams: Vec<UnixStream>,
    /// How long each addressed request waits before it is answered.
    delay: Duration,
    /// A path whose existence is recorded as each addressed request arrives.
    witness: Option<PathBuf>,
    /// For each of `verbs`, whether the witness path existed when it arrived.
    witnessed: Vec<bool>,
    body_log: Option<PathBuf>,
    body_counts: Vec<usize>,
}

impl State {
    fn release(&mut self) {
        if self.claim.take().is_some() {
            self.revision += 1;
        }
    }

    /// The policy body as aterm would answer it now: a halt on the session
    /// turns a policy in effect off, `why=hold`, from the same fact.
    fn policy_body(&self) -> String {
        let held = self
            .status
            .iter()
            .any(|(key, value)| key == "hold" && value == "1");
        if !held
            || !self
                .policy
                .split_ascii_whitespace()
                .any(|w| w == "effective=on")
        {
            return self.policy.clone();
        }
        self.policy
            .split_ascii_whitespace()
            .map(|word| match word.split_once('=') {
                Some(("effective", _)) => "effective=off",
                Some(("why", _)) => "why=hold",
                _ => word,
            })
            .collect::<Vec<_>>()
            .join(" ")
    }

    fn status_line(&self) -> String {
        let mut line = "OK schema=1 sid=1".to_string();
        for (key, value) in &self.status {
            line.push_str(&format!(" {key}={value}"));
        }
        let word = |value: Option<&String>| value.map_or("-", String::as_str).to_string();
        let claim = self.claim.as_ref();
        line.push_str(&format!(
            " supervisor_contract={} supervisor_scope={} supervisor_revision={} supervisor={} \
             supervisor_proven={} supervisor_version={} supervisor_vendor={} supervisor_child={} supervisor_child_start={}",
            word(claim.and_then(|claim| claim.contract.as_ref())),
            word(claim.map(|claim| &claim.scope)),
            self.revision,
            word(claim.map(|claim| &claim.holder)),
            u8::from(claim.is_some() && self.proven),
            word(claim.and_then(|claim| claim.version.as_ref())),
            word(claim.and_then(|claim| claim.vendor.as_ref())),
            word(claim.and_then(|claim| claim.child.as_ref())),
            claim.and_then(|claim|claim.child_generation).unwrap_or(0),
        ));
        line
    }

    /// `meta set supervisor <holder> <words…>` from connection `id`.
    fn claim(&mut self, id: u64, rest: &str) -> String {
        if self.claim_reply != "OK" {
            return self.claim_reply.clone();
        }
        let mut words = rest.split_ascii_whitespace();
        let Some(holder) = words.next() else {
            return "ERR usage: meta set supervisor <holder> [ttl=<ms>] | meta unset supervisor \
                    [holder=<holder>]"
                .to_string();
        };
        if let Some(other) = self.claim.as_ref().filter(|claim| claim.holder != holder) {
            return format!("ERR busy supervisor={}", other.holder);
        }
        let named = |key: &str| {
            rest.split_ascii_whitespace()
                .find_map(|word| word.strip_prefix(key))
                .map(str::to_string)
        };
        let child = named("child=");
        let child_generation = named("child-start=").and_then(|value| value.parse::<u64>().ok());
        if child.is_none()
            && child_generation.is_some()
            && let Some(reply) = &self.start_reply
        {
            return reply.clone();
        }
        self.claim = Some(Claim {
            holder: holder.to_string(),
            connection: id,
            contract: named("contract="),
            scope: named("scope=")
                .unwrap_or_else(|| "relaunch,upgrade,turn-end,approvals,questions,walls".into()),
            version: named("version="),
            vendor: named("vendor="),
            child,
            child_generation,
        });
        self.revision += 1;
        "OK".to_string()
    }

    /// One addressed request from connection `id`.
    fn answer(&mut self, id: u64, verb: &str) -> String {
        self.verbs.push(verb.to_string());
        self.witnessed
            .push(self.witness.as_ref().is_some_and(|path| path.exists()));
        self.body_counts.push(
            self.body_log
                .as_ref()
                .and_then(|path| std::fs::read_to_string(path).ok())
                .map_or(0, |text| {
                    text.lines()
                        .filter(|line| line.starts_with("start\t"))
                        .count()
                }),
        );
        if verb == "harness policy" {
            self.policy_reply
                .clone()
                .unwrap_or_else(|| format!("OK gen={} {}", self.generation, self.policy_body()))
        } else if verb == "native-act info" {
            self.info.clone()
        } else if verb == "status" {
            self.status_line()
        } else if let Some(rest) = verb.strip_prefix("meta set supervisor ") {
            self.claim(id, rest)
        } else if let Some(rest) = verb.strip_prefix("meta unset supervisor") {
            let named = rest.trim().strip_prefix("holder=");
            if self
                .claim
                .as_ref()
                .is_some_and(|claim| named.is_none_or(|holder| holder == claim.holder))
            {
                self.release();
            }
            "OK".to_string()
        } else {
            "ERR unknown verb".to_string()
        }
    }
}

/// A fake aterm serving one session. Dropping it closes every connection and
/// removes its socket directory.
pub struct FakeAterm {
    /// `XDG_RUNTIME_DIR` for the client: it holds `aterm/graph/<sid>`.
    pub xdg: PathBuf,
    /// The short directory holding the socket and its token.
    pub sock_dir: PathBuf,
    /// The session ID.
    pub sid: String,
    state: Arc<Mutex<State>>,
    stop: Arc<AtomicBool>,
}

fn lock(state: &Mutex<State>) -> MutexGuard<'_, State> {
    // A scenario that panicked while holding the state has already failed; the
    // fake keeps answering so its cleanup can run.
    state
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

impl FakeAterm {
    /// A fake aterm whose rendezvous directory is under `root` (a scenario's
    /// private directory). It starts with [`POLICY_ON`] at generation 1,
    /// [`INFO_UNROSTERED`], [`QUIET_STATUS`], a claim reply of `OK`, the proven
    /// bit set and no claim.
    pub fn new(root: &Path) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        let sid = format!("t{n:010x}");
        let xdg = root.join("xdg");
        let graph = xdg.join("aterm").join("graph");
        std::fs::create_dir_all(&graph).expect("graph directory");
        // A Unix socket path holds about a hundred bytes, and a scenario's own
        // directory is far longer than that.
        let parent = Path::new("/tmp/.ubd");
        std::fs::create_dir_all(parent).expect("socket parent");
        let sock_dir = parent.join(format!("fa-{}-{n}.noindex", std::process::id()));
        let _ = std::fs::remove_dir_all(&sock_dir);
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&sock_dir)
            .expect("socket directory");
        let sock = sock_dir.join("c.sock");
        std::fs::write(sock_dir.join("c.sock.token"), format!("{TOKEN}\n")).expect("token");
        std::fs::write(
            graph.join(&sid),
            format!("pid 1\nsock {}\n", sock.display()),
        )
        .expect("graph entry");
        let listener = UnixListener::bind(&sock).expect("bind the fake socket");
        listener.set_nonblocking(true).expect("nonblocking accept");
        let state = Arc::new(Mutex::new(State {
            generation: 1,
            policy: POLICY_ON.to_string(),
            policy_reply: None,
            info: INFO_UNROSTERED.to_string(),
            claim_reply: "OK".to_string(),
            start_reply: None,
            proven: true,
            status: QUIET_STATUS
                .iter()
                .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
                .collect(),
            claim: None,
            revision: 0,
            verbs: Vec::new(),
            subscriptions: 0,
            subscribe_reply: None,
            subscribe_asks: 0,
            events: Vec::new(),
            connections: 0,
            streams: Vec::new(),
            delay: Duration::ZERO,
            witness: None,
            witnessed: Vec::new(),
            body_log: None,
            body_counts: Vec::new(),
        }));
        let stop = Arc::new(AtomicBool::new(false));
        let (accept_state, accept_stop, accept_sid) =
            (Arc::clone(&state), Arc::clone(&stop), sid.clone());
        std::thread::spawn(move || {
            let mut id = 0_u64;
            while !accept_stop.load(Ordering::Relaxed) {
                let Ok((stream, _)) = listener.accept() else {
                    std::thread::sleep(Duration::from_millis(5));
                    continue;
                };
                id += 1;
                // Darwin hands the listener's nonblocking flag to accepted sockets.
                if stream.set_nonblocking(false).is_err() {
                    continue;
                }
                if let Ok(clone) = stream.try_clone() {
                    lock(&accept_state).streams.push(clone);
                }
                let (state, stop, sid) = (
                    Arc::clone(&accept_state),
                    Arc::clone(&accept_stop),
                    accept_sid.clone(),
                );
                std::thread::spawn(move || serve(stream, id, &state, &stop, &sid));
            }
        });
        Self {
            xdg,
            sock_dir,
            sid,
            state,
            stop,
        }
    }

    /// The environment that puts a harness in this fake's tab.
    pub fn env(&self) -> Vec<(&'static str, String)> {
        vec![
            ("TERM_PROGRAM", "aterm".to_string()),
            ("ATERM_PARENT_SESSION_ID", self.sid.clone()),
            ("XDG_RUNTIME_DIR", self.xdg.display().to_string()),
        ]
    }

    /// The locator a harness started with [`FakeAterm::env`] resolves.
    pub fn locator(&self) -> Locator {
        let env = self.env();
        let value = |key: &str| {
            env.iter()
                .find(|(name, _)| *name == key)
                .map(|(_, value)| value.clone())
                .expect("environment key")
        };
        Locator::from_values(
            &value("ATERM_PARENT_SESSION_ID"),
            Some(std::ffi::OsStr::new(&value("XDG_RUNTIME_DIR"))),
            None,
        )
        .expect("locator")
    }

    /// Answer `harness policy` with `OK gen=<n> <body>` at the next generation,
    /// and say so on every subscription (`EVENT 1 harness-policy gen=<n>`), as
    /// aterm does when its policy cell moves.
    pub fn set_policy(&self, body: &str) {
        let mut state = lock(&self.state);
        state.generation += 1;
        state.policy = body.to_string();
        state.policy_reply = None;
        let line = format!("EVENT 1 harness-policy gen={}", state.generation);
        state.events.push(line);
    }

    /// Answer `harness policy` with `OK gen=<n> <body>` at the same generation,
    /// and say nothing: what aterm does when a session's own Autopilot word,
    /// rather than its policy cell, turns the policy on or off for it.
    pub fn set_policy_quietly(&self, body: &str) {
        let mut state = lock(&self.state);
        state.policy = body.to_string();
        state.policy_reply = None;
    }

    /// Answer `harness policy` with exactly `reply`: a refusal, or a line an
    /// older aterm would send. No event is sent.
    pub fn set_policy_reply(&self, reply: &str) {
        lock(&self.state).policy_reply = Some(reply.to_string());
    }

    /// Answer `harness policy` from the policy body again, after
    /// [`FakeAterm::set_policy_reply`]. No event is sent.
    pub fn clear_policy_reply(&self) {
        lock(&self.state).policy_reply = None;
    }

    /// Answer `native-act info` with exactly `reply`.
    pub fn set_info(&self, reply: &str) {
        lock(&self.state).info = reply.to_string();
    }

    /// Answer `meta set supervisor …` with exactly `reply`. Only `OK` stores a
    /// claim; `ERR unproven: …` is what aterm answers a rostered holder it could
    /// not prove.
    pub fn set_claim_reply(&self, reply: &str) {
        lock(&self.state).claim_reply = reply.to_string();
    }

    /// Refuse only pre-spawn claims, leaving ordinary claims/reannouncements usable.
    pub fn set_start_reply(&self, reply: Option<&str>) {
        lock(&self.state).start_reply = reply.map(str::to_string);
    }

    /// Count vendor body receipts at each request, independently of the wire client.
    pub fn set_body_log(&self, path: &Path) {
        lock(&self.state).body_log = Some(path.to_path_buf());
    }

    /// Every addressed request paired with the vendor starts already observed then.
    pub fn verbs_with_body_counts(&self) -> Vec<(String, usize)> {
        let state = lock(&self.state);
        state
            .verbs
            .iter()
            .cloned()
            .zip(state.body_counts.iter().copied())
            .collect()
    }

    /// Whether a stored claim reads `supervisor_proven=1`.
    pub fn set_proven(&self, proven: bool) {
        lock(&self.state).proven = proven;
    }

    /// Set one `status` field, or drop it (`None`: an aterm that does not report
    /// it). The claim's own fields come from the claim state and cannot be set
    /// here.
    pub fn set_status(&self, key: &str, value: Option<&str>) {
        let mut state = lock(&self.state);
        state.status.retain(|(known, _)| known != key);
        if let Some(value) = value {
            state.status.push((key.to_string(), value.to_string()));
        }
    }

    /// Push `EVENT 1 <payload>` to every subscription.
    pub fn event(&self, payload: &str) {
        lock(&self.state).events.push(format!("EVENT 1 {payload}"));
    }

    /// Ask the claim holder to restart its child: `EVENT 1 restart cause=<cause>`,
    /// which aterm sends instead of signalling a harness whose proven claim
    /// takes `relaunch`.
    pub fn send_restart(&self, cause: &str) {
        self.event(&format!("restart cause={cause}"));
    }

    /// An aterm restart: every connection is dropped, and the claim with them.
    /// The socket keeps listening, as a restarted aterm's does.
    pub fn restart(&self) {
        let mut state = lock(&self.state);
        state.release();
        for stream in state.streams.drain(..) {
            let _ = stream.shutdown(Shutdown::Both);
        }
    }

    /// Every addressed request so far, without its `@<sid> `, in arrival order.
    pub fn verbs(&self) -> Vec<String> {
        lock(&self.state).verbs.clone()
    }

    /// Wait `delay` before answering each addressed request from now on: an
    /// aterm that is slow, so whatever a client does while it waits has time to
    /// show.
    pub fn set_reply_delay(&self, delay: Duration) {
        lock(&self.state).delay = delay;
    }

    /// Record, as each addressed request arrives from now on, whether `path`
    /// exists ([`FakeAterm::verbs_with_witness`]): the order of a request and
    /// something a client does outside the socket, such as starting a program
    /// that writes `path`.
    pub fn set_witness(&self, path: &Path) {
        lock(&self.state).witness = Some(path.to_path_buf());
    }

    /// [`FakeAterm::verbs`], each with whether the witness path existed when
    /// the request arrived (`false` while no witness was set).
    pub fn verbs_with_witness(&self) -> Vec<(String, bool)> {
        let state = lock(&self.state);
        state
            .verbs
            .iter()
            .cloned()
            .zip(state.witnessed.iter().copied())
            .collect()
    }

    /// The `status` line as it would be answered now, without recording a
    /// request.
    pub fn status_line(&self) -> String {
        lock(&self.state).status_line()
    }

    /// The standing claim's holder.
    pub fn holder(&self) -> Option<String> {
        lock(&self.state)
            .claim
            .as_ref()
            .map(|claim| claim.holder.clone())
    }

    /// How many connections authenticated.
    pub fn connections(&self) -> u64 {
        lock(&self.state).connections
    }

    /// Answer `subscribe` with exactly `reply` (`None`: the acknowledgement
    /// aterm sends) and close that connection.
    pub fn set_subscribe_reply(&self, reply: Option<&str>) {
        lock(&self.state).subscribe_reply = reply.map(str::to_string);
    }

    /// How many `subscribe` requests arrived, refused ones included.
    pub fn subscribe_asks(&self) -> usize {
        lock(&self.state).subscribe_asks
    }

    /// How many subscriptions were accepted.
    pub fn subscriptions(&self) -> usize {
        lock(&self.state).subscriptions
    }
}

impl Drop for FakeAterm {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        for stream in lock(&self.state).streams.drain(..) {
            let _ = stream.shutdown(Shutdown::Both);
        }
        let _ = std::fs::remove_dir_all(&self.sock_dir);
    }
}

/// Stream the events pushed after this subscription began, until the client
/// goes, the fake stops, or a restart drops the connection.
fn stream_events(mut out: UnixStream, state: &Mutex<State>, stop: &AtomicBool) {
    let mut sent = lock(state).events.len();
    // The read below is the pause between looks and the detector of a closed peer.
    if out
        .set_read_timeout(Some(Duration::from_millis(5)))
        .is_err()
    {
        return;
    }
    while !stop.load(Ordering::Relaxed) {
        let fresh: Vec<String> = {
            let state = lock(state);
            state.events.get(sent..).unwrap_or_default().to_vec()
        };
        for line in &fresh {
            if out.write_all(format!("{line}\n").as_bytes()).is_err() {
                return;
            }
        }
        sent += fresh.len();
        let mut probe = [0_u8; 1];
        match out.read(&mut probe) {
            Ok(0) => return,
            Ok(_) => {}
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock
                        | std::io::ErrorKind::TimedOut
                        | std::io::ErrorKind::Interrupted
                ) => {}
            Err(_) => return,
        }
    }
}

fn serve(stream: UnixStream, id: u64, state: &Mutex<State>, stop: &AtomicBool, sid: &str) {
    let Ok(mut out) = stream.try_clone() else {
        return;
    };
    let mut authenticated = false;
    for line in BufReader::new(stream).lines() {
        let Ok(line) = line else { break };
        if !authenticated {
            // `AUTH <token>` has no reply of its own; the next request's reply is
            // the first line a client reads.
            if line == format!("AUTH {TOKEN}") {
                authenticated = true;
                lock(state).connections += 1;
                continue;
            }
            let _ = out.write_all(b"ERR auth\n");
            break;
        }
        if line.starts_with("subscribe ") {
            if line != format!("subscribe @{sid} events") {
                let _ = out.write_all(b"ERR usage: subscribe\n");
                break;
            }
            let refusal = {
                let mut state = lock(state);
                state.subscribe_asks += 1;
                state.subscribe_reply.clone()
            };
            if let Some(reply) = refusal {
                let _ = out.write_all(format!("{reply}\n").as_bytes());
                break;
            }
            lock(state).subscriptions += 1;
            if out
                .write_all(
                    format!("OK subscribe 1\nsub 1 {sid} role=- attention=- user_title=-\n")
                        .as_bytes(),
                )
                .is_ok()
            {
                stream_events(out, state, stop);
            }
            break;
        }
        let reply = if line == "version" {
            "OK version=fake-aterm".to_string()
        } else if let Some(verb) = line.strip_prefix(&format!("@{sid} ")) {
            // The request is recorded as it arrives and answered after the delay.
            let delay = lock(state).delay;
            let reply = lock(state).answer(id, verb);
            if !delay.is_zero() {
                std::thread::sleep(delay);
            }
            reply
        } else {
            "ERR no such session".to_string()
        };
        if out.write_all(format!("{reply}\n").as_bytes()).is_err() {
            break;
        }
    }
    // The connection is gone, and a claim bound to it goes with it.
    let mut state = lock(state);
    if state
        .claim
        .as_ref()
        .is_some_and(|claim| claim.connection == id)
    {
        state.release();
    }
}

// ───────────────────────── the fake and the client agree ─────────────────────────

/// A private root for one self-test's rendezvous directory.
fn root(name: &str) -> PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let root = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!(
        ".ubd/fake-aterm.noindex/{name}-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).expect("self-test root");
    root
}

fn connect(fake: &FakeAterm) -> Connection {
    let socket = fake.locator().socket().expect("the graph names the socket");
    Connection::connect(&socket, TIMEOUT).expect("connect and authenticate")
}

fn eventually(what: &str, mut holds: impl FnMut() -> bool) {
    let until = Instant::now() + HANG;
    while !holds() {
        assert!(Instant::now() < until, "hang detector: {what}");
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// Drain `events` until `wanted` says an update is the one awaited, waiting on
/// the reader's own descriptor as a supervisor's loop will.
fn await_update(
    what: &str,
    events: &mut EventReader,
    wanted: impl Fn(&EventUpdate) -> bool,
) -> EventUpdate {
    let until = Instant::now() + HANG;
    loop {
        let mut probe = libc::pollfd {
            fd: events.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: probe is one initialized poll record for the reader's open descriptor.
        let _ = unsafe { libc::poll(&raw mut probe, 1, 50) };
        let update = events.drain();
        if wanted(&update) {
            return update;
        }
        assert!(
            !update.ended,
            "the event stream ended awaiting {what}: {update:?}"
        );
        assert!(Instant::now() < until, "hang detector: {what}");
    }
}

fn ask(connection: &mut Connection, request: Option<String>) -> String {
    connection
        .request_line(&request.expect("a session ID the client accepts"))
        .expect("a reply")
}

fn me() -> (u32, String) {
    let pid = std::process::id();
    (pid, aterm::holder(pid))
}

fn claim(fake: &FakeAterm, connection: &mut Connection, scopes: &[Scope]) -> String {
    let (pid, _) = me();
    ask(
        connection,
        aterm::claim_verb(&fake.sid, pid, scopes, "0.1.0", Some("2.1.311")),
    )
}

fn status(fake: &FakeAterm, connection: &mut Connection) -> Status {
    Status::parse(&ask(connection, aterm::status_verb(&fake.sid))).expect("a schema-1 status")
}

fn policy(fake: &FakeAterm, connection: &mut Connection) -> Policy {
    Policy::parse(&ask(connection, aterm::policy_verb(&fake.sid))).expect("a policy")
}

/// The client finds the socket through the graph, reads the token beside it and
/// authenticates; a wrong token and an unknown session are refused.
pub fn the_fake_aterm_is_found_and_authenticates_this_crates_client() {
    let fake = FakeAterm::new(&root("auth"));
    assert_eq!(fake.locator().sid(), fake.sid);
    assert_eq!(
        fake.locator().socket().expect("socket"),
        fake.sock_dir.join("c.sock")
    );
    let mut connection = connect(&fake);
    assert_eq!(connection.version(), "version=fake-aterm");
    assert_eq!(fake.connections(), 1);
    // A request must name this session, and an unknown verb is an ordinary error.
    assert_eq!(
        connection
            .request_line("@tzzzzzzzzzz status")
            .expect("reply"),
        "ERR no such session"
    );
    assert_eq!(
        connection
            .request_line(&format!("@{} look", fake.sid))
            .expect("reply"),
        "ERR unknown verb"
    );
    assert!(!connection.is_poisoned());
    assert_eq!(fake.verbs(), ["look"]);

    let token = fake.sock_dir.join("c.sock.token");
    std::fs::write(&token, "another-token\n").expect("rewrite the token");
    let socket = fake.locator().socket().expect("socket");
    let refused = Connection::connect(&socket, TIMEOUT).expect_err("a wrong token");
    assert_eq!(refused.kind(), std::io::ErrorKind::PermissionDenied);
    assert_eq!(fake.connections(), 1);
    std::fs::write(&token, format!("{TOKEN}\n")).expect("restore the token");
    drop(connect(&fake));
    assert_eq!(fake.connections(), 2);
}

/// `harness policy` parses, moves with its generation, and a refusal or an older
/// aterm's line is an error and never a policy.
pub fn the_fake_aterms_policy_reads_as_a_policy() {
    let fake = FakeAterm::new(&root("policy"));
    let mut connection = connect(&fake);
    let on = policy(&fake, &mut connection);
    assert_eq!(on.generation, 1);
    assert!(on.admits_claim() && on.relaunch && on.upgrade && !on.upgrade_ask);
    assert_eq!(on.human_grace, Duration::from_secs(120));

    fake.set_policy(&POLICY_ON.replace("effective=on why=enabled", "effective=off why=hold"));
    let off = policy(&fake, &mut connection);
    assert_eq!(off.generation, 2);
    assert!(off.serves_contract() && !off.effective && !off.admits_claim());

    // A halt is the policy off, `why=hold`, at the same generation.
    fake.set_policy(POLICY_ON);
    fake.set_status("hold", Some("1"));
    let held = policy(&fake, &mut connection);
    assert_eq!(held.generation, 3);
    assert!(!held.effective && held.why.as_deref() == Some("hold"));
    assert!(!held.off_by_configuration());
    fake.set_status("hold", Some("0"));
    assert!(policy(&fake, &mut connection).effective);

    // An aterm that predates contract 3 says nothing of contracts.
    fake.set_policy_reply("OK gen=12 effective=on why=config continue=on land=off questions=on");
    let older = ask(&mut connection, aterm::policy_verb(&fake.sid));
    assert_eq!(Policy::parse(&older), Err(ReplyError::Missing("contract")));
    assert_eq!(aterm::advertised_contract(&older), None);
    fake.set_policy_reply("ERR denied");
    let denied = ask(&mut connection, aterm::policy_verb(&fake.sid));
    assert_eq!(
        Policy::parse(&denied).unwrap_err().to_string(),
        "reply=ERR:denied"
    );
    // An explicit ERR is a reply: the connection stays usable.
    assert!(!connection.is_poisoned());
    assert_eq!(fake.verbs(), ["harness policy"; 6]);
}

/// `native-act info` carries a bare word, reads through the roster guard, and
/// today's reply names no row for this harness.
pub fn the_fake_aterms_info_reply_reads_through_the_roster_guard() {
    let fake = FakeAterm::new(&root("info"));
    let mut connection = connect(&fake);
    let today = ask(&mut connection, aterm::info_verb(&fake.sid));
    assert_eq!(today, INFO_UNROSTERED);
    assert!(!aterm::rostered(&today));
    // The strict reader refuses the bare `native-act`, so this reply must never
    // go through `request_fields`, which would poison the connection.
    assert!(claude_code_alab::aterm_wire::Fields::parse(&today).is_err());

    fake.set_info(INFO_ROSTERED);
    assert!(aterm::rostered(&ask(
        &mut connection,
        aterm::info_verb(&fake.sid)
    )));
    fake.set_info(INFO_CURRENT);
    let current = ask(&mut connection, aterm::info_verb(&fake.sid));
    assert_eq!(current, INFO_CURRENT);
    assert!(aterm::rostered(&current));
    assert!(aterm::child_proof_supported(&current));
    assert!(aterm::child_start_supported(&current));
    fake.set_info("ERR usage: native-act");
    assert!(!aterm::rostered(&ask(
        &mut connection,
        aterm::info_verb(&fake.sid)
    )));
    assert!(!connection.is_poisoned());
    // The guard is a read: nothing was claimed by asking.
    assert_eq!(fake.holder(), None);
    assert_eq!(fake.verbs(), ["native-act info"; 4]);
}

/// A claim is stored, `status` reads it back proven with its scope, the judge
/// calls a quiet session safe, and a release clears it.
pub fn a_claim_on_the_fake_aterm_reads_back_proven_in_status() {
    let fake = FakeAterm::new(&root("claim"));
    let mut connection = connect(&fake);
    let (pid, holder) = me();
    let before = status(&fake, &mut connection);
    assert_eq!(before.supervisor, None);
    assert!(!before.held_by(&holder) && before.supervisor_scope.is_empty());
    assert_eq!(before.supervisor_revision, Some(0));

    assert_eq!(claim(&fake, &mut connection, &[Scope::Upgrade]), "OK");
    assert_eq!(fake.holder().as_deref(), Some(holder.as_str()));
    let held = status(&fake, &mut connection);
    assert!(held.held_by(&holder));
    assert!(held.takes(Scope::Upgrade) && !held.takes(Scope::Relaunch));
    assert_eq!(held.supervisor_revision, Some(1));
    assert_eq!(held.human, Human::Never);
    let on = policy(&fake, &mut connection);
    assert_eq!(
        judge_safe_point(&on, &held, &holder, Duration::ZERO),
        SafePoint::Safe
    );

    // One field at a time holds, by name.
    fake.set_status("composer", Some("draft"));
    let drafting = status(&fake, &mut connection);
    assert_eq!(
        judge_safe_point(&on, &drafting, &holder, Duration::ZERO),
        SafePoint::Hold(Hold {
            field: "composer",
            value: "draft".to_string(),
            grace_left: None,
        })
    );
    fake.set_status("composer", None);
    let SafePoint::Hold(hold) = judge_safe_point(
        &on,
        &status(&fake, &mut connection),
        &holder,
        Duration::ZERO,
    ) else {
        panic!("a status without composer= holds");
    };
    assert_eq!(hold.to_string(), "wait aterm composer=absent");
    fake.set_status("composer", Some("empty"));

    // A stored claim aterm did not prove is not this harness's to act under.
    fake.set_proven(false);
    let unproven = status(&fake, &mut connection);
    assert!(!unproven.held_by(&holder));
    assert_eq!(unproven.supervisor.as_deref(), Some(holder.as_str()));
    let SafePoint::Hold(hold) = judge_safe_point(&on, &unproven, &holder, Duration::ZERO) else {
        panic!("an unproven claim holds");
    };
    assert_eq!(hold.to_string(), "wait aterm supervisor_proven=0");
    fake.set_proven(true);

    // Claiming again on the same connection widens the scope in place.
    assert_eq!(
        claim(&fake, &mut connection, &[Scope::Upgrade, Scope::Relaunch]),
        "OK"
    );
    let wide = status(&fake, &mut connection);
    assert!(wide.held_by(&holder) && wide.takes(Scope::Relaunch) && wide.takes(Scope::Upgrade));

    assert_eq!(
        ask(&mut connection, aterm::release_verb(&fake.sid, pid)),
        "OK"
    );
    assert_eq!(fake.holder(), None);
    assert_eq!(status(&fake, &mut connection).supervisor, None);

    let verbs = fake.verbs();
    assert_eq!(verbs.first().map(String::as_str), Some("status"));
    assert_eq!(
        verbs.get(1).map(String::as_str),
        Some(
            format!(
                "meta set supervisor {holder} contract=3 scope=upgrade version=0.1.0 \
                 vendor=2.1.311"
            )
            .as_str()
        )
    );
    assert!(verbs.contains(&format!(
        "meta set supervisor {holder} contract=3 scope=relaunch,upgrade version=0.1.0 \
         vendor=2.1.311"
    )));
    assert_eq!(
        verbs.iter().rev().nth(1).map(String::as_str),
        Some(format!("meta unset supervisor holder={holder}").as_str())
    );
}

/// The settable claim reply refuses without storing, another holder's standing
/// claim answers `ERR busy`, and a release naming someone else changes nothing.
pub fn a_refused_or_contended_claim_on_the_fake_aterm_stores_nothing() {
    let fake = FakeAterm::new(&root("refused"));
    let mut connection = connect(&fake);
    let (pid, holder) = me();
    let unproven = "ERR unproven: a contract 3 claim needs its own direct connection, the \
                    session's foreground group and a build of its program from atpkg";
    fake.set_claim_reply(unproven);
    assert_eq!(claim(&fake, &mut connection, &[Scope::Upgrade]), unproven);
    assert_eq!(fake.holder(), None);
    assert_eq!(status(&fake, &mut connection).supervisor, None);
    fake.set_claim_reply("OK");

    // Another program holds the session from its own connection.
    let mut other = connect(&fake);
    assert_eq!(
        other
            .request_line(&format!(
                "@{} meta set supervisor someone-else@77",
                fake.sid
            ))
            .expect("reply"),
        "OK"
    );
    let foreign = status(&fake, &mut connection);
    assert_eq!(foreign.supervisor.as_deref(), Some("someone-else@77"));
    // A claim without `contract=` takes every duty.
    assert!(foreign.takes(Scope::Upgrade) && foreign.takes(Scope::Relaunch));
    assert!(!foreign.held_by(&holder));
    assert_eq!(
        claim(&fake, &mut connection, &[Scope::Upgrade]),
        "ERR busy supervisor=someone-else@77"
    );
    // Releasing by name leaves the other holder's claim alone.
    assert_eq!(
        ask(&mut connection, aterm::release_verb(&fake.sid, pid)),
        "OK"
    );
    assert_eq!(fake.holder().as_deref(), Some("someone-else@77"));
    drop(other);
    eventually("the other holder's connection closes", || {
        fake.holder().is_none()
    });
    assert_eq!(claim(&fake, &mut connection, &[Scope::Upgrade]), "OK");
    assert!(status(&fake, &mut connection).held_by(&holder));
}

/// A claim is bound to its connection: closing it releases the claim, and
/// another connection reads the session unclaimed.
pub fn closing_the_claims_connection_releases_it_on_the_fake_aterm() {
    let fake = FakeAterm::new(&root("drop"));
    let (_, holder) = me();
    let mut claimant = connect(&fake);
    let mut observer = connect(&fake);
    assert_eq!(claim(&fake, &mut claimant, &[Scope::Upgrade]), "OK");
    let seen = status(&fake, &mut observer);
    assert!(seen.held_by(&holder));
    let revision = seen.supervisor_revision.expect("revision");

    // The observer going away releases nothing: the claim is the claimant's
    // connection's, not the last one's.
    drop(observer);
    assert_eq!(fake.connections(), 2);
    assert!(status(&fake, &mut claimant).held_by(&holder));

    drop(claimant);
    eventually("the claim goes with its connection", || {
        fake.holder().is_none()
    });
    let mut later = connect(&fake);
    let after = status(&fake, &mut later);
    assert_eq!(after.supervisor, None);
    assert!(!after.supervisor_proven && after.supervisor_scope.is_empty());
    assert!(after.supervisor_revision.expect("revision") > revision);
}

/// A subscription is acknowledged with a numeric tag this crate's strict reader
/// accepts, and events arrive on a descriptor that polls readable.
pub fn events_from_the_fake_aterm_reach_this_crates_reader() {
    let fake = FakeAterm::new(&root("events"));
    let socket = fake.locator().socket().expect("socket");
    // An event from before the subscription is not replayed.
    fake.event("human");
    let mut events = EventReader::connect(&socket, &fake.sid, TIMEOUT).expect("subscribe");
    eventually("the subscription is counted", || fake.subscriptions() == 1);
    assert!(!events.drain().requires_refresh());

    fake.event("status");
    let update = await_update("a status event", &mut events, |update| update.activity);
    assert!(!update.policy_changed && update.restart.is_none() && update.fault.is_none());

    fake.set_policy(POLICY_ON);
    let update = await_update("a policy event", &mut events, |update| {
        update.policy_changed
    });
    assert!(update.requires_refresh() && !update.ended);
    // The event says only that the policy moved; the policy is read again.
    let mut connection = connect(&fake);
    assert_eq!(policy(&fake, &mut connection).generation, 2);

    // A kind the reader has no use for is read past.
    fake.event("meta field=title value=-");
    fake.event("turn 7 submitted=1 status=done dur_ms=12 by=-");
    let update = await_update("a turn event", &mut events, |update| update.activity);
    assert!(update.fault.is_none() && !update.ended);

    // A subscription for another session is refused outright.
    assert!(EventReader::connect(&socket, "tzzzzzzzzzz", TIMEOUT).is_err());
    assert_eq!(fake.subscriptions(), 1);
}

/// The restart sender's line parses as a restart with its cause.
pub fn a_restart_event_from_the_fake_aterm_parses() {
    let fake = FakeAterm::new(&root("restart-event"));
    let socket = fake.locator().socket().expect("socket");
    let mut events = EventReader::connect(&socket, &fake.sid, TIMEOUT).expect("subscribe");
    fake.send_restart("asked");
    let update = await_update("a restart event", &mut events, |update| {
        update.restart.is_some()
    });
    assert_eq!(
        update.restart.map(|restart| restart.cause).as_deref(),
        Some("asked")
    );
    assert!(update.fault.is_none() && !update.ended && !update.policy_changed);
    fake.send_restart("stall");
    let update = await_update("a second restart event", &mut events, |update| {
        update.restart.is_some()
    });
    assert_eq!(
        update.restart.map(|restart| restart.cause).as_deref(),
        Some("stall")
    );
}

/// An aterm restart drops every connection and the claim; the client sees its
/// request connection fail and its event stream end, finds the socket again
/// through the graph, and claims again.
pub fn restarting_the_fake_aterm_drops_every_connection_and_the_claim() {
    let fake = FakeAterm::new(&root("restart"));
    let (_, holder) = me();
    let socket = fake.locator().socket().expect("socket");
    let mut connection = connect(&fake);
    let mut events = EventReader::connect(&socket, &fake.sid, TIMEOUT).expect("subscribe");
    assert_eq!(claim(&fake, &mut connection, &[Scope::Upgrade]), "OK");
    assert!(status(&fake, &mut connection).held_by(&holder));

    fake.restart();
    assert_eq!(fake.holder(), None);
    let request = aterm::status_verb(&fake.sid).expect("status verb");
    assert!(connection.request_line(&request).is_err());
    // A failed exchange poisons the connection: nothing more is sent on it.
    assert!(connection.is_poisoned());
    assert!(connection.request_line(&request).is_err());
    let update = await_update("the event stream ends", &mut events, |update| update.ended);
    assert!(update.requires_refresh());
    assert!(events.drain().ended);

    let mut again = connect(&fake);
    assert_eq!(status(&fake, &mut again).supervisor, None);
    assert_eq!(claim(&fake, &mut again, &[Scope::Upgrade]), "OK");
    assert!(status(&fake, &mut again).held_by(&holder));
    let mut events = EventReader::connect(&socket, &fake.sid, TIMEOUT).expect("subscribe again");
    fake.event("agent");
    assert!(
        await_update("an event after the restart", &mut events, |update| update
            .activity)
        .fault
        .is_none()
    );
    assert_eq!(fake.subscriptions(), 2);
}
