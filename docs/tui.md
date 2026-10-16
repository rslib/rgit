# Use the TUI

Run `rgit` with no arguments in a repository to open the TUI.
These how-tos use the default key profile.
[TUI keys](reference/tui-keys.md) lists every key, and [Get started](tutorial.md) walks through a first commit.

## Find your way around

The top lines show `Head`, the branch and commit you are on, and `Push`, where that branch goes on the remote.
Below them, the status lists your changes in sections such as `UNTRACKED`, `UNSTAGED` and `STAGED`, then the `RECENT` commits.

The bar at the left edge is the cursor.
Move it with `j` and `k`, or with `Ctrl-n` and `Ctrl-p`.

The bottom line shows the keys for what is under the cursor, so it changes as you move.
On a file it shows `s stage  x discard`, and on a hunk it adds `v lines`.

Press `?` for the full key list, `:` for the command palette and `q` to go back a screen or quit.

## Stage a file, a hunk or a few lines

1. Move the cursor to a file under `UNSTAGED` or `UNTRACKED`.
2. Press `s` to stage the whole file.

To stage part of a file:

1. Press `Tab` on the file to show its hunks.
2. Move to a hunk with `j`.
3. Press `s` to stage only that hunk.

To stage single lines, press `v` on a hunk, move to mark the lines, then press `s`.

`u` unstages what is under the cursor, and `S` and `U` stage or unstage everything.
`x` discards the change under the cursor, and the TUI asks before it deletes anything.
On a section heading, a key acts on every file in that section.

## Commit

1. Stage the changes you want.
2. Press `c` to open the commit menu.
3. Press `c` for a new commit, `a` to amend the last commit, or `e` to add the staged changes to the last commit and keep its message.
4. Type the message.
5. Press `Ctrl-s` to commit.

Press `Esc` in the message box to cancel, or `Ctrl-o` to write the message in `$EDITOR`.
Press `Ctrl-g` to draft the message with your default AI tool, or `Ctrl-t` to pick the tool first.
[Draft commit messages with an AI tool](ai.md) shows how to set the tools up.

A long message stays in view.
The box scrolls to keep the cursor visible, and a long line wraps onto the next row with a `↪` mark at its start.
Press `Alt-z` to turn wrap off.
Without wrap, the box scrolls sideways, and `‹` or `›` shows that text is hidden at that edge.
The top border shows the cursor's line and column and whether wrap is on.
A tinted column marks column 72, the usual width for a message body.

## Resize a box

Drag the border of a box with the mouse to make it bigger or smaller.
Press `Alt-m` in the commit message box, or `m` in the operation output box, to fill the screen, and press it again to go back.
In the operation output box, `w` turns wrap on or off, `j` and `k` scroll and `h` and `l` scroll sideways.
The mouse wheel scrolls the box under the pointer.

## Look at history

- `l` opens the log menu, with options such as `--all` and a filter by author.
- `Enter` on a commit opens it, and `Enter` on a file opens its blame.
- `d` shows the diff between two revisions.
- `y` lists the branches, the remotes and the tags.
- `/` searches the screen, and `n` and `N` jump to the next and the previous match.

## Work with branches, stashes and tags

- `b` opens the branch menu: check out, create, rename or delete a branch.
- `z` opens the stash menu: save, pop, apply or drop a stash.
- `p` pops the stash under the cursor.
- `t` opens the tag menu: create or delete a tag.

## Sync with a remote

- `f` fetches and `F` pulls.
- `P` opens the push menu, with options such as `--force-with-lease`.
- `M` opens the remote menu: list, add or remove a remote.

## Rebase, merge, reset and cherry-pick

- `r` opens the rebase menu: rebase onto a branch, start an interactive rebase, or continue one that stopped.
- `m` merges a branch.
- `O` resets to a commit: soft, mixed or hard.
- `A` cherry-picks the commit under the cursor, and `V` reverts it.

## Undo a change

rgit records each operation, in the TUI and in the shell.
Press `o` to open the operations menu, then:

- `u` undoes the last operation and `r` redoes it.
- `o` shows the operation log, and `Enter` on an entry restores the repository to that point.

The same records back `rgit undo`, `rgit redo` and `rgit oplog` in the shell.

## Review pull requests

Press `:`, type `pull` and choose `Pull requests`.
The view lists the open pull requests of the `origin` remote on GitHub, GitLab or Forgejo.
In it, `d` or `Enter` shows the details, `C` checks the request out, `a` adds a comment and `m` merges.
[Use rgit with Forgejo](forgejo.md) covers the forge setup.

## Use vim keys

The `vim` profile moves with `h`, `j`, `k` and `l` and puts each git action behind a `Space` leader.
Set it in `~/.config/rgit/config.toml`.
The same keys also work in the repo files, `.rgit/config.toml` and `.git/rgit/config.toml`.
[config.example.toml](config.example.toml) lists them all.

```toml
profile = "vim"
```

[Configuration](config.example.toml) shows every setting, including how to rebind keys and build your own profile.
