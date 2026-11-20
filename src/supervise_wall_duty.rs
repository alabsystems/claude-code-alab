// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The walls duty's driver: what the supervisor does with a wall its own
//! Claude Code session's turn ended on, on the read the typing duties share
//! (`src/supervise_duty.rs`, [`Supervisor::duty_look`]), decided by
//! [`crate::walls`] and typed through the claim's fenced writes
//! ([`crate::typing::Typist::write`]). The wall dialogs are boxes, answered by
//! the box duties' driver (`src/supervise_box_duty.rs`) under the same duty;
//! this driver tells that driver whether the usage-limit dialog stands for a
//! limit the harness moves the session off ([`Supervisor::wall_limit`]).
//!
//! **What it acts under.** A claim that takes `walls`
//! ([`crate::aterm::Duties`]), while it stands proven and the session's
//! Autopilot is Cruise or Land. The resume past a usage limit is a `turn-end`
//! act (aterm books `usage-resume@v1` under that duty): it is typed only
//! where the launch takes `turn-end` too, and otherwise not at all, said once
//! (`duty walls left rule=usage-resume@v1 scope=turn-end: not claimed`). The
//! restart the critical-memory banner calls for is a `relaunch` act, which
//! every claim takes ([`crate::aterm::Duties::ALWAYS`]), made only where the
//! policy's `relaunch` is on and aterm judges the moment safe
//! ([`crate::aterm::judge_wall_point`]); successive ones are spaced by
//! [`TurnEndTiming::restart_backoff`], so a banner that comes straight back
//! never restarts the child in a loop.
//!
//! **What it journals.** Each decision once per change (`duty walls wait
//! usage-session-resets until=<ms>`, `duty walls left a lost login: …`),
//! and every write with aterm's answer (`duty walls typed
//! rule=outage-resume@v1 reply=submitted`); a restart as `duty walls restart
//! rule=memory-restart@v1`, beside the supervisor's own `own restart
//! cause=memory …` line. These are journal lines only: nothing here raises an
//! alert or asks a person to act.

use std::time::Duration;

use alab_policy::config::TurnEndTiming;
use alab_policy::ladder::Ladder;
use aterm_phase::{PromptKind, Reading};

use super::link::{self, DutyRead};
use super::turn_end::{BUSY_RETRY_MS, OBSERVED_RETRY_MS, reply_words, typed};
use super::{Ending, Supervisor};
use crate::aterm::{Policy, Restart, Scope};
use crate::limit;
use crate::typing::{Reply, Typed, Write};
use crate::walls::{self, Decision, Look};

/// What spreads this session's resume past a usage limit's reset from other
/// sessions' ([`walls::Look::seed`]): the harness's process id, hashed so
/// that sessions started one after another fall far apart.
fn seed() -> u64 {
    u64::from(std::process::id()).wrapping_mul(0x9E37_79B9_7F4A_7C15)
}

/// `wait` past `now`, saturating.
fn after(now: u64, wait: Duration) -> u64 {
    now.saturating_add(u64::try_from(wait.as_millis()).unwrap_or(u64::MAX))
}

impl Supervisor {
    /// What the harness knows of the usage limit the child stands at beyond
    /// the screen, from the conversation's transcript ([`limit::Sight`]):
    /// when it resets, whether the harness moves the session off it (Fable's
    /// own, on the launch's terms, [`limit::owes_move`]), and whether the
    /// turn-end duty owes the moved session its resume. The second value
    /// says whether the usage-limit dialog on `rows` stands for a limit the
    /// harness moves the session off: the transcript says so, or, before its
    /// row is written, the screen names Fable's limit above the dialog of a
    /// session the move would take ([`walls::fable_limit_above`]). `sight` is
    /// what this look read of the limit's witness.
    pub(super) fn wall_limit(
        &mut self,
        reading: &Reading,
        rows: &[String],
        sight: &limit::Sight,
        now: u64,
    ) -> (walls::Limit, bool) {
        let launch = self.launch_model();
        let current = self.named_model();
        let moving = limit::owes_move(
            &self.limit,
            sight,
            launch.as_deref(),
            current.as_deref(),
            now,
        );
        let on_fable_terms = matches!(self.limit.phase, limit::Phase::OnLaunch { .. })
            && limit::admits_fallback(launch.as_deref())
            && limit::admits_fallback(current.as_deref());
        let dialog_moving = moving
            || (on_fable_terms
                && reading.prompt.as_ref().is_some_and(|p| {
                    p.kind == PromptKind::UsageLimit && walls::fable_limit_above(rows, p.span.0)
                }));
        let known = walls::Limit {
            reset_at_ms: sight
                .wall
                .as_ref()
                .and_then(|wall| wall.resets_at_s)
                .map(|at| at.saturating_mul(1_000)),
            moving,
            // A limit the transcript names since the move is the fallback
            // model's own, and this duty's like any other.
            owed_resume: self.turn_end.carried.owed_resume && sight.wall.is_none(),
        };
        (known, dialog_moving)
    }

