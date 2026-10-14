# rgit lanes

Lanes: several lines of work in one worktree. Assign uncommitted files to lanes and commit each to its own branch (no subcommand lists the lanes)

## Usage

```sh
rgit lanes [OPTIONS] [COMMAND]
```

## Examples

```sh
rgit lanes
```

## Subcommands

### rgit lanes init

Enter lanes mode: record the fork point and a default lane

#### Usage

```sh
rgit lanes init [OPTIONS]
```

#### Examples

```sh
rgit lanes init
```

### rgit lanes off

Leave lanes mode (lane branches are kept)

#### Usage

```sh
rgit lanes off [OPTIONS]
```

#### Examples

```sh
rgit lanes off
```

### rgit lanes list

List the lanes and their owned files (the default when no subcommand)

#### Usage

```sh
rgit lanes list [OPTIONS]
```

#### Examples

```sh
rgit lanes list
```

### rgit lanes new

Create a new lane committing to a same-named branch

#### Usage

```sh
rgit lanes new [OPTIONS] <NAME>
```

#### Examples

```sh
rgit lanes new <lane>
```

#### Arguments

| Argument | Description |
| --- | --- |
| `<NAME>` | The lane (and branch) name |

### rgit lanes stack

Create a new lane stacked on another (its commits build on that lane)

#### Usage

```sh
rgit lanes stack [OPTIONS] --on <ON> <NAME>
```

#### Examples

```sh
rgit lanes stack <lane> --on <parent_lane>
```

#### Arguments

| Argument | Description |
| --- | --- |
| `<NAME>` | The new lane name |

#### Options

| Option | Value | Description |
| --- | --- | --- |
| `--on` | `ON` | The parent lane to stack on |

### rgit lanes assign

Assign a worktree path to a lane, or a single hunk with `--hunk`

#### Usage

```sh
rgit lanes assign [OPTIONS] <LANE> <PATH>
```

#### Examples

```sh
rgit lanes assign <lane> src/lib.rs
rgit lanes assign <lane> src/lib.rs --hunk 10
```

#### Arguments

| Argument | Description |
| --- | --- |
| `<LANE>` | The lane to assign to |
| `<PATH>` | The path to assign |

#### Options

| Option | Value | Description |
| --- | --- | --- |
| `--hunk` | `HUNK` | Assign only the hunk starting at this new-file line, not the file |

### rgit lanes unassign

Return a path to the default lane

#### Usage

```sh
rgit lanes unassign [OPTIONS] <PATH>
```

#### Examples

```sh
rgit lanes unassign src/lib.rs
```

#### Arguments

| Argument | Description |
| --- | --- |
| `<PATH>` | The path to unassign |

### rgit lanes commit

Commit a lane's owned changes to its branch

#### Usage

```sh
rgit lanes commit [OPTIONS] --message <MESSAGE> <LANE>
```

#### Examples

```sh
rgit lanes commit <lane> -m "<message>"
```

#### Arguments

| Argument | Description |
| --- | --- |
| `<LANE>` | The lane to commit |

#### Options

| Option | Value | Description |
| --- | --- | --- |
| `-m, --message` | `MESSAGE` | The commit message |

### rgit lanes rename

Rename a lane and its branch

#### Usage

```sh
rgit lanes rename [OPTIONS] <OLD> <NEW>
```

#### Examples

```sh
rgit lanes rename <old> <new>
```

#### Arguments

| Argument | Description |
| --- | --- |
| `<OLD>` | The lane to rename |
| `<NEW>` | The new name |

### rgit lanes delete

Delete a lane (its changes return to default; its branch is kept)

#### Usage

```sh
rgit lanes delete [OPTIONS] <NAME>
```

#### Examples

```sh
rgit lanes delete <lane>
```

#### Arguments

| Argument | Description |
| --- | --- |
| `<NAME>` | The lane to delete |

### rgit lanes push

Push a lane's branch to the remote

#### Usage

```sh
rgit lanes push [OPTIONS] <LANE>
```

#### Examples

```sh
rgit lanes push <lane>
```

#### Arguments

| Argument | Description |
| --- | --- |
| `<LANE>` | The lane to push |

### rgit lanes pr

Push a lane's branch and open or update its pull request

#### Usage

```sh
rgit lanes pr [OPTIONS] <LANE>
```

#### Examples

```sh
rgit lanes pr <lane>
```

#### Arguments

| Argument | Description |
| --- | --- |
| `<LANE>` | The lane to open a PR for |

### rgit lanes restack

Move each stacked lane onto its parent lane's new tip (in the odb; the worktree is not touched)

#### Usage

```sh
rgit lanes restack [OPTIONS]
```

#### Examples

```sh
rgit lanes restack
```

See the [global options](global-options.md) that every command accepts.
