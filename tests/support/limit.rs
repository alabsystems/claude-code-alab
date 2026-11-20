// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Fable's usage limit, end to end: the harness reads the limit row in the
//! conversation's transcript, ends its child with one SIGTERM and resumes the
//! conversation on Opus, and goes back once the limit has reset. Nothing is
//! typed, to the child or through aterm.

use super::*;

pub(super) const OPUS: &str = "claude-opus-5-5";
pub(super) const FABLE_BUCKET: &str = "seven_day_overage_included";

pub(super) fn epoch_ms() -> u64 {
    u64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_millis(),
    )
    .expect("ms")
}

/// `ms` (epoch) as the UTC stamp Claude Code writes on a transcript row.
fn stamp(ms: u64) -> String {
    let (days, rest) = (ms / 86_400_000, ms % 86_400_000);
    // Days since 1970-01-01 to a civil date (the proleptic Gregorian
    // calendar, counted in 400-year eras that start on a 1 March).
    let z = days + 719_468;
    let (era, day_of_era) = (z / 146_097, z % 146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let shifted_month = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * shifted_month + 2) / 5 + 1;
    let month = if shifted_month < 10 {
        shifted_month + 3
    } else {
        shifted_month - 9
    };
    let year = year_of_era + era * 400 + u64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}.{:03}Z",
        rest / 3_600_000,
        rest / 60_000 % 60,
        rest / 1_000 % 60,
        rest % 1_000
    )
}

/// The conversation's transcript under `s`'s home, created with one row
/// that says nothing, so the harness finds it at its first look.
pub(super) fn transcript(s: &Scenario) -> PathBuf {
    let project = s.home.join(".claude/projects/-scenario-work");
    std::fs::create_dir_all(&project).expect("project dir");
    let path = project.join(format!("{SID}.jsonl"));
    std::fs::write(
        &path,
        format!(
            "{{\"type\":\"user\",\"isSidechain\":false,\"timestamp\":\"{}\",\
             \"message\":{{\"role\":\"user\",\"content\":\"hello\"}}}}\n",
            stamp(epoch_ms())
        ),
    )
    .expect("transcript");
    path
}

pub(super) fn append(transcript: &Path, rows: &str) {
    std::fs::OpenOptions::new()
        .append(true)
        .open(transcript)
        .expect("transcript")
        .write_all(rows.as_bytes())
        .expect("append");
}

/// The row Claude Code 2.1.323 writes for a request refused at a usage limit
/// (read off a real transcript; the usage block is shortened), stamped
/// `at_ms`, for the window `bucket` that resets at `resets_at_s`.
pub(super) fn limit_row(at_ms: u64, bucket: &str, resets_at_s: u64, sidechain: bool) -> String {
    format!(
        "{{\"parentUuid\":\"5e5e0483-0000-4000-8000-000000000001\",\"isSidechain\":{sidechain},\
         \"type\":\"assistant\",\"uuid\":\"597479d3-0000-4000-8000-000000000002\",\
         \"timestamp\":\"{}\",\"message\":{{\"diagnostics\":null,\"id\":\"328c5ebe\",\
         \"container\":null,\"model\":\"<synthetic>\",\"role\":\"assistant\",\
         \"stop_reason\":\"stop_sequence\",\"stop_sequence\":\"\",\"type\":\"message\",\
         \"usage\":{{\"input_tokens\":0,\"output_tokens\":0}},\
         \"content\":[{{\"type\":\"text\",\"text\":\"You've hit your Fable limit \u{b7} resets \
         Nov 15 at 9am (America/Los_Angeles)\"}}],\"context_management\":null}},\
         \"requestId\":\"req_0\",\"apiError\":\"usage_limit_reached\",\
         \"quotaLimits\":{{\"status\":\"rejected\",\"resetsAt\":{resets_at_s},\
         \"unifiedRateLimitFallbackAvailable\":false,\"rateLimitType\":\"{bucket}\",\
         \"overageStatus\":\"rejected\",\"overageDisabledReason\":\"org_level_disabled\",\
         \"upgradePaths\":[\"overage\"],\"isUsingOverage\":false}},\"error\":\"rate_limit\",\
         \"isApiErrorMessage\":true,\"apiErrorStatus\":429,\"perTurnEffort\":\"high\",\
         \"userType\":\"external\",\"entrypoint\":\"cli\",\"sessionId\":\"{SID}\",\
         \"version\":\"2.1.323\"}}\n",
        stamp(at_ms)
    )
}

