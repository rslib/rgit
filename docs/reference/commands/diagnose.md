# rgit diagnose

Zip up repository statistics for a bug report, like `git diagnose`

## Usage

```sh
rgit diagnose [OPTIONS]
```

## Examples

```sh
rgit diagnose
rgit diagnose --mode=all -o /tmp
```

## Options

| Option | Value | Description |
| --- | --- | --- |
| `-o, --output-directory` | `PATH` | Write the zip here |
| `-s, --suffix` | `FORMAT` | strftime format for the file name suffix |
| `--mode` | `MODE` | `stats` (default) or `all` (also the repository's own files) |

See the [global options](global-options.md) that every command accepts.
