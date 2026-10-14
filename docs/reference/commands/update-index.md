# rgit update-index

Change index entries, like `git update-index`: options apply to the paths after them (`--add`, `--remove`, `--force-remove`, `--cacheinfo <mode>,<sha1>,<path>`, `--chmod=(+|-)x`, `--[no-]assume-unchanged`, `--[no-]skip-worktree`, `--info-only`, `--refresh`, `--really-refresh`, `-q`, `--unmerged`, `--ignore-missing`, `--verbose`, and last `--index-info` or `--stdin`, with `-z`)

## Usage

```sh
rgit update-index [OPTIONS] [ARGS]...
```

## Examples

```sh
rgit update-index --add new.txt
rgit update-index --chmod=+x run.sh
rgit update-index --add --cacheinfo 100644,<sha1>,path
rgit update-index --assume-unchanged config.local
rgit update-index --refresh
```

## Arguments

| Argument | Description |
| --- | --- |
| `[ARGS]...` |  |

See the [global options](global-options.md) that every command accepts.
