//! Path arguments work as in git: many paths, folders, globs, deleted files,
//! and paths typed from a subfolder.

use std::path::{Path, PathBuf};
use std::process::Command;

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .unwrap();
    assert!(out.status.success(), "git {args:?} failed");
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn rgit(dir: &Path, args: &[&str]) -> (String, bool) {
    let out = Command::new(env!("CARGO_BIN_EXE_rgit"))
        .args(args)
        .current_dir(dir)
        .env("RGIT_OPLOG", "0")
        .output()
        .unwrap();
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        out.status.success(),
    )
}

fn ok(dir: &Path, args: &[&str]) -> String {
    let (out, success) = rgit(dir, args);
    assert!(success, "rgit {args:?}: {out}");
    out
}

fn short(dir: &Path) -> String {
    git(dir, &["status", "--short"])
}

/// A repo with `dir/a`, `dir/sub/b`, `c.txt` and `other/o` committed, and
/// `*.log` ignored.
fn repo(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("rgit-paths-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("dir/sub")).unwrap();
    std::fs::create_dir_all(dir.join("other")).unwrap();
    git(&dir, &["init", "-q", "-b", "main"]);
    git(&dir, &["config", "user.email", "t@t"]);
    git(&dir, &["config", "user.name", "t"]);
    for (p, text) in [
        ("dir/a", "a\n"),
        ("dir/sub/b", "b\n"),
        ("c.txt", "c\n"),
        ("other/o", "o\n"),
        (".gitignore", "*.log\n"),
    ] {
        std::fs::write(dir.join(p), text).unwrap();
    }
    git(&dir, &["add", "."]);
    git(&dir, &["commit", "-qm", "init"]);
    dir
}

#[test]
fn stage_unstage_and_discard_take_many_paths_folders_and_globs() {
    let dir = repo("stage");
    std::fs::write(dir.join("dir/a"), "a2\n").unwrap();
    std::fs::write(dir.join("dir/sub/new"), "n\n").unwrap();
    std::fs::write(dir.join("d.md"), "d\n").unwrap();
    std::fs::write(dir.join("x.log"), "x\n").unwrap();
    std::fs::remove_file(dir.join("c.txt")).unwrap();

    ok(&dir, &["stage", "dir", "c.txt", "*.md"]);
    assert_eq!(short(&dir), "D  c.txt\nA  d.md\nM  dir/a\nA  dir/sub/new\n");

    ok(&dir, &["unstage", "dir", "c.txt"]);
    assert_eq!(short(&dir), " D c.txt\nA  d.md\n M dir/a\n?? dir/sub/new\n");

    ok(&dir, &["discard", "dir", "c.txt"]);
    assert_eq!(short(&dir), "A  d.md\n?? dir/sub/new\n");

    let (out, success) = rgit(&dir, &["stage", "nope"]);
    assert!(!success);
    assert!(
        out.contains("pathspec 'nope' did not match any files"),
        "{out}"
    );
}

#[test]
fn paths_from_a_subfolder_are_relative_to_it() {
    let dir = repo("cwd");
    std::fs::write(dir.join("dir/a"), "a2\n").unwrap();
    std::fs::write(dir.join("c.txt"), "c2\n").unwrap();
    ok(&dir.join("dir"), &["stage", "a", "../c.txt"]);
    assert_eq!(short(&dir), "M  c.txt\nM  dir/a\n");

    let out = ok(&dir.join("dir"), &["status", "--toon", "."]);
    assert!(out.contains("dir/a") && !out.contains("c.txt"), "{out}");
}

#[test]
fn rm_mv_clean_and_status_take_git_forms() {
    let dir = repo("rm");
    let (out, success) = rgit(&dir, &["rm", "other"]);
    assert!(!success && out.contains("without -r"), "{out}");
    ok(&dir, &["rm", "-r", "other", "c.txt"]);
    assert_eq!(short(&dir), "D  c.txt\nD  other/o\n");
    assert!(!dir.join("other").exists());

    std::fs::create_dir(dir.join("dest")).unwrap();
    ok(&dir, &["mv", "dir/a", "dir/sub", "dest"]);
    assert!(dir.join("dest/a").is_file() && dir.join("dest/sub/b").is_file());
    assert!(short(&dir).contains("R  dir/sub/b -> dest/sub/b"));

    std::fs::write(dir.join("x.log"), "x\n").unwrap();
    std::fs::write(dir.join("u.txt"), "u\n").unwrap();
    let out = ok(&dir, &["status", "--toon", "-uno", "--ignored"]);
    assert!(out.contains("x.log") && !out.contains("u.txt"), "{out}");

    ok(&dir, &["clean", "-fX"]);
    assert!(!dir.join("x.log").exists() && dir.join("u.txt").exists());
}

/// git trusts an entry whose size and second match the file, unless it was
/// staged in the index's own second. After rgit rewrites the index, such an
/// entry must still show as changed.
#[test]
fn an_edit_in_the_index_second_stays_visible_after_rgit_writes() {
    use std::time::{Duration, SystemTime};
    let dir = repo("racy");
    let at = |p: &str, ms: u64| {
        let second = SystemTime::now() - Duration::from_secs(10);
        let second = SystemTime::UNIX_EPOCH
            + Duration::from_secs(
                second
                    .duration_since(SystemTime::UNIX_EPOCH)
                    .unwrap()
                    .as_secs(),
            );
        std::fs::File::options()
            .write(true)
            .open(dir.join(p))
            .unwrap()
            .set_modified(second + Duration::from_millis(ms))
            .unwrap();
    };
    std::fs::write(dir.join("c.txt"), "2\n").unwrap();
    at("c.txt", 100);
    git(&dir, &["add", "c.txt"]);
    at(".git/index", 500);
    std::fs::write(dir.join("c.txt"), "3\n").unwrap();
    at("c.txt", 100);

    std::fs::write(dir.join("x.txt"), "x\n").unwrap();
    ok(&dir, &["stage", "x.txt"]);
    assert_eq!(short(&dir), "MM c.txt\nA  x.txt\n");
}

/// `tool args` in `dir` with `input` on stdin, the user's config kept out:
/// stdout and stderr, and whether it succeeded.
fn run_isolated(tool: &str, dir: &Path, args: &[&str], input: &str) -> (String, bool) {
    use std::io::Write;
    use std::process::Stdio;
    let home = dir.with_extension("home");
    std::fs::create_dir_all(&home).unwrap();
    let mut child = Command::new(tool)
        .args(args)
        .current_dir(dir)
        .env("HOME", &home)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("RGIT_OPLOG", "0")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(input.as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    (
        String::from_utf8_lossy(&out.stdout).into_owned() + &String::from_utf8_lossy(&out.stderr),
        out.status.success(),
    )
}

/// Untracked and ignored files, folders, and nested repositories.
fn clean_repo(tag: &str) -> PathBuf {
    let dir = repo(tag);
    for d in ["t", "u/v/nest", "w", "n", "e", "ig/sub"] {
        std::fs::create_dir_all(dir.join(d)).unwrap();
    }
    std::fs::write(dir.join(".gitignore"), "*.log\nig/\n").unwrap();
    std::fs::write(dir.join("t/tracked"), "x\n").unwrap();
    git(&dir, &["add", "."]);
    git(&dir, &["commit", "-qm", "more"]);
    for (p, text) in [
        ("a.txt", "a"),
        ("b.txt", "b"),
        ("l.log", "l"),
        ("t/new", "t"),
        ("t/j.log", "j"),
        ("u/f", "u"),
        ("u/v/g", "v"),
        ("w/i.log", "i"),
        ("w/f", "w"),
        ("ig/sub/q", "q"),
    ] {
        std::fs::write(dir.join(p), text).unwrap();
    }
    git(&dir.join("n"), &["init", "-q"]);
    git(&dir.join("u/v/nest"), &["init", "-q"]);
    dir
}

/// Every path under `dir` but .git.
fn tree(dir: &Path) -> Vec<String> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).unwrap().flatten() {
            if e.file_name() == ".git" {
                continue;
            }
            out.push(e.path().strip_prefix(dir).unwrap().display().to_string());
            if e.file_type().unwrap().is_dir() {
                stack.push(e.path());
            }
        }
    }
    out.sort();
    out
}

