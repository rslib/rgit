use std::path::Path;
use std::process::Command;

use rgit_git::status_walk::UntrackedMode;
use rgit_git::{FileDiff, Git2Backend, GitBackend, StatusCode, StatusEntry, StatusTriple};

fn scratch(name: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!("rgit-it-walk-{}-{name}", std::process::id()))
}

fn init_repo(name: &str) -> std::path::PathBuf {
    let dir = scratch(name);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    for args in [
        vec!["init", "-q", "-b", "main"],
        vec!["config", "user.email", "t@example.com"],
        vec!["config", "user.name", "test"],
        vec!["config", "commit.gpgsign", "false"],
    ] {
        assert!(
            Command::new("git")
                .arg("-C")
                .arg(&dir)
                .args(&args)
                .status()
                .unwrap()
                .success(),
            "git {args:?} failed"
        );
    }
    dir
}

fn git(dir: &Path, args: &[&str]) {
    assert!(
        Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .status()
            .unwrap()
            .success(),
        "git {args:?} failed"
    );
}

fn entry(entries: &[StatusEntry], path: &str) -> StatusEntry {
    entries
        .iter()
        .find(|e| e.path == path)
        .unwrap_or_else(|| panic!("no entry for {path}; have {}", describe(entries)))
        .clone()
}

fn describe(entries: &[StatusEntry]) -> String {
    entries
        .iter()
        .map(|e| format!("{}:{:?}/{:?}", e.path, e.index, e.worktree))
        .collect::<Vec<_>>()
        .join(", ")
}

fn assert_diffs_eq(single: &[FileDiff], sharded: &[FileDiff], side: &str) {
    assert_eq!(
        single.len(),
        sharded.len(),
        "{side}: len {} != {}: single={single:?} sharded={sharded:?}",
        single.len(),
        sharded.len()
    );
    for (a, b) in single.iter().zip(sharded.iter()) {
        assert_eq!(a.path, b.path, "{side}: path order");
        assert_eq!(a.old_path, b.old_path, "{side} {}: old_path", a.path);
        assert_eq!(a.status, b.status, "{side} {}: status", a.path);
        assert_eq!(a.similarity, b.similarity, "{side} {}: similarity", a.path);
        assert_eq!(a.binary, b.binary, "{side} {}: binary", a.path);
        assert_eq!(a.header, b.header, "{side} {}: header", a.path);
        assert_eq!(a.sizes, b.sizes, "{side} {}: sizes", a.path);
        assert_eq!(a.modes, b.modes, "{side} {}: modes", a.path);
        assert_eq!(a.ids, b.ids, "{side} {}: ids", a.path);
        assert_eq!(a.loaded, b.loaded, "{side} {}: loaded", a.path);
        assert_eq!(a.hunks, b.hunks, "{side} {}: hunks", a.path);
    }
}

fn assert_triple_eq(single: &StatusTriple, sharded: &StatusTriple) {
    assert_eq!(
        single.entries,
        sharded.entries,
        "entries differ: single=[{}] sharded=[{}]",
        describe(&single.entries),
        describe(&sharded.entries)
    );
    assert_diffs_eq(&single.unstaged, &sharded.unstaged, "unstaged");
    assert_diffs_eq(&single.staged, &sharded.staged, "staged");
}

fn check_dir(dir: &Path, context: &str) {
    let gitdir = dir.join(".git");
    check_pair(dir, &gitdir, None, context);
    for full in [false, true] {
        let sharded = rgit_git::status_walk::debug_sharded(
            &gitdir,
            Some(dir),
            None,
            UntrackedMode::Normal,
            full,
        )
        .unwrap();
        let backend = Git2Backend::discover(dir).unwrap();
        let live = if full {
            backend.status_full().unwrap()
        } else {
            backend.status().unwrap()
        };
        assert_eq!(
            live.entries, sharded.entries,
            "{context} backend entries differ (full={full})"
        );
        assert_diffs_eq(
            &sharded.unstaged,
            &live.unstaged,
            &format!("{context} backend unstaged"),
        );
        assert_diffs_eq(
            &sharded.staged,
            &live.staged,
            &format!("{context} backend staged"),
        );
    }
}

