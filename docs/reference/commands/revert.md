# rgit revert

Revert commits on HEAD, or continue/skip/abort a stopped revert

## Usage

```sh
rgit revert [OPTIONS] [REVS]...
```

## Examples

```sh
rgit revert <rev>
rgit revert <rev> --no-commit
rgit revert HEAD~2..HEAD
rgit revert <merge> -m 1
rgit revert <rev> --reference
rgit revert --continue
```

## Arguments

| Argument | Description |
| --- | --- |
| `[REVS]...` | Commits or ranges `A..B` to revert, newest first (prompted for if omitted on a terminal) |

## Options

| Option | Value | Description |
| --- | --- | --- |
| `-S, --gpg-sign` | `KEYID` | Sign the commit with gpg, gpgsm or ssh-keygen (per gpg.format), with user.signingKey or `-S&lt;KEYID&gt;` (git's -S) |
| `--no-gpg-sign` |  | Do not sign, even with commit.gpgSign set |
| `-n, --no-commit` |  | Apply the inverse without committing (git's -n/--no-commit) |
| `-m, --mainline` | `PARENT` | For a merge commit, the parent number (from 1) to revert to |
| `-X, --strategy-option` | `STRATEGY_OPTION` | Take this side on conflicting hunks (git's -X) |
| `--strategy` | `STRATEGY` | The merge strategy; `ours` keeps HEAD's tree |
| `--cleanup` | `MODE` | How to clean up each message: strip, whitespace, verbatim, scissors or default |
| `--rerere-autoupdate` |  | Stage the files rerere resolved with a recorded resolution |
| `--no-rerere-autoupdate` |  | Leave them unstaged, whatever rerere.autoUpdate says |
| `-e, --edit` |  | Open the editor on each revert message |
| `-s, --signoff` |  | Add a Signed-off-by trailer |
| `--reference` |  | Name the reverted commit as `abbrev (subject, date)`, under a title to fill in |
| `--continue` |  | Commit the resolved revert and apply the rest |
| `--skip` |  | Drop the current commit and apply the rest |
| `--abort` |  | Cancel and return to where the revert started |
| `--quit` |  | Forget the stopped revert, leaving HEAD, the index and the working tree as they are |

See the [global options](global-options.md) that every command accepts.
