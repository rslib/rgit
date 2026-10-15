# Using rgit from an agent

rgit is built to be driven by coding agents through the shell as well as by
people at a terminal.

## Output modes

- Human text is what a person's shell gets, whether or not stdout is a
  terminal. Plain `status`, `diff`, `log`, `show` and `blame` print exactly
  what git prints. Color, the pager and prompts still need a real terminal.
- When an agent harness (Claude Code, Codex, Gemini CLI, Cursor, OpenCode,
  goose, amp or pi) runs rgit it prints TOON by default: compact, structured,
  and ending with `help` lines that name the next useful command. Those hints
  already carry an output flag, so following them stays in the same mode.
- `--toon` (alias `--axi`) picks TOON explicitly and still works; `--json`
  prints the same data as JSON, and its hints carry `--json`.
- `--human` (or `RGIT_OUTPUT=human`, or `output.format = "human"` in the rgit config file) prints
  git's text, as does a shell no harness marker names.
- Use `--toon` rather than `--porcelain`, which, as in git, exists only on
  `status` and prints git's raw script format.
- `--compact` (or `output.compact = true` in the rgit config file) prints rgit's older compact
  human forms of `status`, `diff`, `log`, `show` and `blame`.
- `--fields a,b` adds columns to a table (an unknown field lists the valid ones).
- `--full` prints long text (patches, commit bodies) without truncation.

Usage errors exit 2, other errors exit 1. With `--toon` or `--json` both print
`error` and `help` on stdout in the same format; human text errors go to
stderr as git words them. Mutations whose target state already holds (creating
an existing branch, deleting a missing tag) succeed with a `(no-op)` note.

`rgit` with no subcommand prints a home view of the repository in the current
directory: binary, description, status and help, in TOON under an agent
harness. Outside a repository it says so and still exits 0. In a person's
shell with no terminal it prints git's usage text and exits 1, as `git` does.

## Integrations

```sh
rgit agent install              # every agent app found on this machine
rgit agent install claude pi    # just these
rgit agent install --no-tools   # skill and hook only (tools already there stay)
rgit agent install --project    # this project's Codex and OpenCode config
rgit agent status               # what each app has, and whether it is current
rgit agent uninstall            # remove it all again (or --tools-only, --hook-only, --skill-only)
```

With no app named, `install` sets up each app whose program is on PATH or
whose config folder exists. Each app gets what it supports of three parts:

- **Skill**: the rgit Agent Skill (`SKILL.md` plus `references/commands.md`),
  which tells the agent how to use rgit, including that TOON is already the
  default under an agent harness.
- **Session hook**: runs `rgit hook session-start` when a session starts, so
  the agent sees the repository state (`rgit --toon`'s home view) without
  asking.
- **Tools**: rgit's operations as the agent's own tools. Claude Code gets
  all 200 through a plugin mod (deferred, so only names load until the
  model asks for one). Pi gets all 200 over MCP with the 10 core tools
  (status, diff, log, show, stage, unstage, commit, smartlog, undo, grep)
  in the prompt. Omp, Codex and OpenCode get the core set: omp as native
  tools, Codex and OpenCode through `rgit mcp --core`. With
  `--all-tools`, Codex and OpenCode get plain `rgit mcp` with all 200.

| App         | What rgit writes (user scope)                                                                                  | Skill                   | Hook                                  | Tools                                                |
| ----------- | -------------------------------------------------------------------------------------------------------------- | ----------------------- | ------------------------------------- | ---------------------------------------------------- |
| Claude Code | A plugin in a local marketplace, `$XDG_DATA_HOME/rgit/claude-plugin`, installed with `claude plugin install rgit@rgit` | in the plugin           | plugin `hooks/hooks.json`, `SessionStart` | plugin mod `hooks/register.tsx`                      |
| Codex       | `~/.agents/skills/rgit`, `~/.codex/hooks.json`, `~/.codex/config.toml`                                          | `~/.agents/skills/rgit` | `SessionStart` in `hooks.json`        | `[mcp_servers.rgit]` (`rgit mcp --core`)             |
| OpenCode    | `~/.config/opencode/skills/rgit`, `plugins/rgit.js`, `opencode.json`                                           | `skills/rgit`           | a plugin on `session.created`         | `mcp.rgit` (`type: local`, `rgit mcp --core`)        |
| pi          | `~/.pi/agent/skills/rgit`, `~/.pi/agent/extensions/rgit.ts`                                                    | `skills/rgit`           | the extension, on `session_start`     | the extension's MCP server (core tools direct)       |
| omp         | `~/.omp/agent/skills/rgit`, `~/.omp/agent/extensions/rgit.ts`                                                  | `skills/rgit`           | the extension, on `session_start`     | the extension's native core tools                    |

`--project` writes the Codex and OpenCode files into the current project
instead (`.agents/skills/rgit`, `.codex/`, `.opencode/`, `opencode.json`);
Codex asks you to trust project hooks with `/hooks` before they run. Claude
Code, pi and omp load plugins and extensions per user, so they have no
project scope.

The plugin version carries a hash of its files, so after rgit changes (a new
skill, or rgit moved), `rgit agent install` runs `claude plugin update`;
restart Claude Code to load it. For Claude Code, install also removes what
older rgit versions wrote into `settings.json` and `~/.claude/skills/rgit`.
For Codex, rgit turns `[features].hooks` back on only when you set it to
false. The pi and omp extension is built from
[`extensions/rgit/src/rgit.ts`](../extensions/rgit/src/rgit.ts); the Claude
Code mod lives in
[`extensions/claude/hooks/register.tsx`](../extensions/claude/hooks/register.tsx).

Installing again is safe. It changes nothing when everything is current,
repairs entries that point at an old rgit path, and leaves your other
settings, hooks and servers alone. Hooks and entries use the bare name
`rgit` when that is on PATH, else the absolute path. rgit edits plain JSON
only: when a config file has comments (`opencode.jsonc`), it stops and asks
you to make the change by hand. `uninstall` removes only files and entries
rgit wrote.

`rgit agent skill` prints the skill (`--reference` for the command
reference). You can also install it with
`npx skills add rslib/rgit --skill rgit`.

## Session history

rgit records every operation in its op-log (`rgit oplog`), so there is no
session-end hook to install.
