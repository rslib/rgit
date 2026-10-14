# rgit prune-packed

Remove loose objects that packs also hold, like `git prune-packed`

## Usage

```sh
rgit prune-packed [OPTIONS]
```

## Examples

```sh
rgit prune-packed -n
```

## Options

| Option | Value | Description |
| --- | --- | --- |
| `-n, --dry-run` |  | Print the `rm -f` commands instead |
| `-q, --quiet` |  | Show no progress |

See the [global options](global-options.md) that every command accepts.