    /// The walls duty at one read that no box took: whether it spent the read
    /// (typed, pressed Enter, restarted the child, or waits on a wall), so the
    /// turn-end decider does not act on it. A screen with no wall, a busy turn
    /// and a wall another act answers leave the read to the turn-end decider.
    pub(super) fn wall_look(
        &mut self,
        read: &DutyRead,
        reading: &Reading,
        limit: walls::Limit,
        now: u64,
    ) -> bool {
        let look = Look {
            reading,
            rows: &read.screen.rows,
            seq: read.screen.seq,
            now_ms: now,
            limit,
            relaunch: read.policy.relaunch,
            seed: seed(),
        };
        let decision = self
            .turn_end
            .walls
            .decide(&look, &read.walls, &TurnEndTiming::default());
        let duties = self.setup.opts.duties;
        match decision {
            Decision::Pass(_) => false,
            Decision::Type { scope, rule, .. } | Decision::Submit { scope, rule }
                if !duties.takes(scope) =>
            {
                // A duty the launch left out is performed by nothing.
                self.turn_end.wall_note(
                    &self.journal,
                    &format!("left rule={rule} scope={}: not claimed", scope.word()),
                );
                false
            }
            Decision::Type { scope, rule, text } => {
                self.wall_type(&read.policy, scope, rule, &text, now);
                true
            }
            Decision::Submit { scope, rule } => {
                self.wall_submit(&read.policy, scope, rule, now);
                true
            }
            Decision::Restart { rule } => self.wall_restart(rule, now),
            Decision::Wait { until_ms, .. } => {
                self.turn_end.wall_note(&self.journal, &decision.words());
                self.turn_end.read_at(until_ms);
                true
            }
            Decision::Nothing(_) | Decision::Leave(_) => {
                self.turn_end.wall_note(&self.journal, &decision.words());
                true
            }
        }
    }

    /// Type `text` under `rule` as one guarded turn of the duty `scope`, under
    /// `policy`, and journal aterm's answer: the turn-end duty's own write
    /// (`Supervisor::duty_type`), its act remembered by the walls duty
    /// ([`walls::Watch::typed`]).
    fn wall_type(
        &mut self,
        policy: &Policy,
        scope: Scope,
        rule: &'static str,
        text: &str,
        now: u64,
    ) {
        // A turn is one line: a control character in the words (a line break
        // in a wall's own message) is typed as the space it stands for.
        let line: String = text
            .chars()
            .map(|c| if c.is_control() { ' ' } else { c })
            .collect();
        let text = line.as_str();
        // A link lost in the write is journalled where it is lost.
        let Some(outcome) = self.link_duty_write(policy, scope, Write::Turn(text)) else {
            return;
        };
        match outcome {
            Typed::Answered(reply) => {
                let words = reply_words(&reply);
                if typed(&reply) {
                    self.turn_end.walls.typed(scope, rule, text, now);
                    self.turn_end.carried.wall_typed(rule);
                    self.turn_end
                        .wall_act(&self.journal, &format!("typed rule={rule} reply={words}"));
                    return;
                }
                self.wall_unwritten(policy, &reply, now);
                self.turn_end.wall_note(
                    &self.journal,
                    &format!("unwritten rule={rule} reply={words}"),
                );
            }
            Typed::Held(hold) => {
                if let Some(left) = hold.grace_left {
                    self.turn_end.read_at(after(now, left));
                }
                self.turn_end.wall_note(
                    &self.journal,
                    &format!("held rule={rule} {}={}", hold.field, hold.value),
                );
            }
            Typed::Unread(error) => {
                self.turn_end
                    .wall_note(&self.journal, &format!("unread rule={rule} {error}"));
            }
            Typed::Denied { .. } => {
                self.turn_end
                    .wall_act(&self.journal, &format!("denied rule={rule}"));
            }
        }
    }

    /// Press Enter on this duty's own text under `rule`, which stands unsent
    /// in the composer; the decider never asks for it twice for one act.
    fn wall_submit(&mut self, policy: &Policy, scope: Scope, rule: &'static str, now: u64) {
        let Some(outcome) = self.link_duty_write(policy, scope, Write::Key("enter")) else {
            return;
        };
        let words = match outcome {
            Typed::Answered(reply) => {
                if !matches!(reply, Reply::Written { .. }) {
                    self.wall_unwritten(policy, &reply, now);
                }
                format!("reply={}", reply_words(&reply))
            }
            Typed::Held(hold) => format!("held {}={}", hold.field, hold.value),
            Typed::Unread(error) => format!("unread {error}"),
            Typed::Denied { .. } => "denied".to_string(),
        };
        self.turn_end
            .wall_act(&self.journal, &format!("enter rule={rule} {words}"));
    }

