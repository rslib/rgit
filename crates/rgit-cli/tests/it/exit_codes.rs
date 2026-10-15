//! Exit codes: where git exits 1 to report no result, rgit exits 1 in every
//! output mode.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const MARKERS: [&str; 9] = [
    "AI_AGENT",
    "CLAUDECODE",
    "CODEX_SANDBOX",
    "CODEX_THREAD_ID",
    "GEMINI_CLI",
    "CURSOR_AGENT",
    "OPENCODE",
    "OPENCODE_CLIENT",
    "AGENT",
];

fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// A repo on `main` with two commits, `f` containing `hello`.
fn repo(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("rgit-exit-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    git(&dir, &["init", "-q", "-b", "main"]);
    git(&dir, &["config", "user.email", "t@t"]);
    git(&dir, &["config", "user.name", "t"]);
    for (text, message) in [("one\n", "first"), ("hello\n", "second")] {
        std::fs::write(dir.join("f"), text).unwrap();
        git(&dir, &["add", "f"]);
        git(&dir, &["commit", "-qm", message]);
    }
    dir
}

fn run(dir: &Path, mode: &str, args: &[&str]) -> Output {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_rgit"));
    cmd.current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CEILING_DIRECTORIES", std::env::temp_dir())
        .env("GIT_PAGER", "cat")
        .env("RGIT_CONFIG", dir.join("no-such-config"))
        .env("RGIT_OPLOG", "0")
        .env_remove("RGIT_OUTPUT");
    for marker in MARKERS {
        cmd.env_remove(marker);
    }
    if mode == "agent" {
        cmd.env("CLAUDECODE", "1");
    } else {
        cmd.arg(mode);
    }
    cmd.args(args).output().unwrap()
}

fn git_code(dir: &Path, args: &[&str]) -> Option<i32> {
    Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_PAGER", "cat")
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap()
        .status
        .code()
}

/// Each of `cases` exits `want` in git and in every rgit output mode.
fn same_as_git(dir: &Path, cases: &[&[&str]], want: i32) {
    for args in cases {
        assert_eq!(git_code(dir, args), Some(want), "git {args:?}");
        for mode in MODES {
            assert_eq!(
                run(dir, mode, args).status.code(),
                Some(want),
                "{mode} {args:?}"
            );
        }
    }
}

const MODES: [&str; 4] = ["--human", "--toon", "--json", "agent"];

/// `args` exit 1 in every output mode; `shown` is in the TOON and JSON stdout.
fn exits_1(dir: &Path, args: &[&str], shown: &str) {
    for mode in MODES {
        let out = run(dir, mode, args);
        assert_eq!(
            out.status.code(),
            Some(1),
            "{mode} {args:?}: {}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        if mode != "--human" {
            let stdout = String::from_utf8_lossy(&out.stdout);
            assert!(stdout.contains(shown), "{mode} {args:?}: {stdout}");
        }
    }
}

#[test]
fn no_result_grep_exits_1() {
    let dir = repo("grep");
    exits_1(&dir, &["grep", "zzq"], "0 matches");
}

#[test]
fn no_result_grep_variants_exit_1() {
    let dir = repo("grep-variants");
    for args in [
        &["grep", "-q", "zzq"][..],
        &["grep", "-c", "zzq"],
        &["grep", "-l", "zzq"],
        &["grep", "--no-index", "zzq", "f"],
    ] {
        for mode in MODES {
            let out = run(&dir, mode, args);
            assert_eq!(out.status.code(), Some(1), "{mode} {args:?}");
        }
    }
}

#[test]
fn no_result_is_ancestor_exits_1() {
    let dir = repo("ancestor");
    let args = ["merge-base", "--is-ancestor", "HEAD", "HEAD~1"];
    exits_1(&dir, &args, "");
    let json = String::from_utf8_lossy(&run(&dir, "--json", &args).stdout).into_owned();
    assert!(json.contains("\"ancestor\":false"), "{json}");
}

#[test]
fn no_result_check_ignore_exits_1() {
    let dir = repo("check-ignore");
    exits_1(&dir, &["check-ignore", "f"], "ignored");
    exits_1(&dir, &["check-ignore", "-q", "f"], "");
    exits_1(&dir, &["check-ignore", "nope"], "ignored");
}

#[test]
fn no_result_show_ref_exits_1() {
    let dir = repo("show-ref");
    exits_1(&dir, &["show-ref", "nope"], "0 matching refs");
}

#[test]
fn no_result_branch_delete_exits_1() {
    let dir = repo("branch-d");
    exits_1(&dir, &["branch", "-d", "nope"], "branch 'nope' not found");
    exits_1(&dir, &["branch", "-D", "nope"], "branch 'nope' not found");
}

#[test]
fn no_result_rgit_branch_delete_is_a_noop() {
    let dir = repo("branch-delete");
    for mode in ["--human", "--toon", "--json"] {
        let out = run(&dir, mode, &["branch", "delete", "gone"]);
        assert_eq!(out.status.code(), Some(0), "{mode}");
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            stdout.contains("does not exist (no-op)"),
            "{mode}: {stdout}"
        );
    }
}

