# rgit index

Semantic code search: build the on-disk vector index or query it

## Usage

```sh
rgit index [OPTIONS] <COMMAND>
```

## Examples

```sh
rgit index status
```

## Subcommands

### rgit index build

Build (or incrementally rebuild) the index. Local by default; `--root DIR` builds every repo under a directory

#### Usage

```sh
rgit index build [OPTIONS]
```

#### Examples

```sh
rgit index build
rgit index build --root ~/code
```

#### Options

| Option | Value | Description |
| --- | --- | --- |
| `--root` | `ROOT` | Build every git repo directly under this directory (global) |

### rgit index search

Search the semantic index by meaning (local, or global with `--root`). Results are re-ranked by git history, so recently and frequently changed files surface above cold ones at equal relevance

#### Usage

```sh
rgit index search [OPTIONS] <QUERY>
```

#### Examples

```sh
rgit index search "<query>"
rgit index search "<query>" --limit 5
```

#### Arguments

| Argument | Description |
| --- | --- |
| `<QUERY>` | The natural-language or code query |

#### Options

| Option | Value | Description |
| --- | --- | --- |
| `-l, --limit` | `LIMIT` | Maximum results (default: `8`) |
| `--root` | `ROOT` | Search every git repo directly under this directory (global) |

### rgit index status

Report whether an index exists and how many chunks it holds

#### Usage

```sh
rgit index status [OPTIONS]
```

#### Examples

```sh
rgit index status
```

### rgit index code

Hybrid search: fuse literal grep and semantic ranking, then re-rank by git history (churn and recency) (local, or global with `--root`)

#### Usage

```sh
rgit index code [OPTIONS] <QUERY>
```

#### Examples

```sh
rgit index code "<query>"
```

#### Arguments

| Argument | Description |
| --- | --- |
| `<QUERY>` | The query (literal terms help the lexical side; prose helps semantic) |

#### Options

| Option | Value | Description |
| --- | --- | --- |
| `-l, --limit` | `LIMIT` | Maximum results (default: `8`) |
| `--root` | `ROOT` | Search every git repo directly under this directory (global) |

See the [global options](global-options.md) that every command accepts.
