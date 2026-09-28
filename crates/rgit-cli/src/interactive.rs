//! Interactive prompts for missing arguments, shown only on a real terminal.
//! When stdin/stdout are not a TTY (an agent, a pipe, CI) or `--no-input` is
//! set, the caller errors instead, so scripted use stays non-interactive.

use std::io::{IsTerminal, Write};
use std::sync::Arc;

use rgit_git::{GitBackend, LogOptions};

use crate::prompt::{self, Item};

/// Whether prompts should be shown: a real terminal and not `--no-input`.
pub fn enabled(no_input: bool) -> bool {
    !no_input && std::io::stdin().is_terminal() && std::io::stdout().is_terminal()
}

fn err(e: impl std::fmt::Display) -> anyhow::Error {
    anyhow::anyhow!(e.to_string())
}

/// Select a local branch.
pub fn pick_branch(backend: &Arc<dyn GitBackend>, prompt: &str) -> anyhow::Result<String> {
    let branches = backend.local_branches()?;
    if branches.is_empty() {
        anyhow::bail!("no local branches");
    }
    let items = branches
        .into_iter()
        .map(|b| Item::new(b.clone(), b))
        .collect();
    prompt::select(prompt, items).map_err(err)
}

/// Select one of the recent commits (short id + subject).
pub fn pick_commit(backend: &Arc<dyn GitBackend>, prompt: &str) -> anyhow::Result<String> {
    let entries = backend.log(&LogOptions {
        limit: 30,
        ..Default::default()
    })?;
    if entries.is_empty() {
        anyhow::bail!("no commits");
    }
    let items = entries
        .into_iter()
        .map(|e| Item::new(e.short_id.clone(), format!("{}  {}", e.short_id, e.summary)))
        .collect();
    prompt::select(prompt, items).map_err(err)
}

/// Select a stash by index, labelled with its message.
pub fn pick_stash(backend: &Arc<dyn GitBackend>, prompt: &str) -> anyhow::Result<usize> {
    let stashes = backend.status()?.stashes;
    if stashes.is_empty() {
        anyhow::bail!("no stashes");
    }
    let items = stashes
        .into_iter()
        .map(|s| Item::new(s.index, format!("stash@{{{}}}  {}", s.index, s.message)))
        .collect();
    prompt::select(prompt, items).map_err(err)
}

/// Select one or more local branches (for deletion).
pub fn multiselect_branches(
    backend: &Arc<dyn GitBackend>,
    prompt: &str,
) -> anyhow::Result<Vec<String>> {
    let current = backend.status().ok().and_then(|s| s.head.branch);
    let branches: Vec<String> = backend
        .local_branches()?
        .into_iter()
        .filter(|b| Some(b) != current.as_ref())
        .collect();
    if branches.is_empty() {
        anyhow::bail!("no other branches");
    }
    let items = branches
        .into_iter()
        .map(|b| Item::new(b.clone(), b))
        .collect();
    prompt::multiselect(prompt, items).map_err(err)
}

/// Select a changed path from the working tree.
pub fn pick_file(backend: &Arc<dyn GitBackend>, prompt: &str) -> anyhow::Result<String> {
    let paths: Vec<String> = backend
        .status()?
        .entries
        .into_iter()
        .map(|e| e.path)
        .collect();
    if paths.is_empty() {
        anyhow::bail!("no changed files");
    }
    let items = paths.into_iter().map(|p| Item::new(p.clone(), p)).collect();
    prompt::select(prompt, items).map_err(err)
}

/// Free-text input (rejects an empty value).
pub fn input(prompt: &str) -> anyhow::Result<String> {
    prompt::input(prompt, |v| {
        if v.trim().is_empty() {
            Err("required".to_owned())
        } else {
            Ok(())
        }
    })
    .map_err(err)
}

