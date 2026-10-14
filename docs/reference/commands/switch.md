# rgit switch

Switch branches: `switch <branch>`, `-c <new> [<start>]`, `--detach <rev>`, or `-` for the previous branch

## Usage

```sh
rgit switch [OPTIONS] [REV]
```

## Examples

```sh
rgit switch main
rgit switch -c <new_branch>
rgit switch -
rgit switch --detach <rev>
rgit switch -m <branch>
rgit switch --orphan <new_branch>
```

## Arguments

| Argument | Description |
| --- | --- |
| `[REV]` | The branch to switch to (`-` for the previous one); with -c, -C or --detach, the start point (prompted for if omitted on a terminal) |

## Options

| Option | Value | Description |
| --- | --- | --- |
| `-c, --create` | `NEW_BRANCH` | Create a new branch and switch to it |
| `-C, --force-create` | `BRANCH` | Create or reset a branch and switch to it |
| `-d, --detach` |  | Switch to a revision as a detached HEAD |
| `-t, --track` |  | Track the start point as the new branch's upstream |
| `--no-track` |  | Never set an upstream for the new branch |
| `-f, --discard-changes` |  | Throw away local changes when switching |
| `-m, --merge` |  | Carry local changes over to the new branch with a three-way merge |
| `--conflict` | `STYLE` | Conflict marker style for -m: `merge` or `diff3` |
| `--orphan` | `NEW_BRANCH` | Start a new branch with no history and an empty working tree |
| `--no-guess` |  | Do not turn a remote branch's name into a local tracking branch |

See the [global options](global-options.md) that every command accepts.
