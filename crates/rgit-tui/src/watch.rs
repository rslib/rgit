#![cfg_attr(not(target_os = "linux"), allow(dead_code))]

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

use ignore::{WalkBuilder, WalkState};
use notify_debouncer_mini::notify::{self, RecursiveMode, Watcher};
use rgit_git::{GitBackend, RepoStatus};
use tokio::sync::mpsc::UnboundedSender;

use crate::app::Msg;

pub(crate) const GIT_STATE_WAIT: Duration = Duration::from_millis(150);
pub(crate) const GIT_STATE_MAX_WAIT: Duration = Duration::from_millis(300);
pub(crate) const WORKTREE_WAIT: Duration = Duration::from_secs(1);
pub(crate) const GIT_STATE_POLL: Duration = Duration::from_millis(100);
#[cfg(not(target_os = "linux"))]
const FSEVENTS_SETTLE: Duration = Duration::from_secs(3);

pub(crate) struct WatchRoots {
    pub(crate) workdir: PathBuf,
    pub(crate) git_dir: PathBuf,
    pub(crate) common_dir: PathBuf,
}

pub(crate) struct WatchBatch {
    pub(crate) git_state: bool,
    pub(crate) worktree: Vec<PathBuf>,
}

pub(crate) fn spawn_watcher(
    backend: Arc<dyn GitBackend>,
    msg_tx: UnboundedSender<Msg>,
    collapsed: HashSet<PathBuf>,
) -> WatchGuard {
    let (tx, rx) = mpsc::channel();
    let thread_tx = tx.clone();
    std::thread::spawn(move || {
        // git_dir and common_dir lock the repo, so read them off the UI thread.
        let roots = WatchRoots {
            workdir: backend.workdir().to_path_buf(),
            git_dir: backend.git_dir(),
            common_dir: backend.common_dir(),
        };
        run_watcher(backend, msg_tx, roots, collapsed, rx, thread_tx)
    });
    WatchGuard { tx }
}

pub(crate) struct WatchGuard {
    tx: mpsc::Sender<WatchMessage>,
}

impl Drop for WatchGuard {
    fn drop(&mut self) {
        let _ = self.tx.send(WatchMessage::Shutdown);
    }
}

enum WatchMessage {
    Event(notify::Event),
    WalkDone(PathBuf),
    Shutdown,
}

type SharedWatcher = Arc<Mutex<dyn Watcher + Send>>;

