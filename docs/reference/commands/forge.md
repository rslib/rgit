# rgit forge

Manage GitHub repositories, branches, and pull requests with the native API

## Usage

```sh
rgit forge [OPTIONS] <COMMAND>
```

## Examples

```sh
rgit forge --profile work whoami
rgit forge pr list
```

## Options

| Option | Value | Description |
| --- | --- | --- |
| `--profile` | `PROFILE` | Named forge profile from the rgit config file |
| `--account` | `ACCOUNT` | Credential account label for forge API operations |
| `--host` | `HOST` | Forge host override for GitLab API operations |

## Subcommands

### rgit forge login

Store a forge credential in the OS credential store

#### Usage

```sh
rgit forge login [OPTIONS] <PROVIDER>
```

#### Examples

```sh
rgit forge login github --token-stdin
rgit forge login gitlab --host https://gitlab.example.com --token-stdin
rgit forge login forgejo --host https://git.rs.ht --token-stdin
```

#### Arguments

| Argument | Description |
| --- | --- |
| `<PROVIDER>` |  |

#### Options

| Option | Value | Description |
| --- | --- | --- |
| `--host` | `HOST` | Forge host, such as `https://gitlab.example.com` |
| `--account` | `ACCOUNT` | Credential account label (default: `default`) |
| `--token-stdin` |  | Read the token from stdin; it is never accepted as a command argument |

### rgit forge auth

Show authentication status for configured forge providers

#### Usage

```sh
rgit forge auth [OPTIONS] <COMMAND>
```

#### Examples

```sh
rgit forge auth list
```

#### rgit forge auth list

List known forge providers and whether credentials are stored

##### Usage

```sh
rgit forge auth list [OPTIONS]
```

##### Examples

```sh
rgit forge auth list
```

#### rgit forge auth status

Show detailed authentication status

##### Usage

```sh
rgit forge auth status [OPTIONS]
```

##### Examples

```sh
rgit forge auth status
```

### rgit forge whoami

Show the authenticated account for one forge

#### Usage

```sh
rgit forge whoami [OPTIONS] [PROVIDER]
```

#### Examples

```sh
rgit forge whoami github
```

#### Arguments

| Argument | Description |
| --- | --- |
| `[PROVIDER]` | Provider name, such as `github`; defaults when unambiguous |

#### Options

| Option | Value | Description |
| --- | --- | --- |
| `--host` | `HOST` | Forge host override |
| `--account` | `ACCOUNT` | Credential account label (default: `default`) |

### rgit forge logout

Remove the stored forge credential

#### Usage

```sh
rgit forge logout [OPTIONS] <PROVIDER>
```

#### Examples

```sh
rgit forge logout github
```

#### Arguments

| Argument | Description |
| --- | --- |
| `<PROVIDER>` |  |

#### Options

| Option | Value | Description |
| --- | --- | --- |
| `--host` | `HOST` | Forge host override |
| `--account` | `ACCOUNT` | Credential account label (default: `default`) |

### rgit forge repo

View, create, or delete a hosted repository

#### Usage

```sh
rgit forge repo [OPTIONS] <COMMAND>
```

#### Examples

```sh
rgit forge repo view
```

#### rgit forge repo view

Show repository metadata

##### Usage

```sh
rgit forge repo view [OPTIONS] [TARGET] [REPO]
```

##### Examples

```sh
rgit forge repo view
rgit forge repo view owner/repo
```

##### Arguments

| Argument | Description |
| --- | --- |
| `[TARGET]` | Optional `OWNER/REPO`, `github:OWNER/REPO`, or `gitlab:GROUP/PROJECT`; defaults to the Git remote |
| `[REPO]` |  |

#### rgit forge repo create

Create a repository

##### Usage

```sh
rgit forge repo create [OPTIONS] <NAME>
```

##### Examples

```sh
rgit forge repo create <name>
rgit forge repo create <name> --organization <org> --private
```

##### Arguments

| Argument | Description |
| --- | --- |
| `<NAME>` |  |

##### Options

