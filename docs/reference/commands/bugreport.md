# rgit bugreport

Write a bug report template with system details and open it in the editor, like `git bugreport`

## Usage

```sh
rgit bugreport [OPTIONS]
```

## Examples

```sh
rgit bugreport
rgit bugreport --diagnose -o /tmp/report
```

## Options

| Option | Value | Description |
| --- | --- | --- |
| `-o, --output-directory` | `PATH` | Write the report (and the diagnostics zip) here |
| `-s, --suffix` | `FORMAT` | strftime format for the file name suffix |
| `--no-suffix` |  | No suffix |
| `--diagnose` | `MODE` | Also write a diagnostics zip: `stats` (default) or `all` |

See the [global options](global-options.md) that every command accepts.
