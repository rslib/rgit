use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

use rgit_git::{Git2Backend, GitBackend};

fn init_repo(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("rgit-it-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    for args in [
        vec!["init", "-q", "-b", "main"],
        vec!["config", "user.email", "t@example.com"],
        vec!["config", "user.name", "test"],
    ] {
        git(&dir, &args);
    }
    dir
}

fn git(dir: &Path, args: &[&str]) {
    assert!(
        Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .status()
            .unwrap()
            .success(),
        "git {args:?} failed"
    );
}

fn commit(dir: &Path, file: &str, msg: &str) {
    std::fs::write(dir.join(file), msg).unwrap();
    git(dir, &["add", file]);
    git(dir, &["commit", "-q", "-m", msg]);
}

fn remote_ref(dir: &Path, remote: &str, branch: &str) {
    git(
        dir,
        &[
            "update-ref",
            &format!("refs/remotes/{remote}/{branch}"),
            "HEAD",
        ],
    );
}

fn overview(dir: &Path) -> Vec<rgit_git::RemoteOverview> {
    Git2Backend::discover(dir)
        .unwrap()
        .remote_overview()
        .unwrap()
}

#[test]
fn local_two_remotes_show_urls_and_push_url_only_when_different() {
    let dir = init_repo("rv-urls");
    git(
        &dir,
        &["remote", "add", "origin", "https://example.com/a.git"],
    );
    git(
        &dir,
        &["remote", "add", "fork", "https://example.com/b.git"],
    );
    git(
        &dir,
        &[
            "remote",
            "set-url",
            "--push",
            "fork",
            "git@example.com:b.git",
        ],
    );
    git(
        &dir,
        &[
            "config",
            "remote.origin.pushurl",
            "https://example.com/a.git",
        ],
    );

    let all = overview(&dir);

    let names: Vec<_> = all.iter().map(|r| r.name.as_str()).collect();
    assert_eq!(names, ["fork", "origin"]);
    assert_eq!(all[0].fetch_url, "https://example.com/b.git");
    assert_eq!(all[0].push_url.as_deref(), Some("git@example.com:b.git"));
    assert_eq!(all[1].fetch_url, "https://example.com/a.git");
    assert_eq!(all[1].push_url, None);
}

#[test]
fn local_never_fetched_remote_has_no_branches_and_no_last_fetch() {
    let dir = init_repo("rv-never");
    git(
        &dir,
        &["remote", "add", "origin", "https://example.com/a.git"],
    );

    let all = overview(&dir);

    assert_eq!(all.len(), 1);
    assert!(all[0].branches.is_empty());
    assert_eq!(all[0].last_fetch, None);
    assert_eq!(all[0].head_branch, None);
}

#[test]
fn local_head_branch_comes_from_remote_head() {
    let dir = init_repo("rv-head");
    commit(&dir, "a.txt", "one");
    git(
        &dir,
        &["remote", "add", "origin", "https://example.com/a.git"],
    );
    remote_ref(&dir, "origin", "trunk");
    git(
        &dir,
        &[
            "symbolic-ref",
            "refs/remotes/origin/HEAD",
            "refs/remotes/origin/trunk",
        ],
    );

    let all = overview(&dir);

    assert_eq!(all[0].head_branch.as_deref(), Some("trunk"));
    assert!(all[0].last_fetch.is_some());
}

#[test]
fn local_tracked_branch_shows_local_name_ahead_and_behind() {
    let dir = init_repo("rv-track");
    commit(&dir, "a.txt", "one");
    git(
        &dir,
        &["remote", "add", "origin", "https://example.com/a.git"],
    );
    remote_ref(&dir, "origin", "topic");
    remote_ref(&dir, "origin", "other");
    git(&dir, &["checkout", "-q", "-b", "topic"]);
    git(&dir, &["config", "branch.topic.remote", "origin"]);
    git(&dir, &["config", "branch.topic.merge", "refs/heads/topic"]);
    commit(&dir, "b.txt", "two");
    commit(&dir, "c.txt", "three");

    let all = overview(&dir);

    let branches = &all[0].branches;
    assert_eq!(branches.len(), 2);
    assert_eq!(branches[0].name, "other");
    assert!(branches[0].tracked_by.is_none());
    assert_eq!(branches[1].name, "topic");
    assert_eq!(branches[1].subject, "one");
    let tracked = branches[1].tracked_by.as_ref().unwrap();
    assert_eq!(tracked.local, "topic");
    assert_eq!((tracked.ahead, tracked.behind), (2, 0));
}

