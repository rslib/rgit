# rgit archive

Write a tar or zip of a revision's files

## Usage

```sh
rgit archive [OPTIONS] [REV] [PATHS]...
```

## Examples

```sh
rgit archive -o release.tar.gz
rgit archive v1.0 --prefix project/ -o project.zip
rgit archive HEAD src > src.tar
rgit archive -9 --add-file=VERSION --format=tar.gz -o dist.tgz v1.0
rgit archive --remote ../other -o other.zip
rgit archive -v --remote=git@example.com:me/repo.git -o repo.tar.gz v1.0
rgit archive -l
```

## Arguments

| Argument | Description |
| --- | --- |
| `[REV]` | The revision (default HEAD); `-0` to `-9` set the compression level |
| `[PATHS]...` | Limit to these paths |

## Options

| Option | Value | Description |
| --- | --- | --- |
| `--format` | `FORMAT` | tar, tgz, tar.gz, zip or a tar.&lt;format&gt;.command filter (default: from -o's extension, else tar) |
| `-o, --output` | `OUTPUT` | Write to this file instead of stdout |
| `--prefix` | `PREFIX` | Put every entry under this folder (e.g. `project/`) |
| `--add-file` | `FILE` | Add this untracked file (under the prefix) |
| `--add-virtual-file` | `PATH:CONTENT` | Add a file with this content: `&lt;path&gt;:&lt;content&gt;` |
| `--worktree-attributes` |  | Also honour the working tree's .gitattributes (export-ignore, export-subst) |
| `-l, --list` |  | List the formats |
| `--remote` | `REPO` | Archive from this repository instead: a remote name, a local path, or an ssh:// , git:// or `host:path` URL (git's upload-archive protocol) |
| `--exec` | `COMMAND` | With --remote, the remote's upload-archive command |
| `--mtime` | `TIME` | The entries' modification time (a date, `now`, `2.weeks.ago`, ...) |
| `-v, --verbose` |  | Print each archived path on stderr |

See the [global options](global-options.md) that every command accepts.
