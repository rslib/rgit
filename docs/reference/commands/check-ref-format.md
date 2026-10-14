# rgit check-ref-format

Check that a ref name is valid, like `git check-ref-format`; exit 1 if not

## Usage

```sh
rgit check-ref-format [OPTIONS] <NAME>
```

## Examples

```sh
rgit check-ref-format refs/heads/topic
rgit check-ref-format --branch @{-1}
rgit check-ref-format --normalize --allow-onelevel //x
```

## Arguments

| Argument | Description |
| --- | --- |
| `<NAME>` | The ref name |

## Options

| Option | Value | Description |
| --- | --- | --- |
| `--normalize` |  | Print the name with repeated and leading slashes removed |
| `--allow-onelevel` |  | Accept a name with one level (no slash) |
| `--no-allow-onelevel` |  | Require two levels (the default) |
| `--refspec-pattern` |  | Accept one `*`, as in a refspec |
| `--branch` |  | Check a branch name (expanding `@{-N}`) and print it |

See the [global options](global-options.md) that every command accepts.
