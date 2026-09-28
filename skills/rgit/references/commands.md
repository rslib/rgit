# rgit command reference

Every command with what it does and example invocations. Run `rgit <command> --help` for every flag.

## Inspect

- `status`: Compact working-tree status. Agents: add `--toon` for a structured table. `--porcelain`, `--short`, `--branch`, and `-z` print git's raw formats, exactly as `git status` does, for scripts. e.g. `rgit status`, `rgit status --toon`
- `log`: Recent commits as `sha subject` lines. e.g. `rgit log --limit 50`, `rgit log main -- src/lib.rs`, `rgit log main..feature`, `rgit log --author alice --since 2024-01-01`, `rgit log --grep fix --no-merges -p`, `rgit log --follow --stat -- src/lib.rs`
- `diff`: Diffstat of unstaged changes (`--cached` for staged), against a revision, or between two revisions (`A B`, `A..B`, `A...B`). e.g. `rgit diff`, `rgit diff --cached -- src`, `rgit diff main...HEAD --name-status`, `rgit diff v1 HEAD --patch -U1 -w -- src`
- `show`: A commit's header and diffstat, or a file (`rev:path`) or folder at a revision. e.g. `rgit show HEAD`, `rgit show <rev> --patch`, `rgit show <rev> --name-only`, `rgit show HEAD~1:src/lib.rs`
- `blame`: Blame a file: `sha author line` per line. e.g. `rgit blame src/lib.rs`, `rgit blame src/lib.rs -L 10,20`, `rgit blame v1 -- src/lib.rs`
- `refs`: All refs (local branches, remotes, tags). e.g. `rgit refs`
- `smartlog`: Smartlog: your local/draft commits and the trunk they branch from. e.g. `rgit smartlog`, `rgit sl`
- `describe`: Describe a revision relative to the nearest tag (default HEAD). e.g. `rgit describe`, `rgit describe HEAD --tags`, `rgit describe --tags --abbrev=0`

## Stage and discard

- `stage`: Stage paths, some hunks of one path, or specific lines of one hunk. e.g. `rgit stage src/lib.rs`, `rgit stage src/lib.rs --hunk 42`, `rgit stage src/lib.rs --hunk 10,42`, `rgit stage src/lib.rs --hunk 42 --lines 0,2`
- `unstage`: Unstage paths, some hunks of one path, or specific lines of one hunk. e.g. `rgit unstage src/lib.rs`, `rgit unstage src/lib.rs --hunk 42`
- `stage-all`: Stage every change. e.g. `rgit stage-all`
- `unstage-all`: Unstage everything. e.g. `rgit unstage-all`
- `discard`: Discard unstaged changes to paths, some hunks of one path, or specific lines. e.g. `rgit discard src/lib.rs`, `rgit discard src/lib.rs --hunk 10`
- `resolve`: Resolve a conflicted path by taking ours or theirs. e.g. `rgit resolve src/lib.rs --ours`, `rgit resolve src/lib.rs --theirs`
- `rm`: Remove tracked paths from the index and working tree. e.g. `rgit rm src/lib.rs`, `rgit rm src/lib.rs --cached`
- `mv`: Rename/move tracked files or folders. e.g. `rgit mv src/old.rs src/new.rs`
- `clean`: Remove untracked files and directories. e.g. `rgit clean`, `rgit clean --dry-run`

## Commit and rewrite history

