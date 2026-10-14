# rgit fsck

Check the object database for corruption and dangling objects

## Usage

```sh
rgit fsck [OPTIONS] [OBJECTS]...
```

## Examples

```sh
rgit fsck
rgit fsck --unreachable --no-reflogs
rgit fsck --lost-found
rgit fsck --name-objects --full
```

## Arguments

| Argument | Description |
| --- | --- |
| `[OBJECTS]...` | Only these objects (default: the refs, index and reflogs) |

## Options

| Option | Value | Description |
| --- | --- | --- |
| `--full` |  | Also check packed objects (the default) |
| `--no-full` |  | Only check loose objects' contents |
| `--strict` |  | Strict checking |
| `--unreachable` |  | List unreachable objects |
| `--no-dangling` |  | Do not report dangling objects |
| `--connectivity-only` |  | Check only that objects are connected |
| `--lost-found` |  | Write dangling objects to .git/lost-found/{commit,other} |
| `--name-objects` |  | Name each object by how it is reached (e.g. `HEAD~2^{tree}`) |
| `--root` |  | Report root commits |
| `--tags` |  | Report tags |
| `--cache` |  | Also check the index |
| `--no-reflogs` |  | Do not treat reflog entries as reachable |

See the [global options](global-options.md) that every command accepts.
