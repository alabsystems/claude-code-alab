// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

#[test]
fn the_built_duties_are_every_one_aterm_names_in_its_order() {
    assert_eq!(Duties::default(), Duties::BUILT);
    assert_eq!(Duties::BUILT.scopes(), Scope::ALL);
    assert_eq!(
        Duties::BUILT.words(),
        "relaunch,upgrade,turn-end,approvals,questions,walls"
    );
    assert_eq!(
        claim_verb(
            "s-0000000000004d7a2a97",
            4242,
            &Duties::BUILT.scopes(),
            "0.3.0",
            None
        )
        .as_deref(),
        Some(
            "@s-0000000000004d7a2a97 meta set supervisor claude-code-alab@4242 contract=3 \
             scope=relaunch,upgrade,turn-end,approvals,questions,walls version=0.3.0"
        )
    );
    // Negative control: a claim that leaves walls out performs none, and says
    // so by not naming them.
    let without = Duties::parse("relaunch,upgrade,turn-end,approvals,questions").unwrap();
    assert!(!without.takes(Scope::Walls));
    assert!(Duties::BUILT.takes(Scope::Walls));
}

#[test]
fn each_typing_duty_is_switched_on_by_its_own_word_and_the_two_that_type_nothing_always_are() {
    let base = [Scope::Relaunch, Scope::Upgrade];
    assert_eq!(Duties::ALWAYS.scopes(), base);
    for words in [
        "relaunch,upgrade",
        "relaunch",
        "upgrade",
        "upgrade,relaunch",
    ] {
        assert_eq!(Duties::parse(words), Some(Duties::ALWAYS), "{words}");
    }
    assert_eq!(
        Duties::parse("turn-end").map(Duties::scopes),
        Some(vec![Scope::Relaunch, Scope::Upgrade, Scope::TurnEnd])
    );
    // Approvals and questions are each their own word: one without the other
    // performs only that one.
    assert_eq!(
        Duties::parse("approvals").map(Duties::scopes),
        Some(vec![Scope::Relaunch, Scope::Upgrade, Scope::Approvals])
    );
    assert_eq!(
        Duties::parse("questions,relaunch").map(Duties::scopes),
        Some(vec![Scope::Relaunch, Scope::Upgrade, Scope::Questions])
    );
    // Walls are their own word too, alone and beside a duty that types
    // nothing: the same set.
    assert_eq!(
        Duties::parse("walls").map(Duties::scopes),
        Some(vec![Scope::Relaunch, Scope::Upgrade, Scope::Walls])
    );
    assert_eq!(Duties::parse("walls,relaunch"), Duties::parse("walls"));
    // Any order, a word twice: the same set.
    assert_eq!(
        Duties::parse("questions,walls,turn-end,relaunch,approvals,upgrade,turn-end"),
        Some(Duties::BUILT)
    );
    // Negative controls: an empty list or word, a word aterm does not spell
    // so.
    for words in [
        "",
        ",",
        "relaunch,",
        "relaunch,,upgrade",
        "wall",
        "Walls",
        "relaunch,walls ",
        "turn_end",
        "approval",
        "Relaunch",
        " relaunch",
    ] {
        assert_eq!(Duties::parse(words), None, "{words:?}");
    }
}

#[test]
fn a_duty_set_crosses_as_one_byte_and_only_a_built_one_comes_back() {
    for words in [
        "relaunch",
        "upgrade,turn-end",
        "relaunch,upgrade,turn-end",
        "approvals",
        "turn-end,questions",
        "walls",
        "relaunch,upgrade,turn-end,approvals,questions",
        "relaunch,upgrade,turn-end,approvals,questions,walls",
    ] {
        let duties = Duties::parse(words).unwrap();
        assert_eq!(Duties::from_bits(duties.bits()), Some(duties), "{words}");
    }
    // A byte from a build whose set could leave the two out comes back with
    // them.
    assert_eq!(
        Duties::from_bits(Scope::Walls.bit()),
        Duties::parse("walls")
    );
    // Negative controls: nothing, and a bit past every duty aterm names.
    assert_eq!(Duties::from_bits(0), None);
    assert_eq!(Duties::from_bits(Duties::BUILT.bits() | 0x40), None);
    assert_eq!(Duties::from_bits(0x80), None);
}

/// A claim names the typing duties only while the session's Autopilot is a
/// word the harness types under; under any other it names only the duties
/// that type nothing, which every set holds, so it is never empty: the vendor
/// update, a crash's relaunch and the move off Fable's limit are named in
/// every claim, whatever word it is made under (under `off` aterm's policy
/// admits none, `supervise_link::admits`).
#[test]
fn a_claim_names_its_typing_duties_only_under_cruise_or_land_and_the_rest_always() {
    let typing = [
        Scope::TurnEnd,
        Scope::Approvals,
        Scope::Questions,
        Scope::Walls,
    ];
    for mode in [Autopilot::Cruise, Autopilot::Land] {
        assert_eq!(Duties::BUILT.under(Some(mode)), Scope::ALL, "{mode:?}");
    }
    // Negative controls: `off`, `-` (no agent admitted yet) and a host that
    // states no word name no typing duty, and keep the two that type nothing.
    for mode in [Some(Autopilot::Off), None] {
        let named = Duties::BUILT.under(mode);
        assert_eq!(named, [Scope::Relaunch, Scope::Upgrade], "{mode:?}");
        assert!(
            !named.iter().any(|scope| typing.contains(scope)),
            "{mode:?}"
        );
    }
    // A list of typing duties alone still claims the two that type nothing
    // while the mode types nothing.
    let boxes = Duties::parse("approvals,questions").unwrap();
    assert!(boxes.types());
    assert_eq!(boxes.under(None), [Scope::Relaunch, Scope::Upgrade]);
    assert_eq!(
        boxes.under(Some(Autopilot::Land)),
        [
            Scope::Relaunch,
            Scope::Upgrade,
            Scope::Approvals,
            Scope::Questions
        ]
    );
    // Every set this build can hold, under every word: never empty, and
    // always the two.
    for bits in 1..=Duties::BUILT.bits() {
        let Some(duties) = Duties::from_bits(bits) else {
            continue;
        };
        for mode in [
            Some(Autopilot::Cruise),
            Some(Autopilot::Land),
            Some(Autopilot::Off),
            None,
        ] {
            let named = duties.under(mode);
            assert!(
                named.starts_with(&[Scope::Relaunch, Scope::Upgrade]),
                "{bits:#b} {mode:?}: {named:?}"
            );
        }
    }
    // A set that types nothing is the same set under every mode.
    let base = Duties::parse("relaunch,upgrade").unwrap();
    assert!(!base.types());
    for mode in [Some(Autopilot::Cruise), Some(Autopilot::Off), None] {
        assert_eq!(base.under(mode), [Scope::Relaunch, Scope::Upgrade]);
    }
}
