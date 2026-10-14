# rgit unpack-file

Write a blob to a temporary `.merge_file_XXXXXX` file and print its name, like `git unpack-file`

## Usage

```sh
rgit unpack-file [OPTIONS] <BLOB>
```

## Examples

```sh
rgit unpack-file HEAD:README.md
```

## Arguments

| Argument | Description |
| --- | --- |
| `<BLOB>` | The blob |

See the [global options](global-options.md) that every command accepts.
