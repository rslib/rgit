# rgit ls-remote

List the refs in a remote repository, like `git ls-remote`

## Usage

```sh
rgit ls-remote [OPTIONS] [REPOSITORY] [PATTERNS]...
```

## Examples

```sh
rgit ls-remote
rgit ls-remote --heads origin
rgit ls-remote --tags https://github.com/example/repo.git 'v1.*'
rgit ls-remote --symref origin HEAD
```

## Arguments

| Argument | Description |
| --- | --- |
| `[REPOSITORY]` | The remote name or URL (defaults to the current branch's remote) |
| `[PATTERNS]...` | Only refs whose name ends in one of these (globs allowed: `v1.*`) |

## Options

| Option | Value | Description |
| --- | --- | --- |
| `-b, --heads` |  | Only branches (refs/heads) |
| `-t, --tags` |  | Only tags (refs/tags) |
| `--refs` |  | Leave out peeled tags (`^{}`) and HEAD |
| `--symref` |  | Also show what symbolic refs point at |
| `-q, --quiet` |  | Do not print the remote's URL |
| `--exit-code` |  | Exit with status 2 when no ref matches |
| `--get-url` |  | Print the remote's URL instead of listing refs |

See the [global options](global-options.md) that every command accepts.
