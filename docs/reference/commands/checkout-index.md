# rgit checkout-index

Copy files from the index to the working tree, like `git checkout-index`

## Usage

```sh
rgit checkout-index [OPTIONS] [PATHS]...
```

## Examples

```sh
rgit checkout-index -f src/lib.rs
rgit checkout-index -a --prefix=/tmp/export/
rgit checkout-index --stage=all src/lib.rs
```

## Arguments

| Argument | Description |
| --- | --- |
| `[PATHS]...` | The files to write |

## Options

| Option | Value | Description |
| --- | --- | --- |
| `-a, --all` |  | Every file in the index |
| `-f, --force` |  | Overwrite existing files |
| `-u, --index` |  | Update the index's stat data for the written files |
| `-q, --quiet` |  | Say nothing about existing or unknown files |
| `-n, --no-create` |  | Only update files that exist |
| `--prefix` | `PREFIX` | Prepend this to each path written: a folder when it ends in `/`, else a file name prefix |
| `--stage` | `N` | Write the entries of this stage (1, 2, 3, or all with --temp) |
| `--temp` |  | Write temporary files and print their names with each path |
| `--stdin` |  | Read the paths from stdin |
| `-z` |  | NUL-separated paths with --stdin, and NUL-terminated --temp lines |

See the [global options](global-options.md) that every command accepts.
