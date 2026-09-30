//! `rgit fast-export`, `fast-import` and `replay` against git's own.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

const DATE: &str = "1700000000 +0100";

fn run(program: &str, dir: &Path, args: &[&str], input: Option<&[u8]>) -> Output {
    let mut cmd = Command::new(program);
    if program != "git" {
        cmd.arg("--human");
    }
    cmd.args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_DATE", DATE)
        .env("GIT_COMMITTER_DATE", DATE)
        .env("RGIT_OPLOG", "0")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = cmd.spawn().unwrap();
    let mut stdin = child.stdin.take().unwrap();
    stdin.write_all(input.unwrap_or_default()).unwrap();
    drop(stdin);
    child.wait_with_output().unwrap()
}

fn git(dir: &Path, args: &[&str]) -> Vec<u8> {
    let out = run("git", dir, args, None);
    assert!(out.status.success(), "git {args:?}: {:?}", out);
    out.stdout
}

fn rgit(dir: &Path, args: &[&str], input: Option<&[u8]>) -> Output {
    run(env!("CARGO_BIN_EXE_rgit"), dir, args, input)
}

fn fresh(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("rgit-fast-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    git(&dir, &["init", "-q", "-b", "main"]);
    git(&dir, &["config", "user.email", "t@t"]);
    git(&dir, &["config", "user.name", "T"]);
    dir
}

/// Files, a binary, an executable, a symlink, a rename, a merge and tags.
fn history(tag: &str) -> PathBuf {
    let dir = fresh(tag);
    std::fs::write(dir.join("a"), "a\n").unwrap();
    std::fs::create_dir(dir.join("d")).unwrap();
    std::fs::write(dir.join("d/bin"), b"x\0y").unwrap();
    std::fs::write(dir.join("sp ace"), "s\n").unwrap();
    std::os::unix::fs::symlink("a", dir.join("link")).unwrap();
    git(&dir, &["update-index", "--add", "--chmod=+x", "a"]);
    git(&dir, &["add", "."]);
    git(&dir, &["commit", "-qm", "init"]);
    git(&dir, &["tag", "-a", "v1", "-m", "tag msg"]);
    git(&dir, &["tag", "lw"]);
    git(&dir, &["checkout", "-qb", "side"]);
    std::fs::write(dir.join("b"), "b\n").unwrap();
    git(&dir, &["add", "b"]);
    git(&dir, &["commit", "-qm", "side\n\nbody"]);
    git(&dir, &["checkout", "-q", "main"]);
    git(&dir, &["mv", "a", "a2"]);
    git(&dir, &["rm", "-q", "d/bin"]);
    git(&dir, &["commit", "-qm", "mv"]);
    git(&dir, &["merge", "-q", "--no-edit", "side"]);
    dir
}

#[test]
fn fast_export_matches_git() {
    let dir = history("export");
    for args in [
        &["--all"][..],
        &["HEAD"],
        &["main", "side"],
        &["main~1..main"],
        &["v1", "lw"],
        &["--all", "--no-data"],
        &["--all", "--full-tree"],
        &["--all", "--mark-tags", "--show-original-ids"],
        &["--all", "--use-done-feature"],
        &["--all", "--", "b", "link"],
        &["main~1..main", "--", "b"],
        &["--all", "--refspec", "refs/heads/*:refs/heads/x/*"],
    ] {
        let mut full = vec!["fast-export"];
        full.extend_from_slice(args);
        let want = git(&dir, &full);
        let got = rgit(&dir, &full, None);
        assert!(got.status.success(), "{args:?}: {got:?}");
        assert_eq!(
            String::from_utf8_lossy(&got.stdout),
            String::from_utf8_lossy(&want),
            "{args:?}"
        );
    }
}

#[test]
fn fast_import_round_trips_git_streams() {
    let src = history("src");
    let stream = git(&src, &["fast-export", "--all"]);
    let dst = fresh("dst");
    let out = rgit(&dst, &["fast-import", "--quiet"], Some(&stream));
    assert!(out.status.success(), "{out:?}");
    let refs = ["for-each-ref", "--format=%(refname) %(objectname)"];
    assert_eq!(git(&dst, &refs), git(&src, &refs));
    run("git", &dst, &["fsck", "--strict"], None);
}

const STREAM: &str = "feature done
# a comment
blob
mark :1
data <<EOF
hello
EOF

commit refs/heads/main
mark :2
committer C <c@c> 1700000000 +0000
data 6
first
M 644 :1 a.txt
M 100755 inline \"dir/sp ace\"
data 4
x y
M 120000 inline link
data 5
a.txt

progress one done
commit refs/heads/main
mark :3
author A <a@a> 1700000100 -0130
committer C <c@c> 1700000200 +0000
data 7
second
R a.txt dir/b.txt
C \"dir/sp ace\" copy
D link
ls \"dir\"
cat-blob :1

get-mark :2
reset refs/heads/other
from :2

commit refs/heads/other
committer C <c@c> 1700000300 +0000
data 5
other
deleteall
M 644 :1 only

commit refs/heads/main
mark :4
committer C <c@c> 1700000400 +0000
data 5
merge
merge refs/heads/other
M 644 :1 only

tag v1
from :4
tagger T <t@t> 1700000500 +0000
data 3
tag
ls :3 dir/b.txt
ls :3 nope
checkpoint

commit refs/notes/commits
committer C <c@c> 1700000600 +0000
data 4
note
N inline :3
data 5
note

alias
mark :9
to :3

reset refs/heads/aliased
from :9

done
";

#[test]
fn fast_import_matches_git_on_a_hand_written_stream() {
    let g = fresh("hand-git");
    let r = fresh("hand-rgit");
    let want = run(
        "git",
        &g,
        &["fast-import", "--quiet", "--export-marks=marks"],
        Some(STREAM.as_bytes()),
    );
    let got = rgit(
        &r,
        &["fast-import", "--quiet", "--export-marks=marks"],
        Some(STREAM.as_bytes()),
    );
    assert!(got.status.success(), "{got:?}");
    assert_eq!(
        String::from_utf8_lossy(&got.stdout),
        String::from_utf8_lossy(&want.stdout)
    );
    let refs = ["for-each-ref", "--format=%(refname) %(objectname)"];
    assert_eq!(git(&r, &refs), git(&g, &refs));
    assert_eq!(
        std::fs::read_to_string(r.join("marks")).unwrap(),
        std::fs::read_to_string(g.join("marks")).unwrap()
    );

    let rewind = b"commit refs/heads/main\ncommitter C <c@c> 1 +0000\ndata 1\nx\n\n";
    let out = rgit(&r, &["fast-import", "--quiet"], Some(rewind));
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stderr).contains("Not updating refs/heads/main"));
    let out = rgit(&r, &["fast-import", "--quiet", "--force"], Some(rewind));
    assert!(out.status.success());
    let out = rgit(&r, &["fast-import"], Some(b"bogus\n"));
    assert_eq!(out.status.code(), Some(128));
    assert!(String::from_utf8_lossy(&out.stderr).contains("Unsupported command: bogus"));
}

