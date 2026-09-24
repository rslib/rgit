type Entry = (&'static str, &'static [&'static str]);

const EXAMPLES: &[Entry] = &[
    ("status", &["rgit status", "rgit status --toon"]),
    (
        "log",
        &[
            "rgit log --limit 50",
            "rgit log main -- src/lib.rs",
            "rgit log --author alice --since 2024-01-01",
        ],
    ),
    (
        "diff",
        &[
            "rgit diff",
            "rgit diff --cached",
            "rgit diff main HEAD --patch",
        ],
    ),
    (
        "show",
        &[
            "rgit show HEAD",
            "rgit show <rev> --patch",
            "rgit show <rev> --name-only",
        ],
    ),
    (
        "blame",
        &["rgit blame src/lib.rs", "rgit blame src/lib.rs -L 10,20"],
    ),
    ("refs", &["rgit refs"]),
    (
        "stage",
        &[
            "rgit stage src/lib.rs",
            "rgit stage src/lib.rs --hunk 42",
            "rgit stage src/lib.rs --hunk 42 --lines 0,2",
        ],
    ),
    (
        "unstage",
        &[
            "rgit unstage src/lib.rs",
            "rgit unstage src/lib.rs --hunk 42",
        ],
    ),
    ("stage-all", &["rgit stage-all"]),
    ("unstage-all", &["rgit unstage-all"]),
    (
        "discard",
        &[
            "rgit discard src/lib.rs",
            "rgit discard src/lib.rs --hunk 10",
        ],
    ),
    (
        "resolve",
        &[
            "rgit resolve src/lib.rs --ours",
            "rgit resolve src/lib.rs --theirs",
        ],
    ),
    (
        "commit",
        &[
            "rgit commit -m \"<message>\"",
            "rgit commit -a -m \"<message>\"",
            "rgit commit --amend",
        ],
    ),
    ("extend", &["rgit extend"]),
    (
        "reword",
        &[
            "rgit reword -m \"<message>\"",
            "rgit reword -m \"<message>\" <rev>",
        ],
    ),
    ("uncommit", &["rgit uncommit", "rgit uncommit 2"]),
    (
        "squash",
        &[
            "rgit squash",
            "rgit squash <rev>",
            "rgit squash --from <rev>",
        ],
    ),
    (
        "move",
        &[
            "rgit move <rev> --before <rev>",
            "rgit move <rev> --after <rev>",
        ],
    ),
    (
        "split",
        &[
            "rgit split src/lib.rs src/main.rs",
            "rgit split --rev <rev> src/lib.rs",
        ],
    ),
    ("prune", &["rgit prune", "rgit prune --dry-run"]),
    ("next", &["rgit next"]),
    ("prev", &["rgit prev"]),
    (
        "fetch",
        &[
            "rgit fetch",
            "rgit fetch --all",
            "rgit fetch --remote origin --prune",
        ],
    ),
    ("pull", &["rgit pull", "rgit pull --rebase"]),
    ("sync", &["rgit sync"]),
    ("submit", &["rgit submit"]),
    (
        "push",
        &[
            "rgit push",
            "rgit push --set-upstream",
            "rgit push --force-with-lease",
        ],
    ),
    (
        "checkout",
        &["rgit checkout main", "rgit checkout -b <new_branch>"],
    ),
    (
        "merge",
        &[
            "rgit merge <branch>",
            "rgit merge <branch> --no-ff",
            "rgit merge --abort",
        ],
    ),
    (
        "rebase",
        &[
            "rgit rebase main",
            "rgit rebase --onto <newbase> <upstream>",
            "rgit rebase --continue",
        ],
    ),
    ("undo", &["rgit undo"]),
    ("redo", &["rgit redo"]),
    ("oplog", &["rgit oplog"]),
    ("smartlog", &["rgit smartlog", "rgit sl"]),
    (
        "bisect",
        &[
            "rgit bisect start HEAD main",
            "rgit bisect good",
            "rgit bisect bad",
        ],
    ),
    (
        "reset",
        &[
            "rgit reset HEAD~1",
            "rgit reset --hard <rev>",
            "rgit reset -- src/lib.rs",
        ],
    ),
    (
        "cherry-pick",
        &[
            "rgit cherry-pick <rev>",
            "rgit cherry-pick <rev> --no-commit",
        ],
    ),
    (
        "revert",
        &["rgit revert <rev>", "rgit revert <rev> --no-commit"],
    ),
    (
        "tag",
        &[
            "rgit tag",
            "rgit tag v1.0.0 -m \"<message>\"",
            "rgit tag -d v1.0.0",
        ],
    ),
    ("absorb", &["rgit absorb"]),
    ("clean", &["rgit clean", "rgit clean --dry-run"]),
    ("rm", &["rgit rm src/lib.rs", "rgit rm src/lib.rs --cached"]),
    ("mv", &["rgit mv src/old.rs src/new.rs"]),
    ("describe", &["rgit describe", "rgit describe HEAD --tags"]),
    ("init", &["rgit init", "rgit init <path> -b main"]),
    (
        "clone",
        &[
            "rgit clone https://github.com/example/repo.git",
            "rgit clone https://github.com/example/repo.git repo-dir --depth 1",
        ],
    ),
    ("submodule", &["rgit submodule update --init"]),
    ("git", &["rgit git status", "rgit git log --oneline -5"]),
    ("mcp", &["rgit mcp"]),
    (
        "serve",
        &[
            "rgit serve",
            "rgit serve --port 9000",
            "rgit serve --root ~/code --clone-base https://git.example.dev",
        ],
    ),
    ("branch", &["rgit branch", "rgit branch -a"]),
    ("branch create", &["rgit branch create <branch>"]),
    ("branch checkout", &["rgit branch checkout <branch>"]),
    (
        "branch delete",
        &[
            "rgit branch delete <branch>",
            "rgit branch delete <branch> --force",
        ],
    ),
    ("branch rename", &["rgit branch rename <old> <new>"]),
    (
        "branch prune",
        &["rgit branch prune", "rgit branch prune main"],
    ),
    ("stash", &["rgit stash", "rgit stash push \"<message>\""]),
    (
        "stash push",
        &["rgit stash push \"<message>\" --include-untracked"],
    ),
    ("stash pop", &["rgit stash pop", "rgit stash pop 1"]),
    ("stash apply", &["rgit stash apply"]),
    ("stash drop", &["rgit stash drop 0"]),
    ("stash list", &["rgit stash list"]),
    (
        "remote",
        &[
            "rgit remote",
            "rgit remote add origin https://github.com/example/repo.git",
        ],
    ),
    (
        "remote add",
        &["rgit remote add origin https://github.com/example/repo.git"],
    ),
    ("remote remove", &["rgit remote remove origin"]),
    (
        "remote set-url",
        &["rgit remote set-url origin https://github.com/example/repo.git"],
    ),
    ("remote rename", &["rgit remote rename origin upstream"]),
    ("worktree", &["rgit worktree"]),
    ("worktree add", &["rgit worktree add <branch> ../<path>"]),
    (
        "worktree remove",
        &[
            "rgit worktree remove <branch>",
            "rgit worktree remove <branch> --force",
        ],
    ),
    ("worktree prune", &["rgit worktree prune"]),
    ("workspace", &["rgit workspace"]),
    ("workspace new", &["rgit workspace new <name>"]),
    ("workspace list", &["rgit workspace list"]),
    ("workspace remove", &["rgit workspace remove <name>"]),
    ("flow", &["rgit flow status"]),
    ("flow init", &["rgit flow init gitflow"]),
    ("flow start", &["rgit flow start <name>"]),
    ("flow finish", &["rgit flow finish"]),
    (
        "flow release",
        &[
            "rgit flow release 1.2.0",
            "rgit flow release 1.2.0 --finish",
        ],
    ),
    ("flow status", &["rgit flow status"]),
    (
        "forge",
        &["rgit forge --profile work whoami", "rgit forge pr list"],
    ),
    (
        "forge login",
        &[
            "rgit forge login github --token-stdin",
            "rgit forge login gitlab --host https://gitlab.example.com --token-stdin",
        ],
    ),
    ("forge auth", &["rgit forge auth list"]),
    ("forge auth list", &["rgit forge auth list"]),
    ("forge auth status", &["rgit forge auth status"]),
    ("forge whoami", &["rgit forge whoami github"]),
    ("forge logout", &["rgit forge logout github"]),
    ("forge repo", &["rgit forge repo view"]),
    (
        "forge repo view",
        &["rgit forge repo view", "rgit forge repo view owner/repo"],
    ),
    (
        "forge repo create",
        &[
            "rgit forge repo create <name>",
            "rgit forge repo create <name> --organization <org> --private",
        ],
    ),
    (
        "forge repo delete",
        &["rgit forge repo delete owner/repo --yes"],
    ),
    ("forge branch", &["rgit forge branch list"]),
    (
        "forge branch list",
        &[
            "rgit forge branch list",
            "rgit forge branch list owner/repo",
        ],
    ),
    (
        "forge branch delete",
        &[
            "rgit forge branch delete <branch> --yes",
            "rgit forge branch delete <branch> owner/repo --yes",
        ],
    ),
    ("forge pr", &["rgit forge pr list"]),
    (
        "forge pr list",
        &[
            "rgit forge pr list",
            "rgit forge pr list owner/repo",
            "rgit forge pr list --page 2",
        ],
    ),
    (
        "forge pr create",
        &["rgit forge pr create --title \"<title>\" --head <branch> --base main"],
    ),
    (
        "forge pr close",
        &[
            "rgit forge pr close 42 --yes",
            "rgit forge pr close 42 owner/repo --yes",
        ],
    ),
    ("stack", &["rgit stack"]),
    ("stack new", &["rgit stack new <branch>"]),
    ("stack list", &["rgit stack list"]),
    ("stack restack", &["rgit stack restack"]),
    ("lanes", &["rgit lanes"]),
    ("lanes init", &["rgit lanes init"]),
    ("lanes off", &["rgit lanes off"]),
    ("lanes list", &["rgit lanes list"]),
    ("lanes new", &["rgit lanes new <lane>"]),
    (
        "lanes stack",
        &["rgit lanes stack <lane> --on <parent_lane>"],
    ),
    (
        "lanes assign",
        &[
            "rgit lanes assign <lane> src/lib.rs",
            "rgit lanes assign <lane> src/lib.rs --hunk 10",
        ],
    ),
    ("lanes unassign", &["rgit lanes unassign src/lib.rs"]),
    (
        "lanes commit",
        &["rgit lanes commit <lane> -m \"<message>\""],
    ),
    ("lanes rename", &["rgit lanes rename <old> <new>"]),
    ("lanes delete", &["rgit lanes delete <lane>"]),
    ("lanes push", &["rgit lanes push <lane>"]),
    ("lanes pr", &["rgit lanes pr <lane>"]),
    ("lanes restack", &["rgit lanes restack"]),
    ("skills", &["rgit skills list"]),
    ("skills list", &["rgit skills list"]),
    (
        "skills show",
        &["rgit skills show", "rgit skills show --reference"],
    ),
    (
        "skills install",
        &[
            "rgit skills install --project",
            "rgit skills install --user --target claude",
        ],
    ),
    ("index", &["rgit index status"]),
    (
        "index build",
        &["rgit index build", "rgit index build --root ~/code"],
    ),
    (
        "index search",
        &[
            "rgit index search \"<query>\"",
            "rgit index search \"<query>\" --limit 5",
        ],
    ),
    ("index status", &["rgit index status"]),
    ("index code", &["rgit index code \"<query>\""]),
    ("hooks", &["rgit hooks install", "rgit hooks status"]),
    (
        "hooks install",
        &[
            "rgit hooks install",
            "rgit hooks install --app codex",
            "rgit hooks install --user --app all",
        ],
    ),
    ("hooks status", &["rgit hooks status"]),
];

fn render(lines: &[&str]) -> String {
    let mut out = String::from("Examples:\n");
    for line in lines {
        out.push_str("  ");
        out.push_str(line);
        out.push('\n');
    }
    out.pop();
    out
}

/// The usage examples for a space-joined subcommand path.
pub fn lookup(path: &str) -> Option<&'static [&'static str]> {
    EXAMPLES
        .iter()
        .find(|(p, _)| *p == path)
        .map(|(_, lines)| *lines)
}

fn apply_at(cmd: clap::Command, prefix: &str) -> clap::Command {
    let names: Vec<String> = cmd
        .get_subcommands()
        .map(|sub| sub.get_name().to_owned())
        .collect();
    names.into_iter().fold(cmd, |cmd, name| {
        let path = if prefix.is_empty() {
            name.clone()
        } else {
            format!("{prefix} {name}")
        };
        cmd.mut_subcommand(&name, |sub| {
            let sub = apply_at(sub, &path);
            match lookup(&path) {
                Some(lines) => sub.after_help(render(lines)),
                None => sub,
            }
        })
    })
}

/// Attach usage examples to every subcommand's help.
pub fn apply(cmd: clap::Command) -> clap::Command {
    apply_at(cmd, "")
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    fn walk(cmd: &clap::Command, prefix: &str, out: &mut Vec<(String, bool)>) {
        for sub in cmd.get_subcommands() {
            let name = sub.get_name();
            if name == "help" {
                continue;
            }
            let path = if prefix.is_empty() {
                name.to_owned()
            } else {
                format!("{prefix} {name}")
            };
            out.push((path.clone(), sub.get_after_help().is_some()));
            walk(sub, &path, out);
        }
    }

    #[test]
    fn every_table_key_exists() {
        let cmd = crate::cli::Cli::command();
        let mut found = Vec::new();
        walk(&cmd, "", &mut found);
        let paths: Vec<&str> = found.iter().map(|(p, _)| p.as_str()).collect();
        for (path, _) in EXAMPLES {
            assert!(
                paths.contains(path),
                "table key {path:?} not found in the command tree"
            );
        }
    }

    #[test]
    fn every_subcommand_has_after_help() {
        let cmd = apply(crate::cli::Cli::command());
        let mut found = Vec::new();
        walk(&cmd, "", &mut found);
        assert!(!found.is_empty());
        for (path, has_help) in found {
            assert!(has_help, "missing after_help for {path:?}");
        }
    }
}
