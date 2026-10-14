# rgit update-ref

Point a ref at a commit, or delete it, optionally only if it holds an expected value

## Usage

```sh
rgit update-ref [OPTIONS] [NAME] [NEW] [OLD]
```

## Examples

```sh
rgit update-ref refs/heads/topic <rev>
rgit update-ref refs/heads/topic <new> <old>
rgit update-ref -d refs/heads/topic
printf 'start\nupdate refs/heads/a <new> <old>\ncommit\n' | rgit update-ref --stdin
rgit update-ref --create-reflog refs/backup/main main
printf 'symref-update refs/heads/alias refs/heads/main\n' | rgit update-ref --stdin
printf 'create refs/heads/a <rev>\ncreate refs/heads/b <rev>\n' | rgit update-ref --stdin --batch-updates
```

## Arguments

| Argument | Description |
| --- | --- |
| `[NAME]` | The ref (e.g. refs/heads/main) |
| `[NEW]` | The new value (with -d: the expected old value) |
| `[OLD]` | Only update if the ref holds this value (all zeros: does not exist) |

## Options

| Option | Value | Description |
| --- | --- | --- |
| `-d` |  | Delete the ref |
| `--no-deref` |  | Update a symbolic ref itself instead of the ref it points to |
| `-m` | `REASON` | The reflog message |
| `--create-reflog` |  | Write a reflog even for refs outside refs/heads, remotes and notes |
| `--stdin` |  | Read `update`, `create`, `delete`, `verify`, `symref-update`, `symref-create`, `symref-delete`, `symref-verify`, `option no-deref` and `start`/`prepare`/`commit`/`abort` lines from stdin, applied all or nothing |
| `-z` |  | With --stdin, NUL-separated fields and commands |
| `-0, --batch-updates` |  | With --stdin, apply the updates that pass their checks and print `rejected &lt;ref&gt; &lt;new&gt; &lt;old&gt; &lt;reason&gt;` for the others |

See the [global options](global-options.md) that every command accepts.
