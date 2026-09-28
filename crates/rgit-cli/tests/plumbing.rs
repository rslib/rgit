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

/// rgit and git print the same bytes for `args` with `input` on stdin.
fn same_input(dir: &Path, args: &[&str], input: &[u8]) {
    let feed = |bin: &str, args: &[&str]| {
        use std::io::Write;
        let mut child = Command::new(bin)
            .args(args)
            .current_dir(dir)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("RGIT_OPLOG", "0")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        child.stdin.take().unwrap().write_all(input).unwrap();
        child.wait_with_output().unwrap().stdout
    };
    let mut human = vec!["--human"];
    human.extend(args);
    let (want, got) = (feed("git", args), feed(env!("CARGO_BIN_EXE_rgit"), &human));
    assert_eq!(
        String::from_utf8_lossy(&got),
        String::from_utf8_lossy(&want),
        "{args:?}"
    );
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
            &["rev-parse", "HEAD", "--abbrev-ref", "HEAD", "--show-prefix"],
            &["rev-parse", "--short", "HEAD", "--show-cdup"],
            &["rev-parse", "--short=4", "HEAD"],
            &["rev-parse", "--short", "HEAD", "side"],
            &["rev-parse", "--git-common-dir", "--absolute-git-dir"],
            &["rev-parse", "--sq-quote", "a b", "it's", "x!"],
            &["rev-parse", "--local-env-vars"],
            &["rev-parse", "--symbolic", "HEAD", "side~1"],
            &["rev-parse", "--symbolic-full-name", "HEAD", "side", "lw"],
            &["rev-parse", "--not", "main", "^side", "--branches"],
            &["rev-parse", "--all", "--tags"],
            &["rev-parse", "--default", "side"],
            &["rev-parse", "--verify", "--symbolic-full-name", "side"],
            &["rev-parse", "HEAD", "--", "a"],
            &[
                "rev-parse",
                "--is-shallow-repository",
                "--show-object-format",
            ],
        ],
    );
    same(&dir, &[&["rev-parse", "--git-common-dir", "--show-cdup"]]);
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
            &["ls-tree", "--abbrev", "HEAD"],
            &[
                "ls-tree",
                "-r",
                "--format=%(objectmode)|%(objectsize:padded)|%(path)",
                "HEAD",
            ],
            &["ls-files", "-c", "-i", "--exclude-standard"],
            &["ls-files", "--error-unmatch", "c.txt"],
            &["ls-files", "--error-unmatch", "c.txt", "nope"],
            &["ls-files", "-s", "-z", "dir"],
        ],
    );
    // git limits these to the current folder and prints paths from it.
    same(
        &dir.join("dir"),
        &[
            &["ls-files"][..],
            &["ls-files", ".."],
            &["ls-files", "--full-name"],
            &["ls-files", "-s", "sub"],
            &["ls-files", "-o"],
            &["ls-files", "-m", "-d"],
            &["ls-files", "-o", "-i", "--exclude-standard"],
            &["ls-tree", "HEAD"],
            &["ls-tree", "HEAD", ".."],
            &["ls-tree", "-r", "HEAD", "sub"],
            &["ls-tree", "--full-name", "HEAD"],
            &["ls-tree", "--full-tree", "HEAD"],
            &["ls-tree", "--full-tree", "HEAD", "dir"],
            &["grep", "a"],
            &["grep", "-n", "e", "--", ".."],
            &["grep", "a", "HEAD", "--", ".."],
            &["grep", "--full-name", "a", "HEAD"],
            &["grep", "-l", "e", "../c.txt", "a"],
            &["check-ignore", "y.tmp", "./y.tmp", "../x.log", "a"],
            &["check-ignore", "-v", "-n", "y.tmp", "../x.log", "a"],
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
            &["show-ref", "--abbrev", "--tags"],
            &["show-ref", "--abbrev=10", "-d"],
            &["show-ref", "--exists", "refs/heads/main"],
            &["show-ref", "--exists", "main"],
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
            &[
                "for-each-ref",
                "--format=%(if)%(*objectname)%(then)A%(else)L%(end)|\
                 %(if:equals=main)%(refname:short)%(then)M%(end)|\
                 %(align:10)%(refname:short)%(end)|%(align:8,right)%(objecttype)%(end)|\
                 %(align:width=9,position=middle)x%(end)|%(contents:subject)|%(contents:body)|\
                 %(objectsize)|%(numparent)|%(parent)|%(tree)|%(*objecttype)",
            ],
            &["for-each-ref", "--merged"],
            &["for-each-ref", "--merged=side"],
            &["for-each-ref", "--no-merged", "side"],
            &["for-each-ref", "--contains", "side"],
            &["for-each-ref", "--no-contains", "HEAD~1"],
            &["for-each-ref", "--points-at", "side"],
            &["for-each-ref", "--sort=objecttype", "--sort=-refname"],
            &[
                "for-each-ref",
                "--sort=-version:refname",
                "--exclude=refs/tags/lw",
            ],
            &[
                "for-each-ref",
                "--omit-empty",
                "--format=%(if)%(*objectname)%(then)%(refname)%(end)",
            ],
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
fn rev_list_walks_like_git() {
    let dir = repo("walk");
    git(&dir, &["checkout", "-q", "-b", "topic", "lw"], &[]);
    let date = "2024-01-06T10:00:00+0200";
    git(
        &dir,
        &["cherry-pick", "side"],
        &[("GIT_AUTHOR_DATE", date), ("GIT_COMMITTER_DATE", date)],
    );
    git(&dir, &["checkout", "-q", "main"], &[]);
    same(
        &dir,
        &[
            &["rev-list", "--left-right", "--cherry-mark", "topic...main"][..],
            &[
                "rev-list",
                "--count",
                "--left-right",
                "--cherry-mark",
                "topic...main",
            ],
            &["rev-list", "--count", "--left-right", "topic...main"],
            &[
                "rev-list",
                "--cherry-pick",
                "--left-right",
                "--boundary",
                "topic...main",
            ],
            &["rev-list", "--cherry", "topic...main"],
            &["rev-list", "--left-only", "topic...main"],
            &["rev-list", "--parents", "--boundary", "-2", "HEAD"],
            &["rev-list", "--objects", "HEAD~1..HEAD"],
            &["rev-list", "--objects-edge", "HEAD~1..HEAD"],
            &["rev-list", "--objects", "--all"],
            &["rev-list", "--objects", "-2", "HEAD"],
            &["rev-list", "--missing=print", "--objects", "HEAD"],
            &["rev-list", "--date-order", "--all"],
            &["rev-list", "--author-date-order", "--all"],
            &["rev-list", "--topo-order", "--all"],
            &[
                "rev-list",
                "--since=2024-01-03",
                "--until=2024-01-05",
                "HEAD",
            ],
            &["rev-list", "--since=2024-01-03 12:00", "--all"],
            &["rev-list", "--author=Alice", "--all"],
            &["rev-list", "--author=alice", "-i", "--all"],
            &[
                "rev-list",
                "--grep=side",
                "--grep=merge",
                "--all-match",
                "--all",
            ],
            &["rev-list", "--grep=side", "--invert-grep", "--all"],
            &["rev-list", "--parents", "--all", "--", "s.txt"],
            &["rev-list", "--ancestry-path", "lw..main"],
            &["rev-list", "--no-walk", "topic", "main"],
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
            &["grep", "-C1", "-n", "o"],
            &["grep", "-A1", "a"],
            &["grep", "-B2", "two"],
            &["grep", "--heading", "-n", "a"],
            &["grep", "--heading", "--break", "-C1", "o"],
            &["grep", "--break", "a"],
            &["grep", "-o", "-n", "a."],
            &["grep", "-h", "-o", "[a-z]*oo"],
            &["grep", "-L", "a"],
            &["grep", "-c", "-h", "a"],
            &["grep", "-z", "-n", "-C1", "bar"],
            &["grep", "-z", "-l", "a"],
            &["grep", "-z", "-c", "a"],
            &["grep", "-m1", "o"],
            &["grep", "-I", "binary"],
            &["grep", "binary"],
            &["check-ignore", "x.log"],
            &["check-ignore", "-v", "x.log", "dir/y.tmp", "build/out.o"],
            &["check-ignore", "-v", "-n", "c.txt", "x.log"],
            &["check-ignore", "c.txt"],
            &["count-objects"],
            &["count-objects", "-v"],
        ],
    );
    same_input(
        &dir,
        &["check-ignore", "--stdin"],
        b"x.log\nc.txt\ndir/y.tmp\n",
    );
    same_input(
        &dir,
        &["check-ignore", "--stdin", "-z", "-v", "-n"],
        b"x.log\0c.txt\0",
    );
    git(&dir, &["repack", "-a", "-d", "-q"], &[]);
    same(&dir, &[&["count-objects", "-v"]]);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn grep_expressions_and_functions_match_git() {
    let dir = repo("grep-expr");
    write(
        &dir,
        "f.c",
        b"/* the main\n * comment */\nint main(void)\n{\n  foo bar\n\n  baz\n  return 0;\n}\n\n\n\
          static int helper(int x)\n{\n  bar only\n  foo only\n  return x;\n}\n",
    );
    write(&dir, "m.x", b"fn one\n  x foo\nhelper\n  y foo\n");
    write(&dir, ".gitattributes", b"*.x diff=mine\n");
    git(&dir, &["config", "diff.mine.xfuncname", "^fn (.*)$"], &[]);
    git(&dir, &["add", "."], &[]);
    commit(&dir, 6, "functions", "T");
    write(&dir, "u.txt", b"foo untracked\n");
    write(&dir, "x.log", b"foo ignored\n");
    let sub = dir.join("sub");
    std::fs::create_dir_all(&sub).unwrap();
    git(&sub, &["init", "-q"], &[]);
    write(&sub, "g", b"foo in sub\n");
    git(&sub, &["add", "."], &[]);
    commit(&sub, 1, "sub", "T");
    write(
        &dir,
        ".gitmodules",
        b"[submodule \"sub\"]\n\tpath = sub\n\turl = ./sub\n",
    );
    git(&dir, &["add", "sub", ".gitmodules"], &[]);
    git(&dir, &["config", "submodule.sub.url", "./sub"], &[]);
    commit(&dir, 7, "add sub", "T");
    same(
        &dir,
        &[
            &["grep", "-e", "foo", "--or", "-e", "bar"][..],
            &["grep", "-e", "foo", "--and", "-e", "bar"],
            &["grep", "-n", "--not", "-e", "foo"],
            &["grep", "-e", "foo", "--and", "--not", "-e", "bar"],
            &[
                "grep", "(", "-e", "foo", "--or", "-e", "baz", ")", "--and", "-e", "bar",
            ],
            &["grep", "-e", "foo", "-e", "bar", "--and", "-e", "baz"],
            &["grep", "-e", "foo", "--not", "-e", "bar"],
            &["grep", "--all-match", "-e", "foo", "-e", "baz"],
            &[
                "grep",
                "--all-match",
                "-e",
                "foo",
                "--and",
                "-e",
                "bar",
                "-e",
                "only",
            ],
            &["grep", "-c", "--all-match", "-e", "foo", "-e", "baz"],
            &["grep", "-o", "--not", "-e", "bar", "-e", "foo"],
            &["grep", "-l", "--not", "-e", "foo"],
            &["grep", "-e", "foo", "--and"],
            &["grep", "(", "-e", "foo"],
            &["grep", "-e", "-e", "foo"],
            &["grep", "-p", "-e", "bar"],
            &["grep", "-n", "-p", "only"],
            &["grep", "-p", "-C1", "return"],
            &["grep", "-W", "bar"],
            &["grep", "-W", "-n", "baz"],
            &["grep", "-W", "-A1", "foo"],
            &["grep", "-p", "foo", "--", "m.x"],
            &["grep", "-W", "foo", "--", "m.x"],
            &["grep", "-m1", "-A2", "foo"],
            &["grep", "-P", "foo(?= only)"],
            &["grep", "-P", "-w", "ba."],
            &["grep", "--threads", "2", "foo"],
            &["grep", "--untracked", "foo"],
            &["grep", "--untracked", "--no-exclude-standard", "foo"],
            &["grep", "--no-index", "foo"],
            &["grep", "--no-index", "--exclude-standard", "-n", "foo"],
            &["grep", "--recurse-submodules", "foo"],
            &["grep", "--recurse-submodules", "--cached", "foo"],
            &["grep", "--recurse-submodules", "foo", "HEAD"],
        ],
    );
    let bare = std::env::temp_dir().join(format!("rgit-plumbing-{}-noindex", std::process::id()));
    let _ = std::fs::remove_dir_all(&bare);
    write(&bare, "a", b"foo 1\n");
    write(&bare, "s/b", b"foo 2\nbar\n");
    same(
        &bare,
        &[
            &[
                "grep",
                "--no-index",
                "-n",
                "-e",
                "foo",
                "--and",
                "--not",
                "-e",
                "2",
            ][..],
            &["grep", "--no-index", "foo", "--", "s"],
        ],
    );
    let _ = std::fs::remove_dir_all(&bare);
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
    let env = [
        ("GIT_AUTHOR_DATE", "2024-01-02T10:00:00+0200"),
        ("GIT_COMMITTER_DATE", "@1700000000 -0130"),
        ("GIT_ATTR_NOSYSTEM", "1"),
        ("GIT_EDITOR", "vi"),
    ];
    for args in [
        &["var", "-l"][..],
        &["var", "GIT_AUTHOR_IDENT"],
        &["var", "GIT_COMMITTER_IDENT"],
        &["var", "GIT_CONFIG_GLOBAL"],
        &["var", "GIT_SHELL_PATH"],
    ] {
        let want = run("git", &dir, args, &env);
        let mut human = vec!["--human"];
        human.extend(args);
        let got = run(env!("CARGO_BIN_EXE_rgit"), &dir, &human, &env);
        assert_eq!(
            String::from_utf8_lossy(&got.stdout),
            String::from_utf8_lossy(&want.stdout),
            "{args:?}"
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn batch_symbolic_ref_and_path_limits_match_git() {
    let dir = repo("batch");
    let input = b"HEAD\nv1\nnope\nHEAD:c.txt\nHEAD^{tree}\n";
    same_input(&dir, &["cat-file", "--batch-check"], input);
    same_input(&dir, &["cat-file", "--batch"], input);
    same_input(
        &dir,
        &[
            "cat-file",
            "--batch-check=%(objecttype) %(rest)|%(objectname)",
        ],
        b"HEAD some rest\nside\n",
    );
    same(
        &dir,
        &[
            &["cat-file", "--batch-check", "--batch-all-objects"][..],
            &["rev-list", "HEAD", "--", "s.txt"],
            &["rev-list", "HEAD", "--", "c.txt"],
            &["rev-list", "--all", "--", "dir", "e.txt"],
            &["rev-list", "--count", "HEAD", "--", "c.txt"],
            &["rev-list", "--first-parent", "HEAD", "--", "s.txt"],
            &["rev-list", "--skip=1", "--branches"],
            &["rev-list", "--tags", "--abbrev-commit"],
            &["rev-list", "--topo-order", "--parents", "HEAD"],
        ],
    );
    same(&dir.join("dir"), &[&["rev-list", "HEAD", "--", "../s.txt"]]);
    let ok = |args: &[&str]| assert!(rgit(&dir, args).status.success(), "{args:?}");
    ok(&["symbolic-ref", "refs/heads/alias", "refs/heads/side"]);
    assert_eq!(
        git(&dir, &["symbolic-ref", "refs/heads/alias"], &[]),
        "refs/heads/side\n"
    );
    ok(&["symbolic-ref", "-d", "refs/heads/alias"]);
    assert!(
        !run("git", &dir, &["symbolic-ref", "refs/heads/alias"], &[])
            .status
            .success()
    );
    ok(&["symbolic-ref", "-m", "move", "HEAD", "refs/heads/side"]);
    assert_eq!(
        git(&dir, &["symbolic-ref", "HEAD"], &[]),
        "refs/heads/side\n"
    );
    same(
        &dir,
        &[
            &["symbolic-ref", "HEAD", "side"][..],
            &["symbolic-ref", "-d", "HEAD"],
            &["symbolic-ref", "-d", "-q", "refs/heads/main"],
        ],
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn peeled_atoms_batch_command_and_filters_match_git() {
    let dir = repo("peel");
    #[cfg(unix)]
    for (link, target) in [
        ("link", "c.txt"),
        ("out", "/etc/hosts"),
        ("up", "../x"),
        ("dang", "nothere"),
        ("loop1", "loop2"),
        ("loop2", "loop1"),
        ("dlink", "dir"),
        ("dir/back", "../c.txt"),
        ("notdir", "c.txt/b"),
    ] {
        std::os::unix::fs::symlink(target, dir.join(link)).unwrap();
        git(&dir, &["add", link], &[]);
    }
    write(&dir, ".gitattributes", b"c.txt diff=up filter=rot\n");
    git(&dir, &["add", ".gitattributes"], &[]);
    commit(&dir, 6, "links", "T");
    git(&dir, &["config", "diff.up.textconv", "sed s/o/0/"], &[]);
    git(&dir, &["config", "filter.rot.smudge", "sed s/e/E/"], &[]);
    git(&dir, &["repack", "-adq"], &[]);
    same(
        &dir,
        &[
            &[
                "for-each-ref",
                "--format=%(refname)|%(*subject)|%(*authorname)|%(*authordate:unix)|\
                 %(*objectname:short)|%(*tree)|%(*parent)|%(*body)|%(*refname)",
            ][..],
            &[
                "for-each-ref",
                "--format=%(refname) %(describe) %(describe:tags)",
            ],
            &[
                "for-each-ref",
                "--format=%(describe:tags,abbrev=4,match=v*)",
            ],
            &["for-each-ref", "--format=%(refname) %(ahead-behind:side)"],
            &["for-each-ref", "--no-merged=main"],
            &["cat-file", "--textconv", "HEAD:c.txt"],
            &["cat-file", "--filters", "HEAD:c.txt"],
            &["cat-file", "--filters", "--path=c.txt", "HEAD:e.txt"],
            &["cat-file", "--filters", "HEAD:link"],
            &["cat-file", "--filters", "HEAD"],
            &["cat-file", "--textconv", "HEAD:nope"],
            &[
                "cat-file",
                "--batch-all-objects",
                "--batch-check=%(objectname) %(objectsize:disk) %(deltabase)",
            ],
        ],
    );
    let names = b"HEAD:link\nHEAD:out\nHEAD:up\nHEAD:dang\nHEAD:loop1\nHEAD:dlink/a\n\
                  HEAD:dir/back\nHEAD:notdir\nHEAD:nope\n:c.txt\n";
    same_input(&dir, &["cat-file", "--batch", "--follow-symlinks"], names);
    same_input(
        &dir,
        &["cat-file", "--batch-check", "--follow-symlinks"],
        names,
    );
    let commands = b"info HEAD\ncontents HEAD:c.txt\ninfo nope\nflush\ncontents v1\n";
    same_input(&dir, &["cat-file", "--batch-command", "--buffer"], commands);
    same_input(
        &dir,
        &["cat-file", "--batch-command=%(objecttype)"],
        b"info HEAD\n",
    );
    same_input(
        &dir,
        &["cat-file", "--batch-command", "-Z"],
        b"info HEAD\0contents HEAD:e.txt\0",
    );
    same_input(&dir, &["cat-file", "--batch", "-Z"], b"HEAD:e.txt\0nope\0");
    same_input(
        &dir,
        &["cat-file", "--batch", "--textconv"],
        b"HEAD:c.txt c.txt\n",
    );
    same_input(
        &dir,
        &["cat-file", "--batch", "--filters"],
        b"HEAD:e.txt c.txt\n",
    );
    write(&dir, "dir/.gitignore", b"# temp\n*.tmp\n!keep.tmp\n");
    let paths = b"x.log\ndir/y.tmp\ndir/keep.tmp\nc.txt\n";
    same_input(&dir, &["check-ignore", "--stdin", "-v", "-n"], paths);
    same_input(&dir, &["check-ignore", "--stdin"], paths);
    same_input(
        &dir,
        &["check-ignore", "--stdin", "-z", "-v"],
        b"x.log\0dir/keep.tmp\0",
    );
    same(
        &dir,
        &[
            &["check-ignore", "dir/keep.tmp"][..],
            &["check-ignore", "-v", "dir/keep.tmp"],
        ],
    );

    // Each answer comes back before the next path is sent.
    use std::io::{BufRead, Write};
    let mut child = Command::new(env!("CARGO_BIN_EXE_rgit"))
        .args(["--human", "check-ignore", "--stdin", "-v", "-n"])
        .current_dir(&dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("RGIT_OPLOG", "0")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = std::io::BufReader::new(child.stdout.take().unwrap());
    for (path, want) in [
        ("x.log", ".gitignore:1:*.log\tx.log\n"),
        ("c.txt", "::\tc.txt\n"),
    ] {
        writeln!(stdin, "{path}").unwrap();
        stdin.flush().unwrap();
        let mut line = String::new();
        stdout.read_line(&mut line).unwrap();
        assert_eq!(line, want);
    }
    drop(stdin);
    child.wait().unwrap();
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

#[test]
fn diff_plumbing_matches_git() {
    let dir = repo("diff-plumbing");
    git(&dir, &["add", "dir/a"], &[]);
    write(&dir, "dir/a", b"alpha\nagain\n");
    let cases: Vec<Vec<&str>> = [
        "diff-tree HEAD",
        "diff-tree -r HEAD~1",
        "diff-tree -r -t HEAD~2 side",
        "diff-tree --root -r HEAD~2",
        "diff-tree HEAD~2",
        "diff-tree -p HEAD~1",
        "diff-tree --name-status -r lw side",
        "diff-tree --name-only -z -r lw side",
        "diff-tree -z lw side",
        "diff-tree -s HEAD~1",
        "diff-tree --no-commit-id -r HEAD~1",
        "diff-tree lw side -- dir",
        "diff-tree -r HEAD~2 HEAD dir",
        "diff-tree --quiet HEAD~1",
        "diff-tree nope",
        "diff-index HEAD",
        "diff-index --cached HEAD",
        "diff-index -p HEAD",
        "diff-index --cached --name-status HEAD~2",
        "diff-index HEAD -- dir",
        "diff-files",
        "diff-files -p",
        "diff-files --name-only -z",
        "diff-files dir",
        "diff-files --quiet",
    ]
    .iter()
    .map(|c| c.split(' ').collect())
    .collect();
    let cases: Vec<&[&str]> = cases.iter().map(Vec::as_slice).collect();
    same(&dir, &cases);
    same(
        &dir.join("dir"),
        &[&["diff-files", "a"][..], &["diff-index", "HEAD", "."]],
    );
    let revs = git(&dir, &["rev-list", "--all"], &[]);
    same_input(&dir, &["diff-tree", "--stdin", "-r"], revs.as_bytes());
    let trees = git(&dir, &["rev-parse", "lw^{tree}", "side^{tree}"], &[]).replace('\n', " ");
    same_input(
        &dir,
        &["diff-tree", "--stdin"],
        format!("{}\nnot an id\n", trees.trim_end()).as_bytes(),
    );
    let code = |args: &[&str]| rgit(&dir, args).status.code();
    assert_eq!(code(&["diff-files", "--quiet"]), Some(1));
    assert_eq!(
        code(&["diff-index", "--cached", "--exit-code", "HEAD"]),
        Some(1)
    );
    assert_eq!(code(&["diff-tree", "--exit-code", "HEAD", "HEAD"]), Some(0));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn merge_tree_and_merge_file_match_git() {
    let dir = repo("merge-tree");
    git(&dir, &["stash", "-u", "-q"], &[]);
    git(&dir, &["checkout", "-q", "-b", "x", "lw"], &[]);
    write(&dir, "c.txt", b"one\nFoo X\nfoobar\na+b\ntwo\n");
    write(&dir, "e.txt", b"echo x\n");
    write(&dir, "both", b"x\n");
    write(&dir, "g", b"1\n2\n3\n4\n5\n6\n");
    git(&dir, &["add", "."], &[]);
    commit(&dir, 6, "x", "T");
    git(&dir, &["checkout", "-q", "-b", "y", "lw"], &[]);
    write(&dir, "c.txt", b"one\nFoo Y\nfoobar\na+b\ntwo\n");
    git(&dir, &["rm", "-q", "e.txt"], &[]);
    write(&dir, "both", b"y\n");
    git(&dir, &["add", "."], &[]);
    commit(&dir, 7, "y", "T");
    git(&dir, &["checkout", "-q", "-b", "z", "x"], &[]);
    write(&dir, "g", b"1\n2\n3\n4\n5\nsix\n");
    commit_all(&dir, "z");
    git(&dir, &["checkout", "-q", "x"], &[]);
    write(&dir, "g", b"one\n2\n3\n4\n5\n6\n");
    commit_all(&dir, "x2");
    same(
        &dir,
        &[
            &["merge-tree", "--write-tree", "x", "y"][..],
            &["merge-tree", "y", "x"],
            &["merge-tree", "--name-only", "x", "y"],
            &["merge-tree", "-z", "x", "y"],
            &["merge-tree", "--no-messages", "x", "y"],
            &["merge-tree", "x", "z"],
            &["merge-tree", "--messages", "x", "z"],
            &["merge-tree", "-z", "--messages", "x", "z"],
            &["merge-tree", "--merge-base=lw", "z", "y"],
            &["merge-tree", "x", "nope"],
        ],
    );
    assert_eq!(rgit(&dir, &["merge-tree", "x", "y"]).status.code(), Some(1));
    write(&dir, "base", b"a\nb\nc\nd\ne\nf\ng\nh\n");
    write(&dir, "ours", b"a\nB\nc\nd\ne\nf\ng\nh1\n");
    write(&dir, "theirs", b"a\nX\nc\nd\ne\nf\ng\nh2\n");
    write(&dir, "zb", b"a\nb x y\nc\n");
    write(&dir, "zo", b"a\nb x Q\nz\nc\n");
    write(&dir, "zt", b"a\nb x R\nz\nc\n");
    let with = |extra: &[&'static str]| -> Vec<&'static str> {
        let mut v = vec!["merge-file", "-p"];
        v.extend(extra);
        v
    };
    let cases = [
        with(&["ours", "base", "theirs"]),
        with(&["-L", "A", "-L", "B", "-L", "C", "ours", "base", "theirs"]),
        with(&["--diff3", "ours", "base", "theirs"]),
        with(&["--zdiff3", "zo", "zb", "zt"]),
        with(&["--diff3", "zo", "zb", "zt"]),
        with(&["--ours", "ours", "base", "theirs"]),
        with(&["--theirs", "ours", "base", "theirs"]),
        with(&["--union", "ours", "base", "theirs"]),
        with(&["--marker-size=3", "ours", "base", "theirs"]),
        with(&["base", "base", "theirs"]),
    ];
    let cases: Vec<&[&str]> = cases.iter().map(Vec::as_slice).collect();
    same(&dir, &cases);
    for args in &cases {
        assert_eq!(
            rgit(&dir, args).status.code(),
            run("git", &dir, args, &[]).status.code(),
            "{args:?}"
        );
    }
    std::fs::copy(dir.join("ours"), dir.join("mine")).unwrap();
    rgit(&dir, &["merge-file", "mine", "base", "theirs"]);
    run("git", &dir, &["merge-file", "ours", "base", "theirs"], &[]);
    assert_eq!(
        std::fs::read_to_string(dir.join("mine"))
            .unwrap()
            .replace("mine", "ours"),
        std::fs::read_to_string(dir.join("ours")).unwrap()
    );
    let _ = std::fs::remove_dir_all(&dir);
}

fn commit_all(dir: &Path, message: &str) {
    git(dir, &["commit", "-q", "-am", message], &[]);
}

#[test]
fn text_filters_match_git() {
    let dir = repo("text-filters");
    let messy = b"\n\n  a  \n\tb\n\n\n# note\n  \nc\t\n\n";
    same_input(&dir, &["stripspace"], messy);
    same_input(&dir, &["stripspace", "-s"], messy);
    same_input(&dir, &["stripspace", "-c"], messy);
    same_input(&dir, &["stripspace", "-c"], b"no newline");
    let words: String = (1..=23)
        .map(|i| format!("item{}\n", "x".repeat(i % 7)))
        .collect();
    for args in [
        &["column"][..],
        &["column", "--mode=column", "--width=40"],
        &["column", "--mode=row", "--width=40"],
        &["column", "--mode=column,dense", "--width=40"],
        &["column", "--mode=row,dense", "--width=50", "--padding=3"],
        &[
            "column",
            "--mode=column",
            "--width=30",
            "--indent=> ",
            "--nl=|\n",
        ],
        &["column", "--mode=plain", "--indent=* "],
        &["column", "--raw-mode=16", "--width=20"],
        &["column", "--mode=never"],
    ] {
        same_input(&dir, args, words.as_bytes());
    }
    let log = git(&dir, &["log", "-p", "--all"], &[]);
    same_input(&dir, &["patch-id"], log.as_bytes());
    same_input(&dir, &["patch-id", "--stable"], log.as_bytes());
    same_input(&dir, &["patch-id", "--verbatim"], log.as_bytes());
    let mails = git(&dir, &["format-patch", "--stdout", "HEAD~2"], &[]);
    same_input(&dir, &["patch-id", "--stable"], mails.as_bytes());
    let diff = git(&dir, &["diff"], &[]);
    same_input(&dir, &["patch-id"], diff.as_bytes());
    let mut cases: Vec<Vec<&str>> = Vec::new();
    for name in [
        "a/b",
        "a",
        "refs/heads/x.",
        "a/.b",
        "a..b",
        "a/b.lock",
        "a/@{b",
        "@",
        "a//b",
        "/a/b",
        "a/b/",
        "a/*",
        "a/b*c",
        "a/*/*",
        "a b/c",
        "a\\b/c",
        "refs/heads/-x",
        "a/b~1",
    ] {
        for flag in [
            "--normalize",
            "--refspec-pattern",
            "--allow-onelevel",
            "--print",
        ] {
            cases.push(vec!["check-ref-format", flag, name]);
        }
        cases.push(vec!["check-ref-format", name]);
    }
    for name in ["feature/x", "-x", "HEAD", "a..b", "@{-1}"] {
        cases.push(vec!["check-ref-format", "--branch", name]);
    }
    let cases: Vec<&[&str]> = cases.iter().map(Vec::as_slice).collect();
    same(&dir, &cases);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn name_rev_matches_git() {
    let dir = repo("name-rev");
    git(&dir, &["branch", "old", "HEAD~1"], &[]);
    let side = git(&dir, &["rev-parse", "side"], &[]);
    let first = git(&dir, &["rev-parse", "HEAD~1"], &[]);
    same(
        &dir,
        &[
            &["name-rev", "HEAD"][..],
            &["name-rev", "HEAD~1", "side", "HEAD^2", "v1"],
            &["name-rev", "--name-only", "HEAD~1", "side"],
            &["name-rev", "--tags", "HEAD~1", "side"],
            &["name-rev", "--tags", "--name-only", "side"],
            &["name-rev", "--refs=side", "HEAD~1"],
            &["name-rev", "--refs=refs/heads/*", "HEAD~2"],
            &["name-rev", "--exclude=main", "HEAD~1"],
            &["name-rev", "--peel-tag", "v1"],
            &["name-rev", "--tags", "HEAD"],
            &["name-rev", "--tags", "--always", "HEAD"],
            &["name-rev", "--tags", "--no-undefined", "HEAD"],
            &["name-rev", "lw", "HEAD^{tree}"],
        ],
    );
    let text = format!(
        "fix {} and\n{}x\n{}",
        side.trim(),
        first.trim(),
        first.trim()
    );
    same_input(&dir, &["name-rev", "--annotate-stdin"], text.as_bytes());
    same_input(
        &dir,
        &["name-rev", "--annotate-stdin", "--name-only"],
        text.as_bytes(),
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn check_attr_matches_git() {
    let dir = repo("check-attr");
    write(
        &dir,
        ".gitattributes",
        b"*.bin binary\n*.txt text eol=lf foo=bar\n[attr]mine -text zz\nsub/* mine\n\"we ird*\" odd\n",
    );
    write(&dir, "sub/.gitattributes", b"*.c whitespace=x -foo !zz\n");
    git(&dir, &["add", ".gitattributes"], &[]);
    write(&dir, ".gitattributes", b"*.txt -text\n");
    same(
        &dir,
        &[
            &[
                "check-attr",
                "-a",
                "a.bin",
                "a.txt",
                "sub/x.c",
                "sub/y",
                "none",
            ][..],
            &["check-attr", "text", "a.bin", "a.txt", "sub/x.c"],
            &[
                "check-attr",
                "text",
                "eol",
                "binary",
                "--",
                "a.bin",
                "a.txt",
            ],
            &["check-attr", "--cached", "-a", "a.txt", "sub/x.c"],
            &["check-attr", "-a", "-z", "a.txt", "we ird\"x"],
            &["check-attr", "odd", "we ird\"x"],
        ],
    );
    same(
        &dir.join("sub"),
        &[&["check-attr", "-a", "x.c", "../a.txt"][..]],
    );
    same_input(
        &dir,
        &["check-attr", "--stdin", "text", "zz"],
        b"a.txt\nsub/q.c\n",
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// Run each step with git in one twin of `repo` and rgit in the other, and
/// check they print the same, agree on success and leave the same index
/// (`ls-files -s`, `-v`) and working tree (`status`). A step is the folder
/// under the top, the arguments and stdin.
fn twins(tag: &str, steps: &[(&str, &[&str], &[u8])]) {
    let (a, b) = (repo(&format!("{tag}-git")), repo(&format!("{tag}-rgit")));
    let feed = |bin: &str, dir: &Path, args: &[&str], input: &[u8]| {
        use std::io::Write;
        let mut child = Command::new(bin)
            .args(args)
            .current_dir(dir)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("RGIT_OPLOG", "0")
            .env("GIT_AUTHOR_DATE", "2024-02-01T10:00:00+0100")
            .env("GIT_COMMITTER_DATE", "2024-02-01T10:00:00+0100")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        child.stdin.take().unwrap().write_all(input).unwrap();
        child.wait_with_output().unwrap()
    };
    let state = |dir: &Path| {
        [
            &["ls-files", "-s"][..],
            &["ls-files", "-v"],
            &["status", "--porcelain", "-uall"],
        ]
        .map(|args| git(dir, args, &[]))
    };
    for (sub, args, input) in steps {
        let want = feed("git", &a.join(sub), args, input);
        let mut human = vec!["--human"];
        human.extend(*args);
        let got = feed(env!("CARGO_BIN_EXE_rgit"), &b.join(sub), &human, input);
        assert_eq!(
            (String::from_utf8_lossy(&got.stdout), got.status.success()),
            (String::from_utf8_lossy(&want.stdout), want.status.success()),
            "{args:?} in {sub:?}\n  git stderr: {}\n  rgit stderr: {}",
            String::from_utf8_lossy(&want.stderr),
            String::from_utf8_lossy(&got.stderr)
        );
        assert_eq!(state(&b), state(&a), "state after {args:?}");
    }
    let _ = std::fs::remove_dir_all(&a);
    let _ = std::fs::remove_dir_all(&b);
}

#[test]
fn object_writers_match_git() {
    let blob = "3e757656cf36eca53338e520d134963a44f793f8";
    let tree_in = format!(
        "100644 blob {blob}\tz\n040000 tree 4b825dc642cb6eb9a060e54bf8d69288fbee4904\tsub\n100755 blob {blob}\t\"q\\tx\"\n"
    );
    let missing = b"100644 blob 0000000000000000000000000000000000000001\tz\n";
    twins(
        "writers",
        &[
            ("", &["hash-object", "-w", "--stdin"], b"new\n"),
            ("", &["write-tree"], b""),
            ("", &["write-tree", "--prefix=dir"], b""),
            ("", &["write-tree", "--prefix=nope/"], b""),
            (
                "",
                &[
                    "commit-tree",
                    "HEAD^{tree}",
                    "-p",
                    "HEAD",
                    "-m",
                    "one",
                    "-m",
                    "two",
                ],
                b"",
            ),
            (
                "",
                &[
                    "commit-tree",
                    "HEAD^{tree}",
                    "-p",
                    "HEAD",
                    "-p",
                    "side",
                    "-p",
                    "HEAD",
                ],
                b"from stdin\n",
            ),
            ("", &["commit-tree", "HEAD"], b"x"),
            (
                "",
                &["commit-tree", "HEAD^{tree}", "-F", "-"],
                b"no newline",
            ),
            ("", &["mktree"], tree_in.as_bytes()),
            (
                "",
                &["mktree", "--batch"],
                tree_in.replace("\n0", "\n\n0").as_bytes(),
            ),
            ("", &["mktree"], missing),
            ("", &["mktree", "--missing"], missing),
            (
                "",
                &["mktree"],
                format!("100644 tree {blob}\tz\n").as_bytes(),
            ),
            (
                "",
                &["mktree"],
                format!("100644 blob {blob}\tz/y\n").as_bytes(),
            ),
            (
                "",
                &["mktree", "-z"],
                format!("100644 blob {blob}\tz\0").as_bytes(),
            ),
        ],
    );
    let dir = repo("mktag");
    let head = git(&dir, &["rev-parse", "HEAD"], &[]);
    let tag = |object: &str, kind: &str, rest: &str| {
        format!("object {object}\ntype {kind}\ntag v2\n{rest}").into_bytes()
    };
    let head = head.trim();
    let ok = "tagger T <t@e> 1700000000 +0000\n\nhi\n";
    for input in [
        tag(head, "commit", ok),
        tag(head, "tree", ok),
        tag(head, "commit", "\nhi\n"),
        tag(
            head,
            "commit",
            "tagger T <t@e> 1700000000 +0000\nfoo bar\n\nhi\n",
        ),
        tag(head, "commit", "tagger T <t@e> 1700000000 +0000\n"),
        tag(head, "commit", "tagger T t@e 1700000000 +0000\n\nx\n"),
        tag("0000000000000000000000000000000000000001", "commit", ok),
        format!("object {head}\ntype commit\ntag v 1\n{ok}").into_bytes(),
    ] {
        same_input(&dir, &["mktag"], &input);
        same_input(&dir, &["mktag", "--no-strict"], &input);
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn index_writers_match_git() {
    let a_blob = "78981922613b2afb6025042ff6bd878ac1994e85";
    let info = format!(
        "100644 {a_blob} 1\tc2\n100644 {a_blob} 2\tc2\n0 {a_blob}\te.txt\n100755 blob {a_blob}\tw\n"
    );
    let cacheinfo = format!("100644,{a_blob},x");
    twins(
        "update-index",
        &[
            ("", &["hash-object", "-w", "--stdin"], b"a\n"),
            ("", &["update-index", "u.txt"], b""),
            ("", &["update-index", "--add", "u.txt"], b""),
            ("", &["update-index", "dir/sub/b"], b""),
            ("", &["update-index", "--remove", "dir/sub/b"], b""),
            ("", &["update-index", "--force-remove", "e.txt"], b""),
            ("", &["update-index", "--cacheinfo", &cacheinfo], b""),
            (
                "",
                &[
                    "update-index",
                    "--add",
                    "--cacheinfo",
                    "100644",
                    a_blob,
                    "x",
                ],
                b"",
            ),
            ("", &["update-index", "--chmod=+x", "x.log"], b""),
            (
                "dir",
                &[
                    "update-index",
                    "--add",
                    "--chmod=+x",
                    "--verbose",
                    "a",
                    "../x.log",
                ],
                b"",
            ),
            ("", &["update-index", "--chmod=-x", "dir/a"], b""),
            ("", &["update-index", "--assume-unchanged", "c.txt"], b""),
            ("", &["update-index", "--skip-worktree", "s.txt"], b""),
            (
                "",
                &[
                    "update-index",
                    "--no-assume-unchanged",
                    "c.txt",
                    "--no-skip-worktree",
                    "s.txt",
                ],
                b"",
            ),
            ("", &["update-index", "--index-info"], info.as_bytes()),
            ("", &["update-index", "--refresh"], b""),
            ("", &["update-index", "-q", "--refresh"], b""),
            ("dir", &["update-index", "--really-refresh"], b""),
            (
                "",
                &["update-index", "--add", "--stdin"],
                b"dir/y.tmp\nbuild/out.o\n",
            ),
            ("", &["update-index", "--add", "-z", "--stdin"], b"u.txt\0"),
            ("", &["update-index", "--add", "--info-only", "x.log"], b""),
            ("", &["update-index", "--stdin", "u.txt"], b""),
        ],
    );
    twins(
        "checkout-index",
        &[
            ("", &["checkout-index", "dir/a"], b""),
            ("", &["checkout-index", "dir/sub/b", "nope"], b""),
            ("", &["checkout-index", "-a"], b""),
            ("dir", &["checkout-index", "-f", "-u", "a"], b""),
            ("", &["checkout-index", "-f", "-a", "--prefix=out/"], b""),
            (
                "",
                &["checkout-index", "-n", "-f", "--stdin"],
                b"c.txt\ne.txt\n",
            ),
        ],
    );
    twins(
        "read-tree",
        &[
            ("", &["read-tree", "side"], b""),
            ("", &["read-tree", "-u", "HEAD"], b""),
            ("", &["read-tree", "--reset", "-u", "HEAD"], b""),
            ("", &["read-tree", "--prefix=v/", "side"], b""),
            ("", &["read-tree", "--prefix=v", "side"], b""),
            ("", &["read-tree", "-m", "-u", "HEAD", "side"], b""),
            ("", &["read-tree", "--reset", "-u", "HEAD"], b""),
            ("", &["read-tree", "-m", "HEAD~1", "HEAD", "side"], b""),
            ("", &["read-tree", "--empty"], b""),
            (
                "",
                &[
                    "read-tree",
                    "-m",
                    "--aggressive",
                    "-i",
                    "HEAD~2",
                    "HEAD",
                    "side",
                ],
                b"",
            ),
            ("", &["write-tree"], b""),
            ("", &["read-tree", "--reset", "-u", "side"], b""),
            ("", &["read-tree", "-m", "-u", "side", "HEAD"], b""),
            ("", &["read-tree", "-n", "-m", "HEAD", "side"], b""),
        ],
    );
    // A tree whose c.txt differs from HEAD's, for a 3-way conflict.
    let probe = repo("read-tree-probe");
    let new = git(&probe, &["hash-object", "-w", "--stdin"], &[]);
    let new = new.trim();
    let cacheinfo = format!("100644,{new},c.txt");
    git(&probe, &["update-index", "--cacheinfo", &cacheinfo], &[]);
    let theirs = git(&probe, &["write-tree"], &[]);
    let theirs = theirs.trim();
    let _ = std::fs::remove_dir_all(&probe);
    let three = ["read-tree", "-m", "HEAD~2", "HEAD", theirs];
    twins(
        "read-tree-3way",
        &[
            ("", &["update-index", "--cacheinfo", &cacheinfo], b""),
            ("", &["write-tree"], b""),
            ("", &["read-tree", "--reset", "-u", "HEAD"], b""),
            ("", &three, b""),
            ("", &["write-tree"], b""),
            ("", &["read-tree", "-m", "HEAD"], b""),
            ("", &["read-tree", "--reset", "HEAD"], b""),
            (
                "",
                &[
                    "read-tree",
                    "-m",
                    "-u",
                    "--aggressive",
                    "HEAD~2",
                    "HEAD",
                    theirs,
                ],
                b"",
            ),
            ("", &["read-tree", "--reset", "-u", "HEAD"], b""),
            ("", &["read-tree", "-m", "-u", "HEAD", theirs], b""),
        ],
    );
}

#[test]
fn get_tar_commit_id_reads_the_pax_comment() {
    let dir = repo("tar-commit-id");
    let tar = |rev: &str| run("git", &dir, &["archive", rev], &[]).stdout;
    let id = git(&dir, &["rev-parse", "HEAD"], &[]);
    same_input(&dir, &["get-tar-commit-id"], &tar("HEAD"));
    same_input(&dir, &["get-tar-commit-id"], &tar("HEAD^{tree}"));
    same_input(&dir, &["get-tar-commit-id"], b"short");
    let got = |input: &[u8]| {
        use std::io::Write;
        let mut child = Command::new(env!("CARGO_BIN_EXE_rgit"))
            .args(["--human", "get-tar-commit-id"])
            .current_dir(std::env::temp_dir())
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        child.stdin.take().unwrap().write_all(input).unwrap();
        child.wait_with_output().unwrap()
    };
    let out = got(&tar("HEAD"));
    assert!(out.status.success());
    assert_eq!(String::from_utf8_lossy(&out.stdout), id);
    assert_eq!(got(&tar("HEAD^{tree}")).status.code(), Some(1));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn fmt_merge_msg_matches_git() {
    let dir = repo("fmt-merge-msg");
    git(&dir, &["stash", "-u", "-q"], &[]);
    for (branch, n, author) in [("topic", 6, "Bob"), ("fix", 7, "Carol")] {
        git(&dir, &["checkout", "-q", "-b", branch, "main"], &[]);
        for k in 0..3 {
            write(&dir, &format!("{branch}{k}.txt"), b"x\n");
            git(&dir, &["add", "."], &[]);
            commit(&dir, n, &format!("{branch} work {k}"), author);
        }
    }
    git(
        &dir,
        &["tag", "-a", "v2", "-m", "second release\n\nwith notes"],
        &[],
    );
    git(&dir, &["checkout", "-q", "main"], &[]);
    git(
        &dir,
        &[
            "config",
            "branch.topic.description",
            "the topic\nsecond line",
        ],
        &[],
    );
    let id = |r: &str| git(&dir, &["rev-parse", r], &[]).trim().to_owned();
    let (topic, fix, v2, side) = (id("topic"), id("fix"), id("v2"), id("side"));
    let inputs = [
        format!("{topic}\t\tbranch 'topic' of .\n"),
        format!("{topic}\t\tbranch 'topic' of .\n{fix}\t\tbranch 'fix' of .\n"),
        format!("{v2}\t\ttag 'v2' of .\n"),
        format!(
            "{topic}\t\tbranch 'topic' of https://example.com/r.git\n{fix}\tnot-for-merge\tbranch 'fix' of https://example.com/r.git\n"
        ),
        format!(
            "{topic}\t\tbranch 'topic' of https://example.com/r.git\n{fix}\t\t'fix' of https://example.com/s.git\n{v2}\t\tremote-tracking branch 'origin/x' of .\n"
        ),
        format!("{topic}\t\thttps://example.com/r.git\n"),
        format!("{side}\t\tbranch 'side' of .\n{topic}\t\tbranch 'topic' of .\n"),
        format!("{fix}\t\tcommit '{fix}' of .\n"),
    ];
    for input in &inputs {
        same_input(&dir, &["fmt-merge-msg"], input.as_bytes());
        same_input(&dir, &["fmt-merge-msg", "--log"], input.as_bytes());
        same_input(
            &dir,
            &["fmt-merge-msg", "--log=1", "-m", "Custom"],
            input.as_bytes(),
        );
        same_input(
            &dir,
            &["fmt-merge-msg", "--into-name", "rel"],
            input.as_bytes(),
        );
    }
    git(&dir, &["config", "merge.log", "2"], &[]);
    git(&dir, &["config", "merge.branchdesc", "true"], &[]);
    git(&dir, &["config", "merge.suppressDest", "rel*"], &[]);
    same_input(&dir, &["fmt-merge-msg"], inputs[1].as_bytes());
    same_input(&dir, &["fmt-merge-msg", "--no-log"], inputs[1].as_bytes());
    same_input(
        &dir,
        &["fmt-merge-msg", "--into-name", "release"],
        inputs[0].as_bytes(),
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn interpret_trailers_matches_git() {
    let dir = repo("trailers");
    let msgs: &[&[u8]] = &[
        b"",
        b"subject",
        b"subject\n\nbody\n",
        b"subject\n\nbody\n\nSigned-off-by: A <a@x>\nAcked-by: B\n",
        b"subject\n\nSigned-off-by: A\n  continued here\nFixes: 123\n",
        b"subject\n\nbody text\nmore text\nnot: a trailer block really\nSigned-off-by: A\n",
        b"subject\n\nsome prose\n(cherry picked from commit abc)\nMore prose\nx\n",
        b"subject\n\nReviewed-by: R\n\n# comment\n# more\n",
        b"subject\n\nReviewed-by: R\n---\n diff --git a/x b/x\n",
        b"subject\n\nReviewed-by: R\n# ------------------------ >8 ------------------------\ncut: yes\n",
        b"Signed-off-by: only title\n",
        b"subject\n\nbody\n\nfoo bar\nSigned-off-by: A\n",
        b"subject\n\nKey: v1\nKey: v2\nOther: o\n",
    ];
    let cases: &[&[&str]] = &[
        &["interpret-trailers"],
        &["interpret-trailers", "--trailer", "Acked-by: Z"],
        &["interpret-trailers", "--trailer", "Signed-off-by=A <a@x>"],
        &[
            "interpret-trailers",
            "--trailer",
            "key: v2",
            "--trailer",
            "key=v3",
        ],
        &[
            "interpret-trailers",
            "--where",
            "start",
            "--trailer",
            "Key: s",
        ],
        &[
            "interpret-trailers",
            "--where=before",
            "--trailer",
            "Key: b",
            "--no-where",
            "--trailer",
            "Other: e",
        ],
        &[
            "interpret-trailers",
            "--where",
            "after",
            "--if-exists",
            "replace",
            "--trailer",
            "key: new",
        ],
        &[
            "interpret-trailers",
            "--if-exists",
            "addIfDifferent",
            "--trailer",
            "Key: v1",
            "--trailer",
            "Key: v9",
        ],
        &[
            "interpret-trailers",
            "--if-exists",
            "add",
            "--trailer",
            "Key: v2",
        ],
        &[
            "interpret-trailers",
            "--if-exists",
            "doNothing",
            "--trailer",
            "Key: z",
        ],
        &[
            "interpret-trailers",
            "--if-missing",
            "doNothing",
            "--trailer",
            "New: z",
            "--trailer",
            "Key: n",
        ],
        &["interpret-trailers", "--trailer", "Empty", "--trim-empty"],
        &["interpret-trailers", "--trailer", "Empty"],
        &["interpret-trailers", "--only-trailers"],
        &["interpret-trailers", "--only-trailers", "--unfold"],
        &["interpret-trailers", "--parse"],
        &["interpret-trailers", "--no-divider", "--trailer", "X: y"],
        &["interpret-trailers", "--trailer", ": novalue"],
    ];
    for msg in msgs {
        for args in cases {
            same_input(&dir, args, msg);
        }
    }
    for (k, v) in [
        ("trailer.sign.key", "Signed-off-by: "),
        ("trailer.ack.key", "Acked-by"),
        ("trailer.ack.where", "start"),
        ("trailer.separators", ":#"),
        ("trailer.ifexists", "addIfDifferent"),
        ("trailer.see.command", "echo got $ARG"),
    ] {
        git(&dir, &["config", k, v], &[]);
    }
    let conf_cases: &[&[&str]] = &[
        &["interpret-trailers"],
        &["interpret-trailers", "--trailer", "sign=Me"],
        &[
            "interpret-trailers",
            "--trailer",
            "ack: You",
            "--trailer",
            "Key#x",
        ],
        &["interpret-trailers", "--trailer", "see: it"],
        &["interpret-trailers", "--only-input"],
    ];
    for msg in msgs {
        for args in conf_cases {
            same_input(&dir, args, msg);
        }
    }
    write(&dir, "m1.txt", b"subject\n\nbody\n");
    same(
        &dir,
        &[&[
            "interpret-trailers",
            "--trailer",
            "a: b",
            "m1.txt",
            "m1.txt",
        ][..]],
    );
    write(&dir, "m2.txt", b"subject\n\nbody\n");
    let want = git(
        &dir,
        &["interpret-trailers", "--trailer", "a: b", "m1.txt"],
        &[],
    );
    assert!(
        rgit(
            &dir,
            &[
                "interpret-trailers",
                "--in-place",
                "--trailer",
                "a: b",
                "m2.txt"
            ]
        )
        .status
        .success()
    );
    assert_eq!(std::fs::read_to_string(dir.join("m2.txt")).unwrap(), want);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn show_branch_matches_git() {
    let dir = repo("show-branch");
    git(&dir, &["stash", "-u", "-q"], &[]);
    git(&dir, &["branch", "old", "HEAD~1"], &[]);
    git(&dir, &["checkout", "-q", "-b", "feat", "HEAD~1"], &[]);
    for (n, f) in [(6, "f1"), (7, "[PATCH] f2\nwrapped")] {
        write(&dir, &format!("{n}.txt"), b"x\n");
        git(&dir, &["add", "."], &[]);
        commit(&dir, n, f, "T");
    }
    git(&dir, &["checkout", "-q", "-b", "topic", "side"], &[]);
    write(&dir, "t.txt", b"t\n");
    git(&dir, &["add", "."], &[]);
    commit(&dir, 8, "topic work", "T");
    let date = "2024-01-09T10:00:00+0200";
    git(
        &dir,
        &["merge", "-q", "--no-ff", "-m", "merge feat", "feat"],
        &[("GIT_AUTHOR_DATE", date), ("GIT_COMMITTER_DATE", date)],
    );
    git(&dir, &["checkout", "-q", "main"], &[]);
    git(&dir, &["tag", "feat", "HEAD~1"], &[]);
    git(
        &dir,
        &["update-ref", "refs/remotes/origin/main", "HEAD~1"],
        &[],
    );
    git(
        &dir,
        &["update-ref", "refs/remotes/origin/feat", "feat"],
        &[],
    );
    same(
        &dir,
        &[
            &["show-branch"][..],
            &["show-branch", "--all"],
            &["show-branch", "-r"],
            &["show-branch", "main", "side", "heads/feat"],
            &["show-branch", "--more=2", "main", "side"],
            &["show-branch", "--more", "main", "topic"],
            &["show-branch", "--list"],
            &["show-branch", "--list", "-a"],
            &["show-branch", "--merge-base", "main", "heads/feat", "topic"],
            &["show-branch", "--merge-base", "old", "side"],
            &[
                "show-branch",
                "--independent",
                "main",
                "side",
                "heads/feat",
                "old",
            ],
            &["show-branch", "--no-name", "main", "topic"],
            &["show-branch", "--sha1-name", "main", "topic"],
            &["show-branch", "--topics", "main", "topic", "side"],
            &["show-branch", "--sparse", "main", "topic", "side"],
            &[
                "show-branch",
                "--date-order",
                "main",
                "side",
                "heads/feat",
                "topic",
            ],
            &["show-branch", "--topo-order", "old", "topic"],
            &["show-branch", "--current", "side", "topic"],
            &["show-branch", "t*", "heads/*"],
            &["show-branch", "main"],
            &["show-branch", "main~1", "topic^2"],
            &["show-branch", "--color=always", "main", "topic"],
            &["show-branch", "--reflog=2", "main"],
            &["show-branch", "-g"],
            &["show-branch", "--reflog=3,1", "topic"],
        ],
    );
    git(
        &dir,
        &["config", "--add", "showbranch.default", "--topo-order"],
        &[],
    );
    git(
        &dir,
        &["config", "--add", "showbranch.default", "side"],
        &[],
    );
    git(&dir, &["config", "--add", "showbranch.default", "old"], &[]);
    same(
        &dir,
        &[&["show-branch"][..], &["show-branch", "main", "topic"]],
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn mailsplit_and_mailinfo_match_git() {
    let dir = repo("mail");
    let mbox = git(&dir, &["format-patch", "--stdout", "-2", "HEAD~1"], &[]);
    let crlf = mbox.replace('\n', "\r\n");
    write(&dir, "series.mbox", mbox.as_bytes());
    write(&dir, "crlf.mbox", crlf.as_bytes());
    write(
        &dir,
        "bare.txt",
        b"Subject: [PATCH v2 1/1] [tag] bare one\n\nbody\n---\n a | 1 +\n",
    );
    let listing = |d: &Path| -> Vec<(String, Vec<u8>)> {
        let mut v: Vec<_> = std::fs::read_dir(d)
            .unwrap()
            .flatten()
            .map(|e| {
                (
                    e.file_name().to_string_lossy().into_owned(),
                    std::fs::read(e.path()).unwrap(),
                )
            })
            .collect();
        v.sort();
        v
    };
    for (i, args) in [
        &["series.mbox"][..],
        &["-d3", "-f7", "series.mbox", "crlf.mbox"],
        &["-d2", "series.mbox"],
        &["--keep-cr", "crlf.mbox"],
        &["-b", "bare.txt"],
        &["bare.txt"],
    ]
    .iter()
    .enumerate()
    {
        let (g, r) = (format!("g{i}"), format!("r{i}"));
        std::fs::create_dir_all(dir.join(&g)).unwrap();
        std::fs::create_dir_all(dir.join(&r)).unwrap();
        let mut ga = vec!["mailsplit".to_owned(), format!("-o{g}")];
        let mut ra = vec!["mailsplit".to_owned(), format!("-o{r}")];
        ga.extend(args.iter().map(|s| s.to_string()));
        ra.extend(args.iter().map(|s| s.to_string()));
        let ga: Vec<&str> = ga.iter().map(String::as_str).collect();
        let ra: Vec<&str> = ra.iter().map(String::as_str).collect();
        let want = run("git", &dir, &ga, &[]);
        let got = rgit(&dir, &ra);
        assert_eq!(got.stdout, want.stdout, "{args:?}");
        assert_eq!(got.status.success(), want.status.success(), "{args:?}");
        assert_eq!(listing(&dir.join(&r)), listing(&dir.join(&g)), "{args:?}");
    }
    let mails = [
        mbox.split("\nFrom ").next().unwrap().to_owned() + "\n",
        "From: \"Doe, Jane\" <jane@example.com>\nSubject: Re: [PATCH 2/3] [RFC] fix\n thing\nDate: Mon, 1 Jan 2024 10:00:00 +0000\n\nFrom: Other <o@example.com>\nSubject: in-body\n\nmessage\n-- >8 --\nafter scissors\n---\ndiff --git a/x b/x\n".to_owned(),
        "Subject: no patch\n\njust text\n".to_owned(),
    ];
    for mail in &mails {
        for flags in [&[][..], &["-k"], &["-b"], &["--scissors"], &["-m"]] {
            // git -k keeps an in-body Subject's newline and prints an extra
            // empty `Subject: ` line; rgit does not copy that.
            if flags == ["-k"] && mail.contains("\n\nFrom: Other") {
                continue;
            }
            let feed = |bin: &str, pre: &[&str], out: &str| {
                use std::io::Write;
                let mut args: Vec<String> = pre.iter().map(|s| s.to_string()).collect();
                args.push("mailinfo".to_owned());
                args.extend(flags.iter().map(|s| s.to_string()));
                args.push(format!("{out}.msg"));
                args.push(format!("{out}.patch"));
                let mut child = Command::new(bin)
                    .args(&args)
                    .current_dir(&dir)
                    .env("GIT_CONFIG_GLOBAL", "/dev/null")
                    .env("GIT_CONFIG_NOSYSTEM", "1")
                    .stdin(std::process::Stdio::piped())
                    .stdout(std::process::Stdio::piped())
                    .spawn()
                    .unwrap();
                child
                    .stdin
                    .take()
                    .unwrap()
                    .write_all(mail.as_bytes())
                    .unwrap();
                let out_text = child.wait_with_output().unwrap().stdout;
                (
                    String::from_utf8_lossy(&out_text).into_owned(),
                    std::fs::read_to_string(dir.join(format!("{out}.msg"))).unwrap(),
                    std::fs::read_to_string(dir.join(format!("{out}.patch"))).unwrap(),
                )
            };
            let want = feed("git", &[], "g");
            let got = feed(env!("CARGO_BIN_EXE_rgit"), &["--human"], "r");
            assert_eq!(got, want, "{flags:?} {mail}");
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
}
