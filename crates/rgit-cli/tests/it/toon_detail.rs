//! The TOON and JSON shapes for show, diff, log and friends carry git's full
//! detail as structured data (CHG-jvvjmr).

use std::path::{Path, PathBuf};
use std::process::Command;

fn git_env(dir: &Path, env: &[(&str, &str)], args: &[&str]) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .envs(env.iter().copied())
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@t")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@t")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// `rgit --toon <args>` output.
fn toon(dir: &Path, args: &[&str]) -> String {
    let out = Command::new(env!("CARGO_BIN_EXE_rgit"))
        .arg("--toon")
        .args(args)
        .current_dir(dir)
        .env("RGIT_OPLOG", "0")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "rgit --toon {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// `rgit --json <args>` output as a JSON object.
fn json_out(dir: &Path, args: &[&str]) -> serde_json::Value {
    let out = Command::new(env!("CARGO_BIN_EXE_rgit"))
        .arg("--json")
        .args(args)
        .current_dir(dir)
        .env("RGIT_OPLOG", "0")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "rgit --json {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).unwrap()
}

/// A repo whose HEAD commit adds `f` of 4 lines and `g` of 2 lines, with
/// unstaged changes after it: `f` gains a 5th line, `g` is deleted.
fn forms_repo(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("rgit-toon-detail-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    git(&dir, &["init", "-q", "-b", "main"]);
    std::fs::write(dir.join("f"), "a\nb\nc\nd\n").unwrap();
    std::fs::write(dir.join("g"), "1\n2\n").unwrap();
    git(&dir, &["add", "f", "g"]);
    git(&dir, &["commit", "-qm", "init"]);
    std::fs::write(dir.join("f"), "a\nb\nc\nd\ne\n").unwrap();
    std::fs::remove_file(dir.join("g")).unwrap();
    dir
}

#[test]
fn forms_name_status_lists_status_letters() {
    let dir = forms_repo("name-status");
    let out = toon(&dir, &["diff", "--name-status"]);
    assert!(out.contains("files[2]{path,status}:"), "{out}");
    assert!(out.contains("\n  f,M"), "{out}");
    assert!(out.contains("\n  g,D"), "{out}");

    let js = json_out(&dir, &["diff", "--name-status"]);
    assert_eq!(js["files"][0]["path"], "f");
    assert_eq!(js["files"][0]["status"], "M");
    assert_eq!(js["files"][1]["status"], "D");
}

#[test]
fn forms_numstat_lists_line_counts() {
    let dir = forms_repo("numstat");
    let out = toon(&dir, &["diff", "--numstat"]);
    assert!(out.contains("files[2]{path,added,removed}:"), "{out}");
    assert!(out.contains("\n  f,1,0"), "{out}");
    assert!(out.contains("\n  g,0,2"), "{out}");
}

#[test]
fn forms_shortstat_shows_the_summary() {
    let dir = forms_repo("shortstat");
    let out = toon(&dir, &["diff", "--shortstat"]);
    assert!(out.contains("total: \"2 files, +1 -2\""), "{out}");
}

#[test]
fn forms_patch_stat_is_not_empty() {
    let dir = forms_repo("patch-stat");
    let out = toon(&dir, &["diff", "--patch", "--stat"]);
    assert!(out.contains("total: \"2 files, +1 -2\""), "{out}");

    let js = json_out(&dir, &["diff", "--patch", "--stat"]);
    let files = js["files"].as_array().unwrap();
    assert_eq!(files.len(), 2);
    assert_eq!(files[0]["path"], "f");
    assert_eq!(files[0]["added"], 1);
    assert_eq!(files[1]["path"], "g");
    assert_eq!(files[1]["removed"], 2);
    assert_eq!(files[0]["hunks"][0]["lines"].as_array().unwrap().last().unwrap(), "+e");
    assert_eq!(files[1]["hunks"][0]["lines"].as_array().unwrap().len(), 2);
}

/// A stash of an edit to `f`.
fn stash_repo(tag: &str) -> PathBuf {
    let dir = detail_repo(tag);
    std::fs::write(dir.join("f"), "a\nb\nc\nd\ne\n").unwrap();
    git(&dir, &["stash", "-q"]);
    dir
}

#[test]
fn forms_stash_show_defaults_to_the_stat() {
    let dir = stash_repo("stash-stat");
    let out = toon(&dir, &["stash", "show"]);
    assert!(out.contains("files[1]{path,status,added,removed}:"), "{out}");
    assert!(out.contains("\n  f,M,1,0"), "{out}");
    assert!(!out.contains("hunks"), "{out}");
}

#[test]
fn forms_stash_show_patch_has_hunks() {
    let dir = stash_repo("stash-patch");
    let js = json_out(&dir, &["stash", "show", "-p"]);
    let lines = js["files"][0]["hunks"][0]["lines"].as_array().unwrap();
    assert_eq!(lines.last().unwrap(), "+e");
}

/// A result that only has text keeps that text as `lines`.
#[test]
fn forms_text_only_keeps_its_text() {
    let dir = forms_repo("text-only");
    let out = toon(&dir, &["log", "--pretty=medium", "-1"]);
    assert!(out.starts_with("lines["), "{out}");
    assert!(out.contains(" init"), "{out}");
}

/// A repo whose HEAD commit appends `e` to `f` of 4 lines.
fn detail_repo(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("rgit-toon-detail-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    git(&dir, &["init", "-q", "-b", "main"]);
    std::fs::write(dir.join("f"), "a\nb\nc\nd\n").unwrap();
    git(&dir, &["add", "f"]);
    git(&dir, &["commit", "-qm", "one"]);
    dir
}

fn commit_file(dir: &Path, name: &str, content: &str) {
    std::fs::write(dir.join(name), content).unwrap();
    git(dir, &["add", name]);
    git(dir, &["commit", "-qm", &format!("change {name}")]);
}

#[test]
fn diff_modified_file_shows_its_hunk() {
    let dir = detail_repo("hunk");
    commit_file(&dir, "f", "a\nb\nc\nd\ne\n");
    let out = toon(&dir, &["show", "HEAD"]);
    assert!(out.contains("path: f"), "{out}");
    assert!(out.contains("status: M"), "{out}");
    assert!(out.contains("added: 1"), "{out}");
    assert!(out.contains("removed: 0"), "{out}");
    assert!(out.contains("at: \"-2,3 +2,4\""), "{out}");
    assert!(out.contains("hunks[1]:"), "{out}");
    // The last hunk line is the addition, with its `+` kept.
    assert!(out.trim_end().ends_with("+e"), "{out}");

    let js = json_out(&dir, &["show", "HEAD"]);
    let file = &js["files"][0];
    assert_eq!(file["path"], "f");
    assert_eq!(file["status"], "M");
    let lines = file["hunks"][0]["lines"].as_array().unwrap();
    assert_eq!(lines.last().unwrap(), "+e");
}

#[test]
fn diff_rename_names_its_source() {
    let dir = detail_repo("rename");
    git(&dir, &["mv", "f", "h"]);
    let out = toon(&dir, &["diff", "--cached"]);
    assert!(out.contains("path: h"), "{out}");
    assert!(out.contains("status: R"), "{out}");
    assert!(out.contains("old_path: f"), "{out}");
    assert!(out.contains("hunks: []"), "{out}");
    assert!(!out.contains("binary"), "{out}");
}

#[test]
fn diff_binary_file_is_marked() {
    let dir = detail_repo("binary");
    let bytes: &[u8] = &[0, 1, 2, 0];
    std::fs::write(dir.join("b.bin"), bytes).unwrap();
    git(&dir, &["add", "b.bin"]);
    let out = toon(&dir, &["diff", "--cached"]);
    assert!(out.contains("status: A"), "{out}");
    assert!(out.contains("binary: true"), "{out}");

    let js = json_out(&dir, &["diff", "--cached"]);
    assert_eq!(js["files"][0]["status"], "A");
    assert_eq!(js["files"][0]["binary"], true);
}

#[test]
fn diff_stat_omits_hunks() {
    let dir = detail_repo("stat");
    std::fs::write(dir.join("f"), "A\nb\nc\nd\n").unwrap();
    std::fs::write(dir.join("g"), "1\n").unwrap();
    git(&dir, &["add", "f", "g"]);
    git(&dir, &["commit", "-qm", "two"]);
    let out = toon(&dir, &["show", "--stat", "HEAD"]);
    assert!(out.contains("files[2]{path,status,added,removed}:"), "{out}");
    assert!(!out.contains("hunks"), "{out}");
}

#[test]
fn diff_no_patch_prints_no_files() {
    let dir = detail_repo("no-patch");
    commit_file(&dir, "f", "a\nb\nc\nd\ne\n");
    let out = toon(&dir, &["show", "-s", "HEAD"]);
    assert!(!out.contains("files"), "{out}");
    assert!(!out.contains("total"), "{out}");
}

#[test]
fn diff_small_diff_is_not_cut() {
    let dir = detail_repo("small");
    let mut content = String::new();
    for n in 1..=50 {
        content.push_str(&format!("line {n}\n"));
    }
    commit_file(&dir, "f", &content);
    let out = toon(&dir, &["show", "HEAD"]);
    assert!(!out.contains("count"), "{out}");
    assert!(!out.contains("--full"), "{out}");
}

/// Three hunks of 100 changed lines each: 103, 106 and 106 lines with git's
/// 3 lines of context.
fn hunked_repo(tag: &str) -> PathBuf {
    let dir = detail_repo(tag);
    let mut content = String::new();
    let section = |out: &mut String, mark: char| {
        for n in 1..=50 {
            out.push_str(&format!("{mark}{n}\n"));
        }
        for n in 1..=20 {
            out.push_str(&format!("sep{n}\n"));
        }
    };
    section(&mut content, 's');
    section(&mut content, 's');
    section(&mut content, 's');
    commit_file(&dir, "f", &content);
    let mut edited = String::new();
    section(&mut edited, 'S');
    section(&mut edited, 'S');
    section(&mut edited, 'S');
    commit_file(&dir, "f", &edited);
    dir
}

#[test]
fn diff_big_diff_is_cut_at_a_hunk_edge() {
    let dir = hunked_repo("cut");
    let out = toon(&dir, &["show", "HEAD"]);
    assert!(out.contains("hunks[1]:"), "{out}");
    assert!(!out.contains("hunks[2]"), "{out}");
    assert!(out.contains("count: 103 of 315 diff lines"), "{out}");
    assert!(out.contains("--full"), "{out}");
    assert!(out.contains("show HEAD"), "{out}");
}

#[test]
fn diff_full_keeps_every_hunk() {
    let dir = hunked_repo("full");
    let out = toon(&dir, &["--full", "show", "HEAD"]);
    assert!(out.contains("hunks[3]:"), "{out}");
    assert!(!out.contains("count"), "{out}");
}

#[test]
fn diff_default_has_hunks_and_no_patch_hint() {
    let dir = detail_repo("default");
    std::fs::write(dir.join("f"), "A\nb\nc\nd\n").unwrap();
    let out = toon(&dir, &["diff"]);
    assert!(out.contains("hunks[1]:"), "{out}");
    assert!(out.contains("total:"), "{out}");
    assert!(!out.contains("for the full patch"), "{out}");
}

#[test]
fn diff_name_only_lists_paths() {
    let dir = detail_repo("name-only");
    std::fs::write(dir.join("f"), "A\nb\nc\nd\n").unwrap();
    let out = toon(&dir, &["diff", "--name-only"]);
    assert!(out.starts_with("files[1]:"), "{out}");
    assert!(!out.contains("hunks"), "{out}");
}

#[test]
fn show_plain_commit_has_the_full_header() {
    let dir = detail_repo("plain");
    let who = [
        ("GIT_AUTHOR_NAME", "T"),
        ("GIT_AUTHOR_EMAIL", "t@t"),
        ("GIT_COMMITTER_NAME", "T"),
        ("GIT_COMMITTER_EMAIL", "t@t"),
    ];
    std::fs::write(dir.join("f"), "a\nb\nc\nd\ne\n").unwrap();
    git_env(&dir, &who, &["commit", "-am", "change f", "-m", "body line"]);
    let out = toon(&dir, &["show", "HEAD"]);
    let id = out
        .lines()
        .find_map(|l| l.strip_prefix("id: "))
        .unwrap_or_default()
        .to_owned();
    assert_eq!(id.len(), 40, "{out}");
    assert!(id.bytes().all(|b| b.is_ascii_hexdigit()), "{out}");
    assert!(out.contains("author: T <t@t>"), "{out}");
    assert!(out.contains("date: \"2"), "{out}");
    assert!(out.contains("subject: change f"), "{out}");
    assert!(out.contains("body: body line"), "{out}");

    let js = json_out(&dir, &["show", "HEAD"]);
    assert_eq!(js["author"], "T <t@t>");
    assert!(js["date"]
        .as_str()
        .is_some_and(|d| d.len() == 25 && d.as_bytes()[10] == b'T'), "{}", js["date"]);
}

#[test]
fn show_merge_names_its_diff_base() {
    let dir = detail_repo("merge");
    git(&dir, &["checkout", "-q", "-b", "topic"]);
    commit_file(&dir, "f", "a\nb\nc\nd\ntopic\n");
    git(&dir, &["checkout", "-q", "main"]);
    commit_file(&dir, "m", "main\n");
    git(&dir, &["merge", "-q", "--no-ff", "topic", "-m", "merge"]);
    let out = toon(&dir, &["show", "HEAD"]);
    assert!(out.contains("parents[2]:"), "{out}");
    assert!(out.contains("diff: first parent "), "{out}");
}

#[test]
fn show_tag_shows_the_tag_first() {
    let dir = detail_repo("tag");
    let who = [("GIT_COMMITTER_NAME", "T"), ("GIT_COMMITTER_EMAIL", "t@t")];
    git_env(&dir, &who, &["tag", "-a", "v1", "-m", "release one"]);
    let out = toon(&dir, &["show", "v1"]);
    assert!(out.contains("tag:"), "{out}");
    assert!(out.contains("name: v1"), "{out}");
    assert!(out.contains("tagger: T <t@t>"), "{out}");
    assert!(out.contains("date: \"2"), "{out}");
    assert!(out.contains("message: release one"), "{out}");
    assert!(out.contains("subject: one"), "{out}");

    let js = json_out(&dir, &["show", "v1"]);
    assert_eq!(js["tag"]["name"], "v1");
    assert_eq!(js["tag"]["message"], "release one");
    assert_eq!(js["subject"], "one");
}

#[test]
fn show_signed_tag_keeps_its_message_clean() {
    let dir = detail_repo("signed-tag");
    let key = dir.join(".key");
    let made = Command::new("ssh-keygen")
        .args(["-q", "-t", "ed25519", "-N", "", "-f"])
        .arg(&key)
        .status()
        .unwrap();
    assert!(made.success());
    let who = [("GIT_COMMITTER_NAME", "T"), ("GIT_COMMITTER_EMAIL", "t@t")];
    let signing_key = format!("user.signingkey={}", key.display());
    git_env(
        &dir,
        &who,
        &[
            "-c",
            "gpg.format=ssh",
            "-c",
            &signing_key,
            "tag",
            "-s",
            "v2",
            "-m",
            "release two",
        ],
    );
    let out = toon(&dir, &["show", "v2"]);
    assert!(out.contains("message: release two\n"), "{out}");
    assert!(out.contains("signature: ssh"), "{out}");
    assert!(!out.contains("BEGIN SSH SIGNATURE"), "{out}");

    let js = json_out(&dir, &["show", "v2"]);
    assert_eq!(js["tag"]["message"], "release two");
    assert_eq!(js["tag"]["signature"], "ssh");
}

#[test]
fn show_two_revisions_print_two_commits() {
    let dir = detail_repo("two");
    commit_file(&dir, "f", "a\nb\nc\nd\ne\n");
    let out = toon(&dir, &["show", "HEAD~1", "HEAD"]);
    assert!(out.contains("commits[2]:"), "{out}");
    assert_eq!(out.matches("\n  - id: ").count(), 2, "{out}");
    let js = json_out(&dir, &["show", "HEAD~1", "HEAD"]);
    let commits = js["commits"].as_array().unwrap();
    assert_eq!(commits.len(), 2);
    assert_eq!(commits[0]["subject"], "one");
    assert_eq!(commits[1]["subject"], "change f");
    assert_eq!(commits[1]["files"][0]["path"], "f");
    assert_eq!(commits[0]["files"][0]["status"], "A");
}

#[test]
fn log_patch_shows_structured_commits() {
    let dir = detail_repo("log-p");
    commit_file(&dir, "f", "a\nb\nc\nd\ne\n");
    commit_file(&dir, "f", "A\nb\nc\nd\ne\n");
    let out = toon(&dir, &["log", "-p", "-2"]);
    assert!(out.contains("commits[2]:"), "{out}");
    assert_eq!(out.matches("\n  - id: ").count(), 2, "{out}");
    assert_eq!(out.matches("author: t <t@t>").count(), 2, "{out}");
    assert_eq!(out.matches("date: \"2").count(), 2, "{out}");
    assert!(out.contains("hunks[1]:"), "{out}");

    let js = json_out(&dir, &["log", "-p", "-2"]);
    let commits = js["commits"].as_array().unwrap();
    assert_eq!(commits.len(), 2);
    assert!(commits[0]["files"][0]["hunks"].is_array(), "{}", js);
}

#[test]
fn log_stat_shows_files_without_hunks() {
    let dir = detail_repo("log-stat");
    commit_file(&dir, "f", "a\nb\nc\nd\ne\n");
    commit_file(&dir, "f", "A\nb\nc\nd\ne\n");
    let out = toon(&dir, &["log", "--stat", "-2"]);
    assert_eq!(out.matches("files[1]{path,status,added,removed}:").count(), 2, "{out}");
    assert!(!out.contains("hunks"), "{out}");
}

#[test]
fn misc_blame_has_a_date() {
    let dir = detail_repo("blame");
    std::fs::write(dir.join("f"), "one\ntwo\nthree\n").unwrap();
    git(&dir, &["commit", "-qam", "three lines"]);
    let out = toon(&dir, &["blame", "f"]);
    assert!(
        out.contains("lines[3]{line,id,author,when,text}:"),
        "{out}"
    );
    let row = out
        .lines()
        .find_map(|l| l.trim().strip_prefix("1,"))
        .unwrap_or_default();
    let cells: Vec<&str> = row.split(',').collect();
    assert_eq!(cells[1], "t", "{out}");
    assert!(cells[2].strip_suffix('s').is_some_and(|n| n.parse::<u64>().is_ok()), "{out}");

    let js = json_out(&dir, &["blame", "f"]);
    assert!(js["lines"][0]["when"].is_string(), "{}", js);
}

#[test]
fn misc_worktree_list_has_the_head() {
    let dir = detail_repo("worktree");
    git(&dir, &["worktree", "add", "wt2", "-b", "wt2"]);
    let out = toon(&dir, &["worktree", "list"]);
    assert!(out.contains("worktrees[2]{name,branch,path,head}:"), "{out}");
    let short = git(&dir, &["rev-parse", "--short", "HEAD"]);
    let short = short.trim();
    assert_eq!(out.matches(short).count(), 2, "{out}");
}

#[test]
fn misc_cat_file_shows_lines() {
    let dir = detail_repo("cat-file");
    let out = toon(&dir, &["cat-file", "-p", "HEAD"]);
    assert!(out.contains("lines["), "{out}");
    assert!(out.contains("\n  tree "), "{out}");
    assert!(!out.contains("\\n"), "{out}");

    git_env(
        &dir,
        &[("GIT_COMMITTER_NAME", "T"), ("GIT_COMMITTER_EMAIL", "t@t")],
        &["tag", "-a", "v1", "-m", "tag one"],
    );
    let out = toon(&dir, &["cat-file", "-p", "v1"]);
    assert!(out.contains("lines["), "{out}");
    assert!(out.contains("\n  object "), "{out}");
    assert!(!out.contains("\\n"), "{out}");
}

/// `n` lines of the form `<tag>1`, `<tag>2`...
fn numbered(tag: &str, n: usize) -> String {
    (1..=n).map(|i| format!("{tag}{i}\n")).collect()
}

fn commit_files(dir: &Path, files: &[(&str, String)], message: &str) {
    for (name, content) in files {
        std::fs::write(dir.join(name), content).unwrap();
        git(dir, &["add", name]);
    }
    git(dir, &["commit", "-qm", message]);
}

fn file_by_path<'a>(files: &'a serde_json::Value, path: &str) -> &'a serde_json::Value {
    files
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["path"] == path)
        .unwrap_or_else(|| panic!("no file {path} in {files}"))
}

