# rgit show

A commit as git shows it (`--compact` for header and diffstat), or a file (`rev:path`) or folder at a revision

## Usage

```sh
rgit show [OPTIONS] [REV]... [-- <PATH>...]
```

## Examples

```sh
rgit show HEAD
rgit show <rev> --patch
rgit show <rev> --name-only
rgit show <rev> --pretty=fuller --stat
rgit show HEAD~1:src/lib.rs
rgit show <merge> --remerge-diff --format=medium
rgit show <merge> -m --stat --oneline
```

## Arguments

| Argument | Description |
| --- | --- |
| `[REV]...` | The commits to show (branch, tag, or sha; default HEAD), or `rev:path` to print a file as it is at `rev` (`:path` for the staged version) |
| `[PATH]...` | Limit the listed changes to these paths (after `--`) |

## Options

| Option | Value | Description |
| --- | --- | --- |
| `-p, --patch` |  | Print the full unified patch (git's -p) |
| `--stat` |  | Print a diffstat |
| `--name-only` |  | List only the names of changed files |
| `--name-status` |  | List changed files with a status letter (A, M, D, R...) |
| `--numstat` |  | Added and removed line counts per file, tab-separated |
| `--shortstat` |  | Only the diffstat's last line: files changed, insertions, deletions |
| `--raw` |  | git's raw format: `:&lt;modes&gt; &lt;ids&gt; &lt;status&gt;\t&lt;path&gt;` per file |
| `-W, --function-context` |  | Show the whole function around each change as context (git's -W) |
| `--word-diff` | `MODE` | Diff by words: `plain` (`[-old-]{+new+}`, the default), `color`, `porcelain` or `none` |
| `--word-diff-regex` | `REGEX` | What a word is for --word-diff (implies it): an extended regex |
| `--color-words` | `REGEX` | Word diff in color only (git's --word-diff=color), words matching the optional regex |
| `--minimal` |  | Spend extra time to find the smallest diff |
| `--patience` |  | Diff with the patience algorithm |
| `--histogram` |  | Diff with the histogram algorithm |
| `--anchored` | `TEXT` | Keep lines starting with TEXT unchanged if possible (patience diff) |
| `--diff-algorithm` | `ALGORITHM` | myers (default), minimal, patience or histogram |
| `--inter-hunk-context` | `N` | Join hunks up to N lines apart |
| `--relative` | `PATH` | Only changes under PATH (default: the current folder), named from it |
| `--no-relative` |  | Undo --relative and diff.relative |
| `--src-prefix` | `PREFIX` | The old side's prefix instead of `a/` |
| `--dst-prefix` | `PREFIX` | The new side's prefix instead of `b/` |
| `--no-prefix` |  | No `a/` and `b/` prefixes |
| `--default-prefix` |  | `a/` and `b/`, whatever diff.noprefix or diff.mnemonicPrefix say |
| `--line-prefix` | `PREFIX` | Start every output line with PREFIX |
| `--output` | `FILE` | Write the output to FILE instead of stdout |
| `--ext-diff` |  | Run external diff tools (GIT_EXTERNAL_DIFF, diff.external, diff.&lt;driver&gt;.command) |
| `--no-ext-diff` |  | Never run external diff tools |
| `--textconv` |  | Diff files through their diff.&lt;driver&gt;.textconv filters (the default) |
| `--no-textconv` |  | Diff files as they are stored, without textconv filters |
| `--binary` |  | A binary patch for binary files, applicable with `git apply` |
| `--full-index` |  | Full object names on `index` lines |
| `--break-rewrites` | `N[/M]` | Show rewritten files as a whole removal and addition (git's -B[n][/m]) |
| `-D, --irreversible-delete` |  | Leave the removed lines out of a deleted file's patch |
| `--rename-limit` | `N` | Give up rename detection past N files (git's -l&lt;n&gt;) |
| `--submodule` | `FORMAT` | How submodule changes show: short, log or diff |
| `-X, --dirstat` | `PARAM,...` | Share of changes per folder: changes, lines, files, cumulative, &lt;limit%&gt; |
| `--dirstat-by-file` | `PARAM,...` | --dirstat=files |
| `--cumulative` |  | --dirstat=cumulative |
| `--compact-summary` |  | A diffstat that also marks new, gone and mode-changed files |
| `--stat-width` | `N` | The diffstat's total width |
| `--stat-name-width` | `N` | The diffstat's file name width |
| `--stat-count` | `N` | List only the first N files in the diffstat |
| `--stat-graph-width` | `N` | The diffstat's graph width |
| `--check` |  | Report whitespace errors and conflict markers instead of a patch |
| `--color-moved` | `MODE` | Color moved lines: no, default, plain, blocks, zebra or dimmed-zebra |
| `--no-color-moved` |  | Turn --color-moved off |
| `--color-moved-ws` | `MODES` | Whitespace moved lines may differ in: ignore-space-at-eol, ignore-space-change, ignore-all-space, allow-indentation-change, no |
| `--no-color-moved-ws` |  | Turn --color-moved-ws off |
| `--ws-error-highlight` | `KIND` | Which lines show whitespace errors: context, old, new, all, none, default |
| `--no-renames` |  | Do not detect renames |
| `-M, --find-renames` | `N` | Detect renames, optionally at this similarity (git's -M[&lt;n&gt;]) |
| `-C, --find-copies` | `N` | Detect copies as well as renames (git's -C[&lt;n&gt;]) |
| `-m` |  | Diff each merge against every parent in turn, once -p is given (git's -m) |
| `-c` |  | git's combined diff of merges against all parents (implies -p) |
| `--cc` |  | git's dense combined diff: only the hunks that differ from every parent (implies -p; the default for show) |
| `--dd` |  | Diff merges against their first parent (implies -p) |
| `--diff-merges` | `FORMAT` | How to diff merges: off, first-parent, separate, combined, dense-combined or remerge (implies -p unless off) |
| `--no-diff-merges` |  | Print no diff for merges |
| `--remerge-diff` |  | Diff each merge against git's own re-merge of its parents, conflict markers and all (implies -p) |
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
| `-s, --no-patch` |  | Only the header, no changed files (git's -s) |
| `--first-parent` |  | Diff a merge against its first parent only |

See the [global options](global-options.md) that every command accepts.
