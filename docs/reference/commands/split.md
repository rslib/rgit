# rgit split

Split a commit into two by path (given paths first, the rest second)

## Usage

```sh
rgit split [OPTIONS] <PATHS>...
```

## Examples

```sh
rgit split src/lib.rs src/main.rs
rgit split --rev <rev> src/lib.rs
```

## Arguments

| Argument | Description |
| --- | --- |
| `<PATHS>...` | Paths whose changes go into the first commit |

## Options

| Option | Value | Description |
| --- | --- | --- |
| `--rev` | `REV` | The commit to split (default HEAD) (default: `HEAD`) |

See the [global options](global-options.md) that every command accepts.
