# rgit diff-files

Compare the index with the working tree, like `git diff-files`

## Usage

```sh
rgit diff-files [OPTIONS] [PATHS]...
```

## Examples

```sh
rgit diff-files
rgit diff-files --quiet
```

## Arguments

| Argument | Description |
| --- | --- |
| `[PATHS]...` | Limit to these paths |

## Options

| Option | Value | Description |
| --- | --- | --- |
| `-p, --patch` |  | Show patches instead of the raw listing |
| `-s, --no-patch` |  | Show no diff (only diff-tree's commit id) |
| `--raw` |  | Show the raw listing (the default) |
| `--name-only` |  | Show only the names of changed files |
| `--name-status` |  | Show the names and status letters of changed files |
| `-z` |  | NUL-terminated output |
| `-r` |  | Recurse into subtrees |
| `-t` |  | Show changed trees too while recursing (implies -r) |
| `--exit-code` |  | Exit 1 when there are changes |
| `--quiet` |  | Print nothing; exit 1 when there are changes |
| `--stat` | `WIDTH` | Show a diffstat, optionally this many columns wide |
| `--summary` |  | Show created, deleted, renamed and mode-changed files |
| `--find-renames` | `N` | Detect renames, optionally at this similarity (git's -M[&lt;n&gt;]) |
| `--find-copies` | `N` | Detect copies as well as renames (git's -C[&lt;n&gt;]); twice is --find-copies-harder |
| `--find-copies-harder` |  | Look for copy sources among unmodified files too |
| `--abbrev` | `N` | Abbreviate raw object ids, to this many digits if given |

See the [global options](global-options.md) that every command accepts.
