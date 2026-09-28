# rgit command reference

Every command with what it does and example invocations. Run `rgit <command> --help` for every flag.

## Inspect

- `status`: Compact working-tree status. Agents: add `--toon` for a structured table. `--porcelain`, `--short`, `--branch`, and `-z` print git's raw formats, exactly as `git status` does, for scripts. e.g. `rgit status`, `rgit status --toon`
- `log`: Recent commits as `sha subject` lines, or in git's formats with `--oneline`, `--format`, `--pretty` or `--graph`. e.g. `rgit log --limit 50`, `rgit log main -- src/lib.rs`, `rgit log main..feature`, `rgit log --author alice --since 2024-01-01`, `rgit log --grep fix --no-merges -p`, `rgit log --follow --stat -- src/lib.rs`, `rgit log --oneline --graph --all`, `rgit log --oneline -S parse_date -- src`, `rgit log -3 --format='%h %an %ar %s' --date=iso`
- `diff`: Diffstat of unstaged changes (`--cached` for staged), against a revision, or between two revisions (`A B`, `A..B`, `A...B`). e.g. `rgit diff`, `rgit diff --cached -- src`, `rgit diff main...HEAD --name-status`, `rgit diff v1 HEAD --patch -U1 -w -- src`, `rgit diff --quiet && echo clean`, `rgit diff main --name-only --diff-filter=A`, `rgit diff --no-index a.txt b.txt --patch`
- `show`: A commit's header and diffstat, or a file (`rev:path`) or folder at a revision. e.g. `rgit show HEAD`, `rgit show <rev> --patch`, `rgit show <rev> --name-only`, `rgit show <rev> --pretty=fuller --stat`, `rgit show HEAD~1:src/lib.rs`
- `blame`: Blame a file: `sha author line` per line. e.g. `rgit blame src/lib.rs`, `rgit blame src/lib.rs -L 10,20`, `rgit blame v1 -- src/lib.rs`, `rgit blame --porcelain -L 10,20 src/lib.rs`
- `refs`: All refs (local branches, remotes, tags). e.g. `rgit refs`
- `smartlog`: Smartlog: your local/draft commits and the trunk they branch from. e.g. `rgit smartlog`, `rgit sl`
- `describe`: Describe a revision relative to the nearest tag (default HEAD). e.g. `rgit describe`, `rgit describe HEAD --tags`, `rgit describe --tags --abbrev=0`, `rgit describe --contains <rev>`

## Stage and discard

- `stage`: Stage paths, some hunks of one path, or specific lines of one hunk. e.g. `rgit stage src/lib.rs`, `rgit stage src/lib.rs --hunk 42`, `rgit stage src/lib.rs --hunk 10,42`, `rgit stage src/lib.rs --hunk 42 --lines 0,2`
- `unstage`: Unstage paths, some hunks of one path, or specific lines of one hunk. e.g. `rgit unstage src/lib.rs`, `rgit unstage src/lib.rs --hunk 42`
- `stage-all`: Stage every change. e.g. `rgit stage-all`
- `unstage-all`: Unstage everything. e.g. `rgit unstage-all`
- `add`: Stage paths as git's `add` does: `add <paths>`, `add .`, `add -A`, `add -u`. e.g. `rgit add .`, `rgit add src/ '*.md'`, `rgit add -A`, `rgit add -u`, `rgit add -p`, `rgit add -n .`
- `discard`: Discard unstaged changes to paths, some hunks of one path, or specific lines. e.g. `rgit discard src/lib.rs`, `rgit discard src/lib.rs --hunk 10`
- `restore`: Restore files in the working tree (or, with --staged, the index) from the index or a revision. e.g. `rgit restore src/lib.rs`, `rgit restore --staged src/lib.rs`, `rgit restore --source HEAD~1 src/lib.rs`, `rgit restore --ours src/lib.rs`
- `resolve`: Resolve a conflicted path by taking ours or theirs. e.g. `rgit resolve src/lib.rs --ours`, `rgit resolve src/lib.rs --theirs`
- `rm`: Remove tracked paths from the index and working tree. e.g. `rgit rm src/lib.rs`, `rgit rm src/lib.rs --cached`, `rgit rm -n -r src/`
- `mv`: Rename/move tracked files or folders. e.g. `rgit mv src/old.rs src/new.rs`, `rgit mv -n src/old.rs src/new.rs`
- `clean`: Remove untracked files and directories. e.g. `rgit clean`, `rgit clean --dry-run`

