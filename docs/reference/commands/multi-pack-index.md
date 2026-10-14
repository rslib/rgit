# rgit multi-pack-index

Write, check, expire or repack the multi-pack-index (`git multi-pack-index`)

## Usage

```sh
rgit multi-pack-index [OPTIONS] <COMMAND>
```

## Examples

```sh
rgit multi-pack-index write
rgit multi-pack-index verify
```

## Subcommands

### rgit multi-pack-index write

Index every pack in objects/pack

#### Usage

```sh
rgit multi-pack-index write [OPTIONS]
```

#### Examples

```sh
rgit multi-pack-index write
rgit multi-pack-index write --preferred-pack=pack-1234.pack
```

#### Options

| Option | Value | Description |
| --- | --- | --- |
| `--preferred-pack` | `PACK` | Take duplicate objects from this pack (`pack-&lt;hash&gt;.pack`) |
| `--no-progress` |  | Accepted for git compatibility; rgit prints no progress |

### rgit multi-pack-index verify

Check the multi-pack-index against the packs

#### Usage

```sh
rgit multi-pack-index verify [OPTIONS]
```

#### Examples

```sh
rgit multi-pack-index verify
```

#### Options

| Option | Value | Description |
| --- | --- | --- |
| `--no-progress` |  | Accepted for git compatibility; rgit prints no progress |

### rgit multi-pack-index expire

Delete packs the multi-pack-index takes no object from

#### Usage

```sh
rgit multi-pack-index expire [OPTIONS]
```

#### Examples

```sh
rgit multi-pack-index expire
```

#### Options

| Option | Value | Description |
| --- | --- | --- |
| `--no-progress` |  | Accepted for git compatibility; rgit prints no progress |

### rgit multi-pack-index repack

Pack the objects of the oldest small packs into one new pack

#### Usage

```sh
rgit multi-pack-index repack [OPTIONS]
```

#### Examples

```sh
rgit multi-pack-index repack --batch-size=0
rgit multi-pack-index repack --batch-size=100m
```

#### Options

| Option | Value | Description |
| --- | --- | --- |
| `--batch-size` | `SIZE` | How much to repack (k, m, g suffixes; 0: every pack) (default: `0`) |
| `--no-progress` |  | Accepted for git compatibility; rgit prints no progress |

See the [global options](global-options.md) that every command accepts.
