# rgit replay

Replay commits onto a new base without touching the working tree, like `git replay`: updates the refs (git's default since 2.53), or with --ref-action=print emits `update <ref> <new> <old>` lines for `rgit update-ref --stdin`; exits 1 on a conflict

## Usage

```sh
rgit replay [OPTIONS] [REVISION-RANGE]...
```

## Examples

```sh
rgit replay --onto main main..topic
rgit replay --advance main main..topic
rgit replay --onto main main..topic | rgit update-ref --stdin
```

## Arguments

| Argument | Description |
| --- | --- |
| `[REVISION-RANGE]...` | The commits to replay (`base..branch`) |

## Options

| Option | Value | Description |
| --- | --- | --- |
| `--onto` | `REVISION` | Replay onto this commit, updating the branches given |
| `--advance` | `BRANCH` | Replay onto this branch and advance it |
| `--contained` |  | Update every branch inside the replayed range |
| `--ref-action` | `MODE` | What to do with the refs the replay touches: update them (default) or print update lines without touching them (default: `update`) |

See the [global options](global-options.md) that every command accepts.