| Option | Value | Description |
| --- | --- | --- |
| `--provider` | `PROVIDER` |  (default: `github`) |
| `--organization` | `ORGANIZATION` |  |
| `--description` | `DESCRIPTION` |  |
| `--private` |  |  |
| `--auto-init` |  |  |

#### rgit forge repo delete

Delete a repository after explicit confirmation

##### Usage

```sh
rgit forge repo delete [OPTIONS] [TARGET] [REPO]
```

##### Examples

```sh
rgit forge repo delete owner/repo --yes
```

##### Arguments

| Argument | Description |
| --- | --- |
| `[TARGET]` | Optional `OWNER/REPO`, `github:OWNER/REPO`, or `gitlab:GROUP/PROJECT`; defaults to the Git remote |
| `[REPO]` |  |

##### Options

| Option | Value | Description |
| --- | --- | --- |
| `--yes` |  |  |

### rgit forge branch

List or delete branches on the forge

#### Usage

```sh
rgit forge branch [OPTIONS] <COMMAND>
```

#### Examples

```sh
rgit forge branch list
```

#### rgit forge branch list

List remote branches, 100 per page

##### Usage

```sh
rgit forge branch list [OPTIONS] [TARGET] [REPO]
```

##### Examples

```sh
rgit forge branch list
rgit forge branch list owner/repo
```

##### Arguments

| Argument | Description |
| --- | --- |
| `[TARGET]` | Optional `OWNER/REPO`, `github:OWNER/REPO`, or `gitlab:GROUP/PROJECT`; defaults to the Git remote |
| `[REPO]` |  |

##### Options

| Option | Value | Description |
| --- | --- | --- |
| `--page` | `PAGE` | Which page of 100 to show (default: `1`) |

#### rgit forge branch delete

Delete a remote branch after explicit confirmation

##### Usage

```sh
rgit forge branch delete [OPTIONS] <BRANCH> [TARGET] [REPO]
```

##### Examples

```sh
rgit forge branch delete <branch> --yes
rgit forge branch delete <branch> owner/repo --yes
```

##### Arguments

| Argument | Description |
| --- | --- |
| `<BRANCH>` |  |
| `[TARGET]` | Optional `OWNER/REPO`, `github:OWNER/REPO`, or `gitlab:GROUP/PROJECT`; defaults to the Git remote |
| `[REPO]` |  |

##### Options

| Option | Value | Description |
| --- | --- | --- |
| `--yes` |  |  |

### rgit forge pr

Work with pull requests: view, create, edit, merge, review and comment (merge requests on GitLab)

#### Usage

```sh
rgit forge pr [OPTIONS] <COMMAND>
```

#### Examples

```sh
rgit forge pr list
```

#### rgit forge pr view

Show one pull request or merge request by NUMBER or head BRANCH

##### Usage

```sh
rgit forge pr view [OPTIONS] [WHAT] [TARGET] [REPO]
```

##### Examples

```sh
rgit forge pr view
rgit forge pr view 42
rgit forge pr view feature owner/repo
rgit forge pr view 42 --comments
```

##### Arguments

| Argument | Description |
| --- | --- |
| `[WHAT]` | A PR number or a head branch; defaults to the current branch |
| `[TARGET]` | Optional `OWNER/REPO`, `github:OWNER/REPO`, or `gitlab:GROUP/PROJECT`; defaults to the Git remote |
| `[REPO]` |  |

##### Options

| Option | Value | Description |
| --- | --- | --- |
| `--comments` |  | Also show the thread: plain comments, inline code comments and reviews, oldest first |

#### rgit forge pr checks

Show the CI checks on a pull request's head commit (its jobs for GitLab)

##### Usage

```sh
rgit forge pr checks [OPTIONS] [NUMBER] [TARGET] [REPO]
```

##### Examples

```sh
rgit forge pr checks
rgit forge pr checks 42
rgit forge pr checks 42 owner/repo
```

##### Arguments

| Argument | Description |
| --- | --- |
| `[NUMBER]` | A PR number; defaults to the pull request for the current branch |
| `[TARGET]` | Optional `OWNER/REPO`, `github:OWNER/REPO`, or `gitlab:GROUP/PROJECT`; defaults to the Git remote |
| `[REPO]` |  |

