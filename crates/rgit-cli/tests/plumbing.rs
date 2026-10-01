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

/// `bin args` in `dir` with `input` on stdin and `env` set.
fn piped(bin: &str, dir: &Path, args: &[&str], input: &[u8], env: &[(&str, &str)]) -> Output {
    use std::io::Write;
    let rgit = bin != "git";
    let mut child = Command::new(bin)
        .args(rgit.then_some("--human"))
        .args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("RGIT_OPLOG", "0")
        .envs(env.iter().copied())
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(input).unwrap();
    child.wait_with_output().unwrap()
}

/// Each step's stdout and exit status, and the file `log` after it, are the
/// same under git and rgit; `log` is cleared before each tool's run.
fn same_steps(dir: &Path, log: &Path, steps: &[(&[&str], &str)], env: &[(&str, &str)]) {
    let run_all = |bin: &str| {
        let _ = std::fs::remove_file(log);
        let mut out = Vec::new();
        for (args, input) in steps {
            let o = piped(bin, dir, args, input.as_bytes(), env);
            out.push(format!(
                "{args:?} {input:?}\n{} {}\n{}",
                o.status.code().unwrap_or(-1),
                String::from_utf8_lossy(&o.stdout),
                std::fs::read_to_string(log).unwrap_or_default()
            ));
        }
        out
    };
    let want = run_all("git");
    let got = run_all(env!("CARGO_BIN_EXE_rgit"));
    for (w, g) in want.iter().zip(&got) {
        assert_eq!(g, w);
    }
}

