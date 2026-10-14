# rgit serve

Serve the web viewer for this repository, or a directory of repositories

## Usage

```sh
rgit serve [OPTIONS]
```

## Examples

```sh
rgit serve
rgit serve --port 9000
rgit serve --root ~/code --clone-base https://git.example.dev
```

## Options

| Option | Value | Description |
| --- | --- | --- |
| `--host` | `HOST` | Address to bind (default: `127.0.0.1`) |
| `--port` | `PORT` | Port to listen on (default: `8080`) |
| `--root` | `ROOT` | Serve every git repository under this directory, addressed by name, instead of the current repository |
| `--clone-base` | `CLONE_BASE` | Public clone base (e.g. https://git.example.dev). When set, the shown clone URL is &lt;base&gt;/&lt;repo&gt;.git instead of the repo's own remotes |

See the [global options](global-options.md) that every command accepts.
