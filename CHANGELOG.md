# Changelog

## 0.2.0

Renamed from Scriptorium to Chrysanthemum, with a redesigned interface and a built-in compiler installer.

- **New name and logo.** The command is `chrysanthemum` and environment variables start with `CHRYSANTHEMUM_`. Existing data directories and project metadata are migrated automatically.
- **Install and remove compilers from the app.** TeX Live (TinyTeX) and Tectonic are downloaded from pinned URLs, verified against SHA-256 digests, self-tested, and kept in `<data dir>/.toolchains`. Progress and cancel are shown; removal is refused while a document is compiling. Only the host computer may change compilers.
- **Missing LaTeX packages are fetched automatically** for the managed TeX Live (best effort; needs internet).
- **Redesigned interface** in the style of Overleaf: dark navigation bar and file tree, project dashboard with search, editor toolbar, PDF toolbar with a Recompile button, and an install prompt in place of the preview when no compiler exists.
- **Fixed the side-by-side layout.** The pane grid used `fr` units inside `calc()`, which browsers reject, so the editor and preview could stack vertically.
- New projects load Latin Modern fonts when available, which avoids a font failure on minimal TeX installations.
- The expected "Shell escape feature is not enabled" notice is no longer listed as a warning.
- `latexmk` is used only when Perl is available; otherwise the engine is called directly.

## 0.1.0

Initial release.

- Local projects stored as plain folders.
- Compilation with latexmk, pdfLaTeX, XeLaTeX, LuaLaTeX or Tectonic; structured errors and warnings.
- Browser editor with LaTeX highlighting, completion, and live PDF preview.
- Real-time collaboration over the local network (Yjs CRDT), with presence and cursors.
- Optional access-token authentication for network sharing.
