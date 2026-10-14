# rgit reword

Change any commit's message and restack its descendants (default HEAD)

## Usage

```sh
rgit reword [OPTIONS] --message <MESSAGE> [REV]
```

## Examples

```sh
rgit reword -m "<message>"
rgit reword -m "<message>" <rev>
```

## Arguments

| Argument | Description |
| --- | --- |
| `[REV]` | The commit to reword (default HEAD) |

## Options

| Option | Value | Description |
| --- | --- | --- |
| `-m, --message` | `MESSAGE` | The new message |

See the [global options](global-options.md) that every command accepts.
