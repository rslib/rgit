# rgit scalar

Large-repository setup and upkeep (git's `scalar`): recommended config, background maintenance and the registered repo list

## Usage

```sh
rgit scalar [OPTIONS] <COMMAND>
```

## Examples

```sh
rgit scalar register
rgit scalar list
```

## Subcommands

### rgit scalar register

Set scalar's config, start maintenance and add the repository to the global scalar.repo list

#### Usage

```sh
rgit scalar register [OPTIONS] [ENLISTMENT]
```

#### Examples

```sh
rgit scalar register
rgit scalar register ~/src/big
```

#### Arguments

| Argument | Description |
| --- | --- |
| `[ENLISTMENT]` | The enlistment (default: the current repository) |

#### Options

| Option | Value | Description |
| --- | --- | --- |
| `--no-maintenance` |  | Leave background maintenance off |

### rgit scalar unregister

Remove the repository from scalar.repo and maintenance

#### Usage

```sh
rgit scalar unregister [OPTIONS] [ENLISTMENT]
```

#### Examples

```sh
rgit scalar unregister
```

#### Arguments

| Argument | Description |
| --- | --- |
| `[ENLISTMENT]` | The enlistment (default: the current repository) |

### rgit scalar list

Print the registered repositories

#### Usage

```sh
rgit scalar list [OPTIONS]
```

#### Examples

```sh
rgit scalar list
```

### rgit scalar run

Run one task now: all, config, commit-graph, fetch, loose-objects or pack-files

#### Usage

```sh
rgit scalar run [OPTIONS] <TASK> [ENLISTMENT]
```

#### Examples

```sh
rgit scalar run all
rgit scalar run commit-graph
```

#### Arguments

| Argument | Description |
| --- | --- |
| `<TASK>` | The task |
| `[ENLISTMENT]` | The enlistment (default: the current repository) |

### rgit scalar reconfigure

Set scalar's config again, overwriting the required values

#### Usage

```sh
rgit scalar reconfigure [OPTIONS] [ENLISTMENT]
```

#### Examples

```sh
rgit scalar reconfigure --all
```

#### Arguments

| Argument | Description |
| --- | --- |
| `[ENLISTMENT]` | The enlistment (default: the current repository) |

#### Options

| Option | Value | Description |
| --- | --- | --- |
| `-a, --all` |  | Every registered repository |
| `--maintenance` | `MODE` | enable, disable or keep background maintenance |

### rgit scalar delete

Unregister an enlistment and delete its folder

#### Usage

```sh
rgit scalar delete [OPTIONS] <ENLISTMENT>
```

#### Examples

```sh
rgit scalar delete ~/src/big
```

#### Arguments

| Argument | Description |
| --- | --- |
| `<ENLISTMENT>` | The enlistment |

### rgit scalar clone

Clone into `<enlistment>/src` and register it

#### Usage

```sh
rgit scalar clone [OPTIONS] <URL> [ENLISTMENT]
```

#### Examples

```sh
rgit scalar clone https://github.com/org/big.git
```

#### Arguments

| Argument | Description |
| --- | --- |
| `<URL>` | The repository to clone |
| `[ENLISTMENT]` | The enlistment folder (default: from the URL) |

#### Options

| Option | Value | Description |
| --- | --- | --- |
| `-b, --branch` | `BRANCH` | Check out this branch |
| `--single-branch` |  | Fetch only the branch checked out |
| `--no-src` |  | Clone into the enlistment itself, not `src` |
| `--no-tags` |  | Fetch no tags |
| `--full-clone` |  | Accepted for scalar; the whole tree is checked out either way |

### rgit scalar version

Print the version

#### Usage

```sh
rgit scalar version [OPTIONS]
```

#### Examples

```sh
rgit scalar version
```

See the [global options](global-options.md) that every command accepts.
