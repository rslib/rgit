# rgit fast-import

Read a fast-import stream on stdin and write its objects and refs, like `git fast-import` (`--quiet`, `--force`, `--date-format=`, `--export-marks=`, `--import-marks[-if-exists]=`, `--done`)

## Usage

```sh
rgit fast-import [OPTIONS] [ARGS]...
```

## Examples

```sh
rgit fast-import < repo.stream
rgit fast-import --quiet --export-marks=marks < repo.stream
```

## Arguments

| Argument | Description |
| --- | --- |
| `[ARGS]...` |  |

See the [global options](global-options.md) that every command accepts.
