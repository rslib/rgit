//! Commands that write commits, tags or reflog entries take the identity as
//! git does: GIT_{AUTHOR,COMMITTER}_* before config, with no user.name set.

use std::path::{Path, PathBuf};
use std::process::Command;

const ENV: [(&str, &str); 7] = [
    ("GIT_AUTHOR_NAME", "Ann Author"),
    ("GIT_AUTHOR_EMAIL", "ann@example.com"),
    ("GIT_AUTHOR_DATE", "2020-01-01T00:00:00Z"),
    ("GIT_COMMITTER_NAME", "Cy Committer"),
    ("GIT_COMMITTER_EMAIL", "cy@example.com"),
    ("GIT_COMMITTER_DATE", "2020-01-02T00:00:00Z"),
    ("GIT_EDITOR", "true"),
];

fn run(dir: &Path, bin: &str, args: &[&str]) -> String {
    let home = dir.with_extension("home");
    std::fs::create_dir_all(&home).unwrap();
    let out = Command::new(bin)
        .args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("HOME", &home)
        .env("XDG_CONFIG_HOME", &home)
        .env_remove("EMAIL")
        .envs(ENV)
        .env("RGIT_OPLOG", "0")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{bin} {args:?}: {}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn git(dir: &Path, args: &[&str]) -> String {
    run(dir, "git", args)
}

fn rgit(dir: &Path, args: &[&str]) -> String {
    run(dir, env!("CARGO_BIN_EXE_rgit"), args)
}

/// A repo with no identity in any config: main with two commits, and a
/// branch `side` with one.
fn repo(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("rgit-ident-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    git(&dir, &["init", "-q", "-b", "main"]);
    for (f, msg) in [("a", "one"), ("b", "two")] {
        std::fs::write(dir.join(f), format!("{f}\n")).unwrap();
        git(&dir, &["add", f]);
        git(&dir, &["commit", "-qm", msg]);
    }
    git(&dir, &["checkout", "-qb", "side", "HEAD~1"]);
    std::fs::write(dir.join("s"), "s\n").unwrap();
    git(&dir, &["add", "s"]);
    git(&dir, &["commit", "-qm", "side"]);
    git(&dir, &["checkout", "-q", "main"]);
    dir
}

fn committer(dir: &Path, rev: &str) -> String {
    git(dir, &["log", "-1", "--format=%an <%ae> | %cn <%ce>", rev])
}

#[test]
fn writes_take_the_identity_from_the_environment_like_git() {
    // Annotated tag: the tagger is the committer identity.
    let (g, r) = (repo("tag-git"), repo("tag-rgit"));
    git(&g, &["tag", "-a", "v1", "-m", "release"]);
    rgit(&r, &["tag", "-a", "v1", "-m", "release"]);
    let tagger = |d: &Path| {
        git(d, &["cat-file", "tag", "v1"])
            .lines()
            .nth(3)
            .unwrap()
            .to_owned()
    };
    assert_eq!(tagger(&r), tagger(&g));
    assert!(tagger(&r).starts_with("tagger Cy Committer <cy@example.com>"));

    // Rewrites keep the author and take the committer from the environment.
    let dir = repo("rewrite");
    rgit(&dir, &["reword", "-m", "two again"]);
    assert_eq!(
        committer(&dir, "HEAD"),
        "Ann Author <ann@example.com> | Cy Committer <cy@example.com>\n"
    );
    rgit(&dir, &["squash"]);
    assert_eq!(
        committer(&dir, "HEAD"),
        "Ann Author <ann@example.com> | Cy Committer <cy@example.com>\n"
    );

    let dir = repo("rebase");
    rgit(&dir, &["rebase", "side"]);
    assert_eq!(
        committer(&dir, "HEAD"),
        "Ann Author <ann@example.com> | Cy Committer <cy@example.com>\n"
    );

    // Reflog entries written by update-ref name the committer.
    let dir = repo("update-ref");
    rgit(
        &dir,
        &["update-ref", "-m", "moved", "refs/heads/side", "HEAD"],
    );
    let entry = git(
        &dir,
        &["reflog", "show", "--format=%gn <%ge>", "-1", "side"],
    );
    assert_eq!(entry, "Cy Committer <cy@example.com>\n");
}
