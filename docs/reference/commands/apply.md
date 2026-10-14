# rgit apply

Apply a patch to the working tree, the index, or both

## Usage

```sh
rgit apply [OPTIONS] [PATCHES]...
```

## Examples

```sh
rgit apply fix.patch
rgit apply --check fix.patch
rgit apply --cached fix.patch
rgit apply -R fix.patch
rgit apply --3way fix.patch
rgit apply --reject -p2 --directory=vendor/lib fix.patch
rgit apply --stat --summary --numstat fix.patch
rgit apply -C1 --ignore-whitespace fix.patch
rgit apply -N new-files.patch
```

## Arguments

| Argument | Description |
| --- | --- |
| `[PATCHES]...` | Patch files (reads stdin when none or `-`) |

## Options

| Option | Value | Description |
| --- | --- | --- |
| `--cached` |  | Apply to the index only, leaving the working tree as it is |
| `--index` |  | Apply to both the index and the working tree |
| `--check` |  | Only check that the patch applies |
| `-R, --reverse` |  | Undo the patch (apply it in reverse) |
| `--stat` |  | Print the patch's diffstat instead of applying it |
| `--numstat` |  | Print added and deleted line counts per file instead of applying it |
| `--summary` |  | Print created, deleted, renamed and mode-changed files instead of applying it |
| `--apply` |  | Apply even with --stat, --numstat or --summary |
| `-3, --3way` |  | Fall back to a three-way merge with the patch's preimage blobs |
| `--reject` |  | Apply the hunks that fit and write the others to `&lt;file&gt;.rej` |
| `-p` | `N` | Strip this many leading path components (default 1) |
| `--directory` | `ROOT` | Prepend this folder to every path in the patch |
| `--include` | `GLOB` | Apply only to paths matching this glob |
| `--exclude` | `GLOB` | Skip paths matching this glob |
| `--whitespace` | `ACTION` | On trailing whitespace in added lines: nowarn, warn, fix, error or error-all |
| `-v, --verbose` |  | Report each file as it is checked and applied |
| `--allow-empty` |  | A patch that changes no files is not an error |
| `--ours` |  | With --3way, resolve conflicts to our side |
| `--theirs` |  | With --3way, resolve conflicts to their side |
| `--union` |  | With --3way, keep both sides of conflicts |
| `--recount` |  | Count hunk lines instead of trusting the `@@` headers (hand-edited patches) |
| `-q, --quiet` |  | Print no progress |
| `-C` | `N` | Drop context down to N lines around each hunk when it does not match otherwise |
| `--unidiff-zero` |  | Apply hunks without context where their header says (`diff -U0`) |
| `--ignore-whitespace` |  | Ignore changes in the amount of whitespace when matching context |
| `--inaccurate-eof` |  | Tolerate a patch missing newlines at the ends of files |
| `--allow-overlap` |  | Let hunks overlap lines earlier hunks changed |
| `-N, --intent-to-add` |  | Mark new files intent-to-add in the index (working-tree apply only) |
| `--build-fake-ancestor` | `FILE` | Write an index of the preimage blobs to this file instead of applying |
| `-z` |  | With --numstat, end each entry with NUL instead of a newline |

See the [global options](global-options.md) that every command accepts.