#[test]
fn diff_mixed_rows_keep_old_path_and_binary() {
    let dir = detail_repo("mixed");
    commit_files(&dir, &[("a", "x\n".to_owned())], "add a");
    std::fs::write(dir.join("a"), "y\n").unwrap();
    std::fs::write(dir.join("b.bin"), [0u8, 1, 2, 0]).unwrap();
    git(&dir, &["mv", "f", "h"]);
    git(&dir, &["add", "a", "b.bin"]);

    let js = json_out(&dir, &["diff", "--cached"]);
    let files = &js["files"];
    assert_eq!(files.as_array().unwrap().len(), 3, "{js}");
    let a = file_by_path(files, "a");
    assert_eq!(a["status"], "M");
    assert!(a.get("old_path").is_none() && a.get("binary").is_none(), "{a}");
    let bin = file_by_path(files, "b.bin");
    assert_eq!(bin["status"], "A");
    assert_eq!(bin["binary"], true);
    let h = file_by_path(files, "h");
    assert_eq!(h["status"], "R");
    assert_eq!(h["old_path"], "f");

    let out = toon(&dir, &["diff", "--cached"]);
    assert!(out.contains("old_path: f"), "{out}");
    assert!(out.contains("binary: true"), "{out}");
}

#[test]
fn diff_single_hunk_over_the_budget_shows_200_lines() {
    let dir = detail_repo("budget-one");
    commit_files(&dir, &[("big", numbered("n", 300))], "big");
    let js = json_out(&dir, &["show", "HEAD"]);
    let lines = js["files"][0]["hunks"][0]["lines"].as_array().unwrap();
    assert_eq!(lines.len(), 200);
    assert_eq!(lines[0], "+n1");
    assert_eq!(js["count"], "200 of 300 diff lines");
    let out = toon(&dir, &["show", "HEAD"]);
    assert!(out.contains("count: 200 of 300 diff lines"), "{out}");
    assert!(out.contains("rgit --full --toon show HEAD"), "{out}");
}

