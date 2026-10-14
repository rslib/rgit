# rgit merge

Merge revisions into the current branch (several make an octopus merge)

## Usage

```sh
rgit merge [OPTIONS] [REVS]...
```

## Examples

```sh
rgit merge <branch>
rgit merge <branch> --no-ff
rgit merge <branch> --squash
rgit merge <branch> -m "<message>"
rgit merge <a> <b>
rgit merge <a> <b> -e
rgit merge <branch> --autostash
rgit merge <branch> --into-name <other>
rgit merge <branch> -s ours
rgit merge <branch> --log --no-stat
rgit merge <branch> --allow-unrelated-histories
rgit merge --continue
rgit merge --abort
```

## Arguments

| Argument | Description |
| --- | --- |
| `[REVS]...` | The branches or revisions to merge (prompted for if omitted) |

## Options

| Option | Value | Description |
| --- | --- | --- |
| `-S, --gpg-sign` | `KEYID` | Sign the commit with gpg, gpgsm or ssh-keygen (per gpg.format), with user.signingKey or `-S&lt;KEYID&gt;` (git's -S) |
| `--no-gpg-sign` |  | Do not sign, even with commit.gpgSign set |
| `--rerere-autoupdate` |  | Stage the files rerere resolved with a recorded resolution |
| `--no-rerere-autoupdate` |  | Leave them unstaged, whatever rerere.autoUpdate says |
| `--no-ff` |  | Always create a merge commit, even if a fast-forward is possible |
| `--ff-only` |  | Refuse to merge unless it can fast-forward (git's --ff-only) |
| `--squash` |  | Stage the merged changes as one ordinary change, without a merge commit |
| `--no-commit` |  | Merge but stop before committing; finish with `merge --continue` |
| `-m, --message` | `MESSAGE` | The merge commit message |
| `-F, --file` | `PATH` | Read the merge commit message from a file (`-` for stdin) |
| `-X, --strategy-option` | `STRATEGY_OPTION` | Take this side on conflicting hunks (git's -X) |
| `-s, --strategy` | `STRATEGY` | The merge strategy; `ours` records the merge but keeps HEAD's tree |
| `--allow-unrelated-histories` |  | Allow merging histories that share no commit |
| `--log` | `N` | Add the merged commits' subjects (at most N, default 20) to the message |
| `--stat` |  | Show a diffstat at the end (the default) |
| `-n, --no-stat` |  | Do not show a diffstat at the end |
| `-e, --edit` |  | Open the editor on the merge commit message |
| `--no-verify` |  | Skip the pre-merge-commit and commit-msg hooks |
| `--signoff` |  | Add a Signed-off-by trailer |
| `--autostash` |  | Stash local changes first and reapply them after (default: merge.autoStash) |
| `--no-autostash` |  | Do not stash local changes first (overrides merge.autoStash) |
| `--into-name` | `BRANCH` | Word the default message as merging into this branch |
| `--cleanup` | `MODE` | How to clean up the message: strip, whitespace, verbatim, scissors or default |
| `-q, --quiet` |  | Print nothing on success |
| `--continue` |  | Commit a merge whose conflicts are resolved |
| `--abort` |  | Abort an in-progress (conflicted) merge, restoring HEAD |
| `--quit` |  | Forget an in-progress merge, leaving the index and working tree as they are |

See the [global options](global-options.md) that every command accepts.
