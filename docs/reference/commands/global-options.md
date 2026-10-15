# Global options

These options go before or after any command.

| Option | Value | Description |
| --- | --- | --- |
| `--no-input` |  | Never prompt; error if a required argument is missing (for scripts/agents) |
| `--json` |  | Print a JSON result envelope |
| `--toon` |  | Print TOON for agents: no color, spinners, or prompts. The default when an agent harness runs rgit |
| `--human` |  | Print human text, git's own output. The default outside an agent harness |
| `--compact` |  | Print rgit's compact human forms of status, diff, log, show and blame instead of git's (`output.compact` in the rgit config file) |
| `--no-color` |  | Disable ANSI color even on a terminal |
| `--color` | `WHEN` | Color output: always, never or auto (a terminal); overrides the color.* config. Bare `--color` means always |
| `--fields` | `FIELD,...` | Extra table columns to print in agent output (comma-separated) |
| `--full` |  | Print long text (patches, commit bodies) without truncation |
