# rgit diff

Patch of unstaged changes (`--cached` for staged), against a revision, or between two revisions (`A B`, `A..B`, `A...B`); `--compact` for a diffstat

## Usage

```sh
rgit diff [OPTIONS] [REV_OR_PATH]... [-- <PATH>...]
```

## Examples

```sh
rgit diff
rgit diff --cached -- src
rgit diff main...HEAD --name-status
rgit diff v1 HEAD --patch -U1 -w -- src
rgit diff --quiet && echo clean
rgit diff main --name-only --diff-filter=A
rgit diff --no-index a.txt b.txt --patch
rgit diff --patch --word-diff -W -- src/lib.rs
```

## Arguments

| Argument | Description |
| --- | --- |
| `[REV_OR_PATH]...` | Revisions (none: the working tree against the index; one: it against the working tree; `A B`, `A..B`, or `A...B` from their merge base), then paths |
| `[PATH]...` | Limit to these paths (after `--`) |

## Options

| Option | Value | Description |
| --- | --- | --- |
| `--cached` |  | Diff the staged changes (index vs HEAD), like git's --cached |
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
| `-U, --unified` | `N` | Lines of context around each change (git's -U, default 3) |
| `-w, --ignore-all-space` |  | Ignore whitespace when comparing lines |
| `-b, --ignore-space-change` |  | Ignore changes in the amount of whitespace |
| `--exit-code` |  | Exit 1 when there are differences, 0 when there are none |
| `--quiet` |  | Print nothing; only exit 1 when there are differences |
| `--no-index` |  | Compare two files on disk (`--no-index &lt;a&gt; &lt;b&gt;`); exits 1 when they differ |
| `-R` |  | Swap the two sides (git's -R) |
| `--diff-filter` | `ACDMRT` | Only files whose status letter is in this set (`AM`); lowercase letters leave those out (`d`), as git's --diff-filter |

See the [global options](global-options.md) that every command accepts.
