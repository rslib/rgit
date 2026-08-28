use std::path::Path;
use std::process::Command;

use rgit_git::{Git2Backend, GitBackend, LogOptions, StatusCode};

/// A scratch directory in the system temp tree, outside the project so
/// `gix::discover`'s upward walk cannot reach the workspace's own repo.
fn scratch(name: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!("rgit-it-{}-{name}", std::process::id()))
}

/// Create an isolated repo and run `git` init/config in it. Each test uses a
/// distinct name.
fn init_repo(name: &str) -> std::path::PathBuf {
    let dir = scratch(name);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    for args in [
        vec!["init", "-q", "-b", "main"],
        vec!["config", "user.email", "t@example.com"],
        vec!["config", "user.name", "test"],
    ] {
        let ok = Command::new("git")
            .arg("-C")
            .arg(&dir)
            .args(&args)
            .status()
            .unwrap()
            .success();
        assert!(ok, "git {args:?} failed");
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
            .success()
    );
}

#[test]
fn unborn_repo_with_untracked_file() {
    let dir = init_repo("unborn");
    std::fs::write(dir.join("a.txt"), "hello\n").unwrap();

    let backend = Git2Backend::discover(&dir).unwrap();
    let status = backend.status().unwrap();

    assert_eq!(status.head.branch.as_deref(), Some("main"));
    assert_eq!(status.head.oid, None, "unborn branch has no commit");
    assert!(status.recent.is_empty());
    let untracked: Vec<_> = status.entries.iter().filter(|e| e.is_untracked()).collect();
    assert_eq!(untracked.len(), 1);
    assert_eq!(untracked[0].path, "a.txt");
}

#[test]
fn staged_and_unstaged_after_commit() {
    let dir = init_repo("staged-unstaged");
    std::fs::write(dir.join("a.txt"), "one\n").unwrap();
    git(&dir, &["add", "a.txt"]);
    git(&dir, &["commit", "-q", "-m", "add a"]);

    std::fs::write(dir.join("a.txt"), "one\ntwo\n").unwrap();
    git(&dir, &["add", "a.txt"]);
    std::fs::write(dir.join("a.txt"), "one\ntwo\nthree\n").unwrap();

    let backend = Git2Backend::discover(&dir).unwrap();
    let status = backend.status().unwrap();

    assert_eq!(status.head.summary.as_deref(), Some("add a"));
    assert!(status.head.oid.is_some());
    assert_eq!(status.recent.len(), 1);

    let e = status.entries.iter().find(|e| e.path == "a.txt").unwrap();
    assert!(e.is_staged());
    assert!(e.is_unstaged());
    assert_eq!(e.index, StatusCode::Modified);
    assert_eq!(e.worktree, StatusCode::Modified);
}

#[test]
fn diffs_carry_hunks_for_changed_files() {
    let dir = init_repo("diff-hunks");
    std::fs::write(dir.join("a.txt"), "one\ntwo\nthree\n").unwrap();
    git(&dir, &["add", "a.txt"]);
    git(&dir, &["commit", "-q", "-m", "seed"]);

    // one staged change, one unstaged change, in different files
    std::fs::write(dir.join("a.txt"), "one\nTWO\nthree\n").unwrap();
    std::fs::write(dir.join("b.txt"), "new file\n").unwrap();
    git(&dir, &["add", "b.txt"]);

    let backend = Git2Backend::discover(&dir).unwrap();
    let status = backend.status().unwrap();

    let unstaged = status
        .unstaged_diff("a.txt")
        .expect("unstaged diff for a.txt");
    assert_eq!(unstaged.hunks.len(), 1);
    let has_add = unstaged.hunks[0]
        .lines
        .iter()
        .any(|l| l.origin == rgit_git::LineOrigin::Added && l.text == "TWO");
    let has_del = unstaged.hunks[0]
        .lines
        .iter()
        .any(|l| l.origin == rgit_git::LineOrigin::Removed && l.text == "two");
    assert!(has_add && has_del, "hunk shows the line swap");

    let staged = status.staged_diff("b.txt").expect("staged diff for b.txt");
    assert!(
        staged
            .hunks
            .iter()
            .any(|h| h.lines.iter().any(|l| l.text == "new file"))
    );
}

