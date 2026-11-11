# Scriptorium

A local-first LaTeX editor with a live PDF preview and real-time collaboration,
in the spirit of Overleaf, running entirely on your own machine and network.
One binary, no cloud, no account.

- **Side-by-side editing.** CodeMirror editor on the left, rendered PDF on the right.
- **Compiles locally** with your own TeX installation (latexmk, pdfLaTeX, XeLaTeX, LuaLaTeX, or Tectonic). Auto-compile or press Ctrl+Enter.
- **Edit together** with anyone on your network. Changes merge conflict-free (Yjs CRDT), with live cursors and names.
- **Errors you can click.** Problems are parsed from the log, marked in the gutter, and jump to the offending line.
- **Plain files.** Each project is an ordinary folder you can open with any tool or put under Git.
- **Works offline.** The web interface is embedded in the binary; nothing is fetched from the internet.

## Requirements

To run: a TeX distribution on the host ([TeX Live](https://tug.org/texlive/), MacTeX, MiKTeX, or [Tectonic](https://tectonic-typesetting.github.io/)). `latexmk` is recommended because it handles bibliographies and reruns automatically. Without a TeX installation you can still write and collaborate.

To build: Rust 1.88+ and Node.js 22.13+.

## Build and run

```sh
git clone <this repository> scriptorium && cd scriptorium
cargo build --release          # also builds the web interface
./target/release/scriptorium   # then open http://localhost:3000
```

Set `SCRIPTORIUM_SKIP_WEB_BUILD=1` to skip the Node build step if `web/dist` already exists.

## Collaborate on your network

```sh
scriptorium --lan
```

The terminal prints join links containing a one-time access token, for example
`http://192.168.1.20:3000/?token=…`. Send a link to a teammate on the same
network; opening it stores the token in a cookie. The **Share** button in the
editor shows the same links for the current project.

Everyone who joins edits the same live document. Anyone with a link can edit
files and run LaTeX on your machine, so share it only with people you trust.
Traffic is unencrypted HTTP; for untrusted networks put a TLS reverse proxy in front.

## Options

| Flag | Environment | Default | Meaning |
| --- | --- | --- | --- |
| `--data-dir DIR` | `SCRIPTORIUM_DATA_DIR` | platform data dir | Where projects are stored |
| `--host ADDR` | `SCRIPTORIUM_HOST` | `127.0.0.1` | Address to bind |
| `-p, --port N` | `SCRIPTORIUM_PORT` | `3000` | Port |
| `--lan` | | off | Bind `0.0.0.0` for network sharing |
| `--token T` | `SCRIPTORIUM_TOKEN` | generated | Access token (letters, digits, `-._~`) |
| `--no-auth` | | off | Disable the token on a network |
| `--compile-timeout S` | `SCRIPTORIUM_COMPILE_TIMEOUT` | `90` | Seconds before a compile is stopped |
| `--open` | | off | Open a browser on start |

Default data directory: `~/.local/share/scriptorium` (Linux), `~/Library/Application Support/Scriptorium` (macOS), `%APPDATA%\Scriptorium` (Windows).

## Using the editor

- **Files:** create, rename, delete, and upload (including drag and drop) in the left panel. The star marks the main document; choose another with the star button on a `.tex` file.
- **Engine:** pick pdfLaTeX, XeLaTeX, LuaLaTeX or Tectonic from the toolbar (per project).
- **Compile:** Ctrl/Cmd+Enter or Ctrl/Cmd+S. Auto-compile runs shortly after you stop typing and can be turned off.
- **Shortcuts:** Ctrl/Cmd+B bold, Ctrl/Cmd+I italic, Ctrl/Cmd+Shift+M inline math, Ctrl/Cmd+F search, Ctrl/Cmd+Z undo (your own changes only).
- **Output:** the bottom panel lists problems and the raw log; "Clean build" removes generated files.
- **Download:** the PDF or a ZIP of the sources.

## How it works

```
 browser ──HTTP──► axum ──► project folders on disk
    │                 │
    └──WebSocket──────┤  Yjs rooms (one per open file, y-websocket protocol)
                      └► latexmk / pdflatex / … in the project directory
```

- The server hosts one Yjs document per open file and relays updates and presence between browsers. It writes the text back to the file shortly after each change and before every compile. CRDT state is stored in `.scriptorium/docs` so reconnecting clients merge cleanly even after a restart.
- Build output and metadata live in `<project>/.scriptorium/` and are excluded from ZIP exports.
- If a file changes on disk while nobody has it open, the next session loads the new content. If it changes while someone is editing, the disk change is overwritten by the next save.

## Limitations

- No SyncTeX click-to-jump between source and PDF yet.
- Presence (avatars) is shown per file, not per project.
- Only text files are collaboratively edited; images and PDFs can be uploaded and previewed.
- Tectonic support is implemented but less tested than the latexmk path.

## Development

See [CONTRIBUTING.md](CONTRIBUTING.md) and [AGENTS.md](AGENTS.md).

```sh
cargo test                       # unit and integration tests
cd web && npm run typecheck      # frontend types
```

## License

[MIT](LICENSE)
