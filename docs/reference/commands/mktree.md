# rgit mktree

Build a tree from `ls-tree` lines on stdin and print its id, like `git mktree`

## Usage

```sh
rgit mktree [OPTIONS]
```

## Examples

```sh
rgit ls-tree HEAD | rgit mktree
rgit mktree --batch < trees.txt
```

## Options

| Option | Value | Description |
| --- | --- | --- |
| `-z` |  | NUL-terminated lines |
| `--missing` |  | Allow objects that are missing |
| `--batch` |  | Build a tree per blank-line-separated group |

See the [global options](global-options.md) that every command accepts.
