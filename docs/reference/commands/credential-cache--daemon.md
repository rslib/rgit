# rgit credential-cache--daemon

The daemon behind `credential-cache` (`git credential-cache--daemon`)

## Usage

```sh
rgit credential-cache--daemon [OPTIONS] <SOCKET>
```

## Examples

```sh
rgit credential-cache--daemon ~/.cache/git/credential/socket
```

## Arguments

| Argument | Description |
| --- | --- |
| `<SOCKET>` | The socket to listen on |

## Options

| Option | Value | Description |
| --- | --- | --- |
| `--debug` |  | Stay in the foreground output-wise: keep stderr |

See the [global options](global-options.md) that every command accepts.
