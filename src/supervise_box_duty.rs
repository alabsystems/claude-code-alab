// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The approvals and questions duties' driver, which also answers the walls
//! duty's dialogs (the usage-limit menu, the autocompact offer and the model
//! switch): what the supervisor does with a box or a question dialog on the
//! read the typing duties share (`src/supervise_duty.rs`,
//! [`Supervisor::duty_look`]), decided by [`crate::boxes`] and pressed through
//! the claim's fenced keys ([`crate::typing::Typist::press`]).
//!
//! **What it acts under.** A claim that takes `approvals`, `questions` or
//! `walls` ([`crate::aterm::Duties`]), while it stands proven and the
//! session's Autopilot is Cruise or Land; a box of a duty the launch does not
//! take is pressed by nothing, and left to the person
//! ([`crate::boxes::Watch::look`]). The usage-limit menu of a
//! limit the harness moves the session off gets no `Wait here`
//! ([`crate::boxes::WallDialogs::moving`], set by
//! `src/supervise_wall_duty.rs`).
//!
//! **When it reads.** On the turn-end duty's reads, and [`BOX_FOLLOW_MS`]
//! after a key it pressed or a key aterm skipped, so a box answered by moving
//! its focus gets its next key on the read that shows the last one took; a
//! person's grace that held a key, and the box's grace after a key a person's
//! gesture overtook, are read again when they have run. A key aterm skipped,
//! or a person's gesture overtook, counts as an attempt
//! ([`crate::boxes::Watch::skipped`]): a face that keeps getting them is left
//! to the person, said once.
//!
//! **What it journals.** Every key with aterm's answer (`duty approvals pressed
//! rule=allow-once@v1 key=1 act=approve reply=written subject=…`), a box left
//! to the person once with its reason (`duty questions left …`), and a key
//! aterm took nothing of, or held, once while aterm's answer stays the same
//! (`duty approvals unwritten …`, `duty approvals held …`). These are journal
//! lines only: nothing here raises an alert or asks a person to act.

use std::time::Duration;

use aterm_phase::{PromptKind, Reading};

use super::Supervisor;
use super::link::DutyRead;
use super::turn_end::{BUSY_RETRY_MS, OBSERVED_RETRY_MS, reply_words};
use crate::aterm::{Scope, journal_word};
use crate::boxes::{self, Next, Step};
use crate::typing::{Press, Reply, Typed};

/// How soon after a key, or a key aterm skipped, the box duties read again.
pub const BOX_FOLLOW_MS: u64 = 500;

/// Whether aterm's answer `reply` to a key says it reached the session, or may
/// have: the key is then never pressed again on the read it was decided from.
const fn reached(reply: &Reply) -> bool {
    matches!(
        reply,
        Reply::Written { .. }
            | Reply::Submitted
            | Reply::Unverified
            | Reply::Left(_)
            | Reply::InDoubt(_)
    )
}

/// `step`'s key and what it means, as journal words.
fn key_words(step: &Step) -> String {
    match step.act {
        Some(act) => format!("key={} {}", step.key, act.option()),
        None => format!("key={}", step.key),
    }
}

impl Supervisor {
    /// The box duties at one read: whether a key was pressed on the box, or is
    /// awaited, so the read is spent. A box left to the person, and a screen
    /// with none, leave the read to the turn-end decider.
    pub(super) fn box_look(&mut self, read: &DutyRead, reading: &Reading, now: u64) -> bool {
        let duties = self.setup.opts.duties;
        let ruling = boxes::read(reading, &read.screen.rows, &read.boxes);
        // The person's count read in the same hold as the rows: a gesture the
        // rows miss moves past it, and aterm then presses nothing.
        let human_seq = read.screen.human_seq;
        let next = self
            .turn_end
            .boxes
            .look(&ruling, read.screen.seq, human_seq, now, |scope| {
                duties.takes(scope)
            });
        match next {
            Next::Nothing("awaiting-screen" | "awaiting-key") => {
                self.turn_end
                    .read_in(now, Duration::from_millis(BOX_FOLLOW_MS));
                true
            }
            Next::Nothing(_) => false,
            Next::Left { scope, why } => {
                self.turn_end
                    .box_act(&self.journal, scope, &format!("left {why}"));
                false
            }
            Next::Press { scope, kind, step } => {
                self.box_press(read, scope, kind, &step, human_seq, now);
                true
            }
        }
    }