/// A person's message, stamped `at_ms`, whose text is `text`: the notice's
/// words, or a whole limit row quoted as a conversation would hold it.
fn message_row(at_ms: u64, text: &str) -> String {
    let quoted = text.trim_end().replace('\\', "\\\\").replace('"', "\\\"");
    format!(
        "{{\"type\":\"user\",\"isSidechain\":false,\"timestamp\":\"{}\",\
         \"message\":{{\"role\":\"user\",\"content\":\"{quoted}\"}}}}\n",
        stamp(at_ms)
    )
}

/// Write the row of Fable's own limit, refused now and resetting `away_s`
/// from now; the row's time and its reset (epoch seconds).
fn hit_fables_limit(transcript: &Path, away_s: u64) -> (u64, u64) {
    let at_ms = epoch_ms();
    let resets_at_s = at_ms / 1_000 + away_s;
    append(
        transcript,
        &limit_row(at_ms, FABLE_BUCKET, resets_at_s, false),
    );
    (at_ms / 1_000, resets_at_s)
}

fn terms(s: &Scenario) -> usize {
    s.log().iter().filter(|l| *l == "signal TERM").count()
}

/// Nothing was typed into the vendor: no fake ever read a byte from the
/// terminal.
fn assert_nothing_was_typed(s: &Scenario) {
    let read: Vec<String> = s
        .log()
        .into_iter()
        .filter(|l| l.starts_with("read "))
        .collect();
    assert!(
        read.is_empty(),
        "typed into the vendor: {read:?}{}",
        s.report()
    );
}

/// Nothing was typed through aterm either.
fn assert_aterm_typed_nothing(s: &Scenario, fake: &FakeAterm) {
    let typed: Vec<String> = fake
        .verbs()
        .into_iter()
        .filter(|verb| {
            ["send", "key", "turn", "answer"]
                .iter()
                .any(|word| verb.split_ascii_whitespace().next() == Some(word))
        })
        .collect();
    assert!(
        typed.is_empty(),
        "typed through aterm: {typed:?}{}",
        s.report()
    );
}

/// The `back-at=` of the journal's `nth` `model-move` line (epoch seconds),
/// `None` for `-`.
fn back_at(s: &Scenario, nth: usize) -> Option<u64> {
    let journal = s.journal();
    let line = journal
        .lines()
        .filter(|line| line.contains(" model-move "))
        .nth(nth)
        .unwrap_or_else(|| panic!("no model-move line {nth}{}", s.report()));
    line.rsplit_once(" back-at=")
        .unwrap_or_else(|| panic!("no back-at in {line:?}"))
        .1
        .parse()
        .ok()
}

