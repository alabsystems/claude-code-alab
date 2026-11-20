// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;
use std::os::unix::fs::DirBuilderExt as _;
use std::sync::atomic::{AtomicU64, Ordering};

#[test]
fn a_status_counts_changes_and_untracked_paths_and_skips_ignored_ones() {
    // A modified file, a rename (its source in the next field), an untracked
    // file and an ignored one, as `git status --porcelain=v1 -z` writes them.
    let status = " M src/lib.rs\0R  new.rs\0old.rs\0?? notes.txt\0!! target/\0";
    let work = RepoWork::of_git("/w/repo\n", status, "", 0, 0, "").expect("work found");
    assert_eq!(work.repo, "/w/repo");
    assert_eq!(work.changed, 2);
    assert_eq!(work.untracked, 1);
    // The rename's source is no path of its own, and the ignored one counts nothing.
    assert_eq!(work.paths, ["src/lib.rs", "new.rs", "notes.txt"]);
    assert_eq!(
        work.summary(),
        "/w/repo: 2 changed, 1 untracked (src/lib.rs, new.rs, notes.txt)"
    );
    // Negative control: only ignored paths, no stash, nothing unpushed is nothing.
    assert_eq!(
        RepoWork::of_git("/w/repo", "!! target/\0", "", 0, 0, ""),
        None
    );
}

#[test]
fn the_summary_names_stashes_and_commits_no_remote_main_has() {
    let stash = stashes_on(
        "WIP on main: 1 one\nOn main: two\nWIP on other: 3 three\n",
        Some("main"),
    );
    let work = RepoWork::of_git("/w/repo", "", &stash, 1, 2, "origin/main").expect("work found");
    assert_eq!(
        work.summary(),
        "/w/repo: 2 stashes, 1 unpushed commit, 2 commits not on origin/main"
    );
    // More paths than it names end in an ellipsis.
    let status = "?? a\0?? b\0?? c\0?? d\0";
    let work = RepoWork::of_git("/w/repo", status, "", 0, 0, "").expect("work found");
    assert_eq!(work.summary(), "/w/repo: 4 untracked (a, b, c, \u{2026})");
}

#[test]
fn a_stash_counts_only_on_heads_branch() {
    let list = "WIP on main: abc one\nOn feature: two\nWIP on (no branch): def three\n";
    assert_eq!(stashes_on(list, Some("main")), "WIP on main: abc one\n");
    assert_eq!(stashes_on(list, Some("feature")), "On feature: two\n");
    assert_eq!(stashes_on(list, None), "WIP on (no branch): def three\n");
    // Negative control: a branch whose name another opens is not that branch.
    assert_eq!(stashes_on(list, Some("mai")), "");
}

#[test]
fn only_a_clean_reading_lets_a_land_count_and_the_rest_ask_to_land_it() {
    assert!(RepoRead::Clean.is_clean());
    assert_eq!(RepoRead::Clean.ask(), None);
    let work = RepoWork::of_git("/w/repo", "?? wip.txt\0", "", 0, 0, "").expect("work found");
    let found = RepoRead::Found(work);
    assert!(!found.is_clean());
    assert_eq!(
        found.ask().as_deref(),
        Some(
            "Your repository still holds work no remote main has (/w/repo: 1 untracked \
             (wip.txt)): commit, merge and push it to the remote main first."
        )
    );
    assert!(!RepoRead::Unread.is_clean());
    assert_eq!(RepoRead::Unread.ask().as_deref(), Some(REPO_UNREAD));
    // A long finding is cut to its first SUMMARY_CHARS characters.
    let status: String = (0..40)
        .map(|n| format!("?? {}{n}\0", "p".repeat(60)))
        .collect();
    let long = RepoWork::of_git(&"r".repeat(70), &status, "", 0, 0, "").expect("work found");
    let ask = RepoRead::Found(long).ask().expect("an ask");
    let quoted = ask
        .strip_prefix("Your repository still holds work no remote main has (")
        .and_then(|rest| {
            rest.strip_suffix("): commit, merge and push it to the remote main first.")
        })
        .expect("the finding quoted");
    assert_eq!(quoted.chars().count(), SUMMARY_CHARS);
    assert!(quoted.ends_with('\u{2026}'));
}

