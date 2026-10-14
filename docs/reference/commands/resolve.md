# rgit resolve

Resolve a conflicted path by taking ours or theirs

## Usage

```sh
rgit resolve [OPTIONS] <PATH>
```

## Examples

```sh
rgit resolve src/lib.rs --ours
rgit resolve src/lib.rs --theirs
```

## Arguments

| Argument | Description |
| --- | --- |
| `<PATH>` | The conflicted path |

## Options

| Option | Value | Description |
| --- | --- | --- |
| `--ours` |  | Take our side of the conflict |
| `--theirs` |  | Take their side of the conflict |

See the [global options](global-options.md) that every command accepts.