fn check_pair(dir: &Path, gitdir: &Path, index_file: Option<&Path>, context: &str) {
    for mode in [UntrackedMode::Normal, UntrackedMode::All, UntrackedMode::No] {
        for full in [false, true] {
            let single =
                rgit_git::status_walk::debug_single(gitdir, Some(dir), index_file, mode, full)
                    .unwrap_or_else(|e| panic!("{context} single({mode:?}, full={full}): {e}"));
            let sharded =
                rgit_git::status_walk::debug_sharded(gitdir, Some(dir), index_file, mode, full)
                    .unwrap_or_else(|e| panic!("{context} sharded({mode:?}, full={full}): {e}"));
            assert_triple_eq(&single, &sharded);
        }
    }
}

#[test]
fn mixed_changes_match() {
    let dir = init_repo("mixed");
    std::fs::create_dir_all(dir.join("a")).unwrap();
    std::fs::create_dir_all(dir.join("b")).unwrap();
    let body = "alpha\nbeta\ngamma\ndelta\nepsilon\nzeta\neta\ntheta\n";
    std::fs::write(dir.join("a/f1.txt"), "one\n").unwrap();
    std::fs::write(dir.join("a/gone.txt"), body).unwrap();
    std::fs::write(dir.join("b/f2.txt"), "two\n").unwrap();
    std::fs::write(dir.join("m.txt"), "move me\n").unwrap();
    git(&dir, &["add", "."]);
    git(&dir, &["commit", "-q", "-m", "seed"]);

    std::fs::write(dir.join("a/f1.txt"), "one\nmore\n").unwrap();
    std::fs::write(dir.join("b/f2.txt"), "two\nmore\n").unwrap();
    std::fs::remove_file(dir.join("a/gone.txt")).unwrap();
    std::fs::write(dir.join("b/copy.txt"), body).unwrap();
    std::fs::write(dir.join("b/staged.txt"), "staged\n").unwrap();
    git(&dir, &["add", "b/staged.txt"]);
    git(&dir, &["mv", "m.txt", "b/moved.txt"]);

    let single = rgit_git::status_walk::debug_single(
        &dir.join(".git"),
        Some(&dir),
        None,
        UntrackedMode::All,
        false,
    )
    .unwrap();
    assert_eq!(
        entry(&single.entries, "a/f1.txt").worktree,
        StatusCode::Modified
    );
    assert_eq!(
        entry(&single.entries, "b/f2.txt").worktree,
        StatusCode::Modified
    );
    assert_eq!(
        entry(&single.entries, "a/gone.txt").worktree,
        StatusCode::Deleted
    );
    assert_eq!(
        entry(&single.entries, "b/copy.txt").worktree,
        StatusCode::Untracked
    );
    assert_eq!(
        entry(&single.entries, "b/staged.txt").index,
        StatusCode::Added
    );
    let renamed = entry(&single.entries, "b/moved.txt");
    assert_eq!(renamed.index, StatusCode::Renamed);
    assert_eq!(renamed.orig_path.as_deref(), Some("m.txt"));
    check_dir(&dir, "mixed");
}

#[test]
fn nested_gitignore_matches() {
    let dir = init_repo("ignore");
    std::fs::create_dir_all(dir.join("a/nested")).unwrap();
    std::fs::create_dir_all(dir.join("b")).unwrap();
    std::fs::write(dir.join("a/nested/.gitignore"), "*.log\n").unwrap();
    std::fs::write(dir.join("a/nested/keep.txt"), "keep\n").unwrap();
    std::fs::write(dir.join("a/nested/skip.log"), "skip\n").unwrap();
    std::fs::write(dir.join("b/f.txt"), "b\n").unwrap();
    git(&dir, &["add", "."]);
    git(&dir, &["commit", "-q", "-m", "seed"]);
    std::fs::write(dir.join("a/nested/keep.txt"), "keep\nmore\n").unwrap();
    std::fs::write(dir.join("a/nested/new.log"), "new\n").unwrap();
    std::fs::write(dir.join("a/nested/new.txt"), "new\n").unwrap();
    std::fs::write(dir.join("b/f.txt"), "b\nmore\n").unwrap();

    let single = rgit_git::status_walk::debug_single(
        &dir.join(".git"),
        Some(&dir),
        None,
        UntrackedMode::All,
        false,
    )
    .unwrap();
    assert!(
        single.entries.iter().all(|e| !e.path.ends_with(".log")),
        "ignored files stay out: [{}]",
        describe(&single.entries)
    );
    assert_eq!(
        entry(&single.entries, "a/nested/keep.txt").worktree,
        StatusCode::Modified
    );
    check_dir(&dir, "ignore");
}

