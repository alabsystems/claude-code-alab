// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Owner-authored diagnostic wording; the host does not interpret vendor settings.

use super::{Class, Disposition, Finding, Remedy};
use crate::prime::{DefaultState, Meaning, Setting};
use std::borrow::Cow;

fn meaning(value: Meaning) -> (&'static str, &'static str) {
    match value {
        Meaning::TopEffort => ("Top-effort mode", "top-effort mode on"),
        Meaning::Effort => ("Effort level", "xhigh effort"),
        Meaning::SessionSurveys => ("Session-quality survey", "session-quality survey off"),
        Meaning::AllSurveys => ("Rating surveys", "all rating surveys off"),
        Meaning::Drafts => ("Drafted-feedback card", "drafted-feedback card quiet"),
        Meaning::CompactWindow => ("Autocompact window", "autocompact at 400k tokens"),
    }
}

/// Quote controls visibly, including C1 and Unicode line separators. The native
/// paths are carried separately as hex, never converted into display text.
pub(super) fn safe(text: Cow<'_, str>) -> String {
    use std::fmt::Write as _;
    let needs_quote = |ch: char| ch.is_control() || matches!(ch, '\u{2028}' | '\u{2029}');
    if !text.chars().any(needs_quote) {
        return text.into_owned();
    }
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        if needs_quote(ch) {
            write!(out, "\\u{{{:x}}}", ch as u32).expect("writing into String");
        } else {
            out.push(ch);
        }
    }
    out
}

pub(super) fn unavailable(key: &str, detail: &str) -> Finding {
    Finding {
        key: key.into(),
        disposition: Disposition::Unavailable,
        class: Class::Person,
        label: "Claude Code's defaults".into(),
        detail: safe(detail.into()),
        expected: None,
        remedy: Remedy::None,
    }
}

pub(super) fn setting(setting: Setting, auto_prime: bool, offer: Option<&str>) -> Finding {
    let Setting {
        key,
        expected,
        state,
        meaning: purpose,
    } = setting;
    let (label, explanation) = meaning(purpose);
    let mut finding = Finding {
        key: key.into(),
        disposition: Disposition::Ok,
        class: Class::Person,
        label: format!("Claude Code: {label}"),
        detail: String::new(),
        expected: Some(safe(expected.as_str().into())),
        remedy: Remedy::None,
    };
    finding.detail = match state {
        DefaultState::Default => {
            finding.class = Class::Preference;
            format!("{key} uses its default ({expected}): {explanation}")
        }
        DefaultState::Manual(value) if matches!(purpose, Meaning::AllSurveys) => format!(
            "{key} is set by hand to {value}; kept. This switch does not turn rating surveys off; Claude Code recognizes only 1, true, yes or on"
        ),
        DefaultState::Manual(value) => format!("{key} is set by hand to {value}; kept"),
        DefaultState::HeldForEffort(effort) => {
            format!("{key} stays unset because an effort is set by hand ({effort})")
        }
        DefaultState::HeldForRate(rate) => {
            format!("{key} stays unset because the survey rate is set by hand ({rate})")
        }
        DefaultState::HeldForOffer => format!(
            "{key} stays unset because {}",
            offer.unwrap_or("the autocompact offer cannot be read")
        ),
        DefaultState::OverridesEffort(effort) => {
            finding.disposition = Disposition::Change;
            finding.expected = Some("unset".into());
            finding.remedy = Remedy::Edit {
                instruction: format!(
                    "remove {key} from the selected settings file to use the chosen effort"
                ),
            };
            format!("{key} overrides the effort set by hand ({effort})")
        }
        DefaultState::Unset if !auto_prime => {
            format!("{key} stays unset because automatic initialization is off")
        }
        DefaultState::Unset => {
            finding.disposition = Disposition::Change;
            finding.class = Class::Preference;
            format!("{key} is unset; automatic initialization adds {expected} at its next pass")
        }
    };
    finding.detail = safe(finding.detail.into());
    finding
}
