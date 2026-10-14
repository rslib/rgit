# rgit commit

Commit the staged changes (runs hooks), or only the given paths

## Usage

```sh
rgit commit [OPTIONS] [PATHS]...
```

## Examples

```sh
rgit commit -m "<message>"
rgit commit -a -m "<message>"
rgit commit src/lib.rs -m "<message>"
rgit commit -m "<subject>" -m "<body>"
rgit commit --amend --no-edit
rgit commit --fixup <rev>
rgit commit -C <rev> --reset-author
rgit commit --dry-run -a
rgit commit --short
rgit commit -v -e -m "<message>"
```

## Arguments

| Argument | Description |
| --- | --- |
| `[PATHS]...` | Commit only these paths, as they are in the working tree; other staged changes stay staged |

## Options

| Option | Value | Description |
| --- | --- | --- |
| `-m, --message` | `MESSAGE` | The commit message; repeat for more paragraphs (prompted for if omitted on a terminal) |
| `-F, --file` | `FILE` | Read the commit message from a file (`-` for stdin) |
| `--amend` |  | Amend the previous commit instead of creating a new one |
| `--no-edit` |  | Keep the amended commit's message (with --amend), or -c's without opening the editor |
| `-e, --edit` |  | Edit the message in the editor before committing (git's -e) |
| `-C, --reuse-message` | `REV` | Reuse this commit's message and author (git's -C) |
| `-c, --reedit-message` | `REV` | Like -C, but edit the message first when on a terminal (git's -c) |
| `--reset-author` |  | With -C, -c or --amend, make the committer the author, dated now |
| `--date` | `DATE` | The author date: `YYYY-MM-DD[THH:MM:SS]`, `@&lt;unix&gt;` or `&lt;unix&gt;`, each with an optional `+HHMM` offset |
| `--dry-run` |  | Show what would be committed, without committing: git's long status, or `--short`, `--porcelain`, `-z` |
| `--short` |  | A dry run in git's short status format |
| `--porcelain` |  | A dry run in git's porcelain status format |
| `--long` |  | A dry run in git's long status format |
| `-z, --null` |  | A dry run with NUL-terminated entries (porcelain unless --short) |
| `--branch` |  | Show the branch in a --short or --porcelain dry run |
| `-u, --untracked-files` | `MODE` | Untracked files in the dry run or template: `no`, `normal` or `all` |
| `--status` |  | Put the status in the editor's message template (git's default; commit.status) |
| `--no-status` |  | Leave the status out of the message template |
| `-t, --template` | `FILE` | Start the message from this file (commit.template) |
| `-i, --include` |  | Stage the given paths too, then commit the whole index (git's -i) |
| `-v, --verbose` |  | Show the diff to commit below the message template, and in a dry run; repeat for the unstaged diff too (commit.verbose) |
| `--no-verbose` |  | Do not show the diff in the template, whatever commit.verbose says |
| `-p, --patch` |  | Pick the hunks to commit first, as `add -p` does |
| `--interactive` |  | Pick what to commit first from `add -i`'s menu |
| `-a, --all` |  | Stage all tracked, modified files before committing (git's -a) |
| `-n, --no-verify` |  | Skip the pre-commit and commit-msg hooks (git's --no-verify) |
| `--author` | `NAME <EMAIL>` | Set the author, as `Name &lt;email&gt;` or a pattern that names an existing author |
| `-s, --signoff` |  | Add a Signed-off-by trailer for the committer (git's -s) |
| `--allow-empty` |  | Commit even when nothing changed |
| `--fixup` | `REV` | Make a `fixup!` commit for this revision, for a later autosquash |
| `--squash` | `REV` | Make a `squash!` commit for this revision, for a later autosquash |
| `-S, --gpg-sign` | `KEYID` | Sign the commit with gpg, gpgsm or ssh-keygen (per gpg.format), with user.signingKey or `-S&lt;KEYID&gt;` (git's -S) |
| `--no-gpg-sign` |  | Do not sign, even with commit.gpgSign set |
| `--pathspec-from-file` | `FILE` | Read the pathspecs from this file, one per line (`-` for stdin) |
| `--pathspec-file-nul` |  | With --pathspec-from-file, the pathspecs are NUL-separated |

See the [global options](global-options.md) that every command accepts.
