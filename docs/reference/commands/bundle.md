# rgit bundle

Move history as one file (`git bundle`): create, verify, list-heads, unbundle

## Usage

```sh
rgit bundle [OPTIONS] <COMMAND>
```

## Examples

```sh
rgit bundle create repo.bundle --all
rgit bundle verify repo.bundle
```

## Subcommands

### rgit bundle create

Write a bundle of the refs and ranges given (`--all`, `main`, `v1..main`, `^old main`)

#### Usage

```sh
rgit bundle create [OPTIONS] <FILE> <REVS>...
```

#### Examples

```sh
rgit bundle create repo.bundle --all
rgit bundle create update.bundle v1.0..main
```

#### Arguments

| Argument | Description |
| --- | --- |
| `<FILE>` | The bundle file |
| `<REVS>...` | What to bundle, as for rev-list |

#### Options

| Option | Value | Description |
| --- | --- | --- |
| `-q, --quiet` |  | Print nothing |

### rgit bundle verify

Check that a bundle is valid and applies to this repository

#### Usage

```sh
rgit bundle verify [OPTIONS] <FILE>
```

#### Examples

```sh
rgit bundle verify update.bundle
```

#### Arguments

| Argument | Description |
| --- | --- |
| `<FILE>` | The bundle file |

#### Options

| Option | Value | Description |
| --- | --- | --- |
| `-q, --quiet` |  | Only say whether it is okay |

### rgit bundle list-heads

List the refs in a bundle

#### Usage

```sh
rgit bundle list-heads [OPTIONS] <FILE> [REFNAMES]...
```

#### Examples

```sh
rgit bundle list-heads repo.bundle
```

#### Arguments

| Argument | Description |
| --- | --- |
| `<FILE>` | The bundle file |
| `[REFNAMES]...` | Only these refs |

### rgit bundle unbundle

Store a bundle's objects here and print its refs (refs are not changed)

#### Usage

```sh
rgit bundle unbundle [OPTIONS] <FILE>
```

#### Examples

```sh
rgit bundle unbundle update.bundle
```

#### Arguments

| Argument | Description |
| --- | --- |
| `<FILE>` | The bundle file |

See the [global options](global-options.md) that every command accepts.
