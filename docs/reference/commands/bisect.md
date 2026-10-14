# rgit bisect

Find the commit that introduced a change by binary search: `start <bad> <good>`, then mark each step `good`/`bad` (or `run <cmd>`), then `reset`

## Usage

```sh
rgit bisect [OPTIONS] <COMMAND>
```

## Examples

```sh
rgit bisect start HEAD main
rgit bisect good
rgit bisect bad
rgit bisect run cargo test
rgit bisect reset
```

## Subcommands

### rgit bisect start

Start a bisect, optionally with the bad commit and good ones

#### Usage

```sh
rgit bisect start [OPTIONS] [REVS]... [-- <PATH>...]
```

#### Examples

```sh
rgit bisect start
rgit bisect start <bad> <good>
rgit bisect start <bad> <good> -- src/
rgit bisect start --term-new fixed --term-old broken
```

#### Arguments

| Argument | Description |
| --- | --- |
| `[REVS]...` | The bad (new) commit, then good (old) ones |
| `[PATH]...` | Only test commits that touch these paths (after `--`) |

#### Options

| Option | Value | Description |
| --- | --- | --- |
| `--term-new` | `TERM` | The word for the new state instead of `bad` (e.g. `fixed`) |
| `--term-old` | `TERM` | The word for the old state instead of `good` (e.g. `broken`) |
| `--no-checkout` |  | Leave the working tree alone; move BISECT_HEAD to each step instead |
| `--first-parent` |  | Follow only the first parent of merge commits |

### rgit bisect bad

Mark commits (HEAD by default) as bad: the change is there

#### Usage

```sh
rgit bisect bad [OPTIONS] [REVS]...
```

#### Examples

```sh
rgit bisect bad
rgit bisect bad <rev>
```

#### Arguments

| Argument | Description |
| --- | --- |
| `[REVS]...` |  |

### rgit bisect good

Mark commits (HEAD by default) as good: the change is not there yet

#### Usage

```sh
rgit bisect good [OPTIONS] [REVS]...
```

#### Examples

```sh
rgit bisect good
rgit bisect good <a> <b>
```

#### Arguments

| Argument | Description |
| --- | --- |
| `[REVS]...` |  |

### rgit bisect new

Mark commits as new (with `--term-new`/`--term-old` terms)

#### Usage

```sh
rgit bisect new [OPTIONS] [REVS]...
```

#### Examples

```sh
rgit bisect new
```

#### Arguments

| Argument | Description |
| --- | --- |
| `[REVS]...` |  |

### rgit bisect old

Mark commits as old (with `--term-new`/`--term-old` terms)

#### Usage

```sh
rgit bisect old [OPTIONS] [REVS]...
```

#### Examples

```sh
rgit bisect old
```

#### Arguments

| Argument | Description |
| --- | --- |
| `[REVS]...` |  |

### rgit bisect skip

Skip commits (HEAD by default) or ranges `A..B` that cannot be tested

#### Usage

```sh
rgit bisect skip [OPTIONS] [REVS]...
```

#### Examples

```sh
rgit bisect skip
rgit bisect skip <a>..<b>
```

#### Arguments

| Argument | Description |
| --- | --- |
| `[REVS]...` |  |

### rgit bisect reset

End the bisect and check out where it started (or `commit`)

#### Usage

```sh
rgit bisect reset [OPTIONS] [COMMIT]
```

#### Examples

```sh
rgit bisect reset
rgit bisect reset <rev>
```

#### Arguments

| Argument | Description |
| --- | --- |
| `[COMMIT]` |  |

### rgit bisect next

Check out the next commit to test (after marking commits by hand)

#### Usage

```sh
rgit bisect next [OPTIONS]
```

#### Examples

```sh
rgit bisect next
```

### rgit bisect log

Print the bisect log, to save for `replay`

#### Usage

```sh
rgit bisect log [OPTIONS]
```

#### Examples

```sh
rgit bisect log > bisect.log
```

### rgit bisect replay

Redo the bisect a saved log records

#### Usage

```sh
rgit bisect replay [OPTIONS] <FILE>
```

#### Examples

```sh
rgit bisect replay bisect.log
```

#### Arguments

| Argument | Description |
| --- | --- |
| `<FILE>` |  |

### rgit bisect run

Mark each step by running a command: exit 0 good, 125 skip, other bad

#### Usage

```sh
rgit bisect run [OPTIONS] <CMD>...
```

#### Examples

```sh
rgit bisect run cargo test
rgit bisect run ./check.sh --quick
```

#### Arguments

| Argument | Description |
| --- | --- |
| `<CMD>...` |  |

### rgit bisect visualize

List the commits still in the search

#### Usage

```sh
rgit bisect visualize [OPTIONS]
```

#### Examples

```sh
rgit bisect visualize
rgit bisect view
```

### rgit bisect terms

Print the terms for the old and new states

#### Usage

```sh
rgit bisect terms [OPTIONS]
```

#### Examples

```sh
rgit bisect terms
rgit bisect terms --term-good
```

#### Options

| Option | Value | Description |
| --- | --- | --- |
| `--term-good` |  | Print only the term for good (old) commits |
| `--term-bad` |  | Print only the term for bad (new) commits |

See the [global options](global-options.md) that every command accepts.
