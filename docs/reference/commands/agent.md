# rgit agent

Set agent apps (Claude Code, Codex, OpenCode, pi, omp) up for rgit

## Usage

```sh
rgit agent [OPTIONS] <COMMAND>
```

## Examples

```sh
rgit agent install
rgit agent status
```

## Subcommands

### rgit agent install

Install or repair the rgit skill, session hook and tools in agent apps (the ones found on this machine by default)

#### Usage

```sh
rgit agent install [OPTIONS] [APPS]...
```

#### Examples

```sh
rgit agent install
rgit agent install --all-tools codex
rgit agent install --project codex opencode
```

#### Arguments

| Argument | Description |
| --- | --- |
| `[APPS]...` | Apps to set up; by default every one found on PATH or by its config folder |

#### Options

| Option | Value | Description |
| --- | --- | --- |
| `--project` |  | This project's config instead of your user config (codex and opencode only) |
| `--no-tools` |  | Add no tools; tools already installed stay |
| `--all-tools` |  | Install all 200 tools for Codex and OpenCode instead of the 10 core tools they get by default |

### rgit agent status

Show each app's plugin or extension, skill, hook and tools state

#### Usage

```sh
rgit agent status [OPTIONS]
```

#### Examples

```sh
rgit agent status
```

### rgit agent uninstall

Remove what `rgit agent install` added (from every app by default)

#### Usage

```sh
rgit agent uninstall [OPTIONS] [APPS]...
```

#### Examples

```sh
rgit agent uninstall codex
rgit agent uninstall --tools-only
```

#### Arguments

| Argument | Description |
| --- | --- |
| `[APPS]...` | Apps to remove rgit from; by default all of them |

#### Options

| Option | Value | Description |
| --- | --- | --- |
| `--project` |  | This project's config instead of your user config (codex and opencode only) |
| `--skill-only` |  | Remove only the skill |
| `--hook-only` |  | Remove only the session hook |
| `--tools-only` |  | Remove only the tools |

### rgit agent skill

Print the rgit SKILL.md, or its command reference

#### Usage

```sh
rgit agent skill [OPTIONS]
```

#### Examples

```sh
rgit agent skill
rgit agent skill --reference
```

#### Options

| Option | Value | Description |
| --- | --- | --- |
| `--reference` |  | Print references/commands.md instead of SKILL.md |

See the [global options](global-options.md) that every command accepts.
