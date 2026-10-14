# rgit request-pull

Summarize changes for a pull request by mail (`git request-pull`): what `url` holds beyond `start`

## Usage

```sh
rgit request-pull [OPTIONS] <START> <URL> [END]
```

## Examples

```sh
rgit request-pull origin/main https://example.com/me/repo.git topic
rgit request-pull -p v1.0 origin
```

## Arguments

| Argument | Description |
| --- | --- |
| `<START>` | Where the changes start (e.g. origin/main) |
| `<URL>` | The repository to pull from |
| `[END]` | What to pull: a branch or tag pushed to `url` (default HEAD); `local:remote` when the names differ |

## Options

| Option | Value | Description |
| --- | --- | --- |
| `-p` |  | Add the patch after the diffstat |

See the [global options](global-options.md) that every command accepts.
