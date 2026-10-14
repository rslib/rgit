# rgit rerere

Reuse recorded conflict resolutions (`git rerere`, in git's .git/rr-cache): no argument records and replays, or `status`, `remaining`, `diff`, `forget <paths>`, `clear` or `gc`

## Usage

```sh
rgit rerere [OPTIONS] [ARGS]...
```

## Examples

```sh
rgit config rerere.enabled true
rgit rerere status
rgit rerere diff
rgit rerere forget src/lib.rs
rgit rerere gc
```

## Arguments

| Argument | Description |
| --- | --- |
| `[ARGS]...` | The subcommand and its paths |

## Options

| Option | Value | Description |
| --- | --- | --- |
| `--rerere-autoupdate` |  | Stage the files rerere resolved with a recorded resolution |
| `--no-rerere-autoupdate` |  | Leave them unstaged, whatever rerere.autoUpdate says |

See the [global options](global-options.md) that every command accepts.
