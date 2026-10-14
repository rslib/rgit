# rgit tool

Run one MCP tool with its arguments as a JSON object on stdin and print its TOON result, as the pi and omp extension does

## Usage

```sh
rgit tool [OPTIONS] <NAME>
```

## Examples

```sh
echo '{}' | rgit tool git_status
echo '{"limit": 5}' | rgit tool git_log
```

## Arguments

| Argument | Description |
| --- | --- |
| `<NAME>` | The tool, e.g. git_status |

See the [global options](global-options.md) that every command accepts.