#### rgit forge pr list

List pull requests or merge requests, 100 per page

##### Usage

```sh
rgit forge pr list [OPTIONS] [TARGET] [REPO]
```

##### Examples

```sh
rgit forge pr list
rgit forge pr list owner/repo
rgit forge pr list --page 2
rgit forge pr list --state merged --author <login> --head <branch>
```

##### Arguments

| Argument | Description |
| --- | --- |
| `[TARGET]` | Optional `OWNER/REPO`, `github:OWNER/REPO`, or `gitlab:GROUP/PROJECT`; defaults to the Git remote |
| `[REPO]` |  |

##### Options

| Option | Value | Description |
| --- | --- | --- |
| `--state` | `STATE` | Which state to list; open by default (default: `open`) |
| `--author` | `AUTHOR` | Only pull requests whose head author is this login |
| `--head` | `HEAD` | Only pull requests whose head branch is this |
| `--page` | `PAGE` | Which page of 100 to show (default: `1`) |

#### rgit forge pr create

Create a pull request or merge request

##### Usage

```sh
rgit forge pr create [OPTIONS] [TARGET] [REPO]
```

##### Examples

```sh
rgit forge pr create
rgit forge pr create --title "<title>" --head <branch> --base main
rgit forge pr create --fixes 7
```

##### Arguments

| Argument | Description |
| --- | --- |
| `[TARGET]` | Optional `OWNER/REPO`, `github:OWNER/REPO`, or `gitlab:GROUP/PROJECT`; defaults to the Git remote |
| `[REPO]` |  |

##### Options

| Option | Value | Description |
| --- | --- | --- |
| `--title` | `TITLE` | The pull request title; defaults to the head branch's tip commit subject |
| `--head` | `HEAD` | The head branch; defaults to the current branch |
| `--base` | `BASE` | The base branch; defaults to the stack parent, else the default branch |
| `--body` | `BODY` | The pull request body; defaults to the rest of the tip commit message |
| `--body-file` | `BODY_FILE` | Read the body from this file (`-` for stdin) |
| `--draft` |  |  |
| `--fixes` | `FIXES` | An issue number this pull request fixes (`Fixes #N`, `Closes #N` on GitLab) |

#### rgit forge pr checkout

Check out a pull request's head branch, fetching its head commit

##### Usage

```sh
rgit forge pr checkout [OPTIONS] <NUMBER> [TARGET] [REPO]
```

##### Examples

```sh
rgit forge pr checkout 42
rgit forge pr checkout 42 owner/repo
```

##### Arguments

| Argument | Description |
| --- | --- |
| `<NUMBER>` |  |
| `[TARGET]` | Optional `OWNER/REPO`, `github:OWNER/REPO`, or `gitlab:GROUP/PROJECT`; defaults to the Git remote |
| `[REPO]` |  |

#### rgit forge pr close

Close a pull request or merge request

##### Usage

```sh
rgit forge pr close [OPTIONS] <NUMBER> [TARGET] [REPO]
```

##### Examples

```sh
rgit forge pr close 42 --yes
rgit forge pr close 42 owner/repo --yes
```

##### Arguments

| Argument | Description |
| --- | --- |
| `<NUMBER>` |  |
| `[TARGET]` | Optional `OWNER/REPO`, `github:OWNER/REPO`, or `gitlab:GROUP/PROJECT`; defaults to the Git remote |
| `[REPO]` |  |

##### Options

| Option | Value | Description |
| --- | --- | --- |
| `--yes` |  |  |

#### rgit forge pr edit

Change a pull request's title, body or base branch

##### Usage

```sh
rgit forge pr edit [OPTIONS] <NUMBER> [TARGET] [REPO]
```

##### Examples

```sh
rgit forge pr edit 42 --title "<title>"
rgit forge pr edit 42 --base main
```

##### Arguments

