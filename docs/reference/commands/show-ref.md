# rgit show-ref

List refs with their object ids, like `git show-ref`

## Usage

```sh
rgit show-ref [OPTIONS] [PATTERNS]...
```

## Examples

```sh
rgit show-ref --heads
rgit show-ref --verify refs/heads/main
```

## Arguments

| Argument | Description |
| --- | --- |
| `[PATTERNS]...` | Show refs whose name ends with one of these (`main`, `origin/main`) |

## Options

| Option | Value | Description |
| --- | --- | --- |
| `--heads` |  | Only branches (refs/heads) |
| `--tags` |  | Only tags (refs/tags) |
| `--verify` |  | Match full ref names exactly (`refs/heads/main`), failing if one is missing |
| `-d, --dereference` |  | Also show what annotated tags point at (`refs/tags/v1^{}`) |
| `-s, --hash` | `N` | Show only object ids, abbreviated to N digits when given |
| `--abbrev` | `N` | Abbreviate object ids to a unique prefix of at least N digits |
| `--exists` |  | Exit 0 if the one ref given exists, 2 if not |
| `--head` |  | Also show HEAD |
| `-q, --quiet` |  | Print nothing; only set the exit code |

See the [global options](global-options.md) that every command accepts.
