# rgit diff-pairs

Diff the pairs of git's raw diff format on stdin (NUL-separated, as `diff-tree -r -z` prints them), like `git diff-pairs -z`; an empty record ends a batch

## Usage

```sh
rgit diff-pairs [OPTIONS]
```

## Examples

```sh
rgit diff-tree -r -z main HEAD | rgit diff-pairs -z
rgit diff-tree -r -z -M main HEAD | rgit diff-pairs -z --stat
```

## Options

| Option | Value | Description |
| --- | --- | --- |
| `-z` |  | Records are NUL-separated (required, as in git) |
| `-p, --patch` |  | Print patches (the default) |
| `-s, --no-patch` |  | Print nothing |
| `--stat` |  | Print a diffstat |
| `--shortstat` |  | Only the diffstat's summary line |
| `--numstat` |  | Added and deleted line counts per file |
| `--name-only` |  | Only the changed paths |
| `--name-status` |  | The changed paths with their status letters |
| `-U, --unified` | `N` | Lines of context around each change |

See the [global options](global-options.md) that every command accepts.