#[test]
fn stage_and_unstage_a_whole_file() {
    let dir = init_repo("file-stage");
    std::fs::write(dir.join("a.txt"), "one\n").unwrap();
    git(&dir, &["add", "a.txt"]);
    git(&dir, &["commit", "-q", "-m", "seed"]);
    std::fs::write(dir.join("a.txt"), "one\ntwo\n").unwrap();

    let backend = Git2Backend::discover(&dir).unwrap();
    backend.stage_file("a.txt").unwrap();
    let st = backend.status().unwrap();
    assert!(
        st.entries
            .iter()
            .find(|e| e.path == "a.txt")
            .unwrap()
            .is_staged()
    );

    backend.unstage_file("a.txt").unwrap();
    let st = backend.status().unwrap();
    let e = st.entries.iter().find(|e| e.path == "a.txt").unwrap();
    assert!(!e.is_staged() && e.is_unstaged());
}

#[test]
fn stage_then_unstage_a_hunk_roundtrips() {
    let dir = init_repo("hunk-stage");
    std::fs::write(dir.join("a.txt"), "l1\nl2\nl3\n").unwrap();
    git(&dir, &["add", "a.txt"]);
    git(&dir, &["commit", "-q", "-m", "seed"]);
    std::fs::write(dir.join("a.txt"), "l1\nCHANGED\nl3\n").unwrap();

    let backend = Git2Backend::discover(&dir).unwrap();

    let st = backend.status().unwrap();
    let new_start = st.unstaged_diff("a.txt").unwrap().hunks[0].new_start;
    backend.stage_hunk("a.txt", new_start).unwrap();

    let st = backend.status().unwrap();
    assert!(st.unstaged_diff("a.txt").is_none(), "nothing left unstaged");
    let staged = st.staged_diff("a.txt").expect("hunk is now staged");

    backend
        .unstage_hunk("a.txt", staged.hunks[0].new_start)
        .unwrap();
    let st = backend.status().unwrap();
    assert!(st.staged_diff("a.txt").is_none(), "hunk unstaged again");
    assert!(st.unstaged_diff("a.txt").is_some());
}

#[test]
fn stage_only_selected_lines_of_a_hunk() {
    use rgit_git::LineOrigin::Added;
    let dir = init_repo("line-stage");
    std::fs::write(dir.join("a.txt"), "a\nb\nc\n").unwrap();
    git(&dir, &["add", "a.txt"]);
    git(&dir, &["commit", "-q", "-m", "seed"]);
    // Two independent additions in one hunk: X after a, Y after b.
    std::fs::write(dir.join("a.txt"), "a\nX\nb\nY\nc\n").unwrap();

    let backend = Git2Backend::discover(&dir).unwrap();
    let st = backend.status().unwrap();
    let hunk = &st.unstaged_diff("a.txt").unwrap().hunks[0];
    let x = hunk
        .lines
        .iter()
        .position(|l| l.origin == Added && l.text == "X")
        .unwrap();

    backend.stage_lines("a.txt", hunk.new_start, &[x]).unwrap();

    let st = backend.status().unwrap();
    let staged = st.staged_diff("a.txt").unwrap();
    let staged_adds: Vec<&str> = staged.hunks[0]
        .lines
        .iter()
        .filter(|l| l.origin == Added)
        .map(|l| l.text.as_str())
        .collect();
    assert_eq!(staged_adds, ["X"], "only the selected line is staged");

    let unstaged = st.unstaged_diff("a.txt").unwrap();
    let still: Vec<&str> = unstaged.hunks[0]
        .lines
        .iter()
        .filter(|l| l.origin == Added)
        .map(|l| l.text.as_str())
        .collect();
    assert_eq!(still, ["Y"], "the unselected line stays unstaged");
}

#[test]
fn stage_all_then_unstage_all() {
    let dir = init_repo("stage-all");
    std::fs::write(dir.join("a.txt"), "one\n").unwrap();
    git(&dir, &["add", "a.txt"]);
    git(&dir, &["commit", "-q", "-m", "seed"]);
    std::fs::write(dir.join("a.txt"), "one\ntwo\n").unwrap(); // modified
    std::fs::write(dir.join("b.txt"), "new\n").unwrap(); // untracked

    let backend = Git2Backend::discover(&dir).unwrap();
    backend.stage_all().unwrap();
    let st = backend.status().unwrap();
    assert!(st.entries.iter().all(|e| e.is_staged()));
    assert_eq!(st.staged.len(), 2, "both files staged");

    backend.unstage_all().unwrap();
    let st = backend.status().unwrap();
    assert!(st.staged.is_empty(), "nothing staged");
    assert!(
        st.entries
            .iter()
            .any(|e| e.path == "b.txt" && e.is_untracked())
    );
}

