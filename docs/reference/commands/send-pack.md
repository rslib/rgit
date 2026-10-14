# rgit send-pack

Push refs to another repository (a path or file:// URL) with no remote config, like `git send-pack`; the report goes to stderr

## Usage

```sh
rgit send-pack [OPTIONS] <REPOSITORY> [REFS]...
```

## Examples

```sh
rgit send-pack ../backup.git main
rgit send-pack --all ../backup.git
rgit send-pack --force ../backup.git main:refs/heads/old
```

## Arguments

| Argument | Description |
| --- | --- |
| `<REPOSITORY>` | The repository to push to |
| `[REFS]...` | Refspecs: `main`, `src:dst`, `+src:dst`, `:dst` (delete) |

## Options

| Option | Value | Description |
| --- | --- | --- |
| `--all` |  | Push every branch |
| `--mirror` |  | Make every remote ref match a local one, deleting the rest |
| `-n, --dry-run` |  | Report what would be pushed without pushing |
| `-f, --force` |  | Update refs even when that loses commits |
| `--atomic` |  | Update every ref or none |
| `-v, --verbose` |  | List up-to-date refs too |
| `-q, --quiet` |  | Quiet; accepted for git compatibility |
| `--thin` |  | Accepted for git compatibility |
| `--receive-pack` | `PROGRAM` | Accepted for git compatibility |

See the [global options](global-options.md) that every command accepts.
