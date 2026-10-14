# rgit restore

Restore files in the working tree (or, with --staged, the index) from the index or a revision

## Usage

```sh
rgit restore [OPTIONS] [PATHS]...
```

## Examples

```sh
rgit restore src/lib.rs
rgit restore --staged src/lib.rs
rgit restore --source HEAD~1 src/lib.rs
rgit restore --ours src/lib.rs
rgit restore --merge src/lib.rs
rgit restore -p --source=HEAD~1 --staged --worktree
```

## Arguments

| Argument | Description |
| --- | --- |
| `[PATHS]...` | The paths to restore: files, folders or globs |

## Options

| Option | Value | Description |
| --- | --- | --- |
| `--pathspec-from-file` | `FILE` | Read the pathspecs from this file, one per line (`-` for stdin) |
| `--pathspec-file-nul` |  | With --pathspec-from-file, the pathspecs are NUL-separated |
| `-s, --source` | `REV` | Restore from this revision (default: the index, or HEAD with --staged) |
| `-S, --staged` |  | Restore the index (unstage) |
| `-W, --worktree` |  | Restore the working tree (the default; with --staged, both) |
| `--ours` |  | For conflicted paths, take our side (git's --ours) |
| `--theirs` |  | For conflicted paths, take their side (git's --theirs) |
| `-m, --merge` |  | Recreate the merge conflicts of these paths (git's -m) |
| `--conflict` | `STYLE` | Recreate the conflicts with this marker style: merge, diff3 or zdiff3 |
| `--overlay` |  | Keep files the source lacks instead of removing them |
| `-p, --patch` |  | Pick hunks to restore, one by one (needs a terminal) |

See the [global options](global-options.md) that every command accepts.
