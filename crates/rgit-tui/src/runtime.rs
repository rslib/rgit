use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use notify_debouncer_mini::notify::{RecommendedWatcher, RecursiveMode};
use notify_debouncer_mini::{DebounceEventResult, Debouncer, new_debouncer};
use ratatui::DefaultTerminal;
use rgit_git::{GitBackend, GitError, RepoStatus};
use tokio::sync::mpsc::UnboundedSender;

use crate::app::{App, Effect, InfoKind, LaneOp, Leader, Msg, Mutation, TextOp, update};
use crate::events::{Event, Events};
use crate::keymap::{
    self, resolve_commit_key, resolve_confirm_key, resolve_help_key, resolve_key,
    resolve_palette_key, resolve_prompt_key, resolve_rebase_key, resolve_search_key,
    resolve_transient_key,
};
use crate::ui;

/// Route a key while the vim-profile leader (which-key) overlay is open. Mutates
/// the leader level directly and returns the resolved action, if any.
fn resolve_leader(app: &mut App, key: crossterm::event::KeyEvent) -> Option<Msg> {
    use crossterm::event::KeyCode;
    match app.leader? {
        Leader::Root => match key.code {
            KeyCode::Esc | KeyCode::Char(' ') => {
                app.leader = None;
                None
            }
            KeyCode::Char('w') => {
                app.leader = Some(Leader::Window);
                None
            }
            KeyCode::Char(c) => {
                app.leader = None;
                keymap::leader_command(c)
            }
            _ => {
                app.leader = None;
                None
            }
        },
        Leader::Window => {
            app.leader = None;
            match key.code {
                KeyCode::Char('l') | KeyCode::Char('L') => {
                    if app.preview_visible {
                        Some(Msg::FocusPreview)
                    } else {
                        // The preview only exists over a file or commit; say so
                        // rather than doing nothing.
                        app.push_toast(
                            crate::app::ToastKind::Error,
                            "no preview here - move onto a file or commit".into(),
                        );
                        None
                    }
                }
                KeyCode::Char('h') | KeyCode::Char('H') => Some(Msg::FocusNav),
                _ => None,
            }
        }
    }
}

/// Route a key while the hook console is open: scroll always; dismiss or retry
/// only once the run has finished, so a running commit is not abandoned.
fn resolve_hook_key(app: &App, key: crossterm::event::KeyEvent) -> Option<Msg> {
    use crate::app::HookStatus;
    use crossterm::event::KeyCode;
    let running = app
        .hook_console
        .as_ref()
        .is_some_and(|c| c.status == HookStatus::Running);
    match key.code {
        KeyCode::Char('j') | KeyCode::Down => Some(Msg::HookScroll(1)),
        KeyCode::Char('k') | KeyCode::Up => Some(Msg::HookScroll(-1)),
        KeyCode::Char('e') if !running => Some(Msg::HookConsoleRetry),
        KeyCode::Char('q') | KeyCode::Esc if !running => Some(Msg::HookConsoleClose),
        _ => None,
    }
}

/// Route a key in the session-log view: `f` cycles the severity filter; every
/// other key uses the active profile's normal navigation (j/k, `/` search, `q`
/// to close, `g` to refresh).
fn resolve_session_log_key(key: crossterm::event::KeyEvent) -> Option<Msg> {
    use crossterm::event::{KeyCode, KeyModifiers};
    if !key.modifiers.contains(KeyModifiers::CONTROL) {
        // View-local keys, bare in both profiles.
        match key.code {
            KeyCode::Char('f') => return Some(Msg::CycleLogFilter),
            KeyCode::Char('g') => return Some(Msg::Refresh),
            _ => {}
        }
    }
    if keymap::profile() == keymap::Profile::Vim {
        keymap::resolve_vim_key(key)
    } else {
        resolve_key(key)
    }
}

/// Keys in the lanes view: `n` new lane, `a` assign the file at the cursor, `u`
/// unassign it, `c` commit the lane at the cursor. Everything else (nav, fold,
/// quit) falls through to the active profile.
fn resolve_lanes_key(key: crossterm::event::KeyEvent) -> Option<Msg> {
    use crossterm::event::{KeyCode, KeyModifiers};
    if !key.modifiers.contains(KeyModifiers::CONTROL) {
        match key.code {
            KeyCode::Char('n') => return Some(Msg::LaneNewPrompt),
            KeyCode::Char('a') => return Some(Msg::LaneAssignPrompt),
            KeyCode::Char('u') => return Some(Msg::LaneUnassignAtCursor),
            KeyCode::Char('c') => return Some(Msg::LaneCommitPrompt),
            KeyCode::Char('R') => return Some(Msg::LaneRenamePrompt),
            KeyCode::Char('s') => return Some(Msg::LaneStackPrompt),
            KeyCode::Char('S') => return Some(Msg::LaneRestack),
            KeyCode::Char('d') => return Some(Msg::LaneDeleteAtCursor),
            KeyCode::Char('p') => return Some(Msg::LanePushAtCursor),
            KeyCode::Char('P') => return Some(Msg::LanePrAtCursor),
            _ => {}
        }
    }
    if keymap::profile() == keymap::Profile::Vim {
        keymap::resolve_vim_key(key)
    } else {
        resolve_key(key)
    }
}

/// A credential prompt that runs on the network task's thread but drives the UI:
/// it asks the main loop (over `tx`) to open a masked minibuffer and blocks on
/// the reply the user submits. Returning `None` (cancel) fails the operation.
struct TuiCredentialPrompt {
    tx: UnboundedSender<Msg>,
}

impl TuiCredentialPrompt {
    fn ask(&self, label: String, masked: bool) -> Option<String> {
        let (reply, rx) = std::sync::mpsc::channel();
        self.tx
            .send(Msg::CredentialRequest {
                label,
                masked,
                reply,
            })
            .ok()?;
        rx.recv().ok().flatten()
    }
}

impl rgit_git::CredentialPrompt for TuiCredentialPrompt {
    fn username(&self, url: &str) -> Option<String> {
        self.ask(format!("Username for {url}"), false)
    }
    fn password(&self, url: &str, user: &str) -> Option<String> {
        self.ask(format!("Password for {user}@{url}"), true)
    }
    fn ssh_passphrase(&self, key: &str) -> Option<String> {
        self.ask(format!("Passphrase for {key}"), true)
    }
}