/// Let the user edit a commit message in git's editor; `#` lines are dropped.
pub fn edit_message(backend: &Arc<dyn GitBackend>, text: &str) -> anyhow::Result<String> {
    let path = backend.commit_msg_path();
    std::fs::write(&path, format!("{}\n", text.trim_end()))?;
    let editor = crate::plumbing::editor(backend);
    let ran = std::process::Command::new("sh")
        .arg("-c")
        .arg(format!("{editor} \"$@\""))
        .arg(&editor)
        .arg(&path)
        .status()?;
    if !ran.success() {
        anyhow::bail!("there was a problem with the editor '{editor}'");
    }
    let text = std::fs::read_to_string(&path)?;
    Ok(text
        .lines()
        .filter(|l| !l.starts_with('#'))
        .collect::<Vec<_>>()
        .join("\n"))
}

/// Which way `-p` moves the hunks it asks about.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum PatchMode {
    /// `add -p`: stage working-tree hunks.
    Stage,
    /// `reset -p` / `restore -p --staged`: unstage staged hunks.
    Unstage,
    /// `checkout -p` / `restore -p`: discard working-tree hunks.
    Discard,
}

/// One question in the `-p` loop: a whole hunk, or one change group of a
/// split hunk (its line indices, and the lines shown for it).
struct Ask {
    hunk: usize,
    part: Option<(Vec<usize>, std::ops::Range<usize>)>,
}

/// git's `-p` loop: show each hunk of the changes under `paths`, ask
/// y/n/q/a/d/s/?, then stage, unstage or discard the chosen hunks.
pub fn patch(
    backend: &Arc<dyn GitBackend>,
    interactive: bool,
    mode: PatchMode,
    paths: &[String],
) -> anyhow::Result<String> {
    if !interactive {
        anyhow::bail!(
            "-p needs a terminal; pick hunks with `rgit stage`, `rgit unstage` or `rgit discard` and --hunk/--lines"
        );
    }
    let (verb, done) = match mode {
        PatchMode::Stage => ("Stage", "staged"),
        PatchMode::Unstage => ("Unstage", "unstaged"),
        PatchMode::Discard => ("Discard", "discarded"),
    };
    let files = backend.diff(&rgit_git::DiffSpec {
        cached: mode == PatchMode::Unstage,
        paths: paths.to_vec(),
        ..Default::default()
    })?;
    let mut out = std::io::stdout();
    let mut applied = 0;
    let mut quit = false;
    for file in files.iter().filter(|f| !f.binary && !f.hunks.is_empty()) {
        if quit {
            break;
        }
        let mut asks: Vec<Ask> = (0..file.hunks.len())
            .map(|hunk| Ask { hunk, part: None })
            .collect();
        let mut answers: Vec<Option<bool>> = vec![None; asks.len()];
        let _ = writeln!(out, "diff --git a/{0} b/{0}", file.path);
        let mut i = 0;
        while i < asks.len() {
            let hunk = &file.hunks[asks[i].hunk];
            let groups = change_groups(&hunk.lines);
            let splittable = asks[i].part.is_none() && groups.len() > 1;
            let shown = match &asks[i].part {
                Some((_, range)) => range.clone(),
                None => 0..hunk.lines.len(),
            };
            if asks[i].part.is_none() {
                let _ = writeln!(out, "{}", hunk.header);
            }
            for line in &hunk.lines[shown] {
                let _ = writeln!(
                    out,
                    "{}{}",
                    origin(line.origin),
                    line.text.trim_end_matches('\n')
                );
            }
            let keys = if splittable {
                "y,n,q,a,d,s,?"
            } else {
                "y,n,q,a,d,?"
            };
            let what = if mode == PatchMode::Discard {
                "this hunk from worktree"
            } else {
                "this hunk"
            };
            let _ = write!(out, "({}/{}) {verb} {what} [{keys}]? ", i + 1, asks.len());
            let _ = out.flush();
            let mut line = String::new();
            let answer = match std::io::stdin().read_line(&mut line) {
                Ok(0) | Err(_) => "q",
                Ok(_) => line.trim(),
            };
            match answer.chars().next() {
                Some('y') => answers[i] = Some(true),
                Some('n') => answers[i] = Some(false),
                Some(c @ ('a' | 'd')) => answers[i..].fill(Some(c == 'a')),
                Some('q') => {
                    quit = true;
                    break;
                }
                Some('s') if splittable => {
                    let _ = writeln!(out, "Split into {} hunks.", groups.len());
                    let hunk = asks[i].hunk;
                    let parts: Vec<Ask> = groups
                        .into_iter()
                        .map(|(changes, range)| Ask {
                            hunk,
                            part: Some((changes, range)),
                        })
                        .collect();
                    let n = parts.len();
                    asks.splice(i..=i, parts);
                    answers.splice(i..=i, vec![None; n]);
                    continue;
                }
                _ => {
                    let lower = verb.to_lowercase();
                    let _ = writeln!(
                        out,
                        "y - {lower} this hunk\nn - do not {lower} this hunk\n\
                         q - quit; do not {lower} this hunk or any of the remaining ones\n\
                         a - {lower} this hunk and all later hunks in the file\n\
                         d - do not {lower} this hunk or any of the later hunks in the file\n\
                         s - split the current hunk into smaller hunks\n? - print help"
                    );
                    continue;
                }
            }
            if answers[i..].iter().all(Option::is_some) {
                break;
            }
            i += 1;
        }
        // Bottom hunk first, so the hunks above keep their start lines.
        for h in (0..file.hunks.len()).rev() {
            let start = file.hunks[h].new_start;
            let mine = asks.iter().zip(&answers).filter(|(a, _)| a.hunk == h);
            let whole = mine.clone().all(|(_, yes)| *yes == Some(true));
            let lines: Vec<usize> = mine
                .filter(|(_, yes)| **yes == Some(true))
                .flat_map(|(a, _)| a.part.iter().flat_map(|(c, _)| c.clone()))
                .collect();
            let path = file.path.as_str();
            match (mode, whole) {
                (PatchMode::Stage, true) => backend.stage_hunk(path, start)?,
                (PatchMode::Unstage, true) => backend.unstage_hunk(path, start)?,
                (PatchMode::Discard, true) => backend.discard_hunk(path, start)?,
                _ if lines.is_empty() => continue,
                (PatchMode::Stage, false) => backend.stage_lines(path, start, &lines)?,
                (PatchMode::Unstage, false) => backend.unstage_lines(path, start, &lines)?,
                (PatchMode::Discard, false) => backend.discard_lines(path, start, &lines)?,
            }
            applied += 1;
        }
    }
    Ok(if applied == 0 {
        format!("no hunks {done}")
    } else {
        format!("{done} {applied} hunk(s)")
    })
}

