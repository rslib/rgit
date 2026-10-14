# rgit discard

Discard unstaged changes to paths, some hunks of one path, or specific lines

## Usage

```sh
rgit discard [OPTIONS] [PATHS]...
```

## Examples

```sh
rgit discard src/lib.rs
rgit discard src/lib.rs --hunk 10
```

## Arguments

| Argument | Description |
| --- | --- |
| `[PATHS]...` | The paths to discard: files, folders or globs (prompted for if omitted on a terminal) |

## Options

| Option | Value | Description |
| --- | --- | --- |
| `--hunk` | `HUNK` | Discard only the hunks at these new-side start lines (comma-separated) |
| `--lines` | `LINES` | Discard only these line indices within one --hunk (comma-separated) |

See the [global options](global-options.md) that every command accepts.