| Argument | Description |
| --- | --- |
| `<NUMBER>` |  |
| `[TARGET]` | Optional `OWNER/REPO`, `github:OWNER/REPO`, or `gitlab:GROUP/PROJECT`; defaults to the Git remote |
| `[REPO]` |  |

##### Options

| Option | Value | Description |
| --- | --- | --- |
| `--title` | `TITLE` | A new title |
| `--body` | `BODY` | A new body |
| `--body-file` | `BODY_FILE` | Read the new body from this file (`-` for stdin) |
| `--base` | `BASE` | Retarget the pull request onto this branch |

#### rgit forge pr ready

Mark a pull request ready for review

##### Usage

```sh
rgit forge pr ready [OPTIONS] <NUMBER> [TARGET] [REPO]
```

##### Examples

```sh
rgit forge pr ready 42
```

##### Arguments

| Argument | Description |
| --- | --- |
| `<NUMBER>` |  |
| `[TARGET]` | Optional `OWNER/REPO`, `github:OWNER/REPO`, or `gitlab:GROUP/PROJECT`; defaults to the Git remote |
| `[REPO]` |  |

#### rgit forge pr draft

Mark a pull request a draft

##### Usage

```sh
rgit forge pr draft [OPTIONS] <NUMBER> [TARGET] [REPO]
```

##### Examples

```sh
rgit forge pr draft 42
```

##### Arguments

| Argument | Description |
| --- | --- |
| `<NUMBER>` |  |
| `[TARGET]` | Optional `OWNER/REPO`, `github:OWNER/REPO`, or `gitlab:GROUP/PROJECT`; defaults to the Git remote |
| `[REPO]` |  |

#### rgit forge pr reopen

Reopen a pull request or merge request

##### Usage

```sh
rgit forge pr reopen [OPTIONS] <NUMBER> [TARGET] [REPO]
```

##### Examples

```sh
rgit forge pr reopen 42
```

##### Arguments

| Argument | Description |
| --- | --- |
| `<NUMBER>` |  |
| `[TARGET]` | Optional `OWNER/REPO`, `github:OWNER/REPO`, or `gitlab:GROUP/PROJECT`; defaults to the Git remote |
| `[REPO]` |  |

#### rgit forge pr comment

Comment on a pull request or merge request

##### Usage

```sh
rgit forge pr comment [OPTIONS] <NUMBER> [TARGET] [REPO]
```

##### Examples

```sh
rgit forge pr comment 42 --body "<text>"
```

##### Arguments

| Argument | Description |
| --- | --- |
| `<NUMBER>` |  |
| `[TARGET]` | Optional `OWNER/REPO`, `github:OWNER/REPO`, or `gitlab:GROUP/PROJECT`; defaults to the Git remote |
| `[REPO]` |  |

##### Options

| Option | Value | Description |
| --- | --- | --- |
| `--body` | `BODY` | The comment text |
| `--body-file` | `BODY_FILE` | Read the comment text from this file (`-` for stdin) |

#### rgit forge pr review

Review a pull request or merge request

##### Usage

```sh
rgit forge pr review [OPTIONS] <NUMBER> [TARGET] [REPO]
```

##### Examples

```sh
rgit forge pr review 42 --approve
rgit forge pr review 42 --request-changes --body "<text>"
```

##### Arguments

| Argument | Description |
| --- | --- |
| `<NUMBER>` |  |
| `[TARGET]` | Optional `OWNER/REPO`, `github:OWNER/REPO`, or `gitlab:GROUP/PROJECT`; defaults to the Git remote |
| `[REPO]` |  |

##### Options

| Option | Value | Description |
| --- | --- | --- |
| `--approve` |  | Approve the pull request |
| `--request-changes` |  | Ask for changes on the pull request |
| `--comment` |  | Leave a comment review on the pull request |
| `--body` | `BODY` | The review text |

#### rgit forge pr merge

Merge a pull request or merge request

##### Usage

```sh
rgit forge pr merge [OPTIONS] <NUMBER> [TARGET] [REPO]
```

##### Examples

```sh
rgit forge pr merge 42 --yes
rgit forge pr merge 42 --method squash --delete-branch --yes
```

