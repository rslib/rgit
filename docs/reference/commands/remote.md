# rgit remote

Remote management (no subcommand lists remotes)

## Usage

```sh
rgit remote [OPTIONS]
       rgit remote <COMMAND>
```

## Examples

```sh
rgit remote
rgit remote -v
rgit remote add origin https://github.com/example/repo.git
```

## Options

| Option | Value | Description |
| --- | --- | --- |
| `-v, --verbose` |  | List each remote's fetch and push URLs (git's -v) |

## Subcommands

### rgit remote add

Add a remote

#### Usage

```sh
rgit remote add [OPTIONS] <NAME> <URL>
```

#### Examples

```sh
rgit remote add origin https://github.com/example/repo.git
rgit remote add -f -t main upstream https://github.com/example/repo.git
```

#### Arguments

| Argument | Description |
| --- | --- |
| `<NAME>` | The remote name |
| `<URL>` | The remote URL |

#### Options

| Option | Value | Description |
| --- | --- | --- |
| `-f, --fetch` |  | Fetch the remote right after adding it (git's -f) |
| `-t, --track` | `BRANCH` | Track only this branch; repeat for more (git's -t) |
| `-m, --master` | `BRANCH` | Point `&lt;name&gt;/HEAD` at this branch (git's -m) |
| `--mirror` | `fetch\|push` | Mirror the remote: `fetch` copies every ref as is, `push` pushes every ref; both without a value |
| `--tags` |  | Fetch every tag from it |
| `--no-tags` |  | Fetch no tags from it |

### rgit remote remove

Remove a remote

#### Usage

```sh
rgit remote remove [OPTIONS] <NAME>
```

#### Examples

```sh
rgit remote remove origin
```

#### Arguments

| Argument | Description |
| --- | --- |
| `<NAME>` | The remote name |

### rgit remote set-url

Change a remote's URL, add one (--add) or delete those matching a regex (--delete)

#### Usage

```sh
rgit remote set-url [OPTIONS] <NAME> <URL> [OLD]
```

#### Examples

```sh
rgit remote set-url origin https://github.com/example/repo.git
rgit remote set-url --push origin https://github.com/fork/repo.git
rgit remote set-url --add --push origin https://github.com/mirror/repo.git
```

#### Arguments

| Argument | Description |
| --- | --- |
| `<NAME>` | The remote name |
| `<URL>` | The new URL; with --delete, a regex of the URLs to delete |
| `[OLD]` | Replace only the URL matching this regex |

#### Options

| Option | Value | Description |
| --- | --- | --- |
| `--push` |  | Set the push URL instead (git's --push) |
| `--add` |  | Add the URL, keeping the others |
| `--delete` |  | Delete the URLs matching the regex |

### rgit remote get-url

Print a remote's URL

#### Usage

```sh
rgit remote get-url [OPTIONS] <NAME>
```

#### Examples

```sh
rgit remote get-url origin
```

#### Arguments

| Argument | Description |
| --- | --- |
| `<NAME>` | The remote name |

#### Options

| Option | Value | Description |
| --- | --- | --- |
| `--push` |  | Print the push URL instead (git's --push) |
| `--all` |  | Print every URL, not just the first (git's --all) |

### rgit remote rename

Rename a remote, its tracking refs and the branches that track it

#### Usage

```sh
rgit remote rename [OPTIONS] <OLD> <NEW>
```

#### Examples

```sh
rgit remote rename origin upstream
```

#### Arguments

| Argument | Description |
| --- | --- |
| `<OLD>` | The current remote name |
| `<NEW>` | The new remote name |

### rgit remote prune

Delete remote-tracking branches that no longer exist on the remotes

#### Usage

```sh
rgit remote prune [OPTIONS] <NAMES>...
```

#### Examples

```sh
rgit remote prune origin
rgit remote prune -n origin
```

#### Arguments

| Argument | Description |
| --- | --- |
| `<NAMES>...` | The remotes to prune |

#### Options

| Option | Value | Description |
| --- | --- | --- |
| `-n, --dry-run` |  | Only list what would be pruned (git's -n) |

### rgit remote show

Describe remotes as `git remote show` does: URLs, HEAD branch, remote branches, and the local branches that pull from and push to them

#### Usage

```sh
rgit remote show [OPTIONS] [NAMES]...
```

#### Examples

```sh
rgit remote show origin
rgit remote show -n
```

#### Arguments

| Argument | Description |
| --- | --- |
| `[NAMES]...` | The remotes (all when none) |

#### Options

| Option | Value | Description |
| --- | --- | --- |
| `-n` |  | Do not query the remotes (git's -n) |

### rgit remote update

Fetch remotes: all by default, or the named remotes and groups (`remotes.<group>`)

#### Usage

```sh
rgit remote update [OPTIONS] [NAMES]...
```

#### Examples

```sh
rgit remote update
rgit remote update -p <group>
```

#### Arguments

| Argument | Description |
| --- | --- |
| `[NAMES]...` | Remotes or groups to fetch |

#### Options

| Option | Value | Description |
| --- | --- | --- |
| `-p, --prune` |  | Prune stale remote-tracking branches too (git's -p) |

### rgit remote set-head

Set `<name>/HEAD`: to a branch, from the remote (-a), or delete it (-d)

#### Usage

```sh
rgit remote set-head [OPTIONS] <NAME> [BRANCH]
```

#### Examples

```sh
rgit remote set-head origin -a
rgit remote set-head origin main
```

#### Arguments

| Argument | Description |
| --- | --- |
| `<NAME>` | The remote name |
| `[BRANCH]` | The remote branch HEAD should name |

#### Options

| Option | Value | Description |
| --- | --- | --- |
| `-a, --auto` |  | Ask the remote which branch its HEAD names (git's -a) |
| `-d, --delete` |  | Delete `&lt;name&gt;/HEAD` (git's -d) |

### rgit remote set-branches

Fetch only these branches from a remote (with --add, these too)

#### Usage

```sh
rgit remote set-branches [OPTIONS] <NAME> <BRANCHES>...
```

#### Examples

```sh
rgit remote set-branches --add origin <branch>
```

#### Arguments

| Argument | Description |
| --- | --- |
| `<NAME>` | The remote name |
| `<BRANCHES>...` | The branches to fetch |

#### Options

| Option | Value | Description |
| --- | --- | --- |
| `--add` |  | Add to the branches fetched instead of replacing them |

See the [global options](global-options.md) that every command accepts.
