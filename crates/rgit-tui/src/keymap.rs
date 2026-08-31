use std::collections::HashMap;
use std::sync::OnceLock;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::app::Msg;

/// A remappable normal-mode command. Structural keys (Enter, Tab, Esc) are
/// fixed; every command below is bound to a default key and can be rebound to
/// any key spec (a bare char or a chord like `C-f`, `M-b`, `C-x o`) in `[keys]`.
#[derive(Debug, Clone, Copy)]
pub enum Command {
    Quit,
    CursorDown,
    CursorUp,
    ToggleSelect,
    Stage,
    Unstage,
    StageAll,
    UnstageAll,
    Discard,
    CommitMenu,
    RebaseMenu,
    MergeMenu,
    TagMenu,
    ResetMenu,
    CherryPick,
    Revert,
    Operations,
    BranchMenu,
    StashMenu,
    StashPop,
    Log,
    SessionLog,
    Refs,
    RemoteMenu,
    WorktreeMenu,
    DiffPrompt,
    Fetch,
    Pull,
    PushMenu,
    Refresh,
    Help,
    Palette,
    Search,
    SearchNext,
    SearchPrev,
    /// Remove the tracked file under the cursor (git rm).
    RmFile,
    /// Rename/move the tracked file under the cursor (git mv).
    MvFile,
    /// Search the codebase by meaning (and text), opening a results view.
    CodeSearch,
    // Navigation and selection (emacs point motion, vim scroll, pane focus).
    ForwardChar,
    BackwardChar,
    NextLine,
    PrevLine,
    LineStart,
    LineEnd,
    WordForward,
    WordBack,
    WordEnd,
    FirstNonBlank,
    CursorTop,
    CursorBottom,
    HalfDown,
    HalfUp,
    SetMark,
    OtherWindow,
    OpenEditor,
    FocusPreview,
    FocusNav,
}

impl Command {
    fn to_msg(self) -> Msg {
        match self {
            Command::Quit => Msg::Quit,
            Command::CursorDown => Msg::CursorDown,
            Command::CursorUp => Msg::CursorUp,
            Command::ToggleSelect => Msg::ToggleSelect,
            Command::Stage => Msg::Stage,
            Command::Unstage => Msg::Unstage,
            Command::StageAll => Msg::StageAll,
            Command::UnstageAll => Msg::UnstageAll,
            Command::Discard => Msg::Discard,
            Command::CommitMenu => Msg::CommitMenu,
            Command::RebaseMenu => Msg::RebaseMenu,
            Command::MergeMenu => Msg::MergeMenu,
            Command::TagMenu => Msg::TagMenu,
            Command::ResetMenu => Msg::ResetMenu,
            Command::CherryPick => Msg::CherryPick,
            Command::Revert => Msg::Revert,
            Command::Operations => Msg::OperationsMenu,
            Command::BranchMenu => Msg::BranchMenu,
            Command::StashMenu => Msg::StashMenu,
            Command::StashPop => Msg::StashPop,
            Command::SessionLog => Msg::OpenSessionLog,
            Command::Log => Msg::Log,
            Command::Refs => Msg::Refs,
            Command::RemoteMenu => Msg::RemoteMenu,
            Command::WorktreeMenu => Msg::WorktreeMenu,
            Command::DiffPrompt => Msg::DiffPrompt,
            Command::Fetch => Msg::Fetch,
            Command::Pull => Msg::Pull,
            Command::PushMenu => Msg::PushMenu,
            Command::Refresh => Msg::Refresh,
            Command::Help => Msg::HelpToggle,
            Command::Palette => Msg::PaletteOpen,
            Command::Search => Msg::SearchOpen,
            Command::SearchNext => Msg::SearchNext,
            Command::SearchPrev => Msg::SearchPrev,
            Command::RmFile => Msg::RmAtCursor,
            Command::MvFile => Msg::MvAtCursor,
            Command::CodeSearch => Msg::CodeFinderOpen,
            Command::ForwardChar => Msg::ColRight,
            Command::BackwardChar => Msg::ColLeft,
            Command::NextLine => Msg::CursorDown,
            Command::PrevLine => Msg::CursorUp,
            Command::LineStart => Msg::ColLineStart,
            Command::LineEnd => Msg::ColLineEnd,
            Command::WordForward => Msg::ColWordForward,
            Command::WordBack => Msg::ColWordBack,
            Command::WordEnd => Msg::ColWordEnd,
            Command::FirstNonBlank => Msg::ColFirstNonBlank,
            Command::CursorTop => Msg::CursorTop,
            Command::CursorBottom => Msg::CursorBottom,
            Command::HalfDown => Msg::CursorHalfDown,
            Command::HalfUp => Msg::CursorHalfUp,
            Command::SetMark => Msg::ToggleCharSelect,
            Command::OtherWindow => Msg::OtherWindow,
            Command::OpenEditor => Msg::OpenEditor,
            Command::FocusPreview => Msg::FocusPreview,
            Command::FocusNav => Msg::FocusNav,
        }
    }

