# rgit gc

Pack the object database and prune unreachable objects

## Usage

```sh
rgit gc [OPTIONS]
```

## Examples

```sh
rgit gc
rgit gc --prune=now
rgit gc --aggressive --cruft
```

## Options

| Option | Value | Description |
| --- | --- | --- |
| `--prune` | `DATE` | Prune loose objects older than this date (default 2 weeks ago) |
| `--no-prune` |  | Keep every loose object |
| `--aggressive` |  | Repack more thoroughly (slow) |
| `--auto` |  | Only run when enough loose objects have piled up |
| `--force` |  | Run even if another gc may be running |
| `--keep-largest-pack` |  | Leave the largest pack as it is |
| `--cruft` |  | Put unreachable objects in a cruft pack instead of loose files (the default, per gc.cruftPacks) |
| `--no-cruft` |  | Loosen unreachable objects instead of a cruft pack |
| `-q, --quiet` |  | Print nothing |

See the [global options](global-options.md) that every command accepts.
