# rgit add

Stage paths as git's `add` does: `add <paths>`, `add .`, `add -A`, `add -u`

## Usage

```sh
rgit add [OPTIONS] [PATHS]...
```

## Examples

```sh
rgit add .
rgit add src/ '*.md'
rgit add -A
rgit add -u
rgit add -p
rgit add -i
rgit add -e src/lib.rs
rgit add --chmod=+x build.sh
rgit add --renormalize .
rgit add -n .
```

## Arguments

| Argument | Description |
| --- | --- |
| `[PATHS]...` | The paths to add: files, folders or globs |

## Options

| Option | Value | Description |
| --- | --- | --- |
| `--pathspec-from-file` | `FILE` | Read the pathspecs from this file, one per line (`-` for stdin) |
| `--pathspec-file-nul` |  | With --pathspec-from-file, the pathspecs are NUL-separated |
| `-A, --all` |  | Stage every change, new and deleted files too (the whole repo when no paths are given) |
| `-u, --update` |  | Stage only changes to tracked files (the whole repo when no paths are given) |
| `-f, --force` |  | Add ignored files too |
| `-n, --dry-run` |  | Show what would be added without adding it (git's -n) |
| `-v, --verbose` |  | Print each added or removed path |
| `-N, --intent-to-add` |  | Record only that untracked paths will be added, with no content (git's -N) |
| `--ignore-errors` |  | Add what can be added and report the paths that failed |
| `-p, --patch` |  | Pick hunks to stage, one by one (needs a terminal) |
| `-i, --interactive` |  | Pick what to stage from git's interactive menu (needs a terminal) |
| `-e, --edit` |  | Edit the unstaged diff in the editor and stage what is left |
| `--renormalize` |  | Stage tracked files again with the clean filters and line endings applied afresh (implies -u) |
| `--chmod` | `(+\|-)x` | Set the executable bit of the added files in the index: `+x` or `-x` |
| `--sparse` |  | Also update paths outside the sparse checkout |

See the [global options](global-options.md) that every command accepts.
