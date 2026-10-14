# rgit reset

Reset HEAD to a revision (default mixed)

## Usage

```sh
rgit reset [OPTIONS] [REV] [PATH]... [-- <PATH>...]
```

## Examples

```sh
rgit reset HEAD~1
rgit reset --hard <rev>
rgit reset src/lib.rs
rgit reset <rev> -- src/lib.rs
rgit reset --merge
rgit reset -p <rev>
```

## Arguments

| Argument | Description |
| --- | --- |
| `[REV]` | The revision to reset to (prompted for if omitted on a terminal). A path here unstages it (git's `reset &lt;paths&gt;`) |
| `[PATH]...` | Paths whose index entries to reset to `rev` (default HEAD), leaving HEAD alone |
| `[PATH]...` | Paths to reset (after `--`) |

## Options

| Option | Value | Description |
| --- | --- | --- |
| `--soft` |  | Move HEAD only, keep the index and working tree |
| `--mixed` |  | Reset the index but not the working tree (the default) |
| `--hard` |  | Reset the index and working tree too (discards changes) |
| `--keep` |  | Like --hard, but keep local changes and refuse to overwrite them |
| `--merge` |  | Reset the index and the files that change, keeping unstaged changes to other files (aborts a merge; git's --merge) |
| `-p, --patch` |  | Pick staged hunks to unstage, one by one (needs a terminal) |
| `-q, --quiet` |  | Print nothing on success |
| `--pathspec-from-file` | `FILE` | Read the pathspecs from this file, one per line (`-` for stdin) |
| `--pathspec-file-nul` |  | With --pathspec-from-file, the pathspecs are NUL-separated |

See the [global options](global-options.md) that every command accepts.
