# rgit show-index

List a pack index read from stdin, like `git show-index`: `<offset> <id> (<crc32>)` per object

## Usage

```sh
rgit show-index [OPTIONS]
```

## Examples

```sh
rgit show-index < .git/objects/pack/pack-<hash>.idx
```

## Options

| Option | Value | Description |
| --- | --- | --- |
| `--object-format` | `HASH` | The hash algorithm (only sha1) |

See the [global options](global-options.md) that every command accepts.
