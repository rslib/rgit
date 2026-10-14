# rgit for-each-repo

Run an rgit command in every repository a multi-valued config key lists (`git for-each-repo`), e.g. `--config=maintenance.repo`

## Usage

```sh
rgit for-each-repo [OPTIONS] --config <KEY> <ARGS>...
```

## Examples

```sh
rgit for-each-repo --config=maintenance.repo maintenance run --schedule=daily
```

## Arguments

| Argument | Description |
| --- | --- |
| `<ARGS>...` | The rgit command and its arguments |

## Options

| Option | Value | Description |
| --- | --- | --- |
| `--config` | `KEY` | The config key naming the repositories |
| `--keep-going` |  | Go on after a repository fails (exit 1 at the end) |

See the [global options](global-options.md) that every command accepts.
