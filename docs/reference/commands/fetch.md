# rgit fetch

Fetch the current branch's remote, or `<repository> [<refspec>...]`

## Usage

```sh
rgit fetch [OPTIONS] [REPOSITORY] [REFSPECS]...
```

## Examples

```sh
rgit fetch
rgit fetch --all
rgit fetch --remote origin --prune
rgit fetch origin main --depth 1
rgit fetch --dry-run
rgit fetch --unshallow
rgit fetch --prune --prune-tags
rgit fetch --multiple -j 4 origin upstream
rgit fetch --filter=blob:none origin
```

## Arguments

| Argument | Description |
| --- | --- |
| `[REPOSITORY]` | The remote to fetch (defaults to the branch's upstream remote) |
| `[REFSPECS]...` | Refs to fetch, as in git (`main`, `src:dst`); defaults to the remote's |

## Options

| Option | Value | Description |
| --- | --- | --- |
| `--all` |  | Fetch from every remote (git's --all) |
| `-p, --prune` |  | Delete remote-tracking refs that no longer exist upstream (--prune) |
| `--remote` | `REMOTE` | Fetch this named remote instead of the branch's upstream |
| `-t, --tags` |  | Fetch every tag too (git's --tags) |
| `--depth` | `DEPTH` | Limit history to this many commits (git's --depth) (default: `0`) |
| `--dry-run` |  | Show what would be fetched without changing any ref |
| `--multiple` |  | Take every positional as a remote to fetch (git's --multiple) |
| `--unshallow` |  | Fetch the whole history of a shallow clone |
| `--deepen` | `N` | Deepen a shallow history by N commits (default: `0`) |
| `--shallow-since` | `DATE` | Deepen a shallow history to the commits after DATE |
| `-P, --prune-tags` |  | Fetch every tag and, with --prune, delete local tags gone upstream |
| `-n, --no-tags` |  | Follow no tags |
| `-f, --force` |  | Update refs even when that is not a fast-forward |
| `--refmap` | `REFSPEC` | Map the refspecs given through REFSPEC instead of the configured ones; `--refmap=` updates no remote-tracking ref |
| `--set-upstream` |  | Record the fetched branch as the current branch's upstream |
| `-q, --quiet` |  | Print nothing unless an error occurs |
| `-v, --verbose` |  | Report more (accepted for git compatibility) |
| `-j, --jobs` | `N` | Fetch up to N remotes at once (default: `fetch.parallel`) |
| `-a, --append` |  | Add to FETCH_HEAD instead of replacing it |
| `--filter` | `SPEC` | Leave out objects, e.g. `blob:none` or `tree:0`, and make the remote a promisor that later reads fetch them from |

See the [global options](global-options.md) that every command accepts.