/// Outside aterm. A session launched on Fable is refused at Fable's own
/// limit: once the row is a quiet period old the harness sends the vendor one
/// SIGTERM and resumes the conversation on Opus, saying so in one line. When
/// the limit has reset it resumes the conversation on Fable again. The limit
/// row it already answered moves nothing afterwards, and a return that is
/// refused again before it answered stays away for the first rung, not for
/// the sooner reset the new row names.
///
/// CONTROLS, before the limit, each several quiet periods of looks old: a
/// Fable limit row from before this launch, already in the transcript the
/// session starts on; the notice's words and a whole limit row quoted in a
/// person's message; a subagent's Fable limit; and the session's own limit
/// in another window. They move nothing and journal nothing.
pub(super) fn a_fable_limit_resumes_on_opus_and_goes_back_when_it_resets() {
    assert_eq!(
        stamp(1_794_551_966_429),
        "2026-11-13T06:39:26.429Z",
        "the stamp helper, against the real row's own time"
    );
    let s = Scenario::new("limit-fallback");
    let transcript = transcript(&s);
    // A reading from before this launch: Fable's limit, refused two minutes
    // ago and resetting in an hour, in the conversation the session is on.
    let before = epoch_ms() - 120_000;
    append(
        &transcript,
        &limit_row(before, FABLE_BUCKET, before / 1_000 + 3_720, false),
    );
    let pty = Pty::open();
    let (_child, pid) = start_on_pty(&s, &pty, &["--alab-reset-grace-ms=0", "--model", "fable"]);
    let first = await_record(&s, 0);
    let now = epoch_ms();
    let fables = limit_row(now, FABLE_BUCKET, now / 1_000 + 3_600, false);
    append(
        &transcript,
        &format!(
            "{}{}{}{}",
            message_row(
                now,
                "You've hit your Fable limit \u{b7} resets Nov 15 at 9am (America/Los_Angeles)"
            ),
            message_row(now, &fables),
            limit_row(now, FABLE_BUCKET, now / 1_000 + 3_600, true),
            limit_row(now, "five_hour", now / 1_000 + 3_600, false)
        ),
    );
    // Several quiet periods of looks.
    std::thread::sleep(Duration::from_millis(1_800));
    let journal = s.journal();
    assert!(
        s.starts().len() == 1
            && !got_term(&s)
            && !journal.contains(" limit-seen ")
            && !journal.contains(" limit-move ")
            && !journal.contains(" own restart "),
        "moved for what is not the session's own Fable limit since its launch{}",
        s.report()
    );

    // The limit, resetting in a few seconds.
    let (at_s, resets_at_s) = hit_fables_limit(&transcript, 20);
    assert!(
        wait_until(Duration::from_secs(15), || s.starts().len() >= 2),
        "no move off Fable's limit{}",
        s.report()
    );
    let second = await_record(&s, 1);
    let starts = s.starts();
    assert_eq!(
        starts[1].args,
        ["--model", OPUS, "--resume", SID],
        "{}",
        s.report()
    );
    assert_eq!(starts[1].build(), Some(BUILD_1), "{}", s.report());
    assert_eq!(terms(&s), 1, "exactly one SIGTERM{}", s.report());
    assert_eq!(wait_status(pid, Duration::ZERO), None, "the harness stayed");
    let journal = s.journal();
    for line in [
        format!(
            " limit-seen bucket={FABLE_BUCKET} at={at_s} resets-at={resets_at_s} \
             decision=move to={OPUS}\n"
        ),
        format!(
            " own restart cause=fable-limit model={OPUS} SIGTERM pid={first} session={SID} \
             build={BUILD_1} argv=--model\\u{{1f}}{OPUS}\\u{{1f}}--resume\\u{{1f}}{SID}\n"
        ),
        format!(
            " model-move to={OPUS} from=fable cause=fable-limit kept=switched \
             back-at={resets_at_s}\n"
        ),
    ] {
        assert!(journal.contains(&line), "missing {line:?}{}", s.report());
    }
    assert!(
        !journal.contains(" crash") && !journal.contains("ignored") && !journal.contains("failed"),
        "{}",
        s.report()
    );
    assert!(
        wait_until(Duration::from_secs(5), || pty.output().contains(&format!(
            "Fable's usage limit is reached; resuming conversation {SID} on {OPUS}"
        ))),
        "the move was not said\n--- pty ---\n{}",
        pty.output()
    );

    // The reset: back onto the launch's model, at a quiet moment.
    assert!(
        wait_until(Duration::from_secs(60), || s.starts().len() >= 3),
        "no return once the limit had reset{}",
        s.report()
    );
    let third = await_record(&s, 2);
    assert!(
        epoch_ms() / 1_000 >= resets_at_s,
        "went back before the reset{}",
        s.report()
    );
    assert_eq!(
        s.starts()[2].args,
        ["--model", "fable", "--resume", SID],
        "{}",
        s.report()
    );
    assert_eq!(terms(&s), 2, "{}", s.report());
    let journal = s.journal();
    for line in [
        " limit-return due to=fable\n".to_string(),
        // The return is the restart aterm asks for with `model=launch`: the
        // launch's own line, which names Fable here, and nothing kept.
        format!(
            " own restart cause=fable-reset model=launch SIGTERM pid={second} session={SID} \
             build={BUILD_1} argv=--model\\u{{1f}}fable\\u{{1f}}--resume\\u{{1f}}{SID}\n"
        ),
        format!(
            " model-move to=launch launch-model=fable from={OPUS} cause=fable-reset kept=launch \
             back-at=-\n"
        ),
    ] {
        assert!(journal.contains(&line), "missing {line:?}{}", s.report());
    }
    assert!(
        wait_until(Duration::from_secs(5), || pty.output().contains(&format!(
            "Fable's usage limit has reset; resuming conversation {SID} on fable"
        ))),
        "the return was not said\n--- pty ---\n{}",
        pty.output()
    );
    // The row that was answered is spent: looked at for several quiet
    // periods on the returned child, it is neither seen nor acted on again.
    std::thread::sleep(Duration::from_millis(1_800));
    assert_eq!(s.starts().len(), 3, "{}", s.report());
    assert_eq!(
        s.journal().matches(" limit-seen ").count(),
        1,
        "one line per limit{}",
        s.report()
    );

    // Refused again before the return answered anything, by a row that
    // names a reset twenty minutes away: the session leaves Fable again, and
    // this time for the first rung (half an hour), not for the reset a
    // refused return has just shown not to lift the limit.
    let (again_s, _) = hit_fables_limit(&transcript, 1_200);
    assert!(
        wait_until(Duration::from_secs(15), || s.starts().len() >= 4),
        "no move off the second limit{}",
        s.report()
    );
    await_record(&s, 3);
    assert_eq!(
        s.starts()[3].args,
        ["--model", OPUS, "--resume", SID],
        "{}",
        s.report()
    );
    assert!(
        s.journal().contains(&format!(
            " own restart cause=fable-limit model={OPUS} SIGTERM pid={third} "
        )),
        "{}",
        s.report()
    );
    let away = back_at(&s, 2).expect("a time to go back") - again_s;
    assert!(
        (1_790..=1_830).contains(&away),
        "a refused return is away {away} s, not the first rung{}",
        s.report()
    );
    assert_eq!(
        back_at(&s, 0),
        Some(resets_at_s),
        "the control: the named reset"
    );
    assert_eq!(terms(&s), 3, "{}", s.report());
    assert_nothing_was_typed(&s);
    end_session(&s, pid);
}

