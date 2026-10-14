# rgit mailsplit

Split mboxes or Maildirs into one numbered file per mail, like `git mailsplit`; prints the number of the last mail

## Usage

```sh
rgit mailsplit [OPTIONS] -o <DIR> [MBOXES]...
```

## Examples

```sh
rgit mailsplit -o/tmp/mails series.mbox
rgit mailsplit -b -d3 -f10 -o/tmp/mails --mboxrd inbox.mbox
```

## Arguments

| Argument | Description |
| --- | --- |
| `[MBOXES]...` | The mboxes or Maildirs (default: stdin) |

## Options

| Option | Value | Description |
| --- | --- | --- |
| `-o` | `DIR` | The folder to write the mails to |
| `-b` |  | Take a file that does not start with a `From ` line as one mail |
| `-f` | `N` | Number the mails after N (default 0) (default: `0`) |
| `-d` | `N` | Digits in the file names (default 4) (default: `4`) |
| `--keep-cr` |  | Keep CRLF line endings |
| `--mboxrd` |  | Unescape `&gt;From ` lines, as in an mboxrd |

See the [global options](global-options.md) that every command accepts.
