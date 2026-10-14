# rgit check-ignore

Show which paths are ignored and by which rule, like `git check-ignore`

## Usage

```sh
rgit check-ignore [OPTIONS] [PATHS]...
```

## Examples

```sh
rgit check-ignore -v target/out.o
rgit check-ignore --stdin < paths.txt
```

## Arguments

| Argument | Description |
| --- | --- |
| `[PATHS]...` | The paths to check |

## Options

| Option | Value | Description |
| --- | --- | --- |
| `-v, --verbose` |  | Show the rule that matched: `source:line:pattern&lt;TAB&gt;path` |
| `-q, --quiet` |  | Print nothing; only set the exit code |
| `-n, --non-matching` |  | With -v, also show paths that are not ignored |
| `--no-index` |  | Check tracked paths too |
| `--stdin` |  | Read the paths from stdin, one per line |
| `-z` |  | Separate input and output fields with NUL |

See the [global options](global-options.md) that every command accepts.