/// Run one lane operation and describe the result for a toast.
fn run_lane_op(backend: &dyn GitBackend, op: &LaneOp) -> String {
    let result = match op {
        LaneOp::New(name) => backend.lane_new(name).map(|()| format!("created lane {name}")),
        LaneOp::Assign { lane, path } => {
            backend.lane_assign(lane, path).map(|()| format!("{path} -> {lane}"))
        }
        LaneOp::Unassign(path) => backend.lane_unassign(path).map(|()| format!("{path} -> default")),
        LaneOp::Commit { lane, message } => backend.lane_commit(lane, message),
        LaneOp::Rename { old, new } => {
            backend.lane_rename(old, new).map(|()| format!("{old} -> {new}"))
        }
        LaneOp::Delete(name) => backend.lane_delete(name).map(|()| format!("deleted lane {name}")),
        LaneOp::Push(lane) => backend.lane_push(lane),
        LaneOp::Pr(lane) => backend.lane_pr(lane),
        LaneOp::Stack { name, parent } => backend
            .lane_stack(name, parent)
            .map(|()| format!("created {name} stacked on {parent}")),
        LaneOp::Restack => backend
            .lane_restack()
            .map(|o| restack_note(&o).unwrap_or_else(|| "nothing to restack".to_owned())),
    };
    match result {
        Ok(s) => s,
        Err(e) => format!("error: {e}"),
    }
}

const TICK_HZ: f64 = 4.0;
const FRAME_HZ: f64 = 30.0;

/// Run the TUI to completion, restoring the terminal on exit.
pub async fn run(backend: Arc<dyn GitBackend>) -> std::io::Result<()> {
    // Load config before touching the terminal so the theme is in place for the
    // very first frame; a config error is shown in-app, not fatal.
    let (config, config_error) = crate::config::load();
    crate::theme::init(crate::theme::Theme::from_config(&config.theme));
    crate::keymap::init(&config.keys, config.profile.as_deref());
    rgit_model::set_side_by_side(config.ui.side_by_side);
    rgit_model::set_glyph_mode(config.ui.glyph_mode());
    rgit_model::set_syntax_colors(crate::theme::syntax_colors());

    let mouse = config.ui.mouse;
    let mut terminal = ratatui::init();
    if mouse {
        let _ = crossterm::execute!(std::io::stdout(), crossterm::event::EnableMouseCapture);
    }
    install_panic_hook(mouse);
    let result = event_loop(&mut terminal, backend, config, config_error).await;
    if mouse {
        let _ = crossterm::execute!(std::io::stdout(), crossterm::event::DisableMouseCapture);
    }
    ratatui::restore();
    result
}

/// Restore the terminal on panic before the default hook prints the message, so
/// a crash leaves a usable terminal instead of a garbled raw-mode screen; the
/// panic is also logged for later diagnosis.
fn install_panic_hook(mouse: bool) {
    let default = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        if mouse {
            let _ = crossterm::execute!(std::io::stdout(), crossterm::event::DisableMouseCapture);
        }
        ratatui::restore();
        tracing::error!("panic: {info}");
        default(info);
    }));
}

/// One source of work for the loop: a terminal/timer event or an async message.
enum Incoming {
    Event(Event),
    Msg(Msg),
}

async fn event_loop(
    terminal: &mut DefaultTerminal,
    backend: Arc<dyn GitBackend>,
    config: crate::config::Config,
    config_error: Option<String>,
) -> std::io::Result<()> {
    let mut app = App::new(backend, config);
    app.error = config_error;
    let mut events = Events::new(TICK_HZ, FRAME_HZ);
    let (msg_tx, mut msg_rx) = tokio::sync::mpsc::unbounded_channel::<Msg>();

    // Let network operations prompt for a password/passphrase through the UI.
    app.backend()
        .set_credential_prompt(Box::new(TuiCredentialPrompt {
            tx: msg_tx.clone(),
        }));

    // Auto-refresh on worktree changes; kept alive for the loop's duration.
    let _watcher = spawn_watcher(app.backend().workdir(), msg_tx.clone());

    msg_tx.send(Msg::Refresh).ok();
    // Draw once up front; thereafter only when something actually changes, so an
    // idle TUI does no work between the periodic animation ticks.
    terminal.draw(|frame| ui::render(frame, &mut app))?;

    loop {
        // The select borrows `events` only until it resolves, freeing it (and
        // the terminal) for effect handling below.
        let incoming = tokio::select! {
            Some(event) = events.next() => Incoming::Event(event),
            Some(msg) = msg_rx.recv() => Incoming::Msg(msg),
        };

        let redraw = match incoming {
            Incoming::Event(Event::Resize) => true,
            Incoming::Event(Event::Key(key)) => {
                let msg = if app.prompt.is_some() {
                    // A minibuffer prompt is modal input; it takes priority even
                    // over the operation console (a credential prompt opens while
                    // a push/pull is running in the console).
                    resolve_prompt_key(key)
                } else if app.hook_console.is_some() {
                    resolve_hook_key(&app, key)
                } else if app.help {
                    resolve_help_key(key)
                } else if app.rebase_todo.is_some() {
                    resolve_rebase_key(key)
                } else if app.commit_editor.is_some() {
                    resolve_commit_key(key)
                } else if app.palette.is_some() {
                    resolve_palette_key(key)
                } else if app.search.is_some() {
                    resolve_search_key(key)
                } else if app.transient.is_some() {
                    resolve_transient_key(key)
                } else if app.confirm.is_some() {
                    resolve_confirm_key(key)
                } else if app.leader.is_some() {
                    resolve_leader(&mut app, key)
                } else if app.active_kind() == crate::app::ViewKind::SessionLog {
                    resolve_session_log_key(key)
                } else if app.active_kind() == crate::app::ViewKind::Lanes {
                    resolve_lanes_key(key)
                } else if keymap::profile() == keymap::Profile::Vim {
                    keymap::resolve_vim_key(key)
                } else {
                    resolve_key(key)
                };
                if let Some(msg) = msg {
                    run_msg(&mut app, msg, &mut events, terminal, &msg_tx).await?;
                }
                true
            }
            Incoming::Event(Event::Mouse(m)) => {
                if let Some(msg) = mouse_msg(&app, m) {
                    run_msg(&mut app, msg, &mut events, terminal, &msg_tx).await?;
                }
                true
            }
            // Redraw a tick only while something animates (and for the one final
            // tick that clears the last spinner/toast).
            Incoming::Event(Event::Tick) => {
                let was = app.is_animating();
                app.tick();
                was || app.is_animating()
            }
            Incoming::Event(Event::Error) => false,
            Incoming::Msg(msg) => {
                run_msg(&mut app, msg, &mut events, terminal, &msg_tx).await?;
                true
            }
        };

        if redraw {
            terminal.draw(|frame| ui::render(frame, &mut app))?;
        }
        if app.should_quit {
            return Ok(());
        }
    }
}

