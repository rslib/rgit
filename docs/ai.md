# How to draft commit messages with an AI tool

rgit has no built-in AI client and needs no API key.
It runs a command that you choose and reads the message from its output.
Any tool that reads a prompt and prints text works.

## Define a tool

Put a tool in your global file, `~/.config/rgit/config.toml`.
Use the private file, `.git/rgit/config.toml`, to set it for one repository.
[config.example.toml](config.example.toml) lists all the files rgit reads.

```toml
[ai.claude]
command = "claude -p --model haiku --strict-mcp-config --tools \"\" --setting-sources \"\" --settings '{\"disableAllHooks\":true}' --disable-slash-commands"

[ai.pi]
command = "pi -p --no-session --no-tools -ne -ns --model <provider/model> @\"$RGIT_PROMPT_FILE\""

[ai.ollama]
command = "ollama run <model>"
```

The name after `ai.` is the name of the tool.
`command` runs through `sh -c` in the root of the repository.
The tool gets the prompt on standard input.
It also gets the same prompt in the file `$RGIT_PROMPT_FILE`.
rgit takes the standard output of the tool as the result.
A script of your own works the same way.

The `claude` flags keep your own Claude Code setup out of the run.
Without them, your hooks, skills and `CLAUDE.md` can change the reply, so the draft can come back empty or off topic.

## Pick the default tools

```toml
[commit.ai]
draft = "claude"
review = "claude"
```

The commands below use these tools when you do not name one.
If a key is not set, rgit stops and names the key.

## Draft a message

Stage your changes, then run:

```sh
rgit ai draft
rgit ai draft --tool ollama
```

This prints the message.
`--toon` and `--json` add the fields `message` and `tool`.

To commit with the draft, run:

```sh
rgit commit --draft
rgit commit --draft=ollama
rgit commit -a --draft --no-edit
```

On a terminal, `rgit commit --draft` opens your editor with the draft in it.
`--no-edit` commits the draft as it is.
The draft covers what the commit will hold, so `-a`, `-i` and paths count.
With `--amend`, the draft covers the whole amended commit, not only the new changes.
If the tool fails, rgit stops before it makes a commit.

In the TUI, press `Ctrl-g` in the commit editor to draft with the default tool.
Press `Ctrl-t` to pick the tool from a list.
See [Use the TUI](tui.md).

## Review the staged changes

```sh
rgit ai review
rgit ai review --tool ollama
```

In the TUI, open the palette and run "AI: review staged".
"AI: review staged with a chosen tool" lets you pick the tool first.

## Use your own commit rules

The default draft prompt asks for Conventional Commits.
Point `rules_file` at your own rules to replace only that part.

```toml
[commit.ai]
rules_file = "~/.claude/skills/commits/SKILL.md"
```

`rules` holds the same text inline.
The default prompt still adds the recent subjects, the branch, the stat and the diff.
So rules are the smaller change, and you do not rewrite the whole prompt.
rgit inserts the rules as they are, so braces in the file stay as they are.

## Change the whole prompt

Set a prompt for a task under `[commit.ai]`, or for one tool under `[ai.<name>]`.

```toml
[commit.ai]
draft_prompt_file = "prompts/draft.md"
review_prompt = "Review this diff. Be brief.\n\n{{diff}}"

[ai.ollama]
draft_prompt_file = "prompts/ollama-draft.md"
```

The most specific setting wins.
A prompt of the tool beats a prompt of the task, and a prompt of the task beats the built-in one.
A relative file path is read from the folder of the config file that sets it.
`~/` stands for your home folder.

`draft_prompt` and `draft_prompt_file` are one setting.
The later layer wins, whichever form it uses.
If one file sets both, rgit warns and uses the inline text.
The same holds for `review_prompt`, `review_prompt_file`, `rules` and `rules_file`.

Run `rgit ai prompt draft` to print the template in use, with the rules filled in.
Copy it to a file and start from there.
`rgit ai prompt review --tool <name>` does the same for a review.

## Reference

Placeholders in a prompt:

| Placeholder | Value |
| --- | --- |
| `{{diff}}` | The patch that the commit holds |
| `{{stat}}` | The diff stat |
| `{{files}}` | The file names, one per line |
| `{{log}}` | The last 10 commit subjects, one per line |
| `{{branch}}` | The current branch, or `HEAD` when detached |
| `{{rules}}` | The commit rules |

Any other `{{name}}` is an error.

Environment variables of the tool:

| Variable | Value |
| --- | --- |
| `RGIT_PROMPT_FILE` | A temporary file with the prompt |
| `RGIT_DIFF_FILE` | A temporary file with the whole diff, never cut |
| `RGIT_TASK` | `draft` or `review` |
| `RGIT_REPO` | The root of the repository |
| `RGIT_BRANCH` | The current branch |

Other settings:

| Key | Default | Meaning |
| --- | --- | --- |
| `ai.<name>.timeout_secs` | `120` | Seconds before rgit stops the tool |
| `commit.ai.max_diff_bytes` | `100000` | Size of `{{diff}}` before rgit cuts it |

When the diff is longer, `{{diff}}` holds the first bytes and a note that the stat shows the full scope.
For a draft, rgit removes blank lines at both ends of the output.
For a draft, rgit keeps code blocks inside the message and removes only a fence that wraps all of it.
When a tool fails, rgit shows the name of the tool and the end of its error output.
Ctrl-c stops the tool and rgit exits with 130.
When the tool ends, rgit also stops any process it left running in the background.

## Keep a cloned repository safe

The committed file `.rgit/config.toml` cannot set `ai.<name>.command`.
A cloned repository must not choose what runs on your machine.
The same holds for the `forge` keys.
rgit ignores such a key and prints one warning with the file and the key.
That file may set prompts, rules and the names of the tools.
It may also point a prompt or rules file at a path inside the repository, such as `prompts/rules.md` in the folder `.rgit` or `../docs/commit-rules.md`.
rgit ignores a file path in that file that is absolute, starts with `~`, or leaves the repository through `..` or a symlink.
It prints one warning with the file and the key.

## Check the setup

```sh
rgit settings commit.ai.draft
```

This shows the value and the file that set it.