    /// The config action name (and a few aliases) for a command.
    fn from_name(name: &str) -> Option<Command> {
        Some(match name {
            "quit" => Command::Quit,
            "cursor-down" | "down" => Command::CursorDown,
            "cursor-up" | "up" => Command::CursorUp,
            "toggle-select" | "select" => Command::ToggleSelect,
            "stage" => Command::Stage,
            "unstage" => Command::Unstage,
            "stage-all" => Command::StageAll,
            "unstage-all" => Command::UnstageAll,
            "discard" => Command::Discard,
            "commit" => Command::CommitMenu,
            "rebase" | "rebase-menu" => Command::RebaseMenu,
            "merge" | "merge-menu" => Command::MergeMenu,
            "tag" | "tag-menu" => Command::TagMenu,
            "reset" | "reset-menu" => Command::ResetMenu,
            "cherry-pick" => Command::CherryPick,
            "revert" => Command::Revert,
            "operations" | "ops" => Command::Operations,
            "branch" | "branch-menu" => Command::BranchMenu,
            "stash" | "stash-menu" => Command::StashMenu,
            "stash-pop" => Command::StashPop,
            "session-log" | "log-window" => Command::SessionLog,
            "log" => Command::Log,
            "refs" => Command::Refs,
            "remote" | "remote-menu" => Command::RemoteMenu,
            "worktree" | "worktree-menu" => Command::WorktreeMenu,
            "diff" => Command::DiffPrompt,
            "fetch" => Command::Fetch,
            "pull" => Command::Pull,
            "push" | "push-menu" => Command::PushMenu,
            "refresh" => Command::Refresh,
            "help" => Command::Help,
            "palette" => Command::Palette,
            "search" => Command::Search,
            "search-next" => Command::SearchNext,
            "search-prev" => Command::SearchPrev,
            "rm" | "rm-file" => Command::RmFile,
            "mv" | "mv-file" => Command::MvFile,
            "code-search" | "search-code" => Command::CodeSearch,
            "forward-char" => Command::ForwardChar,
            "backward-char" => Command::BackwardChar,
            "next-line" => Command::NextLine,
            "prev-line" | "previous-line" => Command::PrevLine,
            "line-start" | "beginning-of-line" => Command::LineStart,
            "line-end" | "end-of-line" => Command::LineEnd,
            "word-forward" | "forward-word" => Command::WordForward,
            "word-back" | "backward-word" => Command::WordBack,
            "word-end" => Command::WordEnd,
            "first-non-blank" => Command::FirstNonBlank,
            "buffer-start" | "top" => Command::CursorTop,
            "buffer-end" | "bottom" => Command::CursorBottom,
            "half-down" | "scroll-down" => Command::HalfDown,
            "half-up" | "scroll-up" => Command::HalfUp,
            "set-mark" | "mark" => Command::SetMark,
            "other-window" | "switch-pane" => Command::OtherWindow,
            "open-editor" | "editor" => Command::OpenEditor,
            "focus-preview" => Command::FocusPreview,
            "focus-nav" => Command::FocusNav,
            _ => return None,
        })
    }
}

/// The default character bindings, before any config override.
const DEFAULTS: &[(char, Command)] = &[
    ('q', Command::Quit),
    ('j', Command::CursorDown),
    ('k', Command::CursorUp),
    ('v', Command::ToggleSelect),
    ('s', Command::Stage),
    ('u', Command::Unstage),
    ('S', Command::StageAll),
    ('U', Command::UnstageAll),
    ('x', Command::Discard),
    ('c', Command::CommitMenu),
    ('r', Command::RebaseMenu),
    ('m', Command::MergeMenu),
    ('t', Command::TagMenu),
    ('O', Command::ResetMenu),
    ('A', Command::CherryPick),
    ('V', Command::Revert),
    ('o', Command::Operations),
    ('b', Command::BranchMenu),
    ('z', Command::StashMenu),
    ('L', Command::SessionLog),
    ('p', Command::StashPop),
    ('l', Command::Log),
    ('y', Command::Refs),
    ('M', Command::RemoteMenu),
    ('W', Command::WorktreeMenu),
    ('d', Command::DiffPrompt),
    ('f', Command::Fetch),
    ('F', Command::Pull),
    ('P', Command::PushMenu),
    ('g', Command::Refresh),
    ('?', Command::Help),
    (':', Command::Palette),
    ('/', Command::Search),
    ('n', Command::SearchNext),
    ('N', Command::SearchPrev),
    ('D', Command::RmFile),
    ('R', Command::MvFile),
    ('C', Command::CodeSearch),
];

