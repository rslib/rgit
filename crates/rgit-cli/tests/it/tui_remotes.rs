//! The remotes view of the TUI under a pty: it draws at once with remotes
//! that refuse or hang, marks a hanging server `not checked` after the limit
//! and fetches one remote on `f`.

use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

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

fn identify(repo: &Path) {
    git(repo, &["config", "user.email", "t@t"]);
    git(repo, &["config", "user.name", "t"]);
    git(repo, &["config", "commit.gpgsign", "false"]);
}

/// Removes a test's temp folder when the test ends, pass or fail.
struct TempDir(PathBuf);

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A local TCP server that accepts connections and never answers.
struct Hang {
    port: u16,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Hang {
    fn start() -> Hang {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        listener.set_nonblocking(true).expect("nonblocking");
        let port = listener.local_addr().expect("addr").port();
        let stop = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&stop);
        let thread = std::thread::spawn(move || {
            let mut held = Vec::new();
            while !flag.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((stream, _)) => held.push(stream),
                    Err(_) => std::thread::sleep(Duration::from_millis(10)),
                }
            }
        });
        Hang {
            port,
            stop,
            thread: Some(thread),
        }
    }
}

impl Drop for Hang {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// A bare server with `main` and `feature/a`, a clone of it as `work` and the
/// remotes `dead` (refused at once) and `hang` (never answers).
struct Fixture {
    dir: PathBuf,
    server: PathBuf,
    work: PathBuf,
    _hang: Hang,
    _cleanup: TempDir,
}

fn fixture(tag: &str) -> Fixture {
    let dir = std::env::temp_dir().join(format!("rgit-tui-remotes-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let cleanup = TempDir(dir.clone());
    let server = dir.join("server.git");
    let seed = dir.join("seed");
    let work = dir.join("work");
    std::fs::create_dir_all(&seed).expect("mkdir");
    git(&dir, &["init", "-q", "--bare", "-b", "main", "server.git"]);
    git(&seed, &["init", "-q", "-b", "main"]);
    identify(&seed);
    std::fs::write(seed.join("f"), "a\n").expect("write");
    git(&seed, &["add", "f"]);
    git(&seed, &["commit", "-qm", "one"]);
    git(&seed, &["checkout", "-q", "-b", "feature/a"]);
    std::fs::write(seed.join("g"), "b\n").expect("write");
    git(&seed, &["add", "g"]);
    git(&seed, &["commit", "-qm", "feature work"]);
    git(
        &seed,
        &["remote", "add", "origin", server.to_str().unwrap()],
    );
    git(&seed, &["push", "-q", "origin", "main", "feature/a"]);
    git(
        &dir,
        &[
            "clone",
            "-q",
            server.to_str().unwrap(),
            work.to_str().unwrap(),
        ],
    );
    identify(&work);
    let hang = Hang::start();
    git(
        &work,
        &["remote", "add", "dead", "http://127.0.0.1:9/x.git"],
    );
    let url = format!("http://127.0.0.1:{}/x.git", hang.port);
    git(&work, &["remote", "add", "hang", &url]);
    Fixture {
        dir,
        server,
        work,
        _hang: hang,
        _cleanup: cleanup,
    }
}

/// The TUI running in `workdir` under a pty.
struct Tui {
    child: Box<dyn portable_pty::Child + Send + Sync>,
    screen: Arc<Mutex<Vec<u8>>>,
    _master: Box<dyn portable_pty::MasterPty + Send>,
    writer: Box<dyn Write + Send>,
}

impl Tui {
    /// Starts the TUI and waits for its first frame.
    fn open(workdir: &Path, state: &Path) -> Tui {
        let tui = Tui::spawn(workdir, state);
        assert!(
            tui.row_shows(|row| row.contains("main"), Duration::from_secs(10)),
            "no first frame within 10s"
        );
        std::thread::sleep(Duration::from_millis(1500));
        tui
    }

    fn spawn(workdir: &Path, state: &Path) -> Tui {
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
        Tui {
            child,
            screen,
            _master: pair.master,
            writer,
        }
    }

    fn press(&mut self, key: &[u8]) {
        self.writer.write_all(key).expect("write key");
        self.writer.flush().expect("flush key");
    }

    /// Presses `M` then `l`, which opens the remotes view.
    fn open_remotes(&mut self) {
        self.press(b"M");
        std::thread::sleep(Duration::from_millis(100));
        self.press(b"l");
    }

    /// The screen rows as drawn by all output so far.
    fn rows(&self) -> Vec<String> {
        grid(&self.screen.lock().unwrap())
    }

    /// Whether a screen row passes `want` within `limit`.
    fn row_shows(&self, want: impl Fn(&str) -> bool, limit: Duration) -> bool {
        let end = Instant::now() + limit;
        loop {
            if self.rows().iter().any(|row| want(row)) {
                return true;
            }
            if Instant::now() >= end {
                return false;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// Whether a row starts with `name` and contains every one of `parts`.
    fn header_shows(&self, name: &str, parts: &[&str], limit: Duration) -> bool {
        self.row_shows(
            |row| {
                let lead = row.trim_start_matches(|c: char| !c.is_alphanumeric());
                lead.starts_with(name) && parts.iter().all(|p| row.contains(p))
            },
            limit,
        )
    }
}

impl Drop for Tui {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Replays terminal output onto a 40 by 120 grid and returns its rows.
fn grid(raw: &[u8]) -> Vec<String> {
    const ROWS: usize = 40;
    const COLS: usize = 120;
    let mut cells = vec![vec![' '; COLS]; ROWS];
    let (mut row, mut col) = (0usize, 0usize);
    let text = String::from_utf8_lossy(raw);
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\x1b' if chars.peek() == Some(&'[') => {
                chars.next();
                let mut args = String::new();
                let mut last = ' ';
                for c in chars.by_ref() {
                    if c.is_ascii_alphabetic() {
                        last = c;
                        break;
                    }
                    args.push(c);
                }
                let nums: Vec<usize> = args
                    .split(';')
                    .map(|n| n.trim_start_matches('?').parse().unwrap_or(0))
                    .collect();
                match last {
                    'H' | 'f' => {
                        row = nums[0].max(1) - 1;
                        col = nums.get(1).copied().unwrap_or(1).max(1) - 1;
                    }
                    'C' => col += nums[0].max(1),
                    'J' if nums[0] == 2 => cells = vec![vec![' '; COLS]; ROWS],
                    'K' if row < ROWS => {
                        let from = if nums[0] == 0 { col } else { 0 };
                        for cell in cells[row].iter_mut().skip(from) {
                            *cell = ' ';
                        }
                    }
                    _ => {}
                }
            }
            '\x1b' => {
                chars.next();
            }
            '\r' => col = 0,
            '\n' => row += 1,
            c if !c.is_control() => {
                if row < ROWS && col < COLS {
                    cells[row][col] = c;
                }
                col += 1;
            }
            _ => {}
        }
    }
    cells.into_iter().map(|r| r.into_iter().collect()).collect()
}

#[test]
fn tui_remotes_view_draws_within_1_second_with_unreachable_remotes() {
    let fx = fixture("draw");
    let mut tui = Tui::open(&fx.work, &fx.dir.join("state"));

    let started = Instant::now();
    tui.open_remotes();
    for name in ["origin", "dead", "hang"] {
        let left = Duration::from_secs(1).saturating_sub(started.elapsed());
        assert!(
            tui.header_shows(name, &[], left),
            "{name} not shown within 1s of the key press"
        );
    }

    let started = Instant::now();
    tui.press(b"a");
    assert!(
        tui.row_shows(|row| row.contains("Matrix"), Duration::from_secs(1)),
        "no matrix within 1s of a, waited {:?}",
        started.elapsed()
    );
}

#[test]
fn tui_remotes_hanging_server_is_marked_not_checked_after_the_limit() {
    let fx = fixture("hang");
    let mut tui = Tui::open(&fx.work, &fx.dir.join("state"));

    let started = Instant::now();
    tui.open_remotes();
    assert!(
        tui.header_shows("hang", &[], Duration::from_secs(1)),
        "hang not shown within 1s of the key press"
    );
    assert!(
        !tui.header_shows("hang", &["not checked"], Duration::from_secs(2)),
        "hang is marked not checked before the limit"
    );
    let pressed = Instant::now();
    tui.press(b"a");
    assert!(
        tui.row_shows(|row| row.contains("Matrix"), Duration::from_secs(1)),
        "no matrix within 1s of a during the wait, waited {:?}",
        pressed.elapsed()
    );
    tui.press(b"a");
    assert!(
        tui.header_shows("hang", &[], Duration::from_secs(1)),
        "the view did not come back after a"
    );

    let left = Duration::from_secs(15).saturating_sub(started.elapsed());
    assert!(
        tui.header_shows("hang", &["not checked"], left),
        "hang is not marked not checked within 15s, waited {:?}",
        started.elapsed()
    );
}

#[test]
fn tui_remotes_fetch_of_one_remote_shows_a_new_branch() {
    let fx = fixture("fetch");
    let mut tui = Tui::open(&fx.work, &fx.dir.join("state"));
    let other = fx.dir.join("other");
    git(
        &fx.dir,
        &[
            "clone",
            "-q",
            fx.server.to_str().unwrap(),
            other.to_str().unwrap(),
        ],
    );
    identify(&other);
    git(&other, &["checkout", "-q", "-b", "late"]);
    std::fs::write(other.join("h"), "c\n").expect("write");
    git(&other, &["add", "h"]);
    git(&other, &["commit", "-qm", "late work"]);
    git(&other, &["push", "-q", "origin", "late"]);

    tui.open_remotes();
    assert!(
        tui.header_shows("origin", &[], Duration::from_secs(5)),
        "the remotes view did not open"
    );
    assert!(
        tui.header_shows("late", &["not fetched"], Duration::from_secs(15)),
        "late is not shown as not fetched"
    );

    tui.press(b"/");
    tui.press(b"origin");
    tui.press(b"\r");
    std::thread::sleep(Duration::from_millis(300));
    let started = Instant::now();
    tui.press(b"f");
    assert!(
        tui.header_shows("late", &["late work"], Duration::from_secs(5)),
        "late shows no commit subject within 5s of f, waited {:?}",
        started.elapsed()
    );
}
