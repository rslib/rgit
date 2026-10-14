# rgit stash

Stash management (no subcommand stashes the working tree, taking `stash push`'s flags)

## Usage

```sh
rgit stash [OPTIONS] [-- <PATH>...]
       rgit stash <COMMAND>
```

## Examples

```sh
rgit stash
rgit stash -u
rgit stash push -m "<message>"
```

## Arguments

| Argument | Description |
| --- | --- |
| `[PATH]...` | Stash only these paths (after `--`, as in git) |

## Options

| Option | Value | Description |
| --- | --- | --- |
| `-m, --message` | `MESSAGE` | A description for the stash (git's -m) |
| `-u, --include-untracked` |  | Also stash untracked files (git's -u) |
| `-a, --all` |  | Also stash ignored and untracked files (git's -a) |
| `-k, --keep-index` |  | Leave the staged changes in the index as well (git's -k) |
| `--no-keep-index` |  | With -p, reset the index too |
| `-S, --staged` |  | Stash only the staged changes, taking them out of the index and working tree (git's -S) |
| `-p, --patch` |  | Pick the hunks to stash, one by one (needs a terminal; git's -p) |
| `--pathspec-from-file` | `FILE` | Read the pathspecs from this file, one per line (`-` for stdin) |
| `--pathspec-file-nul` |  | With --pathspec-from-file, the pathspecs are NUL-separated |
| `-q, --quiet` |  | Accepted for git compatibility |

## Subcommands

### rgit stash push

Stash the working tree, or only some paths, with an optional message

#### Usage

```sh
rgit stash push [OPTIONS] [PATHS]...
```

#### Examples

```sh
rgit stash push -m "<message>" --include-untracked
rgit stash push -- src/lib.rs
rgit stash push --staged -m "<message>"
rgit stash push -p
```

#### Arguments

| Argument | Description |
| --- | --- |
| `[PATHS]...` | Stash only these paths: files, folders or globs |

#### Options

| Option | Value | Description |
| --- | --- | --- |
| `-m, --message` | `MESSAGE` | A description for the stash (git's -m) |
| `-u, --include-untracked` |  | Also stash untracked files (git's -u) |
| `-a, --all` |  | Also stash ignored and untracked files (git's -a) |
| `-k, --keep-index` |  | Leave the staged changes in the index as well (git's -k) |
| `--no-keep-index` |  | With -p, reset the index too |
| `-S, --staged` |  | Stash only the staged changes, taking them out of the index and working tree (git's -S) |
| `-p, --patch` |  | Pick the hunks to stash, one by one (needs a terminal; git's -p) |
| `--pathspec-from-file` | `FILE` | Read the pathspecs from this file, one per line (`-` for stdin) |
| `--pathspec-file-nul` |  | With --pathspec-from-file, the pathspecs are NUL-separated |
| `-q, --quiet` |  | Accepted for git compatibility |

### rgit stash pop

Apply a stash and drop it (prompted for if no index on a terminal)

#### Usage

```sh
rgit stash pop [OPTIONS] [INDEX]
```

#### Examples

```sh
rgit stash pop
rgit stash pop stash@{1} --index
```

#### Arguments

| Argument | Description |
| --- | --- |
| `[INDEX]` | The stash: `N` or `stash@{N}` (defaults to the most recent) |

#### Options

| Option | Value | Description |
| --- | --- | --- |
| `--index` |  | Restore the staged changes too (git's --index) |
| `-q, --quiet` |  | Print nothing on success |

### rgit stash apply

Apply a stash without dropping it

#### Usage

```sh
rgit stash apply [OPTIONS] [INDEX]
```

#### Examples

```sh
rgit stash apply
```

#### Arguments

| Argument | Description |
| --- | --- |
| `[INDEX]` | The stash: `N` or `stash@{N}` (defaults to the most recent) |

#### Options

| Option | Value | Description |
| --- | --- | --- |
| `--index` |  | Restore the staged changes too (git's --index) |
| `-q, --quiet` |  | Print nothing on success |

### rgit stash drop

Drop a stash

#### Usage

```sh
rgit stash drop [OPTIONS] [INDEX]
```

#### Examples

```sh
rgit stash drop 0
```

#### Arguments

| Argument | Description |
| --- | --- |
| `[INDEX]` | The stash: `N` or `stash@{N}` (defaults to the most recent) |

#### Options

| Option | Value | Description |
| --- | --- | --- |
| `-q, --quiet` |  | Print nothing on success |

### rgit stash list

List the stashes, as `git log -g refs/stash` in the `%gd: %gs` format unless another is given (`%gd`, `%gD` and `%gs` name the entry)

#### Usage

```sh
rgit stash list [OPTIONS]
```

#### Examples

```sh
rgit stash list
rgit stash list --format="%gd %cr %gs" -n 5
rgit stash list --oneline
rgit stash list -p
```

#### Options

| Option | Value | Description |
| --- | --- | --- |
| `--format` | `FORMAT` | Print commits in git's format: oneline, short, medium, full, fuller, raw, reference, or `format:&lt;string&gt;` / `tformat:&lt;string&gt;` with placeholders such as %H %h %s %b %an %ae %ad %ar %d %n |
| `--pretty` | `FORMAT` | The same as --format; a bare `--pretty` is medium |
| `--oneline` |  | git's `&lt;short sha&gt; &lt;subject&gt;` lines |
| `--date` | `STYLE` | Dates in the formats: default, relative, local, iso, iso-strict, rfc, short, raw, unix or format:&lt;strftime&gt; (`-local` for local time) |
| `--graph` |  | Draw the commit graph in ASCII, as git does |
| `--decorate` | `STYLE` | Name the refs at each commit (default: only on a terminal) |
| `--no-decorate` |  | Never name refs at commits |
| `--abbrev-commit` |  | Abbreviate the commit ids in the format's header |
| `--show-signature` |  | Check each commit's signature and show the verifier's report |
| `--notes` | `REF` | Show the notes of the default notes refs, or of this notes ref (repeatable; any format) |
| `--no-notes` |  | Show no notes (a later --notes=&lt;ref&gt; still shows that ref) |
| `-p, --patch` |  | Print the full unified patch (git's -p) |
| `--stat` |  | Print a diffstat |
| `--name-only` |  | List only the names of changed files |
| `--name-status` |  | List changed files with a status letter (A, M, D, R...) |
| `--numstat` |  | Added and removed line counts per file, tab-separated |
| `--shortstat` |  | Only the diffstat's last line: files changed, insertions, deletions |
| `--raw` |  | git's raw format: `:&lt;modes&gt; &lt;ids&gt; &lt;status&gt;\t&lt;path&gt;` per file |
| `-n, --max-count` | `N` | Show at most N stashes |

### rgit stash show

Show the changes a stash records, as a diffstat (-p for the patch)

#### Usage

```sh
rgit stash show [OPTIONS] [INDEX]
```

#### Examples

```sh
rgit stash show
rgit stash show -p stash@{1}
rgit stash show -u --name-only
```

#### Arguments

| Argument | Description |
| --- | --- |
| `[INDEX]` | The stash: `N` or `stash@{N}` (defaults to the most recent) |

#### Options

| Option | Value | Description |
| --- | --- | --- |
| `-p, --patch` |  | Print the full unified patch (git's -p) |
| `--name-only` |  | List only the names of changed files |
| `--name-status` |  | List the changed files with their status letter |
| `--numstat` |  | Show added and deleted line counts per file |
| `--stat` |  | Show a diffstat (the default) |
| `-u, --include-untracked` |  | Show the stashed untracked files too (git's -u) |
| `--only-untracked` |  | Show only the stashed untracked files |
| `--patch-with-stat` |  | The diffstat, then the patch |
| `--no-renames` |  | Do not detect renames |
| `-M, --find-renames` | `N` | Detect renames, optionally at this similarity (git's -M[&lt;n&gt;]) |
| `-C, --find-copies` | `N` | Detect copies as well as renames (git's -C[&lt;n&gt;]) |

### rgit stash create

Make a stash commit of the local changes and print its id, without storing it or touching the working tree (git's `stash create`)

#### Usage

```sh
rgit stash create [OPTIONS] [MESSAGE]...
```

#### Examples

```sh
rgit stash create
rgit stash create "<message>"
```

#### Arguments

| Argument | Description |
| --- | --- |
| `[MESSAGE]...` | The stash message |

### rgit stash store

Put a stash commit (from `stash create`) on the stash list

#### Usage

```sh
rgit stash store [OPTIONS] <COMMIT>
```

#### Examples

```sh
rgit stash store -m "<message>" <commit>
```

#### Arguments

| Argument | Description |
| --- | --- |
| `<COMMIT>` | The stash commit |

#### Options

| Option | Value | Description |
| --- | --- | --- |
| `-m, --message` | `MESSAGE` | The message for the stash list |
| `-q, --quiet` |  | Accepted for git compatibility |

### rgit stash branch

Create and check out a branch at the stash's base commit, apply the stash there and drop it

#### Usage

```sh
rgit stash branch [OPTIONS] <NAME> [INDEX]
```

#### Examples

```sh
rgit stash branch <branch>
```

#### Arguments

| Argument | Description |
| --- | --- |
| `<NAME>` | The new branch name |
| `[INDEX]` | The stash: `N` or `stash@{N}` (defaults to the most recent) |

### rgit stash clear

Drop every stash

#### Usage

```sh
rgit stash clear [OPTIONS]
```

#### Examples

```sh
rgit stash clear
```

See the [global options](global-options.md) that every command accepts.
