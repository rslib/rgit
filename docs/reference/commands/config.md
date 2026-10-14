# rgit config

Get, set, unset or list config values: `config <key>` reads, `config <key> <value>` writes

## Usage

```sh
rgit config [OPTIONS] [KEY] [VALUE] [VALUE_PATTERN]
```

## Examples

```sh
rgit config user.email
rgit config user.email me@example.com
rgit config --global pull.rebase true
rgit config --unset core.pager
rgit config --list --show-origin
rgit config --get-regexp '^remote\.'
rgit config --type=bool --default false core.bare
rgit config --file .gitmodules --list
rgit config --rename-section branch.old branch.new
rgit config get --all --show-names remote.origin.fetch
rgit config set --all core.pager less
```

## Arguments

| Argument | Description |
| --- | --- |
| `[KEY]` | The key (`section.name`); the section for --rename-section and --remove-section, a regex for --get-regexp |
| `[VALUE]` | The value to set; a value pattern with --get, --get-all, --get-regexp and --unset; the new name for --rename-section |
| `[VALUE_PATTERN]` | Only values matching this regex (`!` negates) are replaced |

## Options

| Option | Value | Description |
| --- | --- | --- |
| `--global` |  | Use the user's global config (~/.gitconfig or $GIT_CONFIG_GLOBAL) |
| `--system` |  | Use the system config (/etc/gitconfig or $GIT_CONFIG_SYSTEM) |
| `--local` |  | Use only the repository's config |
| `--worktree` |  | Use the worktree's config (config.worktree when enabled) |
| `-f, --file` | `FILE` | Use this config file |
| `--get` |  | Print the key's value (the default with just a key) |
| `--get-all` |  | Print every value of a multi-valued key |
| `--get-regexp` |  | Print every `key value` whose key matches the regex |
| `--replace-all` |  | Replace every value of a multi-valued key with one |
| `--add` |  | Add a value to a multi-valued key instead of replacing it |
| `--unset` |  | Remove the key |
| `--unset-all` |  | Remove every value of a multi-valued key |
| `--rename-section` |  | Rename a section: `--rename-section &lt;old&gt; &lt;new&gt;` |
| `--remove-section` |  | Remove a section and every key in it |
| `-l, --list` |  | List every `key=value` |
| `-e, --edit` |  | Open the config file in the editor |
| `--type` | `TYPE` | Read and write values as this type |
| `--bool` |  | Same as --type=bool |
| `--int` |  | Same as --type=int (k/m/g suffixes allowed) |
| `--bool-or-int` |  | Same as --type=bool-or-int |
| `--path` |  | Same as --type=path (expands `~/`) |
| `--expiry-date` |  | Same as --type=expiry-date |
| `--fixed-value` |  | Match the value pattern as a literal string, not a regex |
| `--show-origin` |  | Prefix each value with the file it came from |
| `--show-scope` |  | Prefix each value with its scope (system, global, local, worktree) |
| `--name-only` |  | Print only the key names (--list, --get-regexp) |
| `-z, --null` |  | End each value with NUL and put a newline between key and value |
| `--default` | `VALUE` | The value to print when the key is not set |
| `--includes` |  | Follow include.path and includeIf in reads (default only without a scope) |
| `--no-includes` |  | Do not follow include.path and includeIf in reads |
| `--all` |  | With the `get`/`set`/`unset` forms: every value (git's --all) |
| `--regexp` |  | With `get`: the key is a regex (git's --regexp) |
| `--show-names` |  | With `get`: print each key before its value |
| `--value` | `PATTERN` | With the `get`/`set`/`unset` forms: only values matching this pattern |

See the [global options](global-options.md) that every command accepts.
