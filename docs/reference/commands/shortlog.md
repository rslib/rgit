# rgit shortlog

Summarize commits by author, like `git shortlog`

## Usage

```sh
rgit shortlog [OPTIONS] [REVS]...
```

## Examples

```sh
rgit shortlog -sn
rgit shortlog -sne --all
```

## Arguments

| Argument | Description |
| --- | --- |
| `[REVS]...` | Revisions or ranges (default HEAD) |

## Options

| Option | Value | Description |
| --- | --- | --- |
| `-s, --summary` |  | Print only each author's commit count |
| `-n, --numbered` |  | Sort by commit count instead of by name |
| `-e, --email` |  | Show email addresses |
| `-c, --committer` |  | Group by committer instead of author |
| `--all` |  | Walk every ref and HEAD |

See the [global options](global-options.md) that every command accepts.
