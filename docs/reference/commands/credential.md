# rgit credential

Get, save or drop credentials through the configured helpers (`git credential`), with git's `key=value` lines on stdin and stdout

## Usage

```sh
rgit credential [OPTIONS] [ACTION]
```

## Examples

```sh
printf 'protocol=https\nhost=example.com\n' | rgit credential fill
printf 'url=https://me:token@example.com\n' | rgit credential approve
```

## Arguments

| Argument | Description |
| --- | --- |
| `[ACTION]` | fill, approve, reject or capability |

See the [global options](global-options.md) that every command accepts.
