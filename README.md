# rgit

A magit-style git TUI that also works as a CLI and an MCP server.

```sh
cargo install --locked --git https://github.com/rslib/rgit rgit-cli
rgit            # open the TUI (without a terminal, print git's usage)
rgit status     # subcommands print git's human text; an agent harness gets TOON
```

## Forge support

rgit works with GitHub, GitLab and Forgejo.
Set up a profile once, then use `OWNER/REPO` or `forgejo:OWNER/REPO` targets with any `forge` command.
See [docs/forgejo.md](docs/forgejo.md) for Forgejo setup.

## Change-Id trailers

rgit does not add trailers to commit messages by default. For Gerrit-style
stable change ids, run `git config rgit.changeId true` (add `--global` for
every repo): commits rgit makes then carry a `Change-Id:` trailer that is kept
across amend, reword, squash, and rebase.

## Using rgit from a coding agent

rgit prints human text in a person's shell, as git does, even when piped. When
an agent harness runs it (Claude Code, Codex, Gemini CLI, Cursor, OpenCode,
goose, amp or pi) rgit prints TOON by default: compact, structured lines and
tables with next-step hints. `--json` gives the same data as JSON, and
`--human` (or `RGIT_OUTPUT=human`, or config `rgit.output=human`) keeps git's
text. `--compact` (or `rgit.compact=true`) keeps rgit's short human forms of
`status`, `diff`, `log`, `show` and `blame`. There are two
ways to give an agent rgit context, and both rely on the TOON default.
`rgit agent install` sets up every agent app it finds (Claude Code, Codex,
OpenCode, pi, omp) with both:

1. **Session hook.** Each session starts with the repo's live state.
2. **Agent Skill.** rgit's usage guide, loaded when a task needs it.
3. **Tools.** rgit's operations as the agent's own tools.

`rgit agent status` shows what each app has.

See [docs/agent-integration.md](docs/agent-integration.md) for details.
