# rgit unpack-objects

Write each object of a pack read from stdin as a loose object, like `git unpack-objects`; objects the repository has are skipped

## Usage

```sh
rgit unpack-objects [OPTIONS]
```

## Examples

```sh
rgit unpack-objects < received.pack
```

## Options

| Option | Value | Description |
| --- | --- | --- |
| `-n` |  | Only check the pack |
| `-q` |  | Show no progress |

See the [global options](global-options.md) that every command accepts.
