---
name: rgit
description: Use for any git work in this repository - inspecting changes, committing, rewriting or undoing history, branches and stacks, and GitHub/GitLab PRs.
---

# rgit

Inspect and change the git repository in the current directory. Prefer `rgit` over raw `git`. Run `rgit` with no arguments first: it prints the repo's current state and the next useful commands.

If `rgit` is not on PATH, install it with `cargo install --locked --git https://github.com/rslib/rgit rgit-cli`.

## Start here

- Run `rgit diff --patch` to see unstaged changes
- Run `rgit stage <path>` to stage a file
- Run `rgit commit -m "<message>"` to commit staged changes
- Run `rgit log --limit 20` for recent commits
- Run `rgit smartlog` for local branches and stacks

## Output

- rgit prints TOON when stdout is not a terminal. If your shell runs commands in a terminal (a PTY), add `--toon` (alias `--axi`) so you still get TOON with no color, spinners, or prompts. `--json` gives the same data as JSON.
- Use `--toon` or `--axi`, not `--porcelain`. In rgit, as in git, `--porcelain` exists only on `status` and prints git's raw script format, with no counts, hints, or schema.
- Output ends with `help` lines naming useful next commands; follow them. Lists include counts (`count: 20 of 65 total`) and say explicitly when they are empty.
- `--fields a,b` adds table columns; an unknown field lists the valid ones. `--full` disables truncation of patches, commit bodies, and long output.
- Exit codes: 0 success (including no-ops), 1 error, 2 usage error. Errors print `error:` and `help:` on stdout.
- rgit never prompts in agent mode. Pass every value as a flag or argument.

## Safety

- Almost every change is recorded in the op-log. `rgit undo` restores HEAD and the working tree (including uncommitted work) from before the last operation; `rgit redo` reverses it; `rgit oplog` lists it.
- Repeating a change whose result already holds is a no-op (exit 0), for example creating an existing branch or deleting a missing tag.
- Destructive forge operations require `--yes`.
- For git plumbing rgit does not wrap, use `rgit git <args>`.

## Git spellings

- `git add`: use `rgit stage <path>` (or `rgit stage-all`)
- `git switch`: use `rgit checkout <branch>` (`-b <new>` to create)
- `git restore`: use `rgit discard <path>` (worktree) or `rgit unstage <path>` (index)
- `git rev-parse`, `git cat-file`, `git ls-files`, `git for-each-ref`: plumbing is not wrapped; run `rgit git <args>`
- `git branch -d`, `git branch --delete`: use `rgit branch delete <name>`
- `git branch -D`: use `rgit branch delete <name> --force`
- `git branch -m`, `git branch --move`: use `rgit branch rename <old> <new>`
- `git stash save`: use `rgit stash push [<message>]`
- `git stash show`: use `rgit stash list`, then `rgit git stash show -p <stash>`
- `git log --graph`: use `rgit smartlog` for the branch graph
- `git log -p`, `git log --patch`: use `rgit show <id> --patch` for one commit's patch
- `git push -f`: use `--force-with-lease` (or `--force`)

## More commands

rgit also covers history rewriting (reword, squash, split, move, absorb), stacked branches, lanes, copy-on-write workspaces, branching workflows, remotes, GitHub/GitLab repos and PRs, and code search. When a task needs a command not shown above, read [references/commands.md](references/commands.md) for every command with examples, or run `rgit <command> --help`.
