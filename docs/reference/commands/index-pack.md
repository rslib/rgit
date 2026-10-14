# rgit index-pack

Build the index for a pack file, like `git index-pack`; `--stdin` stores a pack read from stdin in the repository

## Usage

```sh
rgit index-pack [OPTIONS] [PACK]
```

## Examples

```sh
rgit index-pack received.pack
rgit index-pack --stdin < received.pack
```

## Arguments

| Argument | Description |
| --- | --- |
| `[PACK]` | The pack file |

## Options

| Option | Value | Description |
| --- | --- | --- |
| `-o` | `INDEX` | Write the index here instead of beside the pack |
| `--stdin` |  | Read the pack from stdin |
| `--fix-thin` |  | Accepted for git compatibility (thin packs are completed from the repository's objects) |
| `--keep` | `MSG` | Write a .keep file, with this message |
| `--verify` |  | Check the existing index instead of writing one |
| `--verify-stat` |  | With --verify, list the objects and the delta histogram |
| `--verify-stat-only` |  | With --verify, only the delta histogram |
| `--rev-index` |  | Write a .rev reverse index (default pack.writeReverseIndex) |
| `--no-rev-index` |  | Do not write a .rev reverse index |

See the [global options](global-options.md) that every command accepts.
