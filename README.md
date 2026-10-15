# rgit

A git client for people and coding agents: a magit-style TUI, a git-compatible CLI and an MCP server.

Status: early (0.0.x).
Commands and output can change between releases.

## Install

```sh
cargo install --locked --git https://github.com/rslib/rgit rgit-cli
```

This needs Rust 1.96 or later.
rgit runs on Linux and macOS.
Prebuilt binaries for Linux (x86_64 and aarch64) and macOS (aarch64) are on the [releases page](https://github.com/rslib/rgit/releases).

rgit completes commands, flags, branches, remotes, changed files and stash entries in bash, zsh and fish.
Add one line to your shell startup file.
The script asks the installed rgit on each Tab, so it always matches your version.

```sh
eval "$(rgit completion bash)"       # ~/.bashrc
source <(rgit completion zsh)        # ~/.zshrc, after compinit
rgit completion fish | source        # ~/.config/fish/config.fish
```

## Quickstart

Run `rgit` with no arguments in a git repository to open the TUI.

Type `rgit` where you would type `git`.
In your shell, rgit prints the same text as git:

```console
$ rgit status
On branch main
Changes not staged for commit:
  (use "git add <file>..." to update what will be committed)
  (use "git restore <file>..." to discard changes in working directory)
	modified:   README.md

no changes added to commit (use "git add" and/or "git commit -a")
```

When a coding agent runs rgit, it prints TOON instead: short structured lines with the next useful commands.

```console
$ rgit --toon status
branch: main
changes: "0 staged, 1 unstaged, 0 untracked"
files[1]{path,staged,unstaged}:
  README.md,"-",M
help[2]:
  Run `rgit diff` to see unstaged changes
  Run `rgit stage <path>` to stage a file
```

Run `rgit --help` for every command and `rgit <command> --help` for its flags.

## Learn more

- [Read the documentation site](https://rslib.github.io/rgit): every guide, the command reference and the TUI settings.
- [Get started](docs/tutorial.md): make your first commit in the TUI, then undo it.
- [Use the TUI](docs/tui.md): stage hunks, commit, rebase, sync and undo from the TUI.
- [Commands](docs/reference/commands/index.md): one page for each rgit command.
- [Use rgit from a coding agent](docs/agent-integration.md): output modes, `rgit agent install` and the MCP tools.
- [Use rgit with Forgejo](docs/forgejo.md): forge profiles for GitHub, GitLab and Forgejo.
- [Add Change-Id trailers](docs/change-id.md): stable change ids for Gerrit-style review.
- [Draft commit messages with an AI tool](docs/ai.md): run a tool you choose to draft or review a commit, with your own rules and prompts.
- [TUI configuration](docs/config.example.toml): every TUI setting with its default.

## Develop

```sh
cargo fmt --all --check
cargo clippy --all-targets -- -D warnings
cargo test --all-targets
```

## Help

Report a bug or ask a question in the [issue tracker](https://github.com/rslib/rgit/issues).

## License

MIT. See [LICENSE](LICENSE).
