# rgit merge-base

Find the common ancestor of two commits, like `git merge-base`

## Usage

```sh
rgit merge-base [OPTIONS] <A> [B]
```

## Examples

```sh
rgit merge-base main HEAD
rgit merge-base --is-ancestor main HEAD
```

## Arguments

| Argument | Description |
| --- | --- |
| `<A>` | The first commit; with --fork-point, the ref |
| `[B]` | The second commit (default HEAD with --fork-point) |

## Options

| Option | Value | Description |
| --- | --- | --- |
| `-a, --all` |  | Print every best common ancestor |
| `--is-ancestor` |  | Exit 0 if the first commit is an ancestor of the second, else 1 |
| `--fork-point` |  | Where the commit left the history the ref has had, by its reflog |

See the [global options](global-options.md) that every command accepts.
