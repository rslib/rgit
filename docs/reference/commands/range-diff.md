# rgit range-diff

Compare two versions of a series (`git range-diff`): `<base> <old> <new>`, `<old-range> <new-range>` or `<old>...<new>`

## Usage

```sh
rgit range-diff [OPTIONS] <REVS>... [-- <PATHS>...]
```

## Examples

```sh
rgit range-diff main topic-v1 topic-v2
rgit range-diff -s main..topic@{1} main..topic
rgit range-diff topic@{u}...topic
rgit range-diff --creation-factor=80 --no-notes -U1 main v1 v2 -- src
```

## Arguments

| Argument | Description |
| --- | --- |
| `<REVS>...` | The ranges or revisions |
| `[PATHS]...` | Only commits touching these paths (after `--`) |

## Options

| Option | Value | Description |
| --- | --- | --- |
| `--creation-factor` | `N` | Percent of a patch that may change for it still to pair (default 60) (default: `60`) |
| `-s, --no-patch` |  | Only list the pairs, without their diffs |
| `--left-only` |  | Only the commits of the first range (and pairs) |
| `--right-only` |  | Only the commits of the second range (and pairs) |
| `-U, --unified` | `N` | Lines of context in the diffs between pairs (default 3) |
| `--notes` | `REF` | Include these notes refs' notes (bare: the default notes too) |
| `--no-notes` |  | Leave notes out of the compared patches |

See the [global options](global-options.md) that every command accepts.