#[test]
fn replay_matches_git() {
    let dir = fresh("replay");
    let commit = |file: &str, text: &str, msg: &str| {
        std::fs::write(dir.join(file), text).unwrap();
        git(&dir, &["add", file]);
        git(&dir, &["commit", "-qm", msg]);
    };
    commit("a", "a\n", "one");
    git(&dir, &["checkout", "-qb", "topic"]);
    commit("b", "b\n", "two");
    commit("c", "c\n", "three");
    git(&dir, &["branch", "mid", "HEAD~1"]);
    git(&dir, &["checkout", "-q", "main"]);
    commit("b", "b\n", "same b");
    commit("d", "d\n", "four");
    git(&dir, &["checkout", "-qb", "clash", "main~1"]);
    commit("d", "z\n", "clash");
    git(&dir, &["checkout", "-q", "main"]);
    // git 2.53+ reworked replay: refs update silently by default (the old
    // update lines need --ref-action=print), nothing prints on a conflict,
    // and commits that become empty are dropped. rgit tracks the new
    // semantics, which no flag on an older git can reproduce, so skip when
    // the oracle predates --ref-action.
    let usage = run("git", &dir, &["replay", "-h"], None);
    let usage = format!(
        "{}{}",
        String::from_utf8_lossy(&usage.stdout),
        String::from_utf8_lossy(&usage.stderr)
    );
    if !usage.contains("--ref-action") {
        eprintln!("skipping: git predates replay --ref-action");
        return;
    }
    for args in [
        &["--onto", "main", "main..topic"][..],
        &["--onto", "main", "main..topic", "main..mid"],
        &["--contained", "--onto", "main", "main..topic"],
        &["--advance", "main", "main..topic"],
        &["--onto", "main", "main~2..clash"],
        &["--advance", "main", "main~2..clash"],
        &["--onto", "main", "--advance", "main", "main..topic"],
    ] {
        // The flag goes before the range so rgit's allow-hyphen revs do not
        // swallow it.
        let mut full = vec!["replay", "--ref-action=print"];
        full.extend_from_slice(args);
        let got = rgit(&dir, &full, None);
        let want = run("git", &dir, &full, None);
        assert_eq!(got.status.code(), want.status.code(), "{args:?}: {got:?}");
        assert_eq!(
            String::from_utf8_lossy(&got.stdout),
            String::from_utf8_lossy(&want.stdout),
            "{args:?}"
        );
    }
    assert_eq!(git(&dir, &["status", "--porcelain"]), b"");
}