/// Translate a mouse event into a message. The wheel scrolls whichever pane the
/// pointer is over (without changing focus); a left click moves the cursor there
/// and focuses that pane. Both are ignored while an overlay is open.
fn mouse_msg(app: &App, m: crossterm::event::MouseEvent) -> Option<Msg> {
    use crossterm::event::{MouseButton, MouseEventKind};
    // Rows moved per wheel notch.
    const WHEEL: isize = 3;
    // The preview owns the columns at or past the split boundary.
    let over_preview = app.split_x.is_some_and(|x| m.column >= x);
    match m.kind {
        MouseEventKind::ScrollDown if over_preview => Some(Msg::ScrollPreview(WHEEL)),
        MouseEventKind::ScrollUp if over_preview => Some(Msg::ScrollPreview(-WHEEL)),
        MouseEventKind::ScrollDown => Some(Msg::Scroll(WHEEL)),
        MouseEventKind::ScrollUp => Some(Msg::Scroll(-WHEEL)),
        MouseEventKind::Down(MouseButton::Left) if !overlay_active(app) => {
            // Map the click to a row within the pane it landed in, accounting for
            // the frame border and each pane's label header.
            if over_preview {
                let offset = m.row.checked_sub(app.preview_top)? as usize;
                Some(Msg::ClickPreview(offset))
            } else {
                let offset = m.row.checked_sub(app.body_top)? as usize;
                Some(Msg::ClickRow(offset))
            }
        }
        _ => None,
    }
}

fn overlay_active(app: &App) -> bool {
    app.help
        || app.palette.is_some()
        || app.prompt.is_some()
        || app.transient.is_some()
        || app.confirm.is_some()
        || app.search.is_some()
        || app.commit_editor.is_some()
        || app.rebase_todo.is_some()
        || app.leader.is_some()
}

