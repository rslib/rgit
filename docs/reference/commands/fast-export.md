# rgit fast-export

Print history as a fast-import stream, like `git fast-export`: revisions and ranges (`--all`, `A..B`, `^A`), `--signed-tags=`, `--signed-commits=`, `--tag-of-filtered-object=`, `--reencode=`, `--export-marks=`, `--import-marks[-if-exists]=`, `--no-data`, `--full-tree`, `--use-done-feature`, `--fake-missing-tagger`, `--refspec`, `--reference-excluded-parents`, `--show-original-ids`, `--mark-tags`, then `-- <paths>`

## Usage

```sh
rgit fast-export [OPTIONS] [ARGS]...
```

## Examples

```sh
rgit fast-export --all > repo.stream
rgit fast-export --no-data main~5..main
rgit fast-export --export-marks=marks main -- src
```

## Arguments

| Argument | Description |
| --- | --- |
| `[ARGS]...` |  |

See the [global options](global-options.md) that every command accepts.