## Commit and rewrite history

- `commit`: Commit the staged changes (runs hooks), or only the given paths. e.g. `rgit commit -m "<message>"`, `rgit commit -a -m "<message>"`, `rgit commit src/lib.rs -m "<message>"`, `rgit commit -m "<subject>" -m "<body>"`, `rgit commit --amend --no-edit`, `rgit commit --fixup <rev>`, `rgit commit -C <rev> --reset-author`
- `extend`: Amend HEAD with the staged changes, keeping its message (no editor). e.g. `rgit extend`
- `reword`: Change any commit's message and restack its descendants (default HEAD). e.g. `rgit reword -m "<message>"`, `rgit reword -m "<message>" <rev>`
- `uncommit`: Undo the last commit(s), keeping the changes staged (default 1). e.g. `rgit uncommit`, `rgit uncommit 2`
- `squash`: Fold a commit into its parent (default HEAD), or a whole range with `--from` (fold every commit after <rev> up to HEAD into one). e.g. `rgit squash`, `rgit squash <rev>`, `rgit squash --from <rev>`
- `move`: Move a commit before or after another in the current branch's history. e.g. `rgit move <rev> --before <rev>`, `rgit move <rev> --after <rev>`
- `split`: Split a commit into two by path (given paths first, the rest second). e.g. `rgit split src/lib.rs src/main.rs`, `rgit split --rev <rev> src/lib.rs`
- `absorb`: Fold each pending change into the stacked commit that last touched those lines (blame-routed fixups + autosquash). e.g. `rgit absorb`
- `cherry-pick`: Cherry-pick commits onto HEAD, or continue/skip/abort a stopped one. e.g. `rgit cherry-pick <rev>`, `rgit cherry-pick <rev> --no-commit`, `rgit cherry-pick <a> <b>`, `rgit cherry-pick main~3..main -x`, `rgit cherry-pick <merge> -m 1`, `rgit cherry-pick <rev> -s --empty=drop`, `rgit cherry-pick --continue`, `rgit cherry-pick --abort`, `rgit cherry-pick --quit`
- `revert`: Revert commits on HEAD, or continue/skip/abort a stopped revert. e.g. `rgit revert <rev>`, `rgit revert <rev> --no-commit`, `rgit revert HEAD~2..HEAD`, `rgit revert <merge> -m 1`, `rgit revert <rev> --reference`, `rgit revert --continue`
- `reset`: Reset HEAD to a revision (default mixed). e.g. `rgit reset HEAD~1`, `rgit reset --hard <rev>`, `rgit reset src/lib.rs`, `rgit reset <rev> -- src/lib.rs`, `rgit reset --merge`

## Undo

- `undo`: Undo the last operation from the op-log, restoring HEAD and the working tree (recovers uncommitted work). Set RGIT_OPLOG=0 to disable the op-log. e.g. `rgit undo`
- `redo`: Redo the operation most recently undone. e.g. `rgit redo`
- `oplog`: Show the operation log (the undo stack), newest first. e.g. `rgit oplog`

## Branches, tags, stashes

- `branch`: Branch management: no subcommand lists local branches, `branch <name> [<start>]` creates one without switching to it, and git's flags work as in git. e.g. `rgit branch`, `rgit branch -vv`, `rgit branch --merged main`, `rgit branch --sort=-committerdate --format='%(refname:short) %(upstream:short)'`, `rgit branch <branch> <start>`, `rgit branch -c <branch> <new_branch>`, `rgit branch -d <branch> <branch>`, `rgit branch -u origin/<branch>`
  - `branch create`: Create a branch (at HEAD, or START) and switch to it. e.g. `rgit branch create <branch>`, `rgit branch create <branch> origin/<branch>`
  - `branch checkout`: Check out an existing branch. e.g. `rgit branch checkout <branch>`
  - `branch delete`: Delete branches (multiselect prompt if no name on a terminal). e.g. `rgit branch delete <branch> <branch>`, `rgit branch delete <branch> --force`
  - `branch rename`: Rename a branch. e.g. `rgit branch rename <old> <new>`
  - `branch prune`: Delete every local branch already merged into a base (default HEAD). e.g. `rgit branch prune`, `rgit branch prune main`
