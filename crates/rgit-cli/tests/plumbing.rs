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
