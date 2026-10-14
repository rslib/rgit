# rgit fmt-merge-msg

Write a merge commit's message from FETCH_HEAD-style lines on stdin, like `git fmt-merge-msg`

## Usage

```sh
rgit fmt-merge-msg [OPTIONS]
```

## Examples

```sh
rgit fmt-merge-msg < .git/FETCH_HEAD
rgit fmt-merge-msg --log=5 -F .git/FETCH_HEAD
```

## Options

| Option | Value | Description |
| --- | --- | --- |
| `-m, --message` | `TEXT` | Start with this text instead of the "Merge ..." title |
| `--log` | `N` | Add a shortlog of at most N commits per merged head (default 20) |
| `--no-log` |  | No shortlog, whatever merge.log says |
| `--into-name` | `NAME` | Name this branch as the one merged into |
| `-F, --file` | `FILE` | Read the lines from this file instead of stdin |

See the [global options](global-options.md) that every command accepts.
