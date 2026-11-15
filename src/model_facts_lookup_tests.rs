// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

const A: &str = "00000000-0000-4000-8000-0000000001a1";
const B: &str = "00000000-0000-4000-8000-0000000001b1";

fn bounded_look(reader: &mut Reader, world: &World, id: &str, now: &str, steps: usize) {
    let process = Process {
        pid: PID,
        record: Some((id, ms(START).unwrap())),
        args: &[],
    };
    reader.look_with_budget(&world.config, &process, ms(now).unwrap(), steps);
}

fn unread(reader: &Reader) {
    assert_eq!(words(reader), (Said::Unread, Said::Unread, Said::Unread));
}

#[test]
fn an_incomplete_lookup_is_unread_paced_and_cannot_accept_a_partial_prefix() {
    let world = World::new("bounded-lookup");
    world.write(
        A,
        &[&answer_at(
            "claude-opus-5-5",
            "2026-09-28T15:01:00Z",
            Some("high"),
        )],
    );
    std::fs::create_dir(world.config.join("projects/another")).unwrap();
    let mut reader = Reader::default();
    bounded_look(&mut reader, &world, A, "2026-09-28T15:01:01Z", 2);
    unread(&reader);
    assert_eq!(session(&reader).lookups, 1);
    bounded_look(&mut reader, &world, A, "2026-09-28T15:01:01.500Z", 3);
    unread(&reader);
    assert_eq!(
        session(&reader).lookups,
        1,
        "a refusal keeps the existing retry pace"
    );
    bounded_look(&mut reader, &world, A, "2026-09-28T15:01:02Z", 3);
    assert_eq!(words(&reader).0, is("Opus 5.5"));
    assert_eq!(words(&reader).1, is("high"));
    assert_eq!(session(&reader).lookups, 2);
    world.append(
        A,
        &body(&[&answer_at(
            "claude-opus-6",
            "2026-09-28T15:01:03Z",
            Some("low"),
        )]),
    );
    bounded_look(&mut reader, &world, A, "2026-09-28T15:01:04Z", 0);
    assert_eq!(words(&reader).0, is("Opus 6"));
    assert_eq!(words(&reader).1, is("low"));
    assert_eq!(
        session(&reader).lookups,
        2,
        "the known path spends no lookup budget"
    );
}

#[test]
fn a_conversation_switch_shares_one_budget_including_after_an_image_move() {
    let world = World::new("shared-lookup-budget");
    world.write(
        A,
        &[&answer_at(
            "claude-opus-5-5",
            "2026-09-28T15:00:10Z",
            Some("high"),
        )],
    );
    world.write(
        B,
        &[&answer_at(
            "claude-sonnet-5-5",
            "2026-09-28T15:01:10Z",
            Some("low"),
        )],
    );
    let mut original = Reader::default();
    bounded_look(&mut original, &world, A, "2026-09-28T15:01:00Z", 2);
    assert_eq!(words(&original).0, is("Opus 5.5"));
    let mut carried = moved(&original);
    let mut enough = moved(&original);
    // Carry deliberately does not keep a path. Its left conversation consumes both
    // attempts (one entry and EOF), leaving no authority to select B from a new scan.
    bounded_look(&mut carried, &world, B, "2026-09-28T15:01:20Z", 2);
    unread(&carried);
    let after_refusal = moved(&carried);
    unread(&after_refusal);
    bounded_look(&mut enough, &world, B, "2026-09-28T15:01:20Z", 4);
    assert_eq!(words(&enough).0, is("Sonnet 5.5"));
    assert_eq!(words(&enough).1, is("low"));
    bounded_look(&mut original, &world, B, "2026-09-28T15:01:20Z", 2);
    assert_eq!(
        words(&original),
        words(&enough),
        "a cached left path needs no discovery"
    );
}

#[test]
fn true_absence_keeps_a_clear_but_a_read_refusal_never_believes_carried_facts() {
    let world = World::new("lookup-absence");
    world.write(
        A,
        &[&answer_at(
            "claude-opus-5-5",
            "2026-09-28T15:00:10Z",
            Some("high"),
        )],
    );
    let mut original = Reader::default();
    bounded_look(&mut original, &world, A, "2026-09-28T15:01:00Z", 2);
    let before = words(&original);
    let mut incomplete = original.clone();
    bounded_look(&mut original, &world, B, "2026-09-28T15:01:10Z", 2);
    assert_eq!(
        words(&original),
        before,
        "a complete lookup found no new transcript"
    );
    bounded_look(&mut incomplete, &world, B, "2026-09-28T15:01:10Z", 1);
    unread(&incomplete);
    let mut unreadable = moved(&original);
    std::fs::rename(
        world.config.join("projects"),
        world.config.join("retained-projects"),
    )
    .unwrap();
    std::fs::write(world.config.join("projects"), b"not a directory").unwrap();
    bounded_look(&mut unreadable, &world, B, "2026-09-28T15:01:20Z", 2);
    unread(&unreadable);
    let mut other = Process {
        pid: PID + 1,
        record: Some((A, ms(START).unwrap())),
        args: &[],
    };
    std::fs::remove_file(world.config.join("projects")).unwrap();
    std::fs::create_dir(world.config.join("projects")).unwrap();
    unreadable.look_with_budget(
        &world.config,
        &other,
        ms("2026-09-28T15:01:30Z").unwrap(),
        2,
    );
    assert_eq!(
        words(&unreadable),
        (Said::Unsaid, Said::Unsaid, Said::Unsaid)
    );
    other.pid = PID;
    unreadable.look_with_budget(
        &world.config,
        &other,
        ms("2026-09-28T15:01:40Z").unwrap(),
        2,
    );
    assert_eq!(
        words(&unreadable),
        (Said::Unsaid, Said::Unsaid, Said::Unsaid)
    );
}
