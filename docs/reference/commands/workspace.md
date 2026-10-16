# rgit workspace

Copy-on-write workspaces: instant, isolated, block-sharing clones of the whole repo (code, build, .git) for parallel work (no subcommand lists)

## Usage

```sh
rgit workspace [OPTIONS] [COMMAND]
```

## Examples

```sh
rgit workspace
```

## Subcommands

### rgit workspace new

Create a copy-on-write clone of the repo on a new branch

#### Usage

```sh
rgit workspace new [OPTIONS] <NAME>
```

#### Examples

```sh
rgit workspace new <name>
```

#### Arguments

| Argument | Description |
| --- | --- |
| `<NAME>` | The workspace name (also the new branch name) |

### rgit workspace list

List this repo's workspaces

#### Usage

```sh
rgit workspace list [OPTIONS]
```

#### Examples

```sh
rgit workspace list
```

### rgit workspace remove

Remove a workspace

#### Usage

```sh
rgit workspace remove [OPTIONS] <NAME>
```

#### Examples

```sh
rgit workspace remove <name>
```

#### Arguments

| Argument | Description |
| --- | --- |
| `<NAME>` | The workspace name |

### rgit workspace pull

Fast-forward the main repository's branch to the workspace's commits

#### Usage

```sh
rgit workspace pull [OPTIONS] <NAME>
```

#### Examples

```sh
rgit workspace pull <name>
rgit workspace pull <name> --into <branch>
```

#### Arguments

| Argument | Description |
| --- | --- |
| `<NAME>` | The workspace name |

#### Options

| Option | Value | Description |
| --- | --- | --- |
| `--into` | `INTO` | The main repository's branch to advance (default: the workspace's branch) |

### rgit workspace push

Fast-forward the workspace to the main repository's branch of the same name

#### Usage

```sh
rgit workspace push [OPTIONS] <NAME>
```

#### Examples

```sh
rgit workspace push <name>
```

#### Arguments

| Argument | Description |
| --- | --- |
| `<NAME>` | The workspace name |

See the [global options](global-options.md) that every command accepts.
