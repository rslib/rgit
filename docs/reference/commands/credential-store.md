# rgit credential-store

Keep credentials in a plain-text file (`git credential-store`), the `store` credential helper

## Usage

```sh
rgit credential-store [OPTIONS] [ACTION]
```

## Examples

```sh
rgit config --global credential.helper store
printf 'protocol=https\nhost=example.com\n' | rgit credential-store get
```

## Arguments

| Argument | Description |
| --- | --- |
| `[ACTION]` | get, store or erase |

## Options

| Option | Value | Description |
| --- | --- | --- |
| `--file` | `PATH` | The file to use instead of ~/.git-credentials and $XDG_CONFIG_HOME/git/credentials |

See the [global options](global-options.md) that every command accepts.