##### Arguments

| Argument | Description |
| --- | --- |
| `<NUMBER>` |  |
| `[TARGET]` | Optional `OWNER/REPO`, `github:OWNER/REPO`, or `gitlab:GROUP/PROJECT`; defaults to the Git remote |
| `[REPO]` |  |

##### Options

| Option | Value | Description |
| --- | --- | --- |
| `-m, --method` | `METHOD` | How to combine the head branch: merge, squash or rebase (default: `merge`) |
| `--delete-branch` |  | Delete the head branch after merging |
| `--yes` |  |  |

### rgit forge issue

Work with issues: list, view, create, edit, close, reopen and comment

#### Usage

```sh
rgit forge issue [OPTIONS] <COMMAND>
```

#### Examples

```sh
rgit forge issue list
```

#### rgit forge issue list

List issues, 100 per page

##### Usage

```sh
rgit forge issue list [OPTIONS] [TARGET] [REPO]
```

##### Examples

```sh
rgit forge issue list
rgit forge issue list owner/repo
rgit forge issue list --page 2
rgit forge issue list --state closed --author <login> --label <label> --assignee <login>
```

##### Arguments

| Argument | Description |
| --- | --- |
| `[TARGET]` | Optional `OWNER/REPO`, `github:OWNER/REPO`, or `gitlab:GROUP/PROJECT`; defaults to the Git remote |
| `[REPO]` |  |

##### Options

| Option | Value | Description |
| --- | --- | --- |
| `--state` | `STATE` | Which state to list; open by default (default: `open`) |
| `--author` | `AUTHOR` | Only issues opened by this login |
| `--label` | `LABEL` | Only issues carrying this label |
| `--assignee` | `ASSIGNEE` | Only issues assigned to this login |
| `--page` | `PAGE` | Which page of 100 to show (default: `1`) |

#### rgit forge issue view

Show one issue by NUMBER

##### Usage

```sh
rgit forge issue view [OPTIONS] <NUMBER> [TARGET] [REPO]
```

##### Examples

```sh
rgit forge issue view 7
rgit forge issue view 7 owner/repo
rgit forge issue view 7 --comments
```

##### Arguments

| Argument | Description |
| --- | --- |
| `<NUMBER>` | An issue number |
| `[TARGET]` | Optional `OWNER/REPO`, `github:OWNER/REPO`, or `gitlab:GROUP/PROJECT`; defaults to the Git remote |
| `[REPO]` |  |

##### Options

| Option | Value | Description |
| --- | --- | --- |
| `--comments` |  | Also show the comment thread, oldest first |

#### rgit forge issue create

Open an issue

##### Usage

```sh
rgit forge issue create [OPTIONS] --title <TITLE> [TARGET] [REPO]
```

##### Examples

```sh
rgit forge issue create --title "<title>"
rgit forge issue create --title "<title>" --body "<text>" --label <label> --assignee <login>
```

##### Arguments

| Argument | Description |
| --- | --- |
| `[TARGET]` | Optional `OWNER/REPO`, `github:OWNER/REPO`, or `gitlab:GROUP/PROJECT`; defaults to the Git remote |
| `[REPO]` |  |

##### Options

| Option | Value | Description |
| --- | --- | --- |
| `--title` | `TITLE` | The issue title |
| `--body` | `BODY` | The issue body |
| `--body-file` | `BODY_FILE` | Read the body from this file (`-` for stdin) |
| `--label` | `LABEL` | A label per flag |
| `--assignee` | `ASSIGNEE` | An assignee login per flag |

#### rgit forge issue edit

Change an issue's title, body, labels or assignees

##### Usage

```sh
rgit forge issue edit [OPTIONS] <NUMBER> [TARGET] [REPO]
```

##### Examples

```sh
rgit forge issue edit 7 --title "<title>"
rgit forge issue edit 7 --label <label> --assignee <login>
```

##### Arguments

| Argument | Description |
| --- | --- |
| `<NUMBER>` |  |
| `[TARGET]` | Optional `OWNER/REPO`, `github:OWNER/REPO`, or `gitlab:GROUP/PROJECT`; defaults to the Git remote |
| `[REPO]` |  |