/// A normalized key press: a base code plus the modifiers that select a
/// binding. Shift is folded into the char itself, so only Ctrl, Alt, and the
/// emacs `C-x` prefix are tracked here.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Chord {
    /// Preceded by the `C-x` prefix key (emacs two-key chords).
    pub prefix: bool,
    pub ctrl: bool,
    pub alt: bool,
    pub code: ChordCode,
}

/// The base key of a [`Chord`]: a character or a named non-text key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ChordCode {
    Char(char),
    Up,
    Down,
    Left,
    Right,
    Enter,
    Tab,
    Esc,
    Backspace,
    Home,
    End,
}

impl Chord {
    /// Normalize a key event into a chord, or None for keys with no base code we
    /// bind (function keys, etc.). Shift is ignored: it is already reflected in
    /// the char.
    fn from_key(key: KeyEvent) -> Option<Chord> {
        let code = match key.code {
            KeyCode::Char(c) => ChordCode::Char(c),
            KeyCode::Up => ChordCode::Up,
            KeyCode::Down => ChordCode::Down,
            KeyCode::Left => ChordCode::Left,
            KeyCode::Right => ChordCode::Right,
            KeyCode::Enter => ChordCode::Enter,
            KeyCode::Tab => ChordCode::Tab,
            KeyCode::Esc => ChordCode::Esc,
            KeyCode::Backspace => ChordCode::Backspace,
            KeyCode::Home => ChordCode::Home,
            KeyCode::End => ChordCode::End,
            _ => return None,
        };
        Some(Chord {
            prefix: false,
            ctrl: key.modifiers.contains(KeyModifiers::CONTROL),
            alt: key.modifiers.contains(KeyModifiers::ALT),
            code,
        })
    }
}

/// Parse a config key spec (e.g. `"C-f"`, `"M-b"`, `"C-x o"`, `"Up"`, `"Spc"`,
/// or a bare `"s"`) into a chord. Returns None for anything unrecognized so a
/// typo cannot silently unbind a default.
fn parse_chord(spec: &str) -> Option<Chord> {
    let spec = spec.trim();
    let (prefix, rest) = match spec.strip_prefix("C-x ") {
        Some(r) => (true, r.trim()),
        None => (false, spec),
    };
    let mut ctrl = false;
    let mut alt = false;
    let mut token = rest;
    loop {
        if let Some(r) = token.strip_prefix("C-").or_else(|| token.strip_prefix("^")) {
            ctrl = true;
            token = r;
        } else if let Some(r) = token.strip_prefix("M-").or_else(|| token.strip_prefix("A-")) {
            alt = true;
            token = r;
        } else {
            break;
        }
    }
    let code = match token {
        "Up" => ChordCode::Up,
        "Down" => ChordCode::Down,
        "Left" => ChordCode::Left,
        "Right" => ChordCode::Right,
        "Enter" | "RET" | "Ret" => ChordCode::Enter,
        "Tab" => ChordCode::Tab,
        "Esc" => ChordCode::Esc,
        "Backspace" | "BS" => ChordCode::Backspace,
        "Home" => ChordCode::Home,
        "End" => ChordCode::End,
        "Spc" | "Space" | "SPC" => ChordCode::Char(' '),
        other => {
            let mut chars = other.chars();
            let c = chars.next()?;
            if chars.next().is_some() {
                return None;
            }
            ChordCode::Char(c)
        }
    };
    Some(Chord {
        prefix,
        ctrl,
        alt,
        code,
    })
}

static BINDINGS: OnceLock<HashMap<Chord, Command>> = OnceLock::new();

/// The active keybinding profile.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Profile {
    /// Single-letter actions plus emacs/readline navigation (magit-style).
    Magit,
    /// hjkl motion, visual select, and a Space leader for actions.
    Vim,
}

static PROFILE: OnceLock<Profile> = OnceLock::new();

pub fn profile() -> Profile {
    PROFILE.get().copied().unwrap_or(Profile::Magit)
}

