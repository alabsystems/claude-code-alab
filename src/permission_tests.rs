// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

fn frame(mode: &str) -> Vec<String> {
    vec![
        "● Done".into(),
        String::new(),
        "─".repeat(60),
        "❯ ".into(),
        "─".repeat(60),
        mode.into(),
    ]
}

#[test]
fn every_drawn_mode_names_the_exact_vendor_setting_and_footprint() {
    for (row, mode, setting) in [
        (
            "  ⏵⏵ bypass permissions on (shift+tab to cycle) · ← for agents",
            Mode::Bypass,
            "bypassPermissions",
        ),
        ("  ⏵⏵ auto mode on (shift+tab to cycle)", Mode::Auto, "auto"),
        ("  ⏵⏵ accept edits on", Mode::AcceptEdits, "acceptEdits"),
        ("  ⏵⏵ don't ask on", Mode::DontAsk, "dontAsk"),
        ("  ⏸ plan mode on", Mode::Plan, "plan"),
        ("  ⏸ manual mode on", Mode::Default, "default"),
    ] {
        assert_eq!(
            pill(&frame(row)),
            Some(Pill {
                mode,
                rows: 2..6,
                mode_row: 5,
            }),
            "{row}"
        );
        assert_eq!(mode.setting(), setting);
        assert_eq!(Mode::of_setting(setting), Some(mode));
    }
    assert_eq!(Mode::of_setting("manual"), Some(Mode::Default));
    assert_eq!(Mode::of_setting("unknown"), None);
}

#[test]
fn narrow_cycle_hints_and_vim_insert_keep_the_complete_mode_words() {
    for row in [
        "  ⏵⏵ auto mode on (shif",
        "  ⏵⏵ auto mode on (shift+tab to",
        "  -- INSERT -- ⏵⏵ auto mode on (shift+tab to cycle)",
    ] {
        assert_eq!(pill(&frame(row)).map(|p| p.mode), Some(Mode::Auto));
    }
    for row in [
        "  ⏵⏵ auto mode o",
        "  ⏵⏵ auto mode only",
        "  ⏸ plan mo",
        "  ⏵⏵ turbo mode on",
    ] {
        assert_eq!(pill(&frame(row)), None, "{row}");
    }
}

#[test]
fn body_text_and_mode_words_without_a_complete_composer_grant_no_reading() {
    let mut rows = frame("  ⏸ manual mode on");
    rows[0] = "  ⏵⏵ bypass permissions on (shift+tab to cycle)".into();
    assert_eq!(pill(&rows).map(|p| p.mode), Some(Mode::Default));
    for remove in [2, 3, 4] {
        let mut cut = frame("  ⏵⏵ bypass permissions on");
        cut.remove(remove);
        assert_eq!(pill(&cut), None, "removed frame row {remove}");
    }
    assert_eq!(pill(&["  ⏵⏵ bypass permissions on".into()]), None);
    let mut separated = frame("  ⏵⏵ bypass permissions on");
    separated.insert(5, String::new());
    assert_eq!(pill(&separated), None);
}

#[test]
fn unknown_first_pill_and_inconsistent_frame_cannot_fall_back_to_a_known_mode() {
    let mut rows = frame("  ⏵⏵ turbo mode on");
    rows.push("  ⏵⏵ bypass permissions on".into());
    assert_eq!(pill(&rows), None);
    let mut rows = frame("  ⏵⏵ bypass permissions on");
    rows[2] = "─".repeat(59);
    assert_eq!(pill(&rows), None);
    rows[2] = format!("{}┬{}", "─".repeat(29), "─".repeat(30));
    assert_eq!(pill(&rows), None);
    rows[2] = "─".repeat(60);
    rows[3] = "  ❯ a list entry".into();
    assert_eq!(pill(&rows), None);
}
