# ADR 0009: REPL streaming frame format

**Status:** Accepted
**Date:** 2026-05-07

## Context

The REPL produces multi-modal output: stdout text, stderr text, return values (which may have multiple MIME representations), images (CairoMakie figures, PNGs), and structured errors with stacktraces. The frontend needs ordered, typed frames to render each correctly.

## Decision

Length-prefixed JSON frames from the REPL display shim, wrapped at the backend into `repl.frame` events on the main protocol stream.

Frame kinds:

- `stdout` — `{text: String}`
- `stderr` — `{text: String}`
- `value` — `{mime: String, text: String}` for textual MIMEs; `{mime: "image/png", blob: <ref>}` for binary
- `image` — `{mime: "image/png", blob: <ref>}` (convenience for CairoMakie etc.)
- `error` — `{message: String, stacktrace: [{file, line, fn}, ...]}`
- `done` — `{eval_id: u64, elapsed_ms: u64}`

Borrow IJulia's `display_data` shape, flattened: no separate metadata channel, all fields at the top level of each frame.

Display shim lives in `repl/src/DisplayShim.jl`. Registers a `MIMEDisplay` that captures `display`/`show` calls. CairoMakie figures route through `show(io, "image/png", x)` automatically.

## Consequences

- Frame ordering is preserved because the REPL process has a single stdout writer.
- `done` frame is the eval-completion signal — backend correlates by `eval_id` to clear UI spinners.
- Adding a new MIME type (HTML, LaTeX, custom) is a frontend-side render change; the frame schema is open via the `mime` field.
- Errors are structured (not just stderr text) so the frontend can render stacktraces with file:line links to the editor.
