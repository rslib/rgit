# rgit squash

Fold a commit into its parent (default HEAD), or a whole range with `--from` (fold every commit after <rev> up to HEAD into one)

## Usage

```sh
rgit squash [OPTIONS] [REV]
```

## Examples

```sh
rgit squash
rgit squash <rev>
rgit squash --from <rev>
```

## Arguments

| Argument | Description |
| --- | --- |
| `[REV]` | The commit to squash into its parent (default HEAD) |

## Options

| Option | Value | Description |
| --- | --- | --- |
| `--from` | `FROM` | Fold every commit after this one, up to HEAD, into a single commit |

See the [global options](global-options.md) that every command accepts.
