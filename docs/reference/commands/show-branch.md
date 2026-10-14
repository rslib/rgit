# rgit show-branch

Show branches side by side with the commits each has, down to where they meet, like `git show-branch`

## Usage

```sh
rgit show-branch [OPTIONS] [REVS]...
```

## Examples

```sh
rgit show-branch
rgit show-branch --more=5 main topic
rgit show-branch --merge-base main topic
rgit show-branch --reflog=4 main
```

## Arguments

| Argument | Description |
| --- | --- |
| `[REVS]...` | Branches, revisions or ref globs (default: every local branch) |

## Options

| Option | Value | Description |
| --- | --- | --- |
| `-a, --all` |  | Show local and remote-tracking branches |
| `-r, --remotes` |  | Show remote-tracking branches |
| `--current` |  | Add the current branch if it is not listed |
| `--topo-order` |  | Parents after all their children (the default) |
| `--date-order` |  | Newest first, parents still after their children |
| `--sparse` |  | Keep merges reachable from only one branch |
| `--more` | `N` | Show N more commits past the common ancestor |
| `--list` |  | List only the branches and their tips |
| `--merge-base` |  | Print the merge bases of the branches |
| `--independent` |  | Print the tips not reachable from any other |
| `--no-name` |  | Omit the names |
| `--sha1-name` |  | Name commits by abbreviated id instead of `branch~n` |
| `--topics` |  | Only commits not on the first branch |
| `-g, --reflog` | `N[,BASE]` | Show a branch's last N reflog entries (default 4), from BASE |
| `--color` | `WHEN` | Color the columns: always, never or auto |

See the [global options](global-options.md) that every command accepts.
