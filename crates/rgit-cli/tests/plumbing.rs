//! git's read-only plumbing commands run natively and print exactly what git
//! prints.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn run(bin: &str, dir: &Path, args: &[&str], env: &[(&str, &str)]) -> Output {
    Command::new(bin)
        .args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("RGIT_OPLOG", "0")
        .envs(env.iter().copied())
        .output()
        .unwrap()
}

fn git(dir: &Path, args: &[&str], env: &[(&str, &str)]) -> String {
    let out = run("git", dir, args, env);
    assert!(out.status.success(), "git {args:?}: {out:?}");
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn rgit(dir: &Path, args: &[&str]) -> Output {
    let mut all = vec!["--human"];
    all.extend(args);
    run(env!("CARGO_BIN_EXE_rgit"), dir, &all, &[])
}

/// rgit and git print the same bytes and agree on success, for every case.
fn same(dir: &Path, cases: &[&[&str]]) {
    let mut bad = Vec::new();
    for args in cases {
        let want = run("git", dir, args, &[]);
        let got = rgit(dir, args);
        if got.stdout != want.stdout || got.status.success() != want.status.success() {
            bad.push(format!(
                "{args:?}\n  git  ({}): {:?}\n  rgit ({}): {:?} {}",
                want.status,
                String::from_utf8_lossy(&want.stdout),
                got.status,
                String::from_utf8_lossy(&got.stdout),
                String::from_utf8_lossy(&got.stderr)
            ));
        }
    }
    assert!(bad.is_empty(), "{}", bad.join("\n"));
}

fn commit(dir: &Path, n: u32, message: &str, author: &str) {
    let date = format!("2024-01-0{n}T10:00:00+0200");
    let email = format!("{}@example.com", author.to_lowercase());
    git(
        dir,
        &["commit", "-q", "-m", message],
        &[
            ("GIT_AUTHOR_DATE", &date),
            ("GIT_COMMITTER_DATE", &date),
            ("GIT_AUTHOR_NAME", author),
            ("GIT_AUTHOR_EMAIL", &email),
        ],
    );
}

fn write(dir: &Path, path: &str, text: &[u8]) {
    let p = dir.join(path);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, text).unwrap();
}