/// A launch that itself asked for a model that is not Fable is never moved:
/// the same row that moves a Fable session within a second is journalled once
/// with the decision to stay, and nothing is signalled.
///
/// CONTROL: the same launch on Fable, given the same row, is moved
/// ([`a_fable_limit_resumes_on_opus_and_goes_back_when_it_resets`]).
pub(super) fn a_fable_limit_never_moves_a_launch_on_another_model() {
    let s = Scenario::new("limit-other-model");
    let transcript = transcript(&s);
    let pty = Pty::open();
    let (_child, pid) = start_on_pty(&s, &pty, &["--model", "sonnet"]);
    await_record(&s, 0);
    let (at_s, resets_at_s) = hit_fables_limit(&transcript, 3_600);
    await_journal(
        &s,
        Duration::from_secs(15),
        &format!(
            "limit-seen bucket={FABLE_BUCKET} at={at_s} resets-at={resets_at_s} \
             decision=stay model=sonnet"
        ),
    );
    // Several quiet periods of looks.
    std::thread::sleep(Duration::from_millis(1_800));
    let journal = s.journal();
    assert!(
        s.starts().len() == 1
            && !got_term(&s)
            && !journal.contains(" limit-move ")
            && !journal.contains(" own restart ")
            && !journal.contains(" model-move "),
        "a launch on another model was moved{}",
        s.report()
    );
    assert_eq!(journal.matches(" limit-seen ").count(), 1, "{}", s.report());
    assert_eq!(wait_status(pid, Duration::ZERO), None, "the harness stayed");
    assert_nothing_was_typed(&s);
    end_session(&s, pid);
}

