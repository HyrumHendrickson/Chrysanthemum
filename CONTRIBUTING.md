# Contributing

Thanks for helping improve Chrysanthemum.

1. Install Rust (1.88 or newer), Node.js 22.13 or newer, and a TeX distribution
   (TeX Live with `latexmk` is recommended).
2. `cargo build && cargo test`
3. For UI work, run `npm run watch` in `web/` and start a debug build of the
   server; debug builds serve `web/dist` from disk.
4. Before opening a pull request: `cargo fmt --all`,
   `cargo clippy --all-targets -- -D warnings`, `cargo test`,
   `npm run typecheck` in `web/`, and the end-to-end test described in
   `AGENTS.md`.

Keep changes focused, add tests for behaviour changes, and describe user-visible
changes in `CHANGELOG.md`. Report security issues as described in `SECURITY.md`.
