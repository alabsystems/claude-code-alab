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
    let text = match value {
        Json::Str(text) | Json::Number(text) => text.trim(),
        Json::Bool(value) => return *value,
        Json::Null | Json::Array(_) | Json::Object(_) => return false,
    };
    ["1", "true", "yes", "on"]
        .iter()
        .any(|word| text.eq_ignore_ascii_case(word))
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
    let mut effort = String::new();
    let mut has_efforts = false;
    for (model, value) in manual_efforts(doc) {
        if has_efforts {
            effort.push_str(", ");
        }
        let key = model.map_or_else(
            || "effortLevel".to_string(),
            |model| format!("modelSettings.{model}.effortLevel"),
        );
        effort.push_str(&settings_json::string(&key));
        effort.push_str(" = ");
        effort.push_str(&value.render());
        has_efforts = true;
    }
    let rate = survey_rate_set_by_hand(doc).map(Json::render);
    let survey_off = matches!(doc.get(SURVEY_RATE_KEY), Some(Json::Number(n)) if n.parse::<f64>().is_ok_and(|n| n == 0.0))
        || value(doc, CLAUDE_SURVEY_SWITCH).is_some_and(env_true);
    Ok(CLAUDE_DEFAULTS.iter().map(|&(key, want, meaning)| {
        let have = value(doc, key);
        let expected = want.json();
        let state = match have {
            None if key == SURVEY_RATE_KEY && survey_off => DefaultState::Default,
            None if held_default(key, has_efforts, rate.is_some(), hold_window) => {
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
                if key == CLAUDE_TOP_EFFORT_KEY && have == &Json::Bool(true) && has_efforts {
                    DefaultState::OverridesEffort(effort.clone())
                } else if (key == CLAUDE_SURVEY_SWITCH && env_true(have))
                    || have == &expected
                    || (key == SURVEY_RATE_KEY && matches!(have, Json::Number(n) if n.parse::<f64>().is_ok_and(|n| n == 0.0)))
                {
                    DefaultState::Default
                } else {
                    DefaultState::Manual(have.render())
                }
            }
        };
        Setting { key, expected: expected.render(), state, meaning }
    }).collect())
}

pub(crate) fn would_add(doc: &Json, hold_window: bool) -> bool {
    let effort = manual_efforts(doc).next().is_some();
    let rate = survey_rate_set_by_hand(doc).is_some();
    CLAUDE_DEFAULTS.iter().any(|(key, _, _)| {
        value(doc, key).is_none() && !held_default(key, effort, rate, hold_window)
    })
}
