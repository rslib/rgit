# rgit stripspace

Clean up text read from stdin as git cleans a commit message, like `git stripspace`

## Usage

```sh
rgit stripspace [OPTIONS]
```

## Examples

```sh
rgit stripspace < msg.txt
rgit stripspace -s < msg.txt
```

## Options

| Option | Value | Description |
| --- | --- | --- |
| `-s, --strip-comments` |  | Drop lines starting with the comment character too |
| `-c, --comment-lines` |  | Prefix each line with the comment character instead |

See the [global options](global-options.md) that every command accepts.
