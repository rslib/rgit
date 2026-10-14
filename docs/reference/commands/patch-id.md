# rgit patch-id

Print the patch id of each patch read from stdin (`git log -p`, `format-patch` or `diff` output), like `git patch-id`

## Usage

```sh
rgit patch-id [OPTIONS]
```

## Examples

```sh
rgit log -p main..topic | rgit patch-id --stable
rgit diff | rgit patch-id
```

## Options

| Option | Value | Description |
| --- | --- | --- |
| `--stable` |  | Sum per-file hashes so file order does not matter |
| `--unstable` |  | Hash the patch as one stream (the default) |
| `--verbatim` |  | Keep whitespace in the hash (implies --stable) |

See the [global options](global-options.md) that every command accepts.
