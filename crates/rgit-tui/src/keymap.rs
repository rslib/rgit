use std::collections::HashMap;
use std::sync::OnceLock;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::app::Msg;

/// A remappable normal-mode command. Structural keys (Enter, Tab, arrows, Esc)
/// are fixed; every mnemonic command below is bound to a character the user can
/// override in `[keys]`.
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

static KEYMAP: OnceLock<HashMap<char, Command>> = OnceLock::new();

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

/// Install the character bindings and profile: defaults overlaid with `[keys]`
/// overrides. Each override maps an action name to a single character; other
/// forms are ignored so a malformed entry cannot unbind the defaults.
pub fn init(overrides: &HashMap<String, String>, profile: Option<&str>) {
    let profile = match profile {
        Some(p) if p.eq_ignore_ascii_case("vim") => Profile::Vim,
        _ => Profile::Magit,
    };
    let _ = PROFILE.set(profile);
    let _ = KEYMAP.set(build_map(overrides));
}

fn build_map(overrides: &HashMap<String, String>) -> HashMap<char, Command> {
    let mut map: HashMap<char, Command> = DEFAULTS.iter().copied().collect();
    for (action, key) in overrides {
        if let (Some(cmd), Some(ch)) = (Command::from_name(action), single_char(key)) {
            map.insert(ch, cmd);
        }
    }
    map
}

fn single_char(key: &str) -> Option<char> {
    let mut chars = key.chars();
    let c = chars.next()?;
    chars.next().is_none().then_some(c)
}

fn lookup(c: char) -> Option<Command> {
    match KEYMAP.get() {
        Some(map) => map.get(&c).copied(),
        None => DEFAULTS.iter().find(|(k, _)| *k == c).map(|(_, cmd)| *cmd),
    }
}

/// Resolve a key event into a message, or `None` if it is unbound.
pub fn resolve_key(key: KeyEvent) -> Option<Msg> {
    if key.modifiers.contains(KeyModifiers::CONTROL) {
        // Emacs/readline navigation alongside the vim-style keys.
        match key.code {
            KeyCode::Char('c') => return Some(Msg::Quit),
            KeyCode::Char('n') => return Some(Msg::CursorDown),
            KeyCode::Char('p') => return Some(Msg::CursorUp),
            KeyCode::Char('f') => return Some(Msg::FocusPreview),
            KeyCode::Char('b') => return Some(Msg::FocusNav),
            _ => {}
        }
    }
    match key.code {
        KeyCode::Esc => Some(Msg::Quit),
        KeyCode::Tab => Some(Msg::ToggleFold),
        KeyCode::Enter => Some(Msg::Enter),
        KeyCode::Down => Some(Msg::CursorDown),
        KeyCode::Up => Some(Msg::CursorUp),
        KeyCode::Right => Some(Msg::FocusPreview),
        KeyCode::Left => Some(Msg::FocusNav),
        KeyCode::Char(c) => lookup(c).map(Command::to_msg),
        _ => None,
    }
}

/// Normal-mode keys in the vim profile. Motion, charwise/linewise visual, and
/// yank keep their vim meaning on bare keys; on top of that, git actions are
/// bound directly on the letters that are NOT motions, so `s` stages and `c`
/// commits without a leader. The handful of git actions whose mnemonic collides
/// with a motion (log/branch/worktree/revert/refs/refresh) stay motions here and
/// live behind the Space leader instead (see [`leader_command`]). This follows
/// neogit's spirit without giving up rgit's full motion set or charwise select.
pub fn resolve_vim_key(key: KeyEvent) -> Option<Msg> {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    if ctrl {
        match key.code {
            KeyCode::Char('c') => return Some(Msg::Quit),
            KeyCode::Char('d') => return Some(Msg::CursorHalfDown),
            KeyCode::Char('u') => return Some(Msg::CursorHalfUp),
            KeyCode::Char('n') => return Some(Msg::CursorDown),
            KeyCode::Char('p') => return Some(Msg::CursorUp),
            _ => return None,
        }
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
/// Ctrl-n/p to move, Tab to fold in the semantic index, Enter to open, Esc to
/// close.
pub fn resolve_finder_key(key: KeyEvent) -> Option<Msg> {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    match key.code {
        KeyCode::Enter => Some(Msg::CodeFinderSubmit),
        KeyCode::Esc => Some(Msg::CodeFinderCancel),
        KeyCode::Char('g') if ctrl => Some(Msg::CodeFinderCancel),
        KeyCode::Tab => Some(Msg::CodeFinderSemantic),
        KeyCode::Backspace => Some(Msg::CodeFinderBackspace),
        KeyCode::Up => Some(Msg::CodeFinderUp),
        KeyCode::Down => Some(Msg::CodeFinderDown),
        KeyCode::Char('p') if ctrl => Some(Msg::CodeFinderUp),
        KeyCode::Char('n') if ctrl => Some(Msg::CodeFinderDown),
        KeyCode::Char('s') if ctrl => Some(Msg::CodeFinderSemantic),
        KeyCode::Char('e') if ctrl => Some(Msg::CodeFinderEditor),
        KeyCode::Char(c) if !ctrl && !key.modifiers.contains(KeyModifiers::ALT) => {
            Some(Msg::CodeFinderChar(c))
        }
        _ => None,
    }
}

pub fn resolve_palette_key(key: KeyEvent) -> Option<Msg> {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    match key.code {
        KeyCode::Enter => Some(Msg::PaletteSubmit),
        KeyCode::Esc => Some(Msg::PaletteCancel),
        KeyCode::Char('g') if ctrl => Some(Msg::PaletteCancel),
        KeyCode::Backspace => Some(Msg::PaletteBackspace),
        KeyCode::Up => Some(Msg::PaletteUp),
        KeyCode::Down => Some(Msg::PaletteDown),
        KeyCode::Char('p') if ctrl => Some(Msg::PaletteUp),
        KeyCode::Char('n') if ctrl => Some(Msg::PaletteDown),
        KeyCode::Char(c) if !ctrl && !key.modifiers.contains(KeyModifiers::ALT) => {
            Some(Msg::PaletteChar(c))
        }
        _ => None,
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
    fn single_char_accepts_only_one_char() {
        assert_eq!(single_char("s"), Some('s'));
        assert_eq!(single_char(":"), Some(':'));
        assert_eq!(single_char("ab"), None);
        assert_eq!(single_char(""), None);
    }

    #[test]
    fn overrides_rebind_without_dropping_defaults() {
        let mut overrides = HashMap::new();
        overrides.insert("log".to_string(), "L".to_string());
        let map = build_map(&overrides);
        assert!(matches!(map.get(&'L'), Some(Command::Log)));
        // an untouched default is still present
        assert!(matches!(map.get(&'c'), Some(Command::CommitMenu)));
        // an unknown action name is ignored, not fatal
        overrides.insert("bogus".to_string(), "Z".to_string());
        let map = build_map(&overrides);
        assert!(!map.contains_key(&'Z'));
    }
}
