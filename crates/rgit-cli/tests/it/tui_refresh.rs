//! The TUI refreshes on its own after a stage or commit made by another
//! process, in a plain repository and in a linked worktree. Each case drives
//! the real `rgit` TUI under a pty and times the refresh from the TUI's log.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use portable_pty::{CommandBuilder, PtySize, native_pty_system};

fn git(dir: &Path, args: &[&str]) {
    let status = Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .status()
        .expect("run git");
    assert!(status.success(), "git {args:?} failed");
}

/// Removes a test's temp folder when the test ends, pass or fail.
struct TempDir(PathBuf);

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A repository with one commit and an unstaged change to `f`.
fn repo(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("rgit-tui-refresh-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("repo")).expect("mkdir");
    let repo = dir.join("repo");
    git(&repo, &["init", "-q", "-b", "main"]);
    git(&repo, &["config", "user.email", "t@t"]);
    git(&repo, &["config", "user.name", "t"]);
    git(&repo, &["config", "commit.gpgsign", "false"]);
    std::fs::write(repo.join("f"), "a\n").expect("write");
    git(&repo, &["add", "f"]);
    git(&repo, &["commit", "-qm", "one"]);
    std::fs::write(repo.join("f"), "a\nb\n").expect("write");
    dir
}

/// The TUI running in `workdir` under a pty, with its log in `state`.
struct Tui {
    child: Box<dyn portable_pty::Child + Send + Sync>,
    screen: Arc<Mutex<Vec<u8>>>,
    log: PathBuf,
    _master: Box<dyn portable_pty::MasterPty + Send>,
    _writer: Box<dyn Write + Send>,
}

impl Tui {
    fn open(workdir: &Path, state: &Path) -> Tui {
        let pair = native_pty_system()
            .openpty(PtySize {
                rows: 40,
                cols: 120,
                pixel_width: 0,
                pixel_height: 0,
            })
            .expect("openpty");
        let mut cmd = CommandBuilder::new(env!("CARGO_BIN_EXE_rgit"));
        cmd.cwd(workdir);
        cmd.env("TERM", "xterm-256color");
        cmd.env("XDG_STATE_HOME", state);
        cmd.env("GIT_CONFIG_GLOBAL", "/dev/null");
        cmd.env("GIT_CONFIG_NOSYSTEM", "1");
        cmd.env("RGIT_OPLOG", "0");
        for marker in ["CLAUDECODE", "AI_AGENT", "AGENT", "RGIT_OUTPUT"] {
            cmd.env_remove(marker);
        }
        let child = pair.slave.spawn_command(cmd).expect("spawn");
        drop(pair.slave);
        let mut reader = pair.master.try_clone_reader().expect("reader");
        let mut writer = pair.master.take_writer().expect("writer");
        // Answer the terminal's cursor-position query up front.
        writer.write_all(b"\x1b[40;120R").expect("write");
        writer.flush().expect("flush");
        let screen = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&screen);
        std::thread::spawn(move || {
            let mut buf = [0u8; 8192];
            while let Ok(n) = reader.read(&mut buf) {
                if n == 0 {
                    break;
                }
                sink.lock().unwrap().extend_from_slice(&buf[..n]);
            }
        });
        let tui = Tui {
            child,
            screen,
            log: state.join("rgit/rgit.log"),
            _master: pair.master,
            _writer: writer,
        };
        tui.wait_for_refreshes(1, Duration::from_secs(10));
        // Let the startup reads and their file events settle.
        std::thread::sleep(Duration::from_millis(1500));
        tui
    }

    fn refreshes(&self) -> Vec<SystemTime> {
        let text = std::fs::read_to_string(&self.log).unwrap_or_default();
        text.lines()
            .filter(|l| l.contains("git refresh completed"))
            .filter_map(|l| parse_utc(l.split_whitespace().next()?))
            .collect()
    }

    fn wait_for_refreshes(&self, n: usize, limit: Duration) {
        let end = Instant::now() + limit;
        while self.refreshes().len() < n {
            assert!(Instant::now() < end, "the TUI logged no refresh");
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// How long after `since` the first refresh completed, waiting up to `limit`.
    fn refresh_after(&self, since: SystemTime, limit: Duration) -> Option<Duration> {
        let end = Instant::now() + limit;
        loop {
            if let Some(t) = self.refreshes().into_iter().find(|t| *t > since) {
                return t.duration_since(since).ok();
            }
            if Instant::now() >= end {
                return None;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// How much screen output has arrived so far.
    fn mark(&self) -> usize {
        self.screen.lock().unwrap().len()
    }

    /// Whether the screen output after `mark` showed the word `text` within
    /// `limit`; `STAGED` inside `UNSTAGED` does not count.
    fn shows(&self, mark: usize, text: &str, limit: Duration) -> bool {
        let end = Instant::now() + limit;
        loop {
            let raw = self.screen.lock().unwrap()[mark..].to_vec();
            if has_word(&strip(&raw), text) {
                return true;
            }
            if Instant::now() >= end {
                return false;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

impl Drop for Tui {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// A log line's `2026-10-14T02:12:45.351855Z` stamp.
fn parse_utc(stamp: &str) -> Option<SystemTime> {
    let (date, time) = stamp.strip_suffix('Z')?.split_once('T')?;
    let mut d = date.split('-').map(|n| n.parse::<i64>());
    let (y, m, day) = (d.next()?.ok()?, d.next()?.ok()?, d.next()?.ok()?);
    let (hms, frac) = time.split_once('.').unwrap_or((time, "0"));
    let mut t = hms.split(':').map(|n| n.parse::<i64>());
    let (h, min, s) = (t.next()?.ok()?, t.next()?.ok()?, t.next()?.ok()?);
    let nanos: u32 = format!("{frac:0<9}")[..9].parse().ok()?;
    // Days from the civil date (Howard Hinnant's days_from_civil, public domain).
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (m + if m > 2 { -3 } else { 9 }) + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146097 + doe - 719468;
    let secs = days * 86400 + h * 3600 + min * 60 + s;
    Some(SystemTime::UNIX_EPOCH + Duration::new(u64::try_from(secs).ok()?, nanos))
}

fn has_word(screen: &str, word: &str) -> bool {
    screen.match_indices(word).any(|(at, _)| {
        let before = screen[..at].chars().next_back();
        let after = screen[at + word.len()..].chars().next();
        !before.is_some_and(char::is_alphanumeric) && !after.is_some_and(char::is_alphanumeric)
    })
}

fn strip(raw: &[u8]) -> String {
    let text = String::from_utf8_lossy(raw);
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            if chars.peek() == Some(&'[') {
                chars.next();
                for c in chars.by_ref() {
                    if c.is_ascii_alphabetic() {
                        break;
                    }
                }
            }
            out.push(' ');
        } else {
            out.push(c);
        }
    }
    out
}

#[test]
fn tui_refresh_cli_stage_and_commit_show_fast() {
    let dir = repo("plain");
    let _cleanup = TempDir(dir.clone());
    let repo = dir.join("repo");
    let tui = Tui::open(&repo, &dir.join("state"));

    let (before, mark) = (SystemTime::now(), tui.mark());
    git(&repo, &["add", "f"]);
    let took = tui.refresh_after(before, Duration::from_secs(5));
    assert!(tui.shows(mark, "STAGED", Duration::from_secs(5)), "f is not shown staged");
    let took = took.expect("no refresh after git add");
    assert!(took <= Duration::from_millis(500), "refresh after {took:?}");

    let (before, mark) = (SystemTime::now(), tui.mark());
    git(&repo, &["commit", "-qm", "two"]);
    let took = tui.refresh_after(before, Duration::from_secs(5));
    assert!(tui.shows(mark, "two", Duration::from_secs(5)), "the new head is not shown");
    let took = took.expect("no refresh after git commit");
    assert!(took <= Duration::from_millis(500), "refresh after {took:?}");
}

#[test]
fn tui_refresh_linked_worktree_shows_stage_and_commit() {
    let dir = repo("linked");
    let _cleanup = TempDir(dir.clone());
    let repo = dir.join("repo");
    git(&repo, &["checkout", "-q", "--", "f"]);
    let wt = dir.join("wt");
    git(&repo, &["worktree", "add", "-q", wt.to_str().unwrap(), "-b", "side"]);
    std::fs::write(wt.join("f"), "a\nc\n").expect("write");
    let tui = Tui::open(&wt, &dir.join("state"));

    let mark = tui.mark();
    git(&wt, &["add", "f"]);
    assert!(tui.shows(mark, "STAGED", Duration::from_secs(2)), "f is not shown staged");
    let mark = tui.mark();
    git(&wt, &["commit", "-qm", "two"]);
    assert!(tui.shows(mark, "two", Duration::from_secs(2)), "the new head is not shown");
}
