# rgit verify-pack

Check packs against their indexes, like `git verify-pack`; `-v` lists every object and the delta chain histogram

## Usage

```sh
rgit verify-pack [OPTIONS] [PACKS]...
```

## Examples

```sh
rgit verify-pack -v .git/objects/pack/pack-*.idx
```

## Arguments

| Argument | Description |
| --- | --- |
| `[PACKS]...` | The packs: `&lt;pack&gt;.idx`, `&lt;pack&gt;.pack` or `&lt;pack&gt;` |

## Options

| Option | Value | Description |
| --- | --- | --- |
| `-v, --verbose` |  | List each object: id, type, size, size in pack, offset and delta base |
| `-s, --stat-only` |  | Show only the delta chain histogram |
| `--object-format` | `HASH` | The hash algorithm (only sha1) |

See the [global options](global-options.md) that every command accepts.
