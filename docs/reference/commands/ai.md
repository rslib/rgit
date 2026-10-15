# rgit ai

Draft or review a commit message with an AI tool you configure, or show the prompt it gets

## Usage

```sh
rgit ai [OPTIONS] <COMMAND>
```

## Examples

```sh
rgit ai draft
rgit ai review
rgit ai prompt draft
```

## Subcommands

### rgit ai draft

Draft a commit message for the staged changes

#### Usage

```sh
rgit ai draft [OPTIONS]
```

#### Examples

```sh
rgit ai draft
rgit ai draft --tool claude
```

#### Options

| Option | Value | Description |
| --- | --- | --- |
| `--tool` | `TOOL` | The AI tool to run (default: commit.ai.draft) |

### rgit ai review

Review the staged changes

#### Usage

```sh
rgit ai review [OPTIONS]
```

#### Examples

```sh
rgit ai review
rgit ai review --tool codex
```

#### Options

| Option | Value | Description |
| --- | --- | --- |
| `--tool` | `TOOL` | The AI tool to run (default: commit.ai.review) |

### rgit ai prompt

Print the prompt a task would use, with the rules filled in

#### Usage

```sh
rgit ai prompt [OPTIONS] <TASK>
```

#### Examples

```sh
rgit ai prompt draft
rgit ai prompt review --tool claude
```

#### Arguments

| Argument | Description |
| --- | --- |
| `<TASK>` | The task |

#### Options

| Option | Value | Description |
| --- | --- | --- |
| `--tool` | `TOOL` | The AI tool whose prompt to show |

See the [global options](global-options.md) that every command accepts.
