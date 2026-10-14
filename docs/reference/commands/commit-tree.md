# rgit commit-tree

Write a commit object for a tree and print its id, like `git commit-tree`; the message comes from -m, -F or stdin

## Usage

```sh
rgit commit-tree [OPTIONS] <TREE>
```

## Examples

```sh
rgit commit-tree 'HEAD^{tree}' -p HEAD -m 'Snapshot'
rgit commit-tree $(rgit write-tree) -p main -p topic -F msg.txt
```

## Arguments

| Argument | Description |
| --- | --- |
| `<TREE>` | The tree (e.g. `HEAD^{tree}`) |

## Options

| Option | Value | Description |
| --- | --- | --- |
| `-p` | `PARENT` | A parent commit (repeat for a merge) |
| `-m` | `MESSAGE` | A message paragraph (repeatable) |
| `-F` | `FILE` | Read the message from a file (`-` is stdin) |

See the [global options](global-options.md) that every command accepts.