/// Install the bindings and profile: defaults overlaid with `[keys]` overrides.
/// Each override maps an action name to a key spec (`"s"`, `"C-f"`, `"C-x o"`,
/// ...); an unknown action or an unparseable key is ignored so a malformed
/// entry cannot unbind the defaults.
pub fn init(overrides: &HashMap<String, String>, profile: Option<&str>) {
    let profile = match profile {
        Some(p) if p.eq_ignore_ascii_case("vim") => Profile::Vim,
        _ => Profile::Magit,
    };
    let _ = PROFILE.set(profile);
    let _ = BINDINGS.set(build_map(profile, overrides));
}

/// The emacs point-motion, selection, and pane chords shared by both profiles,
/// plus the profile's own scroll/arrow bindings.
fn nav_defaults(profile: Profile) -> Vec<(Chord, Command)> {
    let ctrl = |c| Chord { prefix: false, ctrl: true, alt: false, code: ChordCode::Char(c) };
    let alt = |c| Chord { prefix: false, ctrl: false, alt: true, code: ChordCode::Char(c) };
    let key = |code| Chord { prefix: false, ctrl: false, alt: false, code };
    let cx = |c| Chord { prefix: true, ctrl: false, alt: false, code: ChordCode::Char(c) };
    let mut v = vec![
        (ctrl('c'), Command::Quit),
        (ctrl('n'), Command::NextLine),
        (ctrl('p'), Command::PrevLine),
        (ctrl('o'), Command::OpenEditor),
        (cx('o'), Command::OtherWindow),
    ];
    match profile {
        Profile::Magit => v.extend([
            // Emacs point motion, word motion, and set-mark.
            (ctrl('f'), Command::ForwardChar),
            (ctrl('b'), Command::BackwardChar),
            (ctrl('a'), Command::LineStart),
            (ctrl('e'), Command::LineEnd),
            (ctrl(' '), Command::SetMark),
            (alt('f'), Command::WordForward),
            (alt('b'), Command::WordBack),
            (key(ChordCode::Down), Command::NextLine),
            (key(ChordCode::Up), Command::PrevLine),
            (key(ChordCode::Right), Command::FocusPreview),
            (key(ChordCode::Left), Command::FocusNav),
        ]),
        Profile::Vim => v.extend([
            (ctrl('d'), Command::HalfDown),
            (ctrl('u'), Command::HalfUp),
        ]),
    }
    v
}

fn build_map(profile: Profile, overrides: &HashMap<String, String>) -> HashMap<Chord, Command> {
    let mut map: HashMap<Chord, Command> = nav_defaults(profile).into_iter().collect();
    // The magit profile also binds every mnemonic action to a bare letter.
    if profile == Profile::Magit {
        for (ch, cmd) in DEFAULTS {
            map.insert(
                Chord {
                    prefix: false,
                    ctrl: false,
                    alt: false,
                    code: ChordCode::Char(*ch),
                },
                *cmd,
            );
        }
    }
    for (action, spec) in overrides {
        if let (Some(cmd), Some(chord)) = (Command::from_name(action), parse_chord(spec)) {
            map.insert(chord, cmd);
        }
    }
    map
}

/// Look up a chord in the active binding map. Before [`init`] runs (tests, early
/// startup) it falls back to the given profile's defaults, so each resolver sees
/// its own bindings rather than the wrong profile's.
fn resolve_chord(chord: Chord, fallback: Profile) -> Option<Msg> {
    match BINDINGS.get() {
        Some(map) => map.get(&chord).map(|c| c.to_msg()),
        None => build_map(fallback, &HashMap::new())
            .get(&chord)
            .map(|c| c.to_msg()),
    }
}

/// The command bound to a bare letter in the magit action set, for the vim
/// leader (which reuses the magit mnemonics regardless of the active profile).
fn lookup(c: char) -> Option<Command> {
    DEFAULTS.iter().find(|(k, _)| *k == c).map(|(_, cmd)| *cmd)
}

/// Resolve a key event into a message, or `None` if it is unbound. The magit
/// profile drives entirely off the binding map (mnemonic letters, emacs point
/// motion, set-mark, pane arrows); only the fixed structural keys fall through.
pub fn resolve_key(key: KeyEvent) -> Option<Msg> {
    let chord = Chord::from_key(key)?;
    if let Some(msg) = resolve_chord(chord, Profile::Magit) {
        return Some(msg);
    }
    // Structural keys stay fixed and are not remappable.
    match chord.code {
        ChordCode::Esc => Some(Msg::Quit),
        ChordCode::Tab => Some(Msg::ToggleFold),
        ChordCode::Enter => Some(Msg::Enter),
        _ => None,
    }
}

