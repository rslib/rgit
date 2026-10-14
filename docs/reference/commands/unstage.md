# rgit unstage

Unstage paths, some hunks of one path, or specific lines of one hunk

## Usage

```sh
rgit unstage [OPTIONS] <PATHS>...
```

## Examples

```sh
rgit unstage src/lib.rs
rgit unstage src/lib.rs --hunk 42
```

## Arguments

| Argument | Description |
| --- | --- |
| `<PATHS>...` | The paths to unstage: files, folders or globs |

## Options

| Option | Value | Description |
| --- | --- | --- |
| `--hunk` | `HUNK` | Unstage only the hunks at these new-side start lines (comma-separated) |
| `--lines` | `LINES` | Unstage only these line indices within one --hunk (comma-separated) |

See the [global options](global-options.md) that every command accepts.