async fn run_msg(
    app: &mut App,
    msg: Msg,
    events: &mut Events,
    terminal: &mut DefaultTerminal,
    msg_tx: &UnboundedSender<Msg>,
) -> std::io::Result<()> {
    for effect in update(app, msg) {
        match effect {
            Effect::Refresh => spawn_read(app, msg_tx, |b| b.status()),
            Effect::CopyToClipboard(text) => copy_to_clipboard(&text),
            Effect::Mutate(mutation) => {
                // These all rewrite HEAD (amend, reword, squash, uncommit), so
                // they auto-restack the stacked children too (when enabled);
                // other mutations do not.
                let restack_after = app.auto_restack()
                    && matches!(
                        mutation,
                        crate::app::Mutation::Extend
                            | crate::app::Mutation::Reword { .. }
                            | crate::app::Mutation::Squash(_)
                            | crate::app::Mutation::Uncommit(_)
                    );
                let backend = app.backend();
                let tx = msg_tx.clone();
                tokio::task::spawn_blocking(move || {
                    let status = (|| -> Result<RepoStatus, GitError> {
                        apply_mutation(&*backend, &mutation)?;
                        if restack_after {
                            if let Some(note) = auto_restack_note(&*backend)? {
                                let _ = tx.send(Msg::AutoRestackNote(note));
                            }
                        }
                        backend.status()
                    })();
                    let _ = tx.send(Msg::Refreshed(Box::new(status)));
                });
            }
            Effect::Commit { amend } => commit_flow(app, events, terminal, msg_tx, amend).await?,
            Effect::OpenCommitEditor { amend } => {
                let prefill = if amend {
                    let backend = app.backend();
                    tokio::task::spawn_blocking(move || backend.head_message())
                        .await
                        .ok()
                        .flatten()
                        .unwrap_or_default()
                } else {
                    String::new()
                };
                let _ = msg_tx.send(Msg::ShowCommitEditor { amend, prefill });
            }
            Effect::GenerateCommit => {
                let backend = app.backend();
                let msg_tx = msg_tx.clone();
                tokio::spawn(async move {
                    let patch = tokio::task::spawn_blocking(move || backend.staged_patch())
                        .await
                        .ok()
                        .and_then(|r| r.ok())
                        .unwrap_or_default();
                    let out = if patch.trim().is_empty() {
                        Msg::Error("nothing staged to summarize".into())
                    } else {
                        match crate::ai::commit_message(&patch).await {
                            Ok(text) => Msg::SetCommitEditorText(text),
                            Err(e) => Msg::Error(format!("ai: {e}")),
                        }
                    };
                    let _ = msg_tx.send(out);
                });
            }
            Effect::ReviewStaged => {
                let backend = app.backend();
                let msg_tx = msg_tx.clone();
                tokio::spawn(async move {
                    let patch = tokio::task::spawn_blocking(move || backend.staged_patch())
                        .await
                        .ok()
                        .and_then(|r| r.ok())
                        .unwrap_or_default();
                    let out = if patch.trim().is_empty() {
                        Msg::Error("nothing staged to review".into())
                    } else {
                        match crate::ai::code_review(&patch).await {
                            Ok(text) => Msg::AiReviewLoaded(text),
                            Err(e) => Msg::Error(format!("ai: {e}")),
                        }
                    };
                    let _ = msg_tx.send(out);
                });
            }
            Effect::CommitConsole { amend, message } => {
                let backend = app.backend();
                let msg_tx = msg_tx.clone();
                tokio::spawn(commit_console(backend, amend, message, msg_tx));
            }
            Effect::OpConsole(op) => {
                let backend = app.backend();
                let msg_tx = msg_tx.clone();
                tokio::spawn(op_console(backend, op, msg_tx));
            }
            Effect::RunGit { args, label } => {
                git_console(app, events, terminal, msg_tx, args, label).await?
            }
            Effect::LoadRebaseTodo { base } => {
                let backend = app.backend();
                let msg_tx = msg_tx.clone();
                tokio::task::spawn_blocking(move || match backend.commits_between(&base) {
                    Ok(entries) if !entries.is_empty() => {
                        let _ = msg_tx.send(Msg::ShowRebaseTodo { base, entries });
                    }
                    Ok(_) => {
                        let _ = msg_tx.send(Msg::Error("no commits to rebase".into()));
                    }
                    Err(e) => {
                        let _ = msg_tx.send(Msg::Error(e.to_string()));
                    }
                });
            }
            Effect::RunRebaseTodo { base, todo } => {
                // Hand git our edited todo by pointing its sequence editor at a
                // copy command; GIT_EDITOR is left alone so squash/reword still
                // open the message editor in the suspended terminal.
                let dir = std::env::temp_dir();
                let path = dir.join(format!("rgit-rebase-todo-{}", std::process::id()));
                if std::fs::write(&path, todo).is_err() {
                    app.busy = None;
                    app.error = Some("could not write rebase todo".into());
                } else {
                    let env = vec![(
                        "GIT_SEQUENCE_EDITOR".to_owned(),
                        format!("cp {}", path.display()),
                    )];
                    let args = vec!["rebase".into(), "-i".into(), base];
                    git_console_env(app, events, terminal, msg_tx, args, "rebasing".into(), env)
                        .await?;
                    let _ = std::fs::remove_file(&path);
                }
            }
            Effect::LoadBranches => {
                let backend = app.backend();
                let msg_tx = msg_tx.clone();
                tokio::task::spawn_blocking(move || {
                    if let Ok(branches) = backend.local_branches() {
                        let _ = msg_tx.send(Msg::BranchesLoaded(branches));
                    }
                });
            }
            Effect::LoadLog(opts) => {
                let backend = app.backend();
                let msg_tx = msg_tx.clone();
                tokio::task::spawn_blocking(move || {
                    if let Ok(entries) = backend.log(&opts) {
                        let _ = msg_tx.send(Msg::LogLoaded(entries));
                    }
                });
            }
            Effect::LoadCommit(rev) => {
                let backend = app.backend();
                let msg_tx = msg_tx.clone();
                tokio::task::spawn_blocking(move || {
                    if let Ok(details) = backend.commit_details(&rev) {
                        let _ = msg_tx.send(Msg::CommitLoaded(Box::new(details)));
                    }
                });
            }
            Effect::LoadPreview { key, rev } => {
                let backend = app.backend();
                let msg_tx = msg_tx.clone();
                // Fetch and render the diff entirely off the main thread.
                tokio::task::spawn_blocking(move || {
                    if let Ok(details) = backend.commit_details(&rev) {
                        let sections = rgit_model::build_commit(&details);
                        let _ = msg_tx.send(Msg::PreviewBuilt { key, sections });
                    }
                });
            }
            Effect::BuildFilePreview { key, diff } => {
                let msg_tx = msg_tx.clone();
                tokio::task::spawn_blocking(move || {
                    let sections = rgit_model::build_diff(&diff.path, std::slice::from_ref(&diff));
                    let _ = msg_tx.send(Msg::PreviewBuilt { key, sections });
                });
            }
            Effect::LoadBlame(path) => {
                let backend = app.backend();
                let msg_tx = msg_tx.clone();
                tokio::task::spawn_blocking(move || {
                    if let Ok(lines) = backend.blame(&path) {
                        let _ = msg_tx.send(Msg::BlameLoaded(lines));
                    }
                });
            }
            Effect::LoadRefs => {
                let backend = app.backend();
                let msg_tx = msg_tx.clone();
                tokio::task::spawn_blocking(move || {
                    if let Ok(refs) = backend.refs() {
                        let _ = msg_tx.send(Msg::RefsLoaded(refs));
                    }
                });
            }
            Effect::LoadRemotes => {
                let backend = app.backend();
                let msg_tx = msg_tx.clone();
                tokio::task::spawn_blocking(move || {
                    if let Ok(remotes) = backend.remotes() {
                        let _ = msg_tx.send(Msg::RemotesLoaded(remotes));
                    }
                });
            }
            Effect::LoadPushRemotes => {
                let backend = app.backend();
                let msg_tx = msg_tx.clone();
                tokio::task::spawn_blocking(move || {
                    if let Ok(remotes) = backend.remotes() {
                        let _ = msg_tx.send(Msg::PushRemotesLoaded(remotes));
                    }
                });
            }
            Effect::LoadSmartlog => {
                let backend = app.backend();
                let msg_tx = msg_tx.clone();
                tokio::task::spawn_blocking(move || {
                    if let Ok(entries) = backend.smartlog() {
                        let _ = msg_tx.send(Msg::SmartlogLoaded(entries));
                    }
                });
            }
            Effect::LoadOplog => {
                let backend = app.backend();
                let msg_tx = msg_tx.clone();
                tokio::task::spawn_blocking(move || {
                    if let Ok(entries) = backend.oplog() {
                        let _ = msg_tx.send(Msg::OplogLoaded(entries));
                    }
                });
            }
            Effect::LoadStack => {
                let backend = app.backend();
                let msg_tx = msg_tx.clone();
                tokio::task::spawn_blocking(move || {
                    if let Ok(parents) = backend.stack_parents() {
                        let current = backend.status().ok().and_then(|s| s.head.branch);
                        let _ = msg_tx.send(Msg::StackLoaded { parents, current });
                    }
                });
            }
            Effect::LoadLanes => {
                let backend = app.backend();
                let msg_tx = msg_tx.clone();
                tokio::task::spawn_blocking(move || {
                    if !backend.lanes_active() {
                        let _ = msg_tx.send(Msg::LaneNotice(
                            "lanes are off; run `rgit lanes init` first".into(),
                        ));
                        return;
                    }
                    match backend.lanes_state() {
                        Ok(state) => {
                            let _ = msg_tx.send(Msg::LanesLoaded(state));
                        }
                        Err(e) => {
                            let _ = msg_tx.send(Msg::LaneNotice(format!("error: {e}")));
                        }
                    }
                });
            }
            Effect::LaneOp(op) => {
                let backend = app.backend();
                let auto_restack = app.auto_restack();
                let msg_tx = msg_tx.clone();
                tokio::task::spawn_blocking(move || {
                    let _ = msg_tx.send(Msg::LaneNotice(run_lane_op(&*backend, &op)));
                    // After committing a lane, move any child lanes onto its new
                    // tip. lane_restack works in the odb, so it is safe with the
                    // dirty worktree lanes keep (unlike the checkout-based restack).
                    if auto_restack && matches!(op, LaneOp::Commit { .. }) {
                        if let Ok(Some(note)) = backend.lane_restack().map(|o| restack_note(&o)) {
                            let _ = msg_tx.send(Msg::LaneNotice(note));
                        }
                    }
                    if let Ok(state) = backend.lanes_state() {
                        let _ = msg_tx.send(Msg::LanesLoaded(state));
                    }
                });
            }
            Effect::UndoTimes(n) => {
                spawn_read(app, msg_tx, move |b| {
                    for _ in 0..n {
                        b.undo()?;
                    }
                    b.status()
                });
            }
            Effect::RunText(op) => {
                let backend = app.backend();
                let msg_tx = msg_tx.clone();
                tokio::task::spawn_blocking(move || {
                    let result = match op {
                        TextOp::FlowInit(preset) => {
                            rgit_git::workflow::init(backend.as_ref(), &preset)
                        }
                        TextOp::FlowStart(name) => {
                            rgit_git::workflow::start(backend.as_ref(), &name)
                        }
                        TextOp::FlowFinish => rgit_git::workflow::finish(backend.as_ref()),
                        TextOp::WorkspaceNew(name) => {
                            rgit_git::workspace::create(backend.as_ref(), &name)
                        }
                        TextOp::StackNew(name) => backend.stack_new(&name),
                    };
                    let _ = msg_tx.send(Msg::TextResult(result.map_err(|e| e.to_string())));
                });
            }
            Effect::LoadInfo { title, kind } => {
                let backend = app.backend();
                let msg_tx = msg_tx.clone();
                tokio::task::spawn_blocking(move || {
                    let text = match kind {
                        InfoKind::FlowStatus => rgit_git::workflow::status(backend.as_ref()),
                        InfoKind::Workspaces => rgit_git::workspace::list(backend.as_ref()),
                    };
                    let _ = msg_tx.send(Msg::InfoLoaded {
                        title: title.to_owned(),
                        text: text.map_err(|e| e.to_string()),
                    });
                });
            }
            Effect::LoadWorktrees => {
                let backend = app.backend();
                let msg_tx = msg_tx.clone();
                tokio::task::spawn_blocking(move || {
                    if let Ok(worktrees) = backend.worktrees() {
                        let _ = msg_tx.send(Msg::WorktreesLoaded(worktrees));
                    }
                });
            }
            Effect::LoadForge => {
                let backend = app.backend();
                let workdir = backend.workdir().to_path_buf();
                let msg_tx = msg_tx.clone();
                tokio::spawn(async move {
                    let origin = tokio::task::spawn_blocking(move || {
                        backend.remotes().ok().and_then(|rs| {
                            rs.into_iter().find(|r| r.name == "origin").map(|r| r.url)
                        })
                    })
                    .await
                    .ok()
                    .flatten();
                    let result = crate::forge::load(origin, workdir).await;
                    let _ = msg_tx.send(Msg::ForgeLoaded(result));
                });
            }
            Effect::LoadRemoteStatus => {
                let backend = app.backend();
                let workdir = backend.workdir().to_path_buf();
                let branch = app.head().and_then(|h| h.branch.clone());
                let msg_tx = msg_tx.clone();
                tokio::spawn(async move {
                    let Some(branch) = branch else {
                        let _ = msg_tx.send(Msg::RemoteStatusLoaded(None));
                        return;
                    };
                    let origin = tokio::task::spawn_blocking(move || {
                        backend.remotes().ok().and_then(|rs| {
                            rs.into_iter().find(|r| r.name == "origin").map(|r| r.url)
                        })
                    })
                    .await
                    .ok()
                    .flatten();
                    // A forge error (no gh/glab/token) means no section, not a
                    // misleading "no PR"; success with no match means no PR.
                    let summary = match crate::forge::load(origin, workdir).await {
                        Ok(prs) => {
                            let pr = prs.into_iter().find(|p| p.branch == branch);
                            Some(crate::app::RemoteSummary {
                                pr: pr.as_ref().map(|p| (p.number, p.state.clone())),
                                checks: pr.and_then(|p| p.checks),
                            })
                        }
                        Err(_) => None,
                    };
                    let _ = msg_tx.send(Msg::RemoteStatusLoaded(summary));
                });
            }
            Effect::LoadDiff { from, to } => {
                let backend = app.backend();
                let msg_tx = msg_tx.clone();
                tokio::task::spawn_blocking(move || match backend.diff_refs(&from, &to) {
                    Ok(files) => {
                        let title = format!("Diff {from}..{to}");
                        let _ = msg_tx.send(Msg::DiffLoaded { title, files });
                    }
                    Err(e) => {
                        let _ = msg_tx.send(Msg::Error(e.to_string()));
                    }
                });
            }
        }
    }
    Ok(())
}

