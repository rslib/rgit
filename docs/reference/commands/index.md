# Commands

Every rgit command, grouped by task. Options that work with every command are on the [global options](global-options.md) page.

## Inspect

| Command | What it does |
| --- | --- |
| [`status`](status.md) | Working-tree status as git prints it |
| [`log`](log.md) | Commits in git's medium format (`--compact` for `sha subject` lines), or in git's other formats with `--oneline`, `--format`, `--pretty` or `--graph` |
| [`diff`](diff.md) | Patch of unstaged changes (`--cached` for staged), against a revision, or between two revisions |
| [`show`](show.md) | A commit as git shows it (`--compact` for header and diffstat), or a file (`rev:path`) or folder at a revision |
| [`blame`](blame.md) | Blame a file in git's format, with its display flags |
| [`refs`](refs.md) | All refs |
| [`smartlog`](smartlog.md) | Smartlog: your local/draft commits and the trunk they branch from |
| [`describe`](describe.md) | Describe a revision relative to the nearest tag |

## Stage and discard

| Command | What it does |
| --- | --- |
| [`stage`](stage.md) | Stage paths, some hunks of one path, or specific lines of one hunk |
| [`unstage`](unstage.md) | Unstage paths, some hunks of one path, or specific lines of one hunk |
| [`stage-all`](stage-all.md) | Stage every change |
| [`unstage-all`](unstage-all.md) | Unstage everything |
| [`add`](add.md) | Stage paths as git's `add` does: `add &lt;paths&gt;`, `add .`, `add -A`, `add -u` |
| [`discard`](discard.md) | Discard unstaged changes to paths, some hunks of one path, or specific lines |
| [`restore`](restore.md) | Restore files in the working tree (or, with --staged, the index) from the index or a revision |
| [`resolve`](resolve.md) | Resolve a conflicted path by taking ours or theirs |
| [`rm`](rm.md) | Remove tracked paths from the index and working tree |
| [`mv`](mv.md) | Rename/move tracked files or folders |
| [`clean`](clean.md) | Remove untracked files and directories |
| [`sparse-checkout`](sparse-checkout.md) | Check out only some folders, like `git sparse-checkout`: `set`, `add`, `list`, `init`, `reapply`, `disable` and `check-rules`, with `--cone`, `--no-cone`, `--[no-]sparse-index`, `--skip-checks`, `--stdin`, and check-rules' `-z` and `--rules-file &lt;file&gt;` |

## Commit and rewrite history

| Command | What it does |
| --- | --- |
| [`commit`](commit.md) | Commit the staged changes (runs hooks), or only the given paths |
| [`ai`](ai.md) | Draft or review a commit message with an AI tool you configure, or show the prompt it gets |
| [`extend`](extend.md) | Amend HEAD with the staged changes, keeping its message |
| [`reword`](reword.md) | Change any commit's message and restack its descendants |
| [`uncommit`](uncommit.md) | Undo the last commit(s), keeping the changes staged |
| [`squash`](squash.md) | Fold a commit into its parent (default HEAD), or a whole range with `--from` |
| [`move`](move.md) | Move a commit before or after another in the current branch's history |
| [`split`](split.md) | Split a commit into two by path |
| [`absorb`](absorb.md) | Fold each pending change into the stacked commit that last touched those lines |
| [`cherry-pick`](cherry-pick.md) | Cherry-pick commits onto HEAD, or continue/skip/abort a stopped one |
| [`revert`](revert.md) | Revert commits on HEAD, or continue/skip/abort a stopped revert |
| [`reset`](reset.md) | Reset HEAD to a revision |

## Undo

| Command | What it does |
| --- | --- |
| [`undo`](undo.md) | Undo the last operation from the op-log, restoring HEAD and the working tree |
| [`redo`](redo.md) | Redo the operation most recently undone |
| [`oplog`](oplog.md) | Show the operation log (the undo stack), newest first |

## Branches, tags, stashes