#[test]
fn no_result_rgit_branch_create_is_a_noop() {
    let dir = repo("branch-create");
    for mode in MODES {
        let out = run(&dir, mode, &["branch", "create", "main"]);
        assert_eq!(out.status.code(), Some(0), "{mode}");
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            stdout.contains("already exists (no-op)"),
            "{mode}: {stdout}"
        );
    }
}

#[test]
fn error_unguarded_read_failures_keep_their_text() {
    let dir = repo("error-read");
    same_as_git(&dir, &[&["commit", "-F", "nope"]], 128);
    for mode in MODES {
        let out = run(&dir, mode, &["commit", "-F", "nope"]);
        let text = String::from_utf8_lossy(&out.stdout).into_owned()
            + &String::from_utf8_lossy(&out.stderr);
        assert!(!text.contains("for reading"), "{mode}: {text}");
    }
}

#[test]
fn no_result_tag_delete_exits_1() {
    let dir = repo("tag-d");
    exits_1(&dir, &["tag", "-d", "nope"], "tag 'nope' not found.");
}

#[test]
fn no_result_reflog_exists_exits_1() {
    let dir = repo("reflog");
    exits_1(&dir, &["reflog", "exists", "refs/heads/nope"], "exists");
}

#[test]
fn no_result_match_exits_0() {
    let dir = repo("match");
    for mode in MODES {
        let out = run(&dir, mode, &["grep", "hello"]);
        assert_eq!(out.status.code(), Some(0), "{mode}");
    }
}

#[test]
fn same_code_errors_match_human() {
    let dir = repo("same-code");
    for args in [
        &["show", "nope"][..],
        &["cat-file", "-t", "nope"],
        &["cat-file", "-p", "nope"],
        &["rev-list", "nope"],
        &["ls-tree", "nope"],
        &["notes", "list", "nope"],
        &["blame", "nope"],
        &["shortlog", "-s", "nope"],
        &["switch", "nope"],
        &["rm", "nope"],
        &["mv", "nope", "x"],
        &["add", "nope"],
        &["cherry-pick", "nope"],
        &["revert", "nope"],
        &["update-ref", "-d", "refs/heads/nope", "deadbeef"],
        &["merge-base", "HEAD", "nope"],
    ] {
        let human = run(&dir, "--human", args).status.code();
        assert_eq!(human, Some(128), "--human {args:?}");
        for mode in ["--toon", "--json", "agent"] {
            assert_eq!(
                run(&dir, mode, args).status.code(),
                human,
                "{mode} {args:?}"
            );
        }
    }
}

#[test]
fn same_code_usage_error() {
    let dir = repo("same-code-usage");
    for (args, code) in [
        (&["log", "--oneline", "-n", "abc"][..], 128),
        (&["log", "-n"][..], 128),
        (&["status", "--nope"][..], 129),
    ] {
        for mode in MODES {
            assert_eq!(
                run(&dir, mode, args).status.code(),
                Some(code),
                "{mode} {args:?}"
            );
        }
    }
}

