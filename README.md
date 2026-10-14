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

- [Use rgit from a coding agent](docs/agent-integration.md): output modes, `rgit agent install` and the MCP tools.
- [Use rgit with Forgejo](docs/forgejo.md): forge profiles for GitHub, GitLab and Forgejo.
- [Add Change-Id trailers](docs/change-id.md): stable change ids for Gerrit-style review.
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

MIT.
