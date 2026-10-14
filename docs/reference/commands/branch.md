# rgit branch

Branch management: no subcommand lists local branches, `branch <name> [<start>]` creates one without switching to it, and git's flags work as in git

## Usage

```sh
rgit branch [OPTIONS] [ARGS]...
       rgit branch <COMMAND>
```

## Examples

```sh
rgit branch
rgit branch -vv
rgit branch --merged main
rgit branch --sort=-committerdate --format='%(refname:short) %(upstream:short)'
rgit branch <branch> <start>
rgit branch -c <branch> <new_branch>
rgit branch -d <branch> <branch>
rgit branch -u origin/<branch>
rgit branch --column
rgit branch -v --abbrev=12 --color=always
```

## Arguments

| Argument | Description |
| --- | --- |
| `[ARGS]...` | A branch to create and its start point (default HEAD); the branches to delete, rename, copy or track; or with -l, patterns to list |

## Options

| Option | Value | Description |
| --- | --- | --- |
| `-a, --all` |  | List remote-tracking branches too (git's -a) |
| `-r, --remotes` |  | List only remote-tracking branches (git's -r) |
| `-l, --list` |  | List branches, only those matching the given patterns (git's -l) |
| `-v, --verbose` |  | Show each branch's commit and subject; twice adds its upstream (-vv) |
| `--merged` | `REV` | List only branches merged into this commit (default HEAD) |
| `--no-merged` | `REV` | List only branches not merged into this commit (default HEAD) |
| `--contains` | `REV` | List only branches that contain this commit (default HEAD) |
| `--no-contains` | `REV` | List only branches that do not contain this commit (default HEAD) |
| `--points-at` | `REV` | List only branches that point at this commit (default HEAD) |
| `--sort` | `KEY` | Sort by a for-each-ref key (`-committerdate`, `version:refname`); the last --sort is the main key (default refname, or `branch.sort`) |
| `--format` | `FORMAT` | Print each branch in a for-each-ref format, e.g. `%(refname:short)` |
| `-i, --ignore-case` |  | Match patterns and sort case-insensitively (git's -i) |
| `--column` | `OPTIONS` | Lay the list out in columns: always, never, auto, column, row, plain, dense, nodense (default `column.branch`, `column.ui`) |
| `--no-column` |  | List one branch per line |
| `--abbrev` | `N` | With -v, show object names in at least N hex digits |
| `--no-abbrev` |  | With -v, show full object names |
| `--omit-empty` |  | With --format, print no newline for a branch that formats empty |
| `--create-reflog` |  | Keep a reflog for the new branch |
| `--recurse-submodules` |  | Accepted for git compatibility (submodules are left alone) |
| `-t, --track` | `direct\|inherit` | A new branch tracks its start point, or with `=inherit` the start point's own upstream (git's -t) |
| `--no-track` |  | A new branch tracks nothing, even when started from a remote branch |
| `--edit-description` |  | Edit `[&lt;branch&gt;]`'s (default current) description in the editor |
| `-q, --quiet` |  | Accepted for git compatibility |
| `--show-current` |  | Print the current branch's name (nothing when detached) |
| `-d, --delete` |  | Delete the named branches if merged into HEAD (git's -d) |
| `-D` |  | Delete the named branches even if not merged (git's -D) |
| `-m, --move` |  | Rename `[&lt;old&gt;] &lt;new&gt;`, the current branch by default (git's -m) |
| `-M` |  | Rename, replacing an existing branch named `&lt;new&gt;` (git's -M) |
| `-c, --copy` |  | Copy `[&lt;old&gt;] &lt;new&gt;`, the current branch by default (git's -c) |
| `-C` |  | Copy, replacing an existing branch named `&lt;new&gt;` (git's -C) |
| `-u, --set-upstream-to` | `UPSTREAM` | Make `[&lt;branch&gt;]` (default current) track this upstream (git's -u) |
| `--unset-upstream` |  | Stop `[&lt;branch&gt;]` (default current) tracking its upstream |
| `-f, --force` |  | Create even if the branch exists, moving it; delete even if unmerged |

## Subcommands

### rgit branch create

Create a branch (at HEAD, or START) and switch to it

#### Usage

```sh
rgit branch create [OPTIONS] <NAME> [START]
```

#### Examples

```sh
rgit branch create <branch>
rgit branch create <branch> origin/<branch>
```

#### Arguments

| Argument | Description |
| --- | --- |
| `<NAME>` | The new branch name |
| `[START]` | Where the branch starts (default HEAD) |

### rgit branch checkout

Check out an existing branch

#### Usage

```sh
rgit branch checkout [OPTIONS] <NAME>
```

#### Examples

```sh
rgit branch checkout <branch>
```

#### Arguments

| Argument | Description |
| --- | --- |
| `<NAME>` | The branch to check out |

### rgit branch delete

Delete branches (multiselect prompt if no name on a terminal)

#### Usage

```sh
rgit branch delete [OPTIONS] [NAMES]...
```

#### Examples

```sh
rgit branch delete <branch> <branch>
rgit branch delete <branch> --force
```

#### Arguments

| Argument | Description |
| --- | --- |
| `[NAMES]...` | The branches to delete |

#### Options

| Option | Value | Description |
| --- | --- | --- |
| `-D, --force` |  | Delete even if not fully merged (git's -D) |

### rgit branch rename

Rename a branch

#### Usage

```sh
rgit branch rename [OPTIONS] <OLD> <NEW>
```

#### Examples

```sh
rgit branch rename <old> <new>
```

#### Arguments

| Argument | Description |
| --- | --- |
| `<OLD>` | The current branch name |
| `<NEW>` | The new branch name |

### rgit branch prune

Delete every local branch already merged into a base (default HEAD)

#### Usage

```sh
rgit branch prune [OPTIONS] [BASE]
```

#### Examples

```sh
rgit branch prune
rgit branch prune main
```

#### Arguments

| Argument | Description |
| --- | --- |
| `[BASE]` | Delete branches merged into this revision |

See the [global options](global-options.md) that every command accepts.