#[test]
fn the_folders_macos_guards_are_never_read() {
    let home = Path::new("/homes/someone");
    for guarded_path in [
        "/homes/someone/Documents",
        "/homes/someone/Desktop/project",
        "/homes/someone/Downloads/x/y",
        "/homes/someone/Library/Mobile Documents/com~apple~CloudDocs/p",
        "/homes/someone/Library/CloudStorage/Dropbox/p",
        "/Volumes/External/p",
        "/homes/someone/work/../Documents/p",
    ] {
        assert!(guarded(home, Path::new(guarded_path)), "{guarded_path}");
    }
    // Negative controls: an ordinary folder, a name that only opens a guarded
    // one, and the rest of the Library.
    for open in [
        "/homes/someone",
        "/homes/someone/Developer/p",
        "/homes/someone/Documentsx",
        "/homes/someone/Library/Caches/p",
        "/homes/other/Documents",
        "/tmp/p",
    ] {
        assert!(!guarded(home, Path::new(open)), "{open}");
    }
    // macOS's volumes alias names by case.
    assert_eq!(
        guarded(home, Path::new("/homes/someone/documents/p")),
        cfg!(target_os = "macos")
    );
}

/// A fresh scratch folder for one test.
fn scratch(name: &str) -> PathBuf {
    static SERIAL: AtomicU64 = AtomicU64::new(0);
    let parent = std::env::temp_dir().join(".ubd");
    std::fs::create_dir_all(&parent).expect("scratch parent");
    let root = parent.join(format!(
        "claude-alab-repo-{name}-{}-{}-{}.noindex",
        std::process::id(),
        crate::supervise::now_ms(),
        SERIAL.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(&root)
        .expect("an exclusive scratch folder");
    root.canonicalize().expect("the scratch folder's real path")
}

/// Run `git <args>` in `dir` with `home` as its whole configuration, and
/// insist it succeeds.
fn git(dir: &Path, home: &Path, args: &[&str]) {
    let path = std::env::var_os("PATH").expect("PATH");
    let program = git_program(&path).expect("a git that may run here");
    let output = Command::new(program)
        .current_dir(dir)
        .env_clear()
        .env("PATH", &path)
        .env("HOME", home)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .args([
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.invalid",
            "-c",
            "init.defaultBranch=main",
            "-c",
            "commit.gpgsign=false",
        ])
        .args(args)
        .stdout(std::process::Stdio::null())
        .output()
        .unwrap_or_else(|error| panic!("cannot run git {args:?} in {}: {error}", dir.display()));
    assert!(
        output.status.success(),
        "git {args:?} in {} returned {}: {}",
        dir.display(),
        output.status,
        String::from_utf8_lossy(&output.stderr).trim_end()
    );
}

/// What one probe of `folders` reads, each read given a minute.
fn probed<P: AsRef<Path>>(folders: &[P], home: &Path) -> Probed {
    let folders: Vec<PathBuf> = folders
        .iter()
        .map(|folder| folder.as_ref().to_path_buf())
        .collect();
    probe(&folders, Some(home), Duration::from_secs(60))
}

/// The finding of one probe of `folders`, which must find something.
fn found<P: AsRef<Path>>(folders: &[P], home: &Path) -> RepoWork {
    match probed(folders, home) {
        Probed::Read(RepoRead::Found(found)) => found,
        other => panic!("{other:?}"),
    }
}

const CLEAN: Probed = Probed::Read(RepoRead::Clean);

#[test]
fn the_probe_finds_what_no_remote_main_has_and_only_then() {
    let root = scratch("probe");
    let home = root.join("home");
    let work = root.join("work");
    let remote = root.join("remote.git");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&work).unwrap();
    let probe_work = || probed(&[&work], &home);
    // Negative control: a folder in no repository reads clean.
    assert_eq!(probe_work(), CLEAN);
    git(&work, &home, &["init", "--quiet"]);
    std::fs::write(work.join("a.txt"), "a").unwrap();
    let first = found(&[&work], &home);
    assert_eq!((first.untracked, first.changed), (1, 0), "{first:?}");
    assert_eq!(first.paths, ["a.txt"]);
    git(&work, &home, &["add", "a.txt"]);
    git(&work, &home, &["commit", "--quiet", "-m", "a"]);
    // Committed, with no remote anywhere: nowhere to push it, nothing found.
    assert_eq!(probe_work(), CLEAN);
    git(&root, &home, &["init", "--quiet", "--bare", "remote.git"]);
    let remote_url = remote.to_str().expect("a UTF-8 scratch path");
    git(&work, &home, &["remote", "add", "origin", remote_url]);
    // A remote that holds none of it: the commit is unpushed.
    assert_eq!(found(&[&work], &home).unpushed, 1);
    git(&work, &home, &["push", "--quiet", "origin", "main"]);
    assert_eq!(probe_work(), CLEAN);
    // A branch pushed but not on the remote main is still work to land.
    git(&work, &home, &["checkout", "--quiet", "-b", "topic"]);
    std::fs::write(work.join("b.txt"), "b").unwrap();
    git(&work, &home, &["add", "b.txt"]);
    git(&work, &home, &["commit", "--quiet", "-m", "b"]);
    git(&work, &home, &["push", "--quiet", "origin", "topic"]);
    let topic = found(&[&work], &home);
    assert_eq!((topic.unpushed, topic.unlanded), (0, 1), "{topic:?}");
    assert_eq!(topic.main_ref, "origin/main");
    // A stash made on the branch counts.
    git(&work, &home, &["checkout", "--quiet", "main"]);
    std::fs::write(work.join("a.txt"), "changed").unwrap();
    git(&work, &home, &["stash", "--quiet"]);
    let stashed = found(&[&work], &home);
    assert_eq!((stashed.stashes, stashed.changed), (1, 0), "{stashed:?}");
    git(&work, &home, &["stash", "drop", "--quiet"]);
    assert_eq!(probe_work(), CLEAN);
    // A probe whose first read's limit has already passed reads nothing:
    // cut, to be read again, never a reading.
    assert_eq!(
        probe(std::slice::from_ref(&work), Some(&home), Duration::ZERO),
        Probed::Cut
    );
    // A folder with no HOME beside it, or with no git that may run, cannot be
    // read: unread, never clean. Negative control: the same folder with both
    // holds what it holds.
    std::fs::write(work.join("c.txt"), "c").unwrap();
    let unread = Probed::Read(RepoRead::Unread);
    let minute = Duration::from_secs(60);
    assert_eq!(probe(std::slice::from_ref(&work), None, minute), unread);
    assert_eq!(
        probe(std::slice::from_ref(&work), Some(Path::new("home")), minute),
        unread
    );
    let nowhere = OsString::from(root.join("no-git-here"));
    assert_eq!(
        probe_on(
            std::slice::from_ref(&work),
            Some(&home),
            Some(nowhere),
            minute
        ),
        unread
    );
    assert_eq!(
        probe_on(std::slice::from_ref(&work), Some(&home), None, minute),
        unread
    );
    assert_eq!(found(&[&work], &home).untracked, 1);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn every_repository_the_session_works_in_counts_and_its_submodules_unpushed_commits() {
    let root = scratch("walk");
    let home = root.join("home");
    std::fs::create_dir_all(&home).unwrap();
    // Two repositories, each with a remote holding its main.
    let repo = |name: &str| {
        let work = root.join(name);
        let remote = root.join(format!("{name}.git"));
        std::fs::create_dir_all(&work).unwrap();
        git(
            &root,
            &home,
            &["init", "--quiet", "--bare", &format!("{name}.git")],
        );
        git(&work, &home, &["init", "--quiet"]);
        std::fs::write(work.join("a.txt"), name).unwrap();
        git(&work, &home, &["add", "a.txt"]);
        git(&work, &home, &["commit", "--quiet", "-m", name]);
        let url = remote.to_str().expect("a UTF-8 scratch path").to_string();
        git(&work, &home, &["remote", "add", "origin", &url]);
        git(&work, &home, &["push", "--quiet", "origin", "main"]);
        (work, url)
    };
    let (first, _) = repo("first");
    let (second, _) = repo("second");
    let (_, lib_url) = repo("lib");
    // CONTROL: every repository clean, a gone folder and one in no
    // repository passed over.
    let gone = root.join("gone");
    let plain = root.join("plain");
    std::fs::create_dir_all(&plain).unwrap();
    assert_eq!(probed(&[&gone, &plain, &first, &second], &home), CLEAN);
    // Work in the second folder is found though the first is clean.
    std::fs::write(second.join("b.txt"), "b").unwrap();
    let left = found(&[&first, &second], &home);
    assert!(left.repo.ends_with("second"), "{left:?}");
    assert_eq!(left.untracked, 1);
    std::fs::remove_file(second.join("b.txt")).unwrap();
    // A submodule whose pin is committed and pushed in its superproject but
    // whose own commit no remote has.
    git(
        &first,
        &home,
        &[
            "-c",
            "protocol.file.allow=always",
            "submodule",
            "add",
            "--quiet",
            &lib_url,
            "lib",
        ],
    );
    git(&first, &home, &["commit", "--quiet", "-m", "lib"]);
    git(&first, &home, &["push", "--quiet", "origin", "main"]);
    assert_eq!(probed(&[&first], &home), CLEAN);
    let sub = first.join("lib");
    std::fs::write(sub.join("c.txt"), "c").unwrap();
    git(&sub, &home, &["add", "c.txt"]);
    git(&sub, &home, &["commit", "--quiet", "-m", "c"]);
    git(&first, &home, &["add", "lib"]);
    git(&first, &home, &["commit", "--quiet", "-m", "pin"]);
    git(&first, &home, &["push", "--quiet", "origin", "main"]);
    let unpushed = found(&[&first], &home);
    assert!(unpushed.repo.ends_with("lib"), "{unpushed:?}");
    assert_eq!(
        (unpushed.unpushed, unpushed.unlanded),
        (1, 0),
        "{unpushed:?}"
    );
    // A folder inside the submodule reaches its superproject too, and the
    // submodule's own commit is work its remote's main lacks.
    let inner = found(&[&sub], &home);
    assert!(inner.repo.ends_with("lib"), "{inner:?}");
    git(&sub, &home, &["push", "--quiet", "origin", "HEAD:main"]);
    assert_eq!(probed(&[&first], &home), CLEAN);
    assert_eq!(probed(&[&sub], &home), CLEAN);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn the_transcript_names_the_folders_the_session_changed_newest_first() {
    let rows = [
        r#"{"type":"user","cwd":"/w/old","message":{"content":"hi"}}"#,
        r#"{"type":"assistant","cwd":"/w/old","message":{"content":[{"type":"tool_use","name":"Edit","input":{"file_path":"/w/lib/src/a.rs"}}]}}"#,
        r#"{"type":"assistant","cwd":"/w/new","message":{"content":[{"type":"tool_use","name":"Read","input":{"file_path":"/w/read-only/x.rs"}},{"type":"tool_use","name":"NotebookEdit","input":{"notebook_path":"/w/nb/n.ipynb"}}]}}"#,
        r#"{"type":"assistant","cwd":"relative","message":{"content":[{"type":"tool_use","name":"Write","input":{"file_path":"relative/x"}}]}}"#,
        r#"{"type":"assistant","cwd":"/w/repo","message":{"content":[{"type":"tool_use","name":"Bash","input":{"command":"git worktree add -b topic ../wt main && cd ../wt && git commit -m x && git -C '/w/other' status; git -C /w/pushed push; ls $HOME"}}]}}"#,
        r#"{"type":"assistant","cwd":"/w/peer","message":{"content":[{"type":"tool_use","name":"Bash","input":{"command":"cd /w/peer-wt && git status && git log"}}]}}"#,
        r#"not json, "tool_use""#,
    ];
    let tail = rows.join("\n");
    let folders = session_folders(&tail);
    let names: Vec<&str> = folders
        .iter()
        .map(|folder| folder.to_str().expect("UTF-8"))
        .collect();
    // Negative controls: a row's own folder, a folder a command only went
    // into or read with git, a tool that changes nothing, a relative path and
    // a line that is no row name no folder.
    assert_eq!(names, ["/w/pushed", "/w/wt", "/w/nb", "/w/lib/src"]);
    let many: Vec<String> = (0..SESSION_FOLDERS + 5)
        .map(|n| {
            format!(
                r#"{{"cwd":"/w","message":{{"content":[{{"type":"tool_use","name":"Write","input":{{"file_path":"/w/{n}/f"}}}}]}}}}"#
            )
        })
        .collect();
    let folders = session_folders(&many.join("\n"));
    assert_eq!(folders.len(), SESSION_FOLDERS);
    assert_eq!(
        folders.first().map(PathBuf::as_path),
        Some(Path::new(&format!("/w/{}", SESSION_FOLDERS + 4)))
    );
}

#[test]
fn a_command_names_the_folders_it_made_git_work_in_and_none_it_only_read() {
    let cwd = Path::new("/w/repo");
    let named = |command: &str| -> Vec<String> {
        git_work_folders(command, Some(cwd))
            .iter()
            .map(|folder| folder.to_string_lossy().into_owned())
            .collect()
    };
    assert_eq!(
        named("cd /a/./b/../c; git commit -m x | head && (cd \"../q\" && git push)"),
        ["/a/c", "/a/q"]
    );
    assert_eq!(
        named("git -C sub -c core.x=y merge topic; /usr/bin/git stash"),
        ["/w/repo/sub", "/w/repo"]
    );
    assert_eq!(
        named("git worktree add --detach -B x '/w/wt' main"),
        ["/w/wt"]
    );
    assert_eq!(named("git -C /w/side worktree add ../wt2"), ["/w/wt2"]);
    // Negative controls: a folder only gone into or read with git, a `-C`
    // that is not git's, a word that only ends in `git`, what only a shell
    // can expand (and a relative folder after it), an option, and a relative
    // folder with no cwd name nothing.
    assert!(
        named("cd /w/peer && git status && git -C /w/x log; tar -C /x; digit commit").is_empty()
    );
    assert!(named("cd $HOME && git commit; cd ~/x; cd *; cd -").is_empty());
    assert!(git_work_folders("git commit -m x", None).is_empty());
    assert!(git_work_folders("cd sub && git commit", None).is_empty());
}

#[test]
fn a_probes_answer_crosses_its_pipe_as_one_line_and_nothing_else_is_read() {
    let work = RepoWork::of_git(
        "/w/repo",
        " M a.rs\0?? b.rs\0?? c.rs\0?? d.rs\0",
        "",
        2,
        3,
        "origin/main",
    )
    .expect("work");
    for probed in [
        CLEAN,
        Probed::Read(RepoRead::Unread),
        Probed::Cut,
        Probed::Read(RepoRead::Found(work.clone())),
    ] {
        let line = probed.line();
        assert!(!line.contains('\n'), "{line:?}");
        assert_eq!(Probed::of_line(&format!("{line}\n")), Some(probed));
    }
    // A text field that held a tab or a line break is made printable.
    let mut odd = work;
    odd.repo = "/w/a\tb\nc".to_string();
    match Probed::of_line(&Probed::Read(RepoRead::Found(odd)).line()) {
        Some(Probed::Read(RepoRead::Found(read))) => assert_eq!(read.repo, "/w/a?b?c"),
        other => panic!("{other:?}"),
    }
    // Negative controls: nothing else is read as an answer.
    for line in [
        "",
        "Clean",
        "found",
        "found\t/w\t0\t0\t0\t0\t0\t",
        "found\t/w\tx\t0\t0\t0\t0\t",
        "found\t/w\t1\t0\t0\t0\t0\t\ta\tb\tc\td",
    ] {
        assert_eq!(Probed::of_line(line), None, "{line:?}");
    }
}
