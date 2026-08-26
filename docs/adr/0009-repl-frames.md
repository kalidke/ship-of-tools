# ADR 0009: REPL streaming frame format

**Status:** Accepted — **revised 2026-08-24** (see Update; the original Decision
below describes the design as first accepted, and shipped reality diverged on
framing, binary payloads, and file layout)
**Date:** 2026-05-07

## Update (2026-08-24) — shipped reality + ready sentinel

Recorded as part of the Ship's Log P0.5 adapter-semantics pass (ADR 0037), which
audited this protocol against source and found the original text stale in four
ways. What is actually shipped:

- **Framing is newline-delimited JSON (NDJSON), not length-prefixed.** One JSON
  envelope per `\n`-terminated line, both directions, over the child's stdio.
  Every message is wrapped in `{v, id, kind: "req"|"res"|"evt", op, payload}`
  (`v` = protocol version, currently 1); frames ride inside `repl.frame` evt
  payloads as `{eval_id, frame}`. Emission is lock-serialized in the shim, so
  envelopes never interleave.
- **Binary payloads ship inline as base64, not as `blob <ref>`.** The blob-ref
  indirection was never built: `image` frames are
  `{kind, mime, data_base64, bytes}` with `mime ∈ {image/png, image/svg+xml}`.
  Consequence to know: the daemon→frontend leg enforces a 1 MiB envelope cap,
  so an oversized streamed image is rejected at write; the `repl.execute` path
  (ADR 0033) sidesteps this by spilling figures to files and returning paths.
  A future content-addressed artifact store (ADR 0037 P1) is the real fix.
- **Two frame kinds were added since acceptance:** `browser` —
  `{kind, url, open}`, a live loopback-served artifact (wglshow/Bonito,
  ADR 0032); and the backend-synthesized `lifecycle` / `started` frames
  (`repl_state` transitions and `repl.execute` pre-registration) which the shim
  never emits but which share the frame bus.
- **The shim lives in `julia/repl/src/ShipToolsRepl.jl`** (a full serve loop +
  eval engine), not the `repl/src/DisplayShim.jl` sketched below.
- **Ready sentinel (new in this revision).** serve's first act is now an evt
  envelope `op = "repl.ready"`, payload `{julia, protocol}`, emitted at the
  exact point the dispatch loop begins. The supervisor's `starting → ready`
  transition previously keyed off "first stdout line", which only existed once
  the first eval produced output — a booted-but-idle child read `starting`
  indefinitely. The first-line trigger is retained as a fallback for older
  shims; the sentinel makes the signal designed rather than incidental.

The Decision section below is kept as-written for the historical record.

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
