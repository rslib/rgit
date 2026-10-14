# rgit merge-base

Find the common ancestor of two commits, like `git merge-base`

## Usage

```sh
rgit merge-base [OPTIONS] <A> <B>
```

## Examples

```sh
rgit merge-base main HEAD
rgit merge-base --is-ancestor main HEAD
```

## Arguments

| Argument | Description |
| --- | --- |
| `<A>` | The first commit |
| `<B>` | The second commit |

## Options

| Option | Value | Description |
| --- | --- | --- |
| `-a, --all` |  | Print every best common ancestor |
| `--is-ancestor` |  | Exit 0 if the first commit is an ancestor of the second, else 1 |

See the [global options](global-options.md) that every command accepts.
