# rgit init

Create a new repository in the current directory (or PATH)

## Usage

```sh
rgit init [OPTIONS] [PATH]
```

## Examples

```sh
rgit init
rgit init <path> -b main
rgit init --bare --shared=group <path>.git
rgit init --separate-git-dir ../<path>.git
```

## Arguments

| Argument | Description |
| --- | --- |
| `[PATH]` | Where to create the repository (defaults to the current directory) |

## Options

| Option | Value | Description |
| --- | --- | --- |
| `-b, --initial-branch` | `INITIAL_BRANCH` | Name of the initial branch (git's -b/--initial-branch) |
| `--bare` |  | Create a bare repository (git's --bare) |
| `--template` | `DIR` | Copy hooks and other files from DIR into the new repository |
| `--shared` | `PERMISSIONS` | Share the repository: group (default), all, umask or an octal mode |
| `--separate-git-dir` | `GIT_DIR` | Put the repository in GIT_DIR and link it from the working tree |
| `--object-format` | `FORMAT` | The object id hash: only sha1 is supported |
| `-q, --quiet` |  | Print nothing unless an error occurs |

See the [global options](global-options.md) that every command accepts.