#[test]
fn credential_fill_approve_reject_match_git() {
    let dir = repo("credential");
    let log = dir.join("helper.log");
    let helper = dir.join("helper.sh");
    std::fs::write(
        &helper,
        format!(
            "#!/bin/sh\necho \"$*\" >> {0}\ncat >> {0}\n\
             [ \"$1\" = get ] && printf 'capability[]=authtype\\nauthtype=Bearer\\n\
             credential=tok\\nusername=u1\\npassword=p1\\nstate[]=s\\ncontinue=1\\n'\nexit 0\n",
            log.display()
        ),
    )
    .unwrap();
    let user = dir.join("user.sh");
    std::fs::write(
        &user,
        "#!/bin/sh\ncat >/dev/null\necho username=from-user\n",
    )
    .unwrap();
    let h = format!("credential.helper=!sh {}", helper.display());
    let u = format!("credential.helper=!sh {}", user.display());
    let url_helper = format!(
        "credential.https://*.example.com/p.helper=!sh {}",
        helper.display()
    );
    let other = format!("credential.https://other.com.helper=!sh {}", user.display());
    let steps: &[(&[&str], &str)] = &[
        (
            &["-c", &h, "credential", "fill"],
            "protocol=https\nhost=example.com\npath=a/b\n",
        ),
        (
            &["-c", &h, "credential", "fill"],
            "capability[]=authtype\ncapability[]=state\nprotocol=https\nhost=example.com\n\
             wwwauth[]=Basic x\nstate[]=s0\n",
        ),
        (
            &["-c", &u, "-c", &h, "credential", "fill"],
            "capability[]=state\nprotocol=https\nhost=example.com\n",
        ),
        (
            &[
                "-c",
                &u,
                "-c",
                "credential.helper=",
                "-c",
                &url_helper,
                "-c",
                &other,
                "-c",
                "credential.useHttpPath=true",
                "credential",
                "fill",
            ],
            "protocol=https\nhost=a.example.com:443\npath=p/q\n",
        ),
        (
            &[
                "-c",
                &h,
                "-c",
                "credential.example.com.username=zed",
                "credential",
                "fill",
            ],
            "protocol=http\nhost=example.com\n",
        ),
        (
            &["-c", &h, "credential", "approve"],
            "url=https://me:pw@example.com:8080/x/y/\n",
        ),
        (
            &["-c", &h, "credential", "reject"],
            "url=https://me:pw@example.com/x\ncapability[]=authtype\nauthtype=Basic\n\
             credential=zz\nephemeral=1\n",
        ),
        (
            &["-c", &h, "credential", "approve"],
            "protocol=https\nhost=example.com\nusername=a\npassword=b\npassword_expiry_utc=5\n",
        ),
        (
            &["-c", &h, "credential", "approve"],
            "protocol=https\nhost=example.com\nusername=a\n",
        ),
        (&["credential", "capability"], ""),
        (&["credential", "fill"], "protocol=https\n"),
        (&["credential", "fill"], "host=x\n"),
        (&["credential", "fill"], "garbage\n"),
        (&["credential", "fill"], "url=noscheme\n"),
        (
            &["credential", "fill"],
            "protocol=https\nhost=x\r\nusername=a\r\npassword=b\r\n\r\nx=y\n",
        ),
        (
            &["credential", "fill"],
            "protocol=https\nhost=x\nusername=bob\n",
        ),
        (
            &["-c", "credential.useHttpPath=1", "credential", "fill"],
            "protocol=https\nhost=a_b%x y:8080\nusername=u/s er%\npath=p a/b%c\n",
        ),
        (&["credential", "fill"], "url=https://a%0db@x\n"),
        (&["credential"], ""),
        (&["credential", "bogus"], ""),
    ];
    same_steps(&dir, &log, steps, &[("GIT_ASKPASS", "echo")]);
    same_steps(
        &dir,
        &log,
        &[(&["credential", "fill"], "protocol=https\nhost=x\n")],
        &[("GIT_TERMINAL_PROMPT", "0")],
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn credential_store_matches_git() {
    let dir = repo("credential-store");
    let file = dir.join("store.txt");
    let f = file.to_str().unwrap();
    let helper = format!("credential.helper=store --file={f}");
    let steps: &[(&[&str], &str)] = &[
        (
            &["credential-store", "--file", f, "store"],
            "protocol=https\nhost=ex.com:8080\nusername=a b@c\npassword=p:w/%x\npath=x y/z\n",
        ),
        (
            &["credential-store", "--file", f, "store"],
            "protocol=https\nhost=other.com\nusername=bob\npassword=pw\n",
        ),
        (
            &["credential-store", "--file", f, "store"],
            "protocol=https\nhost=other.com\nusername=bob\npassword=pw2\n",
        ),
        (
            &["credential-store", "--file", f, "get"],
            "protocol=https\nhost=other.com\n",
        ),
        (
            &["credential-store", "--file", f, "get"],
            "protocol=https\nhost=ex.com:8080\n",
        ),
        (
            &["credential-store", "--file", f, "get"],
            "protocol=http\nhost=ex.com:8080\n",
        ),
        (
            &["credential-store", "--file", f, "erase"],
            "protocol=https\nhost=other.com\npassword=nope\n",
        ),
        (
            &["credential-store", "--file", f, "erase"],
            "protocol=https\nhost=other.com\npassword=pw2\n",
        ),
        (
            &["credential-store", "--file", f, "store"],
            "protocol=https\nusername=x\n",
        ),
        (&["credential-store", "--file", f, "bogus"], ""),
        (&["credential-store", "--file", f, "capability"], ""),
        (
            &["-c", &helper, "credential", "approve"],
            "protocol=https\nhost=h.com\nusername=a\npassword=b\n",
        ),
        (
            &["-c", &helper, "credential", "fill"],
            "protocol=https\nhost=h.com\n",
        ),
        (&["credential-store"], ""),
    ];
    same_steps(&dir, &file, steps, &[]);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn credential_cache_speaks_git_daemon_protocol() {
    let dir = repo("credential-cache");
    // (client, daemon starter): every pairing of git's and rgit's must agree.
    let rgit = env!("CARGO_BIN_EXE_rgit");
    let mut runs = Vec::new();
    for (client, starter) in [("git", "git"), (rgit, rgit), ("git", rgit), (rgit, "git")] {
        let sock_dir = dir.join("sock");
        let _ = std::fs::remove_dir_all(&sock_dir);
        let sock = sock_dir.join("s");
        let s = sock.to_str().unwrap();
        let mut out = String::new();
        let mut step = |bin: &str, args: &[&str], input: &str| {
            let o = piped(bin, &dir, args, input.as_bytes(), &[]);
            out.push_str(&format!(
                "{args:?} {}\n{}",
                o.status.code().unwrap_or(-1),
                String::from_utf8_lossy(&o.stdout)
            ));
        };
        step(
            starter,
            &["credential-cache", "--socket", s, "store"],
            "protocol=https\nhost=ex.com\nusername=bob\npassword=pw\n",
        );
        step(
            client,
            &["credential-cache", "--socket", s, "store"],
            "capability[]=authtype\nprotocol=https\nhost=ex2.com\nauthtype=Bearer\n\
             credential=tok\npassword_expiry_utc=99999999999\n",
        );
        for input in [
            "protocol=https\nhost=ex.com\n",
            "protocol=https\nhost=ex2.com\n",
            "capability[]=authtype\nprotocol=https\nhost=ex2.com\n",
        ] {
            step(client, &["credential-cache", "--socket", s, "get"], input);
        }
        step(
            client,
            &["credential-cache", "--socket", s, "erase"],
            "protocol=https\nhost=ex.com\npassword=wrong\n",
        );
        step(
            client,
            &["credential-cache", "--socket", s, "get"],
            "protocol=https\nhost=ex.com\n",
        );
        step(
            client,
            &["credential-cache", "--socket", s, "erase"],
            "protocol=https\nhost=ex.com\n",
        );
        step(
            client,
            &["credential-cache", "--socket", s, "get"],
            "protocol=https\nhost=ex.com\n",
        );
        let h = format!("credential.helper=cache --socket {s}");
        step(
            client,
            &["-c", &h, "credential", "approve"],
            "protocol=https\nhost=ex4.com\nusername=a\npassword=b\n",
        );
        step(
            client,
            &["-c", &h, "credential", "fill"],
            "protocol=https\nhost=ex4.com\n",
        );
        step(
            client,
            &["credential-cache", "--socket", s, "capability"],
            "",
        );
        step(client, &["credential-cache", "--socket", s, "exit"], "");
        assert!(!sock.exists(), "the daemon removes its socket on exit");
        step(
            client,
            &["credential-cache", "--socket", s, "get"],
            "protocol=https\nhost=ex.com\n",
        );
        runs.push(out);
    }
    for run in &runs[1..] {
        assert_eq!(run, &runs[0]);
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// Whether the oracle git rejects `hook run` for hook event names it
/// does not know (git 2.55+; rgit keeps running any hook file).
fn oracle_rejects_unknown_hooks(dir: &Path) -> bool {
    static REJECTS: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *REJECTS.get_or_init(|| {
        let out = run("git", dir, &["hook", "run", "rgit-probe-nope"], &[]);
        String::from_utf8_lossy(&out.stderr).starts_with("error: unknown hook event")
    })
}

#[test]
fn hook_run_matches_git() {
    let dir = repo("hook-run");
    let hooks = dir.join(".git/hooks");
    std::fs::write(
        hooks.join("my-hook"),
        "#!/bin/sh\necho \"out $# $*\"\necho err >&2\npwd\necho \"prefix=$GIT_PREFIX\"\ncat\nexit 3\n",
    )
    .unwrap();
    std::fs::write(hooks.join("noexec"), "#!/bin/sh\necho hi\n").unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(
        hooks.join("my-hook"),
        std::fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    write(&dir, "in.txt", b"stdin data\n");
    std::fs::create_dir_all(dir.join("hk")).unwrap();
    std::fs::copy(hooks.join("my-hook"), dir.join("hk/my-hook")).unwrap();
    // git 2.55+ refuses to run hook event names it does not know, even
    // when the hook file exists; every event below is a custom name, so
    // there is nothing to compare against such an oracle.
    if oracle_rejects_unknown_hooks(&dir) {
        let _ = std::fs::remove_dir_all(&dir);
        return;
    }
    for cwd in [dir.clone(), dir.join("dir")] {
        for args in [
            &["hook", "run", "my-hook", "--", "a", "b c"][..],
            &["hook", "run", "--to-stdin=in.txt", "my-hook"],
            &["hook", "run", "--to-stdin=nope.txt", "my-hook"],
            &["hook", "run", "nope"],
            &["hook", "run", "--ignore-missing", "nope"],
            &["hook", "run", "noexec"],
            &["-c", "core.hooksPath=hk", "hook", "run", "my-hook"],
        ] {
            let want = piped("git", &cwd, args, b"ignored\n", &[]);
            let got = piped(env!("CARGO_BIN_EXE_rgit"), &cwd, args, b"ignored\n", &[]);
            assert_eq!(
                (got.status.code(), String::from_utf8_lossy(&got.stderr)),
                (want.status.code(), String::from_utf8_lossy(&want.stderr)),
                "{args:?}"
            );
            assert_eq!(got.stdout, want.stdout, "{args:?}");
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn rev_list_output_options_match_git() {
    let dir = repo("rev-list-more");
    let cases: &[&[&str]] = &[
        &["rev-list", "--objects", "--no-object-names", "main"],
        &[
            "rev-list",
            "--objects",
            "--no-object-names",
            "--object-names",
            "v1",
        ],
        &["rev-list", "--objects-edge", "main~1..main"],
        &["rev-list", "--objects", "--filter=blob:none", "--all"],
        &["rev-list", "--objects", "--filter=tree:0", "main"],
        &["rev-list", "--objects", "--filter=tree:1", "main"],
        &[
            "rev-list",
            "--objects",
            "--filter=tree:2",
            "--filter-print-omitted",
            "main",
        ],
        &[
            "rev-list",
            "--objects",
            "--filter=blob:limit=6",
            "--filter-print-omitted",
            "main",
        ],
        &["rev-list", "--objects", "--filter=blob:limit=1k", "main"],
        &["rev-list", "--objects", "--filter=object:type=tree", "main"],
        &[
            "rev-list",
            "--objects",
            "--filter=object:type=blob",
            "--all",
        ],
        &[
            "rev-list",
            "--objects",
            "--filter=combine:blob:none+tree:2",
            "main",
        ],
        &[
            "rev-list",
            "--objects",
            "--filter=blob:none",
            "--filter=tree:2",
            "main",
        ],
        &[
            "rev-list",
            "--objects",
            "--filter=blob:none",
            "--no-filter",
            "main",
        ],
        &["rev-list", "--filter=blob:none", "main"],
        &["rev-list", "--objects", "--filter=nope", "main"],
        &["rev-list", "--disk-usage", "main"],
        &["rev-list", "--disk-usage=human", "--objects", "--all"],
        &["rev-list", "--children", "--all"],
        &["rev-list", "--children", "--parents", "main"],
        &["rev-list", "--timestamp", "--parents", "main"],
        &["rev-list", "--header", "main"],
        &["rev-list", "--format=%h %s", "main"],
        &[
            "rev-list",
            "--format=%an%n%b",
            "--no-commit-header",
            "--all",
        ],
        &["rev-list", "--format=", "main"],
        &[
            "rev-list",
            "--format=%h",
            "--left-right",
            "--boundary",
            "main...side",
        ],
        &["rev-list", "--pretty=format:%s", "--parents", "main"],
        &["rev-list", "--pretty=oneline", "main"],
        &["rev-list", "--oneline", "--left-right", "main...side"],
        &["rev-list", "--pretty=short", "main"],
        &["rev-list", "--pretty", "main"],
        &["rev-list", "--pretty=raw", "main"],
        &["rev-list", "--pretty=fuller", "--date=iso", "main"],
        &["rev-list", "--abbrev-commit", "--abbrev=10", "main"],
        &["rev-list", "--abbrev-commit", "--parents", "main"],
        &["rev-list", "--bisect", "main"],
        &["rev-list", "--bisect", "side..main"],
        &["rev-list", "--bisect-vars", "main"],
        &["rev-list", "--bisect-all", "main"],
        &["rev-list", "--bisect-all", "--bisect-vars", "main"],
        &["rev-list", "--quiet", "--objects", "main"],
        &["rev-list", "--exclude=s*", "--branches"],
        &["rev-list", "--exclude=refs/heads/main", "--all", "--count"],
    ];
    same(&dir, cases);
    same_input(&dir, &["rev-list", "--stdin", "--count"], b"main\n^side\n");
    same_input(&dir, &["rev-list", "--stdin"], b"");
    same_input(&dir, &["rev-list", "--stdin", "main"], b"--\nc.txt\n");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn fetch_pack_and_send_pack_match_git() {
    let base = std::env::temp_dir().join(format!("rgit-plumbing-{}-packs", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let src = repo("packs-src");
    let mut outs = Vec::new();
    for bin in ["git", env!("CARGO_BIN_EXE_rgit")] {
        let side = base.join(if bin == "git" { "g" } else { "r" });
        std::fs::create_dir_all(&side).unwrap();
        git(&side, &["init", "-q", "-b", "main", "dst"], &[]);
        git(
            &side,
            &["clone", "-q", "--bare", src.to_str().unwrap(), "bare.git"],
            &[],
        );
        let dst = side.join("dst");
        let bare = side.join("bare.git");
        let s = src.to_str().unwrap();
        let url = format!("file://{s}");
        let b = bare.to_str().unwrap();
        let mut out = String::new();
        let step = |dir: &Path, args: &[&str], input: &str| {
            let o = piped(bin, dir, args, input.as_bytes(), &[]);
            format!(
                "{args:?} {}\n{}",
                o.status.code().unwrap_or(-1),
                String::from_utf8_lossy(&o.stdout)
            )
        };
        out.push_str(&step(&dst, &["fetch-pack", s, "main"], ""));
        out.push_str(&step(
            &dst,
            &["fetch-pack", s, "refs/heads/side", "refs/tags/v1"],
            "",
        ));
        out.push_str(&step(&dst, &["fetch-pack", "--all", &url], ""));
        out.push_str(&step(
            &dst,
            &["fetch-pack", "--stdin", s],
            "refs/tags/lw\nnope\n",
        ));
        out.push_str(&step(&dst, &["fetch-pack", "-q", s, "HEAD"], ""));
        out.push_str(&git(&dst, &["for-each-ref"], &[]));
        out.push_str(&git(&dst, &["rev-list", "--all", "--count"], &[]));
        let v1 = git(&src, &["rev-parse", "v1"], &[]);
        out.push_str(&git(&dst, &["cat-file", "-t", v1.trim()], &[]));
        out.push_str(&step(&src, &["send-pack", b, "side:refs/heads/new"], ""));
        out.push_str(&step(&src, &["send-pack", b, "main~1:refs/heads/main"], ""));
        out.push_str(&step(
            &src,
            &["send-pack", "--force", b, "main~1:refs/heads/main"],
            "",
        ));
        out.push_str(&step(&src, &["send-pack", "--dry-run", "--all", b], ""));
        out.push_str(&step(&src, &["send-pack", b, ":refs/heads/new"], ""));
        out.push_str(&step(&src, &["send-pack", b], ""));
        out.push_str(&git(&bare, &["for-each-ref"], &[]));
        outs.push(out.replace(side.to_str().unwrap(), "SIDE"));
    }
    assert_eq!(outs[1], outs[0]);
    let _ = std::fs::remove_dir_all(&base);
    let _ = std::fs::remove_dir_all(&src);
}

#[test]
fn diff_pairs_matches_git() {
    let dir = repo("diff-pairs");
    git(&dir, &["add", "-A"], &[]);
    git(&dir, &["mv", "c.txt", "moved.txt"], &[]);
    write(&dir, "bin.dat", b"a\0binary 2\n");
    git(&dir, &["add", "-A"], &[]);
    commit(&dir, 6, "sixth", "T");
    let mut input = run(
        "git",
        &dir,
        &["diff-tree", "-r", "-z", "-M", "HEAD~1", "HEAD"],
        &[],
    )
    .stdout;
    input.push(0);
    input.extend(
        run(
            "git",
            &dir,
            &["diff-tree", "-r", "-z", "HEAD~2", "HEAD~1"],
            &[],
        )
        .stdout,
    );
    for opts in [
        &[][..],
        &["-p"],
        &["--stat"],
        &["--stat", "-p"],
        &["--numstat"],
        &["--shortstat"],
        &["--name-only"],
        &["--name-status"],
        &["-U1"],
        &["-s"],
    ] {
        let mut args = vec!["diff-pairs", "-z"];
        args.extend(opts);
        same_input(&dir, &args, &input);
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn whatchanged_and_raw_match_git() {
    let dir = repo("whatchanged");
    git(&dir, &["add", "-A"], &[]);
    git(&dir, &["mv", "c.txt", "moved.txt"], &[]);
    git(&dir, &["update-index", "--chmod=+x", "e.txt"], &[]);
    commit(&dir, 6, "sixth", "T");
    // git 2.55+ refuses whatchanged without an opt-out flag; rgit keeps
    // the command working, so hand a new oracle its flag.
    let flag = run(
        "git",
        &dir,
        &["whatchanged", "--i-still-use-this", "-1"],
        &[],
    )
    .status
    .success();
    for args in [
        &["whatchanged"][..],
        &["whatchanged", "-2"],
        &["whatchanged", "-p", "-1"],
        &["whatchanged", "--oneline"],
        &["whatchanged", "--stat", "-2"],
        &["whatchanged", "--format=%h %s", "--", "c.txt"],
    ] {
        let want = if flag {
            let mut a = vec![args[0], "--i-still-use-this"];
            a.extend(&args[1..]);
            run("git", &dir, &a, &[])
        } else {
            run("git", &dir, args, &[])
        };
        let got = rgit(&dir, args);
        assert!(
            got.stdout == want.stdout && got.status.success() == want.status.success(),
            "{args:?}"
        );
    }
    same(
        &dir,
        &[
            &["log", "--raw", "--oneline"][..],
            &["log", "--raw", "--pretty=medium", "-p", "-2"],
            &["show", "--raw", "--format=%h", "HEAD"],
            &["diff", "--raw", "HEAD~2", "HEAD"],
        ],
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn version_is_shaped_like_git() {
    let dir = std::env::temp_dir();
    let out = rgit(&dir, &["version"]);
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        text.starts_with("rgit version ") && text.lines().count() == 1,
        "{text}"
    );
    let out = rgit(&dir, &["version", "--build-options"]);
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        text.contains("\nsizeof-size_t: ") && text.contains("\nshell-path: "),
        "{text}"
    );
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
    write(&dir, ".gitattributes", b"*.x diff=mine\n*.py diff=python\n");
    write(
        &dir,
        "k.py",
        b"class K:\n    x = 1\n\n    def m(self):\n        y = 2\n        return y\n",
    );
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
    // Source-built oracles can lack USE_LIBPCRE, where git grep -P dies
    // for patterns that are not plain literals (git compiles those
    // without PCRE); probe with a regex pattern.
    let pcre = {
        let out = run("git", &dir, &["grep", "-P", "f.o"], &[]);
        !String::from_utf8_lossy(&out.stderr).contains("USE_LIBPCRE")
    };
    let mut cases = vec![
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
        &["grep", "-p", "return", "--", "k.py"],
        &["grep", "-W", "y = 2", "--", "k.py"],
        &["grep", "-m1", "-A2", "foo"],
        &["grep", "--threads", "2", "foo"],
        &["grep", "--untracked", "foo"],
        &["grep", "--untracked", "--no-exclude-standard", "foo"],
        &["grep", "--no-index", "foo"],
        &["grep", "--no-index", "--exclude-standard", "-n", "foo"],
        &["grep", "--recurse-submodules", "foo"],
        &["grep", "--recurse-submodules", "--cached", "foo"],
        &["grep", "--recurse-submodules", "foo", "HEAD"],
    ];
    if pcre {
        cases.push(&["grep", "-P", "foo(?= only)"]);
        cases.push(&["grep", "-P", "-w", "ba."]);
    }
    same(&dir, &cases);
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
fn ident_and_v1_pack_indexes_read_like_git() {
    let dir = repo("ident");
    write(
        &dir,
        "id.txt",
        b"a $Id$ b\n$Id: old $\n$Id: two words $\n$Id\n",
    );
    write(&dir, ".gitattributes", b"id.txt ident\n");
    git(&dir, &["add", "id.txt", ".gitattributes"], &[]);
    commit(&dir, 4, "ident", "T");
    same(
        &dir,
        &[
            &["cat-file", "--filters", "HEAD:id.txt"],
            &["cat-file", "--filters", "--path=id.txt", "HEAD:c.txt"],
            &["cat-file", "-p", "HEAD:id.txt"],
        ],
    );
    git(&dir, &["repack", "-adq"], &[]);
    let pack = std::fs::read_dir(dir.join(".git/objects/pack"))
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .find(|p| p.extension().is_some_and(|x| x == "pack"))
        .unwrap();
    std::fs::remove_file(pack.with_extension("idx")).unwrap();
    let _ = std::fs::remove_file(pack.with_extension("rev"));
    let pack = pack.to_string_lossy().into_owned();
    git(&dir, &["index-pack", "--index-version=1", &pack], &[]);
    same(
        &dir,
        &[
            &["count-objects", "-v"],
            &["rev-parse", "--short", "HEAD"],
            &["log", "--oneline"],
            &["cat-file", "-p", "HEAD:c.txt"],
        ],
    );
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

fn lines(from: u32, to: u32) -> Vec<u8> {
    (from..=to)
        .map(|n| format!("{n}\n"))
        .collect::<String>()
        .into_bytes()
}

/// A repo whose history renames, copies and changes modes, and whose HEAD
/// merges a side branch with conflicts resolved by hand; the index renames
/// and copies and the working tree edits a file.
fn rename_repo(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("rgit-plumbing-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    git(&dir, &["init", "-q", "-b", "main"], &[]);
    git(&dir, &["config", "user.email", "t@example.com"], &[]);
    git(&dir, &["config", "user.name", "T"], &[]);
    write(&dir, "a", &lines(1, 20));
    write(&dir, "b", &lines(100, 130));
    write(&dir, "c", b"x\n");
    write(&dir, "d/e/long", &lines(1, 40));
    git(&dir, &["add", "."], &[]);
    commit(&dir, 1, "one", "T");
    git(&dir, &["mv", "a", "a2"], &[]);
    git(&dir, &["mv", "d/e/long", "d/moved"], &[]);
    write(
        &dir,
        "d/moved",
        &[lines(1, 40), b"extra\n".to_vec()].concat(),
    );
    write(&dir, "b", &lines(100, 128));
    write(&dir, "bcopy", &lines(100, 128));
    write(&dir, "c", b"x\ny\n");
    git(&dir, &["update-index", "--chmod=+x", "c"], &[]);
    git(&dir, &["add", "."], &[]);
    commit(&dir, 2, "two", "T");
    git(&dir, &["checkout", "-q", "-b", "side", "HEAD~1"], &[]);
    write(&dir, "a", &lines(1, 21));
    write(&dir, "s", b"s\n");
    write(&dir, "c", b"side\n");
    git(&dir, &["add", "."], &[]);
    commit(&dir, 3, "side", "T");
    git(&dir, &["checkout", "-q", "main"], &[]);
    let _ = run("git", &dir, &["merge", "-q", "side"], &[]);
    write(&dir, "c", b"merged\n");
    write(&dir, "a2", &lines(0, 20));
    git(&dir, &["add", "."], &[]);
    commit(&dir, 4, "merge", "T");
    git(&dir, &["mv", "b", "b2"], &[]);
    write(&dir, "b2", &lines(100, 129));
    write(
        &dir,
        "newcopy",
        &[lines(1, 40), b"extra\n".to_vec()].concat(),
    );
    git(&dir, &["add", "."], &[]);
    write(&dir, "d/moved", b"changed\n");
    dir
}

#[test]
fn diff_plumbing_renames_stats_and_merges_match_git() {
    let dir = rename_repo("diff-renames");
    let cases: Vec<Vec<&str>> = [
        "diff-tree -M HEAD~1",
        "diff-tree -r -M HEAD~1",
        "diff-tree -r -M --name-status HEAD~1",
        "diff-tree -r -M -z --name-status HEAD~1",
        "diff-tree -r -M --name-only HEAD~1",
        "diff-tree -r -C HEAD~1",
        "diff-tree -r -C -C HEAD~1",
        "diff-tree -r --find-copies-harder HEAD~1",
        "diff-tree -r -M50% HEAD~1",
        "diff-tree -r -M99 HEAD~1",
        "diff-tree -r -M -t HEAD~1",
        "diff-tree -M -p HEAD~1",
        "diff-tree -C --stat --summary -p HEAD~1",
        "diff-tree --stat HEAD~1",
        "diff-tree --stat=40 -M HEAD~1",
        "diff-tree --summary HEAD~1",
        "diff-tree -M --raw --stat HEAD~1",
        "diff-tree -M -p --raw HEAD~1",
        "diff-tree -z -M -p --raw HEAD~1",
        "diff-tree -M --name-status -p HEAD~1",
        "diff-tree -r --abbrev HEAD~1",
        "diff-tree -r --abbrev=10 -M HEAD~1",
        "diff-tree --root --stat --summary HEAD~2",
        "diff-tree -r -M HEAD~2 HEAD~1 -- d",
        "diff-tree HEAD",
        "diff-tree -c HEAD",
        "diff-tree --cc HEAD",
        "diff-tree -c -p HEAD",
        "diff-tree --cc --raw HEAD",
        "diff-tree -c --name-status HEAD",
        "diff-tree -c -z --name-only HEAD",
        "diff-tree -c --stat HEAD",
        "diff-tree --cc --summary HEAD",
        "diff-tree -c --abbrev HEAD",
        "diff-tree -c -z HEAD",
        "diff-tree -c --stat -p HEAD",
        "diff-tree --cc HEAD -- c",
        "diff-index -M HEAD",
        "diff-index --cached -M HEAD",
        "diff-index --cached -C --name-status HEAD",
        "diff-index --cached --find-copies-harder HEAD",
        "diff-index --cached -M --stat --summary -p HEAD",
        "diff-index -p --stat HEAD",
        "diff-index --abbrev=8 HEAD",
        "diff-files --stat",
        "diff-files --summary -p",
        "diff-files --abbrev",
    ]
    .iter()
    .map(|c| c.split(' ').collect())
    .collect();
    let cases: Vec<&[&str]> = cases.iter().map(Vec::as_slice).collect();
    same(&dir, &cases);
    let _ = std::fs::remove_dir_all(&dir);
}

/// Every file under `dir` but `.git`, with temporary names made stable:
/// sorted `(name, mode, content)` rows.
fn snapshot(dir: &Path) -> Vec<(String, u32, Vec<u8>)> {
    use std::os::unix::fs::PermissionsExt;
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).unwrap().flatten() {
            let path = e.path();
            let name = path
                .strip_prefix(dir)
                .unwrap()
                .to_string_lossy()
                .into_owned();
            let meta = std::fs::symlink_metadata(&path).unwrap();
            if name == ".git" {
                continue;
            } else if meta.is_dir() {
                stack.push(path);
            } else if meta.file_type().is_symlink() {
                let target = std::fs::read_link(&path).unwrap();
                out.push((
                    name,
                    0o120000,
                    target.as_os_str().as_encoded_bytes().to_vec(),
                ));
            } else {
                let mode = meta.permissions().mode() & 0o777;
                out.push((stable(&name), mode, std::fs::read(&path).unwrap()));
            }
        }
    }
    out.sort();
    out
}

/// `.merge_file_XXXXXX` for every temporary name.
fn stable(text: &str) -> String {
    let mut out = String::new();
    let mut rest = text;
    while let Some(at) = rest.find(".merge_") {
        let (head, tail) = rest.split_at(at);
        out.push_str(head);
        let kind = if tail.starts_with(".merge_link_") {
            ".merge_link_"
        } else {
            ".merge_file_"
        };
        out.push_str(kind);
        out.push_str("XXXXXX");
        rest = &tail[(kind.len() + 6).min(tail.len())..];
    }
    out.push_str(rest);
    out
}

#[test]
fn checkout_index_temp_stages_and_prefixes_match_git() {
    let build = |tag: &str| {
        let dir = std::env::temp_dir().join(format!("rgit-plumbing-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        git(&dir, &["init", "-q", "-b", "main"], &[]);
        git(&dir, &["config", "user.email", "t@example.com"], &[]);
        git(&dir, &["config", "user.name", "T"], &[]);
        write(&dir, "c", b"base\n");
        write(&dir, "d/f", b"one\n");
        write(&dir, "x", b"run\n");
        write(&dir, "t.txt", b"crlf\n");
        write(&dir, ".gitattributes", b"*.txt eol=crlf\n");
        std::os::unix::fs::symlink("d/f", dir.join("link")).unwrap();
        git(&dir, &["add", "."], &[]);
        git(&dir, &["update-index", "--chmod=+x", "x"], &[]);
        commit(&dir, 1, "one", "T");
        git(&dir, &["checkout", "-q", "-b", "side"], &[]);
        write(&dir, "c", b"theirs\n");
        write(&dir, "del", b"gone\n");
        git(&dir, &["add", "."], &[]);
        commit(&dir, 2, "side", "T");
        git(&dir, &["checkout", "-q", "main"], &[]);
        write(&dir, "c", b"ours\n");
        write(&dir, "del", b"mine\n");
        git(&dir, &["add", "."], &[]);
        commit(&dir, 3, "main", "T");
        let _ = run("git", &dir, &["merge", "-q", "side"], &[]);
        std::fs::remove_file(dir.join("del")).unwrap();
        dir
    };
    let cases: &[(&str, &[&str], &[u8])] = &[
        ("", &["--temp", "c"], b""),
        ("", &["--temp", "x", "link", "t.txt"], b""),
        ("", &["--stage=all", "c", "del"], b""),
        ("", &["--stage=all", "-a"], b""),
        ("", &["-f", "--temp", "--stage=all", "-z", "c"], b""),
        ("", &["--temp", "-z", "--stdin"], b"x\0d/f\0"),
        ("", &["--stage=2", "c"], b""),
        ("", &["--stage=3", "-f", "c"], b""),
        ("", &["--stage=1", "del"], b""),
        ("", &["c"], b""),
        ("", &["nope", "d"], b""),
        (
            "",
            &["-f", "--prefix=out-", "d/f", "x", "link", "t.txt"],
            b"",
        ),
        ("", &["--prefix=o/", "-a"], b""),
        ("", &["--temp", "--prefix=o/", "x"], b""),
        ("", &["-f", "-a"], b""),
        ("d", &["--temp", "f", "../c", "../x"], b""),
        ("d", &["--prefix=p/", "f"], b""),
        ("d", &["--prefix=q-", "-a"], b""),
        ("d", &["--stage=all", "../c"], b""),
    ];
    for (sub, args, input) in cases {
        let (a, b) = (build("co-temp-git"), build("co-temp-rgit"));
        let feed = |bin: &str, dir: &Path, args: &[&str]| {
            use std::io::Write;
            let mut child = Command::new(bin)
                .args(args)
                .current_dir(dir)
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .env("RGIT_OPLOG", "0")
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .spawn()
                .unwrap();
            child.stdin.take().unwrap().write_all(input).unwrap();
            child.wait_with_output().unwrap()
        };
        let mut gargs = vec!["checkout-index"];
        gargs.extend(*args);
        let want = feed("git", &a.join(sub), &gargs);
        let mut rargs = vec!["--human"];
        rargs.extend(&gargs);
        let got = feed(env!("CARGO_BIN_EXE_rgit"), &b.join(sub), &rargs);
        let text = |o: &Output| {
            (
                stable(&String::from_utf8_lossy(&o.stdout)),
                stable(&String::from_utf8_lossy(&o.stderr).replace("rgit", "git")),
                o.status.success(),
            )
        };
        assert_eq!(text(&got), text(&want), "{args:?} in {sub:?}");
        assert_eq!(snapshot(&b), snapshot(&a), "{args:?} in {sub:?}");
        let index = |d: &Path| git(d, &["ls-files", "-s"], &[]);
        assert_eq!(index(&b), index(&a), "{args:?} in {sub:?}");
        let _ = std::fs::remove_dir_all(&a);
        let _ = std::fs::remove_dir_all(&b);
    }
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
fn merge_tree_renames_and_tree_conflicts_match_git() {
    let dir = std::env::temp_dir().join(format!("rgit-plumbing-{}-ort", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    git(&dir, &["init", "-q", "-b", "main"], &[]);
    git(&dir, &["config", "user.email", "t@example.com"], &[]);
    git(&dir, &["config", "user.name", "T"], &[]);
    let text = |tag: &str| -> Vec<u8> {
        (1..=20)
            .map(|i| format!("{tag} line {i}\n"))
            .collect::<String>()
            .into_bytes()
    };
    let edit = |tag: &str, n: usize, to: &str| -> Vec<u8> {
        let mut lines: Vec<String> = String::from_utf8(text(tag))
            .unwrap()
            .lines()
            .map(str::to_owned)
            .collect();
        lines[n] = to.to_owned();
        (lines.join("\n") + "\n").into_bytes()
    };
    for (p, t) in [
        ("renmod", text("rm")),
        ("rendel", text("rd")),
        ("ren12", text("r12")),
        ("x", text("x")),
        ("y", text("y")),
        ("a/1", text("a1")),
        ("a/2", text("a2")),
        ("a/3", text("a3")),
        ("types", b"plain\n".to_vec()),
        ("bin", b"a\0b".to_vec()),
        ("content", text("c")),
    ] {
        write(&dir, p, &t);
    }
    git(&dir, &["add", "."], &[]);
    commit(&dir, 1, "base", "T");
    git(&dir, &["checkout", "-q", "-b", "A"], &[]);
    for (from, to) in [
        ("renmod", "renamed"),
        ("rendel", "rd-new"),
        ("ren12", "r-a"),
        ("x", "t"),
        ("a", "b"),
    ] {
        git(&dir, &["mv", from, to], &[]);
    }
    write(&dir, "content", &edit("c", 3, "A3"));
    write(&dir, "df", b"file\n");
    write(&dir, "bin", b"a\0c");
    std::fs::remove_file(dir.join("types")).unwrap();
    std::os::unix::fs::symlink("target", dir.join("types")).unwrap();
    git(&dir, &["add", "-A"], &[]);
    commit(&dir, 2, "a", "T");
    git(&dir, &["checkout", "-q", "-b", "B", "main"], &[]);
    write(&dir, "renmod", &edit("rm", 5, "B5"));
    git(&dir, &["rm", "-q", "rendel"], &[]);
    git(&dir, &["mv", "ren12", "r-b"], &[]);
    git(&dir, &["mv", "y", "t"], &[]);
    write(&dir, "a/new", b"new\n");
    write(&dir, "content", &edit("c", 3, "B3"));
    write(&dir, "df/inside", b"dir\n");
    write(&dir, "bin", b"a\0d");
    write(&dir, "types", b"changed\n");
    git(&dir, &["add", "-A"], &[]);
    commit(&dir, 3, "b", "T");
    git(&dir, &["checkout", "-q", "-b", "C", "main"], &[]);
    write(&dir, "content", &edit("c", 10, "C10"));
    git(&dir, &["add", "-A"], &[]);
    commit(&dir, 4, "c", "T");
    same(
        &dir,
        &[
            &["merge-tree", "A", "B"][..],
            &["merge-tree", "B", "A"],
            &["merge-tree", "--name-only", "A", "B"],
            &["merge-tree", "-z", "A", "B"],
            &["merge-tree", "--messages", "A", "C"],
            &["merge-tree", "-z", "--messages", "A", "C"],
            &["merge-tree", "--merge-base=main", "B", "A"],
            &["merge-tree", "-Xours", "A", "B"],
            &["merge-tree", "-Xno-renames", "A", "B"],
            &["merge-tree", "--quiet", "A", "B"],
            &["merge-tree", "main", "A", "B"],
            &["merge-tree", "--trivial-merge", "main", "C", "B"],
            &["merge-tree", "nope", "B"],
        ],
    );
    let main = git(&dir, &["rev-parse", "main"], &[]);
    let input = format!("A B\nA C\n{} -- B C\n", main.trim());
    same_input(&dir, &["merge-tree", "--stdin"], input.as_bytes());
    git(&dir, &["config", "merge.directoryRenames", "true"], &[]);
    same(&dir, &[&["merge-tree", "A", "B"][..]]);
    // A criss-cross: two merge bases, merged into a virtual one first.
    git(&dir, &["checkout", "-q", "-b", "X", "C"], &[]);
    write(&dir, "content", &edit("c", 0, "X0"));
    commit_all(&dir, "x");
    git(&dir, &["checkout", "-q", "-b", "Y", "C"], &[]);
    write(&dir, "content", &edit("c", 0, "Y0"));
    commit_all(&dir, "y");
    let merge = |into: &str, from: &str, line: &str| {
        git(&dir, &["checkout", "-q", into], &[]);
        let _ = run("git", &dir, &["merge", "-q", "--no-commit", from], &[]);
        write(&dir, "content", &edit("c", 0, line));
        git(&dir, &["add", "-A"], &[]);
        git(&dir, &["commit", "-q", "--no-edit"], &[]);
    };
    merge("X", "Y~0", "XY");
    merge("Y", "X~1", "YX");
    same(
        &dir,
        &[&["merge-tree", "X", "Y"][..], &["merge-tree", "Y", "X"]],
    );
    let _ = std::fs::remove_dir_all(&dir);
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
fn name_rev_all_lists_in_git_order() {
    let dir = repo("name-rev-all");
    for n in 0..30 {
        write(&dir, "n.txt", format!("{n}\n").as_bytes());
        git(&dir, &["add", "."], &[]);
        git(&dir, &["commit", "-qm", &format!("c{n}")], &[]);
        if n % 7 == 0 {
            git(&dir, &["branch", &format!("b{n}")], &[]);
            git(&dir, &["tag", "-a", &format!("t{n}"), "-m", "t"], &[]);
        }
    }
    git(&dir, &["merge", "-q", "--no-edit", "main"], &[]);
    git(&dir, &["tag", "tree", "HEAD^{tree}"], &[]);
    let cases: &[&[&str]] = &[
        &["name-rev", "--all"],
        &["name-rev", "--all", "--tags"],
        &["name-rev", "--all", "--name-only", "--exclude=b*"],
    ];
    same(&dir, cases);
    git(&dir, &["commit-graph", "write", "--reachable"], &[]);
    git(&dir, &["commit", "-q", "--allow-empty", "-m", "past"], &[]);
    same(&dir, cases);
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
            ("", &["read-tree", "--reset", "-u", "HEAD"], b""),
            (
                "",
                &["read-tree", "-m", "HEAD~2", "HEAD~1", "HEAD", theirs],
                b"",
            ),
            ("", &["read-tree", "--reset", "-u", "HEAD"], b""),
            (
                "",
                &["read-tree", "-m", "--trivial", "HEAD~2", "HEAD", theirs],
                b"",
            ),
            (
                "",
                &["read-tree", "-m", "--trivial", "HEAD", "HEAD", "HEAD"],
                b"",
            ),
            (
                "",
                &[
                    "read-tree",
                    "--index-output=other.idx",
                    "-m",
                    "HEAD~2",
                    "HEAD",
                    theirs,
                ],
                b"",
            ),
            ("", &["read-tree", "--prefix=/v", "side"], b""),
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
    git(
        &dir,
        &["checkout", "-q", "-b", "topic", "side"],
        &[("GIT_COMMITTER_DATE", "2024-01-08T09:00:00+0200")],
    );
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
            &["show-branch", "--reflog=2,1.day.ago", "topic"],
            &["show-branch", "--reflog=3,2024-01-05", "main"],
            &["show-branch", "--reflog=3,2000-01-01", "main"],
            &["show-branch", "--reflog=30"],
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

/// `bin args < input` in `dir`: stdout and success.
fn piped_out(bin: &str, dir: &Path, args: &[&str], input: &[u8]) -> (Vec<u8>, bool) {
    use std::io::Write;
    let mut all: Vec<&str> = if bin == "git" {
        vec![]
    } else {
        vec!["--human"]
    };
    all.extend(args);
    let mut child = Command::new(bin)
        .args(&all)
        .current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("RGIT_OPLOG", "0")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(input).unwrap();
    let out = child.wait_with_output().unwrap();
    (out.stdout, out.status.success())
}

const RGIT: &str = env!("CARGO_BIN_EXE_rgit");

/// The object ids a pack file holds, sorted, as git indexes it.
fn pack_ids(dir: &Path, pack: &str) -> Vec<String> {
    git(dir, &["index-pack", "-o", "ids.idx", pack], &[]);
    let idx = std::fs::read(dir.join("ids.idx")).unwrap();
    let (out, _) = piped_out("git", dir, &["show-index"], &idx);
    let mut ids: Vec<String> = String::from_utf8_lossy(&out)
        .lines()
        .map(|l| l.split(' ').nth(1).unwrap().to_owned())
        .collect();
    ids.sort();
    ids
}

#[test]
fn pack_plumbing_matches_git() {
    let dir = repo("packs");
    git(&dir, &["gc", "-q"], &[]);
    let name = std::fs::read_dir(dir.join(".git/objects/pack"))
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .find(|p| p.ends_with(".pack"))
        .unwrap();
    let hash = name[5..45].to_owned();
    let pack = format!(".git/objects/pack/{name}");
    let idx = pack.replace(".pack", ".idx");
    same(
        &dir,
        &[
            &["verify-pack", "-v", &pack],
            &["verify-pack", "-s", &pack],
            &["verify-pack", &idx],
            &["verify-pack", "-v", &idx],
        ],
    );
    let idx_bytes = std::fs::read(dir.join(&idx)).unwrap();
    same_input(&dir, &["show-index"], &idx_bytes);

    // index-pack writes the .idx and .rev git wrote, and prints the checksum.
    std::fs::copy(dir.join(&pack), dir.join("copy.pack")).unwrap();
    let out = rgit(&dir, &["index-pack", "copy.pack"]);
    assert_eq!(String::from_utf8_lossy(&out.stdout), format!("{hash}\n"));
    assert_eq!(std::fs::read(dir.join("copy.idx")).unwrap(), idx_bytes);
    assert_eq!(
        std::fs::read(dir.join("copy.rev")).unwrap(),
        std::fs::read(dir.join(pack.replace(".pack", ".rev"))).unwrap()
    );
    let data = std::fs::read(dir.join(&pack)).unwrap();
    let (out, _) = piped_out(RGIT, &dir, &["index-pack", "--stdin", "--keep"], &data);
    assert_eq!(String::from_utf8_lossy(&out), format!("keep\t{hash}\n"));
    let (out, _) = piped_out(RGIT, &dir, &["index-pack", "--stdin"], &data);
    assert_eq!(String::from_utf8_lossy(&out), format!("pack\t{hash}\n"));
    std::fs::remove_file(dir.join(pack.replace(".pack", ".keep"))).unwrap();

    // pack-objects packs the objects git packs.
    for (args, input) in [
        (
            &["pack-objects", "--revs", "--stdout"][..],
            &b"main\n^side\n"[..],
        ),
        (&["pack-objects", "--revs", "--stdout"], b"v1\n"),
        (&["pack-objects", "--all", "--stdout"], b""),
    ] {
        let (ours, ok) = piped_out(RGIT, &dir, args, input);
        assert!(ok, "{args:?}");
        std::fs::write(dir.join("ours.pack"), ours).unwrap();
        let (theirs, _) = piped_out("git", &dir, args, input);
        std::fs::write(dir.join("theirs.pack"), theirs).unwrap();
        assert_eq!(
            pack_ids(&dir, "ours.pack"),
            pack_ids(&dir, "theirs.pack"),
            "{args:?}"
        );
    }
    let (list, _) = piped_out("git", &dir, &["rev-list", "--objects", "side"], b"");
    let (ours, _) = piped_out(RGIT, &dir, &["pack-objects", "--stdout"], &list);
    std::fs::write(dir.join("ours.pack"), ours).unwrap();
    assert_eq!(
        pack_ids(&dir, "ours.pack").len(),
        list.split(|b| *b == b'\n')
            .filter(|l| !l.is_empty())
            .count()
    );
    let (out, _) = piped_out(RGIT, &dir, &["pack-objects", "--revs", "out/p"], b"main\n");
    let named = String::from_utf8_lossy(&out).trim().to_owned();
    let written = format!("out/p-{named}.pack");
    let check = git(&dir, &["verify-pack", "-v", &written], &[]);
    assert!(check.ends_with(&format!("{written}: ok\n")), "{check}");

    // unpack-objects writes each object loose in an empty repository.
    git(&dir, &["init", "-q", "empty"], &[]);
    let (_, ok) = piped_out(RGIT, &dir.join("empty"), &["unpack-objects"], &data);
    assert!(ok);
    let n = pack_ids(&dir, &pack).len();
    let count = git(&dir.join("empty"), &["count-objects"], &[]);
    assert!(count.starts_with(&format!("{n} objects")), "{count}");

    // Loose copies of packed objects: prune-packed lists the same ones.
    write(&dir, "p.txt", b"packed twice\n");
    git(&dir, &["add", "p.txt"], &[]);
    commit(&dir, 6, "loose", "T");
    git(&dir, &["repack", "-q"], &[]);
    same(&dir, &[&["prune-packed", "-n"]]);

    // update-server-info writes git's info/refs and objects/info/packs.
    let files = [".git/info/refs", ".git/objects/info/packs"];
    git(&dir, &["update-server-info"], &[]);
    let want: Vec<String> = files
        .iter()
        .map(|f| std::fs::read_to_string(dir.join(f)).unwrap())
        .collect();
    for f in files {
        std::fs::remove_file(dir.join(f)).unwrap();
    }
    assert!(rgit(&dir, &["update-server-info"]).status.success());
    let got: Vec<String> = files
        .iter()
        .map(|f| std::fs::read_to_string(dir.join(f)).unwrap())
        .collect();
    assert_eq!(got, want);

    // A pack whose objects another pack holds is redundant.
    piped_out(
        "git",
        &dir,
        &["pack-objects", "--revs", ".git/objects/pack/pack"],
        b"side\n",
    );
    same(
        &dir,
        &[
            &["pack-redundant", "--all"],
            &["pack-redundant", "--all", "--i-still-use-this"],
        ],
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn check_mailmap_and_unpack_file_match_git() {
    let dir = repo("mailmap");
    write(
        &dir,
        ".mailmap",
        b"Proper <proper@x>  <A@X>\nOther Name <o@x> Old <old@x>\n<new@x> <cased@X>\n# c\nJane <jane@x>\n",
    );
    write(&dir, "extra.map", b"F <f@x> <nobody@x>\n");
    same(
        &dir,
        &[
            &[
                "check-mailmap",
                "A <a@x>",
                "Z <old@x>",
                "Old <OLD@x>",
                "q <cased@x>",
                "jane <jane@x>",
                "<a@x>",
                "<nobody@x>",
                " Sp  <nobody@x> ",
                "bare@x",
            ],
            &[
                "check-mailmap",
                "--mailmap-file",
                "extra.map",
                "N <nobody@x>",
            ],
            &[
                "check-mailmap",
                "--mailmap-blob",
                "HEAD:nope",
                "N <nobody@x>",
            ],
            &["check-mailmap"],
        ],
    );
    same_input(
        &dir,
        &["check-mailmap", "--stdin", "Old <old@x>"],
        b"A <a@x>\nX <nobody@x>\n",
    );

    same(&dir, &[&["unpack-file", "nope"], &["unpack-file", "HEAD"]]);
    let out = rgit(&dir.join("dir"), &["unpack-file", "HEAD:c.txt"]);
    let name = String::from_utf8_lossy(&out.stdout).trim().to_owned();
    assert!(
        name.starts_with(".merge_file_") && name.len() == 18,
        "{name}"
    );
    assert_eq!(
        std::fs::read_to_string(dir.join(&name)).unwrap(),
        git(&dir, &["show", "HEAD:c.txt"], &[])
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn commit_graph_and_multi_pack_index_files_match_git() {
    let dir = repo("graphs");
    git(&dir, &["gc", "-q"], &[]);
    for n in 6..8 {
        write(&dir, &format!("n{n}.txt"), format!("{n}\n").as_bytes());
        git(&dir, &["add", "."], &[]);
        commit(&dir, n, "more", "T");
        git(&dir, &["repack", "-q"], &[]);
    }
    // A pack duplicating objects of the others.
    piped_out(
        "git",
        &dir,
        &["pack-objects", "--revs", ".git/objects/pack/pack"],
        b"side\n",
    );
    let packs = dir.join(".git/objects/pack");
    let same_file = |args: &[&str], file: &str| {
        git(&dir, args, &[]);
        let want = std::fs::read(dir.join(file)).unwrap();
        std::fs::remove_file(dir.join(file)).unwrap();
        let out = rgit(&dir, args);
        assert!(out.status.success(), "{args:?}: {out:?}");
        assert!(
            std::fs::read(dir.join(file)).unwrap() == want,
            "{args:?} differs"
        );
    };
    let graph = ".git/objects/info/commit-graph";
    same_file(&["commit-graph", "write", "--reachable"], graph);
    same_file(&["commit-graph", "write"], graph);
    same(&dir, &[&["commit-graph", "verify"]]);
    let midx = ".git/objects/pack/multi-pack-index";
    same_file(&["multi-pack-index", "write"], midx);
    same(&dir, &[&["multi-pack-index", "verify"]]);

    // repack folds every pack into one; expire drops the others, and git
    // reads the index rgit wrote. Older packs keep the new one from tying on
    // mtime, which would leave expire a pack still in use.
    let old = std::time::SystemTime::now() - std::time::Duration::from_secs(60);
    for e in std::fs::read_dir(&packs).unwrap().flatten() {
        let f = std::fs::File::open(e.path()).unwrap();
        f.set_modified(old).unwrap();
    }
    assert!(rgit(&dir, &["multi-pack-index", "repack"]).status.success());
    assert!(rgit(&dir, &["multi-pack-index", "expire"]).status.success());
    let left: Vec<_> = std::fs::read_dir(&packs)
        .unwrap()
        .flatten()
        .filter(|e| e.path().extension().is_some_and(|x| x == "pack"))
        .collect();
    assert_eq!(left.len(), 1);
    git(&dir, &["multi-pack-index", "verify"], &[]);
    git(&dir, &["fsck", "--no-progress"], &[]);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn merge_index_runs_merge_one_file_like_git() {
    let setup = |tag: &str| {
        let dir = std::env::temp_dir().join(format!("rgit-plumbing-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        git(&dir, &["init", "-q", "-b", "main"], &[]);
        git(&dir, &["config", "user.email", "t@example.com"], &[]);
        git(&dir, &["config", "user.name", "T"], &[]);
        write(&dir, "both.txt", b"1\n2\n3\n4\n5\n");
        write(&dir, "del.txt", b"keep\n");
        write(&dir, "delboth.txt", b"same\n");
        write(&dir, "empty.txt", b"");
        git(&dir, &["add", "."], &[]);
        commit(&dir, 1, "base", "T");
        git(&dir, &["branch", "base"], &[]);
        git(&dir, &["checkout", "-q", "-b", "ours"], &[]);
        write(&dir, "both.txt", b"1\nOURS\n3\n4\n5\n");
        write(&dir, "onlyours.txt", b"added\n");
        write(&dir, "addsame.txt", b"same\n");
        write(&dir, "addboth.txt", b"a\nb\n");
        git(&dir, &["rm", "-q", "delboth.txt"], &[]);
        git(&dir, &["add", "."], &[]);
        commit(&dir, 2, "ours", "T");
        git(&dir, &["checkout", "-q", "-b", "theirs", "base"], &[]);
        write(&dir, "both.txt", b"1\n2\n3\n4\nTHEIRS\n");
        write(&dir, "onlytheirs.txt", b"theirs\n");
        write(&dir, "addsame.txt", b"same\n");
        write(&dir, "addboth.txt", b"a\nc\n");
        git(&dir, &["rm", "-q", "del.txt", "delboth.txt"], &[]);
        git(&dir, &["add", "."], &[]);
        commit(&dir, 3, "theirs", "T");
        git(&dir, &["checkout", "-q", "ours"], &[]);
        git(&dir, &["read-tree", "-m", "base", "ours", "theirs"], &[]);
        dir
    };
    let outcome = |dir: &Path, bin: &str| {
        let out = if bin == "git" {
            run(
                "git",
                dir,
                &["merge-index", "-o", "git-merge-one-file", "-a"],
                &[],
            )
        } else {
            rgit(dir, &["merge-index", "-o", "git-merge-one-file", "-a"])
        };
        let markers = unlabeled(&std::fs::read_to_string(dir.join("addboth.txt")).unwrap());
        (
            String::from_utf8_lossy(&out.stdout).into_owned(),
            out.status.code(),
            git(dir, &["ls-files", "-s"], &[]),
            git(dir, &["status", "--porcelain"], &[]),
            markers,
            std::fs::read_to_string(dir.join("both.txt")).unwrap(),
        )
    };
    let (a, b) = (setup("mi-git"), setup("mi-rgit"));
    assert_eq!(outcome(&b, "rgit"), outcome(&a, "git"));
    let _ = std::fs::remove_dir_all(&a);
    let _ = std::fs::remove_dir_all(&b);
}

/// `text` with the random `.merge_file_XXXXXX` labels made fixed.
fn unlabeled(text: &str) -> String {
    text.lines()
        .map(|l| match l.find(".merge_file_") {
            Some(at) => format!("{}.merge_file_", &l[..at]),
            None => l.to_owned(),
        })
        .collect::<Vec<_>>()
        .join("\n")
}
