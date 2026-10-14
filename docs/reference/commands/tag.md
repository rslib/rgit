# rgit tag

Tag management: `tag` lists, `tag <name> [<rev>]` creates, `tag -d <name>...` deletes, `tag -l <pattern>...` lists matching tags

## Usage

```sh
rgit tag [OPTIONS] [NAMES]...
```

## Examples

```sh
rgit tag
rgit tag v1.0.0 -m "<message>"
rgit tag v1.0.0 <rev>
rgit tag -l "v1.*" -n
rgit tag --sort=-v:refname --format='%(refname:short) %(creatordate:short)'
rgit tag -s v1.0.0 -F notes.txt
rgit tag --contains <rev>
rgit tag -d v1.0.0 v1.0.1
rgit tag --column=row,dense
rgit tag -m "<message>" --trailer "Reviewed-by: <name>" v1.0.0
```

## Arguments

| Argument | Description |
| --- | --- |
| `[NAMES]...` | The tag to create, then the revision to tag (default HEAD); with -d, the tags to delete; with -v, the tags to verify; with -l, patterns to list |

## Options

| Option | Value | Description |
| --- | --- | --- |
| `-m, --message` | `MESSAGE` | Annotation message (implies an annotated tag) |
| `-F, --file` | `FILE` | Read the annotation message from a file, `-` for stdin (git's -F) |
| `-e, --edit` |  | Edit the message in the editor (terminal only; git's -e) |
| `-a, --annotate` |  | Make an annotated tag (needs -m, or a prompt on a terminal) |
| `-s, --sign` |  | Make a GPG-signed annotated tag (git's -s) |
| `--no-sign` |  | Do not sign, even with `tag.gpgSign` set |
| `-u, --local-user` | `KEY-ID` | Sign with this key (git's -u) |
| `--cleanup` | `MODE` | How to clean the message: strip (default; drops `#` lines), whitespace or verbatim |
| `-f, --force` |  | Replace an existing tag of the same name (git's -f) |
| `-d, --delete` |  | Delete the named tags (git's -d) |
| `-v, --verify` |  | Verify the named tags' GPG signatures (git's -v) |
| `-l, --list` |  | List tags, only those matching the given patterns (git's -l) |
| `-n` | `N` | List tags with up to N lines of their message (git's -n, default 1) |
| `--contains` | `REV` | List only tags that contain this commit (default HEAD) |
| `--no-contains` | `REV` | List only tags that do not contain this commit (default HEAD) |
| `--merged` | `REV` | List only tags merged into this commit (default HEAD) |
| `--no-merged` | `REV` | List only tags not merged into this commit (default HEAD) |
| `--points-at` | `REV` | List only tags that point at this commit (default HEAD) |
| `--sort` | `KEY` | Sort by a for-each-ref key (`-creatordate`, `version:refname` or `-v:refname`); the last --sort is the main key (default refname, or `tag.sort`) |
| `--format` | `FORMAT` | Print each tag in a for-each-ref format, e.g. `%(refname:short)` |
| `-i, --ignore-case` |  | Match patterns and sort case-insensitively (git's -i) |
| `--column` | `OPTIONS` | Lay the list out in columns: always, never, auto, column, row, plain, dense, nodense (default `column.tag`, `column.ui`) |
| `--no-column` |  | List one tag per line |
| `--trailer` | `TOKEN[(=\|:)VALUE]` | Add a `&lt;token&gt;: &lt;value&gt;` (or `&lt;token&gt;=&lt;value&gt;`) trailer to the message; implies an annotated tag |
| `--create-reflog` |  | Keep a reflog for the new tag |

See the [global options](global-options.md) that every command accepts.