    /// Press `step`, a key of duty `scope` on a box of `kind`, fenced on the
    /// read `read` it was decided from, and journal aterm's answer.
    fn box_press(
        &mut self,
        read: &DutyRead,
        scope: Scope,
        kind: PromptKind,
        step: &Step,
        human_seq: u64,
        now: u64,
    ) {
        let rule = step.rule;
        // A host that stamps no generation on its screen cannot fence a key
        // on the read it was decided from: nothing is pressed.
        let Some(generation) = read.screen.generation.as_deref() else {
            self.turn_end.box_note(
                &self.journal,
                scope,
                &format!("held rule={rule} gen=absent"),
            );
            return;
        };
        let press = Press {
            key: &step.key,
            generation,
            guard: &step.guard,
            act: step.act,
            human_seq,
            grace: boxes::grace(kind, read.policy.human_grace),
        };
        // A link lost in the press is journalled where it is lost.
        let Some(outcome) = self.link_duty_press(&read.policy, scope, &press) else {
            return;
        };
        let key = key_words(step);
        match outcome {
            Typed::Answered(reply) => {
                let words = reply_words(&reply);
                if reached(&reply) {
                    self.turn_end.boxes.pressed(now);
                    self.turn_end
                        .read_in(now, Duration::from_millis(BOX_FOLLOW_MS));
                    self.turn_end.box_act(
                        &self.journal,
                        scope,
                        &format!(
                            "pressed rule={rule} {key} reply={words} subject={}",
                            journal_word(&step.subject)
                        ),
                    );
                    return;
                }
                self.box_unwritten(&reply, press.grace, now);
                self.turn_end.box_note(
                    &self.journal,
                    scope,
                    &format!("unwritten rule={rule} {key} reply={words}"),
                );
            }
            Typed::Held(hold) => {
                if let Some(left) = hold.grace_left {
                    self.turn_end.read_in(now, left);
                }
                self.turn_end.box_note(
                    &self.journal,
                    scope,
                    &format!("held rule={rule} {}={}", hold.field, hold.value),
                );
            }
            Typed::Unread(error) => {
                self.turn_end.box_note(
                    &self.journal,
                    scope,
                    &format!("unread rule={rule} {error}"),
                );
            }
            Typed::Denied { .. } => {
                self.turn_end
                    .box_act(&self.journal, scope, &format!("denied rule={rule}"));
            }
        }
    }

    /// When the box duties read again after aterm pressed nothing for
    /// `reply`: soon for a screen that moved or a guard no row matched, after
    /// the box's `grace` for a person's gesture (whose box the next read
    /// gives the person), after [`BUSY_RETRY_MS`] for a busy sink, after
    /// [`OBSERVED_RETRY_MS`] for an image aterm has not confirmed, and after
    /// a rung of the usage back-off for a request aterm read otherwise than
    /// it was written (the turn-end duty's own rule). A skip and a person's
    /// gesture count as an attempt on the box's face
    /// ([`crate::boxes::Watch::skipped`]).
    fn box_unwritten(&mut self, reply: &Reply, grace: Duration, now: u64) {
        let follow = match reply {
            Reply::Skipped(_) => {
                self.turn_end.boxes.skipped(now);
                Duration::from_millis(BOX_FOLLOW_MS)
            }
            Reply::Person { .. } => {
                self.turn_end.boxes.skipped(now);
                grace
            }
            Reply::Busy(_) => Duration::from_millis(BUSY_RETRY_MS),
            Reply::Observed => Duration::from_millis(OBSERVED_RETRY_MS),
            Reply::Usage(_) => {
                self.turn_end.usage(now);
                return;
            }
            _ => return,
        };
        self.turn_end.read_in(now, follow);
    }
}
