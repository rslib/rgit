# Get started with rgit

You'll make a new repository, commit a file from the rgit TUI, then undo the commit and bring it back.
It takes about 10 minutes.

## Before you start

You need rgit and a terminal that is at least 80 columns wide.
The [README](https://github.com/rslib/rgit#install) shows how to install rgit.

Check that your shell finds it:

```bash
rgit --version
```

```text
0.0.4
```

A newer version number is fine.

## 1. Make a practice repository

You'll work in a new folder, so nothing you do here can touch your other projects.

```bash
rgit init -b main hello
```

```text
Initialized empty Git repository in /Users/ada/hello/.git/
```

The path on your screen shows your own folder.

## 2. Go into the repository

```bash
cd hello
```

`cd` prints nothing.
Every command from here on runs inside `hello`.

## 3. Tell git who you are

Each commit records a name and an email address.
Set them for this repository only.

```bash
rgit config user.name "Ada Lovelace"
```

```bash
rgit config user.email ada@example.com
```

Neither command prints anything.
Ask for the name to see that it took:

```bash
rgit config user.name
```

```text
Ada Lovelace
```

## 4. Add a file

```bash
printf 'Hello, rgit.\n' > hello.txt
```

Now ask rgit what it sees.
rgit takes git's commands, so `rgit status` prints what `git status` would:

```bash
rgit status
```

```text
On branch main

No commits yet

Untracked files:
  (use "git add <file>..." to include in what will be committed)
	hello.txt

nothing added to commit but untracked files present (use "git add" to track)
```

`hello.txt` is untracked, so git does not keep it yet.

## 5. Try to commit too early

What happens if you commit now?

```bash
rgit commit -m "Add hello"
```

```text
On branch main

Initial commit

Untracked files:
  (use "git add <file>..." to include in what will be committed)
	hello.txt

nothing added to commit but untracked files present (use "git add" to track)
```

rgit made no commit, because nothing is staged.
A commit holds only the changes you stage first.
You'll stage the file in the TUI next.

## 6. Open the TUI

```bash
rgit
```

```text
╭ ◆ rgit hello ───────────────────────────────────────────────────── main  ● 1 ╮
│ ▎Head:   main                                                                │
│  Push:   not published                                                       │
│                                                                              │
│  ▾ UNTRACKED  1                                                              │
│    ?  hello.txt                                                              │
│                                                                              │
│ s stage  c commit  l log  : palette  ? help                                  │
╰──────────────────────────────────────────────────────────────────────────────╯
```

The bar at the left edge is the cursor, and it starts on `Head`.
The bottom line lists the main keys.
Press `q` to leave the TUI at any time.

## 7. Move to the untracked file

Press `j` three times.

```text
│  Head:   main                                                                │
│  Push:   not published                                                       │
│                                                                              │
│ ▎▾ UNTRACKED  1                                                              │
│    ?  hello.txt                                                              │
```

The bar now sits on the `UNTRACKED` heading.
A key on a heading acts on every file under it.

## 8. Stage it

Press `s`.

```text
│ ▎▾ STAGED  1                                                                 │
│    A  hello.txt  +1  ▰▰▰▰▰                                                   │
```

`hello.txt` moved to `STAGED`.
The `A` means a new file, and `+1` means one added line.
Press `u` to unstage it again if you change your mind.

## 9. Open the commit menu

Press `c`.

```text
│  Commit                                                                      │
│  ACTIONS                                                                     │
│   c   commit                                                                 │
│   a   amend                                                                  │
│   e   extend (keep message)                                                  │
│  esc cancel                                                                  │
```

The menu offers a new commit, an amend of the last one, or an extend.

## 10. Write the message

Press `c` again for a new commit.
A box opens for the message.
Type `Add hello`.

```text
│      ╭ commit ────────────────────────────────────────────────────────╮      │
│      │ Add hello                                                      │      │
│      │                                                                │      │
│      │ 9/50  ^S commit  ^G generate  ^O $EDITOR  esc cancel           │      │
│      ╰────────────────────────────────────────────────────────────────╯      │
```

`9/50` counts the characters in the first line against git's usual limit of 50.
Press `Esc` here to cancel and keep everything staged.

## 11. Make the commit

Press `Ctrl-s`.

```text
│  Head:   main  2181a5a  Add hello  · 0s                                      │
│  Push:   not published                                                       │
│                                                                              │
│ ▎✓ working tree clean                                                        │
│                                                                              │
│    nothing to commit                                                         │
│                                                                              │
│  ▾ RECENT                                                                    │
│    2181a5a  main  Add hello  · 0s                                            │
```

`Head` now names your commit, and `RECENT` lists it.
Your commit id won't be `2181a5a`, since every commit gets its own id.

## 12. Leave the TUI

Press `q`.
You're back at the shell prompt.

## 13. Check the commit from the shell

```bash
rgit log --oneline
```

```text
2181a5a Add hello
```

## 14. Undo the commit

rgit records each change it makes, so you can step back.

```bash
rgit undo
```

```text
undid commit
```

Look at the status now:

```bash
rgit status
```

```text
On branch main

No commits yet

Changes to be committed:
  (use "git rm --cached <file>..." to unstage)
	new file:   hello.txt

```

The commit is gone, but `hello.txt` is still staged, as it was before you pressed `Ctrl-s`.

## 15. Bring the commit back

```bash
rgit redo
```

```text
redid commit
```

```bash
rgit log --oneline
```

```text
2181a5a Add hello
```

The same commit, with the same id, is back.

## Try it yourself

Add a second line to `hello.txt`, then run `rgit status`.
Where will rgit list the file this time?

```bash
printf 'One more line.\n' >> hello.txt
```

```bash
rgit status
```

```text
On branch main
Changes not staged for commit:
  (use "git add <file>..." to update what will be committed)
  (use "git restore <file>..." to discard changes in working directory)
	modified:   hello.txt

no changes added to commit (use "git add" and/or "git commit -a")
```

git tracks the file now, so it shows as modified instead of untracked.
Open `rgit` again to stage and commit it the same way.

## What you built

You made a repository, staged and committed a file in the TUI, and undid and redid the commit from the shell.

- [Use the TUI](tui.md) shows the views and the everyday tasks.
- [TUI keys](reference/tui-keys.md) lists every key.
- [Commands](reference/commands/index.md) has a page for each rgit command.
- [Use rgit from a coding agent](agent-integration.md) sets rgit up for Claude Code, Codex and other agents.