fn run_watcher(
    backend: Arc<dyn GitBackend>,
    msg_tx: UnboundedSender<Msg>,
    roots: WatchRoots,
    collapsed: HashSet<PathBuf>,
    rx: mpsc::Receiver<WatchMessage>,
    tx: mpsc::Sender<WatchMessage>,
) {
    // Stamp the git state before the watch setup, which can take seconds on
    // FSEvents: a stamp taken after it would hide a commit made meanwhile.
    let mut poll = GitStatePoll::new(&roots);
    let callback_tx = tx.clone();
    let watcher = notify::recommended_watcher(move |result: notify::Result<notify::Event>| {
        if let Ok(event) = result {
            let _ = callback_tx.send(WatchMessage::Event(event));
        }
    })
    .ok()
    .map(|watcher| Arc::new(Mutex::new(watcher)) as SharedWatcher);

    #[cfg(target_os = "linux")]
    let walks = watcher.as_ref().map(|watcher| {
        add_watch(watcher, &roots.workdir, RecursiveMode::NonRecursive);
        add_watch(watcher, &roots.git_dir, RecursiveMode::NonRecursive);
        add_watch(watcher, &roots.common_dir, RecursiveMode::NonRecursive);
        add_watch(
            watcher,
            &roots.common_dir.join("refs"),
            RecursiveMode::Recursive,
        );
        let walks = spawn_walker(
            Arc::clone(watcher),
            roots.workdir.clone(),
            collapsed.clone(),
            tx.clone(),
        );
        let _ = walks.send(roots.workdir.clone());
        walks
    });
    // FSEvents can take a second per watch, so set up on a side thread and
    // keep the git state poll running meanwhile. A new stream also drops
    // events for a moment after it starts, so look once at the end of the
    // setup and once more when the stream has settled.
    #[cfg(not(target_os = "linux"))]
    if let Some(watcher) = watcher.as_ref() {
        let _ = &collapsed;
        let watcher = Arc::clone(watcher);
        let (workdir, git_dir, common_dir) = (
            roots.workdir.clone(),
            roots.git_dir.clone(),
            roots.common_dir.clone(),
        );
        let done = tx.clone();
        std::thread::spawn(move || {
            let setup = Instant::now();
            add_watch(&watcher, &workdir, RecursiveMode::Recursive);
            if !git_dir.starts_with(&workdir) {
                add_watch(&watcher, &git_dir, RecursiveMode::NonRecursive);
            }
            if !common_dir.starts_with(&workdir) {
                add_watch(&watcher, &common_dir, RecursiveMode::NonRecursive);
                add_watch(&watcher, &common_dir.join("refs"), RecursiveMode::Recursive);
            }
            tracing::info!(
                elapsed_ms = setup.elapsed().as_millis() as u64,
                "file watch ready"
            );
            let _ = done.send(WatchMessage::WalkDone(workdir.clone()));
            std::thread::sleep(FSEVENTS_SETTLE);
            let _ = done.send(WatchMessage::WalkDone(workdir));
        });
    }

    let mut debounce = Debounce::default();
    let mut next_poll = Instant::now() + GIT_STATE_POLL;
    loop {
        let wake = debounce.next().map_or(next_poll, |at| at.min(next_poll));
        match rx.recv_timeout(wake.saturating_duration_since(Instant::now())) {
            Ok(WatchMessage::Shutdown) => return,
            // Files can change before a walk watches their folder, so refresh once it ends.
            Ok(WatchMessage::WalkDone(root)) => debounce.add(
                WatchBatch {
                    git_state: false,
                    worktree: vec![root],
                },
                Instant::now(),
            ),
            Ok(WatchMessage::Event(event)) => {
                #[cfg(target_os = "linux")]
                if let Some(walks) = &walks {
                    for path in new_folders(&event, &roots.workdir, &collapsed) {
                        let _ = walks.send(path);
                    }
                }
                debounce.add(sort_watch_paths(&roots, event.paths), Instant::now());
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => return,
        }
        let now = Instant::now();
        if now >= next_poll {
            if poll.changed() {
                debounce.add(
                    WatchBatch {
                        git_state: true,
                        worktree: Vec::new(),
                    },
                    now,
                );
            }
            next_poll = now + GIT_STATE_POLL;
        }
        while let Some(flush) = debounce.due(Instant::now()) {
            match flush {
                Flush::GitState => {
                    let _ = msg_tx.send(Msg::AutoRefresh);
                }
                Flush::Worktree(paths) => refresh_unless_ignored(&backend, &msg_tx, paths),
            }
        }
    }
}

fn add_watch(watcher: &SharedWatcher, path: &Path, mode: RecursiveMode) {
    let mut watcher = watcher.lock().unwrap_or_else(|poison| poison.into_inner());
    if let Err(error) = watcher.watch(path, mode) {
        tracing::warn!(path = %path.display(), error = %error, "file watch could not be added");
    }
}

/// Folders an event adds to the tree: a create or the new name of a rename.
fn new_folders(
    event: &notify::Event,
    workdir: &Path,
    collapsed: &HashSet<PathBuf>,
) -> Vec<PathBuf> {
    use notify::event::{EventKind, ModifyKind, RenameMode};
    let added = matches!(
        event.kind,
        EventKind::Create(_)
            | EventKind::Modify(ModifyKind::Name(
                RenameMode::To | RenameMode::Both | RenameMode::Any
            ))
    );
    if !added {
        return Vec::new();
    }
    let git = workdir.join(".git");
    event
        .paths
        .iter()
        .filter(|path| {
            path.starts_with(workdir)
                && !path.starts_with(&git)
                && !is_collapsed(path, collapsed)
                && path.is_dir()
        })
        .cloned()
        .collect()
}

/// One thread walks queued roots in turn, so a burst of creates runs one walk per subtree.
fn spawn_walker(
    watcher: SharedWatcher,
    workdir: PathBuf,
    collapsed: HashSet<PathBuf>,
    done: mpsc::Sender<WatchMessage>,
) -> mpsc::Sender<PathBuf> {
    let (walk_tx, walk_rx) = mpsc::channel::<PathBuf>();
    std::thread::spawn(move || {
        while let Ok(first) = walk_rx.recv() {
            let queued = std::iter::once(first).chain(walk_rx.try_iter()).collect();
            for root in outermost(queued) {
                walk_root(&root, &workdir, &collapsed, &|path| {
                    add_watch(&watcher, path, RecursiveMode::NonRecursive)
                });
                if done.send(WatchMessage::WalkDone(root)).is_err() {
                    return;
                }
            }
        }
    });
    walk_tx
}

fn outermost(paths: Vec<PathBuf>) -> Vec<PathBuf> {
    let mut kept: Vec<PathBuf> = Vec::new();
    for path in sort_paths(paths) {
        if !kept.last().is_some_and(|last| path.starts_with(last)) {
            kept.push(path);
        }
    }
    kept
}

/// Calls `visit` on each folder under `root` to watch, as the walk reaches it, so
/// only a folder made in the moment between its read and its visit goes unseen.
fn walk_root(
    root: &Path,
    workdir: &Path,
    collapsed: &HashSet<PathBuf>,
    visit: &(dyn Fn(&Path) + Sync),
) {
    if root == workdir {
        walk_folders(walk_builder(root), collapsed, visit);
    } else if let Some(builder) = created_walk(root, collapsed) {
        walk_folders(builder, collapsed, &|path| {
            if path.starts_with(root) {
                visit(path);
            }
        });
    }
}

#[cfg(test)]
fn collect_watch_folders_parallel(root: &Path, collapsed: &HashSet<PathBuf>) -> Vec<PathBuf> {
    let paths = Mutex::new(vec![root.to_path_buf()]);
    walk_folders(walk_builder(root), collapsed, &|path| {
        paths.lock().unwrap().push(path.to_path_buf())
    });
    sort_paths(paths.into_inner().unwrap())
}

#[cfg(test)]
fn collect_watch_folders_serial(root: &Path, collapsed: &HashSet<PathBuf>) -> Vec<PathBuf> {
    let inside = collapsed.clone();
    let mut builder = walk_builder(root);
    builder.filter_entry(move |entry| {
        entry.file_name() != ".git"
            && (inside.contains(entry.path()) || !is_collapsed(entry.path(), &inside))
    });
    let mut paths = vec![root.to_path_buf()];
    paths.extend(
        builder
            .build()
            .flatten()
            .filter(|entry| entry.file_type().is_some_and(|kind| kind.is_dir()))
            .map(|entry| entry.into_path()),
    );
    sort_paths(paths)
}

pub(crate) fn collapsed_untracked_dirs(workdir: &Path, status: &RepoStatus) -> HashSet<PathBuf> {
    status
        .entries
        .iter()
        .filter(|entry| entry.is_untracked() && entry.path.ends_with('/'))
        .map(|entry| workdir.join(entry.path.trim_end_matches('/')))
        .collect()
}

#[cfg(test)]
fn collect_created_watch_folders(root: &Path, collapsed: &HashSet<PathBuf>) -> Vec<PathBuf> {
    let paths = Mutex::new(Vec::new());
    let workdir = root.parent().unwrap_or(root);
    walk_root(root, workdir, collapsed, &|path| {
        paths.lock().unwrap().push(path.to_path_buf())
    });
    sort_paths(paths.into_inner().unwrap())
}

/// Walks from the repo top down to `root`, so the ignore rules of its parents apply.
fn created_walk(root: &Path, collapsed: &HashSet<PathBuf>) -> Option<WalkBuilder> {
    if !root.is_dir() || is_collapsed(root, collapsed) {
        return None;
    }
    let scan_root = root
        .ancestors()
        .find(|ancestor| ancestor.join(".git").exists())
        .or_else(|| root.parent())?
        .to_path_buf();
    let target = root.to_path_buf();
    let filter_root = scan_root.clone();
    let mut builder = walk_builder(&scan_root);
    builder.filter_entry(move |entry| {
        let path = entry.path();
        path.file_name() != Some(std::ffi::OsStr::new(".git"))
            && (path == filter_root || target.starts_with(path) || path.starts_with(&target))
    });
    Some(builder)
}

fn walk_builder(root: &Path) -> WalkBuilder {
    let mut builder = WalkBuilder::new(root);
    builder
        .git_ignore(true)
        .git_exclude(true)
        .git_global(true)
        .parents(true)
        .ignore(false)
        .hidden(false)
        .follow_links(false)
        .filter_entry(|entry| entry.file_name() != ".git");
    builder
}

fn walk_folders(
    builder: WalkBuilder,
    collapsed: &HashSet<PathBuf>,
    visit: &(dyn Fn(&Path) + Sync),
) {
    builder.build_parallel().run(|| {
        Box::new(move |result| {
            let Ok(entry) = result else {
                return WalkState::Continue;
            };
            if !entry.file_type().is_some_and(|kind| kind.is_dir()) {
                return WalkState::Continue;
            }
            visit(entry.path());
            if is_collapsed(entry.path(), collapsed) {
                WalkState::Skip
            } else {
                WalkState::Continue
            }
        })
    });
}

fn sort_paths(mut paths: Vec<PathBuf>) -> Vec<PathBuf> {
    paths.sort_unstable();
    paths.dedup();
    paths
}

fn is_collapsed(path: &Path, collapsed: &HashSet<PathBuf>) -> bool {
    collapsed
        .iter()
        .any(|collapsed| path == collapsed || path.starts_with(collapsed))
}

pub(crate) fn refresh_unless_ignored(
    backend: &Arc<dyn GitBackend>,
    msg_tx: &UnboundedSender<Msg>,
    paths: Vec<PathBuf>,
) {
    let backend = Arc::clone(backend);
    let msg_tx = msg_tx.clone();
    std::thread::spawn(move || {
        let paths: Vec<&Path> = paths.iter().map(|p| p.as_path()).collect();
        let flags = match backend.paths_ignored(&paths) {
            Ok(flags) => flags,
            Err(_) => {
                let _ = msg_tx.send(Msg::AutoRefresh);
                return;
            }
        };
        if flags.into_iter().any(|ignored| !ignored) {
            let _ = msg_tx.send(Msg::AutoRefresh);
        }
    });
}

pub(crate) type Stamp = Option<(std::time::SystemTime, u64, u64)>;

fn stamp(path: &Path) -> Stamp {
    let meta = std::fs::metadata(path).ok()?;
    #[cfg(unix)]
    let id = std::os::unix::fs::MetadataExt::ino(&meta);
    #[cfg(not(unix))]
    let id = 0;
    Some((meta.modified().ok()?, meta.len(), id))
}

pub(crate) struct GitStatePoll {
    paths: [PathBuf; 3],
    stamps: [Stamp; 3],
}

impl GitStatePoll {
    pub(crate) fn new(roots: &WatchRoots) -> Self {
        let paths = [
            roots.git_dir.join("index"),
            roots.git_dir.join("HEAD"),
            roots.git_dir.join("logs/HEAD"),
        ];
        let stamps = paths.each_ref().map(|p| stamp(p));
        GitStatePoll { paths, stamps }
    }

    pub(crate) fn changed(&mut self) -> bool {
        let stamps = self.paths.each_ref().map(|p| stamp(p));
        let changed = stamps != self.stamps;
        self.stamps = stamps;
        changed
    }
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Flush {
    GitState,
    Worktree(Vec<PathBuf>),
}

#[derive(Default)]
pub(crate) struct Debounce {
    git_state: Option<Instant>,
    git_first: Option<Instant>,
    worktree: Option<Instant>,
    paths: Vec<PathBuf>,
}

impl Debounce {
    pub(crate) fn add(&mut self, batch: WatchBatch, now: Instant) {
        if batch.git_state {
            let first = *self.git_first.get_or_insert(now);
            self.git_state = Some((now + GIT_STATE_WAIT).min(first + GIT_STATE_MAX_WAIT));
        }
        if !batch.worktree.is_empty() {
            self.worktree.get_or_insert(now + WORKTREE_WAIT);
            for path in batch.worktree {
                if !self.paths.contains(&path) {
                    self.paths.push(path);
                }
            }
        }
    }

    pub(crate) fn next(&self) -> Option<Instant> {
        match (self.git_state, self.worktree) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        }
    }

    pub(crate) fn due(&mut self, now: Instant) -> Option<Flush> {
        if self.git_state.is_some_and(|at| at <= now) {
            self.git_state = None;
            self.git_first = None;
            self.worktree = None;
            self.paths.clear();
            return Some(Flush::GitState);
        }
        if self.worktree.is_some_and(|at| at <= now) {
            self.worktree = None;
            return Some(Flush::Worktree(std::mem::take(&mut self.paths)));
        }
        None
    }
}

pub(crate) fn sort_watch_paths(
    roots: &WatchRoots,
    paths: impl IntoIterator<Item = PathBuf>,
) -> WatchBatch {
    let mut batch = WatchBatch {
        git_state: false,
        worktree: Vec::new(),
    };
    for path in paths {
        if is_git_state(roots, &path) {
            batch.git_state = true;
        } else if is_git_internal(roots, &path) {
            continue;
        } else {
            batch.worktree.push(path);
        }
    }
    batch
}

fn is_git_state(roots: &WatchRoots, path: &Path) -> bool {
    let own = path
        .strip_prefix(&roots.git_dir)
        .is_ok_and(|rest| rest == Path::new("index") || rest == Path::new("HEAD"));
    let shared = path
        .strip_prefix(&roots.common_dir)
        .is_ok_and(|rest| rest == Path::new("packed-refs") || rest.starts_with("refs"));
    own || shared
}

fn is_git_internal(roots: &WatchRoots, path: &Path) -> bool {
    path.starts_with(&roots.git_dir)
        || path.starts_with(&roots.common_dir)
        || path
            .strip_prefix(&roots.workdir)
            .is_ok_and(|rel| rel.starts_with(".git"))
}

#[cfg(test)]
mod tests {
    use super::{
        WatchMessage, collect_created_watch_folders, collect_watch_folders_parallel,
        collect_watch_folders_serial, new_folders, outermost, spawn_walker,
    };
    use notify_debouncer_mini::notify;
    use std::collections::HashSet;
    use std::path::{Path, PathBuf};
    use std::sync::{Arc, Mutex, mpsc};
    use std::time::{Duration, Instant};

    fn temp_root(tag: &str) -> PathBuf {
        let root =
            std::env::temp_dir().join(format!("rgit-tui-watch-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        root
    }

    fn make_dirs(root: &Path, prefix: &str, count: usize) {
        for n in 0..count {
            std::fs::create_dir_all(root.join(prefix).join(format!("d{n}"))).unwrap();
        }
    }

    #[test]
    fn watch_ignored_tree_is_not_selected() {
        let root = temp_root("ignored");
        std::fs::create_dir_all(root.join(".git")).unwrap();
        std::fs::write(root.join(".gitignore"), "build/\n").unwrap();
        make_dirs(&root, "build", 500);
        let paths = collect_watch_folders_parallel(&root, &HashSet::new());
        assert!(paths.contains(&root));
        assert!(
            paths
                .iter()
                .all(|path| !path.starts_with(root.join("build")))
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn watch_collapsed_untracked_folder_is_selected_only_at_top() {
        let root = temp_root("collapsed");
        make_dirs(&root, ".venv", 300);
        let collapsed = HashSet::from([root.join(".venv")]);
        let paths = collect_watch_folders_parallel(&root, &collapsed);
        assert!(paths.contains(&root.join(".venv")));
        assert!(
            paths.iter().all(|path| {
                *path == root.join(".venv") || !path.starts_with(root.join(".venv"))
            })
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn watch_honors_nested_ignore_and_never_selects_git() {
        let root = temp_root("rules");
        std::fs::create_dir_all(root.join(".git")).unwrap();
        std::fs::create_dir_all(root.join("src/cache/deep")).unwrap();
        std::fs::create_dir_all(root.join("src/keep")).unwrap();
        std::fs::write(root.join("src/.gitignore"), "cache/\n").unwrap();
        let paths = collect_watch_folders_parallel(&root, &HashSet::new());
        assert!(paths.contains(&root));
        assert!(paths.contains(&root.join("src")));
        assert!(paths.contains(&root.join("src/keep")));
        assert!(
            paths
                .iter()
                .all(|path| !path.starts_with(root.join(".git")))
        );
        assert!(
            paths
                .iter()
                .all(|path| !path.starts_with(root.join("src/cache")))
        );

        let empty = temp_root("empty");
        assert_eq!(
            collect_watch_folders_parallel(&empty, &HashSet::new()),
            vec![empty.clone()]
        );
        let _ = std::fs::remove_dir_all(root);
        let _ = std::fs::remove_dir_all(empty);
    }

    #[test]
    fn watch_parallel_walk_matches_serial_walk() {
        let root = temp_root("same");
        std::fs::write(root.join(".gitignore"), "ignored/\n").unwrap();
        make_dirs(&root, "src", 30);
        make_dirs(&root, "ignored", 30);
        let collapsed = HashSet::from([root.join("src/d0")]);
        let parallel = collect_watch_folders_parallel(&root, &collapsed);
        let serial = collect_watch_folders_serial(&root, &collapsed);
        assert_eq!(parallel, serial);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn watch_created_folder_adds_nonignored_subtree() {
        let root = temp_root("create");
        std::fs::create_dir_all(root.join("new/deep")).unwrap();
        let paths = collect_created_watch_folders(&root.join("new"), &HashSet::new());
        assert!(paths.contains(&root.join("new")));
        assert!(paths.contains(&root.join("new/deep")));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn watch_created_ignored_folder_file_and_collapsed_subtree_add_nothing() {
        let root = temp_root("create-filter");
        std::fs::create_dir_all(root.join(".git")).unwrap();
        std::fs::write(root.join(".gitignore"), "build/\n").unwrap();
        std::fs::create_dir_all(root.join("build/x/deep")).unwrap();
        std::fs::write(root.join("file"), "x").unwrap();
        std::fs::create_dir_all(root.join(".venv/x")).unwrap();
        let collapsed = HashSet::from([root.join(".venv")]);
        assert!(collect_created_watch_folders(&root.join("build"), &collapsed).is_empty());
        assert!(collect_created_watch_folders(&root.join("build/x"), &collapsed).is_empty());
        assert!(collect_created_watch_folders(&root.join("file"), &collapsed).is_empty());
        assert!(collect_created_watch_folders(&root.join(".venv/x"), &collapsed).is_empty());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn watch_skips_nothing_for_a_dot_ignore_file() {
        let root = temp_root("dot-ignore");
        std::fs::create_dir_all(root.join(".git")).unwrap();
        std::fs::write(root.join(".ignore"), "keep/\n").unwrap();
        std::fs::create_dir_all(root.join("keep/deep")).unwrap();
        let paths = collect_watch_folders_parallel(&root, &HashSet::new());
        assert!(paths.contains(&root.join("keep/deep")));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn watch_new_folders_come_from_create_and_rename_to() {
        use notify::event::{CreateKind, EventKind, ModifyKind, RemoveKind, RenameMode};
        let root = temp_root("events");
        std::fs::create_dir_all(root.join(".git/refs")).unwrap();
        std::fs::create_dir_all(root.join("moved")).unwrap();
        std::fs::create_dir_all(root.join(".venv/lib")).unwrap();
        std::fs::write(root.join("file"), "x").unwrap();
        let collapsed = HashSet::from([root.join(".venv")]);
        let event = |kind, paths: &[&str]| {
            paths.iter().fold(notify::Event::new(kind), |event, path| {
                event.add_path(root.join(path))
            })
        };
        let found = |event: notify::Event| new_folders(&event, &root, &collapsed);
        let moved = vec![root.join("moved")];
        assert_eq!(
            found(event(EventKind::Create(CreateKind::Folder), &["moved"])),
            moved
        );
        assert_eq!(
            found(event(
                EventKind::Modify(ModifyKind::Name(RenameMode::To)),
                &["moved"]
            )),
            moved
        );
        assert_eq!(
            found(event(
                EventKind::Modify(ModifyKind::Name(RenameMode::Both)),
                &["gone", "moved"]
            )),
            moved
        );
        assert!(
            found(event(
                EventKind::Modify(ModifyKind::Name(RenameMode::From)),
                &["moved"]
            ))
            .is_empty()
        );
        assert!(found(event(EventKind::Remove(RemoveKind::Folder), &["moved"])).is_empty());
        assert!(found(event(EventKind::Create(CreateKind::Any), &["file", "gone"])).is_empty());
        assert!(
            found(event(
                EventKind::Create(CreateKind::Any),
                &[".git/refs", ".venv/lib"]
            ))
            .is_empty()
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn watch_outermost_drops_roots_inside_other_roots() {
        let paths = ["a/b", "a", "c", "a/b/c", "ab"].map(PathBuf::from).to_vec();
        assert_eq!(
            outermost(paths),
            ["a", "ab", "c"].map(PathBuf::from).to_vec()
        );
    }

    /// inotify on Linux. Elsewhere a poll watch, as FSEvents takes about 1 s per watch call.
    fn walk_with_real_watch(root: &Path) -> (mpsc::Sender<PathBuf>, mpsc::Receiver<WatchMessage>) {
        let (tx, rx) = mpsc::channel();
        let callback_tx = tx.clone();
        let handler = move |result: notify::Result<notify::Event>| {
            if let Ok(event) = result {
                let _ = callback_tx.send(WatchMessage::Event(event));
            }
        };
        #[cfg(target_os = "linux")]
        let watcher = notify::recommended_watcher(handler).unwrap();
        #[cfg(not(target_os = "linux"))]
        let watcher = notify::PollWatcher::new(
            handler,
            notify::Config::default().with_poll_interval(Duration::from_millis(50)),
        )
        .unwrap();
        let walks = spawn_walker(
            Arc::new(Mutex::new(watcher)),
            root.to_path_buf(),
            HashSet::new(),
            tx,
        );
        (walks, rx)
    }

    fn wait_walk_done(rx: &mpsc::Receiver<WatchMessage>, root: &Path) {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            match rx.recv_timeout(left) {
                Ok(WatchMessage::WalkDone(done)) if done == root => return,
                Ok(_) => {}
                Err(error) => panic!("walk of {} did not end: {error}", root.display()),
            }
        }
    }

    #[test]
    fn watch_walker_returns_at_once_and_reports_each_walk() {
        let root = temp_root("walker");
        make_dirs(&root, "src", 50);
        let started = Instant::now();
        let (walks, rx) = walk_with_real_watch(&root);
        walks.send(root.clone()).unwrap();
        assert!(started.elapsed() < Duration::from_millis(100));
        wait_walk_done(&rx, &root);
        let _ = std::fs::remove_dir_all(root);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn watch_folder_made_again_or_renamed_stays_watched() {
        let root = temp_root("inotify");
        std::fs::create_dir_all(root.join(".git")).unwrap();
        let (walks, rx) = walk_with_real_watch(&root);
        walks.send(root.clone()).unwrap();
        wait_walk_done(&rx, &root);

        let expect_event = |file: PathBuf| {
            std::fs::write(&file, "x").unwrap();
            let deadline = Instant::now() + Duration::from_secs(10);
            loop {
                let left = deadline.saturating_duration_since(Instant::now());
                match rx.recv_timeout(left) {
                    Ok(WatchMessage::Event(event)) => {
                        for path in new_folders(&event, &root, &HashSet::new()) {
                            walks.send(path).unwrap();
                        }
                        if event.paths.contains(&file) {
                            return;
                        }
                    }
                    Ok(_) => {}
                    Err(_) => panic!("no event for {}", file.display()),
                }
            }
        };
        let settle = |dir: &Path| {
            let deadline = Instant::now() + Duration::from_secs(10);
            while Instant::now() < deadline {
                match rx.recv_timeout(Duration::from_millis(200)) {
                    Ok(WatchMessage::Event(event)) => {
                        for path in new_folders(&event, &root, &HashSet::new()) {
                            walks.send(path).unwrap();
                        }
                    }
                    Ok(WatchMessage::WalkDone(done)) if dir.starts_with(&done) => return,
                    Ok(_) => {}
                    Err(_) => {}
                }
            }
            panic!("{} was never walked", dir.display());
        };

        std::fs::create_dir_all(root.join("a/b")).unwrap();
        settle(&root.join("a"));
        expect_event(root.join("a/b/one"));

        std::fs::remove_dir_all(root.join("a")).unwrap();
        std::fs::create_dir_all(root.join("a/b")).unwrap();
        settle(&root.join("a"));
        expect_event(root.join("a/b/two"));

        std::fs::rename(root.join("a"), root.join("c")).unwrap();
        settle(&root.join("c"));
        expect_event(root.join("c/b/three"));
        let _ = std::fs::remove_dir_all(root);
    }
}
