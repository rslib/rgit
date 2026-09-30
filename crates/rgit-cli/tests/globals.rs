//! git's top-level options, aliases, external commands and the pager,
//! compared with git itself.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::Duration;

fn base(cmd: &mut Command, dir: &Path) {
    cmd.current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("RGIT_OPLOG", "0")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_PAGER")
        // Identity comes from repo/config files in these tests; an ambient
        // GIT_AUTHOR_*/GIT_COMMITTER_* (e.g. CI-wide env) would override it.
        .env_remove("GIT_AUTHOR_NAME")
        .env_remove("GIT_AUTHOR_EMAIL")
        .env_remove("GIT_COMMITTER_NAME")
        .env_remove("GIT_COMMITTER_EMAIL");
}

fn git(dir: &Path, args: &[&str]) -> Output {
    let mut cmd = Command::new("git");
    base(&mut cmd, dir);
    cmd.args(args).output().unwrap()
}

fn rgit(dir: &Path, args: &[&str]) -> Output {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_rgit"));
    base(&mut cmd, dir);
    cmd.arg("--human").args(args).output().unwrap()
}

fn out(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

fn err(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

/// A folder holding repo `r` (with a `sub` folder) and one commit.
fn setup(tag: &str) -> PathBuf {
    let top = std::env::temp_dir().join(format!("rgit-globals-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&top);
    let r = top.join("r");
    std::fs::create_dir_all(r.join("sub")).unwrap();
    for args in [
        &["init", "-q", "-b", "main"][..],
        &["config", "user.name", "t"],
        &["config", "user.email", "t@t"],
    ] {
        git(&r, args);
    }
    std::fs::write(r.join("sub/a"), "a\n").unwrap();
    git(&r, &["add", "."]);
    git(&r, &["commit", "-qm", "init"]);
    top.canonicalize().unwrap()
}

#[test]
fn dash_c_paths_chain() {
    let top = setup("chain");
    let args = ["-C", "r", "-C", "sub", "log", "--oneline"];
    let (r, g) = (rgit(&top, &args), git(&top, &args));
    assert!(r.status.success(), "{}", err(&r));
    assert_eq!(out(&r), out(&g));
    let r = rgit(&top, &["-C", "nope", "log"]);
    assert_eq!(r.status.code(), Some(128));
    assert!(err(&r).contains("cannot change to 'nope'"), "{}", err(&r));
}

#[test]
fn dash_c_config_reaches_every_read() {
    let top = setup("config");
    let r = top.join("r");
    let args = ["-c", "user.name=Zed", "config", "--get", "user.name"];
    assert_eq!(out(&rgit(&r, &args)), "Zed\n");
    let args = [
        "-c",
        "user.name=Zed",
        "-c",
        "core.pager",
        "config",
        "--list",
        "--show-scope",
        "--show-origin",
    ];
    let pick = |s: String| {
        s.lines()
            .filter(|l| l.starts_with("command"))
            .map(str::to_owned)
            .collect::<Vec<_>>()
    };
    assert_eq!(pick(out(&rgit(&r, &args))), pick(out(&git(&r, &args))));
    // Commits read the identity through libgit2's own config.
    std::fs::write(r.join("b"), "b\n").unwrap();
    git(&r, &["add", "b"]);
    let c = rgit(
        &r,
        &[
            "-c",
            "user.name=Cli Person",
            "-c",
            "user.email=c@c",
            "commit",
            "-m",
            "b",
        ],
    );
    assert!(c.status.success(), "{}", err(&c));
    assert_eq!(
        out(&git(&r, &["log", "-1", "--format=%an <%ae>"])),
        "Cli Person <c@c>\n"
    );
    let colored = out(&rgit(&r, &["-c", "color.ui=always", "log", "--oneline"]));
    assert!(colored.contains('\x1b'), "{colored:?}");
    let plain = out(&rgit(&r, &["-c", "color.ui=never", "log", "--oneline"]));
    assert!(!plain.contains('\x1b'), "{plain:?}");
    // --config-env and GIT_CONFIG_COUNT, as git reads them.
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_rgit"));
    base(&mut cmd, &r);
    let o = cmd
        .env("WHO", "Env Name")
        .env("GIT_CONFIG_COUNT", "1")
        .env("GIT_CONFIG_KEY_0", "user.email")
        .env("GIT_CONFIG_VALUE_0", "e@e")
        .args(["--human", "--config-env=user.name=WHO", "config", "--list"])
        .output()
        .unwrap();
    let text = out(&o);
    assert!(
        text.ends_with("user.name=Env Name\nuser.email=e@e\n"),
        "{text}"
    );
    let bad = rgit(&r, &["-c", "nosection", "log"]);
    assert_eq!(bad.status.code(), Some(128));
}

#[test]
fn global_config_env_is_honoured_by_libgit2_reads() {
    let top = setup("envcfg");
    let r = top.join("r");
    git(&r, &["config", "--unset", "user.name"]);
    git(&r, &["config", "--unset", "user.email"]);
    let home = top.join("home");
    std::fs::create_dir_all(home.join(".config/git")).unwrap();
    std::fs::write(
        home.join(".gitconfig"),
        "[user]\n\tname = Home\n\temail = h@h\n",
    )
    .unwrap();
    std::fs::write(
        home.join(".config/git/config"),
        "[user]\n\tname = Xdg\n\temail = x@x\n",
    )
    .unwrap();
    std::fs::write(
        top.join("global"),
        "[user]\n\tname = Global\n\temail = g@g\n",
    )
    .unwrap();
    std::fs::write(r.join("c"), "c\n").unwrap();
    git(&r, &["add", "c"]);
    let run = |global: &Path| {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_rgit"));
        base(&mut cmd, &r);
        cmd.env("HOME", &home)
            .env_remove("XDG_CONFIG_HOME")
            .env("GIT_CONFIG_GLOBAL", global)
            .args(["--human", "commit", "-m", "c"])
            .output()
            .unwrap()
    };
    let o = run(&top.join("global"));
    assert!(o.status.success(), "{}", err(&o));
    assert_eq!(
        out(&git(&r, &["log", "-1", "--format=%an <%ae>"])),
        "Global <g@g>\n"
    );
    // With /dev/null neither ~/.gitconfig nor the XDG file is read.
    std::fs::write(r.join("c"), "c2\n").unwrap();
    git(&r, &["add", "c"]);
    let o = run(Path::new("/dev/null"));
    let author = out(&git(&r, &["log", "-1", "--format=%an"]));
    assert!(
        !o.status.success() || !["Home\n", "Xdg\n"].contains(&author.as_str()),
        "{author}"
    );
}

#[test]
fn git_dir_and_work_tree() {
    let top = setup("gitdir");
    std::fs::write(top.join("r/new"), "n\n").unwrap();
    let args = ["--git-dir=r/.git", "--work-tree=r", "add", "new"];
    assert!(rgit(&top, &args).status.success());
    assert_eq!(
        out(&git(&top.join("r"), &["diff", "--cached", "--name-only"])),
        "new\n"
    );
    let args = ["--git-dir", "r/.git", "log", "--oneline"];
    assert_eq!(out(&rgit(&top, &args)), out(&git(&top, &args)));
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_rgit"));
    base(&mut cmd, &top);
    let o = cmd
        .env("GIT_DIR", "r/.git")
        .env("GIT_WORK_TREE", "r")
        .args(["--human", "diff", "--cached", "--name-only"])
        .output()
        .unwrap();
    assert_eq!(out(&o), "new\n", "{}", err(&o));
}

#[test]
fn aliases_expand_like_git() {
    let top = setup("alias");
    let r = top.join("r");
    git(&r, &["config", "alias.l", "log --format='%s by %an'"]);
    git(&r, &["config", "alias.ll", "l -1"]);
    git(
        &r,
        &[
            "config",
            "alias.where",
            "!echo \"top=$(pwd) prefix=$GIT_PREFIX\"",
        ],
    );
    git(&r, &["config", "alias.a", "b"]);
    git(&r, &["config", "alias.b", "a"]);
    for args in [&["ll"][..], &["l", "--", "sub"], &["help", "l"]] {
        let (x, y) = (rgit(&r, args), git(&r, args));
        assert_eq!(out(&x), out(&y), "{args:?}: {}", err(&x));
    }
    let sub = r.join("sub");
    let (x, y) = (rgit(&sub, &["where", "1"]), git(&sub, &["where", "1"]));
    assert_eq!(out(&x), out(&y));
    assert!(out(&x).contains("prefix=sub/ 1"), "{}", out(&x));
    let (x, y) = (rgit(&r, &["a"]), git(&r, &["a"]));
    assert_eq!(x.status.code(), Some(128));
    assert_eq!(
        err(&x).replace("rgit", "git"),
        err(&y)
            .lines()
            .filter(|l| !l.starts_with("hint"))
            .collect::<Vec<_>>()
            .join("\n")
            + "\n"
    );
}

#[test]
fn external_commands_run_from_path() {
    use std::os::unix::fs::PermissionsExt;
    let top = setup("external");
    let bin = top.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    for (name, body) in [
        ("git-hello", "echo git-hello \"$@\""),
        ("rgit-hi", "echo rgit-hi \"$@\"; exit 3"),
    ] {
        let p = bin.join(name);
        std::fs::write(&p, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let path = format!("{}:{}", bin.display(), std::env::var("PATH").unwrap());
    let run = |args: &[&str]| {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_rgit"));
        base(&mut cmd, &top.join("r"));
        cmd.env("PATH", &path).args(args).output().unwrap()
    };
    assert_eq!(out(&run(&["hello", "a b", "c"])), "git-hello a b c\n");
    let o = run(&["hi", "x"]);
    assert_eq!(
        (out(&o), o.status.code()),
        ("rgit-hi x\n".to_owned(), Some(3))
    );
    let o = run(&["--human", "stauts"]);
    assert!(!o.status.success());
    assert!(err(&o).contains("status"), "{}", err(&o));
}

/// Run rgit under a pty with a fake pager that copies its input to a file.
fn paged(dir: &Path, args: &[&str], env: &[(&str, &str)]) -> Option<String> {
    use portable_pty::{CommandBuilder, PtySize, native_pty_system};
    let log = dir.join(format!("paged-{}", args.join("_").replace('/', "")));
    let _ = std::fs::remove_file(&log);
    let pair = native_pty_system()
        .openpty(PtySize {
            rows: 24,
            cols: 80,
            pixel_width: 0,
            pixel_height: 0,
        })
        .unwrap();
    let mut cmd = CommandBuilder::new(env!("CARGO_BIN_EXE_rgit"));
    cmd.args(args);
    cmd.cwd(dir);
    cmd.env("GIT_CONFIG_GLOBAL", "/dev/null");
    cmd.env("GIT_CONFIG_SYSTEM", "/dev/null");
    cmd.env("RGIT_OPLOG", "0");
    cmd.env("TERM", "dumb");
    cmd.env_remove("LESS");
    cmd.env(
        "GIT_PAGER",
        format!("echo \"LESS=$LESS\" > '{0}'; cat >> '{0}'", log.display()),
    );
    for (k, v) in env {
        cmd.env(k, v);
    }
    let mut child = pair.slave.spawn_command(cmd).unwrap();
    drop(pair.slave);
    let mut reader = pair.master.try_clone_reader().unwrap();
    std::thread::spawn(move || {
        let mut sink = Vec::new();
        let _ = reader.read_to_end(&mut sink);
    });
    for _ in 0..200 {
        if child.try_wait().unwrap().is_some() {
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    std::fs::read_to_string(&log).ok()
}

#[test]
fn pager_like_git() {
    let top = setup("pager");
    let r = top.join("r");
    let text = paged(&r, &["log", "--oneline"], &[]).expect("log is paged");
    assert!(
        text.starts_with("LESS=FRX\n") && text.contains(" init"),
        "{text:?}"
    );
    assert!(paged(&r, &["-P", "log", "--oneline"], &[]).is_none());
    assert!(paged(&r, &["status"], &[]).is_none());
    assert!(paged(&r, &["-p", "status"], &[]).is_some());
    assert!(paged(&r, &["-c", "pager.log=false", "log"], &[]).is_none());
    assert!(paged(&r, &["-c", "pager.status", "status"], &[]).is_some());
    assert!(paged(&r, &["--toon", "log"], &[]).is_none());
    // Not a terminal: never paged.
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_rgit"));
    base(&mut cmd, &r);
    let o = cmd
        .env("GIT_PAGER", "echo paged")
        .args(["--human", "-p", "log", "--oneline"])
        .output()
        .unwrap();
    assert!(out(&o).contains(" init"), "{}", out(&o));
}

#[test]
fn info_options() {
    let top = setup("info");
    let o = rgit(&top, &["--exec-path"]);
    assert!(o.status.success() && !out(&o).trim().is_empty());
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_rgit"));
    base(&mut cmd, &top);
    let o = cmd
        .args(["--exec-path=/x/y", "--exec-path"])
        .output()
        .unwrap();
    assert_eq!(out(&o), "/x/y\n");
    assert!(rgit(&top, &["--version"]).status.success());
    assert!(rgit(&top, &["--man-path"]).status.success());
    // rgit has no manuals of its own, so it names git's.
    for flag in ["--html-path", "--man-path", "--info-path"] {
        assert_eq!(out(&rgit(&top, &[flag])), out(&git(&top, &[flag])));
    }
}

#[test]
fn alias_pager_and_color_pager() {
    let top = setup("aliaspager");
    let r = top.join("r");
    for (k, v) in [
        ("alias.l", "log -1 --format=%s"),
        ("alias.st", "status --short"),
        ("alias.sh1", "!echo hi"),
        ("alias.pl", "-p status --short"),
        ("alias.np", "--no-pager log"),
        ("alias.cl", "-c log.showRoot=true log -1 --format=%s"),
    ] {
        git(&r, &["config", k, v]);
    }
    assert!(paged(&r, &["l"], &[]).is_some());
    assert!(paged(&r, &["-c", "pager.l=false", "l"], &[]).is_none());
    assert!(paged(&r, &["st"], &[]).is_none());
    let text = paged(&r, &["-c", "pager.sh1=true", "sh1"], &[]).expect("paged");
    assert!(text.contains("hi"), "{text:?}");
    assert!(paged(&r, &["pl"], &[]).is_some());
    let color = [("TERM", "xterm")];
    let fmt = ["log", "-1", "--format=%C(red)%s"];
    let text = paged(&r, &fmt, &color).expect("paged");
    assert!(text.contains("\x1b[31m"), "{text:?}");
    let off = [&["-c", "color.pager=false"][..], &fmt].concat();
    let text = paged(&r, &off, &color).expect("paged");
    assert!(!text.contains('\x1b'), "{text:?}");

    let (o, g) = (rgit(&r, &["np"]), git(&r, &["np"]));
    assert_eq!((err(&o), o.status.code()), (err(&g), g.status.code()));
    assert_eq!(out(&rgit(&r, &["cl"])), out(&git(&r, &["cl"])));
}

#[test]
fn no_command_without_terminal_prints_usage_like_git() {
    let top = setup("usage");
    let r = top.join("r");
    let bare = Command::new(env!("CARGO_BIN_EXE_rgit"))
        .current_dir(&r)
        .env("RGIT_OPLOG", "0")
        .output()
        .unwrap();
    let want = git(&r, &[]);
    assert_eq!(bare.status.code(), want.status.code());
    assert!(out(&bare).starts_with("usage: rgit "), "{}", out(&bare));
    assert!(out(&bare).contains("These are common Git commands"));

    let toon = Command::new(env!("CARGO_BIN_EXE_rgit"))
        .current_dir(&r)
        .env("RGIT_OPLOG", "0")
        .arg("--toon")
        .output()
        .unwrap();
    assert!(toon.status.success(), "{}", err(&toon));
    assert!(!out(&toon).contains("usage:"), "{}", out(&toon));
    let _ = std::fs::remove_dir_all(&top);
}