#[test]
fn diff_budget_cut_is_a_prefix_of_the_hunks() {
    let dir = detail_repo("budget-prefix");
    commit_files(
        &dir,
        &[
            ("p1", numbered("a", 150)),
            ("p2", numbered("b", 150)),
            ("p3", numbered("c", 10)),
        ],
        "three files",
    );
    let js = json_out(&dir, &["show", "HEAD"]);
    let files = &js["files"];
    assert_eq!(file_by_path(files, "p1")["hunks"][0]["lines"].as_array().unwrap().len(), 150);
    assert_eq!(file_by_path(files, "p2")["hunks"], "1 hunk not shown");
    assert_eq!(file_by_path(files, "p3")["hunks"], "1 hunk not shown");
    assert_eq!(js["count"], "150 of 310 diff lines");
    assert_eq!(js["help"].as_array().unwrap().len(), 1, "{js}");
}

#[test]
fn diff_budget_is_shared_across_commits() {
    let dir = detail_repo("budget-log");
    commit_files(&dir, &[("p1", numbered("a", 150))], "first");
    commit_files(&dir, &[("p2", numbered("b", 150))], "second");
    let out = toon(&dir, &["log", "-p", "-2"]);
    assert_eq!(out.matches("count:").count(), 1, "{out}");
    assert!(out.contains("\ncount: 150 of 300 diff lines"), "{out}");
    assert_eq!(out.matches("--full").count(), 1, "{out}");

    let js = json_out(&dir, &["log", "-p", "-2"]);
    assert_eq!(js["count"], "150 of 300 diff lines");
    let commits = js["commits"].as_array().unwrap();
    assert_eq!(commits[0]["files"][0]["hunks"][0]["lines"].as_array().unwrap().len(), 150);
    assert_eq!(commits[1]["files"][0]["hunks"], "1 hunk not shown");
    assert!(commits[0].get("count").is_none() && commits[1].get("count").is_none());
}