#[test]
fn clean_lists_and_removes_what_git_clean_does() {
    let dir = clean_repo("clean-list");
    let rgit = env!("CARGO_BIN_EXE_rgit");
    for args in [
        &["clean", "-n"][..],
        &["clean", "-nd"],
        &["clean", "-ndx"],
        &["clean", "-ndX"],
        &["clean", "-nX"],
        &["clean", "-ndff"],
        &["clean", "-nd", "-e", "b.txt", "-e", "u/v"],
        &["clean", "-nx", "-e", "b.txt"],
        &["clean", "-n", "u"],
        &["clean", "-n", "*.txt"],
        &["clean", "-nd", "u/v"],
        &["clean"],
    ] {
        let want = run_isolated("git", &dir, args, "");
        let mut rargs = vec!["--human"];
        rargs.extend(args);
        let got = run_isolated(rgit, &dir, &rargs, "");
        if args.len() == 1 {
            assert_eq!(got.1, want.1, "{args:?}: {}", got.0);
        } else {
            assert_eq!(got, want, "{args:?}");
        }
    }
    let sub = dir.join("t");
    assert_eq!(
        run_isolated(rgit, &sub, &["--human", "clean", "-nd", ".."], ""),
        run_isolated("git", &sub, &["clean", "-nd", ".."], "")
    );

    for (flags, input) in [
        ("-fd", ""),
        ("-fdx", ""),
        ("-fX", ""),
        ("-fdff", ""),
        ("-id", "2\n*.txt\nnomatch\n\n3\n2-\n\nc\n"),
        ("-id", "?\n\nzz\n4\ny\nn\nyes\n"),
        ("-id", "s\n*\n-1\n\nh\nq\n"),
    ] {
        let a = clean_repo("clean-git");
        let b = clean_repo("clean-rgit");
        let want = run_isolated("git", &a, &["clean", flags], input);
        let got = run_isolated(rgit, &b, &["--human", "clean", flags], input);
        assert_eq!(got, want, "{flags} {input:?}");
        assert_eq!(tree(&b), tree(&a), "{flags} {input:?}");
    }
}

