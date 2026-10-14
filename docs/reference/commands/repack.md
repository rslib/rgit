# rgit repack

Pack the repository's objects (`git repack`)

## Usage

```sh
rgit repack [OPTIONS]
```

## Examples

```sh
rgit repack -a -d
rgit repack -a -d -b --cruft
rgit repack -a -d -f --window=250 --depth=50
```

## Options

| Option | Value | Description |
| --- | --- | --- |
| `-a` |  | Put everything in one pack |
| `-A` |  | Like -a, but loosen unreachable objects instead of dropping them |
| `-d` |  | Delete the packs and loose objects made redundant |
| `-f` |  | Recompute deltas (git's -f) |
| `-F` |  | Recompress every object (git's -F) |
| `-l, --local` |  | Only local objects, not those of alternates |
| `-k, --keep-unreachable` |  | Keep unreachable objects in the pack |
| `-b, --write-bitmap-index` |  | Write a reachability bitmap (with -a; default repack.writeBitmaps) |
| `--no-write-bitmap-index` |  | Write no bitmap, whatever repack.writeBitmaps says |
| `--cruft` |  | Put unreachable objects in a cruft pack |
| `-g, --geometric` | `FACTOR` | Keep a geometric progression of pack sizes with this factor |
| `--window` | `N` | Delta window size |
| `--depth` | `N` | Maximum delta depth |
| `--window-memory` | `SIZE` | Bytes the delta window may hold (k, m and g suffixes) |
| `--threads` | `N` | Delta search threads (0: one per CPU) |
| `--keep-pack` | `PACK` | Leave this pack (`pack-&lt;hash&gt;.pack`) as it is; repeatable |
| `--cruft-expiration` | `DATE` | With --cruft, drop unreachable objects older than this |
| `--unpack-unreachable` | `DATE` | With -A, only loosen unreachable objects newer than this |
| `-n` |  | Do not update objects/info/packs |
| `-q, --quiet` |  | Print nothing |

See the [global options](global-options.md) that every command accepts.
