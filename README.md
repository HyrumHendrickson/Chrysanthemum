<p align="center"><img src="web/src/logo.webp" alt="Chrysanthemum" width="110"></p>

# Chrysanthemum

A local-first LaTeX editor with a live PDF preview and real-time collaboration,
in the spirit of Overleaf, running entirely on your own machine and network.
One binary, no cloud, no account.

- **Side-by-side editing.** CodeMirror editor on the left, rendered PDF on the right, with a formatting toolbar (headings, bold, italic, math, lists, links, comments).
- **Installs its own compiler.** One click installs TeX Live or Tectonic into Chrysanthemum's own folder, and one click removes it. No administrator rights, nothing system-wide.
- **Compiles locally** with that compiler or your own TeX installation (latexmk, pdfLaTeX, XeLaTeX, LuaLaTeX, or Tectonic). Auto-compile or press Ctrl+Enter.
- **Edit together** with anyone on your network. Changes merge conflict-free (Yjs CRDT), with live cursors and names.
- **Errors you can click.** Problems are parsed from the log, marked in the gutter, and jump to the offending line.
- **Plain files.** Each project is an ordinary folder you can open with any tool or put under Git.
- **Works offline.** The web interface is embedded in the binary; nothing is fetched from the internet.

## Requirements

To run: nothing else. If no TeX installation is found, Chrysanthemum offers to install one (see below). An existing [TeX Live](https://tug.org/texlive/), MacTeX, MiKTeX, or [Tectonic](https://tectonic-typesetting.github.io/) is used as is; `latexmk` is recommended because it handles bibliographies and reruns automatically.

To build: Rust 1.88+ and Node.js 22.13+.

## Build and run

```sh
git clone <this repository> chrysanthemum && cd chrysanthemum
cargo build --release          # also builds the web interface
./target/release/chrysanthemum   # then open http://localhost:3000
```

Set `CHRYSANTHEMUM_SKIP_WEB_BUILD=1` to skip the Node build step if `web/dist` already exists.

## Compilers

Open **Compilers** (top bar, or the prompt that replaces the preview when nothing can compile) to install or remove a compiler.

| Compiler | Download | On disk | Notes |
| --- | --- | --- | --- |
| **TeX Live** ([TinyTeX](https://yihui.org/tinytex/) 2026) | 54 to 67 MB (247 MB on Windows) | about 180 to 540 MB | pdfLaTeX, XeLaTeX, LuaLaTeX and latexmk. Closest to Overleaf, works offline. Missing LaTeX packages are fetched on first use. |
| **Tectonic** 0.17.0 | 10 to 22 MB | 26 to 55 MB | Small and fast, XeTeX-based. Downloads the packages a document needs on first use, so the first compile needs internet. |

- Compilers live in `<data dir>/.toolchains/` and are used ahead of anything already on your system. Removing one deletes that folder and leaves nothing behind. Compilers that were already on your computer are used but never removed.
- Downloads come from fixed, version-pinned GitHub release URLs and are verified against SHA-256 digests built into the program before anything is unpacked. The installed compiler must pass a self-test before it is put in place.
- Only the computer running Chrysanthemum can install or remove compilers; collaborators on the network cannot.
- TeX Live needs `perl` on Unix, as `latexmk` and `tlmgr` are Perl scripts. Without it Chrysanthemum calls the engine directly and cannot fetch missing packages.
- The installer is tested on Linux x86-64 with the real downloads. The macOS, Windows and ARM builds use the same code but have not been exercised on those systems.

## Collaborate on your network

```sh
chrysanthemum --lan
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
| `--data-dir DIR` | `CHRYSANTHEMUM_DATA_DIR` | platform data dir | Where projects are stored |
| `--host ADDR` | `CHRYSANTHEMUM_HOST` | `127.0.0.1` | Address to bind |
| `-p, --port N` | `CHRYSANTHEMUM_PORT` | `3000` | Port |
| `--lan` | | off | Bind `0.0.0.0` for network sharing |
| `--token T` | `CHRYSANTHEMUM_TOKEN` | generated | Access token (letters, digits, `-._~`) |
| `--no-auth` | | off | Disable the token on a network |
| `--compile-timeout S` | `CHRYSANTHEMUM_COMPILE_TIMEOUT` | `90` | Seconds before a compile is stopped |
| `--open` | | off | Open a browser on start |

Default data directory: `~/.local/share/chrysanthemum` (Linux), `~/Library/Application Support/Chrysanthemum` (macOS), `%APPDATA%\Chrysanthemum` (Windows).

## Using the editor

- **Files:** create, rename, delete, and upload (including drag and drop) in the left panel. The star marks the main document; choose another with the star button on a `.tex` file.
- **Engine:** pick pdfLaTeX, XeLaTeX, LuaLaTeX or Tectonic above the preview (per project). Engines that are not installed are disabled.
- **Compile:** Ctrl/Cmd+Enter or Ctrl/Cmd+S. Auto-compile runs shortly after you stop typing and can be turned off.
- **Shortcuts:** Ctrl/Cmd+B bold, Ctrl/Cmd+I italic, Ctrl/Cmd+Shift+M inline math, Ctrl/Cmd+F search, Ctrl/Cmd+Z undo (your own changes only).
- **Output:** the **Logs** button opens a panel with clickable problems and the raw log; "Clean build" removes generated files.
- **Download:** the PDF or a ZIP of the sources.

## How it works

```
 browser ──HTTP──► axum ──► project folders on disk
    │                 │
    └──WebSocket──────┤  Yjs rooms (one per open file, y-websocket protocol)
                      └► latexmk / pdflatex / … in the project directory
```

- The server hosts one Yjs document per open file and relays updates and presence between browsers. It writes the text back to the file shortly after each change and before every compile. CRDT state is stored in `.chrysanthemum/docs` so reconnecting clients merge cleanly even after a restart.
- Build output and metadata live in `<project>/.chrysanthemum/` and are excluded from ZIP exports.
- If a file changes on disk while nobody has it open, the next session loads the new content. If it changes while someone is editing, the disk change is overwritten by the next save.

## Upgrading from Scriptorium

This project was renamed. Your projects are kept: the data directory and each project's metadata folder are moved to their new names on first start. Environment variables changed from `SCRIPTORIUM_*` to `CHRYSANTHEMUM_*`, and the command from `scriptorium` to `chrysanthemum`.

## Limitations

- No SyncTeX click-to-jump between source and PDF yet.
- Presence (avatars) is shown per file, not per project.
- Only text files are collaboratively edited; images and PDFs can be uploaded and previewed.
- Tectonic support is less tested than the latexmk path.
- Installing compilers needs an internet connection and, on Unix, Perl for TeX Live.

## Development

See [CONTRIBUTING.md](CONTRIBUTING.md) and [AGENTS.md](AGENTS.md).

```sh
cargo test                       # unit and integration tests
cd web && npm run typecheck      # frontend types
```

## License

[MIT](LICENSE)
