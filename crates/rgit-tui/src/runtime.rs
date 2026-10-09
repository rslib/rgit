use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use notify_debouncer_mini::notify::{RecommendedWatcher, RecursiveMode};
use notify_debouncer_mini::{DebounceEventResult, Debouncer, new_debouncer};
use ratatui::DefaultTerminal;
use rgit_git::{GitBackend, GitError, RepoStatus};
use tokio::sync::mpsc::UnboundedSender;

use crate::app::{App, Effect, InfoKind, LaneOp, Leader, Msg, Mutation, TextOp, update};
use crate::buffer::row_prefix_width;
use crate::events::{Event, Events};
use crate::keymap::{
    self, resolve_commit_key, resolve_confirm_key, resolve_finder_key, resolve_help_key,
    resolve_key, resolve_palette_key, resolve_prompt_key, resolve_rebase_key, resolve_search_key,
    resolve_split_picker_key, resolve_transient_key,
};
use crate::ui;

fn resolve_forge_key(key: crossterm::event::KeyEvent) -> Option<Msg> {
    use crossterm::event::KeyCode;
    match key.code {
        KeyCode::Char(']') => Some(Msg::ForgeProfileNext),
        KeyCode::Char('c') => Some(Msg::ForgeCreate),
        KeyCode::Char('d') | KeyCode::Enter => Some(Msg::ForgeDetails),
        KeyCode::Char('x') => Some(Msg::ForgeClose),
        KeyCode::Char('o') | KeyCode::Char('O') => Some(Msg::ForgeOpen),
        _ if keymap::profile() == keymap::Profile::Vim => keymap::resolve_vim_key(key),
        _ => resolve_key(key),
    }
}
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
        LaneOp::New(name) => backend
            .lane_new(name)
            .map(|()| format!("created lane {name}")),
        LaneOp::Assign { lane, path } => backend
            .lane_assign(lane, path)
            .map(|()| format!("{path} -> {lane}")),
        LaneOp::Unassign(path) => backend
            .lane_unassign(path)
            .map(|()| format!("{path} -> default")),
        LaneOp::Commit { lane, message } => backend.lane_commit(lane, message),
        LaneOp::Rename { old, new } => backend
            .lane_rename(old, new)
            .map(|()| format!("{old} -> {new}")),
        LaneOp::Delete(name) => backend
            .lane_delete(name)
            .map(|()| format!("deleted lane {name}")),
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
pub async fn run(backend: Arc<dyn GitBackend>, no_preview: bool) -> std::io::Result<()> {
    // Load config before touching the terminal so the theme is in place for the
    // very first frame; a config error is shown in-app, not fatal.
    let (mut config, config_error) = crate::config::load();
    // The CLI flag is a hard override of the config toggle.
    if no_preview {
        config.ui.preview = false;
    }
    crate::theme::init(crate::theme::Theme::from_config(&config.theme));
    // Resolve the active profile (built-in or custom) into its base + the merged
    // per-profile key overrides.
    let (base_profile, key_overrides) = config.resolved_keymap();
    crate::keymap::init(&key_overrides, Some(&base_profile));
    rgit_model::set_side_by_side(config.ui.side_by_side);
    rgit_model::set_glyph_mode(config.ui.glyph_mode());
    rgit_model::set_syntax_colors(crate::theme::syntax_colors());

    let mouse = config.ui.mouse;
    MOUSE_CAPTURE.store(mouse, Ordering::Relaxed);
    let mut terminal = ratatui::init();
    enable_mouse_capture();
    install_panic_hook(mouse);
    let result = event_loop(&mut terminal, backend, config, config_error).await;
    if mouse {
        let _ = crossterm::execute!(std::io::stdout(), crossterm::event::DisableMouseCapture);
    }
    ratatui::restore();
    result
}

/// Mouse capture is a distinct terminal mode from raw mode and the alternate
/// screen, so `ratatui::init()` does not restore it. Track whether the user
/// wants it and re-enable it after every suspend/resume around an external
/// program (editor, git console); otherwise the mouse stops working on return.
static MOUSE_CAPTURE: AtomicBool = AtomicBool::new(false);

