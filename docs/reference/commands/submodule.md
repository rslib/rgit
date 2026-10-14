# rgit submodule

Submodules: status (the default), add, init, update, sync, deinit, foreach, summary, set-url, set-branch and absorbgitdirs

## Usage

```sh
rgit submodule [OPTIONS] [COMMAND]
```

## Examples

```sh
rgit submodule
rgit submodule update --init --recursive
```

## Subcommands

### rgit submodule status

Show each submodule's commit, path and name for it: ` ` in step, `-` not initialized, `+` another commit checked out, `U` conflicts

#### Usage

```sh
rgit submodule status [OPTIONS] [PATHS]...
```

#### Examples

```sh
rgit submodule status --recursive
```

#### Arguments

| Argument | Description |
| --- | --- |
| `[PATHS]...` | Only these submodule paths |

#### Options

| Option | Value | Description |
| --- | --- | --- |
| `--cached` |  | Show the commit the superproject records instead of the checked-out one |
| `--recursive` |  | Include nested submodules |
| `-q, --quiet` |  | Print nothing unless an error occurs |

### rgit submodule add

Clone URL into PATH (default: the URL's name) and add it as a submodule

#### Usage

```sh
rgit submodule add [OPTIONS] <URL> [PATH]
```

#### Examples

```sh
rgit submodule add https://github.com/example/lib.git vendor/lib
rgit submodule add -b main https://github.com/example/lib.git
```

#### Arguments

| Argument | Description |
| --- | --- |
| `<URL>` | The repository to add |
| `[PATH]` | Where to put it |

#### Options

| Option | Value | Description |
| --- | --- | --- |
| `-b, --branch` | `BRANCH` | Check out and track this branch |
| `--name` | `NAME` | Name the submodule this instead of its path |
| `-q, --quiet` |  | Print nothing unless an error occurs |

### rgit submodule init

Register submodules in .git/config from .gitmodules

#### Usage

```sh
rgit submodule init [OPTIONS] [PATHS]...
```

#### Examples

```sh
rgit submodule init
```

#### Arguments

| Argument | Description |
| --- | --- |
| `[PATHS]...` | Only these submodule paths |

#### Options

| Option | Value | Description |
| --- | --- | --- |
| `-q, --quiet` |  | Print nothing unless an error occurs |

### rgit submodule update

Clone missing submodules and check out the commits the superproject records

#### Usage

```sh
rgit submodule update [OPTIONS] [PATHS]...
```

#### Examples

```sh
rgit submodule update --init --recursive
rgit submodule update --remote vendor/lib
```

#### Arguments

| Argument | Description |
| --- | --- |
| `[PATHS]...` | Only these submodule paths |

#### Options

| Option | Value | Description |
| --- | --- | --- |
| `--init` |  | Initialize submodules that are not yet |
| `--recursive` |  | Update nested submodules too |
| `--remote` |  | Check out the submodule's remote-tracking branch instead |
| `--checkout` |  | Check out detached (the default; accepted for git compatibility) |
| `-q, --quiet` |  | Print nothing unless an error occurs |
| `-j, --jobs` | `N` | Clone and check out this many submodules at once |

### rgit submodule sync

Copy submodule URLs from .gitmodules into the config and the submodules

#### Usage

```sh
rgit submodule sync [OPTIONS] [PATHS]...
```

#### Examples

```sh
rgit submodule sync --recursive
```

#### Arguments

| Argument | Description |
| --- | --- |
| `[PATHS]...` | Only these submodule paths |

#### Options

| Option | Value | Description |
| --- | --- | --- |
| `--recursive` |  | Sync nested submodules too |
| `-q, --quiet` |  | Print nothing unless an error occurs |

### rgit submodule deinit

Unregister submodules and empty their folders

#### Usage

```sh
rgit submodule deinit [OPTIONS] [PATHS]...
```

#### Examples

```sh
rgit submodule deinit vendor/lib
rgit submodule deinit -f --all
```

#### Arguments

| Argument | Description |
| --- | --- |
| `[PATHS]...` | Only these submodule paths |

#### Options

| Option | Value | Description |
| --- | --- | --- |
| `-f, --force` |  | Discard local changes in them |
| `--all` |  | Every submodule (required when no path is given) |
| `-q, --quiet` |  | Print nothing unless an error occurs |

### rgit submodule foreach

Run a shell command in each checked-out submodule ($name, $sm_path, $displaypath, $sha1 and $toplevel are set)

#### Usage

```sh
rgit submodule foreach [OPTIONS] <COMMAND>...
```

#### Examples

```sh
rgit submodule foreach 'echo $sm_path $sha1'
```

#### Arguments

| Argument | Description |
| --- | --- |
| `<COMMAND>...` | The command |

#### Options

| Option | Value | Description |
| --- | --- | --- |
| `--recursive` |  | Include nested submodules |
| `-q, --quiet` |  | Do not print `Entering '&lt;path&gt;'` |

### rgit submodule summary

Summarize the commits between recorded and checked-out submodule commits

#### Usage

```sh
rgit submodule summary [OPTIONS] [ARGS]...
```

#### Examples

```sh
rgit submodule summary
```

#### Arguments

| Argument | Description |
| --- | --- |
| `[ARGS]...` | git's `submodule summary` arguments |

### rgit submodule set-url

Change a submodule's URL in .gitmodules and sync it

#### Usage

```sh
rgit submodule set-url [OPTIONS] <PATH> <URL>
```

#### Examples

```sh
rgit submodule set-url vendor/lib https://github.com/fork/lib.git
```

#### Arguments

| Argument | Description |
| --- | --- |
| `<PATH>` | The submodule's path |
| `<URL>` | The new URL |

### rgit submodule set-branch

Set (or with -d, clear) the branch `update --remote` follows

#### Usage

```sh
rgit submodule set-branch [OPTIONS] <PATH>
```

#### Examples

```sh
rgit submodule set-branch -b main vendor/lib
rgit submodule set-branch -d vendor/lib
```

#### Arguments

| Argument | Description |
| --- | --- |
| `<PATH>` | The submodule's path |

#### Options

| Option | Value | Description |
| --- | --- | --- |
| `-b, --branch` | `BRANCH` | The branch to follow |
| `-d, --default` |  | Follow the remote's default branch again |

### rgit submodule absorbgitdirs

Move submodules' repositories into the superproject's .git/modules

#### Usage

```sh
rgit submodule absorbgitdirs [OPTIONS] [PATHS]...
```

#### Examples

```sh
rgit submodule absorbgitdirs
```

#### Arguments

| Argument | Description |
| --- | --- |
| `[PATHS]...` | Only these submodule paths |

See the [global options](global-options.md) that every command accepts.
