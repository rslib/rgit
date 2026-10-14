# rgit rev-parse

Resolve revisions to object ids, or print repository paths, like `git rev-parse`. Answers print in the order the options and revisions are given

## Usage

```sh
rgit rev-parse [OPTIONS] [ARGS]...
```

## Examples

```sh
rgit rev-parse HEAD
rgit rev-parse --short HEAD
rgit rev-parse --abbrev-ref HEAD
rgit rev-parse --show-toplevel --show-prefix
rgit rev-parse --symbolic-full-name @{u}
```

## Arguments

| Argument | Description |
| --- | --- |
| `[ARGS]...` | Revisions (`HEAD`, `main~2`, `v1^{commit}`, `HEAD:path`, `^A`, `A..B`, `A...B`) and options: `--verify`, `-q`, `--short[=N]`, `--abbrev-ref`, `--symbolic`, `--symbolic-full-name`, `--not`, `--all`, `--branches`, `--tags`, `--remotes`, `--default REV`, `--show-toplevel`, `--show-prefix`, `--show-cdup`, `--git-dir`, `--absolute-git-dir`, `--git-common-dir`, `--is-inside-work-tree`, `--is-inside-git-dir`, `--is-bare-repository`, `--is-shallow-repository`, `--show-object-format`, `--local-env-vars`, `--sq-quote ARGS...` |

See the [global options](global-options.md) that every command accepts.
