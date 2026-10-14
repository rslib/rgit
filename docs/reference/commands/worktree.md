# rgit worktree

Worktree management (no subcommand lists worktrees)

## Usage

```sh
rgit worktree [OPTIONS] [COMMAND]
```

## Examples

```sh
rgit worktree
```

## Subcommands

### rgit worktree add

Add a linked worktree at PATH, as `git worktree add` does: on BRANCH, on a new branch with -b, detached at a commit or with --detach, or by default on a branch named after PATH's last folder

#### Usage

```sh
rgit worktree add [OPTIONS] <PATH> [BRANCH]
```

#### Examples

```sh
rgit worktree add ../<path> <branch>
rgit worktree add -b <new_branch> ../<path> origin/main
rgit worktree add --detach ../<path> <rev>
rgit worktree add -B <branch> --lock --reason "<reason>" ../<path>
rgit worktree add --orphan -b <new_branch> ../<path>
rgit worktree add --guess-remote ../<branch>
```

#### Arguments

| Argument | Description |
| --- | --- |
| `<PATH>` | The path for the new worktree |
| `[BRANCH]` | The branch to check out there, or the commit to start from |

#### Options

| Option | Value | Description |
| --- | --- | --- |
| `-b` | `NEW_BRANCH` | Create this branch at BRANCH (default HEAD) and check it out there |
| `-B` | `NEW_BRANCH` | Like -b, but reset the branch to BRANCH if it exists |
| `-d, --detach` |  | Check out a detached HEAD (git's --detach) |
| `--orphan` |  | Start an empty, unborn branch (-b's, or named after PATH) |
| `-f, --force` |  | Check out the branch even if another worktree has it |
| `--no-checkout` |  | Leave the new worktree's files and index empty |
| `--lock` |  | Lock the new worktree (see `worktree lock`) |
| `--reason` | `REASON` | Why it is locked (with --lock) |
| `--track` |  | A new branch tracks BRANCH |
| `--no-track` |  | A new branch tracks nothing, even when BRANCH is remote |
| `--guess-remote` |  | With no BRANCH, start the new branch from the one remote branch named after PATH, tracking it (default `worktree.guessRemote`) |
| `--no-guess-remote` |  | Start the new branch from HEAD even when a remote has its name |
| `-q, --quiet` |  | Accepted for git compatibility |

### rgit worktree list

List the worktrees, as `git worktree list` prints them

#### Usage

```sh
rgit worktree list [OPTIONS]
```

#### Examples

```sh
rgit worktree list -v
rgit worktree list --porcelain
```

#### Options

| Option | Value | Description |
| --- | --- | --- |
| `--porcelain` |  | git's stable format, one `key value` line per field |
| `-z` |  | With --porcelain, end lines with NUL |
| `-v, --verbose` |  | Show lock and prune reasons |

### rgit worktree repair

Fix the links between worktrees and the repository after moving either; name worktrees moved by hand

#### Usage

```sh
rgit worktree repair [OPTIONS] [PATHS]...
```

#### Examples

```sh
rgit worktree repair
rgit worktree repair ../<path>
```

#### Arguments

| Argument | Description |
| --- | --- |
| `[PATHS]...` | Worktrees now at these paths |

### rgit worktree remove

Remove a linked worktree (by name or path) and its folder

#### Usage

```sh
rgit worktree remove [OPTIONS] <NAME>
```

#### Examples

```sh
rgit worktree remove ../<path>
rgit worktree remove <name> --force
```

#### Arguments

| Argument | Description |
| --- | --- |
| `<NAME>` | The worktree's name or path |

#### Options

| Option | Value | Description |
| --- | --- | --- |
| `-f, --force` |  | Remove even with changes or when locked (git's -f/--force) |

### rgit worktree lock

Lock a worktree (by name or path) so prune leaves it alone

#### Usage

```sh
rgit worktree lock [OPTIONS] <NAME>
```

#### Examples

```sh
rgit worktree lock ../<path> --reason "<reason>"
```

#### Arguments

| Argument | Description |
| --- | --- |
| `<NAME>` | The worktree's name or path |

#### Options

| Option | Value | Description |
| --- | --- | --- |
| `--reason` | `REASON` | Why it is locked |

### rgit worktree unlock

Unlock a worktree (by name or path)

#### Usage

```sh
rgit worktree unlock [OPTIONS] <NAME>
```

#### Examples

```sh
rgit worktree unlock ../<path>
```

#### Arguments

| Argument | Description |
| --- | --- |
| `<NAME>` | The worktree's name or path |

### rgit worktree move

Move a worktree (by name or path) to a new path

#### Usage

```sh
rgit worktree move [OPTIONS] <NAME> <NEW_PATH>
```

#### Examples

```sh
rgit worktree move ../<path> ../<new_path>
```

#### Arguments

| Argument | Description |
| --- | --- |
| `<NAME>` | The worktree's name or path |
| `<NEW_PATH>` | The new path |

### rgit worktree prune

Prune worktree entries whose working tree is gone (git's `worktree prune`)

#### Usage

```sh
rgit worktree prune [OPTIONS]
```

#### Examples

```sh
rgit worktree prune
```

#### Options

| Option | Value | Description |
| --- | --- | --- |
| `-n, --dry-run` |  | Only list what would be pruned (git's -n) |
| `-v, --verbose` |  | Accepted for git compatibility; the pruned names are printed |

See the [global options](global-options.md) that every command accepts.