- `commit`: Commit the staged changes (runs hooks). e.g. `rgit commit -m "<message>"`, `rgit commit -a -m "<message>"`, `rgit commit --amend`
- `extend`: Amend HEAD with the staged changes, keeping its message (no editor). e.g. `rgit extend`
- `reword`: Change any commit's message and restack its descendants (default HEAD). e.g. `rgit reword -m "<message>"`, `rgit reword -m "<message>" <rev>`
- `uncommit`: Undo the last commit(s), keeping the changes staged (default 1). e.g. `rgit uncommit`, `rgit uncommit 2`
- `squash`: Fold a commit into its parent (default HEAD), or a whole range with `--from` (fold every commit after <rev> up to HEAD into one). e.g. `rgit squash`, `rgit squash <rev>`, `rgit squash --from <rev>`
- `move`: Move a commit before or after another in the current branch's history. e.g. `rgit move <rev> --before <rev>`, `rgit move <rev> --after <rev>`
- `split`: Split a commit into two by path (given paths first, the rest second). e.g. `rgit split src/lib.rs src/main.rs`, `rgit split --rev <rev> src/lib.rs`
- `absorb`: Fold each pending change into the stacked commit that last touched those lines (blame-routed fixups + autosquash). e.g. `rgit absorb`
- `cherry-pick`: Cherry-pick commits onto HEAD, or continue/skip/abort a stopped one. e.g. `rgit cherry-pick <rev>`, `rgit cherry-pick <rev> --no-commit`, `rgit cherry-pick <a> <b>`, `rgit cherry-pick main~3..main -x`, `rgit cherry-pick <merge> -m 1`, `rgit cherry-pick --continue`, `rgit cherry-pick --abort`
- `revert`: Revert commits on HEAD, or continue/skip/abort a stopped revert. e.g. `rgit revert <rev>`, `rgit revert <rev> --no-commit`, `rgit revert HEAD~2..HEAD`, `rgit revert <merge> -m 1`, `rgit revert --continue`
- `reset`: Reset HEAD to a revision (default mixed). e.g. `rgit reset HEAD~1`, `rgit reset --hard <rev>`, `rgit reset -- src/lib.rs`

## Undo

- `undo`: Undo the last operation from the op-log, restoring HEAD and the working tree (recovers uncommitted work). Set RGIT_OPLOG=0 to disable the op-log. e.g. `rgit undo`
- `redo`: Redo the operation most recently undone. e.g. `rgit redo`
- `oplog`: Show the operation log (the undo stack), newest first. e.g. `rgit oplog`

## Branches, tags, stashes

- `branch`: Branch management (no subcommand lists local branches). e.g. `rgit branch`, `rgit branch -a`
  - `branch create`: Create a branch and switch to it. e.g. `rgit branch create <branch>`
  - `branch checkout`: Check out an existing branch. e.g. `rgit branch checkout <branch>`
  - `branch delete`: Delete a branch (multiselect prompt if no name on a terminal). e.g. `rgit branch delete <branch>`, `rgit branch delete <branch> --force`
  - `branch rename`: Rename a branch. e.g. `rgit branch rename <old> <new>`
  - `branch prune`: Delete every local branch already merged into a base (default HEAD). e.g. `rgit branch prune`, `rgit branch prune main`
- `checkout`: Check out a branch or, for any other revision, a detached HEAD. e.g. `rgit checkout main`, `rgit checkout -b <new_branch>`
- `merge`: Merge revisions into the current branch (several make an octopus merge). e.g. `rgit merge <branch>`, `rgit merge <branch> --no-ff`, `rgit merge <branch> --squash`, `rgit merge <branch> -m "<message>"`, `rgit merge <a> <b>`, `rgit merge --continue`, `rgit merge --abort`
- `rebase`: Rebase onto a revision, or continue/skip/abort an in-progress rebase. e.g. `rgit rebase main`, `rgit rebase --onto <newbase> <upstream>`, `rgit rebase --autosquash main`, `rgit rebase --exec "cargo test" main`, `rgit rebase --root`, `rgit rebase --continue`
- `tag`: Tag management: `tag` lists, `tag <name>` creates, `tag -d <name>` deletes. e.g. `rgit tag`, `rgit tag v1.0.0 -m "<message>"`, `rgit tag -d v1.0.0`
- `stash`: Stash management (no subcommand stashes the working tree). e.g. `rgit stash`, `rgit stash push "<message>"`
  - `stash push`: Stash the working tree, with an optional message. e.g. `rgit stash push "<message>" --include-untracked`
  - `stash pop`: Apply a stash and drop it (prompted for if no index on a terminal). e.g. `rgit stash pop`, `rgit stash pop 1`
  - `stash apply`: Apply a stash without dropping it. e.g. `rgit stash apply`
  - `stash drop`: Drop a stash. e.g. `rgit stash drop 0`
  - `stash list`: List the stashes. e.g. `rgit stash list`
