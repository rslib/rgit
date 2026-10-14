# rgit read-tree

Read trees into the index, like `git read-tree`: one tree replaces it, -m merges one, two (switch) or three (base, ours, theirs) trees; with more, all but the last two are merge bases

## Usage

```sh
rgit read-tree [OPTIONS] [TREES]...
```

## Examples

```sh
rgit read-tree HEAD
rgit read-tree -m -u HEAD topic
rgit read-tree -m base ours theirs
rgit read-tree -m --trivial --index-output=merged.idx base ours theirs
rgit read-tree --prefix=vendor/lib/ lib-main
```

## Arguments

| Argument | Description |
| --- | --- |
| `[TREES]...` | The trees (commits or tree ids) |

## Options

| Option | Value | Description |
| --- | --- | --- |
| `-m` |  | Merge the trees into the index instead of replacing it |
| `--reset` |  | Like -m, but drop unmerged entries and ignore local changes |
| `-u` |  | Update the working tree files to match (with -m, --reset or --prefix) |
| `-i` |  | Merge in the index only, without checking the working tree |
| `-n, --dry-run` |  | Check the merge without writing the index |
| `--aggressive` |  | Also resolve removals and identical additions in a 3-way merge |
| `--trivial` |  | Fail if the 3-way merge needs a file-level merge |
| `--index-output` | `FILE` | Write the resulting index to this file instead |
| `--no-sparse-checkout` |  | Accepted for git compatibility (rgit applies no sparse checkout) |
| `--prefix` | `PREFIX` | Read the tree under this folder of the index |
| `--empty` |  | Empty the index |

See the [global options](global-options.md) that every command accepts.
