# rgit pack-refs

Move loose refs into packed-refs (`git pack-refs`)

## Usage

```sh
rgit pack-refs [OPTIONS]
```

## Examples

```sh
rgit pack-refs --all
```

## Options

| Option | Value | Description |
| --- | --- | --- |
| `--all` |  | Pack every ref, not only tags and refs already packed |
| `--no-prune` |  | Keep the loose ref files |
| `--auto` |  | Only when enough loose refs have piled up |

See the [global options](global-options.md) that every command accepts.
