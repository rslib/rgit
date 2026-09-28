//! replace, rerere, check-mailmap and the pack commands run natively and
//! print what git prints, with git's exit codes.

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

fn run(bin: &str, dir: &Path, args: &[&str], input: Option<&[u8]>) -> Output {
    let mut cmd = Command::new(bin);
    cmd.args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("RGIT_OPLOG", "0")
        .env("GIT_AUTHOR_DATE", "2024-01-01T10:00:00+0000")
        .env("GIT_COMMITTER_DATE", "2024-01-01T10:00:00+0000")
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@t")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@t")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if bin != "git" {
        // Nothing may fall through to git's own commands.
        cmd.env("GIT_EXEC_PATH", "/nonexistent");
    }
    let mut child = cmd.spawn().unwrap();
    if let Some(data) = input {
        use std::io::Write;
        child.stdin.take().unwrap().write_all(data).unwrap();
    }
    drop(child.stdin.take());
    child.wait_with_output().unwrap()
}

fn git(dir: &Path, args: &[&str]) -> String {
    let out = run("git", dir, args, None);
    assert!(out.status.success(), "git {args:?}: {out:?}");
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn sh(dir: &Path, script: &str) {
    let out = Command::new("sh")
        .arg("-c")
        .arg(script)
        .current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_DATE", "2024-01-01T10:00:00+0000")
        .env("GIT_COMMITTER_DATE", "2024-01-01T10:00:00+0000")
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@t")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@t")
        .output()
        .unwrap();
    assert!(out.status.success(), "{script}: {out:?}");
}

