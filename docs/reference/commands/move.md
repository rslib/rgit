# rgit move

Move a commit before or after another in the current branch's history

## Usage

```sh
rgit move [OPTIONS] <REV>
```

## Examples

```sh
rgit move <rev> --before <rev>
rgit move <rev> --after <rev>
```

## Arguments

| Argument | Description |
| --- | --- |
| `<REV>` | The commit to move |

## Options

| Option | Value | Description |
| --- | --- | --- |
| `--before` | `BEFORE` | Move it to just before this commit |
| `--after` | `AFTER` | Move it to just after this commit |

See the [global options](global-options.md) that every command accepts.
