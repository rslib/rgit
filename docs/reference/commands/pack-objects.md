# rgit pack-objects

Pack the objects listed on stdin, or reachable from the revisions on stdin with `--revs`, like `git pack-objects`; prints the pack's name

## Usage

```sh
rgit pack-objects [OPTIONS] [BASE_NAME]
```

## Examples

```sh
rgit rev-list --objects main | rgit pack-objects out
echo main | rgit pack-objects --revs --stdout > main.pack
```

## Arguments

| Argument | Description |
| --- | --- |
| `[BASE_NAME]` | Where the pack goes: `&lt;base-name&gt;-&lt;hash&gt;.pack` and `.idx` |

## Options

| Option | Value | Description |
| --- | --- | --- |
| `--stdout` |  | Write the pack to stdout instead of `&lt;base-name&gt;-&lt;hash&gt;.pack` |
| `--revs` |  | Read revisions (`A`, `^B`, `--not`) from stdin and pack what they reach |
| `--all` |  | With --revs, also every ref |
| `-q, --quiet` |  | Show no progress |

See the [global options](global-options.md) that every command accepts.
