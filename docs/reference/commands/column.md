# rgit column

Lay out lines read from stdin in columns, like `git column`

## Usage

```sh
rgit column [OPTIONS]
```

## Examples

```sh
rgit branch | rgit column --mode=column,dense --width=80
```

## Options

| Option | Value | Description |
| --- | --- | --- |
| `--command` | `NAME` | Read column.&lt;name&gt; as well as column.ui |
| `--mode` | `MODE` | The layout: `column`, `row` or `plain`, with `dense`/`nodense` and `always`/`never`/`auto` |
| `--raw-mode` | `N` | The layout as git's option bits |
| `--width` | `N` | The maximum width (default: the terminal's, less one) |
| `--indent` | `STRING` | Text before each row |
| `--nl` | `STRING` | Text after each row (default a newline) |
| `--padding` | `N` | Spaces between columns (default: `1`) |

See the [global options](global-options.md) that every command accepts.
