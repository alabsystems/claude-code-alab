// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! claude-code-alab: the ALab harness for Claude Code.
//!
//! `claude-alab` runs the **unmodified** vendor `claude` that atpkg installed
//! (it never ships, forks or patches vendor bytes) as its child on the same
//! terminal, applies its managed launch policy, follows the child's signals, ends
//! the way the child ends, and moves the child onto a newer vendor build
//! underneath itself at a safe point, resuming the same conversation, without
//! the harness process exiting.
//!
//! Modules, in the order a launch meets them:
//! * [`cli`]: the harness's own options and `--version`;
//! * [`store`]: the vendor build in the atpkg store (never `PATH`);
//! * [`supervise`]: the child, its signals, its end, the live update;
//! * [`quiet`]: the safe point a live update waits for;
//! * [`profile`]: Claude Code's facts (record, resume grammar, permission mode);
//! * [`model_facts`]: the model, effort level and top-effort key the child
//!   states in its transcript, which the supervisor journals;
//! * [`procs`]: the child's background work, which a safe point waits for;
//! * [`aterm`]: the aterm client boundary (claim text, reply parsers, the
//!   safe-point judge), used by the supervisor's link inside an aterm tab;
//! * [`aterm_wire`]: direct transport to aterm's control socket (the link's
//!   request connection and event stream), connection custody, and the
//!   spelling of a fenced typed write;
//! * [`typing`]: the fenced typing client, which types only under the claim's
//!   holder, revision and duty and the person's count (no claim takes a
//!   typing duty yet, so nothing calls it);
//! * [`relaunch`]: aterm's exit classification and crash back-off, which
//!   decide a crash relaunch and a restart aterm asks for;
//! * [`self_image`]: the harness's own build in the atpkg store, selected and
//!   retained for an own-image move;
//! * [`transfer_capsule`]: the bounded frame that carries the supervisor's
//!   state across the exec of a new harness build, and back on a rollback;
//! * [`journal`], [`json`], [`sys`]: support.

// Harness output goes through `cli::say` and the journal, whose writes ignore
// errors: a `print!`-family macro panics on a closed or broken descriptor, and
// a panic mid-relaunch would lose the session.
#![cfg_attr(not(test), deny(clippy::print_stdout, clippy::print_stderr))]

pub mod aterm;
pub mod aterm_wire;
pub mod cli;
pub mod folder_trust;
pub mod inner_pty;
pub mod inspect;
pub mod journal;
pub mod json;
pub mod launch;
pub mod model_facts;
pub mod permission;
pub mod prime;
pub mod procs;
pub mod profile;
pub mod quiet;
pub mod relaunch;
/// Owned headless SDK control observations; never interactive TUI transition authority.
pub mod sdk_permission;
pub mod self_image;
pub mod store;
pub mod supervise;
pub mod sys;
#[cfg(test)]
mod test_support;
#[cfg(test)]
extern crate self as claude_code_alab;
/// The existing integration socket fixture, also used by native unit controls.
#[cfg(test)]
#[path = "../tests/support/aterm.rs"]
pub mod test_aterm;
pub mod transfer_capsule;
pub mod typing;
