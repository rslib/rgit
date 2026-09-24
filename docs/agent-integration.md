# Using rgit from an agent

rgit is built to be driven by coding agents through the shell as well as by
people at a terminal.

## Output modes

- TOON is the default when stdout is not a TTY. It is compact and structured,
  and it ends with `help` lines that name the next useful command.
- `--json` prints the same data as JSON.
- `--human` forces the terminal text form, even when piped.
- `--toon` (alias `--axi`) forces TOON, even on a terminal. Agents should use
  it rather than `--porcelain`, which, as in git, exists only on `status` and
  prints git's raw script format.
- `--fields a,b` adds columns to a table (an unknown field lists the valid ones).
- `--full` prints long text (patches, commit bodies) without truncation.

Usage errors exit 2, other errors exit 1, and both print `error` and `help` on
stdout in the same format. Mutations whose target state already holds (creating
an existing branch, deleting a missing tag) succeed with a `(no-op)` note.

`rgit` with no subcommand prints a home view of the repository in the current
directory: binary, description, status and help. Outside a repository it says
so and still exits 0.

## Integrations

You need only one of these. The hook is the better choice where the app
supports it.

### 1. Session hook (preferred)

```sh
rgit hooks install            # this project, all apps
rgit hooks install --user     # your home directory
rgit hooks install --app claude
rgit hooks status
```

This runs `rgit --toon` at session start, so the agent sees the repo state
without asking:

| App         | Project file                   | User file                           |
| ----------- | ------------------------------ | ----------------------------------- |
| Claude Code | `.claude/settings.json`        | `~/.claude/settings.json`           |
| Codex       | `.codex/hooks.json`, `.codex/config.toml` | `~/.codex/hooks.json`, `~/.codex/config.toml` |
| OpenCode    | `.opencode/plugins/rgit.js`    | `~/.config/opencode/plugins/rgit.js` |

Claude Code and Codex get a `SessionStart` command hook. For Codex, rgit also
sets `[features].hooks = true`, and Codex asks you to trust project hooks with
`/hooks` before they run. OpenCode gets a small managed plugin that listens for
`session.created` and adds the same text to the new session as a context-only
message (`noReply: true`), so it does not start a model reply.

Installing again is safe. It changes nothing when the hook is current, fixes
the executable path when rgit has moved, and leaves your other settings and
hooks alone. The hook uses the bare name `rgit` when that is on PATH, else the
absolute path.

### 2. Agent Skill

```sh
rgit skills install           # or: npx skills add rslib/rgit --skill rgit
```

This installs `skills/rgit/SKILL.md` plus `references/commands.md`, which tells the agent how to use rgit when
the task needs it.

## Session history

rgit records every operation in its op-log (`rgit oplog`), so there is no
session-end hook to install.
