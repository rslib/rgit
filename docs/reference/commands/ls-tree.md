# rgit ls-tree

List a tree's entries, like `git ls-tree`

## Usage

```sh
rgit ls-tree [OPTIONS] <REV> [PATHS]...
```

## Examples

```sh
rgit ls-tree HEAD
rgit ls-tree -r -l HEAD src/
```

## Arguments

| Argument | Description |
| --- | --- |
| `<REV>` | The commit, tag or tree to list |
| `[PATHS]...` | Limit to these paths; `dir/` lists inside dir |

## Options

| Option | Value | Description |
| --- | --- | --- |
| `-r` |  | Recurse into subtrees |
| `-d` |  | Show only trees |
| `-t` |  | Show trees even when recursing into them |
| `-l, --long` |  | Show blob sizes |
| `--name-only` |  | Show only paths |
| `--object-only` |  | Show only object ids |
| `--abbrev` | `N` | Abbreviate object ids to a unique prefix of at least N digits |
| `-z` |  | End each entry with NUL instead of a newline |
| `--full-name` |  | Print paths from the top level, not from the current folder |
| `--full-tree` |  | List from the top level, not the current folder; implies --full-name |
| `--format` | `FORMAT` | The line format: `%(objectmode)`, `%(objecttype)`, `%(objectname)`, `%(objectsize)`, `%(objectsize:padded)`, `%(path)` |

See the [global options](global-options.md) that every command accepts.