/// Two identical repositories: one for git, one for rgit.
fn pair(tag: &str, setup: &str) -> (PathBuf, PathBuf) {
    let base = std::env::temp_dir().join(format!("rgit-more-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let dirs = (base.join("g"), base.join("r"));
    for d in [&dirs.0, &dirs.1] {
        std::fs::create_dir_all(d).unwrap();
        git(d, &["init", "-q", "-b", "main"]);
        sh(d, setup);
    }
    dirs
}

const HISTORY: &str = "echo a > a && git add a && git commit -qm one && \
    echo b > b && git add b && git commit -qm two && \
    git checkout -qb side HEAD~1 && echo s > a && git commit -qam side && \
    git checkout -q main && echo c > c && git add c && git commit -qm three";

/// Run each case in both repositories, in order, and require the same
/// stdout, stderr and exit code. `!` cases are shell set-up run in both.
fn same(dirs: &(PathBuf, PathBuf), cases: &[&str]) {
    let mut bad = Vec::new();
    for case in cases {
        // `$G` in set-up is git in one repository and rgit in the other.
        if let Some(script) = case.strip_prefix('!') {
            sh(&dirs.0, &script.replace("$G", "git"));
            let rgit = format!("{} --human", env!("CARGO_BIN_EXE_rgit"));
            sh(&dirs.1, &script.replace("$G", &rgit));
            continue;
        }
        // `args <<< text` feeds text; `args < file` feeds each repo's file.
        let (args, text, file) = match (case.split_once(" <<< "), case.split_once(" < ")) {
            (Some((a, i)), _) => (a, Some(i.replace("\\n", "\n").into_bytes()), None),
            (None, Some((a, f))) => (a, None, Some(f)),
            _ => (*case, None, None),
        };
        let input = |d: &Path| {
            text.clone()
                .or_else(|| file.map(|f| std::fs::read(d.join(f)).unwrap()))
        };
        let args: Vec<String> = shell_words(args);
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        let want = run("git", &dirs.0, &args, input(&dirs.0).as_deref());
        let mut human = vec!["--human"];
        human.extend(&args);
        let got = run(
            env!("CARGO_BIN_EXE_rgit"),
            &dirs.1,
            &human,
            input(&dirs.1).as_deref(),
        );
        if got.stdout != want.stdout
            || got.status.code() != want.status.code()
            || strip(&got.stderr) != strip(&want.stderr)
        {
            bad.push(format!(
                "{case}\n  git  ({}): {:?} {:?}\n  rgit ({}): {:?} {:?}",
                want.status,
                String::from_utf8_lossy(&want.stdout),
                String::from_utf8_lossy(&want.stderr),
                got.status,
                String::from_utf8_lossy(&got.stdout),
                String::from_utf8_lossy(&got.stderr),
            ));
        }
    }
    assert!(bad.is_empty(), "{}", bad.join("\n"));
}

/// stderr without the `fatal: `/`error: `/`rgit: ` prefix of a one-line
/// error, which rgit's error printer owns.
fn strip(err: &[u8]) -> String {
    let s = String::from_utf8_lossy(err);
    s.lines()
        .map(|l| {
            l.trim_start_matches("rgit: ")
                .trim_start_matches("fatal: ")
                .trim_start_matches("error: ")
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn shell_words(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut word = String::new();
    let mut quote = false;
    let mut started = false;
    for c in s.chars() {
        match c {
            '\'' => {
                quote = !quote;
                started = true;
            }
            ' ' if !quote => {
                if started {
                    out.push(std::mem::take(&mut word));
                    started = false;
                }
            }
            c => {
                word.push(c);
                started = true;
            }
        }
    }
    if started {
        out.push(word);
    }
    out
}

#[test]
fn replace_matches_git() {
    let dirs = pair("replace", HISTORY);
    same(
        &dirs,
        &[
            "replace HEAD~1 nope",
            "replace HEAD~1 HEAD:a",
            "replace HEAD~1 side",
            "replace HEAD~1 side",
            "replace",
            "replace -l --format=medium",
            "replace -l --format=long",
            "replace -l --format=bad",
            "replace --graft HEAD side",
            "replace -l '3*'",
            "replace -d HEAD~1 nope",
            "replace -d HEAD~1",
            "replace --graft HEAD~2 nope",
            "replace --graft HEAD HEAD~1",
            "replace -d HEAD",
            "replace --edit",
            "replace -d",
            "replace a b c",
            "replace -f -l",
            "replace --raw HEAD side",
            "replace --format=short HEAD side",
            "!echo \"$(git rev-parse HEAD) $(git rev-parse side)\" > .git/info/grafts",
            "replace --convert-graft-file",
            "replace --format=medium",
            "!test ! -e .git/info/grafts",
        ],
    );
    let edit = |dir: &Path, bin: &str| {
        let mut cmd = Command::new(bin);
        if bin != "git" {
            cmd.arg("--human");
        }
        let out = cmd
            .args(["replace", "-f", "--edit", "HEAD~1"])
            .current_dir(dir)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_EDITOR", "sed -i.bak s/two/TWO/")
            .output()
            .unwrap();
        assert!(out.status.success(), "{out:?}");
        git(dir, &["replace", "--format=long"])
    };
    assert_eq!(
        edit(&dirs.1, env!("CARGO_BIN_EXE_rgit")),
        edit(&dirs.0, "git")
    );
}

#[test]
fn check_mailmap_matches_git() {
    let dirs = pair("mailmap", HISTORY);
    same(
        &dirs,
        &[
            "!printf 'Real Name <real@x> <old@x>\\n<proper@x> <Other@X>\\nFull <full@x> Nick <nick@x>\\n' > .mailmap",
            "check-mailmap 'A <old@x>'",
            "check-mailmap '<old@x>'",
            "check-mailmap 'B <other@x>' 'Nick <nick@x>' 'Nack <nick@x>'",
            "check-mailmap '<unknown@x>' 'Z <z@z>' bad '<nick@x>'",
            "check-mailmap",
            "check-mailmap --stdin 'Z <other@x>' <<< A <old@x>\\n<old@x>\\nfoo\\n",
            "!printf 'Map <m@x> <z@z>\\n' > mm",
            "check-mailmap --mailmap-file=mm 'Z <z@z>' '<old@x>'",
            "!git config mailmap.file mm",
            "check-mailmap 'Z <z@z>'",
        ],
    );
}

/// Files that must come out byte for byte the same in both repositories.
fn same_files(dirs: &(PathBuf, PathBuf), files: &[&str]) {
    for f in files {
        let read = |d: &Path| std::fs::read(d.join(f)).unwrap_or_default();
        assert!(read(&dirs.0) == read(&dirs.1), "{f} differs");
    }
}

const PACKED: &str = "echo a > a && git add a && git commit -qm one && \
    echo b > b && git add b && git commit -qm two && \
    git checkout -qb side HEAD~1 && echo s > a && git commit -qam side && \
    git checkout -q main && echo c > c && git add c && git commit -qm three && \
    git tag -a -m tagged v1 HEAD~1 && git repack -q -a -d && \
    echo z > z && git add z && git commit -qm four && git repack -q && \
    git unpack-objects < $(ls .git/objects/pack/*.pack | head -1) && \
    cp $(ls .git/objects/pack/*.pack | head -1) copy.pack";

#[test]
fn pack_plumbing_matches_git() {
    let dirs = pair("packs", PACKED);
    same(
        &dirs,
        &[
            "prune-packed -n",
            "verify-pack -v copy.pack",
            "verify-pack -s copy.pack",
            "verify-pack copy.pack",
            "verify-pack",
            "verify-pack -v nope.idx",
            "!ls .git/objects/pack/*.idx | head -1 | xargs cat > first.idx",
            "show-index --object-format=sha1 < first.idx",
            "show-index < a",
            "index-pack copy.pack",
            "index-pack --verify-stat copy.pack",
            "index-pack -o other.idx copy.pack",
            "index-pack",
            "update-server-info",
            "prune-packed",
            "unpack-file nope",
            "commit-graph verify",
            "commit-graph write --reachable",
            "commit-graph verify",
            "multi-pack-index write",
            "multi-pack-index verify",
            "!git repack -q -a",
            "multi-pack-index write",
            "multi-pack-index expire",
            "!rm -rf .git/objects/pack/*",
            "index-pack --stdin --keep=because < copy.pack",
            "!ls .git/objects/pack > packs.txt",
        ],
    );
    same_files(
        &dirs,
        &[
            "copy.idx",
            "copy.rev",
            "other.idx",
            ".git/info/refs",
            ".git/objects/info/packs",
            ".git/objects/info/commit-graph",
            "packs.txt",
        ],
    );
    let listing = |d: &Path| git(d, &["count-objects", "-v"]);
    assert_eq!(listing(&dirs.1), listing(&dirs.0));
}

#[test]
fn pack_objects_and_unpack_objects_round_trip() {
    let dirs = pair("roundtrip", PACKED);
    let r = &dirs.1;
    let rgit = |args: &[&str], input: Option<&[u8]>| {
        let mut all = vec!["--human"];
        all.extend(args);
        let out = run(env!("CARGO_BIN_EXE_rgit"), r, &all, input);
        assert!(out.status.success(), "{args:?}: {out:?}");
        out.stdout
    };
    let objects = git(r, &["rev-list", "--objects", "--all"]);
    let name = rgit(&["pack-objects", "out"], Some(objects.as_bytes()));
    let name = String::from_utf8(name).unwrap();
    let idx = format!("out-{}.idx", name.trim());
    assert!(git(r, &["verify-pack", "-v", &idx]).contains(": ok"));
    let listed = |text: &str| {
        let mut v: Vec<String> = text
            .lines()
            .map(|l| l.split(' ').next().unwrap_or("").to_owned())
            .collect();
        v.sort();
        v
    };
    let in_pack = String::from_utf8(rgit(
        &["show-index"],
        Some(&std::fs::read(r.join(&idx)).unwrap()),
    ))
    .unwrap();
    let mut ids: Vec<String> = in_pack
        .lines()
        .map(|l| l.split(' ').nth(1).unwrap_or("").to_owned())
        .collect();
    ids.sort();
    assert_eq!(ids, listed(&objects));

    let pack = rgit(&["pack-objects", "--revs", "--stdout"], Some(b"main\n"));
    let fresh = r.with_file_name("fresh");
    let _ = std::fs::remove_dir_all(&fresh);
    std::fs::create_dir_all(&fresh).unwrap();
    git(&fresh, &["init", "-q"]);
    let out = run(
        env!("CARGO_BIN_EXE_rgit"),
        &fresh,
        &["--human", "unpack-objects", "-q"],
        Some(&pack),
    );
    assert!(out.status.success(), "{out:?}");
    let main = git(r, &["rev-parse", "main"]);
    git(&fresh, &["update-ref", "refs/heads/main", main.trim()]);
    git(&fresh, &["fsck", "--strict"]);
    assert_eq!(
        git(&fresh, &["log", "--format=%H", "main"]),
        git(r, &["log", "--format=%H", "main"])
    );
}

#[test]
fn merge_index_runs_merge_one_file_like_git() {
    let dirs = pair(
        "mergeindex",
        "printf '1\\n2\\n3\\n' > m && printf 'x\\n' > d1 && printf 'y\\n' > d2 && \
         git add m d1 d2 && git commit -qm base && git checkout -qb o2 && \
         printf '1\\n2\\n3\\n4\\n' > m && git rm -q d1 && printf 'new\\n' > n2 && \
         printf 'same\\n' > both && git add -A && git commit -qm o2 && git checkout -q main && \
         printf '0\\n1\\n2\\n3\\n' > m && printf 'y2\\n' > d2 && printf 'same\\n' > both && \
         printf 'z\\n' > n3 && git add -A && git commit -qm m2 && \
         git read-tree -m -u HEAD~1 HEAD o2",
    );
    same(
        &dirs,
        &[
            "merge-index",
            "merge-index -q x",
            "merge-index git-merge-one-file nope",
            "merge-index git-merge-one-file -a",
            "!git ls-files -s > stages.txt && ls > files.txt",
            "merge-one-file",
            "merge-one-file a b",
        ],
    );
    same_files(&dirs, &["stages.txt", "files.txt", "m"]);
}

#[test]
fn bugreport_and_diagnose_write_what_git_writes() {
    let dirs = pair("bugreport", HISTORY);
    same(
        &dirs,
        &[
            "!GIT_EDITOR=: $G bugreport -o out -s x 2>/dev/null",
            "!ls out > ls.txt && sed -n '1,/System Info/p' out/git-bugreport-x.txt > head.txt",
            "!sed -n '/Enabled Hooks/,$p' out/git-bugreport-x.txt > hooks.txt",
            "!GIT_EDITOR=: $G bugreport -o out -s x 2>/dev/null; echo $? > again.txt",
            "!$G diagnose -o dz -s y >/dev/null 2>&1 && unzip -l dz/*.zip | awk '{print $4}' | sort > zip.txt",
            "diagnose --mode=bogus",
            "backfill",
        ],
    );
    same_files(
        &dirs,
        &["ls.txt", "head.txt", "hooks.txt", "again.txt", "zip.txt"],
    );
}
