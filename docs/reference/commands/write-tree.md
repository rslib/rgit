# rgit write-tree

Write the index as a tree and print its id, like `git write-tree`

## Usage

```sh
rgit write-tree [OPTIONS]
```

## Examples

```sh
rgit write-tree
rgit write-tree --prefix=src/
```

## Options

| Option | Value | Description |
| --- | --- | --- |
| `--missing-ok` |  | Allow entries whose objects are missing |
| `--prefix` | `PREFIX` | Write only the subtree at this folder |

See the [global options](global-options.md) that every command accepts.
