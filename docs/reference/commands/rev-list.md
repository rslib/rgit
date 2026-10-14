# rgit rev-list

List commit ids reachable from revisions, like `git rev-list`

## Usage

```sh
rgit rev-list [OPTIONS] [REVS]... [-- <PATH>...]
```

## Examples

```sh
rgit rev-list --count HEAD
rgit rev-list main..HEAD
rgit rev-list -n 1 --all
rgit rev-list HEAD -- src/lib.rs
rgit rev-list --count --left-right main...feature
rgit rev-list --objects main..feature
rgit rev-list --format='%h %s' --no-commit-header -5 HEAD
rgit rev-list --objects --filter=blob:none --no-object-names HEAD
rgit rev-list --disk-usage=human --objects --all
rgit rev-list --bisect-vars good..bad
```

## Arguments

| Argument | Description |
| --- | --- |
| `[REVS]...` | Revisions: `HEAD`, `^A` (exclude), `A..B`, `A...B` |
| `[PATH]...` | Only commits that change these paths (after `--`), simplified as git does |

## Options

| Option | Value | Description |
| --- | --- | --- |
| `-n, --max-count` | `N` | Show at most N commits |
| `--count` |  | Print only the number of commits |
| `--all` |  | Walk every ref and HEAD |
| `--reverse` |  | Show the oldest commit first |
| `--first-parent` |  | Follow only the first parent of merges |
| `--merges` |  | Show only merge commits |
| `--no-merges` |  | Leave out merge commits |
| `--parents` |  | Print each commit's parents after it |
| `--skip` | `N` | Leave out the first N commits |
| `--branches` | `PATTERN` | Walk every branch, or those matching the pattern |
| `--tags` | `PATTERN` | Walk every tag, or those matching the pattern |
| `--remotes` | `PATTERN` | Walk every remote-tracking branch, or those matching the pattern |
| `--abbrev-commit` |  | Abbreviate commit ids |
| `--since` | `SINCE` | Only commits at or after this date (`2024-01-05`, `2 weeks ago`...) |
| `--until` | `UNTIL` | Only commits at or before this date |
| `--author` | `AUTHOR` | Keep only commits whose author matches this regex |
| `--committer` | `COMMITTER` | Keep only commits whose committer matches this regex |
| `--grep` | `REGEX` | Keep only commits whose message matches this regex (repeat for any of several) |
| `-i, --regexp-ignore-case` |  | Match --grep, --author and --committer case-insensitively |
| `--objects` |  | After the commits, the trees and blobs they need, with their paths |
| `--objects-edge` |  | --objects, first naming the excluded commits the range starts from (`-&lt;id&gt;`) |
| `--missing` | `ACTION` | What to do about missing objects: error, allow-any, allow-promisor or print |
| `--left-right` |  | Mark each commit with the side of a symmetric range it is on (`&lt;`, `&gt;`) |
| `--cherry-pick` |  | Leave out commits whose change the other side of a symmetric range has too |
| `--cherry-mark` |  | Mark commits whose change the other side has too `=`, the others `+` |
| `--cherry` |  | The right side's commits, marking those the left side has too (`--right-only --cherry-mark --no-merges`) |
| `--left-only` |  | Only the left side of a symmetric range |
| `--right-only` |  | Only the right side of a symmetric range |
| `--boundary` |  | After the commits, the excluded ones they have as parents, marked `-` |
| `--ancestry-path` |  | Only commits that descend from the range's excluded end |
| `--simplify-by-decoration` |  | Only commits that a branch or tag points at |
| `--simplify-merges` |  | With paths, leave out merges the shown history does not need |
| `--full-history` |  | With paths, follow every parent of a merge |
| `--dense` |  | With paths, show only commits that change them (the default) |
| `--sparse` |  | With paths, show every commit walked |
| `--source` |  | Name the starting ref that reached each commit |
| `--merge` |  | During a conflicted merge, the commits on either side that touch the conflicted paths |
| `--no-walk` | `sorted\|unsorted` | Show only the given commits, newest first (`=unsorted`: as given) |
| `--do-walk` |  | Walk the history (undoes --no-walk) |
| `--topo-order` |  | Never show a parent before all of its children, keeping branches together |
| `--date-order` |  | Never show a parent before all of its children, else by commit date |
| `--author-date-order` |  | Never show a parent before all of its children, else by author date |
| `--all-match` |  | Keep commits matching every --grep, not any |
| `--invert-grep` |  | Keep commits whose message matches no --grep |
| `--no-object-names` |  | With --objects, print only the ids, not the objects' paths |
| `--object-names` |  | With --objects, print each object's path after its id (the default) |
| `--filter` | `SPEC` | Leave objects out of --objects: `blob:none`, `blob:limit=&lt;n&gt;[kmg]`, `tree:&lt;depth&gt;`, `object:type=&lt;type&gt;` or `combine:&lt;a&gt;+&lt;b&gt;` |
| `--no-filter` |  | Forget the --filter options before it |
| `--filter-print-omitted` |  | Also print the objects the filter left out, as `~&lt;id&gt;` |
| `--disk-usage` | `human` | Print the total size on disk of what would be listed instead (`=human` for KiB, MiB, GiB) |
| `--children` |  | Print each commit's children after it |
| `--timestamp` |  | Print each commit's committer timestamp before it |
| `--header` |  | Each commit in raw form, NUL-terminated |
| `--format` | `FORMAT` | Print each commit in this format (`%h %s`), after a `commit &lt;id&gt;` line |
| `--pretty` | `FORMAT` | Print each commit in a named format (oneline, short, medium, full, fuller, raw) or `format:&lt;string&gt;` |
| `--oneline` |  | `--pretty=oneline --abbrev-commit` |
| `--date` | `STYLE` | The date style for the formats (`%ad`) |
| `--no-commit-header` |  | With --format, leave out the `commit &lt;id&gt;` lines |
| `--commit-header` |  | With --format, print the `commit &lt;id&gt;` lines (the default) |
| `--bisect` |  | Print the commit that halves the range (bisection's next step) |
| `--bisect-vars` |  | Print shell variables for bisection's next step |
| `--bisect-all` |  | Print every commit with its distance from the range's ends, best first |
| `--quiet` |  | Print nothing; only the exit status says whether the walk worked |
| `--abbrev` | `N` | Abbreviate ids to at least N hex digits |
| `--stdin` |  | Read more revisions (and, after a `--` line, paths) from stdin |
| `--exclude` | `GLOB` | Leave refs matching this glob out of --all, --branches, --tags and --remotes |

See the [global options](global-options.md) that every command accepts.