/// `tool args` in `dir` with `env`, the user's config kept out: stdout and
/// whether it succeeded.
fn stdout_of(tool: &str, dir: &Path, args: &[&str], env: &[(&str, &str)]) -> (String, bool) {
    let home = dir.with_extension("home");
    std::fs::create_dir_all(&home).unwrap();
    let out = Command::new(tool)
        .args(args)
        .current_dir(dir)
        .envs(env.iter().copied())
        .env("HOME", &home)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("RGIT_OPLOG", "0")
        .output()
        .unwrap();
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        out.status.success(),
    )
}

/// `rgit --human args` and `git args` in `dir` give the same stdout and
/// success.
fn same(dir: &Path, args: &[&str], env: &[(&str, &str)]) {
    let mut rargs = vec!["--human"];
    rargs.extend(args);
    let got = stdout_of(env!("CARGO_BIN_EXE_rgit"), dir, &rargs, env);
    assert_eq!(got, stdout_of("git", dir, args, env), "{args:?} {env:?}");
}

/// [`repo`] with `*.txt` marked `text`, edits, a staged change and untracked
/// files.
fn magic_repo(tag: &str) -> PathBuf {
    let dir = repo(tag);
    std::fs::write(dir.join("dir/t.txt"), "t\n").unwrap();
    std::fs::write(dir.join(".gitattributes"), "*.txt text\n").unwrap();
    git(&dir, &["add", "."]);
    git(&dir, &["commit", "-qm", "attrs"]);
    for p in ["dir/a", "dir/sub/b", "c.txt", "other/o", "dir/t.txt"] {
        std::fs::write(dir.join(p), "2\n").unwrap();
    }
    std::fs::write(dir.join("new.txt"), "n\n").unwrap();
    std::fs::write(dir.join("dir/new"), "m\n").unwrap();
    git(&dir, &["add", "dir/sub/b", "c.txt"]);
    dir
}

const MAGIC: &[&str] = &[
    ":!dir",
    ":^dir/sub",
    ":(exclude)*.txt",
    ":(icase)DIR/A",
    ":(glob)*.txt",
    ":(glob)**/b",
    ":(glob)dir/*",
    ":(literal)*.txt",
    ":/c.txt",
    ":(top,icase)C.TXT",
    ":(attr:text)",
    ":(attr:-text)",
];

