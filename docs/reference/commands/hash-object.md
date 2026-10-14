# rgit hash-object

Print the object id of files or stdin; -w stores them

## Usage

```sh
rgit hash-object [OPTIONS] [PATHS]...
```

## Examples

```sh
rgit hash-object src/lib.rs
rgit hash-object -w --stdin
rgit hash-object --stdin --path src/lib.rs
rgit hash-object --no-filters -t blob file.bin
```

## Arguments

| Argument | Description |
| --- | --- |
| `[PATHS]...` | Files to hash |

## Options

| Option | Value | Description |
| --- | --- | --- |
| `-w` |  | Store the objects in the repository |
| `--stdin` |  | Hash stdin (before any files) |
| `--stdin-paths` |  | Read the paths of the files to hash from stdin, one per line |
| `-t` | `TYPE` | The object type: blob (default), tree, commit or tag |
| `--literally` |  | Skip the format check of trees, commits and tags |
| `--path` | `PATH` | Filter as if the content were at this path (e.g. for --stdin) |
| `--no-filters` |  | Hash the content as is, without the clean filters (crlf, filter drivers) |

See the [global options](global-options.md) that every command accepts.
