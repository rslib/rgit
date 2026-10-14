# rgit commit-graph

Write or check the commit-graph file or chain (`git commit-graph`)

## Usage

```sh
rgit commit-graph [OPTIONS] <COMMAND>
```

## Examples

```sh
rgit commit-graph write --reachable --changed-paths
rgit commit-graph verify
```

## Subcommands

### rgit commit-graph write

Write the commit-graph: the commits in every pack, or those the refs reach, or those given on stdin

#### Usage

```sh
rgit commit-graph write [OPTIONS]
```

#### Examples

```sh
rgit commit-graph write --reachable
rgit commit-graph write --reachable --split --size-multiple=4
rgit rev-list --all | rgit commit-graph write --stdin-commits --append
```

#### Options

| Option | Value | Description |
| --- | --- | --- |
| `--reachable` |  | The commits the refs reach |
| `--stdin-packs` |  | The commits in the packs named on stdin |
| `--stdin-commits` |  | The commits named on stdin (and what they reach) |
| `--append` |  | Keep the commits already in the graph |
| `--split` | `STRATEGY` | Add a layer to a split chain: merge layers as git does, `no-merge` never, `replace` into one new layer |
| `--size-multiple` | `N` | Merge a layer that is at most this many times the new one (2) |
| `--max-commits` | `N` | Merge layers while the new one would pass this many commits |
| `--expire-time` | `DATE` | Only remove unused layer files older than this date |
| `--changed-paths` |  | Write changed-path Bloom filters |
| `--no-changed-paths` |  | Write none, even if the graph has them |
| `--no-progress` |  | Accepted for git compatibility; rgit prints no progress |

### rgit commit-graph verify

Check the commit-graph against the commits it lists

#### Usage

```sh
rgit commit-graph verify [OPTIONS]
```

#### Examples

```sh
rgit commit-graph verify
rgit commit-graph verify --shallow
```

#### Options

| Option | Value | Description |
| --- | --- | --- |
| `--shallow` |  | Only the top layer of a split chain |
| `--no-progress` |  | Accepted for git compatibility; rgit prints no progress |

See the [global options](global-options.md) that every command accepts.
