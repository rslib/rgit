# rgit blame

Blame a file in git's format, with its display flags (`-t`, `--date`, `-f`, `-n`, `-c`, `--root`...); `--compact` for `sha author line`

## Usage

```sh
rgit blame [OPTIONS] <REV_OR_PATH>...
```

## Examples

```sh
rgit blame src/lib.rs
rgit blame src/lib.rs -L 10,20
rgit blame v1 -- src/lib.rs
rgit blame --porcelain -L 10,20 src/lib.rs
rgit blame -w -C -L :main src/lib.rs
rgit blame --ignore-rev <rev> --date=short src/lib.rs
```

## Arguments

| Argument | Description |
| --- | --- |
| `<REV_OR_PATH>...` | `[REV] PATH`: the file to annotate, as it is in the working tree or at REV (`rgit blame &lt;rev&gt; -- &lt;path&gt;` also works); `A..B` or `^A` stops at A |

## Options

| Option | Value | Description |
| --- | --- | --- |
| `-L` | `START,END` | Limit to line ranges (repeatable): `START,END`, `START,+COUNT`, `/regex/`, `/regex/,+COUNT` or `:funcname` (git's -L) |
| `-p, --porcelain` |  | git's machine-readable format, each commit's details given once |
| `--line-porcelain` |  | git's machine-readable format with the details on every line |
| `-e, --show-email` |  | The author's email instead of the name (git's -e) |
| `-s` |  | Leave out the author (git's -s) |
| `-l` |  | Full commit ids (git's -l) |
| `-t` |  | git's format with raw timestamps (git's -t) |
| `--date` | `STYLE` | git's format with dates in this style (default iso, or blame.date) |
| `-f, --show-name` |  | git's format with each line's file name (git's -f) |
| `-n, --show-number` |  | git's format with each line's number in its commit (git's -n) |
| `-c` |  | git annotate's format (git's -c) |
| `-b` |  | Blank ids for boundary commits (git's -b) |
| `--abbrev` | `N` | git's format with ids this many digits long |
| `--show-stats` |  | git's format, then the work done: blobs read, patches, commits |
| `--incremental` |  | git's machine format, each group of lines as blame settles it |
| `-w` |  | Ignore whitespace when comparing a commit with its parents (git's -w) |
| `-M` |  | Find lines moved or copied within the file (git's -M[score]) |
| `-C` |  | Find lines copied from files changed in the same commit (git's -C[score]); twice: from any file when the file is created; three times: from any file in any commit |
| `--ignore-rev` | `REV` | Pass this revision's changes through to the lines before them |
| `--ignore-revs-file` | `FILE` | Ignore the revisions listed in this file (after blame.ignoreRevsFile; an empty name forgets the files before it) |
| `--reverse` |  | For each line, the last commit it was still in (walk `A..B` forward) |
| `--first-parent` |  | Follow only the first parent of merges |
| `--root` |  | Blame root commits too, not as boundaries (and print git's format) |
| `--contents` | `FILE` | Blame this file's contents (`-` for stdin) as the working tree's version, on top of the revision given (default HEAD) |
| `--progress` |  | Report progress on a terminal (accepted; blame runs in-process) |
| `--encoding` | `ENCODING` | The encoding of author names and summaries (UTF-8, or `none`) |

See the [global options](global-options.md) that every command accepts.