/// main: c1, c2, a merge of side; side: c3 by Alice, tagged v1 (annotated);
/// lw is a lightweight tag. The working tree has a modified, a deleted, an
/// untracked and some ignored files.
fn repo(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("rgit-plumbing-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    git(&dir, &["init", "-q", "-b", "main"], &[]);
    git(&dir, &["config", "user.email", "t@example.com"], &[]);
    git(&dir, &["config", "user.name", "T"], &[]);
    for (p, text) in [
        ("dir/a", &b"alpha\nbeta aa\n"[..]),
        ("dir/sub/b", b"bee\n"),
        ("c.txt", b"one\nFoo bar\nfoobar\na+b\n"),
        (".gitignore", b"*.log\nbuild/\n"),
        ("dir/.gitignore", b"# temp\n*.tmp\n"),
        ("bin.dat", b"a\0binary\n"),
    ] {
        write(&dir, p, text);
    }
    git(&dir, &["add", "."], &[]);
    commit(&dir, 1, "first", "T");
    write(&dir, "c.txt", b"one\nFoo bar\nfoobar\na+b\ntwo\n");
    write(&dir, "e.txt", b"echo\n");
    git(&dir, &["add", "."], &[]);
    commit(&dir, 2, "second\n\nbody line", "T");
    git(&dir, &["tag", "lw"], &[]);
    git(&dir, &["checkout", "-q", "-b", "side", "HEAD~1"], &[]);
    write(&dir, "s.txt", b"side\n");
    git(&dir, &["add", "."], &[]);
    commit(&dir, 3, "side work", "Alice");
    git(
        &dir,
        &["tag", "-a", "v1", "-m", "release one"],
        &[("GIT_COMMITTER_DATE", "2024-01-04T10:00:00+0200")],
    );
    git(&dir, &["checkout", "-q", "main"], &[]);
    let date = "2024-01-05T10:00:00+0200";
    git(
        &dir,
        &["merge", "-q", "--no-ff", "-m", "merge side", "side"],
        &[("GIT_AUTHOR_DATE", date), ("GIT_COMMITTER_DATE", date)],
    );
    write(&dir, "dir/a", b"alpha\nbeta aa\nchanged\n");
    std::fs::remove_file(dir.join("dir/sub/b")).unwrap();
    write(&dir, "u.txt", b"untracked foo\n");
    write(&dir, "x.log", b"log\n");
    write(&dir, "build/out.o", b"obj\n");
    write(&dir, "dir/y.tmp", b"tmp\n");
    dir
}

#[test]
fn rev_parse_matches_git() {
    let dir = repo("rev-parse");
    same(
        &dir,
        &[
            &["rev-parse", "HEAD"][..],
            &["rev-parse", "HEAD~1", "side"],
            &["rev-parse", "--short", "HEAD"],
            &["rev-parse", "--short=10", "HEAD"],
            &["rev-parse", "--abbrev-ref", "HEAD"],
            &["rev-parse", "--symbolic-full-name", "HEAD"],
            &["rev-parse", "main..side"],
            &["rev-parse", "side...main"],
            &["rev-parse", "^main"],
            &[
                "rev-parse",
                "v1",
                "v1^{commit}",
                "HEAD^{tree}",
                "HEAD:dir/a",
            ],
            &["rev-parse", "--verify", "HEAD"],
            &["rev-parse", "--verify", "-q", "nope"],
            &["rev-parse", "--show-toplevel"],
            &["rev-parse", "--git-dir"],
            &["rev-parse", "--is-inside-work-tree"],
            &["rev-parse", "--show-prefix"],
        ],
    );
    let sub = dir.join("dir");
    same(
        &sub,
        &[
            &["rev-parse", "--show-prefix"][..],
            &["rev-parse", "--show-cdup"],
            &["rev-parse", "--git-dir"],
            &["rev-parse", "--show-toplevel"],
        ],
    );
    git(&dir, &["checkout", "-q", "--detach"], &[]);
    same(&dir, &[&["rev-parse", "--abbrev-ref", "HEAD"]]);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn ls_files_and_ls_tree_match_git() {
    let dir = repo("ls");
    same(
        &dir,
        &[
            &["ls-files"][..],
            &["ls-files", "-s"],
            &["ls-files", "-z"],
            &["ls-files", "-o", "--exclude-standard"],
            &["ls-files", "-o"],
            &["ls-files", "-o", "-i", "--exclude-standard"],
            &["ls-files", "-m"],
            &["ls-files", "-d"],
            &["ls-files", "dir"],
            &["ls-files", "*.txt"],
            &["ls-tree", "HEAD"],
            &["ls-tree", "-r", "HEAD"],
            &["ls-tree", "-l", "HEAD"],
            &["ls-tree", "-r", "--name-only", "HEAD"],
            &["ls-tree", "-r", "-t", "HEAD"],
            &["ls-tree", "-d", "HEAD"],
            &["ls-tree", "HEAD", "dir"],
            &["ls-tree", "HEAD", "dir/"],
            &["ls-tree", "HEAD", "dir/sub/b"],
            &["ls-tree", "--abbrev=8", "v1"],
        ],
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn cat_file_matches_git() {
    let dir = repo("cat-file");
    same(
        &dir,
        &[
            &["cat-file", "-t", "HEAD"][..],
            &["cat-file", "-s", "HEAD"],
            &["cat-file", "-p", "HEAD"],
            &["cat-file", "-p", "HEAD^{tree}"],
            &["cat-file", "-p", "HEAD:c.txt"],
            &["cat-file", "-p", "HEAD:bin.dat"],
            &["cat-file", "blob", "HEAD:c.txt"],
            &["cat-file", "tree", "HEAD"],
            &["cat-file", "-t", "v1"],
            &["cat-file", "-p", "v1"],
            &["cat-file", "-e", "HEAD:c.txt"],
            &["cat-file", "-e", "HEAD:nope"],
        ],
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn refs_match_git() {
    let dir = repo("refs");
    let fmt = "--format=%(refname)|%(refname:short)|%(objectname)|%(objectname:short)|\
               %(objecttype)|%(*objectname)|%(subject)|%(body)|%(authorname)|%(authoremail)|\
               %(authordate)|%(committerdate:iso)|%(creatordate:short)|%(taggername)|\
               %(HEAD)|%(refname:lstrip=-1)%%%09end";
    same(
        &dir,
        &[
            &["show-ref"][..],
            &["show-ref", "--heads"],
            &["show-ref", "--tags", "-d"],
            &["show-ref", "main"],
            &["show-ref", "--verify", "refs/heads/main"],
            &["show-ref", "--verify", "main"],
            &["show-ref", "--hash=8", "--heads"],
            &["show-ref", "-s", "v1"],
            &["show-ref", "--head", "--heads"],
            &["show-ref", "nosuch"],
            &["for-each-ref"],
            &["for-each-ref", "refs/heads"],
            &["for-each-ref", "refs/tags/*"],
            &["for-each-ref", fmt],
            &[
                "for-each-ref",
                "--sort=-committerdate",
                "--format=%(refname:short) %(committerdate:unix)",
            ],
            &["for-each-ref", "--sort=-refname", "--count=2"],
            &["symbolic-ref", "HEAD"],
            &["symbolic-ref", "--short", "HEAD"],
        ],
    );
    git(&dir, &["checkout", "-q", "--detach"], &[]);
    same(&dir, &[&["symbolic-ref", "-q", "HEAD"]]);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn history_matches_git() {
    let dir = repo("history");
    same(
        &dir,
        &[
            &["rev-list", "HEAD"][..],
            &["rev-list", "--all"],
            &["rev-list", "--count", "HEAD"],
            &["rev-list", "main..side"],
            &["rev-list", "side...main"],
            &["rev-list", "--reverse", "HEAD"],
            &["rev-list", "-n", "2", "HEAD"],
            &["rev-list", "--max-count=1", "--reverse", "HEAD"],
            &["rev-list", "--first-parent", "HEAD"],
            &["rev-list", "--parents", "HEAD"],
            &["rev-list", "--merges", "HEAD"],
            &["rev-list", "--no-merges", "HEAD", "^side"],
            &["merge-base", "main", "side"],
            &["merge-base", "--all", "HEAD~1", "side"],
            &["merge-base", "--is-ancestor", "side", "main"],
            &["merge-base", "--is-ancestor", "main", "side"],
            &["reflog"],
            &["reflog", "show", "side"],
            &["reflog", "-n", "2"],
            &["shortlog", "HEAD"],
            &["shortlog", "-s", "HEAD"],
            &["shortlog", "-sne", "--all"],
            &["shortlog", "-n", "main..side"],
        ],
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn grep_matches_git() {
    let dir = repo("grep");
    same(
        &dir,
        &[
            &["grep", "foo"][..],
            &["grep", "-n", "a"],
            &["grep", "-i", "foo"],
            &["grep", "-w", "aa"],
            &["grep", "-l", "a"],
            &["grep", "-c", "a"],
            &["grep", "-v", "-n", "o"],
            &["grep", "-e", "bee", "-e", "echo"],
            &["grep", "--cached", "changed"],
            &["grep", "changed"],
            &["grep", "one", "HEAD~1"],
            &["grep", "a", "--", "dir"],
            &["grep", "-F", "a+b"],
            &["grep", "a+b"],
            &["grep", "-E", "a+b"],
            &["grep", "a\\+b"],
            &["grep", "-q", "foo"],
            &["grep", "zzz"],
            &["check-ignore", "x.log"],
            &["check-ignore", "-v", "x.log", "dir/y.tmp", "build/out.o"],
            &["check-ignore", "-v", "-n", "c.txt", "x.log"],
            &["check-ignore", "c.txt"],
            &["count-objects"],
            &["count-objects", "-v"],
        ],
    );
    git(&dir, &["repack", "-a", "-d", "-q"], &[]);
    same(&dir, &[&["count-objects", "-v"]]);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn var_reads_identity_and_editor() {
    let dir = repo("var");
    let out = rgit(&dir, &["var", "GIT_COMMITTER_IDENT"]);
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.starts_with("T <t@example.com> "), "{text}");
    let out = run(
        env!("CARGO_BIN_EXE_rgit"),
        &dir,
        &["--human", "var", "GIT_EDITOR"],
        &[("GIT_EDITOR", "nano -w")],
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout), "nano -w\n");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn agent_output_is_structured() {
    let dir = repo("toon");
    let toon = |args: &[&str]| {
        let mut all = vec!["--toon"];
        all.extend(args);
        let out = run(env!("CARGO_BIN_EXE_rgit"), &dir, &all, &[]);
        String::from_utf8_lossy(&out.stdout).into_owned()
    };
    let head = git(&dir, &["rev-parse", "HEAD"], &[]);
    assert_eq!(toon(&["rev-parse", "HEAD"]), format!("result: {head}"));
    assert!(
        toon(&["ls-files"]).starts_with("files["),
        "{}",
        toon(&["ls-files"])
    );
    assert!(toon(&["merge-base", "--is-ancestor", "main", "side"]).contains("ancestor: false"));
    assert!(toon(&["grep", "zzz"]).contains("0 matches"));
    assert!(toon(&["shortlog", "HEAD"]).contains("authors[2]{author,count}:"));
    let _ = std::fs::remove_dir_all(&dir);
}
