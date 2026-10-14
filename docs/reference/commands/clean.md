# rgit clean

Remove untracked files and directories

## Usage

```sh
rgit clean [OPTIONS] [PATHS]...
```

## Examples

```sh
rgit clean -n
rgit clean -fd
rgit clean -idx
```

## Arguments

| Argument | Description |
| --- | --- |
| `[PATHS]...` | Limit to these paths |

## Options

| Option | Value | Description |
| --- | --- | --- |
| `-n, --dry-run` |  | List what would be removed without deleting (git's -n) |
| `-x` |  | Also remove ignored files (git's -x) |
| `-X` |  | Remove only ignored files (git's -X) |
| `-e, --exclude` | `PATTERN` | Keep files matching this pattern (git's -e) |
| `-d` |  | Remove untracked folders too, not only files |
| `-f, --force` |  | Really remove (needed while clean.requireForce is true); twice also removes nested repositories |
| `-i, --interactive` |  | Pick what to remove from git's menu, read from stdin |
| `-q, --quiet` |  | Print nothing on success |

See the [global options](global-options.md) that every command accepts.
