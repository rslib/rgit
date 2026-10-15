mod agent_hints;
mod agent_install;
mod ai;
mod compat_errors;
mod completion;
mod credentials;
mod exit_codes;
mod fast_import;
mod flags;
mod globals;
mod help_output;
mod history;
mod hooks;
mod hunks;
mod ident;
mod line_mode;
mod maintenance;
mod more_plumbing;
mod output_default;
mod paths;
mod plumbing;
mod refs_ops;
mod remote_ops;
mod sequencer;
mod settings;
mod sparse;
mod stack;
mod status_untracked;
mod toon_detail;
mod tui_refresh;
mod tui_remotes;
mod workflow;
mod workspace;
mod worktree_ops;

/// Whether the git on PATH is `major.minor` or newer.
fn git_at_least(major: u32, minor: u32) -> bool {
    static VERSION: std::sync::OnceLock<(u32, u32)> = std::sync::OnceLock::new();
    let have = *VERSION.get_or_init(|| {
        let out = std::process::Command::new("git")
            .arg("--version")
            .output()
            .unwrap();
        let text = String::from_utf8_lossy(&out.stdout).into_owned();
        let mut parts = text
            .split_whitespace()
            .nth(2)
            .unwrap_or_default()
            .split('.')
            .map(|n| n.parse().unwrap_or(0));
        (parts.next().unwrap_or(0), parts.next().unwrap_or(0))
    });
    have >= (major, minor)
}

/// `cases` without the ones in `changed` when git predates `major.minor`.
/// rgit follows the newest git, so an older git prints those cases the old
/// way and cannot be compared.
fn since<'a, 'b>(
    major: u32,
    minor: u32,
    cases: &[&'a [&'b str]],
    changed: &[&[&str]],
) -> Vec<&'a [&'b str]> {
    if git_at_least(major, minor) {
        return cases.to_vec();
    }
    eprintln!(
        "skipping {} cases: git predates {major}.{minor}",
        changed.len()
    );
    cases
        .iter()
        .copied()
        .filter(|c| !changed.contains(c))
        .collect()
}