#[test]
fn show_second_revision_keeps_its_body() {
    let dir = detail_repo("body");
    std::fs::write(dir.join("f"), "a\nb\nc\nd\ne\n").unwrap();
    git(&dir, &["commit", "-qam", "change f", "-m", "body line"]);
    let js = json_out(&dir, &["show", "HEAD~1", "HEAD"]);
    let commits = js["commits"].as_array().unwrap();
    assert!(commits[0].get("body").is_none(), "{js}");
    assert_eq!(commits[1]["body"], "body line");
    let out = toon(&dir, &["show", "HEAD~1", "HEAD"]);
    assert!(out.contains("body: body line"), "{out}");
}

#[test]
fn show_name_forms_of_two_revisions_print_no_hunks() {
    let dir = detail_repo("name-forms");
    commit_file(&dir, "f", "a\nb\nc\nd\ne\n");
    let names = json_out(&dir, &["show", "--name-only", "HEAD~1", "HEAD"]);
    let commits = names["commits"].as_array().unwrap();
    assert_eq!(commits.len(), 2);
    assert_eq!(commits[1]["files"][0], "f");
    assert!(commits[1].get("total").is_none(), "{names}");
    let out = toon(&dir, &["show", "--name-only", "HEAD~1", "HEAD"]);
    assert!(!out.contains("hunks"), "{out}");

    let status = json_out(&dir, &["show", "--name-status", "HEAD~1", "HEAD"]);
    let commits = status["commits"].as_array().unwrap();
    assert_eq!(commits[0]["files"][0]["status"], "A");
    assert_eq!(commits[1]["files"][0]["path"], "f");
    assert_eq!(commits[1]["files"][0]["status"], "M");
    let out = toon(&dir, &["show", "--name-status", "HEAD~1", "HEAD"]);
    assert!(!out.contains("hunks"), "{out}");

    let numstat = json_out(&dir, &["show", "--numstat", "HEAD~1", "HEAD"]);
    assert_eq!(numstat["commits"][1]["files"][0]["added"], 1);
    let stat = toon(&dir, &["show", "--stat", "HEAD~1", "HEAD"]);
    assert!(!stat.contains("hunks"), "{stat}");
    let quiet = toon(&dir, &["show", "-s", "HEAD~1", "HEAD"]);
    assert!(!quiet.contains("files"), "{quiet}");
}

#[test]
fn log_line_range_lists_the_commits_git_lists() {
    let dir = detail_repo("log-l");
    commit_file(&dir, "f", "A\nb\nc\nd\n");
    commit_file(&dir, "f", "A\nb\nc\nd\ne\n");
    let expected = git(&dir, &["log", "-L1,1:f", "-s", "--format=%H"]);
    let ids: Vec<&str> = expected.lines().collect();
    assert_eq!(ids.len(), 2, "{expected}");
    let out = toon(&dir, &["log", "-p", "-L1,1:f"]);
    let shown: Vec<&str> = out
        .lines()
        .filter_map(|l| l.trim().strip_prefix("commit "))
        .collect();
    assert_eq!(shown, ids, "{out}");
    assert!(!out.contains("commits["), "{out}");
}
