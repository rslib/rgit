# rgit rm

Remove tracked paths from the index and working tree

## Usage

```sh
rgit rm [OPTIONS] [PATHS]...
```

## Examples

```sh
rgit rm src/lib.rs
rgit rm src/lib.rs --cached
rgit rm -n -r src/
```

## Arguments

| Argument | Description |
| --- | --- |
| `[PATHS]...` | The paths to remove: files, folders (with -r) or globs (prompted for if omitted on a terminal) |

## Options

| Option | Value | Description |
| --- | --- | --- |
| `--pathspec-from-file` | `FILE` | Read the pathspecs from this file, one per line (`-` for stdin) |
| `--pathspec-file-nul` |  | With --pathspec-from-file, the pathspecs are NUL-separated |
| `--cached` |  | Remove only from the index, keeping the working-tree file (--cached) |
| `-r` |  | Remove folders recursively (git's -r) |
| `-f, --force` |  | Remove files even with staged or unstaged changes |
| `-n, --dry-run` |  | List what would be removed without removing it (git's -n) |
| `-q, --quiet` |  | Print nothing on success |
| `--ignore-unmatch` |  | Succeed even when a path matches no file |
| `--sparse` |  | Also update paths outside the sparse checkout |

See the [global options](global-options.md) that every command accepts.
