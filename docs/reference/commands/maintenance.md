# rgit maintenance

Background upkeep (`git maintenance`): run tasks now, schedule them, or (un)register this repository

## Usage

```sh
rgit maintenance [OPTIONS] <COMMAND>
```

## Examples

```sh
rgit maintenance run --task=gc
rgit maintenance start
```

## Subcommands

### rgit maintenance run

Run maintenance tasks now

#### Usage

```sh
rgit maintenance run [OPTIONS]
```

#### Examples

```sh
rgit maintenance run
rgit maintenance run --task=commit-graph --task=loose-objects
```

#### Options

| Option | Value | Description |
| --- | --- | --- |
| `--task` | `TASK` | Only this task (prefetch, loose-objects, incremental-repack, gc, commit-graph, pack-refs, reflog-expire, worktree-prune, rerere-gc); repeatable |
| `--auto` |  | Only the tasks whose auto conditions are met |
| `--schedule` | `FREQ` | Run the tasks of this schedule (hourly, daily, weekly) |
| `-q, --quiet` |  | Print nothing |

### rgit maintenance start

Register this repository and schedule hourly/daily/weekly runs (launchd on macOS, systemd timers or cron elsewhere)

#### Usage

```sh
rgit maintenance start [OPTIONS]
```

#### Examples

```sh
rgit maintenance start
rgit maintenance start --scheduler=crontab
```

#### Options

| Option | Value | Description |
| --- | --- | --- |
| `--scheduler` | `SCHEDULER` | auto, launchctl, systemd-timer or crontab |

### rgit maintenance stop

Remove the schedule (the repositories stay registered)

#### Usage

```sh
rgit maintenance stop [OPTIONS]
```

#### Examples

```sh
rgit maintenance stop
```

### rgit maintenance register

Add this repository to the global maintenance.repo list

#### Usage

```sh
rgit maintenance register [OPTIONS]
```

#### Examples

```sh
rgit maintenance register
```

### rgit maintenance unregister

Remove this repository from the global maintenance.repo list

#### Usage

```sh
rgit maintenance unregister [OPTIONS]
```

#### Examples

```sh
rgit maintenance unregister --force
```

#### Options

| Option | Value | Description |
| --- | --- | --- |
| `--force` |  | Do not fail when it is not registered |

See the [global options](global-options.md) that every command accepts.
