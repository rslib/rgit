# rgit stage

Stage paths, some hunks of one path, or specific lines of one hunk

## Usage

```sh
rgit stage [OPTIONS] <PATHS>...
```

## Examples

```sh
rgit stage src/lib.rs
rgit stage src/lib.rs --hunk 42
rgit stage src/lib.rs --hunk 10,42
rgit stage src/lib.rs --hunk 42 --lines 0,2
```

## Arguments

| Argument | Description |
| --- | --- |
| `<PATHS>...` | The paths to stage: files, folders or globs |

## Options

| Option | Value | Description |
| --- | --- | --- |
| `--hunk` | `HUNK` | Stage only the hunks at these new-side start lines (comma-separated) |
| `--lines` | `LINES` | Stage only these line indices within one --hunk (comma-separated) |

See the [global options](global-options.md) that every command accepts.