/// Inside aterm, under the proven claim. With the vendor's dialog open aterm
/// shows a busy agent, no composer (`composer=-`) and a session that needs a
/// person; none of that holds the move, and a person at the keys does. Once the person is
/// quiet the harness asks aterm once more and makes the move with one
/// SIGTERM, typing nothing. A restart aterm then asks for onto the model the
/// child already runs on is journalled and makes no second restart.
///
/// CONTROL: the same request naming another model is made.
pub(super) fn aterm_a_fable_limit_moves_under_the_claim_and_a_repeat_request_is_a_no_op() {
    let s = Scenario::new("aterm-limit-fallback");
    let transcript = transcript(&s);
    let fake = rostered_aterm(&s);
    let pty = Pty::open();
    let (_child, pid) = start_in_aterm_with(&s, &pty, &fake, 40, &["--model", "fable"]);
    let first = await_record(&s, 0);
    for (key, value) in [
        ("agent", "busy"),
        ("composer", "-"),
        ("need", "approval"),
        // Inside the policy's 120 s grace.
        ("human_ms", "1000"),
    ] {
        fake.set_status(key, Some(value));
    }
    let (_, resets_at_s) = hit_fables_limit(&transcript, 3_600);
    await_journal(
        &s,
        Duration::from_secs(15),
        "limit-move wait aterm human_ms=1000",
    );
    // Several looks under the hold.
    std::thread::sleep(Duration::from_millis(400));
    assert!(
        !got_term(&s) && s.starts().len() == 1,
        "moved over a person at the keys{}",
        s.report()
    );
    fake.set_status("human_ms", Some("-"));
    fake.event("status");
    assert!(
        wait_until(Duration::from_secs(15), || s.starts().len() >= 2),
        "no move once the person was quiet{}",
        s.report()
    );
    let second = await_record(&s, 1);
    assert_eq!(
        s.starts()[1].args,
        ["--model", OPUS, "--resume", SID],
        "{}",
        s.report()
    );
    assert_eq!(terms(&s), 1, "{}", s.report());
    let journal = s.journal();
    for line in [
        format!(" own restart cause=fable-limit model={OPUS} SIGTERM pid={first} session={SID} "),
        format!(
            " model-move to={OPUS} from=fable cause=fable-limit kept=switched \
             back-at={}\n",
            // The default grace after the named reset.
            resets_at_s + 60
        ),
    ] {
        assert!(journal.contains(&line), "missing {line:?}{}", s.report());
    }
    assert!(
        !journal.contains("wait aterm agent=")
            && !journal.contains("wait aterm composer=")
            && !journal.contains("wait aterm need="),
        "the dialog held the move{}",
        s.report()
    );
    assert_eq!(
        fake.holder(),
        Some(format!("claude-code-alab@{pid}")),
        "the claim stands"
    );

    // aterm asks for the same move a moment later.
    fake.event(&format!("restart cause=model model={OPUS}"));
    await_journal(
        &s,
        Duration::from_secs(10),
        &format!("aterm restart cause=model model={OPUS} ignored already-on"),
    );
    std::thread::sleep(Duration::from_millis(500));
    assert!(
        s.starts().len() == 2 && terms(&s) == 1,
        "a second restart onto the model the child runs on{}",
        s.report()
    );
    // CONTROL: another model is a request, and is made.
    fake.set_status("need", Some("-"));
    fake.event("restart cause=model model=sonnet");
    assert!(
        wait_until(Duration::from_secs(10), || s.starts().len() >= 3),
        "a model restart onto another model was not made{}",
        s.report()
    );
    await_record(&s, 2);
    assert_eq!(
        s.starts()[2].args,
        ["--model", "sonnet", "--resume", SID],
        "{}",
        s.report()
    );
    assert!(
        s.journal().contains(&format!(
            " aterm restart cause=model model=sonnet SIGTERM pid={second} "
        )),
        "{}",
        s.report()
    );
    assert_eq!(terms(&s), 2, "{}", s.report());
    assert_nothing_was_typed(&s);
    assert_aterm_typed_nothing(&s, &fake);
    end_session(&s, pid);
}

