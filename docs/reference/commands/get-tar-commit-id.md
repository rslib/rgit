# rgit get-tar-commit-id

Print the commit id `git archive` stored in the tar read from stdin, like `git get-tar-commit-id`; exit 1 when it has none

## Usage

```sh
rgit get-tar-commit-id [OPTIONS]
```

## Examples

```sh
rgit get-tar-commit-id < release.tar
```

See the [global options](global-options.md) that every command accepts.
