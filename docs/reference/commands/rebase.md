# rgit rebase

Rebase onto a revision, or continue/skip/abort an in-progress rebase

## Usage

```sh
rgit rebase [OPTIONS] [ONTO] [BRANCH]
```

## Examples

```sh
rgit rebase main
rgit rebase --onto <newbase> <upstream>
rgit rebase --autosquash main
rgit rebase --exec "cargo test" main
rgit rebase --root
rgit rebase -r --committer-date-is-author-date main
rgit rebase --update-refs --autostash main
GIT_SEQUENCE_EDITOR="sed -i.bak 1s/^pick/edit/" rgit rebase -i main
rgit rebase --continue
rgit rebase --abort
rgit rebase --show-current-patch
```

## Arguments

| Argument | Description |
| --- | --- |
| `[ONTO]` | The upstream to rebase onto (prompted for if omitted). With --onto, this is the upstream whose commits after it are replayed |
| `[BRANCH]` | Check out this branch first and rebase it (git's `&lt;upstream&gt; &lt;branch&gt;`) |

## Options

| Option | Value | Description |
| --- | --- | --- |
| `--onto` | `NEWBASE` | Replay commits after &lt;upstream&gt; onto this new base (git's --onto) |
| `-i, --interactive` |  | Interactive rebase: opens the todo editor (needs a terminal, or GIT_SEQUENCE_EDITOR set to a script) |
| `--root` |  | Rebase every commit down to the root commit |
| `--autosquash` |  | Move `fixup!`/`squash!` commits after their targets and fold them in |
| `-x, --exec` | `CMD` | Run this shell command after each rebased commit (repeatable) |
| `--update-refs` |  | Also move branches that point into the rebased commits |
| `-X, --strategy-option` | `STRATEGY_OPTION` | Take this side on conflicting hunks (git's -X) |
| `--keep-empty` |  | Keep commits that start out empty |
| `--no-autosquash` |  | Do not move `fixup!`/`squash!` commits (overrides rebase.autoSquash) |
| `-f, --force-rebase` |  | Replay every commit, even ones that could be kept as they are |
| `--fork-point` |  | Refine the upstream with its reflog (git's merge-base --fork-point) |
| `--no-fork-point` |  | Use the upstream as it is, without its reflog |
| `--keep-base` |  | Keep the base: replay onto the merge base of the upstream and the branch |
| `--committer-date-is-author-date` |  | Give each commit its author date as the committer date |
| `--reset-author-date` |  | Give each commit the current time as its author date |
| `-r, --rebase-merges` | `MODE` | Recreate merge commits instead of flattening them |
| `--empty` | `EMPTY` | What to do with a commit that becomes empty |
| `--reapply-cherry-picks` |  | Apply every commit, even ones already upstream |
| `--signoff` |  | Add a Signed-off-by trailer to each commit |
| `--autostash` |  | Stash local changes first and restore them after |
| `--no-verify` |  | Skip the pre-rebase hook |
| `-q, --quiet` |  | Print nothing on success |
| `-v, --verbose` |  | Show a diffstat of what changed upstream |
| `-S, --gpg-sign` | `KEYID` | Sign the commit with gpg, gpgsm or ssh-keygen (per gpg.format), with user.signingKey or `-S&lt;KEYID&gt;` (git's -S) |
| `--no-gpg-sign` |  | Do not sign, even with commit.gpgSign set |
| `--rerere-autoupdate` |  | Stage the files rerere resolved with a recorded resolution |
| `--no-rerere-autoupdate` |  | Leave them unstaged, whatever rerere.autoUpdate says |
| `--continue` |  | Continue after resolving conflicts |
| `--skip` |  | Skip the current commit |
| `--abort` |  | Abort the in-progress rebase |
| `--quit` |  | Stop the rebase, leaving HEAD, the index and the working tree as they are |
| `--edit-todo` |  | Edit the todo list of the in-progress rebase (needs a terminal, or GIT_SEQUENCE_EDITOR set to a script) |
| `--show-current-patch` |  | Show the commit the rebase stopped at |

See the [global options](global-options.md) that every command accepts.