/// The move is made whatever the session's Autopilot word is. Under Land the
/// claim stands and aterm is asked, as under Cruise. With Autopilot off at
/// the launch there is no claim, and with Autopilot turned off later the
/// claim is given back at the question: both times the harness moves on its
/// own readings of the terminal.
pub(super) fn aterm_a_fable_limit_moves_under_every_autopilot_word() {
    let land = POLICY_ON.replace("autopilot=cruise", "autopilot=land");
    assert_ne!(land, POLICY_ON);
    for (name, at_launch, later) in [
        ("land", Some(land.clone()), None),
        ("off-at-launch", Some(autopilot_off()), None),
        ("turned-off", None, Some(autopilot_off())),
    ] {
        let s = Scenario::new(&format!("aterm-limit-autopilot-{name}"));
        let transcript = transcript(&s);
        let fake = rostered_aterm(&s);
        if let Some(policy) = &at_launch {
            fake.set_policy_quietly(policy);
        }
        let pty = Pty::open();
        let (_child, pid) = start_in_aterm_with(&s, &pty, &fake, 40, &["--model", "fable"]);
        let first = await_record(&s, 0);
        let claimed = fake.holder() == Some(format!("claude-code-alab@{pid}"));
        assert_eq!(claimed, name != "off-at-launch", "{name}{}", s.report());
        if let Some(policy) = &later {
            fake.set_policy_quietly(policy);
        }
        hit_fables_limit(&transcript, 3_600);
        assert!(
            wait_until(Duration::from_secs(20), || s.starts().len() >= 2),
            "{name}: no move off Fable's limit{}",
            s.report()
        );
        await_record(&s, 1);
        assert_eq!(
            s.starts()[1].args,
            ["--model", OPUS, "--resume", SID],
            "{name}{}",
            s.report()
        );
        assert_eq!(terms(&s), 1, "{name}{}", s.report());
        let journal = s.journal();
        let restart = format!(" own restart cause=fable-limit model={OPUS} SIGTERM pid={first} ");
        assert!(journal.contains(&restart), "{name}{}", s.report());
        match name {
            "land" => assert!(
                !journal.contains(" aterm released ") && !journal.contains(" aterm unclaimed "),
                "{name}{}",
                s.report()
            ),
            "off-at-launch" => assert!(
                journal.contains(" aterm unclaimed policy effective=off why=autopilot-off\n"),
                "{name}{}",
                s.report()
            ),
            _ => {
                let order = [
                    " aterm released policy effective=off why=autopilot-off\n",
                    restart.as_str(),
                ]
                .map(|line| journal_at(&s, line));
                assert!(order.is_sorted(), "{name} {order:?}{}", s.report());
            }
        }
        assert_nothing_was_typed(&s);
        assert_aterm_typed_nothing(&s, &fake);
        end_session(&s, pid);
    }
}

