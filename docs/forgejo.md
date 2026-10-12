# Forgejo profile setup

rgit treats a Forgejo host the same as GitHub or GitLab once a profile names it.
All `forge` commands then work with `forgejo:OWNER/REPO` targets or with the Git remote.

## Point a profile at your host

Add a profile to `~/.config/rgit/config.toml` (`RGIT_CONFIG` overrides the path).
Name `forgejo` as the provider and your host as the host.

```toml
[forge.profiles.lab]
provider = "forgejo"
host = "https://git.rs.ht"
```

A remote whose host matches the profile host resolves to Forgejo with no flags.
This matches `forgejo@git.rs.ht:rayandrew/lab.git` and `https://git.rs.ht/rayandrew/lab.git`.

Without a profile, set `FORGEJO_HOST` to your host.
The default host is `https://codeberg.org`.

## Log in once

A Forgejo login reads the token from stdin and stores it in the OS credential store.
The token never appears as a command argument.

```sh
rgit forge login forgejo --host https://git.rs.ht --token-stdin < ~/.token
```

`rgit forge auth list` shows the stored account.
A `FORGEJO_TOKEN` environment variable works without a login and wins over the stored token.

## Use it

```sh
rgit forge pr list forgejo:rayandrew/lab
```

Pull requests close issues with `Fixes #N` trailers, as on GitHub.
