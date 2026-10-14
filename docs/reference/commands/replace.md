# rgit replace

Stand one object in for another with refs under refs/replace/, like `git replace`: `<object> <replacement>`, `--graft`, `--edit`, `-d` or a listing

## Usage

```sh
rgit replace [OPTIONS] [ARGS]...
```

## Examples

```sh
rgit replace <old> <new>
rgit replace --graft <commit> <parent>
rgit replace --format=long
rgit replace -d <old>
```

## Arguments

| Argument | Description |
| --- | --- |
| `[ARGS]...` | Objects (and replacement, parents or pattern, by mode) |

## Options

| Option | Value | Description |
| --- | --- | --- |
| `-l, --list` |  | List replace refs matching a pattern (the default with no arguments) |
| `-d, --delete` |  | Delete the replace refs of these objects |
| `-e, --edit` |  | Edit an object's content in the editor and use the result |
| `-g, --graft` |  | Replace a commit with one that has these parents |
| `--convert-graft-file` |  | Turn each line of info/grafts into a --graft, then remove the file |
| `-f, --force` |  | Replace the ref if it exists, and allow objects of another type |
| `--raw` |  | With --edit, edit a tree's raw bytes rather than ls-tree lines |
| `--format` | `FORMAT` | Listing format: short, medium or long |

See the [global options](global-options.md) that every command accepts.
