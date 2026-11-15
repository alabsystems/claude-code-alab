// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;
use std::cell::Cell;
use std::os::unix::fs::symlink;
use std::sync::atomic::{AtomicU64, Ordering};

const SESSION: &str = "7c1e2a90-4b3d-4f6e-9a18-2d5c6b7e8f01";

struct World(PathBuf);

impl World {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(".ubd").join(format!(
            "claude-model-lookup-{}-{stamp}-{}.noindex",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(root.join("projects")).unwrap();
        Self(root)
    }

    fn project(&self, name: &str, transcript: bool) -> PathBuf {
        let project = self.0.join("projects").join(name);
        fs::create_dir(&project).unwrap();
        let path = project.join(format!("{SESSION}.jsonl"));
        if transcript {
            fs::write(&path, "{}\n").unwrap();
        }
        path
    }
}

impl Drop for World {
    fn drop(&mut self) {
        if !std::thread::panicking() {
            fs::remove_dir_all(&self.0).unwrap();
        }
    }
}

#[test]
fn a_budget_counts_every_entry_and_the_proof_that_enumeration_ended() {
    let world = World::new();
    let path = world.project("one", true);
    world.project("two", false);
    fs::write(world.0.join("projects/unrelated-file"), b"not a project").unwrap();
    assert_eq!(legacy(&world.0, SESSION), Some(path.clone()));
    for mut remaining in [0, 1, 2, 3] {
        assert_eq!(
            bounded(&world.0, SESSION, &mut remaining),
            Err(Refused::Incomplete)
        );
        assert_eq!(remaining, 0);
    }
    let mut remaining = 4;
    assert_eq!(bounded(&world.0, SESSION, &mut remaining), Ok(Some(path)));
    assert_eq!(remaining, 0);
    let visited = Cell::new(0);
    let entries = fs::read_dir(world.0.join("projects"))
        .unwrap()
        .inspect(|_| visited.set(visited.get() + 1));
    assert_eq!(
        find(entries, SESSION, Some(&mut 2)),
        Err(Refused::Incomplete)
    );
    assert_eq!(
        visited.get(),
        2,
        "no entry is pulled after the budget is spent"
    );
}

#[test]
fn an_ambiguous_or_interrupted_scan_never_accepts_its_first_match() {
    let world = World::new();
    let first = world.project("one", true);
    world.project("two", true);
    assert_eq!(bounded(&world.0, SESSION, &mut 4), Err(Refused::Ambiguous));
    assert_eq!(legacy(&world.0, SESSION), None);
    let entry = || {
        fs::read_dir(world.0.join("projects"))
            .unwrap()
            .map(Result::unwrap)
            .find(|entry| entry.path() == first.parent().unwrap())
            .unwrap()
    };
    let interrupted = || {
        std::iter::once(Ok(entry())).chain(std::iter::once(Err(io::Error::other(
            "directory read refused",
        ))))
    };
    assert_eq!(
        find(interrupted(), SESSION, Some(&mut 4)),
        Err(Refused::Unreadable)
    );
    assert_eq!(find(interrupted(), SESSION, None), Ok(Some(first)));
}

#[test]
fn missing_projects_are_absent_but_named_unreadable_sources_refuse() {
    let world = World::new();
    let projects = world.0.join("projects");
    fs::remove_dir(&projects).unwrap();
    assert_eq!(bounded(&world.0, SESSION, &mut 1), Ok(None));
    fs::write(&projects, b"not a directory").unwrap();
    assert_eq!(bounded(&world.0, SESSION, &mut 1), Err(Refused::Unreadable));
    assert_eq!(legacy(&world.0, SESSION), None);
    fs::remove_file(&projects).unwrap();
    symlink(world.0.join("missing"), &projects).unwrap();
    assert_eq!(bounded(&world.0, SESSION, &mut 1), Err(Refused::Unreadable));
    fs::remove_file(&projects).unwrap();
    fs::create_dir(&projects).unwrap();
    let path = world.project("one", false);
    symlink(world.0.join("missing-transcript"), &path).unwrap();
    assert_eq!(bounded(&world.0, SESSION, &mut 2), Err(Refused::Unreadable));
    assert_eq!(legacy(&world.0, SESSION), None);
    fs::remove_file(&path).unwrap();
    fs::create_dir(&path).unwrap();
    assert_eq!(bounded(&world.0, SESSION, &mut 2), Err(Refused::Unreadable));
}

#[test]
fn complete_lookups_keep_native_names_and_supported_transcript_links() {
    let world = World::new();
    let project = world
        .0
        .join("projects")
        .join("project-\u{03a9} with spaces");
    fs::create_dir(&project).unwrap();
    let path = project.join(format!("{SESSION}.jsonl"));
    let stored = world.0.join("stored.jsonl");
    fs::write(&stored, "{}\n").unwrap();
    symlink(&stored, &path).unwrap();
    assert_eq!(bounded(&world.0, SESSION, &mut 2), Ok(Some(path.clone())));
    assert_eq!(legacy(&world.0, SESSION), Some(path));
    let mut remaining = 0;
    assert_eq!(bounded(&world.0, "../../other", &mut remaining), Ok(None));
    assert_eq!(remaining, 0);
}
