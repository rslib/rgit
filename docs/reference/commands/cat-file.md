# rgit cat-file

Print an object's type, size or content, like `git cat-file`

## Usage

```sh
rgit cat-file [OPTIONS] [[TYPE] OBJECT]...
```

## Examples

```sh
rgit cat-file -p HEAD
rgit cat-file -p HEAD:src/lib.rs
rgit cat-file -t <rev>
rgit cat-file --batch-check < ids.txt
rgit cat-file --textconv HEAD:doc.pdf
```

## Arguments

| Argument | Description |
| --- | --- |
| `[[TYPE] OBJECT]...` | The object (`HEAD`, `HEAD:src/lib.rs`, an id), optionally after its type (`blob HEAD:a.txt`) |

## Options

| Option | Value | Description |
| --- | --- | --- |
| `-t` |  | Print the object's type |
| `-s` |  | Print the object's size in bytes |
| `-p` |  | Print the object's content (a tree as `ls-tree` lists it) |
| `-e` |  | Exit 0 if the object exists, 1 if not, printing nothing |
| `--batch` | `FORMAT` | Read object names from stdin; print each one's id, type, size and content, or the given format (`%(objectname)`, `%(objecttype)`, `%(objectsize)`, `%(objectsize:disk)`, `%(deltabase)`, `%(rest)`) |
| `--batch-check` | `FORMAT` | Like --batch, without the content |
| `--batch-all-objects` |  | With --batch or --batch-check, answer for every object instead of stdin |
| `--batch-command` | `FORMAT` | Read `contents &lt;object&gt;`, `info &lt;object&gt;` and (with --buffer) `flush` commands from stdin, answering as --batch / --batch-check |
| `--buffer` |  | With --batch or --batch-check, do not flush after each object |
| `-Z` |  | With a batch mode, NUL-separate the input and output |
| `-z` |  | With a batch mode, NUL-separate the input |
| `--follow-symlinks` |  | With a batch mode, follow symlinks within the tree for `&lt;tree&gt;:&lt;path&gt;` |
| `--textconv` |  | Show a blob through its diff driver's textconv command |
| `--filters` |  | Show a blob as it would be checked out (smudge and eol filters) |
| `--path` | `PATH` | The path whose attributes pick the --textconv / --filters driver |

See the [global options](global-options.md) that every command accepts.
