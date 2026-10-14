# rgit git

Run a git subcommand that rgit does not have, such as `lfs` or `filter-repo`. rgit takes git's own commands and flags directly

## Usage

```sh
rgit git [OPTIONS] [ARGS]...
```

## Examples

```sh
rgit git lfs pull
rgit git filter-repo --path src
```

## Arguments

| Argument | Description |
| --- | --- |
| `[ARGS]...` | The git subcommand and its arguments |

See the [global options](global-options.md) that every command accepts.