/// The second key of a `C-x` prefix chord (currently only `C-x o`). The runtime
/// arms the prefix; this resolves the follow-up key against the binding map.
pub fn resolve_prefixed(key: KeyEvent) -> Option<Msg> {
    let mut chord = Chord::from_key(key)?;
    chord.prefix = true;
    resolve_chord(chord, profile())
}

/// Normal-mode keys in the vim profile. Motion, charwise/linewise visual, and
/// yank keep their vim meaning on bare keys; on top of that, git actions are
/// bound directly on the letters that are NOT motions, so `s` stages and `c`
/// commits without a leader. The handful of git actions whose mnemonic collides
/// with a motion (log/branch/worktree/revert/refs/refresh) stay motions here and
/// live behind the Space leader instead (see [`leader_command`]). This follows
/// neogit's spirit without giving up rgit's full motion set or charwise select.
pub fn resolve_vim_key(key: KeyEvent) -> Option<Msg> {
    let chord = Chord::from_key(key)?;
    // The binding map holds the ctrl scroll/nav chords plus any user overrides,
    // and wins over the bare vim motions below (so a rebind takes effect).
    if let Some(msg) = resolve_chord(chord, Profile::Vim) {
        return Some(msg);
    }
    // A modified key that is not bound is swallowed, never treated as its bare
    // letter (so C-d does not fall through to `d`).
    if chord.ctrl || chord.alt {
        return None;
    }
    match key.code {
        KeyCode::Char(' ') => Some(Msg::LeaderOpen),
        // Motion and visual: unchanged vim behaviour.
        KeyCode::Char('j') | KeyCode::Down => Some(Msg::CursorDown),
        KeyCode::Char('k') | KeyCode::Up => Some(Msg::CursorUp),
        KeyCode::Char('h') | KeyCode::Left => Some(Msg::ColLeft),
        KeyCode::Char('l') | KeyCode::Right => Some(Msg::ColRight),
        KeyCode::Char('w') => Some(Msg::ColWordForward),
        KeyCode::Char('b') => Some(Msg::ColWordBack),
        KeyCode::Char('e') => Some(Msg::ColWordEnd),
        KeyCode::Char('0') => Some(Msg::ColLineStart),
        KeyCode::Char('$') => Some(Msg::ColLineEnd),
        KeyCode::Char('^') => Some(Msg::ColFirstNonBlank),
        KeyCode::Char('g') => Some(Msg::CursorTop),
        KeyCode::Char('G') => Some(Msg::CursorBottom),
        KeyCode::Char('v') => Some(Msg::ToggleCharSelect),
        KeyCode::Char('V') => Some(Msg::ToggleSelect),
        KeyCode::Char('y') => Some(Msg::Yank),
        // Mnemonic git actions on the non-motion letters. These mirror the magit
        // profile's letters exactly, so the same help legend is accurate for
        // both profiles; only the motion-clashing actions (log/branch/refs/
        // refresh/revert) differ, living behind the Space leader instead.
        KeyCode::Char('s') => Some(Msg::Stage),
        KeyCode::Char('S') => Some(Msg::StageAll),
        KeyCode::Char('u') => Some(Msg::Unstage),
        KeyCode::Char('U') => Some(Msg::UnstageAll),
        KeyCode::Char('x') => Some(Msg::Discard),
        KeyCode::Char('c') => Some(Msg::CommitMenu),
        KeyCode::Char('r') => Some(Msg::RebaseMenu),
        KeyCode::Char('m') => Some(Msg::MergeMenu),
        KeyCode::Char('t') => Some(Msg::TagMenu),
        KeyCode::Char('O') => Some(Msg::ResetMenu),
        KeyCode::Char('A') => Some(Msg::CherryPick),
        KeyCode::Char('o') => Some(Msg::OperationsMenu),
        KeyCode::Char('z') => Some(Msg::StashMenu),
        KeyCode::Char('p') => Some(Msg::StashPop),
        KeyCode::Char('M') => Some(Msg::RemoteMenu),
        KeyCode::Char('W') => Some(Msg::WorktreeMenu),
        KeyCode::Char('d') => Some(Msg::DiffPrompt),
        KeyCode::Char('f') => Some(Msg::Fetch),
        KeyCode::Char('F') => Some(Msg::Pull),
        KeyCode::Char('P') => Some(Msg::PushMenu),
        KeyCode::Char('L') => Some(Msg::OpenSessionLog),
        // Structural / search.
        KeyCode::Tab => Some(Msg::ToggleFold),
        KeyCode::Enter => Some(Msg::Enter),
        KeyCode::Char('/') => Some(Msg::SearchOpen),
        KeyCode::Char('n') => Some(Msg::SearchNext),
        KeyCode::Char('N') => Some(Msg::SearchPrev),
        KeyCode::Char(':') => Some(Msg::PaletteOpen),
        KeyCode::Char('?') => Some(Msg::HelpToggle),
        KeyCode::Char('q') | KeyCode::Esc => Some(Msg::Quit),
        _ => None,
    }
}

