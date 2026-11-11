# AGENTS.md

Guidance for AI coding agents and new contributors working on Chrysanthemum.

## What this is

A local-first, collaborative LaTeX editor. One Rust binary serves a web UI,
compiles projects with the user's own TeX installation, and synchronises edits
between browsers on the same network using Yjs (CRDT) over WebSockets.

## Layout

| Path | Purpose |
| --- | --- |
| `src/store.rs` | Project storage on disk, path validation, settings, ZIP export |
| `src/toolchain.rs` | Compiler catalog (pinned URLs and digests), detection, install, remove |
| `src/compile.rs` | Process supervision, TeX log parsing, automatic package fetching |
| `src/collab.rs` | Yjs rooms, y-websocket protocol, presence, persistence |
| `src/api.rs` | HTTP and WebSocket routes, security headers |
| `src/guard.rs` | Host/origin checks and access-token authentication |
| `src/assets.rs` | Serves the embedded web UI |
| `src/config.rs`, `src/main.rs` | CLI and startup |
| `build.rs` | Builds `web/` into `web/dist` for embedding |
| `web/src/` | TypeScript UI (no framework); bundled with esbuild |
| `web/src/toolchain.ts`, `compilers.ts` | Live compiler status store and the Compilers dialog |
| `tests/api.rs` | Integration tests (HTTP, compile, two-client sync) |
| `web/test/e2e.mjs` | Runs the real browser provider against a live server |

## Commands

```sh
cargo build                 # also builds the web UI (needs Node 22.13+)
cargo test                  # unit + integration tests (compile tests skip without TeX)
cargo fmt --all && cargo clippy --all-targets -- -D warnings
cd web && npm ci && npm run typecheck && npm run build
cd web && npm run watch     # rebuild UI on change (debug binaries read web/dist from disk)
# end-to-end (server must be running on port 3111):
cargo run -- --data-dir /tmp/sdata --port 3111 &
cd web && CHRYSANTHEMUM_URL=http://127.0.0.1:3111 npm run test:e2e
```

## Conventions

- Run fmt, clippy and tests before finishing; keep clippy warning-free.
- Errors go through `AppError`; never return internal details to clients.
- Every project-relative path from a client must pass `store::clean_rel_path`
  and `Store::resolve`. Never join client input onto a path directly.
- Do not add dependencies casually. `yrs` is pinned to 0.23 for compiler
  compatibility; the wire format must stay compatible with `yjs` 13 and
  `y-protocols`.
- The server is the only writer of Yjs document seeds. Clients must never seed
  content, or reconnecting clients would duplicate text.
- The `epoch` message (type 100) tells clients that the server document was
  replaced; keep it in sync between `collab.rs` and `web/src/provider.ts`.
- The UI must work under the CSP in `src/api.rs`: no inline scripts, no remote
  resources. Everything must run offline.
- Add tests with behaviour changes. Compile tests must skip when TeX is absent.

- Compiler downloads must stay pinned: to update a version, change the URL and the SHA-256 together in `toolchain::catalog()`. `catalog_is_well_formed` checks the shape; compute digests from the real files, never by hand.
- Programs run by the compile step must be started through `Resolved::program` and `run_command`, so that managed compilers come first on `PATH`.
- Anything that installs or removes compilers must call `require_local` in `api.rs`.
- CSS: never use `fr` units inside `calc()`; set whole `fr` values from script (see `--col-editor`).

## Security model

Default bind is loopback only. `--lan` binds all interfaces and requires a
token unless `--no-auth`. Anyone with access can edit files and run LaTeX on the
host, so shell escape stays disabled and compilations are time-limited. Treat
changes to `guard.rs`, path handling, and engine arguments as security-sensitive.
