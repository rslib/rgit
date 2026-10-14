# rgit stack

Stacked branches: chain branches and restack descendants after edits (no subcommand lists the current stack)

## Usage

```sh
rgit stack [OPTIONS] [COMMAND]
```

## Examples

```sh
rgit stack
```

## Subcommands

### rgit stack new

Create a new branch stacked on the current one

#### Usage

```sh
rgit stack new [OPTIONS] <NAME>
```

#### Examples

```sh
rgit stack new <branch>
```

#### Arguments

| Argument | Description |
| --- | --- |
| `<NAME>` | The new branch name |

### rgit stack list

List the stack containing the current branch

#### Usage

```sh
rgit stack list [OPTIONS]
```

#### Examples

```sh
rgit stack list
```

### rgit stack restack

Rebase every descendant onto its parent's new tip

#### Usage

```sh
rgit stack restack [OPTIONS]
```

#### Examples

```sh
rgit stack restack
```

### rgit stack sync

Retarget the children of merged parents onto the grandparent branch, then restack

#### Usage

```sh
rgit stack sync [OPTIONS]
```

#### Examples

```sh
rgit stack sync
```

See the [global options](global-options.md) that every command accepts.