#[test]
fn root_files_only_match() {
    let dir = init_repo("root-only");
    std::fs::write(dir.join("a.txt"), "a\n").unwrap();
    std::fs::write(dir.join("b.txt"), "b\n").unwrap();
    std::fs::write(dir.join("c.txt"), "c\n").unwrap();
    git(&dir, &["add", "."]);
    git(&dir, &["commit", "-q", "-m", "seed"]);
    std::fs::write(dir.join("a.txt"), "a\nmore\n").unwrap();
    std::fs::remove_file(dir.join("b.txt")).unwrap();
    std::fs::write(dir.join("d.txt"), "d\n").unwrap();
    git(&dir, &["add", "a.txt"]);

    let single = rgit_git::status_walk::debug_single(
        &dir.join(".git"),
        Some(&dir),
        None,
        UntrackedMode::All,
        false,
    )
    .unwrap();
    assert_eq!(entry(&single.entries, "a.txt").index, StatusCode::Modified);
    assert_eq!(
        entry(&single.entries, "b.txt").worktree,
        StatusCode::Deleted
    );
    assert_eq!(
        entry(&single.entries, "d.txt").worktree,
        StatusCode::Untracked
    );
    check_dir(&dir, "root-only");
}

#[test]
fn intent_to_add_rename_matches() {
    let dir = init_repo("ita-rename");
    std::fs::create_dir_all(dir.join("a")).unwrap();
    let body = "alpha\nbeta\ngamma\ndelta\nepsilon\nzeta\neta\ntheta\n";
    std::fs::write(dir.join("old.txt"), body).unwrap();
    std::fs::write(dir.join("a/keep.txt"), "keep\n").unwrap();
    git(&dir, &["add", "."]);
    git(&dir, &["commit", "-q", "-m", "seed"]);
    std::fs::write(dir.join("new.txt"), body).unwrap();
    git(&dir, &["add", "-N", "new.txt"]);
    std::fs::remove_file(dir.join("old.txt")).unwrap();

    check_dir(&dir, "ita-rename");
}

#[test]
fn separate_gitdir_matches() {
    let dir = init_repo("separate");
    std::fs::create_dir_all(dir.join("a")).unwrap();
    std::fs::write(dir.join("a/f.txt"), "f\n").unwrap();
    std::fs::write(dir.join("r.txt"), "r\n").unwrap();
    git(&dir, &["add", "."]);
    git(&dir, &["commit", "-q", "-m", "seed"]);
    let gitdir = scratch("separate-git");
    let _ = std::fs::remove_dir_all(&gitdir);
    std::fs::rename(dir.join(".git"), &gitdir).unwrap();
    std::fs::write(dir.join(".git"), format!("gitdir: {}\n", gitdir.display())).unwrap();
    std::fs::write(dir.join("a/f.txt"), "f\nmore\n").unwrap();
    std::fs::write(dir.join("new.txt"), "new\n").unwrap();

    check_pair(&dir, &gitdir, None, "separate");
}

#[test]
fn second_index_matches() {
    let dir = init_repo("second-index");
    std::fs::create_dir_all(dir.join("a")).unwrap();
    std::fs::write(dir.join("a/f.txt"), "f\n").unwrap();
    std::fs::write(dir.join("r.txt"), "r\n").unwrap();
    git(&dir, &["add", "."]);
    git(&dir, &["commit", "-q", "-m", "seed"]);
    std::fs::write(dir.join("r.txt"), "r\nmore\n").unwrap();
    let index_file = dir.join("second-index");
    std::fs::copy(dir.join(".git/index"), &index_file).unwrap();
    assert!(
        Command::new("git")
            .arg("-C")
            .arg(&dir)
            .env("GIT_INDEX_FILE", &index_file)
            .args(["add", "r.txt"])
            .status()
            .unwrap()
            .success()
    );

    check_pair(&dir, &dir.join(".git"), Some(&index_file), "second-index");
    let single = rgit_git::status_walk::debug_single(
        &dir.join(".git"),
        Some(&dir),
        Some(&index_file),
        UntrackedMode::Normal,
        false,
    )
    .unwrap();
    assert_eq!(
        entry(&single.entries, "r.txt").index,
        StatusCode::Modified,
        "the second index stages r.txt: [{}]",
        describe(&single.entries)
    );
}

