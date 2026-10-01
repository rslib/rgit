//! sparse-checkout, and the commands that honor it, leave the same patterns,
//! config, skip-worktree bits and files as git, with git's messages.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

fn run(program: &str, dir: &Path, args: &[&str], stdin: &str) -> (String, String, i32) {
    let mut cmd = Command::new(program);
    if program != "git" {
        cmd.arg("--human");
    }
    let mut child = cmd
        .args(args)
        .current_dir(dir)
        .env("RGIT_OPLOG", "0")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    std::io::Write::write_all(&mut child.stdin.take().unwrap(), stdin.as_bytes()).unwrap();
    let out = child.wait_with_output().unwrap();
    // The prefix of a fatal error is the one difference allowed.
    let err: String = String::from_utf8_lossy(&out.stderr)
        .lines()
        .map(|l| format!("{}\n", l.strip_prefix("rgit: ").unwrap_or(l)))
        .map(|l| l.strip_prefix("fatal: ").map(str::to_owned).unwrap_or(l))
        .collect();
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        err,
        out.status.code().unwrap_or(-1),
    )
}

fn git(dir: &Path, args: &[&str]) -> String {
    let (out, err, code) = run("git", dir, args, "");
    assert_eq!(code, 0, "git {args:?}: {err}");
    out
}

/// The repo each side starts from: top-level files, nested folders, a
/// folder with a space, an ignore rule, and an `other` branch.
fn repo(dir: &Path) {
    let _ = std::fs::remove_dir_all(dir);
    for d in ["a/b", "c", "d", "e f"] {
        std::fs::create_dir_all(dir.join(d)).unwrap();
    }
    git(dir, &["init", "-q", "-b", "main"]);
    git(dir, &["config", "user.email", "t@t"]);
    git(dir, &["config", "user.name", "t"]);
    for p in ["top", "a/x", "a/b/y", "c/z", "d/w", "e f/q"] {
        std::fs::write(dir.join(p), "1\n").unwrap();
    }
    std::fs::write(dir.join(".gitignore"), "*.log\n").unwrap();
    git(dir, &["add", "."]);
    git(dir, &["commit", "-qm", "init"]);
    git(dir, &["checkout", "-qb", "other"]);
    for p in ["top", "a/b/y", "c/z"] {
        std::fs::write(dir.join(p), "2\n").unwrap();
    }
    git(dir, &["rm", "-q", "d/w"]);
    git(dir, &["commit", "-qam", "two"]);
    git(dir, &["checkout", "-q", "main"]);
}

/// Everything sparsity touches: the patterns, the worktree config, the
/// skip-worktree bits, the files on disk, status and HEAD.
fn state(dir: &Path) -> String {
    let read = |p: &str| std::fs::read_to_string(dir.join(p)).unwrap_or_default();
    let files: Vec<String> = walk(dir, dir);
    [
        read(".git/info/sparse-checkout"),
        read(".git/config.worktree"),
        git(dir, &["ls-files", "-t"]),
        files.join("\n"),
        git(dir, &["status", "--porcelain"]),
        git(dir, &["log", "-1", "--format=%s %D"]),
    ]
    .join("\n--\n")
}

fn walk(top: &Path, dir: &Path) -> Vec<String> {
    let mut out = Vec::new();
    for e in std::fs::read_dir(dir).unwrap().flatten() {
        let path = e.path();
        if path.file_name().is_some_and(|n| n == ".git") {
            continue;
        }
        if path.is_dir() {
            out.extend(walk(top, &path));
        } else {
            out.push(path.strip_prefix(top).unwrap().display().to_string());
        }
    }
    out.sort();
    out
}

/// Run each step with git in one copy of the repo and rgit in another, and
/// require the same output, exit code and resulting state. A step starting
/// with `!` is shell setup run in both; one with `?` checks only the exit
/// code and the state.
fn same(tag: &str, steps: &[&str]) {
    let base = std::env::temp_dir().join(format!("rgit-sparse-{}-{tag}", std::process::id()));
    let (g, r): (PathBuf, PathBuf) = (base.join("g"), base.join("r"));
    repo(&g);
    repo(&r);
    for step in steps {
        if let Some(sh) = step.strip_prefix('!') {
            for dir in [&g, &r] {
                let ok = Command::new("sh")
                    .args(["-c", sh])
                    .current_dir(dir)
                    .status()
                    .unwrap();
                assert!(ok.success(), "{sh}");
            }
            continue;
        }
        // `?`: only the exit code and the state need match.
        let (step, quiet) = match step.strip_prefix('?') {
            Some(s) => (s, true),
            None => (*step, false),
        };
        let (step, stdin) = step.split_once(" <<< ").unwrap_or((step, ""));
        let stdin = stdin.replace("\\n", "\n");
        let args = split(step);
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        let mut want = run("git", &g, &args, &stdin);
        // git 2.52 added `clean`, which rgit does not have yet.
        want.1 = want.1.replace(" | clean)", ")");
        let mut got = run(env!("CARGO_BIN_EXE_rgit"), &r, &args, &stdin);
        // rgit's human add and mv say "ok" where git says nothing.
        if got.0 == "ok\n" {
            got.0.clear();
        }
        if quiet {
            assert_eq!(got.2, want.2, "{tag}: {step}");
        } else {
            assert_eq!(got, want, "{tag}: {step}");
        }
        assert_eq!(state(&r), state(&g), "{tag}: state after {step}");
    }
    let _ = std::fs::remove_dir_all(&base);
}

