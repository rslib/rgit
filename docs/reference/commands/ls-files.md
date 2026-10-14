# rgit ls-files

List files in the index and the working tree, like `git ls-files`

## Usage

```sh
rgit ls-files [OPTIONS] [PATHS]...
```

## Examples

```sh
rgit ls-files
rgit ls-files -s src
rgit ls-files -o --exclude-standard
```

## Arguments

| Argument | Description |
| --- | --- |
| `[PATHS]...` | Limit to these paths: files, folders or globs (default: the current folder) |

## Options

| Option | Value | Description |
| --- | --- | --- |
| `-c, --cached` |  | Show tracked files (the default) |
| `-s, --stage` |  | Show mode, object id and stage for each tracked file |
| `-o, --others` |  | Show untracked files (with ignored ones unless --exclude-standard) |
| `-i, --ignored` |  | With -o or -c and --exclude-standard, show only ignored files |
| `--exclude-standard` |  | Honor .gitignore, .git/info/exclude and core.excludesFile |
| `-m, --modified` |  | Show files changed in the working tree (deleted ones too) |
| `-d, --deleted` |  | Show files deleted from the working tree |
| `-u, --unmerged` |  | Show only unmerged files, with -s's format |
| `-z` |  | End each entry with NUL instead of a newline |
| `--full-name` |  | Print paths from the top level, not from the current folder |
| `--error-unmatch` |  | Fail if a path matches no file |
| `-t` |  | Tag each file with its status: H cached, S skip-worktree, M unmerged, R removed, C changed, ? other |
| `-v` |  | Like -t, with lowercase tags for assume-unchanged files |

See the [global options](global-options.md) that every command accepts.
