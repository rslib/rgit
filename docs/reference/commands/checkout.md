# rgit checkout

Check out a branch or, for any other revision, a detached HEAD; or restore paths from a revision or the index

## Usage

```sh
rgit checkout [OPTIONS] [REV] [PATH]... [-- <PATH>...]
```

## Examples

```sh
rgit checkout main
rgit checkout -b <new_branch>
rgit checkout -
rgit checkout <rev> -- src/lib.rs
rgit checkout -f <branch>
rgit checkout --theirs -- src/lib.rs
rgit checkout --conflict=diff3 -- src/lib.rs
rgit checkout -p <rev>
```

## Arguments

| Argument | Description |
| --- | --- |
| `[REV]` | The branch or revision, `-` for the previous one (prompted for if omitted on a terminal). A path here restores it from the index |
| `[PATH]...` | Paths to restore from `rev` (index and working tree), or from the index when no revision is given |
| `[PATH]...` | Paths to restore (after `--`) |

## Options

| Option | Value | Description |
| --- | --- | --- |
| `-b` | `NEW_BRANCH` | Create a new branch and switch to it (git's -b), from `rev` or HEAD |
| `-B` | `BRANCH` | Create or reset a branch and switch to it (git's -B) |
| `--detach` |  | Detach HEAD at `rev` (default HEAD), even when it is a branch |
| `-t, --track` |  | Track `rev` as the new branch's upstream; without -b, create a local branch named after the remote one |
| `--no-track` |  | Never set an upstream for the new branch |
| `-f, --force` |  | Throw away local changes when switching (git's -f) |
| `-m, --merge` |  | Carry local changes over to the new branch with a three-way merge |
| `--conflict` | `STYLE` | Conflict marker style for -m: `merge` or `diff3` |
| `--orphan` | `NEW_BRANCH` | Start a new branch with no history at `rev` (default HEAD) |
| `--ours` |  | For conflicted paths, write our side to the working tree |
| `--theirs` |  | For conflicted paths, write their side to the working tree |
| `--no-guess` |  | Do not turn a remote branch's name into a local tracking branch |
| `-p, --patch` |  | Pick hunks to discard from the working tree, one by one (needs a terminal) |
| `--pathspec-from-file` | `FILE` | Read the pathspecs from this file, one per line (`-` for stdin) |
| `--pathspec-file-nul` |  | With --pathspec-from-file, the pathspecs are NUL-separated |

See the [global options](global-options.md) that every command accepts.
