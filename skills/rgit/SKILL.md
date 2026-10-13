---
name: rgit
description: Load before you run any git command or answer a git question in this repository, even a quick status, diff, log, add or commit. rgit replaces git for inspecting changes, staging, committing, rewriting or undoing history, branches and stacks, and GitHub/GitLab PRs.
---

# rgit

Inspect and change the git repository in the current directory. Type `rgit` wherever you would type `git`. rgit takes git's commands and flags, so `git status --short` becomes `rgit status --short` and `git rev-parse HEAD` becomes `rgit rev-parse HEAD`. Do not run `git` or `rgit git` for a command rgit has. rgit prints TOON by default when an agent harness runs it; `--json` gives the same data as JSON, and `--human` (or `RGIT_OUTPUT=human`) prints git's text. Run `rgit` with no other arguments first: it prints the repo's current state and the next useful commands.

If `rgit` is not on PATH, install it with `cargo install --locked --git https://github.com/rslib/rgit rgit-cli`.

## Start here

- Run `rgit --toon diff` to see unstaged changes
- Run `rgit --toon stage <path>` to stage a file
- Run `rgit --toon commit -m "<message>"` to commit staged changes
- Run `rgit --toon log --limit 20` for recent commits
- Run `rgit --toon smartlog` for local branches and stacks

## Output

- rgit prints TOON by default when an agent harness runs it, so no output flag is needed: structured lines and tables, no color, spinners or prompts, and `help` lines that name the next command. The harness is one of Claude Code, Codex, Gemini CLI, Cursor, OpenCode, goose, amp or pi. `--json` prints the same data as JSON. `--human` (or `RGIT_OUTPUT=human`, or config `rgit.output=human`) prints git's text, as does a shell with no harness marker. Other examples in this skill and its reference omit the output flag; it is already on.
- Use `--toon` or `--axi`, not `--porcelain`. In rgit, as in git, `--porcelain` exists only on `status` and prints git's raw script format, with no counts, hints, or schema.
- Output ends with `help` lines naming useful next commands, already carrying `--toon` (or `--json`); follow them. Lists include counts (`count: 20 of 65 total`) and say explicitly when they are empty.
- `--fields a,b` adds table columns; an unknown field lists the valid ones. `--full` disables truncation of patches, commit bodies, and long output.
- Exit codes: 0 success (including no-ops), 1 error, 2 usage error. With `--toon` or `--json`, errors print `error:` and `help:` on stdout.
- rgit never prompts with `--toon` or `--json`, or when stdin or stdout is not a terminal. Pass every value as a flag or argument.

## Safety

- Almost every change is recorded in the op-log. `rgit undo` restores HEAD and the working tree (including uncommitted work) from before the last operation; `rgit redo` reverses it; `rgit oplog` lists it.
- Repeating a change whose result already holds is a no-op (exit 0), for example creating an existing branch or deleting a missing tag.
- Destructive forge operations require `--yes`.
- rgit runs git's plumbing too (`rgit rev-parse`, `rgit ls-files`, `rgit cat-file`, `rgit for-each-ref`). Use `rgit git <args>` only for a git tool that rgit does not have, such as `git lfs`.

## Git spellings

- `git stash save`: use `rgit stash push -m <message>`
- `git log --graph`: use `rgit smartlog` for the branch graph
- `git push -f`: use `--force-with-lease` (or `--force`)

## More commands

rgit also covers history rewriting (reword, squash, split, move, absorb), stacked branches, lanes, copy-on-write workspaces, branching workflows, remotes, GitHub/GitLab repos and PRs, and code search. When a task needs a command not shown above, read [references/commands.md](references/commands.md) for every command with examples, or run `rgit <command> --help`.
