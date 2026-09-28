//! `rgit credential`, `credential-store`, `credential-cache` and `scalar`
//! against git, each run with its own HOME so no real config is touched.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

fn home(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("rgc-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir.canonicalize().unwrap()
}

/// Run `program args` in `dir` with HOME there, `input` on stdin: stdout,
/// stderr and the exit code.
fn run(
    program: &str,
    dir: &Path,
    args: &[&str],
    input: &str,
    env: &[(&str, &str)],
) -> (String, String, i32) {
    let mut child = Command::new(program)
        .args(args)
        .current_dir(dir)
        .env("HOME", dir)
        .env("XDG_CONFIG_HOME", dir.join("xdg"))
        .env("XDG_CACHE_HOME", dir.join("cache"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("RGIT_OPLOG", "0")
        .env("RGIT_TEST_MAINT_SCHEDULER_DIR", dir.join("sched"))
        .env_remove("GIT_ASKPASS")
        .env_remove("SSH_ASKPASS")
        .envs(env.iter().copied())
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
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
        out.status.code().unwrap_or(-1),
    )
}

fn git(dir: &Path, args: &[&str], input: &str, env: &[(&str, &str)]) -> (String, String, i32) {
    run("git", dir, args, input, env)
}

fn rgit(dir: &Path, args: &[&str], input: &str, env: &[(&str, &str)]) -> (String, String, i32) {
    let mut all = vec!["--human"];
    all.extend_from_slice(args);
    run(env!("CARGO_BIN_EXE_rgit"), dir, &all, input, env)
}

/// Both tools give the same stdout, stderr and exit code in fresh homes set
/// up by `setup`.
fn same(tag: &str, setup: impl Fn(&Path), args: &[&str], input: &str, env: &[(&str, &str)]) {
    let (a, b) = (home(&format!("{tag}-g")), home(&format!("{tag}-r")));
    setup(&a);
    setup(&b);
    let g = git(&a, args, input, env);
    let r = rgit(&b, args, input, env);
    let norm = |s: &str, d: &Path| s.replace(&d.display().to_string(), "HOME");
    assert_eq!(
        (norm(&r.0, &b), norm(&r.1, &b), r.2),
        (norm(&g.0, &a), norm(&g.1, &a), g.2),
        "{args:?}"
    );
    for f in [".git-credentials", "xdg/git/credentials"] {
        assert_eq!(
            std::fs::read_to_string(b.join(f)).ok(),
            std::fs::read_to_string(a.join(f)).ok(),
            "{f} after {args:?}"
        );
    }
}

fn helper(dir: &Path) {
    let script = dir.join("h.sh");
    std::fs::write(
        &script,
        "#!/bin/sh\necho \"ARGS $@\" >&2\ncat >&2\n[ \"$2\" = get ] && printf 'username=hu\\npassword=hp\\n'\nexit 0\n",
    )
    .unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    let value = format!("!{} extra", script.display());
    git(
        dir,
        &["config", "--global", "credential.helper", &value],
        "",
        &[],
    );
}

fn stored(dir: &Path) {
    std::fs::write(
        dir.join(".git-credentials"),
        "https://a%20b:p%40ss@x/a%20b\nhttps://u:p@y%3a8080\n",
    )
    .unwrap();
}

#[test]
fn credential_runs_helpers_like_git() {
    same(
        "fill",
        helper,
        &["credential", "fill"],
        "url=https://u@example.com/foo/\n",
        &[],
    );
    same(
        "approve",
        helper,
        &["credential", "approve"],
        "protocol=https\nhost=example.com\nusername=a\npassword=b\n",
        &[],
    );
    same(
        "approve-nopass",
        helper,
        &["credential", "approve"],
        "protocol=https\nhost=h\n",
        &[],
    );
    same(
        "reject",
        helper,
        &["credential", "reject"],
        "protocol=https\nhost=h\n",
        &[],
    );
    same(
        "bad-line",
        helper,
        &["credential", "fill"],
        "garbage\n",
        &[],
    );
    same("capability", |_| {}, &["credential", "capability"], "", &[]);
}

#[test]
fn credential_prompts_like_git() {
    same(
        "noprompt",
        |_| {},
        &["credential", "fill"],
        "protocol=https\nhost=z\n",
        &[("GIT_TERMINAL_PROMPT", "0")],
    );
    let ask = |d: &Path| {
        std::fs::write(
            d.join("ask.sh"),
            "#!/bin/sh\necho \"ASK $1\" >&2\necho answer\n",
        )
        .unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(d.join("ask.sh"), std::fs::Permissions::from_mode(0o755)).unwrap();
    };
    let a = home("askpass");
    ask(&a);
    let prog = a.join("ask.sh").display().to_string();
    let env = [("GIT_ASKPASS", prog.as_str())];
    let input = "protocol=https\nhost=z\npath=p\n";
    assert_eq!(
        rgit(&a, &["credential", "fill"], input, &env),
        git(&a, &["credential", "fill"], input, &env)
    );
}

#[test]
fn credential_store_matches_git() {
    let input = "protocol=https\nhost=x\nusername=a b\npassword=p@ss:/\npath=a b/c\n";
    same(
        "st-store",
        |_| {},
        &["credential-store", "store"],
        input,
        &[],
    );
    same(
        "st-store2",
        stored,
        &["credential-store", "store"],
        "protocol=https\nhost=y:8080\nusername=u\npassword=new\n",
        &[],
    );
    same(
        "st-incomplete",
        |_| {},
        &["credential-store", "store"],
        "protocol=https\nhost=x\n",
        &[],
    );
    same(
        "st-get",
        stored,
        &["credential-store", "get"],
        "protocol=https\nhost=x\n",
        &[],
    );
    same(
        "st-get-port",
        stored,
        &["credential-store", "get"],
        "protocol=https\nhost=y:8080\n",
        &[],
    );
    same(
        "st-erase-wrong",
        stored,
        &["credential-store", "erase"],
        "protocol=https\nhost=y:8080\npassword=no\n",
        &[],
    );
    same(
        "st-erase",
        stored,
        &["credential-store", "erase"],
        "protocol=https\nhost=y:8080\n",
        &[],
    );
    same(
        "st-erase-empty",
        stored,
        &["credential-store", "erase"],
        "\n",
        &[],
    );
    let xdg = |d: &Path| {
        std::fs::create_dir_all(d.join("xdg/git")).unwrap();
        std::fs::write(d.join("xdg/git/credentials"), "").unwrap();
    };
    same(
        "st-xdg",
        xdg,
        &["credential-store", "store"],
        "protocol=https\nhost=q\nusername=u\npassword=p\n",
        &[],
    );
    same(
        "st-file",
        |_| {},
        &["credential-store", "--file", "f", "store"],
        "protocol=https\nhost=q\nusername=u\npassword=p\n",
        &[],
    );
    same(
        "st-unknown",
        stored,
        &["credential-store", "bogus"],
        "",
        &[],
    );
}

#[test]
fn credential_cache_keeps_and_forgets() {
    let d = home("cache");
    let sock = d.join("s/sock").display().to_string();
    let cache = |op: &str, input: &str| {
        rgit(
            &d,
            &["credential-cache", "--socket", &sock, "--timeout", "60", op],
            input,
            &[],
        )
        .0
    };
    assert_eq!(cache("get", "protocol=https\nhost=a\n"), "");
    cache("store", "protocol=https\nhost=a\nusername=u\npassword=p\n");
    assert_eq!(
        cache("get", "protocol=https\nhost=a\n"),
        "capability[]=authtype\nusername=u\npassword=p\n"
    );
    assert_eq!(cache("get", "protocol=https\nhost=b\n"), "");
    cache("erase", "protocol=https\nhost=a\n");
    assert_eq!(cache("get", "protocol=https\nhost=a\n"), "");
    cache("exit", "");
    assert!(!Path::new(&sock).exists());

    let helper = format!("cache --socket {sock}");
    git(
        &d,
        &["config", "--global", "credential.helper", &helper],
        "",
        &[],
    );
    rgit(
        &d,
        &["credential", "approve"],
        "protocol=https\nhost=a\nusername=u\npassword=p\n",
        &[],
    );
    let (out, _, code) = rgit(
        &d,
        &["credential", "fill"],
        "protocol=https\nhost=a\n",
        &[("GIT_TERMINAL_PROMPT", "0")],
    );
    assert_eq!(
        (out.as_str(), code),
        ("protocol=https\nhost=a\nusername=u\npassword=p\n", 0)
    );
    cache("exit", "");
}

#[test]
fn scalar_registers_and_lists() {
    let d = home("scalar");
    std::fs::create_dir_all(d.join("sched")).unwrap();
    git(&d, &["init", "-q", "big/src"], "", &[]);
    let (_, err, code) = rgit(&d, &["scalar", "register", "big"], "", &[]);
    assert_eq!(code, 0, "{err}");
    let repo = d.join("big/src").display().to_string();
    assert_eq!(
        rgit(&d, &["scalar", "list"], "", &[]).0,
        format!("{repo}\n")
    );
    let get = |k: &str| git(&d, &["-C", "big/src", "config", "--get-all", k], "", &[]).0;
    assert_eq!(get("index.version"), "4\n");
    assert_eq!(get("log.excludeDecoration"), "refs/prefetch/*\n");
    assert_eq!(get("maintenance.repo"), format!("{repo}\n"), "{err}");
    let (_, err, code) = rgit(&d, &["scalar", "run", "commit-graph", "big"], "", &[]);
    assert_eq!(code, 0, "{err}");
    rgit(&d, &["scalar", "unregister", "big"], "", &[]);
    assert_eq!(rgit(&d, &["scalar", "list"], "", &[]).0, "");
    assert_eq!(get("maintenance.repo"), "");
}