/// Watch the worktree and post a quiet refresh when files change. Returns the
/// debouncer, which must be held alive for watching to continue. Degrades to
/// manual refresh (`g`) if the watch cannot be set up.
fn spawn_watcher(
    workdir: &Path,
    msg_tx: UnboundedSender<Msg>,
) -> Option<Debouncer<RecommendedWatcher>> {
    let mut debouncer = new_debouncer(
        Duration::from_millis(400),
        move |res: DebounceEventResult| {
            if res.is_ok() {
                let _ = msg_tx.send(Msg::AutoRefresh);
            }
        },
    )
    .ok()?;
    debouncer
        .watcher()
        .watch(workdir, RecursiveMode::Recursive)
        .ok()?;
    Some(debouncer)
}

/// Run a synchronous read/mutation on a blocking task and report the resulting
/// status back as a [`Msg`], so the render loop never blocks.
fn spawn_read<F>(app: &App, msg_tx: &UnboundedSender<Msg>, work: F)
where
    F: FnOnce(&dyn GitBackend) -> Result<RepoStatus, GitError> + Send + 'static,
{
    let backend = app.backend();
    let msg_tx = msg_tx.clone();
    tokio::task::spawn_blocking(move || {
        let _ = msg_tx.send(Msg::Refreshed(Box::new(work(&*backend))));
    });
}