#[test]
fn local_branch_names_drop_the_remote_prefix_and_skip_head() {
    let dir = init_repo("rv-names");
    commit(&dir, "a.txt", "one");
    git(
        &dir,
        &["remote", "add", "origin", "https://example.com/a.git"],
    );
    git(
        &dir,
        &["remote", "add", "team/a", "https://example.com/t.git"],
    );
    remote_ref(&dir, "origin", "feature/b");
    remote_ref(&dir, "origin", "feature/a");
    remote_ref(&dir, "origin", "main");
    git(
        &dir,
        &[
            "symbolic-ref",
            "refs/remotes/origin/HEAD",
            "refs/remotes/origin/main",
        ],
    );
    remote_ref(&dir, "team/a", "dev");

    let all = overview(&dir);

    let names =
        |i: usize| -> Vec<String> { all[i].branches.iter().map(|b| b.name.clone()).collect() };
    assert_eq!(all[0].name, "origin");
    assert_eq!(names(0), ["feature/a", "feature/b", "main"]);
    assert_eq!(all[1].name, "team/a");
    assert_eq!(names(1), ["dev"]);
}

#[test]
fn local_fetch_refspecs_are_listed() {
    let dir = init_repo("rv-specs");
    git(
        &dir,
        &["remote", "add", "origin", "https://example.com/a.git"],
    );
    git(
        &dir,
        &[
            "config",
            "--add",
            "remote.origin.fetch",
            "+refs/tags/*:refs/tags/*",
        ],
    );

    let all = overview(&dir);

    assert_eq!(
        all[0].fetch_refspecs,
        [
            "+refs/heads/*:refs/remotes/origin/*",
            "+refs/tags/*:refs/tags/*"
        ]
    );
}

#[test]
fn local_does_not_touch_the_network() {
    let dir = init_repo("rv-net");
    commit(&dir, "a.txt", "one");
    let url = serve(None);
    git(&dir, &["remote", "add", "origin", &url]);
    remote_ref(&dir, "origin", "main");

    let start = Instant::now();
    let all = overview(&dir);

    assert!(start.elapsed() < Duration::from_secs(2));
    assert_eq!(all[0].fetch_url, url);
    assert_eq!(all[0].branches.len(), 1);
}

#[test]
fn local_urls_are_the_rewritten_ones() {
    let dir = init_repo("rv-insteadof");
    git(
        &dir,
        &["config", "url.https://example.com/.insteadOf", "ex:"],
    );
    git(
        &dir,
        &[
            "config",
            "url.git@example.com:.pushInsteadOf",
            "https://example.com/",
        ],
    );
    git(&dir, &["remote", "add", "origin", "ex:a.git"]);
    git(
        &dir,
        &["remote", "add", "other", "https://example.com/b.git"],
    );

    let all = overview(&dir);

    // As `git remote get-url --push`: pushInsteadOf sees the URL as written.
    assert_eq!(all[0].fetch_url, "https://example.com/a.git");
    assert_eq!(all[0].push_url, None);
    assert_eq!(all[1].fetch_url, "https://example.com/b.git");
    assert_eq!(all[1].push_url.as_deref(), Some("git@example.com:b.git"));
}

#[test]
fn local_remote_names_that_share_a_prefix_keep_their_own_branches() {
    let dir = init_repo("rv-prefix");
    commit(&dir, "a.txt", "one");
    for name in ["origin", "origin2", "team", "team/a"] {
        git(
            &dir,
            &[
                "remote",
                "add",
                name,
                &format!("https://example.com/{name}.git"),
            ],
        );
    }
    remote_ref(&dir, "origin", "x");
    remote_ref(&dir, "origin2", "y");
    remote_ref(&dir, "team", "z");
    remote_ref(&dir, "team/a", "dev");

    let all = overview(&dir);

    let branches = |name: &str| -> Vec<String> {
        let remote = all.iter().find(|r| r.name == name).unwrap();
        remote.branches.iter().map(|b| b.name.clone()).collect()
    };
    assert_eq!(branches("origin"), ["x"]);
    assert_eq!(branches("origin2"), ["y"]);
    assert_eq!(branches("team"), ["z"]);
    assert_eq!(branches("team/a"), ["dev"]);
}