#[test]
fn pathspec_magic_selects_what_git_selects() {
    let dir = magic_repo("magic-read");
    for spec in MAGIC.iter().chain(&[":(bogus)x", ":(literal,glob)x"]) {
        for args in [
            &["status", "--short", "--"][..],
            &["diff", "--stat", "--"],
            &["ls-files", "--"],
            &["log", "--format=%s", "--"],
            &["grep", "-n", ".", "--"],
        ] {
            let mut args = args.to_vec();
            args.push(spec);
            same(&dir, &args, &[]);
        }
    }
    let sub = dir.join("dir");
    for spec in [":!a", ":(icase)T.TXT", ":(top)c.txt", ":!sub"] {
        same(&sub, &["status", "--short", "--", spec], &[]);
        same(&sub, &["ls-files", "--", spec], &[]);
        same(&sub, &["grep", "-n", ".", "--", spec], &[]);
    }
    for (env, spec) in [
        (("GIT_GLOB_PATHSPECS", "1"), "*.txt"),
        (("GIT_NOGLOB_PATHSPECS", "1"), "*.txt"),
        (("GIT_ICASE_PATHSPECS", "1"), "DIR"),
        (("GIT_LITERAL_PATHSPECS", "1"), ":/c.txt"),
    ] {
        same(&dir, &["status", "--short", "--", spec], &[env]);
        same(&dir, &["ls-files", "--", spec], &[env]);
        same(&dir, &["diff", "--stat", "--", spec], &[env]);
    }
    let both = [("GIT_GLOB_PATHSPECS", "1"), ("GIT_NOGLOB_PATHSPECS", "1")];
    same(&dir, &["status", "--short", "--", "c.txt"], &both);
    same(&dir, &["--glob-pathspecs", "ls-files", "--", "*.txt"], &[]);
    same(
        &dir,
        &["--icase-pathspecs", "status", "--short", "--", "C.TXT"],
        &[],
    );
}

#[test]
fn pathspec_magic_changes_what_git_changes() {
    let state = |dir: &Path| {
        let stashes = stdout_of("git", dir, &["stash", "list", "--format=stash"], &[]);
        git(dir, &["status", "--short"]) + &stashes.0
    };
    let list = std::env::temp_dir().join(format!("rgit-paths-{}-list", std::process::id()));
    std::fs::write(&list, "c.txt\n:(glob)dir/*\n").unwrap();
    let from_file = format!("--pathspec-from-file={}", list.display());
    for spec in [
        ":!dir",
        ":(exclude)*.txt",
        ":(icase)DIR/A",
        ":(glob)dir/*",
        ":(attr:text)",
    ] {
        for cmd in [
            &["add"][..],
            &["add", "-u"],
            &["rm", "-q", "-r", "--cached"],
            &["restore"],
            &["restore", "--staged"],
            &["checkout"],
            &["reset", "-q"],
            &["stash", "push", "-q"],
            &["commit", "-qm", "x"],
        ] {
            let (a, b) = (magic_repo("magic-git"), magic_repo("magic-rgit"));
            let mut args = cmd.to_vec();
            args.extend(["--", spec]);
            let want = stdout_of("git", &a, &args, &[]).1;
            let mut rargs = vec!["--human"];
            rargs.extend(&args);
            let got = stdout_of(env!("CARGO_BIN_EXE_rgit"), &b, &rargs, &[]).1;
            assert_eq!((got, state(&b)), (want, state(&a)), "{args:?}");
            if spec == ":!dir" {
                let (a, b) = (magic_repo("magic-git"), magic_repo("magic-rgit"));
                let mut args = cmd.to_vec();
                args.push(&from_file);
                stdout_of("git", &a, &args, &[]);
                let mut rargs = vec!["--human"];
                rargs.extend(&args);
                stdout_of(env!("CARGO_BIN_EXE_rgit"), &b, &rargs, &[]);
                assert_eq!(state(&b), state(&a), "{args:?}");
            }
        }
    }
    let (a, b) = (magic_repo("magic-git"), magic_repo("magic-rgit"));
    let sub = |d: &Path| d.join("dir");
    stdout_of("git", &sub(&a), &["add", "--", ":!new", ":(top)c.txt"], &[]);
    stdout_of(
        env!("CARGO_BIN_EXE_rgit"),
        &sub(&b),
        &["--human", "add", "--", ":!new", ":(top)c.txt"],
        &[],
    );
    assert_eq!(state(&b), state(&a));
}
