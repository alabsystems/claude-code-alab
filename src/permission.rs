// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Claude Code's permission pill in an original complete terminal frame.
//!
//! This is vendor grammar, ported from aterm's `lights::read_lights`,
//! `footer::pill_indicator` and the complete composer frame. A parsed pill is
//! text, not permission authority. Its original native process, context,
//! output epoch, input and repaint evidence must survive native admission;
//! a transcript row or a later status cannot supply those facts.

use std::ops::Range;

/// One permission mode Claude Code draws beneath its composer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// Ordinary permission prompts.
    Default,
    /// Automatic approval through the vendor's classifier.
    Auto,
    /// Automatic file edits.
    AcceptEdits,
    /// Refuse actions requiring a permission prompt.
    DontAsk,
    /// Planning without ordinary tool execution.
    Plan,
    /// Skip the vendor's permission checks.
    Bypass,
}

impl Mode {
    /// The vendor's exact `--permission-mode` value.
    #[must_use]
    pub const fn setting(self) -> &'static str {
        match self {
            Self::Default => "default",
            Self::Auto => "auto",
            Self::AcceptEdits => "acceptEdits",
            Self::DontAsk => "dontAsk",
            Self::Plan => "plan",
            Self::Bypass => "bypassPermissions",
        }
    }

    /// Parse a vendor setting, including its `manual` alias for `default`.
    #[must_use]
    pub fn of_setting(value: &str) -> Option<Self> {
        Some(match value {
            "default" | "manual" => Self::Default,
            "auto" => Self::Auto,
            "acceptEdits" => Self::AcceptEdits,
            "dontAsk" => Self::DontAsk,
            "plan" => Self::Plan,
            "bypassPermissions" => Self::Bypass,
            _ => return None,
        })
    }
}

/// A recognized pill and the exact frame rows its interpretation depends on.
/// This carries no native authority and must never be treated as a lease.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pill {
    /// The mode explicitly drawn by the vendor.
    pub mode: Mode,
    /// Composer top rule through the pill, inclusive, as a half-open range.
    pub rows: Range<usize>,
    /// The mode row, whose repaint must follow every relevant input.
    pub mode_row: usize,
}

fn rule(row: &str) -> bool {
    let row = row.trim_end();
    row.starts_with('─')
        && row.ends_with('─')
        && row.chars().filter(|c| *c == '─').count() >= 10
        && !row
            .chars()
            .any(|c| matches!(c, '┬' | '┴' | '┼' | '├' | '┤' | '┌' | '┐' | '└' | '┘' | '│'))
}

fn lead(row: &str) -> Option<&str> {
    let row = row.strip_prefix("  ")?;
    Some(row.strip_prefix("-- INSERT -- ").unwrap_or(row))
}

fn pill_mode(row: &str) -> Option<Mode> {
    let row = lead(row)?;
    [
        ("⏵⏵ bypass permissions on", Mode::Bypass),
        ("⏵⏵ auto mode on", Mode::Auto),
        ("⏵⏵ accept edits on", Mode::AcceptEdits),
        ("⏵⏵ don't ask on", Mode::DontAsk),
        ("⏸ plan mode on", Mode::Plan),
        ("⏸ manual mode on", Mode::Default),
    ]
    .into_iter()
    .find_map(|(words, mode)| {
        let after = row.strip_prefix(words)?;
        (after.is_empty() || after.starts_with(char::is_whitespace)).then_some(mode)
    })
}

/// Read the lowest complete column-zero Claude composer and its mode pill.
/// Unknown or truncated mode words, a box instead of a composer, and a blank
/// separating the composer from the footer remain unread. Narrow panes may
/// truncate the optional cycle hint after the complete mode words.
#[must_use]
pub fn pill(rows: &[String]) -> Option<Pill> {
    // The column-zero caret immediately under a rule is unambiguous even when
    // an option or quoted task elsewhere contains the same glyph. An indented
    // or incomplete frame remains unread rather than guessing a list entry.
    let caret = (1..rows.len())
        .rev()
        .find(|&at| rows[at].starts_with('❯') && rule(&rows[at - 1]))?;
    let top = caret - 1;
    let bottom = (caret + 1..rows.len()).find(|&at| rule(&rows[at]))?;
    if rows[top].trim_end().chars().count() != rows[bottom].trim_end().chars().count() {
        return None;
    }
    let mode_row = (bottom + 1..rows.len())
        .take_while(|&at| !rows[at].trim().is_empty())
        .find(|&at| {
            lead(&rows[at]).is_some_and(|text| text.starts_with("⏵⏵") || text.starts_with('⏸'))
        })?;
    Some(Pill {
        mode: pill_mode(&rows[mode_row])?,
        rows: top..mode_row + 1,
        mode_row,
    })
}

#[cfg(test)]
#[path = "permission_tests.rs"]
mod tests;