##### Options

| Option | Value | Description |
| --- | --- | --- |
| `--title` | `TITLE` | A new title |
| `--body` | `BODY` | A new body |
| `--body-file` | `BODY_FILE` | Read the new body from this file (`-` for stdin) |
| `--label` | `LABEL` | Replace the labels (one per flag) |
| `--assignee` | `ASSIGNEE` | Replace the assignees (one login per flag) |

#### rgit forge issue close

Close an issue after explicit confirmation

##### Usage

```sh
rgit forge issue close [OPTIONS] <NUMBER> [TARGET] [REPO]
```

##### Examples

```sh
rgit forge issue close 7 --yes
rgit forge issue close 7 owner/repo --yes
```

##### Arguments

| Argument | Description |
| --- | --- |
| `<NUMBER>` |  |
| `[TARGET]` | Optional `OWNER/REPO`, `github:OWNER/REPO`, or `gitlab:GROUP/PROJECT`; defaults to the Git remote |
| `[REPO]` |  |

##### Options

| Option | Value | Description |
| --- | --- | --- |
| `--yes` |  |  |

#### rgit forge issue reopen

Reopen an issue

##### Usage

```sh
rgit forge issue reopen [OPTIONS] <NUMBER> [TARGET] [REPO]
```

##### Examples

```sh
rgit forge issue reopen 7
```

##### Arguments

| Argument | Description |
| --- | --- |
| `<NUMBER>` |  |
| `[TARGET]` | Optional `OWNER/REPO`, `github:OWNER/REPO`, or `gitlab:GROUP/PROJECT`; defaults to the Git remote |
| `[REPO]` |  |

#### rgit forge issue comment

Comment on an issue

##### Usage

```sh
rgit forge issue comment [OPTIONS] <NUMBER> [TARGET] [REPO]
```

##### Examples

```sh
rgit forge issue comment 7 --body "<text>"
```

##### Arguments

| Argument | Description |
| --- | --- |
| `<NUMBER>` |  |
| `[TARGET]` | Optional `OWNER/REPO`, `github:OWNER/REPO`, or `gitlab:GROUP/PROJECT`; defaults to the Git remote |
| `[REPO]` |  |

##### Options

| Option | Value | Description |
| --- | --- | --- |
| `--body` | `BODY` | The comment text |
| `--body-file` | `BODY_FILE` | Read the comment text from this file (`-` for stdin) |

#### rgit forge issue browse

Open an issue in the browser

##### Usage

```sh
rgit forge issue browse [OPTIONS] [NUMBER] [TARGET] [REPO]
```

##### Examples

```sh
rgit forge issue browse 7
rgit forge issue browse 7 owner/repo --print
```

##### Arguments

| Argument | Description |
| --- | --- |
| `[NUMBER]` | An issue number; opens the repository page when omitted |
| `[TARGET]` | Optional `OWNER/REPO`, `github:OWNER/REPO`, or `gitlab:GROUP/PROJECT`; defaults to the Git remote |
| `[REPO]` |  |

##### Options

| Option | Value | Description |
| --- | --- | --- |
| `--print` |  | Print the URL instead of opening it |

### rgit forge browse

Open the forge repository or one pull request in the browser

#### Usage

```sh
rgit forge browse [OPTIONS] [NUMBER] [TARGET] [REPO]
```

#### Examples

```sh
rgit forge browse
rgit forge browse 42
rgit forge browse 42 owner/repo --print
```

#### Arguments

| Argument | Description |
| --- | --- |
| `[NUMBER]` | A PR number; opens the repository page when omitted |
| `[TARGET]` | Optional `OWNER/REPO`, `github:OWNER/REPO`, or `gitlab:GROUP/PROJECT`; defaults to the Git remote |
| `[REPO]` |  |

#### Options

| Option | Value | Description |
| --- | --- | --- |
| `--print` |  | Print the URL instead of opening it |

See the [global options](global-options.md) that every command accepts.
