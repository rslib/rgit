# rgit pull

Fetch and integrate the current branch's upstream (merges when it has diverged, unless `pull.rebase` says otherwise)

## Usage

```sh
rgit pull [OPTIONS] [REPOSITORY] [BRANCH]
```

## Examples

```sh
rgit pull
rgit pull --rebase
rgit pull --ff-only
rgit pull origin main
rgit pull --autostash --rebase
rgit pull --squash origin feature
```

## Arguments

| Argument | Description |
| --- | --- |
| `[REPOSITORY]` | The remote, or a path or URL, to pull from (defaults to the upstream's remote) |
| `[BRANCH]` | The remote branch to integrate (defaults to the upstream branch) |

## Options

| Option | Value | Description |
| --- | --- | --- |
| `-r, --rebase` |  | Rebase local commits onto the upstream instead of merging |
| `--no-rebase` |  | Merge even if `pull.rebase` is set (git's --no-rebase) |
| `--ff-only` |  | Refuse unless the upstream fast-forwards the branch (git's --ff-only) |
| `--no-ff` |  | Always make a merge commit, even for a fast-forward |
| `--squash` |  | Stage the upstream's changes without committing or recording a merge |
| `--no-commit` |  | Merge but stop before making the merge commit |
| `--autostash` |  | Stash local changes before the pull and restore them after (default: `rebase.autoStash` / `merge.autoStash`) |
| `--no-autostash` |  | Do not stash local changes around the pull |
| `-X, --strategy-option` | `OPTION` | On conflicting hunks take `ours` or `theirs` (merge only) |
| `--all` |  | Fetch every remote first |
| `--depth` | `N` | Limit the fetched history to N commits (default: `0`) |
| `-q, --quiet` |  | Print nothing unless an error occurs |
| `-v, --verbose` |  | Report more (accepted for git compatibility) |

See the [global options](global-options.md) that every command accepts.
