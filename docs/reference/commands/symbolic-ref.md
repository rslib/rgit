# rgit symbolic-ref

Print where a symbolic ref points (`HEAD` -> `refs/heads/main`), point it elsewhere, or delete it, like `git symbolic-ref`

## Usage

```sh
rgit symbolic-ref [OPTIONS] <NAME> [TARGET]
```

## Examples

```sh
rgit symbolic-ref HEAD
rgit symbolic-ref --short HEAD
rgit symbolic-ref HEAD refs/heads/main
```

## Arguments

| Argument | Description |
| --- | --- |
| `<NAME>` | The symbolic ref, usually HEAD |
| `[TARGET]` | Point it at this ref (`refs/heads/main`) |

## Options

| Option | Value | Description |
| --- | --- | --- |
| `--short` |  | Shorten the ref name (`main`) |
| `-q, --quiet` |  | Fail without a message when the ref is not symbolic (detached HEAD) |
| `-d, --delete` |  | Delete the symbolic ref |
| `-m` | `REASON` | The reflog message for the change |

See the [global options](global-options.md) that every command accepts.