#[test]
fn empty_repo_matches() {
    let dir = init_repo("empty");
    let single = rgit_git::status_walk::debug_single(
        &dir.join(".git"),
        Some(&dir),
        None,
        UntrackedMode::All,
        false,
    )
    .unwrap();
    assert!(single.entries.is_empty());
    check_dir(&dir, "empty");
}

#[test]
fn submodule_matches() {
    let sub = init_repo("submod-src");
    std::fs::write(sub.join("lib.txt"), "lib\n").unwrap();
    git(&sub, &["add", "."]);
    git(&sub, &["commit", "-q", "-m", "sub seed"]);

    let dir = init_repo("submod");
    std::fs::create_dir_all(dir.join("a")).unwrap();
    std::fs::write(dir.join("a/f.txt"), "f\n").unwrap();
    git(&dir, &["add", "."]);
    git(&dir, &["commit", "-q", "-m", "seed"]);
    let status = Command::new("git")
        .arg("-C")
        .arg(&dir)
        .args(["-c", "protocol.file.allow=always", "submodule", "add", "-q"])
        .arg(&sub)
        .arg("vendor/sub")
        .status()
        .unwrap()
        .success();
    assert!(status, "submodule add failed");
    git(&dir, &["commit", "-q", "-m", "add sub"]);
    std::fs::write(dir.join("a/f.txt"), "f\nmore\n").unwrap();
    std::fs::write(sub.join("lib.txt"), "lib\nmore\n").unwrap();

    let single = rgit_git::status_walk::debug_single(
        &dir.join(".git"),
        Some(&dir),
        None,
        UntrackedMode::All,
        false,
    )
    .unwrap();
    assert!(
        single.entries.iter().any(|e| e.path == "a/f.txt"),
        "fixture has a worktree change: [{}]",
        describe(&single.entries)
    );
    check_dir(&dir, "submodule");
}

#[test]
fn sparse_checkout_matches() {
    let dir = init_repo("sparse");
    std::fs::create_dir_all(dir.join("a")).unwrap();
    std::fs::create_dir_all(dir.join("b")).unwrap();
    std::fs::write(dir.join("a/f.txt"), "a\n").unwrap();
    std::fs::write(dir.join("b/g.txt"), "b\n").unwrap();
    git(&dir, &["add", "."]);
    git(&dir, &["commit", "-q", "-m", "seed"]);
    git(&dir, &["sparse-checkout", "init", "--cone"]);
    git(&dir, &["sparse-checkout", "set", "a"]);
    std::fs::write(dir.join("a/f.txt"), "a\nmore\n").unwrap();

    let single = rgit_git::status_walk::debug_single(
        &dir.join(".git"),
        Some(&dir),
        None,
        UntrackedMode::All,
        false,
    )
    .unwrap();
    assert_eq!(
        entry(&single.entries, "a/f.txt").worktree,
        StatusCode::Modified
    );
    check_dir(&dir, "sparse");
}

#[test]
fn many_untracked_match() {
    let dir = init_repo("many-untracked");
    std::fs::create_dir_all(dir.join("a")).unwrap();
    std::fs::create_dir_all(dir.join("b")).unwrap();
    std::fs::write(dir.join("a/f.txt"), "f\n").unwrap();
    git(&dir, &["add", "."]);
    git(&dir, &["commit", "-q", "-m", "seed"]);
    for i in 0..120 {
        std::fs::write(dir.join(format!("a/new-{i}.txt")), "new\n").unwrap();
    }
    for i in 0..120 {
        std::fs::write(dir.join(format!("b/new-{i}.txt")), "new\n").unwrap();
    }
    std::fs::write(dir.join("a/f.txt"), "f\nmore\n").unwrap();

    let single = rgit_git::status_walk::debug_single(
        &dir.join(".git"),
        Some(&dir),
        None,
        UntrackedMode::All,
        false,
    )
    .unwrap();
    assert!(
        single.entries.iter().filter(|e| e.is_untracked()).count() > 200,
        "fixture passes EAGER_UNTRACKED"
    );
    assert_eq!(
        entry(&single.entries, "a/f.txt").worktree,
        StatusCode::Modified
    );
    check_dir(&dir, "many-untracked");
}
