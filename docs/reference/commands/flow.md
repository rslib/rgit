# rgit flow

Branching workflows: pick a preset (gitflow, github, gitlab, trunk, release-flow); start/finish/release then follow its rules

## Usage

```sh
rgit flow [OPTIONS] <COMMAND>
```

## Examples

```sh
rgit flow status
```

## Subcommands

### rgit flow init

Set the active workflow: gitflow, github, gitlab, trunk, release-flow

#### Usage

```sh
rgit flow init [OPTIONS] <PRESET>
```

#### Examples

```sh
rgit flow init gitflow
```

#### Arguments

| Argument | Description |
| --- | --- |
| `<PRESET>` | The workflow preset name |

### rgit flow start

Start a feature branch per the active workflow

#### Usage

```sh
rgit flow start [OPTIONS] <NAME>
```

#### Examples

```sh
rgit flow start <name>
```

#### Arguments

| Argument | Description |
| --- | --- |
| `<NAME>` | The feature name |

### rgit flow finish

Finish the current feature (local merge, or push + PR per the workflow)

#### Usage

```sh
rgit flow finish [OPTIONS]
```

#### Examples

```sh
rgit flow finish
```

### rgit flow release

Start a release (or finish it with --finish)

#### Usage

```sh
rgit flow release [OPTIONS] <VERSION>
```

#### Examples

```sh
rgit flow release 1.2.0
rgit flow release 1.2.0 --finish
```

#### Options

| Option | Value | Description |
| --- | --- | --- |
| `--finish` |  | Finish the release instead of starting it |

### rgit flow status

Show the active workflow and its policy

#### Usage

```sh
rgit flow status [OPTIONS]
```

#### Examples

```sh
rgit flow status
```

See the [global options](global-options.md) that every command accepts.