/// Dispatch a leader key (after Space) to the same action its bare letter fires
/// in the magit profile, so `<Space>c` opens the commit menu, etc.
pub fn leader_command(c: char) -> Option<Msg> {
    lookup(c).map(Command::to_msg)
}

/// The (key, label) pairs shown in the leader which-key popup, in a stable
/// order. Mirrors the default command letters plus the `w` window group.
pub fn leader_entries() -> Vec<(char, &'static str)> {
    vec![
        ('w', "window ▸"),
        ('o', "operations ▸"),
        ('s', "stage"),
        ('u', "unstage"),
        ('x', "discard"),
        ('c', "commit"),
        ('b', "branch"),
        ('l', "log"),
        ('L', "session log"),
        ('r', "rebase"),
        ('m', "merge"),
        ('z', "stash"),
        ('t', "tag"),
        ('d', "diff"),
        ('f', "fetch"),
        ('F', "pull"),
        ('P', "push"),
        ('y', "refs"),
        ('M', "remote"),
        ('W', "worktree"),
        ('O', "reset"),
        ('D', "rm file"),
        ('R', "mv file"),
        ('C', "code search"),
        ('g', "refresh"),
        ('?', "help"),
    ]
}

/// Key handling while the in-app commit editor is open. Ctrl-S commits, Ctrl-O
/// hands off to $EDITOR, Ctrl-G drafts a message; Esc cancels. Every other key
/// (including emacs/readline motions like Ctrl-A/E/F/B) edits the text.
pub fn resolve_commit_key(key: KeyEvent) -> Option<Msg> {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    match key.code {
        KeyCode::Char('s') if ctrl => Some(Msg::CommitEditorSubmit),
        KeyCode::Char('o') if ctrl => Some(Msg::CommitEditorEditor),
        KeyCode::Char('g') if ctrl => Some(Msg::CommitEditorGenerate),
        KeyCode::Esc => Some(Msg::CommitEditorCancel),
        _ => Some(Msg::CommitEditorInput(key)),
    }
}

/// Key handling while the interactive-rebase todo editor is open.
pub fn resolve_rebase_key(key: KeyEvent) -> Option<Msg> {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    match key.code {
        KeyCode::Char('n') if ctrl => Some(Msg::RebaseTodoDown),
        KeyCode::Char('p') if ctrl => Some(Msg::RebaseTodoUp),
        KeyCode::Char('j') | KeyCode::Down => Some(Msg::RebaseTodoDown),
        KeyCode::Char('k') | KeyCode::Up => Some(Msg::RebaseTodoUp),
        KeyCode::Char('J') => Some(Msg::RebaseTodoMoveDown),
        KeyCode::Char('K') => Some(Msg::RebaseTodoMoveUp),
        KeyCode::Char(c @ ('p' | 's' | 'f' | 'd' | 'r' | 'e')) => Some(Msg::RebaseTodoSetAction(c)),
        KeyCode::Enter => Some(Msg::RebaseTodoRun),
        KeyCode::Esc | KeyCode::Char('q') => Some(Msg::RebaseTodoCancel),
        _ => None,
    }
}

/// Key handling while the incremental search minibuffer is open.
pub fn resolve_search_key(key: KeyEvent) -> Option<Msg> {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    match key.code {
        KeyCode::Enter => Some(Msg::SearchSubmit),
        KeyCode::Esc => Some(Msg::SearchCancel),
        KeyCode::Char('g') if ctrl => Some(Msg::SearchCancel),
        KeyCode::Backspace => Some(Msg::SearchBackspace),
        KeyCode::Char(c) if !ctrl && !key.modifiers.contains(KeyModifiers::ALT) => {
            Some(Msg::SearchChar(c))
        }
        _ => None,
    }
}

/// Key handling while the help overlay is open: any key dismisses it.
pub fn resolve_help_key(_key: KeyEvent) -> Option<Msg> {
    Some(Msg::HelpClose)
}

