# rgit clone

Clone a repository into a new directory

## Usage

```sh
rgit clone [OPTIONS] <URL> [DIR]
```

## Examples

```sh
rgit clone https://github.com/example/repo.git
rgit clone https://github.com/example/repo.git repo-dir --depth 1
rgit clone --bare https://github.com/example/repo.git
rgit clone --recurse-submodules -o upstream https://github.com/example/repo.git
rgit clone --single-branch -b main https://github.com/example/repo.git
rgit clone --mirror https://github.com/example/repo.git
rgit clone --filter=blob:none https://github.com/example/repo.git
rgit clone --shallow-since=2024-01-01 https://github.com/example/repo.git
rgit clone --reference ../other --dissociate https://github.com/example/repo.git
rgit clone --sparse https://github.com/example/repo.git
```

## Arguments

| Argument | Description |
| --- | --- |
| `<URL>` | The repository URL to clone |
| `[DIR]` | The target directory (defaults to the repository name) |

## Options

| Option | Value | Description |
| --- | --- | --- |
| `-b, --branch` | `BRANCH` | Check out this branch instead of the remote's default (git's -b) |
| `--depth` | `DEPTH` | Shallow-clone this many commits of history (git's --depth) (default: `0`) |
| `--bare` |  | Make a bare repository (git's --bare) |
| `-o, --origin` | `ORIGIN` | Name the remote this instead of `origin` (git's -o) |
| `--recurse-submodules` |  | Clone the submodules too (git's --recurse-submodules) |
| `--single-branch` |  | Fetch only the branch checked out (-b, else the remote's HEAD) |
| `--no-single-branch` |  | Fetch every branch (the default) |
| `-n, --no-checkout` |  | Leave the working tree empty |
| `--mirror` |  | A bare copy of every ref that `fetch` keeps in step with the remote |
| `--no-tags` |  | Follow no tags, now or on later fetches |
| `--reference` | `REPO` | Borrow objects from this local repository (git's alternates) |
| `--reference-if-able` | `REPO` | Like --reference, but go on without it when REPO is not a repository |
| `--bundle-uri` | `URI` | Accepted for git compatibility; the clone fetches everything itself |
| `--dissociate` |  | With --reference, copy the borrowed objects and drop the link |
| `-s, --shared` |  | Share the source's objects instead of copying them (local only) |
| `--filter` | `SPEC` | A partial clone, e.g. `blob:none` or `tree:0` |
| `--sparse` |  | Start with a sparse checkout of the top-level files only |
| `--template` | `DIR` | Copy hooks and other files from DIR into the new repository |
| `--shallow-since` | `DATE` | Shallow-clone the commits after DATE |
| `--separate-git-dir` | `GIT_DIR` | Put the repository in GIT_DIR and link it from the working tree |
| `-c, --config` | `KEY=VALUE` | Set KEY=VALUE in the new repository's config (repeatable) |
| `-q, --quiet` |  | Print nothing unless an error occurs |
| `-v, --verbose` |  | Report more (accepted for git compatibility) |
| `-j, --jobs` | `N` | Parallel submodule fetches (accepted; fetched in turn) |

See the [global options](global-options.md) that every command accepts.
