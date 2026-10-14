# rgit mailinfo

Read one mail from stdin, write its message and patch to MSG and PATCH, and print its author, subject and date, like `git mailinfo`

## Usage

```sh
rgit mailinfo [OPTIONS] <MSG> <PATCH>
```

## Examples

```sh
rgit mailinfo msg.txt patch.diff < /tmp/mails/0001
```

## Arguments

| Argument | Description |
| --- | --- |
| `<MSG>` | Where to write the message |
| `<PATCH>` | Where to write the patch |

## Options

| Option | Value | Description |
| --- | --- | --- |
| `-k` |  | Keep the subject as it is |
| `-b` |  | Drop only `[PATCH ...]` brackets from the subject |
| `-u` |  | Reencode to UTF-8 (the default) |
| `-m, --message-id` |  | Add the Message-ID header to the message |
| `--scissors` |  | Drop everything above a scissors line (`-- &gt;8 --`) |
| `--no-scissors` |  | Ignore scissors lines (overrides mailinfo.scissors) |

See the [global options](global-options.md) that every command accepts.