#[test]
fn local_a_ref_that_is_not_a_commit_is_skipped() {
    let dir = init_repo("rv-blob");
    commit(&dir, "a.txt", "one");
    git(
        &dir,
        &["remote", "add", "origin", "https://example.com/a.git"],
    );
    remote_ref(&dir, "origin", "main");
    let blob = Command::new("git")
        .arg("-C")
        .arg(&dir)
        .args(["rev-parse", "HEAD:a.txt"])
        .output()
        .unwrap();
    let blob = String::from_utf8(blob.stdout).unwrap();
    git(
        &dir,
        &["update-ref", "refs/remotes/origin/blob", blob.trim()],
    );

    let all = overview(&dir);

    let names: Vec<_> = all[0].branches.iter().map(|b| b.name.as_str()).collect();
    assert_eq!(names, ["main"]);
}

fn serve(reply: Option<&'static str>) -> String {
    use std::io::{Read, Write};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/x.git", listener.local_addr().unwrap());
    std::thread::spawn(move || {
        let mut held = Vec::new();
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            match reply {
                Some(reply) => {
                    let mut buf = [0u8; 4096];
                    let _ = stream.read(&mut buf);
                    let _ = stream.write_all(reply.as_bytes());
                }
                None => held.push(stream),
            }
        }
    });
    url
}

fn server_branches(dir: &Path, timeout: Duration) -> (Option<Vec<String>>, Duration) {
    let start = Instant::now();
    let found = Git2Backend::discover(dir)
        .unwrap()
        .remote_server_branches("origin", timeout)
        .unwrap();
    (found, start.elapsed())
}

#[test]
fn server_lists_the_branches_of_a_reachable_remote() {
    let upstream = init_repo("rv-srv-up");
    commit(&upstream, "a.txt", "one");
    git(&upstream, &["branch", "feature/x"]);
    let dir = init_repo("rv-srv-ok");
    git(
        &dir,
        &["remote", "add", "origin", upstream.to_str().unwrap()],
    );

    let (found, _) = server_branches(&dir, Duration::from_secs(10));

    assert_eq!(found, Some(vec!["feature/x".to_owned(), "main".to_owned()]));
}

#[test]
fn server_password_request_is_not_checked_without_a_prompt() {
    let url = serve(Some(
        "HTTP/1.1 401 Unauthorized\r\nWWW-Authenticate: Basic realm=\"x\"\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
    ));
    let dir = init_repo("rv-srv-auth");
    git(&dir, &["remote", "add", "origin", &url]);

    let (found, took) = server_branches(&dir, Duration::from_secs(10));

    assert_eq!(found, None);
    assert!(took < Duration::from_secs(5), "took {took:?}");
}

#[test]
fn server_that_hangs_is_not_checked_after_the_time_limit() {
    let url = serve(None);
    let dir = init_repo("rv-srv-hang");
    git(&dir, &["remote", "add", "origin", &url]);

    let (found, took) = server_branches(&dir, Duration::from_secs(1));

    assert_eq!(found, None);
    assert!(took >= Duration::from_secs(1), "took {took:?}");
    assert!(took < Duration::from_secs(4), "took {took:?}");
}

#[test]
fn server_check_does_not_block_other_calls_while_it_waits() {
    let url = serve(None);
    let dir = init_repo("rv-srv-lock");
    git(&dir, &["remote", "add", "origin", &url]);
    let backend = std::sync::Arc::new(Git2Backend::discover(&dir).unwrap());
    let checker = std::sync::Arc::clone(&backend);
    let check = std::thread::spawn(move || {
        checker.remote_server_branches("origin", Duration::from_secs(3))
    });
    std::thread::sleep(Duration::from_millis(300));

    let start = Instant::now();
    backend.remotes().unwrap();
    let took = start.elapsed();

    assert!(took < Duration::from_secs(1), "remotes() waited {took:?}");
    assert_eq!(check.join().unwrap().unwrap(), None);
}

#[test]
fn local_ahead_behind_counts_between_two_refs() {
    let dir = init_repo("rv-ab");
    commit(&dir, "a.txt", "one");
    git(
        &dir,
        &["remote", "add", "upstream", "https://example.com/u.git"],
    );
    remote_ref(&dir, "upstream", "main");
    commit(&dir, "b.txt", "two");
    commit(&dir, "c.txt", "three");

    let backend = Git2Backend::discover(&dir).unwrap();

    assert_eq!(
        backend.ahead_behind("main", "upstream/main").unwrap(),
        (2, 0)
    );
    assert_eq!(
        backend.ahead_behind("upstream/main", "main").unwrap(),
        (0, 2)
    );
    assert!(backend.ahead_behind("main", "upstream/nope").is_err());
}