    /// When the duty reads again after aterm wrote nothing for `reply`: after
    /// the person's grace for a person's key, soon for a busy sink, after
    /// [`OBSERVED_RETRY_MS`] for an image aterm has not confirmed, and after
    /// a rung of the usage back-off for a request aterm read otherwise than
    /// it was written (the turn-end duty's own rule,
    /// `Supervisor::duty_unwritten`).
    fn wall_unwritten(&mut self, policy: &Policy, reply: &Reply, now: u64) {
        match reply {
            Reply::Person { .. } => self.turn_end.read_at(after(now, policy.human_grace)),
            Reply::Busy(_) => self.turn_end.read_at(now.saturating_add(BUSY_RETRY_MS)),
            Reply::Observed => self.turn_end.read_at(now.saturating_add(OBSERVED_RETRY_MS)),
            Reply::Usage(_) => self.turn_end.usage(now),
            _ => {}
        }
    }

    /// Restart the child on its conversation for the critical-memory banner
    /// (`rule`), on the model it runs, under the `relaunch` every claim takes:
    /// the restart back-off must be over, nothing else may be ending,
    /// stopping or moving the child, its record must name a conversation that
    /// resumes here, its build must still be a file, and aterm must judge the
    /// moment safe ([`link::Point::Wall`]: nobody typing, no hand, no hold, no
    /// input). Without aterm's word nothing is restarted. Always `true`: the
    /// read is spent on the banner.
    fn wall_restart(&mut self, rule: &'static str, now: u64) -> bool {
        let backoff = self.turn_end.memory_after_ms;
        if now < backoff {
            self.turn_end.wall_note(
                &self.journal,
                &format!("wait restart-backoff rule={rule} until={backoff}"),
            );
            self.turn_end.read_at(backoff);
            return true;
        }
        let held = if self.ending != Ending::None {
            Some("ending".to_string())
        } else if self.child.stopped {
            Some("child-stopped".to_string())
        } else if self.image.blocks_vendor_move() {
            Some("own-image-move".to_string())
        } else if !self.child.build.exe.is_file() {
            Some(format!("vendor-missing build={}", self.child.build.number))
        } else {
            None
        };
        if let Some(why) = held {
            self.turn_end
                .wall_note(&self.journal, &format!("wait {why} rule={rule}"));
            return true;
        }
        let Some(record) = self.record() else {
            self.turn_end
                .wall_note(&self.journal, &format!("wait no-record rule={rule}"));
            return true;
        };
        let restart = Restart {
            cause: walls::CAUSE_MEMORY.to_string(),
            model: None,
            account: None,
        };
        let resume = match self.resume_plan_on(&record, self.model_for(&restart).as_deref()) {
            Ok(resume) => resume,
            Err(why) => {
                self.turn_end
                    .wall_note(&self.journal, &format!("left rule={rule} {why}"));
                return true;
            }
        };
        match self.link_safe_point(link::Point::Wall) {
            link::Verdict::Safe => {}
            link::Verdict::Hold(why) => {
                self.turn_end
                    .wall_note(&self.journal, &format!("held rule={rule} {why}"));
                return true;
            }
            link::Verdict::Local => {
                self.turn_end
                    .wall_note(&self.journal, &format!("wait aterm rule={rule}"));
                return true;
            }
        }
        let plan = match self.same_build_plan(record.session_id.clone(), resume) {
            Ok(plan) => plan,
            Err(why) => {
                self.turn_end
                    .wall_note(&self.journal, &format!("left rule={rule} {why}"));
                return true;
            }
        };
        match self.restart_signal(&restart, &record, plan, true) {
            Ok(()) => {
                let timing = TurnEndTiming::default();
                let n = usize::try_from(self.turn_end.memory_restarts).unwrap_or(usize::MAX);
                let pause = Ladder(timing.restart_backoff.as_slice())
                    .at(n)
                    .unwrap_or_default();
                self.turn_end.memory_restarts = self.turn_end.memory_restarts.saturating_add(1);
                self.turn_end.memory_after_ms = after(now, pause);
                self.turn_end
                    .wall_act(&self.journal, &format!("restart rule={rule}"));
            }
            Err(why) => {
                self.turn_end
                    .wall_note(&self.journal, &format!("unrestarted rule={rule} {why}"));
            }
        }
        true
    }
}
