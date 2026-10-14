# rgit reflog

Show where a ref pointed over time (`git reflog [show] [REF]`), or `expire [--expire=<date>] [--expire-unreachable=<date>] [--all] [--rewrite] [--updateref] [--stale-fix] [-n] [--verbose] [REF...]`, `delete [--rewrite] [--updateref] [-n] REF@{N}...`, `exists REF`

## Usage

```sh
rgit reflog [OPTIONS] [[show] REF]...
```

## Examples

```sh
rgit reflog
rgit reflog show main -n 10
rgit reflog expire --expire=30.days.ago --all
rgit reflog delete --rewrite HEAD@{2}
```

## Arguments

| Argument | Description |
| --- | --- |
| `[[show] REF]...` | `show` (optional) and the ref (default HEAD); or expire, delete or exists and their arguments |

## Options

| Option | Value | Description |
| --- | --- | --- |
| `-n, --max-count` | `N` | Show at most N entries |

See the [global options](global-options.md) that every command accepts.