#[test]
fn error_fatal_codes_match_git() {
    let dir = repo("error-fatal");
    same_as_git(
        &dir,
        &[
            &["show-ref", "--verify", "refs/heads/nope"],
            &["cat-file", "-e", "deadbeef"],
            &["symbolic-ref", "nope"],
            &["branch", "-m", "nope", "x"],
            &["config", "--get", "--type=bool", "user.name"],
            &["blame", "nope"],
            &["reflog", "show", "nope"],
            &["remote", "show", "nope"],
            &["rebase", "nope"],
            &["apply", "nope.patch"],
            &["am", "nope.mbox"],
            &["hash-object", "nope"],
        ],
        128,
    );
}

#[test]
fn error_remote_codes_match_git() {
    let dir = repo("error-remote");
    same_as_git(
        &dir,
        &[
            &["remote", "get-url", "nope"],
            &["remote", "remove", "nope"],
        ],
        2,
    );
}

#[test]
fn error_var_usage() {
    let dir = repo("error-var");
    same_as_git(&dir, &[&["var", "NOPE"]], 129);
}

#[test]
fn error_rev_parse_quiet_exits_1() {
    let dir = repo("error-quiet");
    same_as_git(
        &dir,
        &[
            &["rev-parse", "-q", "--verify", "nope"],
            &["symbolic-ref", "-q", "nope"],
            &["show-ref", "-q", "--verify", "refs/heads/nope"],
        ],
        1,
    );
}

#[test]
fn error_create_existing_name_exits_128() {
    let dir = repo("error-existing");
    git(&dir, &["tag", "v1"]);
    same_as_git(
        &dir,
        &[
            &["branch", "main"],
            &["tag", "v1"],
            &["checkout", "-b", "main"],
            &["switch", "-c", "main"],
        ],
        128,
    );
}

