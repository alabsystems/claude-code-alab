// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Where a launch looks for the vendor's store: the prefix this harness is
//! installed under, read off its own executable, before the one under the
//! launch's `HOME`, for a launch the launcher handed over. Each has a control
//! that reads the home's store alone.

use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};

use super::*;

/// A fresh folder for one test, its links resolved (the temporary folder is
/// behind `/var` on macOS), in a `.ubd` folder with a `.noindex` name.
fn scratch(name: &str) -> PathBuf {
    let root = std::env::temp_dir().join(".ubd").join(format!(
        "claude-alab-prefix-{name}-{}-{}.noindex",
        std::process::id(),
        line!()
    ));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).expect("a scratch folder");
    root.canonicalize().expect("the scratch folder resolved")
}

/// An executable file at `path` holding `text`.
fn executable(path: &Path, text: &str) {
    std::fs::create_dir_all(path.parent().expect("its folder")).expect("mkdir");
    std::fs::write(path, text).expect("written");
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
}

/// This harness installed as build `number` under `prefix`: its executable.
fn harness_at(prefix: &Path, number: u64) -> PathBuf {
    let exe = prefix
        .join("store")
        .join(crate::self_image::PACKAGE)
        .join(number.to_string())
        .join("bin")
        .join(crate::self_image::COMMAND);
    executable(&exe, "harness\n");
    exe
}

/// A usable vendor build `number` under `prefix`, named by `current`.
fn vendor_at(prefix: &Path, number: u64) {
    let store = Store::new(prefix, "claude", "claude");
    executable(
        &store.program_dir.join(format!("{number}/bin/claude")),
        "vendor\n",
    );
    std::fs::write(store.program_dir.join(format!("{number}.ready")), "ok\n").expect("ready");
    std::os::unix::fs::symlink(
        store.program_dir.join(number.to_string()),
        store.current_link(),
    )
    .expect("current");
}

#[test]
fn the_installed_prefix_is_read_off_a_harness_laid_out_in_its_store() {
    let exe = Path::new("/p/store/claude-code-alab/12/bin/claude-alab");
    assert_eq!(installed_prefix_of(exe), Some(PathBuf::from("/p")));
    // CONTROLS: anything else laid out names no prefix.
    for other in [
        "/p/store/claude-code-alab/12/bin/claude",
        "/p/store/claude/12/bin/claude-alab",
        "/p/store/claude-code-alab/012/bin/claude-alab",
        "/p/store/claude-code-alab/current/bin/claude-alab",
        "/p/store/claude-code-alab/12/sbin/claude-alab",
        "/p/stores/claude-code-alab/12/bin/claude-alab",
        "/p/bin/claude-alab",
        "/target/debug/claude-alab",
        "claude-alab",
    ] {
        assert_eq!(installed_prefix_of(Path::new(other)), None, "{other}");
    }
}

#[test]
fn a_link_to_an_installed_harness_names_its_prefix() {
    let root = scratch("link");
    let prefix = root.join("pkg");
    let exe = harness_at(&prefix, 3);
    // The launcher's hand-off runs `<prefix>/bin/claude-alab`, which leads
    // into the store.
    let shim = prefix.join("bin").join(crate::self_image::COMMAND);
    std::fs::create_dir_all(shim.parent().expect("bin")).expect("mkdir");
    std::os::unix::fs::symlink(&exe, &shim).expect("a link");
    assert_eq!(installed_prefix(&shim), Some(prefix.clone()));
    assert_eq!(installed_prefix(&exe), Some(prefix));
    // CONTROL: a copy that is not in a store names none, and nor does a
    // path that leads nowhere.
    let copy = root.join("elsewhere").join(crate::self_image::COMMAND);
    executable(&copy, "harness\n");
    assert_eq!(installed_prefix(&copy), None);
    assert_eq!(installed_prefix(&root.join("gone")), None);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_handed_over_launch_reads_the_installed_prefix_first() {
    let root = scratch("order");
    let home = root.join("home");
    let installed = root.join("pkg");
    let exe = harness_at(&installed, 3);
    assert_eq!(
        vendor_prefixes(true, Some(&exe), &home),
        [installed, default_prefix(&home)]
    );
    // CONTROLS: a direct launch, a hand-off with no own executable or one
    // not in a store, and a hand-off whose installed prefix is the home's
    // own, read the home's store alone.
    assert_eq!(
        vendor_prefixes(false, Some(&exe), &home),
        [default_prefix(&home)]
    );
    assert_eq!(vendor_prefixes(true, None, &home), [default_prefix(&home)]);
    let copy = root.join("elsewhere").join(crate::self_image::COMMAND);
    executable(&copy, "harness\n");
    assert_eq!(
        vendor_prefixes(true, Some(&copy), &home),
        [default_prefix(&home)]
    );
    let at_home = harness_at(&default_prefix(&home), 4);
    assert_eq!(
        vendor_prefixes(true, Some(&at_home), &home),
        [default_prefix(&home)]
    );
    // The same folder through a link to the home is read once, by the
    // home's path.
    let linked_home = root.join("linked-home");
    std::os::unix::fs::symlink(&home, &linked_home).expect("a link");
    assert_eq!(
        vendor_prefixes(true, Some(&at_home), &linked_home),
        [default_prefix(&linked_home)]
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_launch_under_another_home_runs_the_launchers_vendor_build() {
    let root = scratch("find");
    let installed = root.join("pkg");
    let exe = harness_at(&installed, 3);
    vendor_at(&installed, 7);
    // `HOME=/scratch claude`: no store under that home.
    let home = root.join("scratch-home");
    let prefixes = vendor_prefixes(true, Some(&exe), &home);
    let (prefix, store, build) =
        find_vendor(&prefixes, "claude", "claude", Some(&exe)).expect("the launcher's build");
    assert_eq!(prefix, installed);
    assert_eq!(store.program_dir, installed.join("store").join("claude"));
    assert_eq!(build.number, 7);
    // A usable build under the home is not read first in a hand-off.
    vendor_at(&default_prefix(&home), 9);
    let (_, _, build) = find_vendor(&prefixes, "claude", "claude", Some(&exe)).expect("a build");
    assert_eq!(build.number, 7);
    // CONTROL: a direct launch reads the home's store alone.
    let direct = vendor_prefixes(false, Some(&exe), &home);
    let (prefix, _, build) = find_vendor(&direct, "claude", "claude", Some(&exe)).expect("a build");
    assert_eq!((prefix, build.number), (default_prefix(&home), 9));
    // CONTROL: where neither store has a usable build, the first one's
    // refusal is the answer.
    let nowhere = root.join("nowhere");
    let none = [nowhere.join("a"), nowhere.join("b")];
    let (prefix, missing) =
        find_vendor(&none, "claude", "claude", Some(&exe)).expect_err("no build");
    assert_eq!(prefix, none[0]);
    assert_eq!(
        missing,
        Missing::NotInstalled(none[0].join("store").join("claude").join("current"))
    );
    let (prefix, missing) = find_vendor(&[], "claude", "claude", Some(&exe)).expect_err("none");
    assert_eq!(prefix, PathBuf::new());
    assert_eq!(missing, Missing::NotInstalled(PathBuf::new()));
    let _ = std::fs::remove_dir_all(&root);
}
