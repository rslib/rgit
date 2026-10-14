# rgit mv

Rename/move tracked files or folders

## Usage

```sh
rgit mv [OPTIONS] <PATH> <PATH>...
```

## Examples

```sh
rgit mv src/old.rs src/new.rs
rgit mv -n src/old.rs src/new.rs
```

## Arguments

| Argument | Description |
| --- | --- |
| `<PATH>...` | The paths to move, then the destination (a folder when moving several) |

## Options

| Option | Value | Description |
| --- | --- | --- |
| `-f, --force` |  | Overwrite the destination if it exists (git's -f) |
| `-k` |  | Skip moves that would fail instead of stopping (git's -k) |
| `-n, --dry-run` |  | Show what would be moved without moving it (git's -n) |
| `-v, --verbose` |  | Print each move |
| `--sparse` |  | Also update paths outside the sparse checkout |

See the [global options](global-options.md) that every command accepts.