/// Forward a child stream's lines to the hook console as they arrive.
async fn forward_lines<R>(reader: R, tx: UnboundedSender<Msg>)
where
    R: tokio::io::AsyncRead + Unpin,
{
    use tokio::io::AsyncBufReadExt;
    let mut lines = tokio::io::BufReader::new(reader).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        let _ = tx.send(Msg::HookOutput(line));
    }
}

/// Whether a hook file exists and is executable (git skips non-executable ones).
fn hook_runnable(path: &std::path::Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(path)
            .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
            .unwrap_or(false)
    }
    #[cfg(not(unix))]
    {
        path.is_file()
    }
}

/// Run one hook script, streaming its merged stdout/stderr into the console.
/// Returns whether it succeeded (a missing hook counts as success).
async fn run_hook(
    hook: &std::path::Path,
    args: &[std::path::PathBuf],
    workdir: &std::path::Path,
    gitdir: &std::path::Path,
    msg_tx: &UnboundedSender<Msg>,
) -> bool {
    if !hook_runnable(hook) {
        return true;
    }
    let mut cmd = tokio::process::Command::new(hook);
    cmd.args(args)
        .current_dir(workdir)
        .env("GIT_DIR", gitdir)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    let mut child = match cmd.spawn() {
        Ok(child) => child,
        Err(e) => {
            let _ = msg_tx.send(Msg::HookOutput(format!(
                "could not run {}: {e}",
                hook.display()
            )));
            return false;
        }
    };
    let mut readers = Vec::new();
    if let Some(out) = child.stdout.take() {
        readers.push(tokio::spawn(forward_lines(out, msg_tx.clone())));
    }
    if let Some(err) = child.stderr.take() {
        readers.push(tokio::spawn(forward_lines(err, msg_tx.clone())));
    }
    for r in readers {
        let _ = r.await;
    }
    matches!(child.wait().await, Ok(status) if status.success())
}

/// Commit via libgit2, running the pre-commit and commit-msg hook scripts
/// ourselves and streaming their output into the console (so no git CLI is
/// involved). A hook rejection holds the commit with its output on screen; on
/// success we echo a git-style summary to the console and the log.
async fn commit_console(
    backend: Arc<dyn GitBackend>,
    amend: bool,
    message: String,
    msg_tx: UnboundedSender<Msg>,
) {
    let workdir = backend.workdir().to_path_buf();
    let hooks = backend.hooks_dir();
    let msg_path = backend.commit_msg_path();
    let gitdir = msg_path
        .parent()
        .map(std::path::Path::to_path_buf)
        .unwrap_or_else(|| workdir.join(".git"));

    let fail = |m: &str| {
        tracing::warn!(target: "git", "commit failed: {m}");
        let _ = msg_tx.send(Msg::HookOutput(m.to_owned()));
        let _ = msg_tx.send(Msg::HookFinished {
            ok: false,
            summary: None,
        });
    };

    // pre-commit.
    if !run_hook(&hooks.join("pre-commit"), &[], &workdir, &gitdir, &msg_tx).await {
        tracing::warn!(target: "git", "commit rejected by the pre-commit hook");
        let _ = msg_tx.send(Msg::HookFinished {
            ok: false,
            summary: None,
        });
        return;
    }

    // commit-msg: hand the hook the message file, then read back its edits.
    let mut message = message;
    let commit_msg = hooks.join("commit-msg");
    if hook_runnable(&commit_msg) {
        if std::fs::write(&msg_path, &message).is_err() {
            fail("could not write the commit message file");
            return;
        }
        if !run_hook(
            &commit_msg,
            std::slice::from_ref(&msg_path),
            &workdir,
            &gitdir,
            &msg_tx,
        )
        .await
        {
            let _ = msg_tx.send(Msg::HookFinished {
                ok: false,
                summary: None,
            });
            return;
        }
        if let Ok(edited) = std::fs::read_to_string(&msg_path) {
            message = edited
                .lines()
                .filter(|l| !l.starts_with('#'))
                .collect::<Vec<_>>()
                .join("\n")
                .trim()
                .to_owned();
        }
    }

    // Commit through libgit2 (hooks already ran).
    let commit = {
        let backend = backend.clone();
        let message = message.clone();
        tokio::task::spawn_blocking(move || {
            if amend {
                backend.amend_no_verify(&message)
            } else {
                backend.commit_no_verify(&message)
            }
        })
        .await
    };
    if let Ok(Err(e)) = &commit {
        fail(&e.to_string());
        return;
    }
    if commit.is_err() {
        fail("commit task failed");
        return;
    }

    // Echo a git-style summary to the console and the log, then run post-commit.
    let report = {
        let backend = backend.clone();
        tokio::task::spawn_blocking(move || backend.commit_report())
            .await
            .unwrap_or_default()
    };
    for line in &report {
        tracing::info!(target: "git", "{line}");
        let _ = msg_tx.send(Msg::HookOutput(line.clone()));
    }
    let _ = run_hook(&hooks.join("post-commit"), &[], &workdir, &gitdir, &msg_tx).await;

    let _ = msg_tx.send(Msg::HookFinished {
        ok: true,
        summary: report.into_iter().next(),
    });
}

/// Run a network op (fetch/pull/push) via libgit2, streaming its git-style
/// progress lines and transfer counts into the operation console and the log.
/// libgit2 is blocking, so it runs on a blocking task; the report callback sends
/// messages from there so the render loop never blocks.
async fn op_console(
    backend: Arc<dyn GitBackend>,
    op: crate::app::ConsoleOp,
    msg_tx: UnboundedSender<Msg>,
) {
    use crate::app::ConsoleOp;
    let title = op.title().to_owned();
    let tx = msg_tx.clone();
    let report = move |p: rgit_git::OpProgress| match p {
        rgit_git::OpProgress::Line(s) => {
            tracing::info!(target: "git", "{s}");
            let _ = tx.send(Msg::HookOutput(s));
        }
        rgit_git::OpProgress::Transfer { received, total } => {
            let _ = tx.send(Msg::HookProgress { received, total });
        }
    };
    let result = tokio::task::spawn_blocking(move || match op {
        ConsoleOp::Fetch => backend.fetch(&report),
        ConsoleOp::Pull => backend.pull(&report),
        ConsoleOp::Push {
            force,
            force_with_lease,
            set_upstream,
            remote,
        } => backend.push(remote.as_deref(), force, force_with_lease, set_upstream, &report),
        ConsoleOp::Merge(rev) => backend.merge(&rev, false, &report),
        ConsoleOp::RebaseOnto(rev) => backend.rebase_onto(&rev, &report),
    })
    .await;

    let (ok, summary) = match result {
        Ok(Ok(())) => (true, Some(format!("{title} complete"))),
        Ok(Err(e)) => {
            tracing::warn!(target: "git", "{title} failed: {e}");
            let _ = msg_tx.send(Msg::HookOutput(e.to_string()));
            (false, None)
        }
        Err(_) => {
            tracing::warn!(target: "git", "{title} task failed");
            let _ = msg_tx.send(Msg::HookOutput(format!("{title} task failed")));
            (false, None)
        }
    };
    let _ = msg_tx.send(Msg::HookFinished { ok, summary });
}

