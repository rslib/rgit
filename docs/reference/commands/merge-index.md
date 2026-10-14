# rgit merge-index

Run a merge program on each unmerged path, like `git merge-index [-o] [-q] <program> (-a | [--] <path>...)`; `git-merge-one-file` runs rgit's own

## Usage

```sh
rgit merge-index [OPTIONS] [ARGS]...
```

## Examples

```sh
rgit merge-index git-merge-one-file -a
```

## Arguments

| Argument | Description |
| --- | --- |
| `[ARGS]...` |  |

See the [global options](global-options.md) that every command accepts.
