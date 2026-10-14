# rgit var

Print a git variable: GIT_AUTHOR_IDENT, GIT_COMMITTER_IDENT, GIT_EDITOR, GIT_SEQUENCE_EDITOR, GIT_PAGER, GIT_DEFAULT_BRANCH, GIT_SHELL_PATH, GIT_ATTR_SYSTEM, GIT_ATTR_GLOBAL, GIT_CONFIG_SYSTEM or GIT_CONFIG_GLOBAL, like `git var`

## Usage

```sh
rgit var [OPTIONS] [NAME]
```

## Examples

```sh
rgit var GIT_AUTHOR_IDENT
rgit var GIT_EDITOR
rgit var -l
```

## Arguments

| Argument | Description |
| --- | --- |
| `[NAME]` | The variable |

## Options

| Option | Value | Description |
| --- | --- | --- |
| `-l` |  | Print the config, then every variable as `NAME=value` |

See the [global options](global-options.md) that every command accepts.
