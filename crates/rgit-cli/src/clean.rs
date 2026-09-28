//! `rgit clean`, with git's `-i` menu read line by line from stdin.

use std::io::{BufRead, Write};
use std::sync::Arc;

use rgit_git::{CleanOptions, GitBackend};

/// The folder the command runs in, from the repository root.
fn prefix(backend: &Arc<dyn GitBackend>) -> String {
    let cwd = std::env::current_dir().and_then(|c| c.canonicalize());
    let root = backend.workdir().canonicalize();
    match (cwd, root) {
        (Ok(cwd), Ok(root)) => cwd
            .strip_prefix(&root)
            .map(|p| p.to_string_lossy().replace('\\', "/"))
            .unwrap_or_default(),
        _ => String::new(),
    }
}

/// `git clean`: refuse without -f (clean.requireForce), -n or -i; list or
/// remove what goes; return git's report.
pub fn run(
    backend: &Arc<dyn GitBackend>,
    mut opts: CleanOptions,
    force: u8,
    interactive: bool,
    quiet: bool,
) -> anyhow::Result<String> {
    let require = backend
        .config_get("clean.requireForce")?
        .is_none_or(|v| !matches!(v.to_lowercase().as_str(), "false" | "no" | "off" | "0"));
    if require && force == 0 && !opts.dry_run && !interactive {
        return Err(crate::cli::CliError {
            message: "clean.requireForce is true and -f not given: refusing to clean".to_owned(),
            help: Some("Run `rgit clean -n` to see what would go, then `rgit clean -f`".to_owned()),
            code: 128,
        }
        .into());
    }
    opts.force_repos = force > 1;
    opts.prefix = prefix(backend);
    if opts.paths.is_empty() && !opts.prefix.is_empty() {
        opts.paths.push(opts.prefix.clone());
    }
    let mut items = backend.clean_candidates(&opts)?;
    if interactive && !items.is_empty() {
        items = menu(items, &opts.prefix)?;
    }
    let lines = backend.clean_remove(&items, &opts)?;
    Ok(if quiet {
        lines
            .into_iter()
            .filter(|l| l.starts_with("warning:"))
            .collect::<Vec<_>>()
            .join("\n")
    } else {
        lines.join("\n")
    })
}

fn term_width() -> usize {
    std::env::var("COLUMNS")
        .ok()
        .and_then(|c| c.parse().ok())
        .filter(|c| *c > 0)
        .or_else(|| {
            use std::io::IsTerminal;
            std::io::stdout()
                .is_terminal()
                .then(|| crossterm::terminal::size().ok().map(|(w, _)| w as usize))
                .flatten()
        })
        .unwrap_or(80)
}

/// git's print_columns: `items` in columns (filled down, or across with
/// `across`), indented by two, two apart.
fn columns(items: &[String], across: bool) -> String {
    let width = term_width().saturating_sub(1);
    let cell = items.iter().map(|i| i.chars().count()).max().unwrap_or(0) + 2;
    let cols = (width.saturating_sub(2) / cell).max(1);
    let rows = items.len().div_ceil(cols);
    let mut out = String::new();
    for y in 0..rows {
        for x in 0..cols {
            let i = if across { y * cols + x } else { x * rows + y };
            let Some(item) = items.get(i) else {
                break;
            };
            let last = if across {
                x == cols - 1 || i == items.len() - 1
            } else {
                i + rows >= items.len()
            };
            if x == 0 {
                out.push_str("  ");
            }
            out.push_str(item);
            if last {
                out.push('\n');
                break;
            }
            out.push_str(&" ".repeat(cell - item.chars().count()));
        }
    }
    out
}

fn say(text: &str) {
    print!("{text}");
    let _ = std::io::stdout().flush();
}

fn read_line() -> Option<String> {
    let mut line = String::new();
    match std::io::stdin().lock().read_line(&mut line) {
        Ok(0) | Err(_) => None,
        Ok(_) => Some(line.trim().to_owned()),
    }
}

const MENU: &str = "*** Commands ***
    1: clean                2: filter by pattern    3: select by numbers
    4: ask each             5: quit                 6: help
What now> ";