/// Suspend the TUI, open the commit-message editor, then commit from the index.
async fn commit_flow(
    app: &mut App,
    events: &mut Events,
    terminal: &mut DefaultTerminal,
    msg_tx: &UnboundedSender<Msg>,
    amend: bool,
) -> std::io::Result<()> {
    let backend = app.backend();
    let path = backend.commit_msg_path();
    // Amending prefills the editor with the HEAD commit's message.
    let template = if amend {
        match backend.head_message() {
            Some(msg) => format!("{msg}{COMMIT_TEMPLATE}"),
            None => {
                app.error = Some("nothing to amend".into());
                return Ok(());
            }
        }
    } else {
        COMMIT_TEMPLATE.to_owned()
    };
    if std::fs::write(&path, &template).is_err() {
        app.error = Some("could not write the commit message file".into());
        return Ok(());
    }

    // Hand the terminal and stdin to the editor, then take them back. A fresh
    // `init` starts with an empty buffer so the next render repaints in full;
    // an explicit clear here can wedge on the re-entered terminal.
    events.pause();
    ratatui::restore();
    let editor = editor_command();
    let edit_path = path.clone();
    let edited = tokio::task::spawn_blocking(move || run_editor(&editor, &edit_path)).await;
    *terminal = ratatui::init();
    events.resume();

    if !matches!(edited, Ok(Ok(true))) {
        app.error = Some("editor exited abnormally; commit aborted".into());
        return Ok(());
    }

    let message = strip_comments(&std::fs::read_to_string(&path).unwrap_or_default());
    if message.is_empty() {
        app.error = Some("aborting commit due to empty commit message".into());
        return Ok(());
    }

    app.loading = true;
    // After an amend (or reword), the current commit's oid changes, so any
    // branch stacked on it needs to move. Restack automatically (when enabled)
    // and note what happened, following jj/Sapling; a conflict is non-blocking.
    let backend = app.backend();
    let auto_restack = app.auto_restack();
    let tx = msg_tx.clone();
    tokio::task::spawn_blocking(move || {
        let status = (|| -> Result<RepoStatus, GitError> {
            if amend {
                backend.amend(&message)?;
                if auto_restack {
                    if let Some(note) = auto_restack_note(&*backend)? {
                        let _ = tx.send(Msg::AutoRestackNote(note));
                    }
                }
            } else {
                backend.commit(&message)?;
            }
            backend.status()
        })();
        let _ = tx.send(Msg::Refreshed(Box::new(status)));
    });
    Ok(())
}

/// Restack the stacked branches after an amend and describe what moved, or
/// `None` when there was nothing stacked to restack.
fn auto_restack_note(backend: &dyn GitBackend) -> Result<Option<String>, GitError> {
    Ok(restack_note(&backend.restack()?))
}

/// A one-line toast summary of a restack outcome, or `None` when nothing moved.
fn restack_note(outcome: &rgit_git::RestackOutcome) -> Option<String> {
    if outcome.is_empty() {
        return None;
    }
    let mut parts = Vec::new();
    if !outcome.restacked.is_empty() {
        parts.push(format!("restacked {}", outcome.restacked.len()));
    }
    if !outcome.conflicted.is_empty() {
        parts.push(format!("conflicts: {}", outcome.conflicted.join(", ")));
    }
    Some(format!("auto-restack ({})", parts.join("; ")))
}

fn apply_mutation(backend: &dyn GitBackend, mutation: &Mutation) -> Result<(), GitError> {
    match mutation {
        Mutation::StageAll => backend.stage_all(),
        Mutation::UnstageAll => backend.unstage_all(),
        Mutation::StageFile(path) => backend.stage_file(path),
        Mutation::UnstageFile(path) => backend.unstage_file(path),
        Mutation::StageHunk { path, new_start } => backend.stage_hunk(path, *new_start),
        Mutation::UnstageHunk { path, new_start } => backend.unstage_hunk(path, *new_start),
        Mutation::StageLines {
            path,
            new_start,
            lines,
        } => backend.stage_lines(path, *new_start, lines),
        Mutation::UnstageLines {
            path,
            new_start,
            lines,
        } => backend.unstage_lines(path, *new_start, lines),
        Mutation::DiscardFile(path) => backend.discard_file(path),
        Mutation::DiscardHunk { path, new_start } => backend.discard_hunk(path, *new_start),
        Mutation::DiscardLines {
            path,
            new_start,
            lines,
        } => backend.discard_lines(path, *new_start, lines),
        Mutation::CheckoutBranch(name) => backend.checkout_branch(name),
        Mutation::CreateBranch(name) => backend.create_branch(name),
        Mutation::CheckoutDetached(name) => backend.checkout_detached(name),
        Mutation::StashPush => backend.stash_push().map(drop),
        Mutation::StashPushMessage(msg) => backend.stash_push_message(msg).map(drop),
        Mutation::StashPop(index) => backend.stash_pop(*index),
        Mutation::StashApply(index) => backend.stash_apply(*index),
        Mutation::StashDrop(index) => backend.stash_drop(*index),
        Mutation::RenameBranch { old, new } => backend.rename_branch(old, new),
        Mutation::AddRemote { name, url } => backend.add_remote(name, url),
        Mutation::RemoveRemote(name) => backend.remove_remote(name),
        Mutation::AddWorktree { name, path } => backend.add_worktree(name, path),
        Mutation::RemoveWorktree(name) => backend.remove_worktree(name),
        Mutation::Extend => backend.commit_extend(),
        Mutation::RebaseAbort => backend.rebase_abort(),
        Mutation::RebaseContinue => backend.rebase_continue(),
        Mutation::RebaseSkip => backend.rebase_skip(),
        Mutation::Undo => backend.undo().map(drop),
        Mutation::Redo => backend.redo().map(drop),
        Mutation::Absorb => backend.absorb().map(drop),
        Mutation::Restack => backend.restack().map(drop),
        Mutation::Bisect(args) => backend.bisect(args).map(drop),
        Mutation::Reset { rev, mode } => backend.reset(rev, *mode),
        Mutation::CherryPick(rev) => backend.cherry_pick(rev),
        Mutation::Revert(rev) => backend.revert(rev),
        Mutation::ResolveConflict { path, ours } => backend.resolve_conflict(path, *ours),
        Mutation::CreateTag(name) => backend.create_tag(name, ""),
        Mutation::DeleteTag(name) => backend.delete_tag(name),
        Mutation::DeleteBranch(name) => backend.delete_branch(name, true),
        Mutation::Reword { rev, message } => backend.reword(rev, message),
        Mutation::Squash(rev) => backend.squash(rev),
        Mutation::Uncommit(n) => backend.uncommit(*n),
    }
}