/// The runs of changed lines in a hunk that `s` splits it into: each run's
/// line indices, and the lines to show for it (with the context around it).
fn change_groups(lines: &[rgit_git::DiffLine]) -> Vec<(Vec<usize>, std::ops::Range<usize>)> {
    use rgit_git::LineOrigin::{Added, Removed};
    let mut groups: Vec<Vec<usize>> = Vec::new();
    let mut prev_change = false;
    for (i, l) in lines.iter().enumerate() {
        let change = matches!(l.origin, Added | Removed);
        if change && !prev_change {
            groups.push(Vec::new());
        }
        if change && let Some(g) = groups.last_mut() {
            g.push(i);
        }
        prev_change = change;
    }
    let bounds: Vec<(usize, usize)> = groups.iter().map(|g| (g[0], g[g.len() - 1] + 1)).collect();
    groups
        .into_iter()
        .enumerate()
        .map(|(k, g)| {
            let from = if k == 0 { 0 } else { bounds[k - 1].1 };
            let to = bounds.get(k + 1).map_or(lines.len(), |b| b.0);
            (g, from..to)
        })
        .collect()
}

fn origin(o: rgit_git::LineOrigin) -> char {
    match o {
        rgit_git::LineOrigin::Added => '+',
        rgit_git::LineOrigin::Removed => '-',
        rgit_git::LineOrigin::Context => ' ',
        rgit_git::LineOrigin::Meta => '\\',
    }
}

/// A yes/no confirmation.
pub fn confirm(prompt: &str) -> anyhow::Result<bool> {
    prompt::confirm(prompt, false).map_err(err)
}
