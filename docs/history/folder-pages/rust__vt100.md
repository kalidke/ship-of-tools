# rust/vt100: the project's fork of vt100-ctt (capsule)

Fork of vt100-ctt 0.17.1 (MIT), the terminal-state parser a capsule keeps for its pane. It adds
`Screen::checkpoint` and `Parser::restore_screen`, an exact versioned hand-off of terminal state, and removes the
escape-sequence writing stack. Part of the capsule subsystem; charter: rust/log/CLAUDE.md. The vendored layout
(`src/`, `src/vte/`, `tests/`) follows upstream and is exempt from the size bounds (tools/exempt.txt).

## Files
- `CHANGELOG.md`: provenance and every difference from upstream.
- `Cargo.toml`: package `vt100-ctt`, with the vendoring notes in its header.
- `LICENSE`: upstream's MIT license.
- `README.md`: upstream's readme.
- `tests/`: upstream's suite and fixture corpus (`tests/data`) plus the checkpoint tests (`tests/checkpoint/`).
- `src/attrs.rs`: cell attributes.
- `src/callbacks.rs`: the callback trait the parser reports to.
- `src/cell.rs`: one screen cell.
- `src/checkpoint.rs`: the checkpoint format, its encoder and its fail-closed decoder.
- `src/grid.rs`: the grid of rows, the scroll region and the scrollback ring.
- `src/lib.rs`: crate root and public re-exports.
- `src/parser.rs`: `Parser`, the byte-stream entry point, including `is_ground`.
- `src/perform.rs`: the `vte::Perform` implementation that applies parsed sequences to the screen.
- `src/row.rs`: one row of cells and its wrap flag.
- `src/screen.rs`: `Screen`, the visible state and its getters.
- `src/vte/`: vendored vte parser core plus utf8parse, with their licenses.

## Start here
`src/checkpoint.rs` (its module doc is the format) for a format change; `tests/checkpoint/main.rs` for how the format
tests are organised (roundtrips in main, `rejects`, `canonical` and `pinned` beside it).

## Rules
- The checkpoint format is versioned and old versions are still read: `VERSION` is written, `MIN_READABLE_VERSION`
  through `VERSION` are read, and `tests/checkpoint/pinned.rs` pins the version 1 and version 2 bytes.
- The crate version stays the upstream release it forks.
- `CHANGELOG.md` records every difference from upstream.
- The fork is wired in by `[patch.crates-io]` in rust/Cargo.toml, so dependents keep their `package = "vt100-ctt"` lines.
- `src/vte/` vendors vte's parser core so `Parser::is_ground` can report the parser state.