#[test]
fn discard_deletes_untracked_and_reverts_tracked() {
    let dir = init_repo("discard-file");
    std::fs::write(dir.join("kept.txt"), "orig\n").unwrap();
    git(&dir, &["add", "kept.txt"]);
    git(&dir, &["commit", "-q", "-m", "seed"]);

    std::fs::write(dir.join("kept.txt"), "orig\nmodified\n").unwrap(); // unstaged edit
    std::fs::write(dir.join("junk.txt"), "trash\n").unwrap(); // untracked

    let backend = Git2Backend::discover(&dir).unwrap();
    backend.discard_file("junk.txt").unwrap();
    backend.discard_file("kept.txt").unwrap();

    assert!(!dir.join("junk.txt").exists(), "untracked file deleted");
    assert_eq!(
        std::fs::read_to_string(dir.join("kept.txt")).unwrap(),
        "orig\n",
        "tracked file reverted to committed content"
    );
}

#[test]
fn discard_lines_reverts_only_selected_lines() {
    use rgit_git::LineOrigin::Added;
    let dir = init_repo("discard-lines");
    std::fs::write(dir.join("a.txt"), "a\nb\nc\n").unwrap();
    git(&dir, &["add", "a.txt"]);
    git(&dir, &["commit", "-q", "-m", "seed"]);
    std::fs::write(dir.join("a.txt"), "a\nX\nb\nY\nc\n").unwrap();

    let backend = Git2Backend::discover(&dir).unwrap();
    let st = backend.status().unwrap();
    let hunk = &st.unstaged_diff("a.txt").unwrap().hunks[0];
    let x = hunk
        .lines
        .iter()
        .position(|l| l.origin == Added && l.text == "X")
        .unwrap();

    backend
        .discard_lines("a.txt", hunk.new_start, &[x])
        .unwrap();

    assert_eq!(
        std::fs::read_to_string(dir.join("a.txt")).unwrap(),
        "a\nb\nY\nc\n",
        "only X was discarded; Y remains"
    );
}

#[test]
fn blame_attributes_lines_to_their_commits() {
    let dir = init_repo("blame");
    std::fs::write(dir.join("f.txt"), "a\nb\n").unwrap();
    git(&dir, &["add", "f.txt"]);
    git(&dir, &["commit", "-q", "-m", "first"]);
    std::fs::write(dir.join("f.txt"), "a\nB\nc\n").unwrap();
    git(&dir, &["add", "f.txt"]);
    git(&dir, &["commit", "-q", "-m", "second"]);

    let backend = Git2Backend::discover(&dir).unwrap();
    let blame = backend.blame("f.txt").unwrap();

    assert_eq!(blame.len(), 3);
    assert_eq!(blame[0].line, "a");
    assert_eq!(blame[2].line, "c");
    assert!(blame.iter().all(|b| !b.short_id.is_empty()));
    assert_ne!(
        blame[0].short_id, blame[1].short_id,
        "line a is from the first commit, line B from the second"
    );
}

#[test]
fn commit_details_carry_metadata_and_diff() {
    let dir = init_repo("commit-details");
    std::fs::write(dir.join("f.txt"), "one\n").unwrap();
    git(&dir, &["add", "f.txt"]);
    git(&dir, &["commit", "-q", "-m", "first"]);
    std::fs::write(dir.join("f.txt"), "one\ntwo\n").unwrap();
    git(&dir, &["add", "f.txt"]);
    git(&dir, &["commit", "-q", "-m", "add two"]);

    let backend = Git2Backend::discover(&dir).unwrap();
    let head_id = backend
        .log(&LogOptions {
            limit: 1,
            ..Default::default()
        })
        .unwrap()[0]
        .short_id
        .clone();
    let details = backend.commit_details(&head_id).unwrap();

    assert_eq!(details.message.trim(), "add two");
    let file = details.files.iter().find(|f| f.path == "f.txt").unwrap();
    assert!(
        file.hunks[0]
            .lines
            .iter()
            .any(|l| l.origin == rgit_git::LineOrigin::Added && l.text == "two")
    );
}

