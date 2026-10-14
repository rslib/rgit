# rgit format-patch

Write commits as mbox patch files (`-<n>`, `<since>` or `<a>..<b>`)

## Usage

```sh
rgit format-patch [OPTIONS] [REVS]...
```

## Examples

```sh
rgit format-patch -1
rgit format-patch main -o patches
rgit format-patch main..topic --stdout
rgit format-patch main --cover-letter --thread -v2 --to list@example.com
rgit format-patch -3 --rfc --base=auto --subject-prefix="PATCH net"
rgit format-patch -2 --attach --notes --output=series.mbox
```

## Arguments

| Argument | Description |
| --- | --- |
| `[REVS]...` | `-&lt;n&gt;` for the newest n commits, `&lt;rev&gt;` for the commits after it up to HEAD, or a range `&lt;a&gt;..&lt;b&gt;` |

## Options

| Option | Value | Description |
| --- | --- | --- |
| `-o, --output-directory` | `DIR` | Write the files to this folder |
| `--stdout` |  | Print the patches instead of writing files |
| `-n, --numbered` |  | Number the subjects `[PATCH n/m]` even for one patch |
| `-N, --no-numbered` |  | Never number the subjects |
| `--start-number` | `N` | Number the first patch this (default 1) |
| `--numbered-files` |  | Name the files 1, 2, ... without a suffix |
| `--suffix` | `SFX` | The file suffix (default .patch) |
| `-k, --keep-subject` |  | Keep the subject as is, without `[PATCH]` |
| `-p, --no-stat` |  | Leave out the diffstat |
| `--subject-prefix` | `PREFIX` | The subject prefix instead of PATCH |
| `--rfc` | `TEXT` | Prefix the subject with RFC (or this text; `-text` goes after) |
| `-s, --signoff` |  | Add a Signed-off-by trailer for you |
| `--from` | `IDENT` | Send as this ident (default you), keeping the author in the body |
| `--filename-max-length` | `N` | The longest file name, suffix included (default 64) |
| `--no-binary` |  | Show binary files as "Binary files ... differ" |
| `--ignore-if-in-upstream` |  | Leave out commits whose change upstream already has |
| `--cover-from-description` | `MODE` | How the cover letter uses the branch description: message, subject, auto or none |
| `--description-file` | `FILE` | Use this file as the branch description |
| `--interdiff` | `REV` | Add an interdiff against this previous version to the cover letter |
| `--range-diff` | `REV` | Add a range-diff against this previous version to the cover letter |
| `--creation-factor` | `N` | Percent for pairing commits in --range-diff (default 60) |
| `-v, --reroll-count` | `N` | Mark the series as version N (`[PATCH vN]`, `vN-` file names) |
| `--cover-letter` |  | Add a cover letter (0000-cover-letter.patch) to fill in |
| `--thread` | `STYLE` | Add Message-ID and In-Reply-To headers: shallow (reply to the first) or deep (reply to the previous) |
| `--in-reply-to` | `ID` | Make the first mail a reply to this Message-ID |
| `--to` | `ADDR` | Add a To: header |
| `--cc` | `ADDR` | Add a Cc: header |
| `--add-header` | `HEADER` | Add this header line |
| `--base` | `COMMIT` | Record the base commit (`auto`: the upstream's merge base) |
| `--zero-commit` |  | Put all zeros in the `From &lt;commit&gt;` line |
| `--signature` | `TEXT` | The signature after `-- ` (default: git's version) |
| `--signature-file` | `FILE` | Read the signature from this file |
| `--no-signature` |  | Leave out the signature |
| `--root` |  | With one revision, format every commit up to it, from the root |
| `-q, --quiet` |  | Do not print the names of the files written |
| `--attach` | `BOUNDARY` | Put each patch in a MIME attachment, with this boundary (default: git's version) |
| `--inline` | `BOUNDARY` | Like --attach, as an inline part |
| `--no-attach` |  | No MIME attachment (overrides format.attach) |
| `--notes` | `REF` | Add the commit's notes after the `---` line (of this notes ref) |
| `--no-notes` |  | No notes (overrides format.notes) |
| `--encode-email-headers` |  | RFC 2047-encode non-ASCII From: and Subject: (the default) |
| `--no-encode-email-headers` |  | Leave non-ASCII From: and Subject: as raw UTF-8 |
| `--output` | `FILE` | Write all the patches to this one file |
| `--no-cover-letter` |  | No cover letter (overrides format.coverLetter) |
| `--no-thread` |  | No threading headers (overrides format.thread) |
| `--force-in-body-from` |  | With --from, keep the author's From: in the body even when it is the sender |
| `--no-force-in-body-from` |  | Only put the author's From: in the body when it is not the sender |
| `--no-to` |  | Drop the To: addresses of format.to |
| `--no-cc` |  | Drop the Cc: addresses of format.cc |

See the [global options](global-options.md) that every command accepts.
