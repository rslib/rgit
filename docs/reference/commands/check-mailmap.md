# rgit check-mailmap

Show each contact (`Name <email>` or `<email>`) as the mailmap maps it, like `git check-mailmap`

## Usage

```sh
rgit check-mailmap [OPTIONS] [CONTACTS]...
```

## Examples

```sh
rgit check-mailmap 'Jane <jane@old.example>'
rgit log --format='%an <%ae>' | rgit check-mailmap --stdin
```

## Arguments

| Argument | Description |
| --- | --- |
| `[CONTACTS]...` | The contacts to map |

## Options

| Option | Value | Description |
| --- | --- | --- |
| `--stdin` |  | Also read contacts from stdin, one per line, after the arguments |
| `--mailmap-file` | `FILE` | Read this mailmap file too |
| `--mailmap-blob` | `BLOB` | Read the mailmap in this blob too (`HEAD:.mailmap`) |

See the [global options](global-options.md) that every command accepts.