#[test]
fn stash_push_pop_and_drop() {
    let dir = init_repo("stash");
    std::fs::write(dir.join("a.txt"), "orig\n").unwrap();
    git(&dir, &["add", "a.txt"]);
    git(&dir, &["commit", "-q", "-m", "seed"]);

    let backend = Git2Backend::discover(&dir).unwrap();

    // stash an edit, worktree returns to committed state
    std::fs::write(dir.join("a.txt"), "orig\nedit\n").unwrap();
    backend.stash_push(false).unwrap();
    assert_eq!(backend.status().unwrap().stashes.len(), 1);
    assert_eq!(
        std::fs::read_to_string(dir.join("a.txt")).unwrap(),
        "orig\n"
    );

    // pop restores it and clears the stash
    backend.stash_pop(0).unwrap();
    assert!(backend.status().unwrap().stashes.is_empty());
    assert_eq!(
        std::fs::read_to_string(dir.join("a.txt")).unwrap(),
        "orig\nedit\n"
    );

    // drop removes a stash without applying it
    backend.stash_push(false).unwrap();
    backend.stash_drop(0).unwrap();
    assert!(backend.status().unwrap().stashes.is_empty());
    assert_eq!(
        std::fs::read_to_string(dir.join("a.txt")).unwrap(),
        "orig\n"
    );
}

#[test]
fn stash_with_message_and_apply_keeps_the_stash() {
    let dir = init_repo("stash-apply");
    std::fs::write(dir.join("a.txt"), "orig\n").unwrap();
    git(&dir, &["add", "a.txt"]);
    git(&dir, &["commit", "-q", "-m", "seed"]);

    let backend = Git2Backend::discover(&dir).unwrap();
    std::fs::write(dir.join("a.txt"), "orig\nedit\n").unwrap();
    backend.stash_push_message("wip: my edit", false).unwrap();

    let stashes = backend.status().unwrap().stashes;
    assert_eq!(stashes.len(), 1);
    assert!(stashes[0].message.contains("wip: my edit"));

    // apply restores the edit but leaves the stash in place
    backend.stash_apply(0).unwrap();
    assert_eq!(backend.status().unwrap().stashes.len(), 1);
    assert_eq!(
        std::fs::read_to_string(dir.join("a.txt")).unwrap(),
        "orig\nedit\n"
    );
}

#[test]
fn diff_refs_shows_changes_between_two_revisions() {
    let dir = init_repo("diff-refs");
    std::fs::write(dir.join("f.txt"), "one\n").unwrap();
    git(&dir, &["add", "f.txt"]);
    git(&dir, &["commit", "-q", "-m", "first"]);
    std::fs::write(dir.join("f.txt"), "one\ntwo\n").unwrap();
    git(&dir, &["add", "f.txt"]);
    git(&dir, &["commit", "-q", "-m", "second"]);

    let backend = Git2Backend::discover(&dir).unwrap();
    let files = backend.diff_refs("HEAD~1", "HEAD").unwrap();
    let file = files.iter().find(|f| f.path == "f.txt").unwrap();
    assert!(file.hunks[0].lines.iter().any(|l| l.text.trim() == "two"));

    // no differences between a revision and itself
    assert!(backend.diff_refs("HEAD", "HEAD").unwrap().is_empty());
}