/// Key handling while the command palette is open.
/// Keys while the live code-search finder is open: type to search, arrows or
/// Ctrl-n/p to move, Tab to fold in the semantic index, Enter to open, Ctrl-o to open in $EDITOR, Esc to
/// close.
pub fn resolve_finder_key(key: KeyEvent) -> Option<Msg> {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    match key.code {
        KeyCode::Enter => Some(Msg::CodeFinderSubmit),
        KeyCode::Esc => Some(Msg::CodeFinderCancel),
        KeyCode::Char('g') if ctrl => Some(Msg::CodeFinderCancel),
        KeyCode::Tab => Some(Msg::CodeFinderSemantic),
        // Result-list navigation: arrows and Ctrl-n/p (emacs next/prev-line).
        KeyCode::Up => Some(Msg::CodeFinderUp),
        KeyCode::Down => Some(Msg::CodeFinderDown),
        KeyCode::Char('p') if ctrl => Some(Msg::CodeFinderUp),
        KeyCode::Char('n') if ctrl => Some(Msg::CodeFinderDown),
        KeyCode::Char('s') if ctrl => Some(Msg::CodeFinderSemantic),
        KeyCode::Char('o') if ctrl => Some(Msg::CodeFinderEditor),
        // Everything else edits the query line: text, Backspace, and the
        // emacs/readline motions and kills (Ctrl-a/e/f/b/d/k/u/w, Alt-b/f,
        // arrows, Home/End) via the shared line editor.
        _ => Some(Msg::CodeFinderInput(key)),
    }
}

pub fn resolve_palette_key(key: KeyEvent) -> Option<Msg> {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    match key.code {
        KeyCode::Enter => Some(Msg::PaletteSubmit),
        KeyCode::Esc => Some(Msg::PaletteCancel),
        KeyCode::Char('g') if ctrl => Some(Msg::PaletteCancel),
        // Entry-list navigation: arrows and Ctrl-n/p (emacs next/prev-line).
        KeyCode::Up => Some(Msg::PaletteUp),
        KeyCode::Down => Some(Msg::PaletteDown),
        KeyCode::Char('p') if ctrl => Some(Msg::PaletteUp),
        KeyCode::Char('n') if ctrl => Some(Msg::PaletteDown),
        // Everything else edits the query line via the shared line editor
        // (text, Backspace, Ctrl-a/e/f/b/d/k/u/w, Alt-b/f, arrows, Home/End).
        _ => Some(Msg::PaletteInput(key)),
    }
}

/// Key handling while a destructive action awaits confirmation: only yes/no.
pub fn resolve_confirm_key(key: KeyEvent) -> Option<Msg> {
    match key.code {
        KeyCode::Char('y') | KeyCode::Char('Y') => Some(Msg::ConfirmAccept),
        KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => Some(Msg::ConfirmCancel),
        _ => None,
    }
}

/// Key handling while a transient menu is open: a char toggles an arg or fires
/// an action; Esc/`q` closes it.
pub fn resolve_transient_key(key: KeyEvent) -> Option<Msg> {
    match key.code {
        KeyCode::Esc | KeyCode::Char('q') => Some(Msg::TransientCancel),
        KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
            Some(Msg::TransientChar(c))
        }
        _ => None,
    }
}