- `bisect`: Run a git bisect subcommand: `start <bad> <good>`, `good`, `bad`, `reset`. e.g. `rgit bisect start HEAD main`, `rgit bisect good`, `rgit bisect bad`
- `prune`: Prune unreachable objects (git's `prune`). For deleting merged branches, use `branch prune`. e.g. `rgit prune`, `rgit prune --dry-run`

## Stacks, lanes, workspaces, worktrees

- `stack`: Stacked branches: chain branches and restack descendants after edits (no subcommand lists the current stack). e.g. `rgit stack`
  - `stack new`: Create a new branch stacked on the current one. e.g. `rgit stack new <branch>`
  - `stack list`: List the stack containing the current branch. e.g. `rgit stack list`
  - `stack restack`: Rebase every descendant onto its parent's new tip. e.g. `rgit stack restack`
- `next`: Check out the branch stacked on this one (move up the stack). e.g. `rgit next`
- `prev`: Check out this branch's stack parent (move down the stack). e.g. `rgit prev`
- `sync`: Fetch, fast-forward branches to their upstreams, and restack the stack. e.g. `rgit sync`
- `submit`: Push every branch in the stack and open a pull request per branch. e.g. `rgit submit`
- `lanes`: Lanes: several lines of work in one worktree. Assign uncommitted files to lanes and commit each to its own branch (no subcommand lists the lanes). e.g. `rgit lanes`
  - `lanes init`: Enter lanes mode: record the fork point and a default lane. e.g. `rgit lanes init`
  - `lanes off`: Leave lanes mode (lane branches are kept). e.g. `rgit lanes off`
  - `lanes list`: List the lanes and their owned files (the default when no subcommand). e.g. `rgit lanes list`
  - `lanes new`: Create a new lane committing to a same-named branch. e.g. `rgit lanes new <lane>`
  - `lanes stack`: Create a new lane stacked on another (its commits build on that lane). e.g. `rgit lanes stack <lane> --on <parent_lane>`
  - `lanes assign`: Assign a worktree path to a lane, or a single hunk with `--hunk`. e.g. `rgit lanes assign <lane> src/lib.rs`, `rgit lanes assign <lane> src/lib.rs --hunk 10`
  - `lanes unassign`: Return a path to the default lane. e.g. `rgit lanes unassign src/lib.rs`
  - `lanes commit`: Commit a lane's owned changes to its branch. e.g. `rgit lanes commit <lane> -m "<message>"`
  - `lanes rename`: Rename a lane and its branch. e.g. `rgit lanes rename <old> <new>`
  - `lanes delete`: Delete a lane (its changes return to default; its branch is kept). e.g. `rgit lanes delete <lane>`
  - `lanes push`: Push a lane's branch to the remote. e.g. `rgit lanes push <lane>`
  - `lanes pr`: Push a lane's branch and open a pull request (via gh/glab). e.g. `rgit lanes pr <lane>`
  - `lanes restack`: Move each stacked lane onto its parent lane's new tip (in the odb; the worktree is not touched). e.g. `rgit lanes restack`
- `workspace`: Copy-on-write workspaces: instant, isolated, block-sharing clones of the whole repo (code, build, .git) for parallel work (no subcommand lists). e.g. `rgit workspace`
  - `workspace new`: Create a copy-on-write clone of the repo on a new branch. e.g. `rgit workspace new <name>`
  - `workspace list`: List this repo's workspaces. e.g. `rgit workspace list`
  - `workspace remove`: Remove a workspace. e.g. `rgit workspace remove <name>`
- `worktree`: Worktree management (no subcommand lists worktrees). e.g. `rgit worktree`
  - `worktree add`: Add a linked worktree. e.g. `rgit worktree add <branch> ../<path>`
  - `worktree remove`: Remove a linked worktree. e.g. `rgit worktree remove <branch>`, `rgit worktree remove <branch> --force`
  - `worktree prune`: Prune worktree entries whose working tree is gone (git's `worktree prune`). e.g. `rgit worktree prune`
- `flow`: Branching workflows: pick a preset (gitflow, github, gitlab, trunk, release-flow); start/finish/release then follow its rules. e.g. `rgit flow status`
  - `flow init`: Set the active workflow: gitflow, github, gitlab, trunk, release-flow. e.g. `rgit flow init gitflow`
  - `flow start`: Start a feature branch per the active workflow. e.g. `rgit flow start <name>`
  - `flow finish`: Finish the current feature (local merge, or push + PR per the workflow). e.g. `rgit flow finish`
  - `flow release`: Start a release (or finish it with --finish). e.g. `rgit flow release 1.2.0`, `rgit flow release 1.2.0 --finish`
  - `flow status`: Show the active workflow and its policy. e.g. `rgit flow status`

## Remotes and forge (GitHub/GitLab)

- `fetch`: Fetch the current branch's remote, or `<repository> [<refspec>...]`. e.g. `rgit fetch`, `rgit fetch --all`, `rgit fetch --remote origin --prune`, `rgit fetch origin main --depth 1`, `rgit fetch --dry-run`
- `pull`: Fetch and integrate the current branch's upstream (merges when it has diverged, unless `pull.rebase` says otherwise). e.g. `rgit pull`, `rgit pull --rebase`, `rgit pull --ff-only`, `rgit pull origin main`
- `push`: Push the current branch to its upstream, or `<repository> [<refspec>...]`. e.g. `rgit push`, `rgit push --set-upstream`, `rgit push --force-with-lease`, `rgit push origin feature`, `rgit push origin local:remote`, `rgit push origin --delete feature`, `rgit push --all --dry-run`
- `remote`: Remote management (no subcommand lists remotes). e.g. `rgit remote`, `rgit remote add origin https://github.com/example/repo.git`
  - `remote add`: Add a remote. e.g. `rgit remote add origin https://github.com/example/repo.git`
  - `remote remove`: Remove a remote. e.g. `rgit remote remove origin`
  - `remote set-url`: Change a remote's URL. e.g. `rgit remote set-url origin https://github.com/example/repo.git`
  - `remote rename`: Rename a remote. e.g. `rgit remote rename origin upstream`
- `forge`: Manage GitHub repositories, branches, and pull requests without gh. e.g. `rgit forge --profile work whoami`, `rgit forge pr list`
  - `forge login`: Store a forge credential in the OS credential store. e.g. `rgit forge login github --token-stdin`, `rgit forge login gitlab --host https://gitlab.example.com --token-stdin`
  - `forge auth`: Show authentication status for configured forge providers. e.g. `rgit forge auth list`
    - `forge auth list`: List known forge providers and whether credentials are stored. e.g. `rgit forge auth list`
    - `forge auth status`: Show detailed authentication status. e.g. `rgit forge auth status`
  - `forge whoami`: Show the authenticated account for one forge. e.g. `rgit forge whoami github`
  - `forge logout`: Remove the stored forge credential. e.g. `rgit forge logout github`
  - `forge repo`: View, create, or delete a hosted repository. e.g. `rgit forge repo view`
    - `forge repo view`: Show repository metadata. e.g. `rgit forge repo view`, `rgit forge repo view owner/repo`
    - `forge repo create`: Create a repository. e.g. `rgit forge repo create <name>`, `rgit forge repo create <name> --organization <org> --private`
    - `forge repo delete`: Delete a repository after explicit confirmation. e.g. `rgit forge repo delete owner/repo --yes`
  - `forge branch`: List or delete branches on the forge. e.g. `rgit forge branch list`
    - `forge branch list`: List remote branches, 100 per page. e.g. `rgit forge branch list`, `rgit forge branch list owner/repo`
    - `forge branch delete`: Delete a remote branch after explicit confirmation. e.g. `rgit forge branch delete <branch> --yes`, `rgit forge branch delete <branch> owner/repo --yes`
  - `forge pr`: List, create, or close pull requests (merge requests on GitLab). e.g. `rgit forge pr list`
    - `forge pr list`: List open pull requests or merge requests, 100 per page. e.g. `rgit forge pr list`, `rgit forge pr list owner/repo`, `rgit forge pr list --page 2`
    - `forge pr create`: Create a pull request or merge request. e.g. `rgit forge pr create --title "<title>" --head <branch> --base main`
    - `forge pr close`: Close a pull request or merge request. e.g. `rgit forge pr close 42 --yes`, `rgit forge pr close 42 owner/repo --yes`

## Patches, notes, config and maintenance

- `format-patch`: Write commits as mbox patch files (`-<n>`, `<since>` or `<a>..<b>`). e.g. `rgit format-patch -1`, `rgit format-patch main -o patches`, `rgit format-patch main..topic --stdout`
- `am`: Apply mbox patches (from format-patch) as commits. e.g. `rgit am patches/*.patch`, `rgit am --continue`, `rgit am --abort`
- `apply`: Apply a patch to the working tree, the index, or both. e.g. `rgit apply fix.patch`, `rgit apply --check fix.patch`, `rgit apply --cached fix.patch`, `rgit apply -R fix.patch`
- `archive`: Write a tar or zip of a revision's files. e.g. `rgit archive -o release.tar.gz`, `rgit archive v1.0 --prefix project/ -o project.zip`, `rgit archive HEAD src > src.tar`
- `notes`: Notes attached to commits (no subcommand lists them). e.g. `rgit notes`, `rgit notes show HEAD`
  - `notes list`: List notes as `<note id> <object id>`, or the note id of one object. e.g. `rgit notes list`
  - `notes show`: Print an object's note (default HEAD). e.g. `rgit notes show`, `rgit notes show <rev>`
  - `notes add`: Attach a note to an object (default HEAD). e.g. `rgit notes add -m "<note>"`, `rgit notes add <rev> -m "<note>" --force`
  - `notes append`: Add a paragraph to an object's note, creating it if needed. e.g. `rgit notes append -m "<more>"`
  - `notes remove`: Remove an object's note (default HEAD). e.g. `rgit notes remove <rev>`
- `config`: Get, set, unset or list config values: `config <key>` reads, `config <key> <value>` writes. e.g. `rgit config user.email`, `rgit config user.email me@example.com`, `rgit config --global pull.rebase true`, `rgit config --unset core.pager`, `rgit config --list`
- `update-ref`: Point a ref at a commit, or delete it, optionally only if it holds an expected value. e.g. `rgit update-ref refs/heads/topic <rev>`, `rgit update-ref refs/heads/topic <new> <old>`, `rgit update-ref -d refs/heads/topic`
- `hash-object`: Print the object id of files or stdin; -w stores them. e.g. `rgit hash-object src/lib.rs`, `rgit hash-object -w --stdin`
- `gc`: Pack the object database and prune unreachable objects. e.g. `rgit gc`, `rgit gc --prune=now`
- `fsck`: Check the object database for corruption and dangling objects. e.g. `rgit fsck`, `rgit fsck --unreachable`

## Code search

- `index`: Semantic code search: build the on-disk vector index or query it. e.g. `rgit index status`
  - `index build`: Build (or incrementally rebuild) the index. Local by default; `--root DIR` builds every repo under a directory. e.g. `rgit index build`, `rgit index build --root ~/code`
  - `index search`: Search the semantic index by meaning (local, or global with `--root`). Results are re-ranked by git history, so recently and frequently changed files surface above cold ones at equal relevance. e.g. `rgit index search "<query>"`, `rgit index search "<query>" --limit 5`
  - `index status`: Report whether an index exists and how many chunks it holds. e.g. `rgit index status`
  - `index code`: Hybrid search: fuse literal grep and semantic ranking, then re-rank by git history (churn and recency) (local, or global with `--root`). e.g. `rgit index code "<query>"`

## Repositories and escape hatch

- `init`: Create a new repository in the current directory (or PATH). e.g. `rgit init`, `rgit init <path> -b main`
- `clone`: Clone a repository into a new directory. e.g. `rgit clone https://github.com/example/repo.git`, `rgit clone https://github.com/example/repo.git repo-dir --depth 1`, `rgit clone --bare https://github.com/example/repo.git`, `rgit clone --recurse-submodules -o upstream https://github.com/example/repo.git`
- `submodule`: Submodule management: forwards to `git submodule <args>`. e.g. `rgit submodule update --init`
- `git`: Escape hatch: run any `git` subcommand and print its output. e.g. `rgit git status`, `rgit git log --oneline -5`

## Agent integration and servers

- `hooks`: Install session hooks so Claude Code, Codex, and OpenCode start with rgit context. e.g. `rgit hooks install`, `rgit hooks status`
  - `hooks install`: Install or repair the session-start hook (project scope by default). e.g. `rgit hooks install`, `rgit hooks install --app codex`, `rgit hooks install --user --app all`
  - `hooks status`: Show which agent apps have the rgit hook and whether it is current. e.g. `rgit hooks status`
- `skills`: Install the rgit Agent Skill for Claude Code, Codex, and other agents. e.g. `rgit skills list`
  - `skills list`: List embedded skills and install targets. e.g. `rgit skills list`
  - `skills show`: Print the rgit SKILL.md, or its command reference. e.g. `rgit skills show`, `rgit skills show --reference`
  - `skills install`: Install embedded skills into user or project skill directories. e.g. `rgit skills install --project`, `rgit skills install --user --target claude`
- `mcp`: Run the Model Context Protocol server over stdio. e.g. `rgit mcp`
- `serve`: Serve the web viewer for this repository, or a directory of repositories. e.g. `rgit serve`, `rgit serve --port 9000`, `rgit serve --root ~/code --clone-base https://git.example.dev`
