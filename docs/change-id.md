# How to add Change-Id trailers

rgit adds no trailers to commit messages by default.
Turn on Change-Id trailers when your review system, such as Gerrit, follows a change by a stable id.

Turn them on for one repository.
Put this in `.git/rgit/config.toml`:

```toml
[commit]
change_id = true
```

Turn them on for every repository.
Put the same lines in your global file, `~/.config/rgit/config.toml`.
[config.example.toml](config.example.toml) lists all the files rgit reads.

After that, each commit that rgit makes carries a `Change-Id:` trailer.
rgit keeps the same id when you amend, reword, squash or rebase the commit.
