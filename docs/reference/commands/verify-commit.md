# rgit verify-commit

Check commits' gpg, x509 or ssh signatures (`git verify-commit`)

## Usage

```sh
rgit verify-commit [OPTIONS] <COMMITS>...
```

## Examples

```sh
rgit verify-commit HEAD
rgit verify-commit -v --raw HEAD
```

## Arguments

| Argument | Description |
| --- | --- |
| `<COMMITS>...` | The commits to check |

## Options

| Option | Value | Description |
| --- | --- | --- |
| `-v, --verbose` |  | Print each commit's contents too |
| `--raw` |  | Print the verifier's status lines instead of its report |

See the [global options](global-options.md) that every command accepts.
