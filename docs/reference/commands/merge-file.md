# rgit merge-file

Three-way merge of files into the first, like `git merge-file`; exits with the number of conflicts

## Usage

```sh
rgit merge-file [OPTIONS] <CURRENT> <BASE> <OTHER>
```

## Examples

```sh
rgit merge-file ours.txt base.txt theirs.txt
rgit merge-file -p --diff3 -L a -L b -L c a b c
```

## Arguments

| Argument | Description |
| --- | --- |
| `<CURRENT>` |  |
| `<BASE>` |  |
| `<OTHER>` |  |

## Options

| Option | Value | Description |
| --- | --- | --- |
| `-L` | `LABEL` | Labels for current, base and other (up to three -L) |
| `-p, --stdout` |  | Print the result instead of writing the current file |
| `--ours` |  | Resolve conflicts with our side |
| `--theirs` |  | Resolve conflicts with their side |
| `--union` |  | Keep both sides of conflicts |
| `--diff3` |  | Show the base version in conflicts |
| `--zdiff3` |  | Like --diff3, with common lines moved out of the conflict |
| `--marker-size` | `N` | Conflict marker length |
| `-q, --quiet` |  | Do not warn about conflicts |
| `--object-id` |  | The arguments are blob ids; write the result as a blob and print its id |

See the [global options](global-options.md) that every command accepts.
