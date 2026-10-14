# rgit name-rev

Name commits by the refs that reach them (`main~2`, `tags/v1^0`), like `git name-rev`

## Usage

```sh
rgit name-rev [OPTIONS] [REVS]...
```

## Examples

```sh
rgit name-rev HEAD~3
rgit name-rev --tags --name-only <rev>
rgit log --format=%H | rgit name-rev --annotate-stdin
```

## Arguments

| Argument | Description |
| --- | --- |
| `[REVS]...` | The commits to name |

## Options

| Option | Value | Description |
| --- | --- | --- |
| `--name-only` |  | Print only the names |
| `--tags` |  | Use only tags |
| `--refs` | `PATTERN` | Use only refs matching this pattern (repeatable) |
| `--exclude` | `PATTERN` | Skip refs matching this pattern (repeatable) |
| `--all` |  | Name every commit reachable from a ref |
| `--annotate-stdin` |  | Append names to the object ids in the text read from stdin |
| `--no-undefined` |  | Fail on an unnamed commit instead of printing `undefined` |
| `--always` |  | Print an abbreviated id for an unnamed commit |
| `--peel-tag` |  | Name tags by the commit they point at |

See the [global options](global-options.md) that every command accepts.