fn git_out(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// A repo whose `main` was rewound to its first commit, with `side` on its old tip.
fn rewound(tag: &str) -> PathBuf {
    let dir = repo(tag);
    git(&dir, &["branch", "side"]);
    git(&dir, &["checkout", "-q", "side"]);
    std::fs::write(dir.join("g"), "g\n").unwrap();
    git(&dir, &["add", "g"]);
    git(&dir, &["commit", "-qm", "third"]);
    git(&dir, &["branch", "-f", "main", "HEAD~2"]);
    dir
}

#[test]
fn flag_fork_point_matches_git() {
    let dir = rewound("fork-point");
    let plain = git_out(&dir, &["merge-base", "main", "side"]);
    let fork = git_out(&dir, &["merge-base", "--fork-point", "main", "side"]);
    assert_ne!(plain, fork);
    for args in [
        &["merge-base", "--fork-point", "main", "side"][..],
        &["merge-base", "--fork-point", "main"],
    ] {
        assert_eq!(git_code(&dir, args), Some(0));
        let human = run(&dir, "--human", args);
        assert_eq!(human.status.code(), Some(0), "{args:?}");
        assert_eq!(String::from_utf8_lossy(&human.stdout), fork, "{args:?}");
        for mode in ["--toon", "--json", "agent"] {
            let out = run(&dir, mode, args);
            assert_eq!(out.status.code(), Some(0), "{mode} {args:?}");
            assert!(
                String::from_utf8_lossy(&out.stdout).contains(fork.trim()),
                "{mode} {args:?}"
            );
        }
    }
}

#[test]
fn flag_fork_point_without_one_exits_1() {
    let dir = repo("fork-point-none");
    git(&dir, &["checkout", "-q", "--orphan", "other"]);
    git(&dir, &["commit", "-qm", "unrelated", "--allow-empty"]);
    same_as_git(&dir, &[&["merge-base", "--fork-point", "main", "other"]], 1);
}

#[test]
fn flag_show_ref_exists_matches_git() {
    let dir = repo("exists");
    same_as_git(&dir, &[&["show-ref", "--exists", "refs/heads/nope"]], 2);
    same_as_git(&dir, &[&["show-ref", "--exists", "refs/heads/main"]], 0);
    let out = run(
        &dir,
        "--human",
        &["show-ref", "--exists", "refs/heads/main"],
    );
    assert!(out.stdout.is_empty());
}

#[test]
fn flag_log_exit_code_matches_git() {
    let dir = repo("log-exit");
    for args in [
        &["log", "-1", "--exit-code"][..],
        &["log", "-1", "--exit-code", "--format="],
        &["log", "-1", "--exit-code", "-p"],
        &["log", "-1", "--exit-code", "--stat"],
        &["log", "-1", "--exit-code", "--oneline"],
        &["log", "-1", "--exit-code", "--name-only"],
        &["log", "--exit-code", "HEAD..HEAD"],
        &["log", "-1", "--exit-code", "-p", "--", "nopath"],
        &["log", "-1", "--exit-code", "--format=%s", "--", "f"],
    ] {
        let want = git_code(&dir, args);
        for mode in MODES {
            assert_eq!(run(&dir, mode, args).status.code(), want, "{mode} {args:?}");
        }
    }
}

/// Two commits, tag `v1` and branch `side` on the first, a modified `f`, and an
/// ignored, untracked `ign`.
fn sweep_repo(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("rgit-exit-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    git(&dir, &["init", "-q", "-b", "main"]);
    git(&dir, &["config", "user.email", "t@t"]);
    git(&dir, &["config", "user.name", "t"]);
    git(&dir, &["config", "commit.gpgsign", "false"]);
    std::fs::write(dir.join(".gitignore"), "ign\n").unwrap();
    std::fs::write(dir.join("f"), "one\n").unwrap();
    git(&dir, &["add", "."]);
    git(&dir, &["commit", "-qm", "A"]);
    git(&dir, &["tag", "v1"]);
    git(&dir, &["branch", "side"]);
    std::fs::write(dir.join("f"), "hello\n").unwrap();
    git(&dir, &["commit", "-qam", "B"]);
    std::fs::write(dir.join("f"), "hello\nmore\n").unwrap();
    std::fs::write(dir.join("ign"), "x\n").unwrap();
    dir
}

/// Commands that fail or only read, so the order does not matter.
const SWEEP: &[&str] = &[
    "grep zzq",
    "grep -q zzq",
    "grep -c zzq",
    "grep -l zzq",
    "grep -L a",
    "grep a",
    "grep -q a",
    "grep --no-index zzq f",
    "log --grep=zzq",
    "log -1 --exit-code",
    "log nope",
    "log -1",
    "show nope",
    "show HEAD",
    "diff --exit-code",
    "diff --quiet",
    "diff --cached --exit-code",
    "diff --no-index f f",
    "diff --no-index --exit-code f .gitignore",
    "diff nope",
    "diff-index --quiet HEAD",
    "diff-index --exit-code HEAD",
    "diff-files --quiet",
    "diff-files --exit-code",
    "diff-tree --exit-code HEAD~1 HEAD",
    "diff-tree -s --exit-code HEAD HEAD",
    "merge-base --is-ancestor HEAD HEAD~1",
    "merge-base --is-ancestor HEAD~1 HEAD",
    "merge-base HEAD nope",
    "merge-base HEAD side",
    "merge-base --fork-point main side",
    "check-ignore f",
    "check-ignore ign",
    "check-ignore -q f",
    "check-ignore nope",
    "check-attr text f",
    "check-ref-format refs/heads/a..b",
    "check-ref-format refs/heads/ok",
    "check-ref-format --branch a..b",
    "check-mailmap nobody",
    "show-ref nope",
    "show-ref v1",
    "show-ref --verify refs/heads/nope",
    "show-ref --verify refs/heads/main",
    "show-ref -q --verify refs/heads/nope",
    "show-ref --exists refs/heads/nope",
    "show-ref --exists refs/heads/main",
    "cat-file -e deadbeef",
    "cat-file -e HEAD",
    "cat-file -t nope",
    "cat-file -p nope",
    "cat-file -t HEAD",
    "rev-parse nope",
    "rev-parse --verify nope",
    "rev-parse -q --verify nope",
    "rev-parse --verify HEAD",
    "rev-parse --abbrev-ref nope",
    "rev-list nope",
    "rev-list --count HEAD",
    "symbolic-ref HEAD",
    "symbolic-ref -q HEAD",
    "symbolic-ref nope",
    "name-rev nope",
    "describe",
    "describe --tags",
    "describe nope",
    "for-each-ref refs/nope",
    "branch --list nope",
    "branch -d nope",
    "branch -D nope",
    "branch -m nope x",
    "tag -l nope",
    "tag -d nope",
    "tag -v v1",
    "stash list",
    "stash show",
    "stash drop",
    "stash pop",
    "config --get nope.key",
    "config --get-all nope.key",
    "config --get-regexp nope",
    "config --get --type=bool user.name",
    "config nope",
    "var NOPE",
    "var GIT_AUTHOR_IDENT",
    "ls-files --error-unmatch nope",
    "ls-files nope",
    "ls-tree nope",
    "ls-tree HEAD nope",
    "ls-remote --exit-code . refs/nope",
    "ls-remote . refs/nope",
    "notes show",
    "notes list nope",
    "blame nope",
    "blame f",
    "shortlog -s nope",
    "reflog show nope",
    "reflog exists refs/heads/nope",
    "reflog exists refs/heads/main",
    "worktree list",
    "remote get-url nope",
    "remote show nope",
    "remote remove nope",
    "submodule status",
    "fsck",
    "verify-commit HEAD",
    "verify-tag v1",
    "status",
    "status --porcelain",
    "checkout nope",
    "switch nope",
    "restore nope",
    "rm nope",
    "mv nope x",
    "add nope",
    "reset nope",
    "cherry-pick nope",
    "revert nope",
    "merge nope",
    "rebase nope",
    "apply nope.patch",
    "am nope.mbox",
    "commit -m x",
    "help nope",
    "nope-command",
    "log --oneline -n abc",
    "hash-object nope",
    "mktag",
    "update-ref -d refs/heads/nope deadbeef",
    "branch main",
    "tag v1",
    "checkout -b main",
    "switch -c main",
    "log -1 --exit-code --format=",
    "log -1 --exit-code -p",
    "log --exit-code HEAD..HEAD",
    "merge-base --fork-point main",
    "log -n",
    "status --nope",
];

/// Commands whose exit code changes between git releases: `help nope` is 1 in
/// git 2.50 and 16 in 2.55, and `stash show/drop/pop` with no stash is 1 in
/// 2.55 and 128 in 2.56. For these the modes must agree with each other only.
const VARIES_BY_GIT_VERSION: &[&str] = &["help nope", "stash show", "stash drop", "stash pop"];

#[test]
fn matches_git_sweep() {
    let dir = sweep_repo("sweep");
    let mut mismatches = Vec::new();
    for line in SWEEP {
        let args: Vec<&str> = line.split_whitespace().collect();
        let codes: Vec<Option<i32>> = ["--human", "--toon", "--json"]
            .iter()
            .map(|mode| run(&dir, mode, &args).status.code())
            .collect();
        let git = if VARIES_BY_GIT_VERSION.contains(line) {
            codes[0]
        } else {
            git_code(&dir, &args)
        };
        if codes.iter().any(|c| *c != git) {
            mismatches.push(format!(
                "{line}: git={git:?} human={:?} toon={:?} json={:?}",
                codes[0], codes[1], codes[2]
            ));
        }
    }
    assert!(SWEEP.len() >= 100);
    assert!(
        mismatches.is_empty(),
        "exit codes differ from git:\n{}",
        mismatches.join("\n")
    );
}
