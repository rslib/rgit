# rgit sparse-checkout

Check out only some folders, like `git sparse-checkout`: `set`, `add`, `list`, `init`, `reapply`, `disable` and `check-rules`, with `--cone`, `--no-cone`, `--[no-]sparse-index`, `--skip-checks`, `--stdin`, and check-rules' `-z` and `--rules-file <file>`

## Usage

```sh
rgit sparse-checkout [OPTIONS] [ARGS]...
```

## Examples

```sh
rgit sparse-checkout set src docs
rgit sparse-checkout add tests
rgit sparse-checkout list
rgit sparse-checkout set --no-cone '/*' '!/build/'
rgit add --sparse vendor/lib.c
rgit sparse-checkout disable
```

## Arguments

| Argument | Description |
| --- | --- |
| `[ARGS]...` |  |

See the [global options](global-options.md) that every command accepts.
