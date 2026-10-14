# rgit cherry-pick

Cherry-pick commits onto HEAD, or continue/skip/abort a stopped one

## Usage

```sh
rgit cherry-pick [OPTIONS] [REVS]...
```

## Examples

```sh
rgit cherry-pick <rev>
rgit cherry-pick <rev> --no-commit
rgit cherry-pick <a> <b>
rgit cherry-pick main~3..main -x
rgit cherry-pick <merge> -m 1
rgit cherry-pick <rev> -s --empty=drop
rgit cherry-pick <rev> --strategy=ours --cleanup=strip
rgit cherry-pick --continue
rgit cherry-pick --abort
rgit cherry-pick --quit
```

## Arguments

| Argument | Description |
| --- | --- |
| `[REVS]...` | Commits or ranges `A..B` to apply in order (prompted for if omitted on a terminal) |

## Options

| Option | Value | Description |
| --- | --- | --- |
| `-S, --gpg-sign` | `KEYID` | Sign the commit with gpg, gpgsm or ssh-keygen (per gpg.format), with user.signingKey or `-S&lt;KEYID&gt;` (git's -S) |
| `--no-gpg-sign` |  | Do not sign, even with commit.gpgSign set |
| `-n, --no-commit` |  | Apply the change without committing (git's -n/--no-commit) |
| `-x` |  | Append "(cherry picked from commit ...)" to each message (git's -x) |
| `-m, --mainline` | `PARENT` | For a merge commit, the parent number (from 1) to diff against |
| `-X, --strategy-option` | `STRATEGY_OPTION` | Take this side on conflicting hunks (git's -X) |
| `--strategy` | `STRATEGY` | The merge strategy; `ours` keeps HEAD's tree |
| `--cleanup` | `MODE` | How to clean up each message: strip, whitespace, verbatim, scissors or default |
| `--rerere-autoupdate` |  | Stage the files rerere resolved with a recorded resolution |
| `--no-rerere-autoupdate` |  | Leave them unstaged, whatever rerere.autoUpdate says |
| `-e, --edit` |  | Open the editor on each commit message |
| `-s, --signoff` |  | Add a Signed-off-by trailer |
| `--ff` |  | Fast-forward over a commit whose parent is HEAD instead of copying it |
| `--allow-empty` |  | Keep commits that were empty to begin with |
| `--keep-redundant-commits` |  | Keep commits that become empty (same as --empty=keep) |
| `--empty` | `EMPTY` | What to do with a commit whose change is already in HEAD |
| `--continue` |  | Commit the resolved commit and apply the rest |
| `--skip` |  | Drop the current commit and apply the rest |
| `--abort` |  | Cancel and return to where the cherry-pick started |
| `--quit` |  | Forget the stopped cherry-pick, leaving HEAD, the index and the working tree as they are |

See the [global options](global-options.md) that every command accepts.