/// A session moved off Fable's limit under Cruise is owed a resume: the
/// vendor stopped at its limit wall, and nothing else asks the agent to go
/// on. It is typed once, through the claim's fence, in the harness's own
/// words (`fallback-resume@v1`), in place of Cruise's continuation.
///
/// CONTROL: the move itself types nothing while aterm states no mode, here
/// and in [`aterm_a_fable_limit_moves_under_every_autopilot_word`].
// Not registered: the resume is the turn-end duty's, which the shared link's
// claim does not take yet (`src/supervise_link.rs`).
#[allow(dead_code)]
pub(super) fn aterm_a_session_moved_off_fables_limit_is_resumed_once_under_cruise() {
    let s = Scenario::new("aterm-limit-resume");
    let transcript = transcript(&s);
    let fake = rostered_aterm(&s);
    let pty = Pty::open();
    let (_child, pid) = start_in_aterm_with(&s, &pty, &fake, 40, &["--model", "fable"]);
    await_record(&s, 0);
    hit_fables_limit(&transcript, 3_600);
    assert!(
        wait_until(Duration::from_secs(20), || s.starts().len() >= 2),
        "no move off Fable's limit{}",
        s.report()
    );
    await_record(&s, 1);
    assert_aterm_typed_nothing(&s, &fake);
    fake.set_status("autopilot", Some("cruise"));
    fake.event("meta field=autopilot value=cruise");
    let resume = format!("turn {}", claude_code_alab::duty::FALLBACK_RESUME_TEXT);
    assert!(
        wait_until(Duration::from_secs(15), || fake.typed().contains(&resume)),
        "no resume{}",
        s.report()
    );
    await_journal(
        &s,
        Duration::from_secs(5),
        "duty turn-end typed rule=fallback-resume@v1 reply=submitted",
    );
    // The screen that shows it, read again, gets nothing more.
    assert!(
        wait_until(Duration::from_secs(15), || turn_end::awaited_again(&s)),
        "the unchanged screen was not read again{}",
        s.report()
    );
    assert_eq!(fake.typed(), [resume.as_str(), "enter"], "{}", s.report());
    assert_nothing_was_typed(&s);
    end_session(&s, pid);
}

/// A move that cannot be made is journalled once, with its pause, and is not
/// tried on every look: the child is left running at the limit. Here the
/// build the child runs has lost its executable, so a child ended for the
/// move could not be started again.
///
/// CONTROL: with the build back in place nothing moves either until the
/// pause is over (a minute), and the same limit with the build in place
/// from the start moves within a second
/// ([`a_fable_limit_resumes_on_opus_and_goes_back_when_it_resets`]).
pub(super) fn a_fable_limit_move_that_cannot_be_made_backs_off() {
    let s = Scenario::new("limit-backoff");
    let transcript = transcript(&s);
    let pty = Pty::open();
    let (_child, pid) = start_on_pty(&s, &pty, &["--model", "fable"]);
    await_record(&s, 0);
    let exe = PathBuf::from(&s.starts()[0].exe);
    let aside = exe.with_extension("aside");
    std::fs::rename(&exe, &aside).expect("take the build's executable away");
    hit_fables_limit(&transcript, 3_600);
    let failed = format!(
        " limit-move failed cause=fable-limit vendor-missing build={BUILD_1} attempt=1 \
         retry-in-ms=60000\n"
    );
    assert!(
        wait_until(Duration::from_secs(15), || s.journal().contains(&failed)),
        "the failure was not journalled{}",
        s.report()
    );
    std::fs::rename(&aside, &exe).expect("put the executable back");
    // Dozens of looks inside the pause.
    std::thread::sleep(Duration::from_millis(2_000));
    let journal = s.journal();
    assert_eq!(
        journal.matches(" limit-move failed ").count(),
        1,
        "{}",
        s.report()
    );
    assert_eq!(journal.matches(" limit-seen ").count(), 1, "{}", s.report());
    assert!(
        !got_term(&s) && s.starts().len() == 1 && !journal.contains(" own restart "),
        "the child was ended for a move that could not be made, or the move was tried again \
         inside its pause{}",
        s.report()
    );
    assert_eq!(wait_status(pid, Duration::ZERO), None, "the harness stayed");
    assert_nothing_was_typed(&s);
    end_session(&s, pid);
}
