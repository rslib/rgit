# rgit credential-cache

Keep credentials in memory for a while (`git credential-cache`), the `cache` credential helper; a background daemon holds them

## Usage

```sh
rgit credential-cache [OPTIONS] [ACTION]
```

## Examples

```sh
rgit config --global credential.helper 'cache --timeout=3600'
rgit credential-cache exit
```

## Arguments

| Argument | Description |
| --- | --- |
| `[ACTION]` | get, store, erase or exit |

## Options

| Option | Value | Description |
| --- | --- | --- |
| `--timeout` | `SECONDS` | Seconds to keep a credential (default: `900`) |
| `--socket` | `PATH` | The daemon's socket |

See the [global options](global-options.md) that every command accepts.