fn enable_mouse_capture() {
    if MOUSE_CAPTURE.load(Ordering::Relaxed) {
        let _ = crossterm::execute!(std::io::stdout(), crossterm::event::EnableMouseCapture);
    }
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
        .set_credential_prompt(Box::new(TuiCredentialPrompt { tx: msg_tx.clone() }));

    // Auto-refresh on worktree changes; kept alive for the loop's duration.
    let _watcher = spawn_watcher(app.backend().clone(), msg_tx.clone());
    let gate = RefreshGate::default();

    msg_tx.send(Msg::Refresh).ok();
    tracing::info!(target: "startup", "tui initial refresh queued");
    // Draw once up front; thereafter only when something actually changes, so an
    // idle TUI does no work between the periodic animation ticks.
    terminal.draw(|frame| ui::render(frame, &mut app))?;

    loop {
        // The select borrows `events` only until it resolves, freeing it (and
        // the terminal) for effect handling below.
        let was_signing = app.signing;
        let incoming = tokio::select! {
            Some(event) = events.next() => Incoming::Event(event),
            Some(msg) = msg_rx.recv() => Incoming::Msg(msg),
        };

        let redraw = match incoming {
            Incoming::Event(Event::Resize) => true,
            Incoming::Event(Event::Key(key)) => {
                // Emacs `C-x o` prefix chord: only in a normal buffer, never while
                // a text-input overlay owns the keyboard.
                let text_overlay = app.prompt.is_some()
                    || app.palette.is_some()
                    || app.code_finder.is_some()
                    || app.search.is_some()
                    || app.commit_editor.is_some();
                let msg = if app.ctrl_x_pending {
                    app.ctrl_x_pending = false;
                    // The follow-up key resolves against the binding map (C-x o).
                    keymap::resolve_prefixed(key)
                } else if !text_overlay
                    && key
                        .modifiers
                        .contains(crossterm::event::KeyModifiers::CONTROL)
                    && key.code == crossterm::event::KeyCode::Char('x')
                {
                    app.ctrl_x_pending = true;
                    None
                } else if app.prompt.is_some() {
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
                } else if app.split_picker.is_some() {
                    resolve_split_picker_key(key)
                } else if app.commit_editor.is_some() {
                    resolve_commit_key(key)
                } else if app.palette.is_some() {
                    resolve_palette_key(key)
                } else if app.code_finder.is_some() {
                    resolve_finder_key(key)
                } else if app.search.is_some() {
                    resolve_search_key(key)
                } else if app.transient.is_some() {
                    resolve_transient_key(key)
                } else if app.confirm.is_some() {
                    resolve_confirm_key(key)
                } else if app.leader.is_some() {
                    resolve_leader(&mut app, key)
                } else if (app.active_kind() == crate::app::ViewKind::Status
                    && app.buffer().cursor_id().as_deref() == Some("remote/forge")
                    && key.code == crossterm::event::KeyCode::Enter)
                    || (key
                        .modifiers
                        .contains(crossterm::event::KeyModifiers::CONTROL)
                        && key.code == crossterm::event::KeyCode::Char('g'))
                {
                    Some(Msg::Forge)
                } else if app.active_kind() == crate::app::ViewKind::Forge {
                    resolve_forge_key(key)
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
                    run_msg(&mut app, msg, &mut events, terminal, &msg_tx, &gate).await?;
                }
                true
            }
            Incoming::Event(Event::Mouse(m)) => {
                if let Some(msg) = mouse_msg(&app, m) {
                    run_msg(&mut app, msg, &mut events, terminal, &msg_tx, &gate).await?;
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
                run_msg(&mut app, msg, &mut events, terminal, &msg_tx, &gate).await?;
                true
            }
        };

        // While a signed commit runs, a tty passphrase prompt may be drawing on
        // the terminal: yield input and the screen to it, then clear and
        // repaint everything it scribbled once the commit finishes.
        if app.signing && !was_signing {
            events.pause();
        }
        if !app.signing && was_signing {
            events.resume();
            // Wipe whatever the passphrase prompt scribbled and force a full
            // repaint. Best-effort: it asks the terminal for the cursor
            // position, which a dumb pipe cannot answer.
            let _ = terminal.clear();
        }
        if redraw && (!app.signing || !was_signing) {
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

    // A pane hit as (offset, content_col): the pane's top row, then subtract
    // the frame/padding (pane_left) and the row's own prefix (cursor bar +
    // indent + fold chevron) so the column lines up with the character the
    // mouse actually hit. Prefix width is per-row, so re-compute per event.
    let body_hit = |row: u16| -> Option<(usize, usize)> {
        let offset = row.checked_sub(app.body_top)? as usize;
        let idx = app.buffer().scroll() + offset;
        let prefix = app
            .buffer()
            .rows()
            .nth(idx)
            .map(|r| row_prefix_width(&r))
            .unwrap_or(0);
        let col = m
            .column
            .saturating_sub(app.body_left)
            .saturating_sub(prefix as u16) as usize;
        Some((offset, col))
    };
    let preview_hit = |row: u16| -> Option<(usize, usize)> {
        if !app.preview_visible {
            return None;
        }
        let offset = row.checked_sub(app.preview_top)? as usize;
        let idx = app.preview_buffer().scroll() + offset;
        let prefix = app
            .preview_buffer()
            .rows()
            .nth(idx)
            .map(|r| row_prefix_width(&r))
            .unwrap_or(0);
        let col = m
            .column
            .saturating_sub(app.preview_left)
            .saturating_sub(prefix as u16) as usize;
        Some((offset, col))
    };

    match m.kind {
        MouseEventKind::ScrollDown if over_preview => Some(Msg::ScrollPreview(WHEEL)),
        MouseEventKind::ScrollUp if over_preview => Some(Msg::ScrollPreview(-WHEEL)),
        MouseEventKind::ScrollDown => Some(Msg::Scroll(WHEEL)),
        MouseEventKind::ScrollUp => Some(Msg::Scroll(-WHEEL)),
        MouseEventKind::Down(MouseButton::Left) if !overlay_active(app) => {
            if over_preview {
                let (offset, col) = preview_hit(m.row)?;
                Some(Msg::ClickPreview { offset, col })
            } else {
                let (offset, col) = body_hit(m.row)?;
                Some(Msg::ClickRow { offset, col })
            }
        }
        // Drag extends a selection only after a press in the same pane; the
        // press handler recorded which pane via `mouse_drag_pane`.
        MouseEventKind::Drag(MouseButton::Left) if !overlay_active(app) => {
            let focused_preview = app.drag_state().0.unwrap_or(app.preview_focus);
            if focused_preview {
                let (offset, col) = preview_hit(m.row)?;
                Some(Msg::DragPreview { offset, col })
            } else {
                let (offset, col) = body_hit(m.row)?;
                Some(Msg::DragRow { offset, col })
            }
        }
        // A release ends any drag; a click without motion already acted on Down.
        MouseEventKind::Up(MouseButton::Left) => {
            let (pending, dragging) = app.drag_state();
            (dragging || pending.is_some()).then_some(Msg::DragEnd)
        }
        _ => None,
    }
}

fn overlay_active(app: &App) -> bool {
    app.help
        || app.palette.is_some()
        || app.code_finder.is_some()
        || app.prompt.is_some()
        || app.transient.is_some()
        || app.confirm.is_some()
        || app.search.is_some()
        || app.commit_editor.is_some()
        || app.rebase_todo.is_some()
        || app.leader.is_some()
}

/// Bounds concurrent status refreshes to one running plus one queued: a
/// worktree-watch burst (an editor save, a build) must not pile up full
/// `status()` calls that each hold the repo lock and starve user operations.
#[derive(Default)]
struct RefreshGate {
    inflight: std::sync::atomic::AtomicBool,
    queued: std::sync::atomic::AtomicBool,
}

impl RefreshGate {
    /// Whether the caller should spawn the refresh; otherwise it is queued
    /// behind the one already running.
    fn start_or_queue(&self) -> bool {
        use std::sync::atomic::Ordering::{AcqRel, Release};
        if self.inflight.swap(true, AcqRel) {
            self.queued.store(true, Release);
            false
        } else {
            true
        }
    }

    /// A refresh finished; whether a queued one should run now.
    fn finish(&self) -> bool {
        use std::sync::atomic::Ordering::{AcqRel, Release};
        self.inflight.store(false, Release);
        self.queued.swap(false, AcqRel)
    }
}

async fn run_msg(
    app: &mut App,
    msg: Msg,
    events: &mut Events,
    terminal: &mut DefaultTerminal,
    msg_tx: &UnboundedSender<Msg>,
    gate: &RefreshGate,
) -> std::io::Result<()> {
    let requeue = matches!(msg, Msg::Refreshed(_)) && gate.finish();
    for effect in update(app, msg) {
        match effect {
            Effect::Refresh => {
                if gate.start_or_queue() {
                    spawn_read(app, msg_tx, |b| b.status());
                }
            }
            Effect::FillDiffs => {
                if gate.start_or_queue() {
                    spawn_read(app, msg_tx, |b| b.status_full());
                }
            }
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
                        if restack_after && let Some(note) = auto_restack_note(&*backend)? {
                            let _ = tx.send(Msg::AutoRestackNote(note));
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
            Effect::CommitConsole {
                amend,
                message,
                sign,
            } => {
                let backend = app.backend();
                let msg_tx = msg_tx.clone();
                if sign {
                    // A tty pinentry/ssh-keygen prompt draws on the terminal
                    // behind the TUI; freeze our frames until it finishes.
                    app.signing = true;
                }
                tokio::spawn(commit_console(backend, amend, message, sign, msg_tx));
            }
            Effect::OpConsole(op) => {
                let backend = app.backend();
                let msg_tx = msg_tx.clone();
                tokio::spawn(op_console(backend, op, msg_tx));
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
            Effect::LoadSplitFiles(rev) => {
                let backend = app.backend();
                let msg_tx = msg_tx.clone();
                tokio::task::spawn_blocking(move || match backend.commit_overview(&rev) {
                    Ok(ov) => {
                        let files = ov.files.into_iter().map(|f| f.path).collect();
                        let _ = msg_tx.send(Msg::ShowSplitPicker { rev, files });
                    }
                    Err(e) => {
                        let _ = msg_tx.send(Msg::Error(e.to_string()));
                    }
                });
            }
            Effect::LoadPromptCandidates(action) => {
                let backend = app.backend();
                let msg_tx = msg_tx.clone();
                if action == crate::app::PromptAction::MergeBranch {
                    tokio::task::spawn_blocking(move || {
                        if let Ok(branches) = backend.local_branches() {
                            let options = branches.into_iter().map(|b| (b.clone(), b)).collect();
                            let _ = msg_tx.send(Msg::PromptCandidatesLoaded { action, options });
                        }
                    });
                } else {
                    let opts = rgit_git::LogOptions {
                        limit: crate::app::REV_CANDIDATES,
                        ..Default::default()
                    };
                    tokio::task::spawn_blocking(move || {
                        if let Ok(entries) = backend.log(&opts) {
                            let options = entries
                                .into_iter()
                                .map(|e| (format!("{} {}", e.short_id, e.summary), e.short_id))
                                .collect();
                            let _ = msg_tx.send(Msg::PromptCandidatesLoaded { action, options });
                        }
                    });
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
            Effect::LoadFilePreview { key, path, staged } => {
                let backend = app.backend();
                let msg_tx = msg_tx.clone();
                tokio::task::spawn_blocking(move || {
                    let files: Vec<_> = backend
                        .file_diff(&path, staged)
                        .ok()
                        .flatten()
                        .into_iter()
                        .collect();
                    let sections = rgit_model::build_diff(&path, &files);
                    let _ = msg_tx.send(Msg::PreviewBuilt { key, sections });
                });
            }
            Effect::BuildFilePreview { key, diff } => {
                let msg_tx = msg_tx.clone();
                tokio::task::spawn_blocking(move || {
                    let sections = rgit_model::build_diff(&diff.path, std::slice::from_ref(&diff));
                    let _ = msg_tx.send(Msg::PreviewBuilt { key, sections });
                });
            }
            Effect::OpenInEditor { path, line, col } => {
                // Best-effort: a diff line from a commit may name a file that has
                // since moved or shrunk. Skip if it is gone, and clamp the line to
                // the file's current length so the editor lands somewhere sane.
                // A worktree inspection view resolves paths against its own root.
                let root = app
                    .active_base()
                    .unwrap_or_else(|| app.backend().workdir().to_path_buf());
                let full = root.join(&path);
                let line = match std::fs::read_to_string(&full) {
                    Ok(text) => line.min(text.lines().count().max(1)),
                    Err(_) => {
                        app.push_toast(
                            crate::app::ToastKind::Error,
                            format!("{path} is not in the working tree"),
                        );
                        continue;
                    }
                };
                // The editor that spawned rgit opens the file in its own
                // running instance when it left a trace in the environment;
                // the remote/CLI command exits at once, so restore the TUI
                // immediately. Otherwise hand the terminal to $EDITOR at the
                // line/column and take it back, the same suspend/resume the
                // commit editor uses.
                if let Some(host) = HostEditor::detect() {
                    events.pause();
                    ratatui::restore();
                    let opened =
                        tokio::task::spawn_blocking(move || host.run(&full, line, col)).await;
                    *terminal = ratatui::init();
                    enable_mouse_capture();
                    events.resume();
                    if !matches!(opened, Ok(Ok(true))) {
                        app.push_toast(
                            crate::app::ToastKind::Error,
                            "could not open in host editor".into(),
                        );
                    }
                    continue;
                }
                // Hand the terminal to $EDITOR at the line/column, then take it
                // back, the same suspend/resume the commit editor uses.
                events.pause();
                ratatui::restore();
                let editor = editor_command();
                let opened =
                    tokio::task::spawn_blocking(move || run_editor_at(&editor, &full, line, col))
                        .await;
                *terminal = ratatui::init();
                enable_mouse_capture();
                events.resume();
                if !matches!(opened, Ok(Ok(true))) {
                    app.push_toast(
                        crate::app::ToastKind::Error,
                        "editor exited abnormally".into(),
                    );
                }
            }
            Effect::BuildSnippetPreview { key, path, line } => {
                let dir = app.backend().workdir().to_path_buf();
                let msg_tx = msg_tx.clone();
                tokio::task::spawn_blocking(move || {
                    let sections = snippet_sections(&dir, &path, line);
                    let _ = msg_tx.send(Msg::PreviewBuilt { key, sections });
                });
            }
            Effect::LoadBlame(path) => {
                let backend = app.backend();
                let msg_tx = msg_tx.clone();
                tokio::task::spawn_blocking(move || {
                    let msg = match backend.blame(&path) {
                        Ok(lines) => Msg::BlameLoaded { path, lines },
                        Err(e) => Msg::BlameFailed(e.to_string()),
                    };
                    let _ = msg_tx.send(msg);
                });
            }
            Effect::CodeSearchLive(query) => {
                let backend = app.backend();
                let msg_tx = msg_tx.clone();
                tokio::task::spawn_blocking(move || {
                    let hits = grep_hits(&*backend, &query).map_err(|e| e.to_string());
                    let _ = msg_tx.send(Msg::CodeFinderHits {
                        query,
                        semantic: false,
                        hits,
                    });
                });
            }
            Effect::CodeSearch(query) => {
                let backend = app.backend();
                let msg_tx = msg_tx.clone();
                tokio::task::spawn_blocking(move || {
                    let hits = code_search(&*backend, &query).map_err(|e| e.to_string());
                    let _ = msg_tx.send(Msg::CodeFinderHits {
                        query,
                        semantic: true,
                        hits,
                    });
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
            Effect::LoadRemoteNames => {
                let backend = app.backend();
                let msg_tx = msg_tx.clone();
                tokio::task::spawn_blocking(move || {
                    if let Ok(remotes) = backend.remotes() {
                        let _ = msg_tx.send(Msg::RemoteNamesLoaded(
                            remotes.into_iter().map(|r| r.name).collect(),
                        ));
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
                    // Opening the lanes view enables lanes on first use, so it
                    // works from the TUI without dropping to `rgit lanes init`.
                    if !backend.lanes_active()
                        && let Err(e) = backend.lanes_init()
                    {
                        let _ =
                            msg_tx.send(Msg::LaneNotice(format!("could not enable lanes: {e}")));
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
                    if auto_restack
                        && matches!(op, LaneOp::Commit { .. })
                        && let Ok(Some(note)) = backend.lane_restack().map(|o| restack_note(&o))
                    {
                        let _ = msg_tx.send(Msg::LaneNotice(note));
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
                        TextOp::PruneMerged(base) => backend.prune_merged(&base).map(|deleted| {
                            if deleted.is_empty() {
                                "no merged branches to prune".to_owned()
                            } else {
                                format!("pruned {}: {}", deleted.len(), deleted.join(", "))
                            }
                        }),
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
            Effect::InspectWorktree { name, path } => {
                // Open the worktree as its own repo and read its working-tree
                // changes, off the UI thread.
                let msg_tx = msg_tx.clone();
                tokio::task::spawn_blocking(move || {
                    let base = std::path::PathBuf::from(&path);
                    let msg = match rgit_git::Git2Backend::discover(&base).and_then(|b| b.status())
                    {
                        Ok(status) => {
                            let changed = status.unstaged.len();
                            let head = status.head.oid.as_deref().unwrap_or("unborn");
                            let branch = status.head.branch.as_deref().unwrap_or("detached");
                            let title = format!(
                                "worktree {name} \u{b7} {branch} @ {head} \u{b7} {changed} changed"
                            );
                            Msg::WorktreeInspected {
                                title,
                                base,
                                files: status.unstaged,
                            }
                        }
                        Err(e) => Msg::WorktreeInspected {
                            title: format!("worktree {name}: {e}"),
                            base,
                            files: Vec::new(),
                        },
                    };
                    let _ = msg_tx.send(msg);
                });
            }
            Effect::LoadForge => {
                let backend = app.backend();
                let workdir = backend.workdir().to_path_buf();
                let forge_account = app.forge_account.clone();
                let forge_host = app.forge_host.clone();
                let msg_tx = msg_tx.clone();
                tokio::spawn(async move {
                    let started = Instant::now();
                    let origin = tokio::task::spawn_blocking(move || {
                        backend.remotes().ok().and_then(|rs| {
                            rs.into_iter().find(|r| r.name == "origin").map(|r| r.url)
                        })
                    })
                    .await
                    .ok()
                    .flatten();
                    let handle = tokio::runtime::Handle::current();
                    let result = tokio::task::spawn_blocking(move || {
                        handle.block_on(crate::forge::load(
                            origin,
                            workdir,
                            forge_account,
                            forge_host,
                        ))
                    })
                    .await
                    .unwrap_or_else(|error| Err(format!("forge worker failed: {error}")));
                    tracing::info!(
                        target: "startup",
                        elapsed_ms = started.elapsed().as_millis(),
                        "forge load completed"
                    );
                    let _ = msg_tx.send(Msg::ForgeLoaded(result));
                });
            }
            Effect::ForgeClose {
                provider,
                host,
                account,
                repository,
                number,
            } => {
                let msg_tx = msg_tx.clone();
                tokio::spawn(async move {
                    let result = async {
                        let repo = rgit_forge::RepoRef::parse(&repository)
                            .map_err(|error| error.to_string())?;
                        match provider.as_str() {
                            "github" => {
                                let client =
                                    rgit_forge::GithubClient::from_environment_for(&account)
                                        .await
                                        .map_err(|error| error.to_string())?;
                                client
                                    .close_pull_request(&repo, number)
                                    .await
                                    .map_err(|error| error.to_string())?;
                            }
                            "gitlab" => {
                                let client =
                                    rgit_forge::GitlabClient::from_environment_for(&host, &account)
                                        .map_err(|error| error.to_string())?;
                                client
                                    .close_merge_request(&repo, number)
                                    .await
                                    .map_err(|error| error.to_string())?;
                            }
                            _ => return Err("unsupported forge provider".to_owned()),
                        }
                        Ok(format!("closed {provider} request #{number}"))
                    }
                    .await;
                    let _ = msg_tx.send(Msg::ForgeMutationDone(result));
                });
            }
            Effect::ForgeCreate {
                provider,
                host,
                account,
                repository,
                title,
                head,
                base,
            } => {
                let msg_tx = msg_tx.clone();
                tokio::spawn(async move {
                    let result = async {
                        let repo = rgit_forge::RepoRef::parse(&repository)
                            .map_err(|error| error.to_string())?;
                        let request = rgit_forge::CreatePullRequest {
                            title,
                            head,
                            base,
                            body: None,
                            draft: false,
                        };
                        match provider.as_str() {
                            "github" => {
                                let client =
                                    rgit_forge::GithubClient::from_environment_for(&account)
                                        .await
                                        .map_err(|error| error.to_string())?;
                                client
                                    .create_pull_request(&repo, &request)
                                    .await
                                    .map_err(|error| error.to_string())?;
                            }
                            "gitlab" => {
                                let client =
                                    rgit_forge::GitlabClient::from_environment_for(&host, &account)
                                        .map_err(|error| error.to_string())?;
                                client
                                    .create_merge_request(&repo, &request)
                                    .await
                                    .map_err(|error| error.to_string())?;
                            }
                            _ => return Err("unsupported forge provider".to_owned()),
                        }
                        Ok(format!("created {provider} request"))
                    }
                    .await;
                    let _ = msg_tx.send(Msg::ForgeMutationDone(result));
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
    if requeue && gate.start_or_queue() {
        spawn_read(app, msg_tx, |b| b.status());
    }
    Ok(())
}

/// Watch the worktree and post a quiet refresh when files change. Returns the
/// debouncer, which must be held alive for watching to continue. Degrades to
/// manual refresh (`g`) if the watch cannot be set up.
///
/// Events are filtered so an agent or build churning through ignored paths
/// does not force a full status refresh per burst: `.git` internals are
/// dropped by path prefix, and everything else goes through one batched
/// ignore check before a refresh is queued.
fn spawn_watcher(
    backend: Arc<dyn GitBackend>,
    msg_tx: UnboundedSender<Msg>,
) -> Option<Debouncer<RecommendedWatcher>> {
    let workdir = backend.workdir().to_path_buf();
    let watchdir = workdir.clone();
    let watchdir_in = workdir.clone();
    let mut debouncer = new_debouncer(Duration::from_secs(1), move |res: DebounceEventResult| {
        let Ok(events) = res else {
            return;
        };
        let mut interesting: Vec<std::path::PathBuf> = Vec::new();
        for event in events {
            if is_git_internal(&watchdir_in, &event.path) {
                continue;
            }
            interesting.push(event.path);
        }
        if interesting.is_empty() {
            return;
        }
        // `.gitignore`-driven check: one lock acquisition for the batch.
        let backend = Arc::clone(&backend);
        let msg_tx = msg_tx.clone();
        std::thread::spawn(move || {
            let paths: Vec<&std::path::Path> = interesting.iter().map(|p| p.as_path()).collect();
            let flags = match backend.paths_ignored(&paths) {
                Ok(flags) => flags,
                Err(_) => {
                    // Without a verdict, assume the events matter.
                    let _ = msg_tx.send(Msg::AutoRefresh);
                    return;
                }
            };
            if flags.into_iter().any(|ignored| !ignored) {
                let _ = msg_tx.send(Msg::AutoRefresh);
            }
        });
    })
    .ok()?;
    debouncer
        .watcher()
        .watch(&watchdir, RecursiveMode::Recursive)
        .ok()?;
    Some(debouncer)
}

/// Whether `path` is `.git` plumbing whose change cannot alter what rgit
/// shows. Index, HEAD and refs do matter (staging, branch switches).
fn is_git_internal(workdir: &Path, path: &Path) -> bool {
    let Ok(rel) = path.strip_prefix(workdir) else {
        return false;
    };
    let mut parts = rel.components();
    if parts.next() != Some(std::path::Component::Normal(".git".as_ref())) {
        return false;
    }
    let rest = rel
        .strip_prefix(".git")
        .expect("checked first component above");
    if rest.as_os_str().is_empty() {
        return true;
    }
    !(rest == Path::new("index") || rest == Path::new("HEAD") || rest.starts_with("refs"))
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
        let started = Instant::now();
        let result = work(&*backend);
        tracing::info!(
            target: "startup",
            elapsed_ms = started.elapsed().as_millis(),
            "git refresh completed"
        );
        let _ = msg_tx.send(Msg::Refreshed(Box::new(result)));
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
    sign: bool,
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

    // Commit through libgit2 (hooks already ran), signing natively when asked.
    if sign {
        // The TUI freezes while this runs; a tty pinentry/ssh-keygen prompt
        // then owns the screen, so say where the passphrase goes up front.
        let _ = msg_tx.send(Msg::HookOutput(
            "signing: if your key has a passphrase, answer the prompt on screen".into(),
        ));
    }
    let commit = {
        let backend = backend.clone();
        let message = message.clone();
        tokio::task::spawn_blocking(move || {
            backend.commit_with(
                &message,
                &rgit_git::CommitOptions {
                    amend,
                    no_verify: true,
                    sign: sign.then(String::new),
                    ..Default::default()
                },
            )
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
        ConsoleOp::Fetch => backend.fetch(None, &[], &Default::default(), &report),
        ConsoleOp::Pull => backend.pull(None, None, &Default::default(), &report),
        ConsoleOp::Push {
            force,
            force_with_lease,
            set_upstream,
            remote,
        } => backend.push(
            remote.as_deref(),
            force,
            force_with_lease,
            set_upstream,
            &report,
        ),
        ConsoleOp::Merge { rev, no_ff } => backend.merge(&rev, no_ff, false, &report),
        ConsoleOp::RebaseOnto(rev) => backend.rebase_onto(&rev, &report),
        ConsoleOp::Sync => backend.sync(&report).map(drop),
        ConsoleOp::Submit => backend.submit_stack(&report).map(|notes| {
            for n in notes {
                report(rgit_git::OpProgress::Line(n));
            }
        }),
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
    enable_mouse_capture();
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
                if auto_restack && let Some(note) = auto_restack_note(&*backend)? {
                    let _ = tx.send(Msg::AutoRestackNote(note));
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
    let since = backend.index_second();
    let result = match mutation {
        Mutation::StageAll => backend.stage_all(),
        Mutation::UnstageAll => backend.unstage_all(),
        Mutation::StageFile(path) => backend.stage_file(path),
        Mutation::UnstageFile(path) => backend.unstage_file(path),
        Mutation::StageFiles(paths) => paths.iter().try_for_each(|p| backend.stage_file(p)),
        Mutation::UnstageFiles(paths) => paths.iter().try_for_each(|p| backend.unstage_file(p)),
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
        Mutation::StashPush => backend.stash_push(false).map(drop),
        Mutation::StashPushMessage(msg) => backend.stash_push_message(msg, false).map(drop),
        Mutation::StashPop(index) => backend.stash_pop(*index),
        Mutation::StashApply(index) => backend.stash_apply(*index),
        Mutation::StashDrop(index) => backend.stash_drop(*index),
        Mutation::RenameBranch { old, new } => backend.rename_branch(old, new),
        Mutation::AddRemote { name, url } => backend.add_remote(name, url),
        Mutation::RemoveRemote(name) => backend.remove_remote(name),
        Mutation::SetRemoteUrl { name, url } => backend.set_remote_url(name, url),
        Mutation::RenameRemote { old, new } => backend.rename_remote(old, new),
        Mutation::AddWorktree { name, path } => backend.add_worktree(name, path),
        Mutation::RemoveWorktree(name) => backend.remove_worktree(name, false),
        Mutation::Extend => backend.commit_extend(),
        Mutation::RebaseAbort => backend.rebase_abort().map(drop),
        Mutation::RebaseContinue => backend.rebase_continue().map(drop),
        Mutation::RebaseSkip => backend.rebase_skip().map(drop),
        Mutation::Undo => backend.undo().map(drop),
        Mutation::Redo => backend.redo().map(drop),
        Mutation::Absorb => backend.absorb().map(drop),
        Mutation::Restack => backend.restack().map(drop),
        Mutation::Bisect(args) => backend.bisect(args).map(drop),
        Mutation::Reset { rev, mode } => backend.reset(rev, *mode),
        Mutation::CherryPick(rev) => backend.cherry_pick(rev, false),
        Mutation::Revert(rev) => backend.revert(rev, false),
        Mutation::ResolveConflict { path, ours } => backend.resolve_conflict(path, *ours),
        Mutation::CreateTag(name) => backend.create_tag(name, ""),
        Mutation::DeleteTag(name) => backend.delete_tag(name),
        Mutation::DeleteBranch(name) => backend.delete_branch(name, true),
        Mutation::Reword { rev, message } => backend.reword(rev, message),
        Mutation::Squash(rev) => backend.squash(rev),
        Mutation::Uncommit(n) => backend.uncommit(*n),
        Mutation::Clean => {
            let opts = rgit_git::CleanOptions {
                dirs: true,
                ..Default::default()
            };
            backend
                .clean_candidates(&opts)
                .and_then(|items| backend.clean_remove(&items, &opts))
                .map(drop)
        }
        Mutation::StackNext => stack_move(backend, true),
        Mutation::StackPrev => stack_move(backend, false),
        Mutation::Reorder { rev, target } => backend.reorder(rev, target, true),
        Mutation::Split { rev, paths } => backend.split(rev, paths),
        Mutation::RemovePath(path) => backend
            .remove_paths(
                std::slice::from_ref(path),
                rgit_git::RmOptions {
                    recursive: true,
                    force: true,
                    ..Default::default()
                },
            )
            .map(drop),
        Mutation::MovePath { from, to } => backend.move_path(from, to, false, false).map(drop),
    };
    let _ = backend.smudge_racy(since);
    result
}

/// Render a window of `path` around `line` (1-based) as preview sections, with
/// gutter line numbers and the hit line emphasized. Reads from the working tree.
fn snippet_sections(dir: &Path, path: &str, line: usize) -> Vec<rgit_model::Section> {
    use rgit_model::{NodeKind, Section, Span, Style};
    const CONTEXT: usize = 10;
    let Ok(text) = std::fs::read_to_string(dir.join(path)) else {
        return vec![Section::leaf(
            "snippet/none",
            NodeKind::Info,
            vec![Span::new("cannot read file".to_owned(), Style::Dim)],
        )];
    };
    let highlighted = rgit_model::highlight_file(path, &text);
    let count = highlighted.len();
    let hit = line.saturating_sub(1);
    let start = hit.saturating_sub(CONTEXT);
    let end = (hit + CONTEXT + 1).min(count);
    let width = end.to_string().len();
    (start..end)
        .map(|i| {
            let n = i + 1;
            let marker = if n == line { "\u{25b8}" } else { " " };
            let mut spans = vec![Span::new(format!("{marker}{n:>width$}  "), Style::Dim)];
            spans.extend(highlighted.get(i).cloned().unwrap_or_default());
            Section::leaf(format!("snippet/{n}"), NodeKind::Info, spans)
        })
        .collect()
}

/// Instant lexical-only results for the finder's live typing: literal grep,
/// mapped to code hits (tagged "text"), no index or embedder involved.
fn grep_hits(backend: &dyn GitBackend, query: &str) -> Result<Vec<crate::app::CodeHit>, GitError> {
    let matches = backend.grep_query(&rgit_git::GrepQuery {
        pattern: query.to_owned(),
        regex: false,
        path: None,
        exts: Vec::new(),
    })?;
    Ok(matches
        .into_iter()
        .take(50)
        .map(|m| crate::app::CodeHit {
            path: m.path,
            line: m.line,
            score: 0.0,
            tag: "text",
            preview: m.text,
        })
        .collect())
}

/// Fuse literal grep and semantic-index results with reciprocal-rank fusion,
/// bucketed to the chunk window so a lexical and a semantic hit in the same
/// region reinforce each other. History-boosts the semantic side, and degrades
/// to grep when there is no index. Mirrors the CLI's `code_search`.
fn code_search(
    backend: &dyn GitBackend,
    query: &str,
) -> Result<Vec<crate::app::CodeHit>, GitError> {
    use std::collections::HashMap;
    const K: f64 = 60.0;
    const CHUNK_STEP: usize = 30;
    const POOL: usize = 40;

    let semantic = match rgit_index::load(&rgit_index::index_path(backend.workdir())) {
        Some(index) => {
            let boost = backend
                .file_activity(500)
                .map(|a| rgit_git::activity_weights(&a))
                .unwrap_or_default();
            rgit_index::Embedder::new()
                .and_then(|e| rgit_index::search_boosted(&index, &e, query, POOL, &boost, 0.5))
                .unwrap_or_default()
        }
        None => Vec::new(),
    };
    let lexical = backend
        .grep_query(&rgit_git::GrepQuery {
            pattern: query.to_owned(),
            regex: false,
            path: None,
            exts: Vec::new(),
        })
        .unwrap_or_default();

    #[derive(Default)]
    struct Fused {
        score: f64,
        lexical: bool,
        semantic: bool,
        line: usize,
        preview: String,
    }
    let bucket = |line: usize| (line.saturating_sub(1) / CHUNK_STEP) * CHUNK_STEP + 1;
    let mut acc: HashMap<(String, usize), Fused> = HashMap::new();
    for (rank, m) in lexical.iter().enumerate().take(POOL) {
        let e = acc.entry((m.path.clone(), bucket(m.line))).or_default();
        e.score += 1.0 / (K + rank as f64);
        e.lexical = true;
        if e.line == 0 {
            e.line = m.line;
            e.preview = m.text.clone();
        }
    }
    for (rank, h) in semantic.iter().enumerate() {
        let e = acc
            .entry((h.path.clone(), bucket(h.start_line)))
            .or_default();
        e.score += 1.0 / (K + rank as f64);
        e.semantic = true;
        if e.line == 0 {
            e.line = h.start_line;
            e.preview = h.preview.lines().next().unwrap_or("").to_owned();
        }
    }
    let mut hits: Vec<crate::app::CodeHit> = acc
        .into_iter()
        .map(|((path, _), f)| crate::app::CodeHit {
            path,
            line: f.line,
            score: f.score as f32,
            tag: match (f.lexical, f.semantic) {
                (true, true) => "both",
                (false, true) => "semantic",
                _ => "text",
            },
            preview: f.preview,
        })
        .collect();
    hits.sort_by(|a, b| b.score.total_cmp(&a.score));
    hits.truncate(30);
    Ok(hits)
}

/// Check out the current branch's stacked child (`up`) or its parent, using the
/// recorded stack relationships. A no-op error names the missing direction.
fn stack_move(backend: &dyn GitBackend, up: bool) -> Result<(), GitError> {
    let current = backend
        .status()?
        .head
        .branch
        .ok_or_else(|| GitError::Other("not on a branch".to_owned()))?;
    let parents = backend.stack_parents()?;
    let target = if up {
        // The child is the branch whose stack parent is the current branch.
        let children: Vec<&String> = parents
            .iter()
            .filter(|(_, p)| p.as_deref() == Some(current.as_str()))
            .map(|(b, _)| b)
            .collect();
        match children.as_slice() {
            [] => {
                return Err(GitError::Other(format!(
                    "{current} has no branch stacked on it"
                )));
            }
            [one] => (*one).clone(),
            many => {
                return Err(GitError::Other(format!(
                    "multiple children: {}",
                    many.iter()
                        .map(|s| s.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                )));
            }
        }
    } else {
        parents
            .into_iter()
            .find(|(b, _)| *b == current)
            .and_then(|(_, p)| p)
            .ok_or_else(|| GitError::Other(format!("{current} has no stack parent")))?
    };
    backend.checkout_branch(&target)
}

/// Suspend the TUI and run `rgit --human <args>` in the user's terminal so it
/// can drive its own editor (interactive rebase, `--continue`), then refresh.
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
        // rgit itself, which runs these (commit -S, rebase -i) natively.
        let exe = std::env::current_exe().unwrap_or_else(|_| "rgit".into());
        let mut cmd = std::process::Command::new(exe);
        cmd.arg("--human").args(&args).current_dir(&workdir);
        for (k, v) in &env {
            cmd.env(k, v);
        }
        cmd.status()
    })
    .await;
    *terminal = ratatui::init();
    enable_mouse_capture();
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

/// Opening in the editor that spawned rgit, when it left a trace in the
/// environment. `Fire` commands return immediately (the CLI talks to an
/// already-running editor), so the caller redraws the TUI and does not wait;
/// only the nvim path holds the terminal.
#[derive(Debug, PartialEq, Eq)]
enum HostEditor {
    /// nvim job/`:!` child: `$NVIM` names the host's RPC socket.
    Nvim(String),
    /// vim clientserver child: `$VIM_SERVERNAME` names the host instance.
    Vim(String),
    /// GUI editor launched rgit from its integrated terminal; the app's CLI
    /// targets the running instance (usually the window we came from).
    Gui(&'static str),
}

impl HostEditor {
    /// Detect the spawning editor from the environment, before falling back
    /// to `$EDITOR`. `$NVIM` wins over a `$EDITOR=nvim` mention: rgit running
    /// inside nvim opens the file in the host, not as a nested instance.
    fn detect() -> Option<HostEditor> {
        Self::detect_with(|k| std::env::var(k).ok().filter(|v| !v.is_empty()))
    }

    /// The pure decision, parameterized over the env lookup so tests can pass
    /// a fixed environment without mutating process state.
    fn detect_with(env: impl Fn(&str) -> Option<String>) -> Option<HostEditor> {
        env("NVIM").map(HostEditor::Nvim).or_else(|| {
            env("VIM_SERVERNAME").map(HostEditor::Vim).or_else(|| {
                let prog = env("TERM_PROGRAM")?;
                match prog.as_str() {
                    "vscode" | "Cursor" | "Code - OSS" => Some(HostEditor::Gui("code")),
                    "zed" => Some(HostEditor::Gui("zed")),
                    _ => None,
                }
            })
        })
    }

    /// The shell command that opens `$1` at `line`:`col` in the host editor.
    fn open_command(&self, line: usize, col: usize) -> String {
        match self {
            // Prefer nvr over the RPC socket; fall back to bare nvim --remote,
            // which opens the file but not at the position.
            HostEditor::Nvim(_) => format!(
                "if command -v nvr >/dev/null 2>&1; then nvr \"+call cursor({line},{col})\" --remote-tab \"$1\"; else nvim --remote-tab \"$1\"; fi"
            ),
            HostEditor::Vim(name) => format!(
                "vim --servername {name} --remote-tab \"+call cursor({line},{col})\" \"$1\""
            ),
            HostEditor::Gui(cli) => format!("{cli} -g \"$1\":{line}:{col}"),
        }
    }

    /// Run the host-open command on `path`, inheriting the terminal. Returns
    /// whether it exited successfully.
    fn run(&self, path: &Path, line: usize, col: usize) -> std::io::Result<bool> {
        let status = std::process::Command::new("sh")
            .arg("-c")
            .arg(self.open_command(line.max(1), col.max(1)))
            .arg("sh")
            .arg(path)
            .status()?;
        Ok(status.success())
    }
}

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

/// Open `path` in `editor` at `line`. The `+<line>` argument is understood by
/// vim, nvim, emacs, nano, and most terminal editors; it degrades to opening the
/// file at the top for the few that ignore it.
fn run_editor_at(editor: &str, path: &Path, line: usize, col: usize) -> std::io::Result<bool> {
    let status = std::process::Command::new("sh")
        .arg("-c")
        .arg(editor_open_command(editor, line.max(1), col.max(1)))
        .arg("sh")
        .arg(path)
        .status()?;
    Ok(status.success())
}

/// The shell command that opens `$1` at `line`:`col`, using each known editor's
/// own jump syntax; unknown editors fall back to the near-universal `+line`
/// (column dropped), which vi, nano, emacs, and most others accept.
fn editor_open_command(editor: &str, line: usize, col: usize) -> String {
    // Match on the program name (first word of $EDITOR), ignoring any flags.
    let prog = editor
        .split_whitespace()
        .next()
        .and_then(|p| p.rsplit(['/', '\\']).next())
        .unwrap_or(editor);
    match prog {
        // VS Code / derivatives: `-g file:line:col`.
        "code" | "code-insiders" | "codium" | "cursor" | "windsurf" => {
            format!("{editor} -g \"$1\":{line}:{col}")
        }
        // Vim family: set the cursor to line/column after opening.
        "vim" | "nvim" | "vi" | "view" | "gvim" => {
            format!("{editor} \"+call cursor({line},{col})\" \"$1\"")
        }
        // Emacs accepts `+line:col`.
        "emacs" | "emacsclient" => format!("{editor} +{line}:{col} \"$1\""),
        // Nano uses `+line,col`.
        "nano" => format!("{editor} +{line},{col} \"$1\""),
        // Helix uses `file:line`.
        "hx" | "helix" => format!("{editor} \"$1\":{line}"),
        // Universal fallback: line only.
        _ => format!("{editor} +{line} \"$1\""),
    }
}

fn strip_comments(raw: &str) -> String {
    raw.lines()
        .filter(|line| !line.starts_with('#'))
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_owned()
}

/// Copy `text` to the system clipboard. OSC 52 first (direct, then tmux-wrapped
/// when `$TMUX` is set, since tmux filters bare sequences), then a platform
/// clipboard command - OSC 52 is ignored by default on several terminals, and
/// a silent drop would lose the yank.
fn copy_to_clipboard(text: &str) {
    use std::io::Write;
    let payload = base64_encode(text.as_bytes());
    let seq = format!("\x1b]52;c;{}\x07", payload);
    let mut stdout = std::io::stdout();
    if std::env::var_os("TMUX").is_some() {
        // tmux's passthrough: DCS tmux; <seq> ST, with the semicolon escaped.
        let wrapped = format!("\x1bPtmux;\x1b]52;c;{}\x07\x1b\\", payload);
        let _ = stdout.write_all(wrapped.as_bytes());
    } else {
        let _ = stdout.write_all(seq.as_bytes());
    }
    let _ = stdout.flush();
    if osc52_untrusted() && clipboard_command(text).is_err() {
        tracing::debug!("clipboard: OSC 52 sent and no fallback command available");
    }
}

/// Whether the output may not honor OSC 52, so a subprocess fallback is worth
/// running too: the terminal is unknown or on the known-ignore list. The copy
/// is still attempted; a duplicate copy is harmless, a missing one is not.
fn osc52_untrusted() -> bool {
    match std::env::var("TERM_PROGRAM") {
        Ok(program) => matches!(
            program.as_str(),
            "Apple_Terminal" | "iTerm.app" | "Hyper" | "vscode"
        ),
        Err(_) => true,
    }
}

/// The first available platform clipboard command, run with `text` on stdin.
fn clipboard_command(text: &str) -> std::io::Result<()> {
    use std::io::Write;
    use std::process::{Command, Stdio};
    let (name, args): (&str, &[&str]) = if cfg!(target_os = "macos") {
        ("pbcopy", &[])
    } else if std::env::var_os("WAYLAND_DISPLAY").is_some() {
        ("wl-copy", &[])
    } else {
        ("xclip", &["-selection", "clipboard"])
    };
    let mut child = Command::new(name)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| {
            tracing::debug!("clipboard: no {name}: {e}");
            e
        })?;
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(text.as_bytes());
    }
    // Do not wait: a hung clipboard manager must not stall the event loop.
    Ok(())
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
    use super::{
        HostEditor, base64_encode, editor_open_command, grep_hits, restack_note, snippet_sections,
    };
    use rgit_git::RestackOutcome;

    #[test]
    fn host_editor_detect_reads_the_environment() {
        // (NVIM, VIM_SERVERNAME, TERM_PROGRAM) -> expected variant.
        let cases: &[(&str, &str, &str, Option<&str>)] = &[
            ("/tmp/nvim-socket", "", "", Some("nvim")),
            ("", "MAIN", "", Some("vim")),
            ("", "", "vscode", Some("gui")),
            ("", "", "Cursor", Some("gui")),
            ("", "", "zed", Some("gui")),
            ("", "", "tmux", None),
            ("", "", "", None),
            ("/tmp/nvim-socket", "MAIN", "vscode", Some("nvim")),
        ];
        for (nvim, servername, term, want) in cases {
            let vars = [
                ("NVIM", *nvim),
                ("VIM_SERVERNAME", *servername),
                ("TERM_PROGRAM", *term),
            ];
            let env = |k: &str| {
                vars.iter()
                    .find(|(name, _)| *name == k)
                    .map(|(_, v)| *v)
                    .filter(|v| !v.is_empty())
                    .map(str::to_owned)
            };
            let got = HostEditor::detect_with(env).map(|h| match h {
                HostEditor::Nvim(_) => "nvim",
                HostEditor::Vim(_) => "vim",
                HostEditor::Gui(_) => "gui",
            });
            assert_eq!(
                got.map(String::from),
                want.map(|s| s.to_string()),
                "NVIM={nvim:?} VIM_SERVERNAME={servername:?} TERM_PROGRAM={term:?}"
            );
        }
    }

    #[test]
    fn host_editor_open_command_targets_the_running_instance() {
        assert_eq!(
            HostEditor::Nvim("/tmp/s".into()).open_command(12, 5),
            "if command -v nvr >/dev/null 2>&1; then nvr \"+call cursor(12,5)\" --remote-tab \"$1\"; else nvim --remote-tab \"$1\"; fi"
        );
        assert_eq!(
            HostEditor::Vim("MAIN".into()).open_command(12, 5),
            "vim --servername MAIN --remote-tab \"+call cursor(12,5)\" \"$1\""
        );
        assert_eq!(
            HostEditor::Gui("code").open_command(12, 5),
            "code -g \"$1\":12:5"
        );
    }

    #[test]
    fn editor_open_command_uses_each_editors_jump_syntax() {
        assert_eq!(
            editor_open_command("nvim", 12, 5),
            "nvim \"+call cursor(12,5)\" \"$1\""
        );
        assert_eq!(editor_open_command("emacs", 12, 5), "emacs +12:5 \"$1\"");
        assert_eq!(editor_open_command("nano", 12, 5), "nano +12,5 \"$1\"");
        assert_eq!(
            editor_open_command("code --wait", 12, 5),
            "code --wait -g \"$1\":12:5"
        );
        // A full path to the program still matches on its basename.
        assert_eq!(
            editor_open_command("/usr/bin/vim", 3, 1),
            "/usr/bin/vim \"+call cursor(3,1)\" \"$1\""
        );
        // Unknown editors fall back to the universal `+line` (column dropped).
        assert_eq!(editor_open_command("ed", 7, 2), "ed +7 \"$1\"");
    }

    fn temp_repo(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("rgit-finder-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        for a in [
            vec!["init", "-q", "-b", "main"],
            vec!["config", "user.email", "t@t"],
            vec!["config", "user.name", "t"],
        ] {
            std::process::Command::new("git")
                .arg("-C")
                .arg(&dir)
                .args(&a)
                .status()
                .unwrap();
        }
        dir
    }

    #[test]
    fn grep_hits_finds_the_query_as_a_text_hit() {
        let dir = temp_repo("grep");
        std::fs::write(dir.join("f.rs"), "fn alpha() {}\nfn zztokenzz() {}\n").unwrap();
        for a in [vec!["add", "f.rs"], vec!["commit", "-qm", "c"]] {
            std::process::Command::new("git")
                .arg("-C")
                .arg(&dir)
                .args(&a)
                .status()
                .unwrap();
        }
        let backend = rgit_git::Git2Backend::discover(&dir).unwrap();
        let hits = grep_hits(&backend, "zztokenzz").unwrap();
        assert!(
            hits.iter()
                .any(|h| h.path == "f.rs" && h.line == 2 && h.tag == "text"),
            "grep_hits should find the token at f.rs:2"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn snippet_sections_windows_around_the_hit_line() {
        let dir = temp_repo("snippet");
        let body: String = (1..=40).map(|n| format!("line {n}\n")).collect();
        std::fs::write(dir.join("f.txt"), body).unwrap();
        let sections = snippet_sections(&dir, "f.txt", 20);
        // A +/-10 window around line 20 is at most 21 rows and is non-empty.
        assert!(!sections.is_empty() && sections.len() <= 21);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn restack_note_summarizes_moves_and_conflicts() {
        assert_eq!(restack_note(&RestackOutcome::default()), None);
        let moved = RestackOutcome {
            restacked: vec!["a -> main".into(), "b -> a".into()],
            conflicted: vec![],
        };
        assert_eq!(
            restack_note(&moved).as_deref(),
            Some("auto-restack (restacked 2)")
        );
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
