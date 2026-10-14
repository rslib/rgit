# rgit describe

Describe a revision relative to the nearest tag (default HEAD)

## Usage

```sh
rgit describe [OPTIONS] [REV]
```

## Examples

```sh
rgit describe
rgit describe HEAD --tags
rgit describe --tags --abbrev=0
rgit describe --contains <rev>
rgit describe --all --dirty=-wip
rgit describe --first-parent --exclude 'rc*'
```

## Arguments

| Argument | Description |
| --- | --- |
| `[REV]` | The revision to describe (defaults to HEAD) |

## Options

| Option | Value | Description |
| --- | --- | --- |
| `--tags` |  | Use lightweight tags too, not just annotated ones (git's --tags) |
| `--all` |  | Use any ref: branches as `heads/main`, tags as `tags/v1` |
| `--dirty` | `MARK` | Append this mark (default -dirty) when tracked files have changes |
| `--broken` | `MARK` | Like --dirty, but append this mark (default -broken) when the working tree cannot be read |
| `--long` |  | Always use the long format (tag-count-oid), even on a tag |
| `--abbrev` | `N` | Number of hex digits for the abbreviated commit oid (0: the tag only) |
| `--always` |  | Show the abbreviated commit oid when no tag is found |
| `--first-parent` |  | Follow only the first parent of merges |
| `--candidates` | `N` | Consider this many tags (default 10; 0 is --exact-match) |
| `--match` | `GLOB` | Only consider tags matching this glob (repeatable) |
| `--exclude` | `GLOB` | Leave out tags matching this glob (repeatable) |
| `--exact-match` |  | Print the tag only when it points at the revision itself; else fail |
| `--contains` |  | Name the revision after the oldest tag that contains it, as `v1~2` or `v1~1^2` (git's --contains) |

See the [global options](global-options.md) that every command accepts.
