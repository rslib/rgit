# rgit merge-one-file

Resolve one unmerged path from its three stages, like the `git merge-one-file` script: `<orig> <ours> <theirs> <path> <orig mode> <our mode> <their mode>`, empty for a missing side

## Usage

```sh
rgit merge-one-file [OPTIONS] [ARGS]...
```

## Examples

```sh
rgit merge-one-file <orig> <ours> <theirs> <path> 100644 100644 100644
```

## Arguments

| Argument | Description |
| --- | --- |
| `[ARGS]...` |  |

See the [global options](global-options.md) that every command accepts.
