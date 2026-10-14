# rgit am

Apply mbox patches (from format-patch) as commits

## Usage

```sh
rgit am [OPTIONS] [MBOX]...
```

## Examples

```sh
rgit am patches/*.patch
rgit am --continue
rgit am --abort
rgit am -3 -s --committer-date-is-author-date series.mbox
rgit am --show-current-patch=diff
rgit am -c -m --keep-cr ~/Maildir/patches
rgit am --skip
```

## Arguments

| Argument | Description |
| --- | --- |
| `[MBOX]...` | mbox files (reads stdin when none) |

## Options

| Option | Value | Description |
| --- | --- | --- |
| `--abort` |  | Give up and restore the branch as it was |
| `-r, --continue` |  | Commit the resolved patch and go on |
| `--skip` |  | Skip the current patch |
| `--quit` |  | Stop, keeping the branch and index as they are |
| `--allow-empty` |  | Commit the stopped empty patch as an empty commit and go on |
| `--keep-cr` |  | Keep CR at the end of lines (default: am.keepcr) |
| `--no-keep-cr` |  | Strip CR at the end of lines (overrides am.keepcr) |
| `--show-current-patch` | `PART` | Print the patch am stopped at (`diff`: only its diff, `raw`: the mail) |
| `-3, --3way` |  | Fall back to a three-way merge |
| `-s, --signoff` |  | Add a Signed-off-by trailer |
| `-k, --keep` |  | Keep the subject as is (no stripping of `[PATCH]`) |
| `--keep-non-patch` |  | Keep a `[...]` that is not `[PATCH ...]` in the subject |
| `-m, --message-id` |  | Add the Message-ID to the commit message |
| `-c, --scissors` |  | Drop everything above a `-- &gt;8 --` scissors line |
| `--no-scissors` |  | Keep a scissors line and what is above it (overrides mailinfo.scissors) |
| `--committer-date-is-author-date` |  | Use the author date as the committer date |
| `--ignore-date` |  | Use the committer date as the author date |
| `-i, --interactive` |  | Confirm each patch on the terminal before applying it |
| `-n, --no-verify` |  | Skip the pre-applypatch and applypatch-msg hooks |
| `--empty` | `ACTION` | On a patch with no changes: stop, drop or keep |
| `-p` | `N` | Strip this many leading path components (as apply's -p) |
| `--directory` | `ROOT` | Prepend this folder to every path |
| `--exclude` | `GLOB` | Skip paths matching this glob |
| `--include` | `GLOB` | Apply only paths matching this glob |
| `--reject` |  | Leave the hunks that do not apply in `.rej` files |
| `--whitespace` | `ACTION` | On trailing whitespace: nowarn, warn, fix, error or error-all |
| `-q, --quiet` |  | Print nothing but errors |
| `--patch-format` | `FORMAT` | The patches' format (default: detected from the first one) |
| `-S, --gpg-sign` | `KEYID` | Sign the commit with gpg, gpgsm or ssh-keygen (per gpg.format), with user.signingKey or `-S&lt;KEYID&gt;` (git's -S) |
| `--no-gpg-sign` |  | Do not sign, even with commit.gpgSign set |
| `--rerere-autoupdate` |  | Stage the files rerere resolved with a recorded resolution |
| `--no-rerere-autoupdate` |  | Leave them unstaged, whatever rerere.autoUpdate says |

See the [global options](global-options.md) that every command accepts.