const HELP: &str = "clean               - start cleaning
filter by pattern   - exclude items from deletion
select by numbers   - select items to be deleted by numbers
ask each            - confirm each deletion (like \"rm -i\")
quit                - stop cleaning
help                - this screen
?                   - help for prompt selection
";

const NAMES: [&str; 6] = [
    "clean",
    "filter by pattern",
    "select by numbers",
    "ask each",
    "quit",
    "help",
];

/// git clean's interactive loop; returns the items to remove (none on quit).
fn menu(items: Vec<String>, prefix: &str) -> anyhow::Result<Vec<String>> {
    let shown: Vec<String> = items
        .iter()
        .map(|i| rgit_git::clean_relative(i, prefix))
        .collect();
    let mut keep: Vec<bool> = vec![true; items.len()];
    let current = |keep: &[bool], shown: &[String]| -> Vec<String> {
        shown
            .iter()
            .zip(keep)
            .filter(|(_, k)| **k)
            .map(|(s, _)| s.clone())
            .collect()
    };
    let mut header = true;
    loop {
        let list = current(&keep, &shown);
        if list.is_empty() {
            say("No more files to clean, exiting.\n");
            return Ok(Vec::new());
        }
        if header {
            say(if list.len() == 1 {
                "Would remove the following item:\n"
            } else {
                "Would remove the following items:\n"
            });
            say(&columns(&list, false));
        }
        header = true;
        say(MENU);
        let Some(choice) = read_line() else {
            say("Bye.\n");
            return Ok(Vec::new());
        };
        let pick = match choice.parse::<usize>() {
            Ok(n) if (1..=6).contains(&n) => Some(n - 1),
            _ if choice.is_empty() => {
                header = false;
                continue;
            }
            _ if choice == "?" => {
                say(
                    "Prompt help:\n1          - select a numbered item\nfoo        - select item \
                     based on unique prefix\n           - (empty) select nothing\n",
                );
                header = false;
                continue;
            }
            _ => {
                let lower = choice.to_lowercase();
                let hits: Vec<usize> = (0..NAMES.len())
                    .filter(|i| NAMES[*i].starts_with(&lower))
                    .collect();
                match hits.as_slice() {
                    [one] => Some(*one),
                    _ => None,
                }
            }
        };
        match pick {
            None => {
                say(&format!("Huh ({choice})?\n"));
                header = false;
            }
            Some(0) => break,
            Some(1) => filter(&mut keep, &items, &shown),
            Some(2) => select(&mut keep, &shown),
            Some(3) => {
                ask_each(&mut keep, &shown);
                break;
            }
            Some(4) => {
                say("Bye.\n");
                return Ok(Vec::new());
            }
            _ => say(HELP),
        }
    }
    Ok(items
        .into_iter()
        .zip(keep)
        .filter(|(_, k)| *k)
        .map(|(i, _)| i)
        .collect())
}

/// Drop the items that match the ignore patterns typed in.
fn filter(keep: &mut [bool], items: &[String], shown: &[String]) {
    let mut changed = true;
    loop {
        let list: Vec<String> = shown
            .iter()
            .zip(keep.iter())
            .filter(|(_, k)| **k)
            .map(|(s, _)| s.clone())
            .collect();
        if list.is_empty() {
            return;
        }
        if changed {
            say(&columns(&list, false));
        }
        say("Input ignore patterns>> ");
        let line = read_line();
        if line.is_none() {
            say("\n");
        }
        let line = line.unwrap_or_default();
        if line.is_empty() {
            return;
        }
        let patterns: Vec<&str> = line.split(' ').filter(|p| !p.is_empty()).collect();
        changed = false;
        for (i, item) in items.iter().enumerate() {
            let dir = item.ends_with('/');
            if keep[i] && rgit_git::ignore_match(&patterns, shown[i].trim_end_matches('/'), dir) {
                keep[i] = false;
                changed = true;
            }
        }
        if !changed {
            say(&format!("WARNING: Cannot find items matched by: {line}\n"));
        }
    }
}

/// git's list_and_choose over the items still kept: numbers, ranges (`2-3`,
/// `4-`), `*`, prefixes and `-` to unselect; an empty line ends it.
fn select(keep: &mut [bool], shown: &[String]) {
    let idx: Vec<usize> = (0..keep.len()).filter(|i| keep[*i]).collect();
    let mut chosen = vec![false; idx.len()];
    loop {
        let cells: Vec<String> = idx
            .iter()
            .enumerate()
            .map(|(n, i)| {
                let mark = if chosen[n] { '*' } else { ' ' };
                format!("{mark}{:>2}: {}", n + 1, shown[*i])
            })
            .collect();
        say(&columns(&cells, true));
        say("Select items to delete>> ");
        let Some(line) = read_line() else {
            say("\n");
            break;
        };
        if line.is_empty() {
            break;
        }
        if line == "?" {
            say(
                "Prompt help:\n1          - select a single item\n3-5        - select a range \
                 of items\n2-3,6-9    - select multiple ranges\nfoo        - select item based \
                 on unique prefix\n-...       - unselect specified items\n*          - choose \
                 all items\n           - (empty) finish selecting\n",
            );
            continue;
        }
        for token in line.split([' ', ',']).filter(|t| !t.is_empty()) {
            let (on, token) = match token.strip_prefix('-') {
                Some(rest) => (false, rest),
                None => (true, token),
            };
            let range = if token == "*" {
                Some((1, idx.len()))
            } else if let Some((a, b)) = token.split_once('-') {
                a.parse::<usize>()
                    .ok()
                    .map(|a| (a, b.parse().unwrap_or(idx.len())))
            } else if let Ok(n) = token.parse::<usize>() {
                Some((n, n))
            } else {
                let hits: Vec<usize> = (0..idx.len())
                    .filter(|n| shown[idx[*n]].starts_with(token))
                    .collect();
                match hits.as_slice() {
                    [one] => Some((one + 1, one + 1)),
                    _ => None,
                }
            };
            match range {
                Some((a, b)) if a >= 1 && a <= b && b <= idx.len() => {
                    chosen[a - 1..b].iter_mut().for_each(|c| *c = on);
                }
                _ => say(&format!("Huh ({token})?\n")),
            }
        }
    }
    for (n, i) in idx.iter().enumerate() {
        keep[*i] = chosen[n];
    }
}

/// Ask about each item, as `rm -i` does; only a yes keeps it for removal.
fn ask_each(keep: &mut [bool], shown: &[String]) {
    let mut eof = false;
    for i in 0..keep.len() {
        if !keep[i] {
            continue;
        }
        let mut answer = String::new();
        if !eof {
            say(&format!("Remove {} [y/N]? ", shown[i]));
            match read_line() {
                Some(a) => answer = a,
                None => {
                    say("\n");
                    eof = true;
                }
            }
        }
        let yes = !answer.is_empty() && "yes".starts_with(&answer.to_lowercase());
        keep[i] = yes;
    }
}