/// Words split on spaces, with '...' quoting.
fn split(s: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut word = String::new();
    let (mut quoted, mut started) = (false, false);
    for c in s.chars() {
        match c {
            '\'' => {
                quoted = !quoted;
                started = true;
            }
            ' ' if !quoted => {
                if started {
                    words.push(std::mem::take(&mut word));
                }
                started = false;
            }
            c => {
                word.push(c);
                started = true;
            }
        }
    }
    if started {
        words.push(word);
    }
    words
}

#[test]
fn cone_mode_writes_the_patterns_and_bits_git_does() {
    same(
        "cone",
        &[
            "sparse-checkout list",
            "sparse-checkout add a",
            "sparse-checkout reapply",
            "sparse-checkout init",
            "sparse-checkout list",
            "sparse-checkout set a/b d/",
            "sparse-checkout add c",
            "sparse-checkout list",
            "sparse-checkout add a",
            "sparse-checkout set --stdin <<< c\\n\"e f\"\\n",
            "sparse-checkout list",
            "!cd a 2>/dev/null || mkdir a",
            "sparse-checkout set --skip-checks 'x*y' 'e f'",
            "sparse-checkout list",
            "sparse-checkout disable",
            "sparse-checkout disable",
            "sparse-checkout set --no-sparse-index a",
        ],
    );
}

#[test]
fn cone_mode_checks_and_usage_errors_match_git() {
    same(
        "checks",
        &[
            "sparse-checkout set /a",
            "sparse-checkout set '!a'",
            "sparse-checkout set 'a*'",
            "sparse-checkout set top",
            "sparse-checkout frob",
            "sparse-checkout",
            "sparse-checkout set --bogus",
            "sparse-checkout list -x",
            "!printf '/*\\n!/*/\\n/a/b/*.txt\\n' > .git/info/sparse-checkout",
            "!git config core.sparseCheckout true",
            "sparse-checkout list",
            "sparse-checkout add c",
        ],
    );
}

#[test]
fn dirty_and_untracked_files_stay_with_git_warnings() {
    same(
        "dirty",
        &[
            "sparse-checkout set a",
            "!echo dirty >> a/x",
            "sparse-checkout set d",
            "!git checkout a/x",
            "sparse-checkout reapply",
            "!mkdir -p c && echo x > c/o.log",
            "!mkdir -p a/b && echo u > a/b/untracked",
            "sparse-checkout set 'e f'",
        ],
    );
}

#[test]
fn non_cone_patterns_and_check_rules_match_git() {
    same(
        "noncone",
        &[
            "sparse-checkout init --no-cone",
            "sparse-checkout set --no-cone '/*' '!/a/' 'a/b/'",
            "sparse-checkout list",
            "sparse-checkout add c",
            "sparse-checkout check-rules <<< a/x\\nc/z\\nd/w\\ntop\\n",
            "sparse-checkout check-rules --cone --rules-file /dev/null <<< a/x\\ntop\\n",
            "sparse-checkout set top a",
            "sparse-checkout set",
            "!printf '# c\\n/*\\n\\n!/*/  \\n' > .git/info/sparse-checkout",
            "sparse-checkout list",
        ],
    );
}

#[test]
fn checkout_reset_stash_and_merge_keep_the_sparse_tree() {
    same(
        "ops",
        &[
            "sparse-checkout set a",
            "switch -q other",
            "switch -q main",
            "reset -q --hard other",
            "reset -q --hard main",
            "reset -q other",
            "reset -q --hard main",
            "!echo m > a/x",
            "?stash",
            "?stash pop",
            "reset -q --hard",
            "merge -q other",
            "status --long",
            "status --short",
            "diff --stat",
            "ls-files -v",
            "ls-files -t -m -d",
        ],
    );
}

#[test]
fn add_rm_and_mv_refuse_paths_outside_without_sparse() {
    same(
        "paths",
        &[
            "sparse-checkout set a",
            "!echo 3 > a/x",
            "!mkdir -p q && echo 1 > q/u",
            "add c/z",
            "add c",
            "add -A",
            "add --sparse c/z",
            "rm -q --cached a/x c/z",
            "reset -q",
            "rm --cached --sparse c/z",
            "reset -q --hard",
            "mv c/z a/z2",
            "mv a/x c/x2",
            "mv --sparse c/z a/z2",
            "reset -q --hard",
            "mv --sparse a/x c/x2",
            "reset -q --hard",
            "add --sparse q/u",
            "?commit -qam wip",
        ],
    );
}