/// Suspend the TUI and run `git <args>` in the user's terminal so it can drive
/// its own editor (interactive rebase, `--continue`), then refresh. Used for the
/// few operations libgit2 cannot express in-process.
async fn git_console(
    app: &mut App,
    events: &mut Events,
    terminal: &mut DefaultTerminal,
    msg_tx: &UnboundedSender<Msg>,
    args: Vec<String>,
    label: String,
) -> std::io::Result<()> {
    git_console_env(app, events, terminal, msg_tx, args, label, Vec::new()).await
}

async fn git_console_env(
    app: &mut App,
    events: &mut Events,
    terminal: &mut DefaultTerminal,
    msg_tx: &UnboundedSender<Msg>,
    args: Vec<String>,
    label: String,
    env: Vec<(String, String)>,
) -> std::io::Result<()> {
    let workdir = app.backend().workdir().to_path_buf();

    events.pause();
    ratatui::restore();
    let ran = tokio::task::spawn_blocking(move || {
        let mut cmd = std::process::Command::new("git");
        cmd.args(&args).current_dir(&workdir);
        for (k, v) in &env {
            cmd.env(k, v);
        }
        cmd.status()
    })
    .await;
    *terminal = ratatui::init();
    events.resume();

    match ran {
        Ok(Ok(status)) if status.success() => {}
        Ok(Ok(_)) => app.error = Some(format!("{label} exited with a non-zero status")),
        _ => app.error = Some(format!("could not run git {label}")),
    }

    app.busy = None;
    app.loading = true;
    spawn_read(app, msg_tx, |b| b.status());
    Ok(())
}

const COMMIT_TEMPLATE: &str = "\n\
    # Please enter the commit message for your changes. Lines starting\n\
    # with '#' are ignored, and an empty message aborts the commit.\n";

fn editor_command() -> String {
    std::env::var("GIT_EDITOR")
        .or_else(|_| std::env::var("VISUAL"))
        .or_else(|_| std::env::var("EDITOR"))
        .unwrap_or_else(|_| "vi".to_owned())
}

/// Run `<editor> <path>` via the shell (so a multi-word `$GIT_EDITOR`, such as
/// an `nvr` invocation, works), inheriting the terminal. Returns whether it
/// exited successfully.
fn run_editor(editor: &str, path: &Path) -> std::io::Result<bool> {
    let status = std::process::Command::new("sh")
        .arg("-c")
        .arg(format!("{editor} \"$1\""))
        .arg("sh")
        .arg(path)
        .status()?;
    Ok(status.success())
}

fn strip_comments(raw: &str) -> String {
    raw.lines()
        .filter(|line| !line.starts_with('#'))
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_owned()
}

/// Copy `text` to the system clipboard via the OSC 52 escape sequence, which
/// works over ssh and inside tmux (with `set -g set-clipboard on`) since the
/// terminal, not this process, owns the clipboard.
///
/// Best-effort and non-blocking by construction: it only ever WRITES a payload
/// the caller has already bounded (never the OSC 52 query form, which waits for
/// a reply), and it swallows write errors so a stalled or broken output can
/// never take down or hang the event loop.
fn copy_to_clipboard(text: &str) {
    use std::io::Write;
    let seq = format!("\x1b]52;c;{}\x07", base64_encode(text.as_bytes()));
    let mut stdout = std::io::stdout();
    let _ = stdout.write_all(seq.as_bytes());
    let _ = stdout.flush();
}

/// Standard base64 (RFC 4648). Hand-rolled to avoid a direct dependency for the
/// one place we need it.
fn base64_encode(input: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(input.len().div_ceil(3) * 4);
    for chunk in input.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = *chunk.get(1).unwrap_or(&0) as u32;
        let b2 = *chunk.get(2).unwrap_or(&0) as u32;
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(T[((n >> 18) & 63) as usize] as char);
        out.push(T[((n >> 12) & 63) as usize] as char);
        out.push(if chunk.len() > 1 {
            T[((n >> 6) & 63) as usize] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            T[(n & 63) as usize] as char
        } else {
            '='
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::{base64_encode, restack_note};
    use rgit_git::RestackOutcome;

    #[test]
    fn restack_note_summarizes_moves_and_conflicts() {
        assert_eq!(restack_note(&RestackOutcome::default()), None);
        let moved = RestackOutcome {
            restacked: vec!["a -> main".into(), "b -> a".into()],
            conflicted: vec![],
        };
        assert_eq!(restack_note(&moved).as_deref(), Some("auto-restack (restacked 2)"));
        let mixed = RestackOutcome {
            restacked: vec!["a -> main".into()],
            conflicted: vec!["b".into()],
        };
        assert_eq!(
            restack_note(&mixed).as_deref(),
            Some("auto-restack (restacked 1; conflicts: b)")
        );
    }

    #[test]
    fn base64_matches_rfc4648_vectors() {
        assert_eq!(base64_encode(b""), "");
        assert_eq!(base64_encode(b"f"), "Zg==");
        assert_eq!(base64_encode(b"fo"), "Zm8=");
        assert_eq!(base64_encode(b"foo"), "Zm9v");
        assert_eq!(base64_encode(b"foob"), "Zm9vYg==");
        assert_eq!(base64_encode(b"fooba"), "Zm9vYmE=");
        assert_eq!(base64_encode(b"foobar"), "Zm9vYmFy");
    }
}
