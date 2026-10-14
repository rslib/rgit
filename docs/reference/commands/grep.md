# rgit grep

Search tracked files, the index or a revision, like `git grep`. Paths print from the current folder, which limits the search by default

## Usage

```sh
rgit grep [OPTIONS] [PATTERN_OR_REV]... [-- <PATH>...]
```

## Examples

```sh
rgit grep -n TODO
rgit grep -i -e foo -e bar -- src
rgit grep pattern HEAD~5
rgit grep -n -C2 --heading TODO
rgit grep -e TODO --and --not -e FIXME
rgit grep -W -n parse_args -- src
```

## Arguments

| Argument | Description |
| --- | --- |
| `[PATTERN_OR_REV]...` | The pattern (unless -e is given), then revisions or paths |
| `[PATH]...` | Limit to these paths (after `--`) |

## Options

| Option | Value | Description |
| --- | --- | --- |
| `-A, --after-context` | `N` | Show this many lines of context after each match |
| `-B, --before-context` | `N` | Show this many lines of context before each match |
| `-C, --context` | `N` | Show this many lines of context around each match |
| `-o, --only-matching` |  | Show only the matching part of each line |
| `-L, --files-without-match` |  | Show only the names of files without a match |
| `--heading` |  | Show each file's name once, above its matches |
| `--break` |  | Print an empty line between files |
| `-h` |  | Leave file names out of match lines |
| `-H` |  | Show file names on match lines (the default) |
| `-m, --max-count` | `N` | Stop each file after N matching lines |
| `-I` |  | Skip binary files |
| `-z, --null` |  | Print NUL after file names instead of `:` |
| `--full-name` |  | Print paths from the top level, not from the current folder |
| `-i, --ignore-case` |  | Match regardless of case |
| `-w, --word-regexp` |  | Match whole words only |
| `-v, --invert-match` |  | Show lines that do not match |
| `-n, --line-number` |  | Prefix each line with its number |
| `-l, --files-with-matches` |  | Show only the names of files with matches |
| `-c, --count` |  | Show the number of matching lines per file |
| `-q, --quiet` |  | Print nothing; exit 0 on a match, 1 otherwise |
| `-F, --fixed-strings` |  | Patterns are fixed strings, not regexes |
| `-E, --extended-regexp` |  | Patterns are extended regexes (the default is basic) |
| `-P, --perl-regexp` |  | Patterns are Perl-compatible regexes, with lookaround |
| `-e` | `PATTERN` | A pattern; repeat to match any of several. Combine them with `--and`, `--or`, `--not` and `(` `)` as git does |
| `--all-match` |  | Keep only files that match every pattern (every `--or` branch) |
| `-p, --show-function` |  | Show the line naming the function around each match |
| `-W, --function-context` |  | Show the whole function around each match |
| `--cached` |  | Search the index instead of the working tree |
| `--untracked` |  | Search untracked files too |
| `--no-index` |  | Search every file under the current folder, tracked or not (works outside a repository) |
| `--exclude-standard` |  | Leave out ignored files with --no-index |
| `--no-exclude-standard` |  | Search ignored files too with --untracked |
| `--recurse-submodules` |  | Search the checked-out submodules too |
| `--threads` | `N` | Accepted for git compatibility; the search always runs in parallel |

See the [global options](global-options.md) that every command accepts.
