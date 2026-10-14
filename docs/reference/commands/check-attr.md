# rgit check-attr

Print the gitattributes of paths, like `git check-attr`: `ATTR PATH...`, `ATTR... -- PATH...` or `-a PATH...`

## Usage

```sh
rgit check-attr [OPTIONS] [ATTR_OR_PATH]... [-- <PATH>...]
```

## Examples

```sh
rgit check-attr -a src/lib.rs
rgit check-attr text eol -- a.txt b.bin
```

## Arguments

| Argument | Description |
| --- | --- |
| `[ATTR_OR_PATH]...` | Attributes, then paths |
| `[PATH]...` | The paths, after `--` |

## Options

| Option | Value | Description |
| --- | --- | --- |
| `-a, --all` |  | Every attribute that is set, unset or has a value |
| `--cached` |  | Read .gitattributes from the index only |
| `--stdin` |  | Read the paths from stdin, one per line |
| `-z` |  | NUL-separated input and output |

See the [global options](global-options.md) that every command accepts.
