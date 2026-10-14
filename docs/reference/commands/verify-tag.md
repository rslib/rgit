# rgit verify-tag

Check annotated tags' gpg, x509 or ssh signatures (`git verify-tag`)

## Usage

```sh
rgit verify-tag [OPTIONS] <TAGS>...
```

## Examples

```sh
rgit verify-tag v1.0
rgit verify-tag -v v1.0
```

## Arguments

| Argument | Description |
| --- | --- |
| `<TAGS>...` | The tags to check |

## Options

| Option | Value | Description |
| --- | --- | --- |
| `-v, --verbose` |  | Print each tag's contents too |
| `--raw` |  | Print the verifier's status lines instead of its report |

See the [global options](global-options.md) that every command accepts.