#[test]
fn log_filters_by_author_and_limit() {
    let dir = init_repo("log-filter");
    std::fs::write(dir.join("a.txt"), "1\n").unwrap();
    git(&dir, &["add", "a.txt"]);
    git(
        &dir,
        &[
            "-c",
            "user.name=Alice",
            "-c",
            "user.email=alice@x.io",
            "commit",
            "-q",
            "-m",
            "by alice",
        ],
    );
    std::fs::write(dir.join("a.txt"), "1\n2\n").unwrap();
    git(&dir, &["add", "a.txt"]);
    git(
        &dir,
        &[
            "-c",
            "user.name=Bob",
            "-c",
            "user.email=bob@x.io",
            "commit",
            "-q",
            "-m",
            "by bob",
        ],
    );

    let backend = Git2Backend::discover(&dir).unwrap();

    let all = backend.log(&LogOptions::default()).unwrap();
    assert_eq!(all.len(), 2);

    let alice = backend
        .log(&LogOptions {
            author: Some("alice".into()),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(alice.len(), 1);
    assert_eq!(alice[0].summary, "by alice");

    let limited = backend
        .log(&LogOptions {
            limit: 1,
            ..Default::default()
        })
        .unwrap();
    assert_eq!(limited.len(), 1);
    assert_eq!(limited[0].summary, "by bob");
}

#[test]
fn add_list_and_remove_remotes() {
    let dir = init_repo("remotes");
    std::fs::write(dir.join("a.txt"), "x\n").unwrap();
    git(&dir, &["add", "a.txt"]);
    git(&dir, &["commit", "-q", "-m", "seed"]);

    let backend = Git2Backend::discover(&dir).unwrap();
    assert!(backend.remotes().unwrap().is_empty());

    backend
        .add_remote("origin", "https://example.com/repo.git")
        .unwrap();
    let remotes = backend.remotes().unwrap();
    assert_eq!(remotes.len(), 1);
    assert_eq!(remotes[0].name, "origin");
    assert_eq!(remotes[0].url, "https://example.com/repo.git");

    backend.remove_remote("origin").unwrap();
    assert!(backend.remotes().unwrap().is_empty());
}

#[test]
fn add_list_and_remove_worktrees() {
    let dir = init_repo("worktrees");
    std::fs::write(dir.join("a.txt"), "x\n").unwrap();
    git(&dir, &["add", "a.txt"]);
    git(&dir, &["commit", "-q", "-m", "seed"]);

    let backend = Git2Backend::discover(&dir).unwrap();
    assert!(backend.worktrees().unwrap().is_empty());

    let wt_path = scratch("worktrees-linked");
    let _ = std::fs::remove_dir_all(&wt_path);
    backend
        .add_worktree("feature-wt", wt_path.to_str().unwrap())
        .unwrap();
    let worktrees = backend.worktrees().unwrap();
    assert_eq!(worktrees.len(), 1);
    assert_eq!(worktrees[0].name, "feature-wt");

    backend.remove_worktree("feature-wt").unwrap();
    assert!(backend.worktrees().unwrap().is_empty());
}

#[test]
fn rename_branch_changes_the_current_head() {
    let dir = init_repo("rename");
    std::fs::write(dir.join("a.txt"), "x\n").unwrap();
    git(&dir, &["add", "a.txt"]);
    git(&dir, &["commit", "-q", "-m", "seed"]);

    let backend = Git2Backend::discover(&dir).unwrap();
    backend.rename_branch("main", "trunk").unwrap();

    assert_eq!(backend.local_branches().unwrap(), ["trunk"]);
    assert_eq!(
        backend.status().unwrap().head.branch.as_deref(),
        Some("trunk")
    );
}

#[test]
fn refs_lists_branches_and_tags() {
    use rgit_git::RefKind;
    let dir = init_repo("refs");
    std::fs::write(dir.join("f.txt"), "x\n").unwrap();
    git(&dir, &["add", "f.txt"]);
    git(&dir, &["commit", "-q", "-m", "seed"]);
    git(&dir, &["branch", "feature"]);
    git(&dir, &["tag", "-m", "release 1", "v1"]);

    let backend = Git2Backend::discover(&dir).unwrap();
    let refs = backend.refs().unwrap();

    let locals: Vec<&str> = refs
        .iter()
        .filter(|r| r.kind == RefKind::Local)
        .map(|r| r.name.as_str())
        .collect();
    assert!(locals.contains(&"main") && locals.contains(&"feature"));
    assert!(
        refs.iter()
            .any(|r| r.kind == RefKind::Local && r.name == "main" && r.is_head)
    );
    assert!(
        refs.iter()
            .any(|r| r.kind == RefKind::Tag && r.name == "v1")
    );
}

#[test]
fn create_list_and_checkout_branches() {
    let dir = init_repo("branches");
    std::fs::write(dir.join("a.txt"), "x\n").unwrap();
    git(&dir, &["add", "a.txt"]);
    git(&dir, &["commit", "-q", "-m", "seed"]);

    let backend = Git2Backend::discover(&dir).unwrap();
    assert_eq!(backend.local_branches().unwrap(), ["main"]);

    backend.create_branch("feature").unwrap();
    assert_eq!(backend.local_branches().unwrap(), ["feature", "main"]);
    assert_eq!(
        backend.status().unwrap().head.branch.as_deref(),
        Some("feature")
    );

    backend.checkout_branch("main").unwrap();
    assert_eq!(
        backend.status().unwrap().head.branch.as_deref(),
        Some("main")
    );
}

fn write_hook(dir: &Path, name: &str, body: &str) {
    let hooks = dir.join(".git/hooks");
    std::fs::create_dir_all(&hooks).unwrap();
    let path = hooks.join(name);
    std::fs::write(&path, body).unwrap();
    Command::new("chmod").arg("+x").arg(&path).status().unwrap();
}

#[test]
fn commit_creates_a_commit_from_the_index() {
    let dir = init_repo("commit-ok");
    std::fs::write(dir.join("a.txt"), "hi\n").unwrap();
    git(&dir, &["add", "a.txt"]);

    let backend = Git2Backend::discover(&dir).unwrap();
    backend.commit("first commit").unwrap();

    let st = backend.status().unwrap();
    assert_eq!(st.head.summary.as_deref(), Some("first commit"));
    assert_eq!(st.recent.len(), 1);
    assert!(st.entries.is_empty(), "index committed, tree clean");
}

#[test]
fn amend_replaces_head_and_keeps_history() {
    let dir = init_repo("amend");
    std::fs::write(dir.join("a.txt"), "1\n").unwrap();
    git(&dir, &["add", "a.txt"]);
    git(&dir, &["commit", "-q", "-m", "first"]);
    std::fs::write(dir.join("b.txt"), "2\n").unwrap();
    git(&dir, &["add", "b.txt"]);
    git(&dir, &["commit", "-q", "-m", "second"]);

    // stage a further change, then amend the second commit
    std::fs::write(dir.join("c.txt"), "3\n").unwrap();
    git(&dir, &["add", "c.txt"]);

    let backend = Git2Backend::discover(&dir).unwrap();
    assert_eq!(backend.head_message().unwrap().trim(), "second");
    backend.amend("second, amended").unwrap();

    let st = backend.status().unwrap();
    assert_eq!(st.recent.len(), 2, "still two commits; HEAD replaced");
    assert_eq!(st.head.summary.as_deref(), Some("second, amended"));
    assert!(
        st.entries.is_empty(),
        "the staged change went into the amend"
    );
}

#[test]
fn extend_keeps_the_message_and_folds_in_staged_changes() {
    let dir = init_repo("extend");
    std::fs::write(dir.join("f.txt"), "1\n").unwrap();
    git(&dir, &["add", "f.txt"]);
    git(&dir, &["commit", "-q", "-m", "keep me"]);
    std::fs::write(dir.join("g.txt"), "2\n").unwrap();
    git(&dir, &["add", "g.txt"]);

    let backend = Git2Backend::discover(&dir).unwrap();
    backend.commit_extend().unwrap();

    let st = backend.status().unwrap();
    assert_eq!(st.head.summary.as_deref(), Some("keep me"));
    assert_eq!(st.recent.len(), 1, "HEAD replaced, not a new commit");
    assert!(st.entries.is_empty(), "the staged change went into HEAD");
}

#[test]
fn nothing_staged_is_rejected() {
    let dir = init_repo("commit-empty");
    std::fs::write(dir.join("a.txt"), "hi\n").unwrap();
    git(&dir, &["add", "a.txt"]);
    git(&dir, &["commit", "-q", "-m", "seed"]);

    let backend = Git2Backend::discover(&dir).unwrap();
    assert!(matches!(
        backend.commit("noop"),
        Err(rgit_git::GitError::NothingToCommit)
    ));
}

#[test]
fn a_failing_pre_commit_hook_aborts() {
    let dir = init_repo("commit-hook");
    std::fs::write(dir.join("a.txt"), "hi\n").unwrap();
    git(&dir, &["add", "a.txt"]);
    write_hook(
        &dir,
        "pre-commit",
        "#!/bin/sh\necho rejected 1>&2\nexit 1\n",
    );

    let backend = Git2Backend::discover(&dir).unwrap();
    let err = backend.commit("blocked").unwrap_err();
    assert!(matches!(err, rgit_git::GitError::Hook(_)));
    assert!(
        backend.status().unwrap().recent.is_empty(),
        "no commit made"
    );
}

fn run(dir: &Path, args: &[&str]) {
    assert!(
        Command::new("git")
            .current_dir(dir)
            .args(args)
            .status()
            .unwrap()
            .success(),
        "git {args:?} failed"
    );
}

#[test]
fn push_fetch_and_pull_over_a_local_remote() {
    let base = scratch("network");
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(&base).unwrap();
    let remote = base.join("remote.git");
    let a = base.join("a");
    let b = base.join("b");

    run(
        &base,
        &[
            "init",
            "-q",
            "--bare",
            "-b",
            "main",
            remote.to_str().unwrap(),
        ],
    );

    // Repo A: seed a commit and publish it, wiring up the upstream.
    std::fs::create_dir_all(&a).unwrap();
    run(&a, &["init", "-q", "-b", "main"]);
    run(&a, &["config", "user.email", "a@e.com"]);
    run(&a, &["config", "user.name", "a"]);
    run(&a, &["remote", "add", "origin", remote.to_str().unwrap()]);
    std::fs::write(a.join("f1.txt"), "one\n").unwrap();
    run(&a, &["add", "f1.txt"]);
    run(&a, &["commit", "-q", "-m", "first"]);
    run(&a, &["push", "-q", "-u", "origin", "main"]);

    // Repo B: clone, so it tracks origin/main.
    run(
        &base,
        &["clone", "-q", remote.to_str().unwrap(), b.to_str().unwrap()],
    );
    run(&b, &["config", "user.email", "b@e.com"]);
    run(&b, &["config", "user.name", "b"]);

    // A commits again and pushes through the backend.
    std::fs::write(a.join("f2.txt"), "two\n").unwrap();
    run(&a, &["add", "f2.txt"]);
    run(&a, &["commit", "-q", "-m", "second"]);
    Git2Backend::discover(&a)
        .unwrap()
        .push(None, false, false, false, &|_| {})
        .unwrap();

    // B fetches: it should now see itself one commit behind.
    let backend_b = Git2Backend::discover(&b).unwrap();
    backend_b.fetch(&|_| {}).unwrap();
    assert_eq!(backend_b.status().unwrap().head.behind, 1);

    // B pulls: fast-forwards and gains the second file.
    backend_b.pull(&|_| {}).unwrap();
    let st = backend_b.status().unwrap();
    assert_eq!(st.head.behind, 0);
    assert_eq!(st.recent.len(), 2);
    assert!(
        b.join("f2.txt").exists(),
        "pull brought the new file into the worktree"
    );
}

#[test]
fn discover_outside_a_repo_is_an_error() {
    let dir = scratch("not-a-repo");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    assert!(Git2Backend::discover(&dir).is_err());
}

#[test]
fn commits_not_on_a_remote_are_marked_unpushed() {
    let dir = init_repo("unpushed");
    std::fs::write(dir.join("a.txt"), "1\n").unwrap();
    git(&dir, &["add", "a.txt"]);
    git(&dir, &["commit", "-qm", "C0"]);
    let c0 = String::from_utf8_lossy(
        &Command::new("git")
            .arg("-C")
            .arg(&dir)
            .args(["rev-parse", "HEAD"])
            .output()
            .unwrap()
            .stdout,
    )
    .trim()
    .to_owned();
    // Pretend C0 is on origin/main; C1 is then local-only.
    git(&dir, &["update-ref", "refs/remotes/origin/main", &c0]);
    std::fs::write(dir.join("a.txt"), "2\n").unwrap();
    git(&dir, &["commit", "-aqm", "C1"]);

    let backend = Git2Backend::discover(&dir).unwrap();
    let recent = backend.status().unwrap().recent;
    let c1 = recent.iter().find(|c| c.summary == "C1").unwrap();
    let c0e = recent.iter().find(|c| c.summary == "C0").unwrap();
    assert!(c1.unpushed, "C1 is not on any remote");
    assert!(!c0e.unpushed, "C0 is on origin/main");

    let log = backend.log(&Default::default()).unwrap();
    assert!(log.iter().find(|e| e.summary == "C1").unwrap().unpushed);
    assert!(!log.iter().find(|e| e.summary == "C0").unwrap().unpushed);

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn list_tree_and_read_blob_at_head() {
    let dir = init_repo("tree-blob");
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(dir.join("src/main.rs"), "fn main() {}\n").unwrap();
    std::fs::write(dir.join("README.md"), "# hi\n").unwrap();
    git(&dir, &["add", "."]);
    git(&dir, &["commit", "-q", "-m", "init"]);

    let backend = Git2Backend::discover(&dir).unwrap();

    // Root listing: directories before files, each alphabetical.
    let root = backend.list_tree("HEAD", "").unwrap();
    let names: Vec<_> = root.iter().map(|e| e.name.as_str()).collect();
    assert_eq!(names, vec!["src", "README.md"]);
    assert!(root[0].is_dir && !root[1].is_dir);
    assert_eq!(root[1].path, "README.md");

    // Subdirectory listing carries full paths.
    let src = backend.list_tree("HEAD", "src").unwrap();
    assert_eq!(src.len(), 1);
    assert_eq!(src[0].path, "src/main.rs");

    // Blob content decodes as text.
    let blob = backend.read_blob("HEAD", "src/main.rs").unwrap();
    assert!(!blob.is_binary);
    assert_eq!(blob.text.as_deref(), Some("fn main() {}\n"));
    assert_eq!(blob.size, 13);

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn tree_last_commits_reports_the_touching_commit_per_path() {
    let dir = init_repo("tree-last");
    std::fs::write(dir.join("a.txt"), "a1\n").unwrap();
    std::fs::write(dir.join("b.txt"), "b1\n").unwrap();
    git(&dir, &["add", "."]);
    git(&dir, &["commit", "-q", "-m", "add a and b"]);
    // A second commit touches only a.txt; b's last commit stays the first.
    std::fs::write(dir.join("a.txt"), "a1\na2\n").unwrap();
    git(&dir, &["add", "a.txt"]);
    git(&dir, &["commit", "-q", "-m", "extend a"]);

    let backend = Git2Backend::discover(&dir).unwrap();
    let paths = vec!["a.txt".to_owned(), "b.txt".to_owned()];
    let last = backend.tree_last_commits("HEAD", &paths).unwrap();
    assert_eq!(last["a.txt"].summary, "extend a");
    assert_eq!(last["b.txt"].summary, "add a and b");

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn list_files_walks_the_whole_tree() {
    let dir = init_repo("list-files");
    std::fs::create_dir_all(dir.join("src/util")).unwrap();
    std::fs::write(dir.join("Cargo.toml"), "[package]\n").unwrap();
    std::fs::write(dir.join("src/main.rs"), "fn main(){}\n").unwrap();
    std::fs::write(dir.join("src/util/mod.rs"), "\n").unwrap();
    git(&dir, &["add", "."]);
    git(&dir, &["commit", "-q", "-m", "init"]);

    let backend = Git2Backend::discover(&dir).unwrap();
    let mut files = backend.list_files("HEAD").unwrap();
    files.sort();
    assert_eq!(files, vec!["Cargo.toml", "src/main.rs", "src/util/mod.rs"]);

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn rev_parse_and_archive() {
    let dir = init_repo("archive");
    std::fs::write(dir.join("a.txt"), "hello\n").unwrap();
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(dir.join("src/main.rs"), "fn main(){}\n").unwrap();
    git(&dir, &["add", "."]);
    git(&dir, &["commit", "-q", "-m", "init"]);

    let backend = Git2Backend::discover(&dir).unwrap();

    // rev_parse resolves HEAD to a 40-hex id.
    let sha = backend.rev_parse("HEAD").unwrap();
    assert_eq!(sha.len(), 40);
    assert!(sha.chars().all(|c| c.is_ascii_hexdigit()));

    // The archive is a real gzip stream carrying the tree's files.
    let bytes = backend.archive_targz("HEAD").unwrap();
    assert_eq!(&bytes[..2], &[0x1f, 0x8b], "gzip magic");
    assert!(bytes.len() > 20);

    let _ = std::fs::remove_dir_all(&dir);
}
