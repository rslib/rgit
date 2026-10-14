# rgit hook

Run a repository hook as git would (`git hook run`)

## Usage

```sh
rgit hook [OPTIONS] <COMMAND>
```

## Examples

```sh
rgit hook run pre-commit
rgit hook run --ignore-missing post-merge -- 0
```

## Subcommands

### rgit hook run

Run the hook `name` from the hooks directory (`core.hooksPath`) with the arguments after `--`; exit with its status

#### Usage

```sh
rgit hook run [OPTIONS] <NAME> [-- <ARGS>...]
```

#### Examples

```sh
rgit hook run pre-push -- origin https://example.com/repo.git
rgit hook run --to-stdin=refs.txt reference-transaction -- committed
```

#### Arguments

| Argument | Description |
| --- | --- |
| `<NAME>` | The hook, e.g. pre-commit |
| `[ARGS]...` | Arguments for the hook |

#### Options

| Option | Value | Description |
| --- | --- | --- |
| `--ignore-missing` |  | Succeed quietly when there is no such hook |
| `--to-stdin` | `PATH` | Feed the hook this file on stdin |

See the [global options](global-options.md) that every command accepts.
