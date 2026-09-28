# rgit

A magit-style git TUI that also works as a CLI and an MCP server.

```sh
cargo install --locked --git https://github.com/rslib/rgit rgit-cli
rgit            # open the TUI (or, when piped, print `git status`)
rgit status     # subcommands print git's human text; agents add --toon
```

## Change-Id trailers

rgit does not add trailers to commit messages by default. For Gerrit-style
stable change ids, run `git config rgit.changeId true` (add `--global` for
every repo): commits rgit makes then carry a `Change-Id:` trailer that is kept
across amend, reword, squash, and rebase.

## Using rgit from a coding agent

rgit prints human text by default, as git does, even when piped. Agents pass
`--toon` (alias `--axi`) on every command for compact TOON with next-step
hints, or `--json` for JSON. `--compact` (or `rgit.compact=true`) keeps rgit's
short human forms of `status`, `diff`, `log`, `show` and `blame`. There are two
ways to give an agent rgit context, and both tell it to pass `--toon`. You need
only one:

1. **Session hook (preferred).** `rgit hooks install` makes Claude Code, Codex,
   and OpenCode start every session with the repo's live state.
2. **Agent Skill.** `rgit skills install`, or
   `npx skills add rslib/rgit --skill rgit`, loads rgit's usage guide only when
   a task needs it.

See [docs/agent-integration.md](docs/agent-integration.md) for details.
