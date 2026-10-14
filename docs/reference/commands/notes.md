# rgit notes

Notes attached to commits (no subcommand lists them)

## Usage

```sh
rgit notes [OPTIONS] [COMMAND]
```

## Examples

```sh
rgit notes
rgit notes show HEAD
```

## Options

| Option | Value | Description |
| --- | --- | --- |
| `--ref` | `REF` | The notes ref (default refs/notes/commits) |

## Subcommands

### rgit notes list

List notes as `<note id> <object id>`, or the note id of one object

#### Usage

```sh
rgit notes list [OPTIONS] [REV]
```

#### Examples

```sh
rgit notes list
```

#### Arguments

| Argument | Description |
| --- | --- |
| `[REV]` | The annotated object |

### rgit notes show

Print an object's note (default HEAD)

#### Usage

```sh
rgit notes show [OPTIONS] [REV]
```

#### Examples

```sh
rgit notes show
rgit notes show <rev>
```

#### Arguments

| Argument | Description |
| --- | --- |
| `[REV]` | The annotated object |

### rgit notes add

Attach a note to an object (default HEAD); opens the editor without -m, -F or -C

#### Usage

```sh
rgit notes add [OPTIONS] [REV]
```

#### Examples

```sh
rgit notes add -m "<note>"
rgit notes add <rev> -m "<note>" --force
rgit notes add -F notes.txt <rev>
rgit notes add --no-stripspace -F notes.txt <rev>
```

#### Arguments

| Argument | Description |
| --- | --- |
| `[REV]` | The annotated object |

#### Options

| Option | Value | Description |
| --- | --- | --- |
| `-m, --message` | `MESSAGE` | The note text; several -m become paragraphs |
| `-F, --file` | `FILE` | Read the note text from this file (`-` for stdin) |
| `-C, --reuse-message` | `OBJECT` | Reuse this blob (e.g. another note) as the note |
| `-c, --reedit-message` | `OBJECT` | Like -C, then edit it |
| `--allow-empty` |  | Keep an empty note instead of removing it |
| `-e, --edit` |  | Edit the text in the editor before saving |
| `--separator` | `TEXT` | Put this line between paragraphs instead of a blank line |
| `--no-separator` |  | Put nothing between paragraphs |
| `--no-stripspace` |  | Keep the text as given: no cleanup of blank lines and trailing spaces |
| `--stripspace` |  | Clean up blank lines and trailing spaces (the default, but for -C) |
| `-f, --force` |  | Replace an existing note |

### rgit notes copy

Copy the note of one object to another (default HEAD)

#### Usage

```sh
rgit notes copy [OPTIONS] <FROM> [TO]
```

#### Examples

```sh
rgit notes copy <from> <to>
```

#### Arguments

| Argument | Description |
| --- | --- |
| `<FROM>` | The object whose note is copied |
| `[TO]` | The object that gets it (default HEAD) |

#### Options

| Option | Value | Description |
| --- | --- | --- |
| `-f, --force` |  | Replace an existing note |

### rgit notes append

Add a paragraph to an object's note, creating it if needed

#### Usage

```sh
rgit notes append [OPTIONS] [REV]
```

#### Examples

```sh
rgit notes append -m "<more>"
```

#### Arguments

| Argument | Description |
| --- | --- |
| `[REV]` | The annotated object |

#### Options

| Option | Value | Description |
| --- | --- | --- |
| `-m, --message` | `MESSAGE` | The note text; several -m become paragraphs |
| `-F, --file` | `FILE` | Read the note text from this file (`-` for stdin) |
| `-C, --reuse-message` | `OBJECT` | Reuse this blob (e.g. another note) as the note |
| `-c, --reedit-message` | `OBJECT` | Like -C, then edit it |
| `--allow-empty` |  | Keep an empty note instead of removing it |
| `-e, --edit` |  | Edit the text in the editor before saving |
| `--separator` | `TEXT` | Put this line between paragraphs instead of a blank line |
| `--no-separator` |  | Put nothing between paragraphs |
| `--no-stripspace` |  | Keep the text as given: no cleanup of blank lines and trailing spaces |
| `--stripspace` |  | Clean up blank lines and trailing spaces (the default, but for -C) |

### rgit notes edit

Edit an object's note in the editor (default HEAD)

#### Usage

```sh
rgit notes edit [OPTIONS] [REV]
```

#### Examples

```sh
rgit notes edit <rev>
```

#### Arguments

| Argument | Description |
| --- | --- |
| `[REV]` | The annotated object |

#### Options

| Option | Value | Description |
| --- | --- | --- |
| `--allow-empty` |  | Keep an empty note instead of removing it |

### rgit notes remove

Remove the notes of objects (default HEAD)

#### Usage

```sh
rgit notes remove [OPTIONS] [REVS]...
```

#### Examples

```sh
rgit notes remove <rev>
rgit notes remove --ignore-missing a b
```

#### Arguments

| Argument | Description |
| --- | --- |
| `[REVS]...` | The annotated objects |

#### Options

| Option | Value | Description |
| --- | --- | --- |
| `--ignore-missing` |  | Do not fail on an object without a note |

### rgit notes prune

Remove the notes of objects that no longer exist

#### Usage

```sh
rgit notes prune [OPTIONS]
```

#### Examples

```sh
rgit notes prune -n
```

#### Options

| Option | Value | Description |
| --- | --- | --- |
| `-n, --dry-run` |  | List them without removing (git's -n) |
| `-v, --verbose` |  | Name each removed note's object |

### rgit notes merge

Merge another notes ref into the current one, with their merge base

#### Usage

```sh
rgit notes merge [OPTIONS] [NOTES_REF]
```

#### Examples

```sh
rgit notes merge -s union origin
rgit notes merge origin
rgit notes merge --commit
rgit notes merge --abort
```

#### Arguments

| Argument | Description |
| --- | --- |
| `[NOTES_REF]` | The notes ref to merge (e.g. `origin` for refs/notes/origin) |

#### Options

| Option | Value | Description |
| --- | --- | --- |
| `-s, --strategy` | `STRATEGY` | On conflicting notes: manual (leave them in .git/NOTES_MERGE_WORKTREE), ours, theirs, union or cat_sort_uniq (default: notes.&lt;ref&gt;.mergeStrategy, notes.mergeStrategy, manual) |
| `--commit` |  | Commit the notes resolved in .git/NOTES_MERGE_WORKTREE |
| `--abort` |  | Abandon a manual notes merge |
| `-v, --verbose` |  | Say more (repeatable) |
| `-q, --quiet` |  | Say less |

### rgit notes get-ref

Print the notes ref in use

#### Usage

```sh
rgit notes get-ref [OPTIONS]
```

#### Examples

```sh
rgit notes --ref review get-ref
```

See the [global options](global-options.md) that every command accepts.
