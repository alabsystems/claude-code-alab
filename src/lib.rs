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
//! * [`supervise`]: the child, its signals, its end, the live update, and the adapter
//!   to [`alab_wire::link::Link`] for lifecycle claims and recovery;
//! * [`quiet`]: the safe point a live update waits for;
//! * [`profile`]: Claude Code's facts (record, resume grammar, permission mode);
//! * [`model_facts`]: the model, effort level and top-effort key the child
//!   states in its transcript, which the supervisor journals;
//! * [`limit`]: Fable's own usage limit, read from the transcript, and when
//!   the supervisor moves the session onto Opus and back for it;
//! * [`procs`]: the child's background work, which a safe point waits for;
//! * [`aterm`]: local request/reply interpretation used by typed-client protocol fixtures;
//! * [`aterm_wire`]: direct transport, typed request builders and retained
//!   connection custody for image recovery, and the
//!   spelling of a fenced typed write;
//! * [`typing`]: the fenced typing client, which types only under the claim's
//!   holder, revision and duty and the person's count (the shared link's
//!   claim takes no typing duty yet, so nothing types through it);
//! * [`duty`]: the turn-end duty's decider, Autopilot's Off, Cruise and Land
//!   on one certain read of Claude Code's screen, which the supervisor types
//!   through [`typing`] once its claim takes `turn-end`;
//! * [`boxes`]: the approvals and questions duties' decider, which presses
//!   one fenced key at a read on a box or a question dialog of the session's
//!   own Claude Code once its claim takes `approvals` or `questions`, and
//!   the wall dialogs (a usage limit's options, the autocompact offer, a model
//!   switch's confirmation) once it takes `walls`;
//! * [`walls`]: the walls duty's decider: a usage limit waited out and
//!   resumed past its reset, an outage or API error tried again, a full
//!   context compacted, the critical-memory banner's restart, a lost login
//!   left to its person, on one certain read once the claim takes `walls`;
//! * [`repo`]: what git finds in the session's repositories that no remote
//!   main has, which Land's rest waits on, read in a process of its own
//!   beside the loop ([`repo_probe`]);
//! * [`relaunch`]: exit classification and crash back-off supporting the supervisor;
//! * [`self_image`]: the harness's own build in the atpkg store, selected and
//!   retained for an own-image move;
//! * [`transfer_capsule`]: the bounded frame that carries the supervisor's
//!   state across the exec of a new harness build, and back on a rollback;
//! * [`journal`], [`json`], [`sys`]: support.

// Harness output goes through `cli::say` and the journal, whose writes ignore
// errors: a `print!`-family macro panics on a closed or broken descriptor, and
// a panic mid-relaunch would lose the session.
#![cfg_attr(not(test), deny(clippy::print_stdout, clippy::print_stderr))]

pub mod account;
pub mod aterm;
pub mod aterm_wire;
pub mod boxes;
pub mod cli;
pub mod duty;
pub mod folder_trust;
pub mod inner_pty;
pub mod inspect;
pub mod journal;
pub mod json;
pub mod launch;
pub mod limit;
pub mod model_facts;
pub mod permission;
pub mod prime;
pub mod procs;
pub mod profile;
pub mod quiet;
pub mod relaunch;
pub mod repo;
pub mod repo_probe;
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
pub mod walls;
