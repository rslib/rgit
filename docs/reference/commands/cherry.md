# rgit cherry

Commits not yet upstream (`git cherry`): `+ <id>` for each, `- <id>` when upstream already has an equivalent change

## Usage

```sh
rgit cherry [OPTIONS] [UPSTREAM] [HEAD] [LIMIT]
```

## Examples

```sh
rgit cherry
rgit cherry -v origin/main topic
```

## Arguments

| Argument | Description |
| --- | --- |
| `[UPSTREAM]` | The branch to compare with (default: the upstream of HEAD) |
| `[HEAD]` | The commits to look at (default HEAD) |
| `[LIMIT]` | Leave out the commits up to this one |

## Options

| Option | Value | Description |
| --- | --- | --- |
| `-v, --verbose` |  | Add each commit's subject |

See the [global options](global-options.md) that every command accepts.