- `checkout`: Check out a branch or, for any other revision, a detached HEAD; or restore paths from a revision or the index. e.g. `rgit checkout main`, `rgit checkout -b <new_branch>`, `rgit checkout -`, `rgit checkout <rev> -- src/lib.rs`, `rgit checkout -f <branch>`, `rgit checkout --theirs -- src/lib.rs`
- `switch`: Switch branches: `switch <branch>`, `-c <new> [<start>]`, `--detach <rev>`, or `-` for the previous branch. e.g. `rgit switch main`, `rgit switch -c <new_branch>`, `rgit switch -`, `rgit switch --detach <rev>`, `rgit switch -m <branch>`, `rgit switch --orphan <new_branch>`
- `merge`: Merge revisions into the current branch (several make an octopus merge). e.g. `rgit merge <branch>`, `rgit merge <branch> --no-ff`, `rgit merge <branch> --squash`, `rgit merge <branch> -m "<message>"`, `rgit merge <a> <b>`, `rgit merge <branch> -s ours`, `rgit merge <branch> --log --no-stat`, `rgit merge <branch> --allow-unrelated-histories`, `rgit merge --continue`, `rgit merge --abort`
- `rebase`: Rebase onto a revision, or continue/skip/abort an in-progress rebase. e.g. `rgit rebase main`, `rgit rebase --onto <newbase> <upstream>`, `rgit rebase --autosquash main`, `rgit rebase --exec "cargo test" main`, `rgit rebase --root`, `rgit rebase -r --committer-date-is-author-date main`, `rgit rebase --update-refs --autostash main`, `GIT_SEQUENCE_EDITOR="sed -i.bak 1s/^pick/edit/" rgit rebase -i main`, `rgit rebase --continue`, `rgit rebase --abort`, `rgit rebase --show-current-patch`
- `tag`: Tag management: `tag` lists, `tag <name> [<rev>]` creates, `tag -d <name>...` deletes, `tag -l <pattern>...` lists matching tags. e.g. `rgit tag`, `rgit tag v1.0.0 -m "<message>"`, `rgit tag v1.0.0 <rev>`, `rgit tag -l "v1.*" -n`, `rgit tag --sort=-v:refname --format='%(refname:short) %(creatordate:short)'`, `rgit tag -s v1.0.0 -F notes.txt`, `rgit tag --contains <rev>`, `rgit tag -d v1.0.0 v1.0.1`
- `stash`: Stash management (no subcommand stashes the working tree, taking `stash push`'s flags). e.g. `rgit stash`, `rgit stash -u`, `rgit stash push -m "<message>"`
  - `stash push`: Stash the working tree, or only some paths, with an optional message. e.g. `rgit stash push -m "<message>" --include-untracked`, `rgit stash push -- src/lib.rs`, `rgit stash push --staged -m "<message>"`, `rgit stash push -p`
  - `stash pop`: Apply a stash and drop it (prompted for if no index on a terminal). e.g. `rgit stash pop`, `rgit stash pop stash@{1} --index`
  - `stash apply`: Apply a stash without dropping it. e.g. `rgit stash apply`
  - `stash drop`: Drop a stash. e.g. `rgit stash drop 0`
  - `stash list`: List the stashes. e.g. `rgit stash list`, `rgit stash list --format="%gd %cr %gs" -n 5`
  - `stash show`: Show the changes a stash records, as a diffstat (-p for the patch). e.g. `rgit stash show`, `rgit stash show -p stash@{1}`, `rgit stash show -u --name-only`
  - `stash create`: Make a stash commit of the local changes and print its id, without storing it or touching the working tree (git's `stash create`). e.g. `rgit stash create`, `rgit stash create "<message>"`
  - `stash store`: Put a stash commit (from `stash create`) on the stash list. e.g. `rgit stash store -m "<message>" <commit>`
  - `stash branch`: Create and check out a branch at the stash's base commit, apply the stash there and drop it. e.g. `rgit stash branch <branch>`
  - `stash clear`: Drop every stash. e.g. `rgit stash clear`
- `bisect`: Find the commit that introduced a change by binary search: `start <bad> <good>`, then mark each step `good`/`bad` (or `run <cmd>`), then `reset`. e.g. `rgit bisect start HEAD main`, `rgit bisect good`, `rgit bisect bad`, `rgit bisect run cargo test`, `rgit bisect reset`
  - `bisect start`: Start a bisect, optionally with the bad commit and good ones. e.g. `rgit bisect start`, `rgit bisect start <bad> <good>`, `rgit bisect start <bad> <good> -- src/`, `rgit bisect start --term-new fixed --term-old broken`
  - `bisect bad`: Mark commits (HEAD by default) as bad: the change is there. e.g. `rgit bisect bad`, `rgit bisect bad <rev>`
  - `bisect good`: Mark commits (HEAD by default) as good: the change is not there yet. e.g. `rgit bisect good`, `rgit bisect good <a> <b>`
  - `bisect new`: Mark commits as new (with `--term-new`/`--term-old` terms). e.g. `rgit bisect new`
  - `bisect old`: Mark commits as old (with `--term-new`/`--term-old` terms). e.g. `rgit bisect old`
  - `bisect skip`: Skip commits (HEAD by default) or ranges `A..B` that cannot be tested. e.g. `rgit bisect skip`, `rgit bisect skip <a>..<b>`
  - `bisect reset`: End the bisect and check out where it started (or `commit`). e.g. `rgit bisect reset`, `rgit bisect reset <rev>`
  - `bisect log`: Print the bisect log, to save for `replay`. e.g. `rgit bisect log > bisect.log`
  - `bisect replay`: Redo the bisect a saved log records. e.g. `rgit bisect replay bisect.log`
  - `bisect run`: Mark each step by running a command: exit 0 good, 125 skip, other bad. e.g. `rgit bisect run cargo test`, `rgit bisect run ./check.sh --quick`
  - `bisect visualize`: List the commits still in the search. e.g. `rgit bisect visualize`, `rgit bisect view`
  - `bisect terms`: Print the terms for the old and new states. e.g. `rgit bisect terms`, `rgit bisect terms --term-good`
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
  - `worktree add`: Add a linked worktree at PATH, as `git worktree add` does: on BRANCH, on a new branch with -b, detached at a commit or with --detach, or by default on a branch named after PATH's last folder. e.g. `rgit worktree add ../<path> <branch>`, `rgit worktree add -b <new_branch> ../<path> origin/main`, `rgit worktree add --detach ../<path> <rev>`, `rgit worktree add -B <branch> --lock --reason "<reason>" ../<path>`, `rgit worktree add --orphan -b <new_branch> ../<path>`
  - `worktree list`: List the worktrees, as `git worktree list` prints them. e.g. `rgit worktree list -v`, `rgit worktree list --porcelain`
  - `worktree repair`: Fix the links between worktrees and the repository after moving either; name worktrees moved by hand. e.g. `rgit worktree repair`, `rgit worktree repair ../<path>`
  - `worktree remove`: Remove a linked worktree (by name or path) and its folder. e.g. `rgit worktree remove ../<path>`, `rgit worktree remove <name> --force`
  - `worktree lock`: Lock a worktree (by name or path) so prune leaves it alone. e.g. `rgit worktree lock ../<path> --reason "<reason>"`
  - `worktree unlock`: Unlock a worktree (by name or path). e.g. `rgit worktree unlock ../<path>`
  - `worktree move`: Move a worktree (by name or path) to a new path. e.g. `rgit worktree move ../<path> ../<new_path>`
  - `worktree prune`: Prune worktree entries whose working tree is gone (git's `worktree prune`). e.g. `rgit worktree prune`
- `flow`: Branching workflows: pick a preset (gitflow, github, gitlab, trunk, release-flow); start/finish/release then follow its rules. e.g. `rgit flow status`
  - `flow init`: Set the active workflow: gitflow, github, gitlab, trunk, release-flow. e.g. `rgit flow init gitflow`
  - `flow start`: Start a feature branch per the active workflow. e.g. `rgit flow start <name>`
  - `flow finish`: Finish the current feature (local merge, or push + PR per the workflow). e.g. `rgit flow finish`
  - `flow release`: Start a release (or finish it with --finish). e.g. `rgit flow release 1.2.0`, `rgit flow release 1.2.0 --finish`
  - `flow status`: Show the active workflow and its policy. e.g. `rgit flow status`

## Remotes and forge (GitHub/GitLab)

- `fetch`: Fetch the current branch's remote, or `<repository> [<refspec>...]`. e.g. `rgit fetch`, `rgit fetch --all`, `rgit fetch --remote origin --prune`, `rgit fetch origin main --depth 1`, `rgit fetch --dry-run`, `rgit fetch --unshallow`, `rgit fetch --prune --prune-tags`, `rgit fetch --multiple -j 4 origin upstream`
- `pull`: Fetch and integrate the current branch's upstream (merges when it has diverged, unless `pull.rebase` says otherwise). e.g. `rgit pull`, `rgit pull --rebase`, `rgit pull --ff-only`, `rgit pull origin main`, `rgit pull --autostash --rebase`, `rgit pull --squash origin feature`
- `push`: Push the current branch to its upstream, or `<repository> [<refspec>...]`. e.g. `rgit push`, `rgit push --set-upstream`, `rgit push --force-with-lease`, `rgit push origin feature`, `rgit push origin local:remote`, `rgit push origin --delete feature`, `rgit push origin :v1`, `rgit push --all --dry-run`, `rgit push --follow-tags`, `rgit push --atomic origin main v1`, `rgit push --porcelain origin main`, `rgit push --recurse-submodules=on-demand origin main`
- `ls-remote`: List the refs in a remote repository, like `git ls-remote`. e.g. `rgit ls-remote`, `rgit ls-remote --heads origin`, `rgit ls-remote --tags https://github.com/example/repo.git 'v1.*'`, `rgit ls-remote --symref origin HEAD`
- `remote`: Remote management (no subcommand lists remotes). e.g. `rgit remote`, `rgit remote -v`, `rgit remote add origin https://github.com/example/repo.git`
  - `remote add`: Add a remote. e.g. `rgit remote add origin https://github.com/example/repo.git`, `rgit remote add -f -t main upstream https://github.com/example/repo.git`
  - `remote remove`: Remove a remote. e.g. `rgit remote remove origin`
  - `remote set-url`: Change a remote's URL, add one (--add) or delete those matching a regex (--delete). e.g. `rgit remote set-url origin https://github.com/example/repo.git`, `rgit remote set-url --push origin https://github.com/fork/repo.git`, `rgit remote set-url --add --push origin https://github.com/mirror/repo.git`
  - `remote get-url`: Print a remote's URL. e.g. `rgit remote get-url origin`
  - `remote rename`: Rename a remote, its tracking refs and the branches that track it. e.g. `rgit remote rename origin upstream`
  - `remote prune`: Delete remote-tracking branches that no longer exist on the remotes. e.g. `rgit remote prune origin`, `rgit remote prune -n origin`
  - `remote show`: Describe remotes as `git remote show` does: URLs, HEAD branch, remote branches, and the local branches that pull from and push to them. e.g. `rgit remote show origin`, `rgit remote show -n`
  - `remote update`: Fetch remotes: all by default, or the named remotes and groups (`remotes.<group>`). e.g. `rgit remote update`, `rgit remote update -p <group>`
  - `remote set-head`: Set `<name>/HEAD`: to a branch, from the remote (-a), or delete it (-d). e.g. `rgit remote set-head origin -a`, `rgit remote set-head origin main`
  - `remote set-branches`: Fetch only these branches from a remote (with --add, these too). e.g. `rgit remote set-branches --add origin <branch>`
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

- `format-patch`: Write commits as mbox patch files (`-<n>`, `<since>` or `<a>..<b>`). e.g. `rgit format-patch -1`, `rgit format-patch main -o patches`, `rgit format-patch main..topic --stdout`, `rgit format-patch main --cover-letter --thread -v2 --to list@example.com`, `rgit format-patch -3 --rfc --base=auto --subject-prefix="PATCH net"`
- `am`: Apply mbox patches (from format-patch) as commits. e.g. `rgit am patches/*.patch`, `rgit am --continue`, `rgit am --abort`, `rgit am -3 -s --committer-date-is-author-date series.mbox`, `rgit am --show-current-patch=diff`
- `apply`: Apply a patch to the working tree, the index, or both. e.g. `rgit apply fix.patch`, `rgit apply --check fix.patch`, `rgit apply --cached fix.patch`, `rgit apply -R fix.patch`, `rgit apply --3way fix.patch`, `rgit apply --reject -p2 --directory=vendor/lib fix.patch`, `rgit apply --stat --summary --numstat fix.patch`
- `archive`: Write a tar or zip of a revision's files. e.g. `rgit archive -o release.tar.gz`, `rgit archive v1.0 --prefix project/ -o project.zip`, `rgit archive HEAD src > src.tar`, `rgit archive -9 --add-file=VERSION --format=tar.gz -o dist.tgz v1.0`, `rgit archive --remote ../other -o other.zip`, `rgit archive -l`
- `notes`: Notes attached to commits (no subcommand lists them). e.g. `rgit notes`, `rgit notes show HEAD`
  - `notes list`: List notes as `<note id> <object id>`, or the note id of one object. e.g. `rgit notes list`
  - `notes show`: Print an object's note (default HEAD). e.g. `rgit notes show`, `rgit notes show <rev>`
  - `notes add`: Attach a note to an object (default HEAD); opens the editor without -m, -F or -C. e.g. `rgit notes add -m "<note>"`, `rgit notes add <rev> -m "<note>" --force`, `rgit notes add -F notes.txt <rev>`
  - `notes copy`: Copy the note of one object to another (default HEAD). e.g. `rgit notes copy <from> <to>`
  - `notes append`: Add a paragraph to an object's note, creating it if needed. e.g. `rgit notes append -m "<more>"`
  - `notes edit`: Edit an object's note in the editor (default HEAD). e.g. `rgit notes edit <rev>`
  - `notes remove`: Remove the notes of objects (default HEAD). e.g. `rgit notes remove <rev>`, `rgit notes remove --ignore-missing a b`
  - `notes prune`: Remove the notes of objects that no longer exist. e.g. `rgit notes prune -n`
  - `notes merge`: Merge another notes ref into the current one. e.g. `rgit notes merge -s union origin`
  - `notes get-ref`: Print the notes ref in use. e.g. `rgit notes --ref review get-ref`
- `config`: Get, set, unset or list config values: `config <key>` reads, `config <key> <value>` writes. e.g. `rgit config user.email`, `rgit config user.email me@example.com`, `rgit config --global pull.rebase true`, `rgit config --unset core.pager`, `rgit config --list --show-origin`, `rgit config --get-regexp '^remote\.'`, `rgit config --type=bool --default false core.bare`, `rgit config --file .gitmodules --list`, `rgit config --rename-section branch.old branch.new`, `rgit config get --all --show-names remote.origin.fetch`, `rgit config set --all core.pager less`
- `update-ref`: Point a ref at a commit, or delete it, optionally only if it holds an expected value. e.g. `rgit update-ref refs/heads/topic <rev>`, `rgit update-ref refs/heads/topic <new> <old>`, `rgit update-ref -d refs/heads/topic`, `printf 'start\nupdate refs/heads/a <new> <old>\ncommit\n' | rgit update-ref --stdin`, `rgit update-ref --create-reflog refs/backup/main main`
- `hash-object`: Print the object id of files or stdin; -w stores them. e.g. `rgit hash-object src/lib.rs`, `rgit hash-object -w --stdin`, `rgit hash-object --stdin --path src/lib.rs`, `rgit hash-object --no-filters -t blob file.bin`
- `gc`: Pack the object database and prune unreachable objects. e.g. `rgit gc`, `rgit gc --prune=now`, `rgit gc --aggressive --cruft`
- `fsck`: Check the object database for corruption and dangling objects. e.g. `rgit fsck`, `rgit fsck --unreachable --no-reflogs`, `rgit fsck --lost-found`, `rgit fsck --name-objects --full`
- `repack`: Pack the repository's objects (`git repack`). e.g. `rgit repack -a -d`, `rgit repack -a -d -b --cruft`
- `pack-refs`: Move loose refs into packed-refs (`git pack-refs`). e.g. `rgit pack-refs --all`
- `maintenance`: Background upkeep (`git maintenance`): run tasks now, schedule them, or (un)register this repository. e.g. `rgit maintenance run --task=gc`, `rgit maintenance start`
  - `maintenance run`: Run maintenance tasks now. e.g. `rgit maintenance run`, `rgit maintenance run --task=commit-graph --task=loose-objects`
  - `maintenance start`: Register this repository and schedule hourly/daily/weekly runs. e.g. `rgit maintenance start`
  - `maintenance stop`: Remove the schedule (the repositories stay registered). e.g. `rgit maintenance stop`
  - `maintenance register`: Add this repository to the global maintenance.repo list. e.g. `rgit maintenance register`
  - `maintenance unregister`: Remove this repository from the global maintenance.repo list. e.g. `rgit maintenance unregister --force`
- `cherry`: Commits not yet upstream (`git cherry`): `+ <id>` for each, `- <id>` when upstream already has an equivalent change. e.g. `rgit cherry`, `rgit cherry -v origin/main topic`
- `bundle`: Move history as one file (`git bundle`): create, verify, list-heads, unbundle. e.g. `rgit bundle create repo.bundle --all`, `rgit bundle verify repo.bundle`
  - `bundle create`: Write a bundle of the refs and ranges given (`--all`, `main`, `v1..main`, `^old main`). e.g. `rgit bundle create repo.bundle --all`, `rgit bundle create update.bundle v1.0..main`
  - `bundle verify`: Check that a bundle is valid and applies to this repository. e.g. `rgit bundle verify update.bundle`
  - `bundle list-heads`: List the refs in a bundle. e.g. `rgit bundle list-heads repo.bundle`
  - `bundle unbundle`: Store a bundle's objects here and print its refs (refs are not changed). e.g. `rgit bundle unbundle update.bundle`
- `request-pull`: Summarize changes for a pull request by mail (`git request-pull`): what `url` holds beyond `start`. e.g. `rgit request-pull origin/main https://example.com/me/repo.git topic`, `rgit request-pull -p v1.0 origin`
- `range-diff`: Compare two versions of a series (`git range-diff`): `<base> <old> <new>`, `<old-range> <new-range>` or `<old>...<new>`. e.g. `rgit range-diff main topic-v1 topic-v2`, `rgit range-diff -s main..topic@{1} main..topic`, `rgit range-diff topic@{u}...topic`
- `difftool`: Show changes in the configured diff tool (`git difftool`): diff.tool, difftool.<tool>.cmd or a known tool (vimdiff, meld, code, ...). e.g. `rgit difftool -y -t meld`, `rgit difftool --cached`, `rgit difftool -d main topic`
- `mergetool`: Resolve conflicts in the configured merge tool (`git mergetool`): merge.tool, mergetool.<tool>.cmd or a known tool. e.g. `rgit mergetool`, `rgit mergetool -t vimdiff -- src/lib.rs`

## Plumbing (git's own output formats)

- `rev-parse`: Resolve revisions to object ids, or print repository paths, like `git rev-parse`. Answers print in the order the options and revisions are given. e.g. `rgit rev-parse HEAD`, `rgit rev-parse --short HEAD`, `rgit rev-parse --abbrev-ref HEAD`, `rgit rev-parse --show-toplevel --show-prefix`, `rgit rev-parse --symbolic-full-name @{u}`
- `ls-files`: List files in the index and the working tree, like `git ls-files`. e.g. `rgit ls-files`, `rgit ls-files -s src`, `rgit ls-files -o --exclude-standard`
- `ls-tree`: List a tree's entries, like `git ls-tree`. e.g. `rgit ls-tree HEAD`, `rgit ls-tree -r -l HEAD src/`
- `cat-file`: Print an object's type, size or content, like `git cat-file`. e.g. `rgit cat-file -p HEAD`, `rgit cat-file -p HEAD:src/lib.rs`, `rgit cat-file -t <rev>`, `rgit cat-file --batch-check < ids.txt`
- `show-ref`: List refs with their object ids, like `git show-ref`. e.g. `rgit show-ref --heads`, `rgit show-ref --verify refs/heads/main`
- `for-each-ref`: List refs in a custom format, like `git for-each-ref`. e.g. `rgit for-each-ref refs/heads`, `rgit for-each-ref --sort=-committerdate --format='%(refname:short) %(subject)'`, `rgit for-each-ref --merged main refs/heads`
- `rev-list`: List commit ids reachable from revisions, like `git rev-list`. e.g. `rgit rev-list --count HEAD`, `rgit rev-list main..HEAD`, `rgit rev-list -n 1 --all`, `rgit rev-list HEAD -- src/lib.rs`
- `merge-base`: Find the common ancestor of two commits, like `git merge-base`. e.g. `rgit merge-base main HEAD`, `rgit merge-base --is-ancestor main HEAD`
- `reflog`: Show where a ref pointed over time, like `git reflog show`. e.g. `rgit reflog`, `rgit reflog show main -n 10`
- `shortlog`: Summarize commits by author, like `git shortlog`. e.g. `rgit shortlog -sn`, `rgit shortlog -sne --all`
- `grep`: Search tracked files, the index or a revision, like `git grep`. Paths print from the current folder, which limits the search by default. e.g. `rgit grep -n TODO`, `rgit grep -i -e foo -e bar -- src`, `rgit grep pattern HEAD~5`, `rgit grep -n -C2 --heading TODO`
- `check-ignore`: Show which paths are ignored and by which rule, like `git check-ignore`. e.g. `rgit check-ignore -v target/out.o`, `rgit check-ignore --stdin < paths.txt`
- `var`: Print a git variable: GIT_AUTHOR_IDENT, GIT_COMMITTER_IDENT, GIT_EDITOR, GIT_SEQUENCE_EDITOR, GIT_PAGER, GIT_DEFAULT_BRANCH, GIT_SHELL_PATH, GIT_ATTR_SYSTEM, GIT_ATTR_GLOBAL, GIT_CONFIG_SYSTEM or GIT_CONFIG_GLOBAL, like `git var`. e.g. `rgit var GIT_AUTHOR_IDENT`, `rgit var GIT_EDITOR`, `rgit var -l`
- `symbolic-ref`: Print where a symbolic ref points (`HEAD` -> `refs/heads/main`), point it elsewhere, or delete it, like `git symbolic-ref`. e.g. `rgit symbolic-ref HEAD`, `rgit symbolic-ref --short HEAD`, `rgit symbolic-ref HEAD refs/heads/main`
- `count-objects`: Count loose and packed objects, like `git count-objects`. e.g. `rgit count-objects -v`

## Code search

- `index`: Semantic code search: build the on-disk vector index or query it. e.g. `rgit index status`
  - `index build`: Build (or incrementally rebuild) the index. Local by default; `--root DIR` builds every repo under a directory. e.g. `rgit index build`, `rgit index build --root ~/code`
  - `index search`: Search the semantic index by meaning (local, or global with `--root`). Results are re-ranked by git history, so recently and frequently changed files surface above cold ones at equal relevance. e.g. `rgit index search "<query>"`, `rgit index search "<query>" --limit 5`
  - `index status`: Report whether an index exists and how many chunks it holds. e.g. `rgit index status`
  - `index code`: Hybrid search: fuse literal grep and semantic ranking, then re-rank by git history (churn and recency) (local, or global with `--root`). e.g. `rgit index code "<query>"`

## Repositories and escape hatch

- `init`: Create a new repository in the current directory (or PATH). e.g. `rgit init`, `rgit init <path> -b main`, `rgit init --bare --shared=group <path>.git`, `rgit init --separate-git-dir ../<path>.git`
- `clone`: Clone a repository into a new directory. e.g. `rgit clone https://github.com/example/repo.git`, `rgit clone https://github.com/example/repo.git repo-dir --depth 1`, `rgit clone --bare https://github.com/example/repo.git`, `rgit clone --recurse-submodules -o upstream https://github.com/example/repo.git`, `rgit clone --single-branch -b main https://github.com/example/repo.git`, `rgit clone --mirror https://github.com/example/repo.git`, `rgit clone --filter=blob:none https://github.com/example/repo.git`, `rgit clone --shallow-since=2024-01-01 https://github.com/example/repo.git`, `rgit clone --reference ../other --dissociate https://github.com/example/repo.git`, `rgit clone --sparse https://github.com/example/repo.git`
- `submodule`: Submodules: status (the default), add, init, update, sync, deinit, foreach, summary, set-url, set-branch and absorbgitdirs. e.g. `rgit submodule`, `rgit submodule update --init --recursive`
  - `submodule status`: Show each submodule's commit, path and name for it: ` ` in step, `-` not initialized, `+` another commit checked out, `U` conflicts. e.g. `rgit submodule status --recursive`
  - `submodule add`: Clone URL into PATH (default: the URL's name) and add it as a submodule. e.g. `rgit submodule add https://github.com/example/lib.git vendor/lib`, `rgit submodule add -b main https://github.com/example/lib.git`
  - `submodule init`: Register submodules in .git/config from .gitmodules. e.g. `rgit submodule init`
  - `submodule update`: Clone missing submodules and check out the commits the superproject records. e.g. `rgit submodule update --init --recursive`, `rgit submodule update --remote vendor/lib`
  - `submodule sync`: Copy submodule URLs from .gitmodules into the config and the submodules. e.g. `rgit submodule sync --recursive`
  - `submodule deinit`: Unregister submodules and empty their folders. e.g. `rgit submodule deinit vendor/lib`, `rgit submodule deinit -f --all`
  - `submodule foreach`: Run a shell command in each checked-out submodule ($name, $sm_path, $displaypath, $sha1 and $toplevel are set). e.g. `rgit submodule foreach 'echo $sm_path $sha1'`
  - `submodule summary`: Summarize the commits between recorded and checked-out submodule commits. e.g. `rgit submodule summary`
  - `submodule set-url`: Change a submodule's URL in .gitmodules and sync it. e.g. `rgit submodule set-url vendor/lib https://github.com/fork/lib.git`
  - `submodule set-branch`: Set (or with -d, clear) the branch `update --remote` follows. e.g. `rgit submodule set-branch -b main vendor/lib`, `rgit submodule set-branch -d vendor/lib`
  - `submodule absorbgitdirs`: Move submodules' repositories into the superproject's .git/modules. e.g. `rgit submodule absorbgitdirs`
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