/// Key handling while a minibuffer prompt is open: text entry plus candidate
/// navigation.
pub fn resolve_prompt_key(key: KeyEvent) -> Option<Msg> {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    match key.code {
        KeyCode::Enter => Some(Msg::PromptSubmit),
        KeyCode::Esc => Some(Msg::PromptCancel),
        KeyCode::Char('g') if ctrl => Some(Msg::PromptCancel),
        // Ctrl-P/N walk the candidate list, matching the palette.
        KeyCode::Up => Some(Msg::PromptUp),
        KeyCode::Down => Some(Msg::PromptDown),
        KeyCode::Char('p') if ctrl => Some(Msg::PromptUp),
        KeyCode::Char('n') if ctrl => Some(Msg::PromptDown),
        // Everything else is line editing (text, Backspace, emacs/readline motions).
        _ => Some(Msg::PromptInput(key)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn emacs_profile_has_char_navigation_and_selection() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let ctrl = |c| KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL);
        let alt = |c| KeyEvent::new(KeyCode::Char(c), KeyModifiers::ALT);
        // C-f/C-b move by character; C-a/C-e to line ends; C-Space sets the mark.
        assert!(matches!(resolve_key(ctrl('f')), Some(Msg::ColRight)));
        assert!(matches!(resolve_key(ctrl('b')), Some(Msg::ColLeft)));
        assert!(matches!(resolve_key(ctrl('a')), Some(Msg::ColLineStart)));
        assert!(matches!(resolve_key(ctrl('e')), Some(Msg::ColLineEnd)));
        assert!(matches!(resolve_key(ctrl(' ')), Some(Msg::ToggleCharSelect)));
        assert!(matches!(resolve_key(ctrl('n')), Some(Msg::CursorDown)));
        assert!(matches!(resolve_key(ctrl('p')), Some(Msg::CursorUp)));
        // M-f/M-b move by word.
        assert!(matches!(resolve_key(alt('f')), Some(Msg::ColWordForward)));
        assert!(matches!(resolve_key(alt('b')), Some(Msg::ColWordBack)));
    }

    #[test]
    fn vim_keeps_motion_and_charwise() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let key = |c| KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE);
        // Motions, charwise/linewise visual, and yank keep their vim meaning.
        assert!(matches!(resolve_vim_key(key(' ')), Some(Msg::LeaderOpen)));
        assert!(matches!(resolve_vim_key(key('j')), Some(Msg::CursorDown)));
        assert!(matches!(resolve_vim_key(key('G')), Some(Msg::CursorBottom)));
        assert!(matches!(resolve_vim_key(key('w')), Some(Msg::ColWordForward)));
        assert!(matches!(resolve_vim_key(key('v')), Some(Msg::ToggleCharSelect)));
        assert!(matches!(resolve_vim_key(key('V')), Some(Msg::ToggleSelect)));
        assert!(matches!(resolve_vim_key(key('y')), Some(Msg::Yank)));
    }

    #[test]
    fn vim_binds_mnemonic_actions_on_non_motion_letters() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let key = |c| KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE);
        // Non-motion letters fire git actions directly, no leader needed.
        assert!(matches!(resolve_vim_key(key('s')), Some(Msg::Stage)));
        assert!(matches!(resolve_vim_key(key('c')), Some(Msg::CommitMenu)));
        assert!(matches!(resolve_vim_key(key('P')), Some(Msg::PushMenu)));
        // Motion-clashers (log) stay motions and live behind the leader.
        assert!(matches!(resolve_vim_key(key('l')), Some(Msg::ColRight)));
        assert!(matches!(leader_command('l'), Some(Msg::Log)));
        assert!(matches!(leader_command('b'), Some(Msg::BranchMenu)));
    }

    #[test]
    fn parse_chord_reads_modifiers_and_named_keys() {
        let ch = |c| ChordCode::Char(c);
        assert_eq!(
            parse_chord("s"),
            Some(Chord { prefix: false, ctrl: false, alt: false, code: ch('s') })
        );
        assert_eq!(
            parse_chord("C-f"),
            Some(Chord { prefix: false, ctrl: true, alt: false, code: ch('f') })
        );
        assert_eq!(
            parse_chord("M-b"),
            Some(Chord { prefix: false, ctrl: false, alt: true, code: ch('b') })
        );
        assert_eq!(
            parse_chord("C-x o"),
            Some(Chord { prefix: true, ctrl: false, alt: false, code: ch('o') })
        );
        assert_eq!(
            parse_chord("C-Spc"),
            Some(Chord { prefix: false, ctrl: true, alt: false, code: ch(' ') })
        );
        assert_eq!(
            parse_chord("Up"),
            Some(Chord { prefix: false, ctrl: false, alt: false, code: ChordCode::Up })
        );
        // Two bare chars are not a chord.
        assert_eq!(parse_chord("ab"), None);
        assert_eq!(parse_chord(""), None);
    }

    #[test]
    fn overrides_rebind_including_chords() {
        let mut overrides = HashMap::new();
        // A bare-letter action rebind and a chord rebind for a nav command.
        overrides.insert("log".to_string(), "G".to_string());
        overrides.insert("forward-char".to_string(), "C-l".to_string());
        let map = build_map(Profile::Magit, &overrides);
        let bare = |c| Chord { prefix: false, ctrl: false, alt: false, code: ChordCode::Char(c) };
        let ctrl = |c| Chord { prefix: false, ctrl: true, alt: false, code: ChordCode::Char(c) };
        assert!(matches!(map.get(&bare('G')), Some(Command::Log)));
        assert!(matches!(map.get(&ctrl('l')), Some(Command::ForwardChar)));
        // Untouched defaults survive, and the emacs C-f default is still there.
        assert!(matches!(map.get(&bare('c')), Some(Command::CommitMenu)));
        assert!(matches!(map.get(&ctrl('f')), Some(Command::ForwardChar)));
        // An unknown action name is ignored, not fatal.
        overrides.insert("bogus".to_string(), "Z".to_string());
        let map = build_map(Profile::Magit, &overrides);
        assert!(!map.contains_key(&bare('Z')));
    }
}
