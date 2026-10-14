# rgit push

Push the current branch to its upstream, or `<repository> [<refspec>...]`

## Usage

```sh
rgit push [OPTIONS] [REPOSITORY] [REFSPECS]...
```

## Examples

```sh
rgit push
rgit push --set-upstream
rgit push --force-with-lease
rgit push origin feature
rgit push origin local:remote
rgit push origin --delete feature
rgit push origin :v1
rgit push --all --dry-run
rgit push --follow-tags
rgit push --atomic origin main v1
rgit push --porcelain origin main
rgit push --recurse-submodules=on-demand origin main
```

## Arguments

| Argument | Description |
| --- | --- |
| `[REPOSITORY]` | The remote to push to (defaults to the branch's upstream remote) |
| `[REFSPECS]...` | Refs to push, as in git: `branch`, `src:dst`, `:branch` (delete), `+src:dst` (force) |

## Options

| Option | Value | Description |
| --- | --- | --- |
| `-f, --force` |  | Overwrite the remote branch unconditionally (dangerous) |
| `--force-with-lease` |  | Overwrite the remote branch only if it still matches our tracking ref |
| `-u, --set-upstream` |  | Record the pushed branch as the upstream |
| `--remote` | `REMOTE` | Push to this named remote instead of the branch's upstream |
| `--tags` |  | Push all local tags (git's --tags) |
| `--all` |  | Push every local branch (git's --all) |
| `-d, --delete` |  | Delete the named branches on the remote (git's --delete) |
| `-n, --dry-run` |  | Show what would be pushed without sending anything |
| `--follow-tags` |  | Also push annotated tags that point into the pushed history (default: `push.followTags`) |
| `--atomic` |  | Push every ref or none |
| `--prune` |  | With --all or --tags, delete remote refs no local ref maps to |
| `--mirror` |  | Make every remote ref match the local one: force-updates and deletes |
| `-o, --push-option` | `OPTION` | Pass OPTION to the server's hooks (repeatable) |
| `--no-verify` |  | Skip the pre-push hook |
| `--porcelain` |  | Report in git's machine-readable format |
| `--recurse-submodules` | `MODE` | Submodule commits the pushed history records: `check` refuses unpushed ones, `on-demand` pushes them first, `only` pushes just them, `no` ignores them (default: `push.recurseSubmodules`) |
| `-q, --quiet` |  | Print nothing unless an error occurs |
| `-v, --verbose` |  | Also list refs that are already up to date |

See the [global options](global-options.md) that every command accepts.