| Command | What it does |
| --- | --- |
| [`branch`](branch.md) | Branch management: no subcommand lists local branches, `branch &lt;name&gt; [&lt;start&gt;]` creates one without switching to it, and git's flags work as in git |
| [`checkout`](checkout.md) | Check out a branch or, for any other revision, a detached HEAD |
| [`switch`](switch.md) | Switch branches: `switch &lt;branch&gt;`, `-c &lt;new&gt; [&lt;start&gt;]`, `--detach &lt;rev&gt;`, or `-` for the previous branch |
| [`merge`](merge.md) | Merge revisions into the current branch |
| [`rebase`](rebase.md) | Rebase onto a revision, or continue/skip/abort an in-progress rebase |
| [`tag`](tag.md) | Tag management: `tag` lists, `tag &lt;name&gt; [&lt;rev&gt;]` creates, `tag -d &lt;name&gt;...` deletes, `tag -l &lt;pattern&gt;...` lists matching tags |
| [`stash`](stash.md) | Stash management |
| [`bisect`](bisect.md) | Find the commit that introduced a change by binary search: `start &lt;bad&gt; &lt;good&gt;`, then mark each step `good`/`bad` (or `run &lt;cmd&gt;`), then `reset` |
| [`rerere`](rerere.md) | Reuse recorded conflict resolutions (`git rerere`, in git's .git/rr-cache): no argument records and replays, or `status`, `remaining`, `diff`, `forget &lt;paths&gt;`, `clear` or `gc` |
| [`prune`](prune.md) | Prune unreachable objects |

## Stacks, lanes, workspaces, worktrees

| Command | What it does |
| --- | --- |
| [`stack`](stack.md) | Stacked branches: chain branches and restack descendants after edits |
| [`next`](next.md) | Check out the branch stacked on this one |
| [`prev`](prev.md) | Check out this branch's stack parent |
| [`sync`](sync.md) | Fetch, fast-forward branches to their upstreams, and restack the stack |
| [`submit`](submit.md) | Push every branch in the stack and open a pull request per branch |
| [`lanes`](lanes.md) | Lanes: several lines of work in one worktree |
| [`workspace`](workspace.md) | Copy-on-write workspaces: instant, isolated, block-sharing clones of the whole repo (code, build, .git) for parallel work |
| [`worktree`](worktree.md) | Worktree management |
| [`flow`](flow.md) | Branching workflows: pick a preset |

## Remotes and forge (GitHub/GitLab)

| Command | What it does |
| --- | --- |
| [`fetch`](fetch.md) | Fetch the current branch's remote, or `&lt;repository&gt; [&lt;refspec&gt;...]` |
| [`pull`](pull.md) | Fetch and integrate the current branch's upstream |
| [`push`](push.md) | Push the current branch to its upstream, or `&lt;repository&gt; [&lt;refspec&gt;...]` |
| [`ls-remote`](ls-remote.md) | List the refs in a remote repository, like `git ls-remote` |
| [`remote`](remote.md) | Remote management |
| [`forge`](forge.md) | Manage GitHub repositories, branches, and pull requests with the native API |

## Patches, notes, config and maintenance

| Command | What it does |
| --- | --- |
| [`format-patch`](format-patch.md) | Write commits as mbox patch files |
| [`am`](am.md) | Apply mbox patches (from format-patch) as commits |
| [`apply`](apply.md) | Apply a patch to the working tree, the index, or both |
| [`archive`](archive.md) | Write a tar or zip of a revision's files |
| [`notes`](notes.md) | Notes attached to commits |
| [`config`](config.md) | Get, set, unset or list config values: `config &lt;key&gt;` reads, `config &lt;key&gt; &lt;value&gt;` writes |
| [`update-ref`](update-ref.md) | Point a ref at a commit, or delete it, optionally only if it holds an expected value |
| [`hash-object`](hash-object.md) | Print the object id of files or stdin |
| [`gc`](gc.md) | Pack the object database and prune unreachable objects |
| [`fsck`](fsck.md) | Check the object database for corruption and dangling objects |
| [`verify-commit`](verify-commit.md) | Check commits' gpg, x509 or ssh signatures |
| [`verify-tag`](verify-tag.md) | Check annotated tags' gpg, x509 or ssh signatures |
| [`repack`](repack.md) | Pack the repository's objects |
| [`pack-refs`](pack-refs.md) | Move loose refs into packed-refs |
| [`commit-graph`](commit-graph.md) | Write or check the commit-graph file or chain |
| [`multi-pack-index`](multi-pack-index.md) | Write, check, expire or repack the multi-pack-index |
| [`maintenance`](maintenance.md) | Background upkeep (`git maintenance`): run tasks now, schedule them, or (un)register this repository |
| [`for-each-repo`](for-each-repo.md) | Run an rgit command in every repository a multi-valued config key lists (`git for-each-repo`), e.g |
| [`scalar`](scalar.md) | Large-repository setup and upkeep (git's `scalar`): recommended config, background maintenance and the registered repo list |
| [`credential`](credential.md) | Get, save or drop credentials through the configured helpers (`git credential`), with git's `key=value` lines on stdin and stdout |
| [`credential-store`](credential-store.md) | Keep credentials in a plain-text file (`git credential-store`), the `store` credential helper |
| [`credential-cache`](credential-cache.md) | Keep credentials in memory for a while (`git credential-cache`), the `cache` credential helper |
| [`credential-cache--daemon`](credential-cache--daemon.md) | The daemon behind `credential-cache` |
| [`hook`](hook.md) | Run a repository hook as git would |
| [`cherry`](cherry.md) | Commits not yet upstream (`git cherry`): `+ &lt;id&gt;` for each, `- &lt;id&gt;` when upstream already has an equivalent change |
| [`bundle`](bundle.md) | Move history as one file (`git bundle`): create, verify, list-heads, unbundle |
| [`request-pull`](request-pull.md) | Summarize changes for a pull request by mail (`git request-pull`): what `url` holds beyond `start` |
| [`range-diff`](range-diff.md) | Compare two versions of a series (`git range-diff`): `&lt;base&gt; &lt;old&gt; &lt;new&gt;`, `&lt;old-range&gt; &lt;new-range&gt;` or `&lt;old&gt;...&lt;new&gt;` |
| [`difftool`](difftool.md) | Show changes in the configured diff tool (`git difftool`): diff.tool, difftool.&lt;tool&gt;.cmd or a known tool |
| [`mergetool`](mergetool.md) | Resolve conflicts in the configured merge tool (`git mergetool`): merge.tool, mergetool.&lt;tool&gt;.cmd or a known tool |

## Plumbing (git's own output formats)

| Command | What it does |
| --- | --- |
| [`rev-parse`](rev-parse.md) | Resolve revisions to object ids, or print repository paths, like `git rev-parse` |
| [`ls-files`](ls-files.md) | List files in the index and the working tree, like `git ls-files` |
| [`ls-tree`](ls-tree.md) | List a tree's entries, like `git ls-tree` |
| [`cat-file`](cat-file.md) | Print an object's type, size or content, like `git cat-file` |
| [`show-ref`](show-ref.md) | List refs with their object ids, like `git show-ref` |
| [`for-each-ref`](for-each-ref.md) | List refs in a custom format, like `git for-each-ref` |
| [`rev-list`](rev-list.md) | List commit ids reachable from revisions, like `git rev-list` |
| [`merge-base`](merge-base.md) | Find the common ancestor of two commits, like `git merge-base` |
| [`reflog`](reflog.md) | Show where a ref pointed over time (`git reflog [show] [REF]`), or `expire [--expire=&lt;date&gt;] [--expire-unreachable=&lt;date&gt;] [--all] [--rewrite] [--updateref] [--stale-fix] [-n] [--verbose] [REF...]`, `delete [--rewrite] [--updateref] [-n] REF@{N}...`, `exists REF` |
| [`shortlog`](shortlog.md) | Summarize commits by author, like `git shortlog` |
| [`grep`](grep.md) | Search tracked files, the index or a revision, like `git grep` |
| [`check-ignore`](check-ignore.md) | Show which paths are ignored and by which rule, like `git check-ignore` |
| [`var`](var.md) | Print a git variable: GIT_AUTHOR_IDENT, GIT_COMMITTER_IDENT, GIT_EDITOR, GIT_SEQUENCE_EDITOR, GIT_PAGER, GIT_DEFAULT_BRANCH, GIT_SHELL_PATH, GIT_ATTR_SYSTEM, GIT_ATTR_GLOBAL, GIT_CONFIG_SYSTEM or GIT_CONFIG_GLOBAL, like `git var` |
| [`symbolic-ref`](symbolic-ref.md) | Print where a symbolic ref points (`HEAD` -&gt; `refs/heads/main`), point it elsewhere, or delete it, like `git symbolic-ref` |
| [`count-objects`](count-objects.md) | Count loose and packed objects, like `git count-objects` |
| [`name-rev`](name-rev.md) | Name commits by the refs that reach them (`main~2`, `tags/v1^0`), like `git name-rev` |
| [`check-attr`](check-attr.md) | Print the gitattributes of paths, like `git check-attr`: `ATTR PATH...`, `ATTR |
| [`check-ref-format`](check-ref-format.md) | Check that a ref name is valid, like `git check-ref-format` |
| [`patch-id`](patch-id.md) | Print the patch id of each patch read from stdin (`git log -p`, `format-patch` or `diff` output), like `git patch-id` |
| [`stripspace`](stripspace.md) | Clean up text read from stdin as git cleans a commit message, like `git stripspace` |
| [`column`](column.md) | Lay out lines read from stdin in columns, like `git column` |
| [`diff-tree`](diff-tree.md) | Compare two trees, or a commit with its parent, like `git diff-tree` |
| [`diff-index`](diff-index.md) | Compare a tree with the working tree or the index, like `git diff-index` |
| [`diff-files`](diff-files.md) | Compare the index with the working tree, like `git diff-files` |
| [`merge-tree`](merge-tree.md) | Merge two commits without touching the index or working tree, like `git merge-tree --write-tree`: prints the merged tree, conflicted files and messages |
| [`merge-file`](merge-file.md) | Three-way merge of files into the first, like `git merge-file` |
| [`fast-export`](fast-export.md) | Print history as a fast-import stream, like `git fast-export`: revisions and ranges (`--all`, `A..B`, `^A`), `--signed-tags=`, `--signed-commits=`, `--tag-of-filtered-object=`, `--reencode=`, `--export-marks=`, `--import-marks[-if-exists]=`, `--no-data`, `--full-tree`, `--use-done-feature`, `--fake-missing-tagger`, `--refspec`, `--reference-excluded-parents`, `--show-original-ids`, `--mark-tags`, then `-- &lt;paths&gt;` |
| [`fast-import`](fast-import.md) | Read a fast-import stream on stdin and write its objects and refs, like `git fast-import` |
| [`replay`](replay.md) | Replay commits onto a new base without touching the working tree, like `git replay`: updates the refs (git's default since 2.53), or with --ref-action=print emits `update &lt;ref&gt; &lt;new&gt; &lt;old&gt;` lines for `rgit update-ref --stdin` |
| [`commit-tree`](commit-tree.md) | Write a commit object for a tree and print its id, like `git commit-tree` |
| [`write-tree`](write-tree.md) | Write the index as a tree and print its id, like `git write-tree` |
| [`read-tree`](read-tree.md) | Read trees into the index, like `git read-tree`: one tree replaces it, -m merges one, two (switch) or three (base, ours, theirs) trees |
| [`update-index`](update-index.md) | Change index entries, like `git update-index`: options apply to the paths after them |
| [`checkout-index`](checkout-index.md) | Copy files from the index to the working tree, like `git checkout-index` |
| [`mktree`](mktree.md) | Build a tree from `ls-tree` lines on stdin and print its id, like `git mktree` |
| [`mktag`](mktag.md) | Check a tag object on stdin and store it, like `git mktag` |
| [`get-tar-commit-id`](get-tar-commit-id.md) | Print the commit id `git archive` stored in the tar read from stdin, like `git get-tar-commit-id` |
| [`fmt-merge-msg`](fmt-merge-msg.md) | Write a merge commit's message from FETCH_HEAD-style lines on stdin, like `git fmt-merge-msg` |
| [`mailsplit`](mailsplit.md) | Split mboxes or Maildirs into one numbered file per mail, like `git mailsplit` |
| [`mailinfo`](mailinfo.md) | Read one mail from stdin, write its message and patch to MSG and PATCH, and print its author, subject and date, like `git mailinfo` |
| [`interpret-trailers`](interpret-trailers.md) | Add or parse trailers (`Signed-off-by: ...`) in a commit message read from files or stdin, like `git interpret-trailers` |
| [`show-branch`](show-branch.md) | Show branches side by side with the commits each has, down to where they meet, like `git show-branch` |
| [`fetch-pack`](fetch-pack.md) | Fetch refs' objects from another repository (a path or file:// URL) without updating any ref, printing `&lt;id&gt; &lt;ref&gt;` for each, like `git fetch-pack` |
| [`send-pack`](send-pack.md) | Push refs to another repository (a path or file:// URL) with no remote config, like `git send-pack` |
| [`diff-pairs`](diff-pairs.md) | Diff the pairs of git's raw diff format on stdin (NUL-separated, as `diff-tree -r -z` prints them), like `git diff-pairs -z` |

## Object replacement, packs and diagnostics

| Command | What it does |
| --- | --- |
| [`replace`](replace.md) | Stand one object in for another with refs under refs/replace/, like `git replace`: `&lt;object&gt; &lt;replacement&gt;`, `--graft`, `--edit`, `-d` or a listing |
| [`check-mailmap`](check-mailmap.md) | Show each contact (`Name &lt;email&gt;` or `&lt;email&gt;`) as the mailmap maps it, like `git check-mailmap` |
| [`verify-pack`](verify-pack.md) | Check packs against their indexes, like `git verify-pack` |
| [`show-index`](show-index.md) | List a pack index read from stdin, like `git show-index`: `&lt;offset&gt; &lt;id&gt; (&lt;crc32&gt;)` per object |
| [`index-pack`](index-pack.md) | Build the index for a pack file, like `git index-pack` |
| [`unpack-objects`](unpack-objects.md) | Write each object of a pack read from stdin as a loose object, like `git unpack-objects` |
| [`pack-objects`](pack-objects.md) | Pack the objects listed on stdin, or reachable from the revisions on stdin with `--revs`, like `git pack-objects` |
| [`prune-packed`](prune-packed.md) | Remove loose objects that packs also hold, like `git prune-packed` |
| [`update-server-info`](update-server-info.md) | Write info/refs and objects/info/packs for dumb transports, like `git update-server-info` |
| [`unpack-file`](unpack-file.md) | Write a blob to a temporary `.merge_file_XXXXXX` file and print its name, like `git unpack-file` |
| [`merge-one-file`](merge-one-file.md) | Resolve one unmerged path from its three stages, like the `git merge-one-file` script: `&lt;orig&gt; &lt;ours&gt; &lt;theirs&gt; &lt;path&gt; &lt;orig mode&gt; &lt;our mode&gt; &lt;their mode&gt;`, empty for a missing side |
| [`merge-index`](merge-index.md) | Run a merge program on each unmerged path, like `git merge-index [-o] [-q] &lt;program&gt; (-a \| [--] &lt;path&gt;...)` |
| [`bugreport`](bugreport.md) | Write a bug report template with system details and open it in the editor, like `git bugreport` |
| [`diagnose`](diagnose.md) | Zip up repository statistics for a bug report, like `git diagnose` |
| [`backfill`](backfill.md) | Fetch the missing objects of a partial clone, like `git backfill` |

## Code search

| Command | What it does |
| --- | --- |
| [`index`](index-command.md) | Semantic code search: build the on-disk vector index or query it |

## Repositories and escape hatch

| Command | What it does |
| --- | --- |
| [`init`](init.md) | Create a new repository in the current directory |
| [`clone`](clone.md) | Clone a repository into a new directory |
| [`submodule`](submodule.md) | Submodules: status (the default), add, init, update, sync, deinit, foreach, summary, set-url, set-branch and absorbgitdirs |
| [`git`](git.md) | Run a git subcommand that rgit does not have, such as `lfs` or `filter-repo` |

## Agent integration and servers

| Command | What it does |
| --- | --- |
| [`agent`](agent.md) | Set agent apps (Claude Code, Codex, OpenCode, pi, omp) up for rgit |
| [`completion`](completion.md) | Print a shell completion script for rgit's commands and flags |
| [`mcp`](mcp.md) | Run the Model Context Protocol server over stdio |
| [`tool`](tool.md) | Run one MCP tool with its arguments as a JSON object on stdin and print its TOON result, as the pi and omp extension does |
| [`serve`](serve.md) | Serve the web viewer for this repository, or a directory of repositories |
| [`settings`](settings.md) | Show rgit's own settings, each with its value and where it came from |
