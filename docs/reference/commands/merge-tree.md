# rgit merge-tree

Merge two commits without touching the index or working tree, like `git merge-tree --write-tree`: prints the merged tree, conflicted files and messages; exits 1 on conflicts. With three trees (base, ours, theirs) it is git's old trivial merge report

## Usage

```sh
rgit merge-tree [OPTIONS] [COMMIT]...
```

## Examples

```sh
rgit merge-tree --write-tree main feature
rgit merge-tree --name-only main feature
rgit merge-tree -z --messages -Xno-renames main feature
printf 'main feature\n' | rgit merge-tree --stdin
rgit merge-tree base-tree ours theirs
```

## Arguments

| Argument | Description |
| --- | --- |
| `[COMMIT]...` | `&lt;branch1&gt; &lt;branch2&gt;`, or `&lt;base&gt; &lt;branch1&gt; &lt;branch2&gt;` |

## Options

| Option | Value | Description |
| --- | --- | --- |
| `--write-tree` |  | A real merge of two commits (the default with two arguments) |
| `--trivial-merge` |  | The old trivial merge of three trees (the default with three) |
| `--quiet` |  | Print nothing; only the exit status tells whether it is clean |
| `--stdin` |  | Merge each `[&lt;base&gt; -- ]&lt;branch1&gt; &lt;branch2&gt;` line of stdin |
| `-X, --strategy-option` | `OPTION` | A merge strategy option (ours, theirs, no-renames, find-renames[=&lt;n&gt;], ignore-space-change, ...) |
| `--name-only` |  | List only the names of conflicted files |
| `--messages` |  | Print the informational messages even on a clean merge |
| `--no-messages` |  | Do not print the informational messages |
| `-z` |  | NUL-terminated output |
| `--allow-unrelated-histories` |  | Merge even without a common ancestor |
| `--merge-base` | `TREE-ISH` | Use this commit or tree as the merge base |

See the [global options](global-options.md) that every command accepts.
