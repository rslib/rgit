# rgit for-each-ref

List refs in a custom format, like `git for-each-ref`

## Usage

```sh
rgit for-each-ref [OPTIONS] [PATTERNS]...
```

## Examples

```sh
rgit for-each-ref refs/heads
rgit for-each-ref --sort=-committerdate --format='%(refname:short) %(subject)'
rgit for-each-ref --merged main refs/heads
rgit for-each-ref --format='%(refname:short) %(ahead-behind:main) %(*subject)'
```

## Arguments

| Argument | Description |
| --- | --- |
| `[PATTERNS]...` | Show refs that start with one of these (`refs/heads`) or match a glob |

## Options

| Option | Value | Description |
| --- | --- | --- |
| `--format` | `FORMAT` | The line format: `%(refname)`, `%(refname:short)`, `%(objectname)`, `%(objectname:short)`, `%(objecttype)`, `%(subject)`, `%(body)`, `%(authorname)`, `%(authoremail)`, `%(authordate[:short\|iso\|unix\|relative])`, `%(committer*)`, `%(tagger*)`, `%(creatordate)`, `%(contents[:signature])`, `%(upstream[:short\|track\|trackshort])`, `%(HEAD)`, `%(symref)`, `%(objectsize)`, `%(tree)`, `%(parent)`, `%(describe[:tags,abbrev=N,match=P])`, `%(ahead-behind:&lt;ref&gt;)`, `%(*subject)` and every other field with `*` for what an annotated tag points at, `%(align:N[,middle\|right])...%(end)`, `%(if[:equals=X])...%(then)...[%(else)...]%(end)` |
| `--sort` | `KEY` | Sort by this field; `-` in front reverses (`-committerdate`); `version:` compares numbers by value (`version:refname`). The last --sort is the main key |
| `--count` | `N` | Show at most N refs |
| `--merged` | `COMMIT` | Only refs reachable from this commit (default HEAD) |
| `--no-merged` | `COMMIT` | Only refs not reachable from this commit (default HEAD) |
| `--contains` | `COMMIT` | Only refs that contain this commit (default HEAD) |
| `--no-contains` | `COMMIT` | Only refs that do not contain this commit (default HEAD) |
| `--points-at` | `OBJECT` | Only refs that point at this object |
| `--exclude` | `PATTERN` | Leave out refs matching this pattern |
| `--omit-empty` |  | Print nothing for a ref whose format expands to nothing |

See the [global options](global-options.md) that every command accepts.
