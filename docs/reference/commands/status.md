# rgit status

Working-tree status as git prints it (`--compact` for rgit's short form). In human mode `--porcelain`, `--short`, `--branch`, `-z`, `-v` and git's other status flags print git's own formats byte for byte, for scripts. Under an agent harness `--porcelain` and `-z` still do

## Usage

```sh
rgit status [OPTIONS] [PATHS]...
```

## Examples

```sh
rgit status
rgit status --toon
rgit status --long
rgit status -sb
rgit status --porcelain=v2 -b --show-stash
rgit status -vv -uall --ignored=matching
```

## Arguments

| Argument | Description |
| --- | --- |
| `[PATHS]...` | Limit to these paths: files, folders or globs |

## Options

| Option | Value | Description |
| --- | --- | --- |
| `--porcelain` | `VERSION` | git's script format, `v1` (default) or `v2`. Agents should prefer `--toon` |
| `-s, --short` |  | git's short format |
| `--long` |  | git's long format (git's default) |
| `-b, --branch` |  | Add branch and tracking info |
| `-z, --null` |  | Terminate entries with NUL |
| `-v, --verbose` |  | Also show the staged diff; repeat for the unstaged one |
| `--show-stash` |  | Show the number of stashes |
| `--ahead-behind` |  | Count commits ahead of and behind the upstream (git's default) |
| `--no-ahead-behind` |  | Only say whether the branch differs from its upstream |
| `--ignore-submodules` | `WHEN` | Ignore submodule changes: `none`, `untracked`, `dirty` or `all` (default) |
| `--column` | `OPTIONS` | List untracked files in columns (git's column.status options) |
| `--no-renames` |  | Do not detect renames |
| `-M, --find-renames` | `N` | Detect renames, optionally at this similarity (git's -M) |
| `-u, --untracked-files` | `MODE` | Show untracked files: `no`, `normal` or `all` (git's -u, default all) |
| `--ignored` | `MODE` | Also list ignored files: `traditional` (default), `matching` or `no` |

See the [global options](global-options.md) that every command accepts.
