# rgit mktag

Check a tag object on stdin and store it, like `git mktag`

## Usage

```sh
rgit mktag [OPTIONS]
```

## Examples

```sh
rgit mktag < tag.txt
```

## Options

| Option | Value | Description |
| --- | --- | --- |
| `--no-strict` |  | Allow what git's fsck only warns about (a missing tagger, a bad tag name) |

See the [global options](global-options.md) that every command accepts.
