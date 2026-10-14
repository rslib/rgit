# rgit mergetool

Resolve conflicts in the configured merge tool (`git mergetool`): merge.tool, mergetool.<tool>.cmd or a known tool

## Usage

```sh
rgit mergetool [OPTIONS] [REVS]... [-- <PATHS>...]
```

## Examples

```sh
rgit mergetool
rgit mergetool -t vimdiff -- src/lib.rs
```

## Arguments

| Argument | Description |
| --- | --- |
| `[REVS]...` | difftool: up to two revisions to compare (default: the index against the working tree). mergetool: the conflicted paths |
| `[PATHS]...` | Limit to these paths |

## Options

| Option | Value | Description |
| --- | --- | --- |
| `-t, --tool` | `TOOL` | The tool (default diff.tool / merge.tool) |
| `-x, --extcmd` | `COMMAND` | difftool: run this command with the two files instead of a tool |
| `-y, --no-prompt` |  | Do not ask before each file |
| `--prompt` |  | Ask before each file even if configured not to |
| `--cached` |  | difftool: compare the index with HEAD (or the revision) |
| `-d, --dir-diff` |  | difftool: compare whole folders at once |
| `--trust-exit-code` |  | Stop when the tool exits with an error |
| `--tool-help` |  | List the known tools |

See the [global options](global-options.md) that every command accepts.
