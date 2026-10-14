# rgit fetch-pack

Fetch refs' objects from another repository (a path or file:// URL) without updating any ref, printing `<id> <ref>` for each, like `git fetch-pack`

## Usage

```sh
rgit fetch-pack [OPTIONS] <REPOSITORY> [REFS]...
```

## Examples

```sh
rgit fetch-pack ../other refs/heads/main
rgit fetch-pack --all file:///srv/repo.git
```

## Arguments

| Argument | Description |
| --- | --- |
| `<REPOSITORY>` | The repository to fetch from |
| `[REFS]...` | Full ref names to fetch (`refs/heads/main`, `HEAD`) |

## Options

| Option | Value | Description |
| --- | --- | --- |
| `--all` |  | Fetch every ref the remote has |
| `--stdin` |  | Read more ref names from stdin, one per line |
| `--depth` | `N` | Deepen no further than N commits (a shallow fetch) |
| `-q, --quiet` |  | Quiet; accepted for git compatibility |
| `-k, --keep` |  | Keep the pack; accepted for git compatibility |
| `--thin` |  | Accepted for git compatibility |
| `--include-tag` |  | Accepted for git compatibility |
| `--no-progress` |  | Accepted for git compatibility |
| `-v, --verbose` |  | Accepted for git compatibility |
| `--upload-pack` | `PROGRAM` | Accepted for git compatibility |

See the [global options](global-options.md) that every command accepts.
