# rgit

A magit-style git TUI that also works as a CLI and an MCP server.

```sh
cargo install --locked --git https://github.com/rslib/rgit rgit-cli
rgit            # open the TUI (or, when piped, print the repo home view)
rgit status     # any subcommand prints compact output
```

## Change-Id trailers

rgit does not add trailers to commit messages by default. For Gerrit-style
stable change ids, run `git config rgit.changeId true` (add `--global` for
every repo): commits rgit makes then carry a `Change-Id:` trailer that is kept
across amend, reword, squash, and rebase.

## Using rgit from a coding agent

When stdout is not a terminal, rgit prints compact TOON with next-step hints.
`--json` prints JSON and `--human` prints terminal text. There are two ways to
give an agent rgit context. You need only one:

1. **Session hook (preferred).** `rgit hooks install` makes Claude Code, Codex,
   and OpenCode start every session with the repo's live state.
2. **Agent Skill.** `rgit skills install`, or
   `npx skills add rslib/rgit --skill rgit`, loads rgit's usage guide only when
   a task needs it.

See [docs/agent-integration.md](docs/agent-integration.md) for details.
