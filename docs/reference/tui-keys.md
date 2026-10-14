# TUI keys

Every key the TUI binds, grouped as in its `?` help screen.

## Navigation

| Key | Action |
| --- | --- |
| `j / k · ^n / ^p` | move down / up |
| `^f / ^b` | forward / back a character |
| `^a / ^e` | start / end of line |
| `M-f / M-b` | forward / back a word |
| `^Spc / v` | set mark (char / line selection) |
| `M-w · ^w` | copy the selection (vim: y) |
| `^x o · arrows` | switch nav / preview pane |
| `Tab` | fold / unfold section |
| `RET` | open commit · blame file · checkout ref |
| `v` | select line(s) for staging |
| `q` | back a screen · quit at root |
| `/` | search · n / N next / prev |
| `:` | command palette |
| `g` | refresh |

## Staging

| Key | Action |
| --- | --- |
| `s / u` | stage / unstage at point |
| `S / U` | stage / unstage everything |
| `x` | discard · delete branch/tag at point |

## Commit & history

| Key | Action |
| --- | --- |
| `c` | commit menu (a amend · e extend) |
| `l` | log menu (--all · filter by author) |
| `L` | session log (in-app diagnostics) |
| `d` | diff two revisions |
| `y` | refs (branches, remotes, tags) |
| `A / V` | cherry-pick / revert a commit |

## Branch & stash

| Key | Action |
| --- | --- |
| `b` | branch menu (checkout · create · rename · delete) |
| `z` | stash menu (save · pop · apply · drop) |
| `p` | pop the stash under the cursor |
| `t` | tag menu (create · delete) |

## Sync & power tools

| Key | Action |
| --- | --- |
| `f / F` | fetch / pull |
| `P` | push menu (--force-with-lease, …) |
| `M` | remote menu (list · add · remove) |
| `r` | rebase menu (onto · interactive · continue) |
| `m` | merge a branch |
| `O` | reset (soft · mixed · hard) |

## Operations menu (o · space o)

| Key | Action |
| --- | --- |
| `u / r` | undo / redo the last operation |
| `o` | op-log timeline · RET restores to that point |
| `s` | smartlog (draft commits vs the trunk) |
| `k / n / R` | stack: view · new branch · restack |
| `a` | absorb changes into the owning commits |
| `f` | workflow status (flow init/start/finish: palette) |
| `w` | copy-on-write workspaces (new: palette) |

## Forge (pull requests)

| Key | Action |
| --- | --- |
| `m` | merge (pick a method, press again to confirm) |
| `C` | check out the request |
| `g` | refresh |
| `R` | toggle ready / draft |
| `a` | comment |
| `c` | create a request |
| `d / RET` | details |
| `x` | close (press again to confirm) |
| `o` | open in the browser |
| `]` | next forge profile |
