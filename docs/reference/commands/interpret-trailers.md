# rgit interpret-trailers

Add or parse trailers (`Signed-off-by: ...`) in a commit message read from files or stdin, like `git interpret-trailers`. Takes git's options in order: `--trailer <key>[(=|:)<value>]`, `--where`, `--if-exists`, `--if-missing` (and their `--no-` forms, applying to the trailers after them), `--in-place`, `--trim-empty`, `--only-trailers`, `--only-input`, `--unfold`, `--parse`, `--no-divider`, then the files

## Usage

```sh
rgit interpret-trailers [OPTIONS] [ARGS]...
```

## Examples

```sh
rgit interpret-trailers --trailer 'Reviewed-by: A <a@x>' < msg.txt
rgit interpret-trailers --in-place --where start --trailer Fixes=123 msg.txt
rgit log -1 --format=%B | rgit interpret-trailers --parse
```

## Arguments

| Argument | Description |
| --- | --- |
| `[ARGS]...` | Options and files, as for git |

See the [global options](global-options.md) that every command accepts.
