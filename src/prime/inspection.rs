// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Read-only interpretation beside the initializer's single settings schema.

use super::*;

pub(crate) enum DefaultState {
    Default,
    Manual(String),
    HeldForEffort(String),
    HeldForRate(String),
    HeldForOffer,
    OverridesEffort(String),
    Unset,
}

pub(crate) struct Setting {
    pub key: &'static str,
    pub expected: String,
    pub state: DefaultState,
    pub meaning: Meaning,
}

#[derive(Clone, Copy)]
pub(crate) enum Meaning {
    TopEffort,
    Effort,
    SessionSurveys,
    AllSurveys,
    Drafts,
    CompactWindow,
}

fn env_true(value: &Json) -> bool {
    let text = value.as_str().map_or_else(|| value.render(), str::to_owned);
    matches!(
        text.trim().to_ascii_lowercase().as_str(),
        "1" | "true" | "yes" | "on"
    )
}

fn value<'a>(doc: &'a Json, key: &str) -> Option<&'a Json> {
    match key.split_once('.') {
        Some((parent, child)) => doc.get(parent)?.get(child),
        None => doc.get(key),
    }
}

pub(crate) fn settings_state(doc: &Json, hold_window: bool) -> Result<Vec<Setting>, String> {
    if doc.as_object().is_none() {
        return Err("the top level is not a JSON object".into());
    }
    if doc.get("env").is_some_and(|env| env.as_object().is_none()) {
        return Err("its env is not a JSON object".into());
    }
    let efforts = efforts_set_by_hand(doc);
    let effort = efforts
        .iter()
        .map(|(key, v)| format!("{} = {v}", settings_json::string(key)))
        .collect::<Vec<_>>()
        .join(", ");
    let rate = survey_rate_set_by_hand(doc);
    let survey_off = matches!(doc.get(SURVEY_RATE_KEY), Some(Json::Number(n)) if n.parse::<f64>().is_ok_and(|n| n == 0.0))
        || value(doc, CLAUDE_SURVEY_SWITCH).is_some_and(env_true);
    Ok(CLAUDE_DEFAULTS.iter().map(|&(key, want, meaning)| {
        let have = value(doc, key);
        let state = match have {
            None if key == SURVEY_RATE_KEY && survey_off => DefaultState::Default,
            None if held_default(key, !efforts.is_empty(), rate.is_some(), hold_window) => {
                if CLAUDE_EFFORT_DEFAULTS.contains(&key) {
                    DefaultState::HeldForEffort(effort.clone())
                } else if key == CLAUDE_SURVEY_SWITCH {
                    DefaultState::HeldForRate(rate.clone().unwrap_or_default())
                } else {
                    DefaultState::HeldForOffer
                }
            }
            None => DefaultState::Unset,
            Some(have) => {
                let text = have.render();
                if key == CLAUDE_TOP_EFFORT_KEY && have == &Json::Bool(true) && !efforts.is_empty() {
                    DefaultState::OverridesEffort(effort.clone())
                } else if (key == CLAUDE_SURVEY_SWITCH && env_true(have))
                    || have == &want.json()
                    || (key == SURVEY_RATE_KEY && matches!(have, Json::Number(n) if n.parse::<f64>().is_ok_and(|n| n == 0.0)))
                {
                    DefaultState::Default
                } else {
                    DefaultState::Manual(text)
                }
            }
        };
        Setting { key, expected: want.json().render(), state, meaning }
    }).collect())
}

pub(crate) fn would_add(doc: &Json, hold_window: bool) -> bool {
    let effort = !efforts_set_by_hand(doc).is_empty();
    let rate = survey_rate_set_by_hand(doc).is_some();
    CLAUDE_DEFAULTS.iter().any(|(key, _, _)| {
        value(doc, key).is_none() && !held_default(key, effort, rate, hold_window)
    })
}
