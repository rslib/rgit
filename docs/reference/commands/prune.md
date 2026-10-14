# rgit prune

Prune unreachable objects (git's `prune`). For deleting merged branches, use `branch prune`

## Usage

```sh
rgit prune [OPTIONS]
```

## Examples

```sh
rgit prune
rgit prune --dry-run
rgit prune -v --expire=2.weeks.ago
```

## Options

| Option | Value | Description |
| --- | --- | --- |
| `-n, --dry-run` |  | List what would be removed without deleting (git's -n) |
| `-v, --verbose` |  | Name each removed object |
| `--expire` | `DATE` | Only objects older than this (default: all) |

See the [global options](global-options.md) that every command accepts.
