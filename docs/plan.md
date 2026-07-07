# Ship of Tools — Phase 1 Implementation Plan

A dependency-ordered plan for the Phase 1 milestone defined in `CLAUDE.md`. The first milestone is a thin end-to-end skeleton; each subsequent milestone adds exactly one capability.

> **Note (2026-05-07):** Plan revised after the C-pivot. The original plan assumed the frontend was a TUI rendered into the user's terminal. That premise was wrong for the user's deployment (Windows → SSH → tmux → Linux remote). The revised plan keeps the kernel, core IR, line protocol, and backend daemon intact but rebuilds the frontend as a **native local window** that owns rendering end-to-end.

## Scope

Phase 1, per `CLAUDE.md`:

- Files mode (filesystem nav with previews — markdown, PNG, syntax-highlighted .jl)
- Modules mode, read-only (structural via `JuliaSyntax.jl`)
- Persistent Julia REPL with code dispatch (line, block, file)
- Orchestrator LLM chat with file + REPL tools
- `.concept/` annotation read+write with AST-hash provenance
- One demo external plugin (HDF5 preview), validating the ABI from outside core

Out: Types/Math/Outputs modes, multi-agent, multi-laptop semantics beyond first-wins, background staleness sweep, embedded editor, MP4 playback, automatic plot capture.

## Risk register

| Risk | De-risked in |
|------|--------------|
| Native rendering of markdown + PNG + MathJax-SVG end-to-end | **M1 spike** (the go/no-go gate) |
| ratatui custom backend (cell stream into wgpu) — prior art partial | M1 spike: try `ratatui-wgpu` first; roll our own if it can't carry our requirements |
| SSH Unix-socket forwarding on Windows OpenSSH | M1 spike: confirm StreamLocalBindUnlink + LocalForward path; TCP per-session port is the fallback |
| Reconnect restoring state without losing in-flight work | M1 spike: kill the frontend, restart, see kernel state and tree position resume |
| Cross-platform Julia subprocess supervision | M2 — verify no orphaned `julia` after Ctrl-C / `taskkill` / task-manager kill |
| AST hash stability across `JuliaSyntax` versions | M3 property test + pinned version + pinned hash format |
| `JuliaSyntax` parsing perf on large files | M3 cache by `(path, mtime, size)` |
| Anthropic streaming + tool-use loop | M6 — build against recorded fixture before live API |

## Milestones

### M0 — Decisions and scaffolding (DONE — pivoted)

**Status:** core/, julia/kernel/, rust/protocol/, rust/backend/ scaffolds are committed and stay. rust/frontend/ as committed targeted crossterm; pruned in the M0 closeout (the `ratatui-image` / `crossterm` deps and `rust/scratch/img-probe/` are removed).

### M1 — Spike: native window + chrome + previews + remote socket + reconnect (1 week)

The go/no-go gate for option C. If this lands, C is defensible. If preview-layer requirements start pulling in HTML/iframes/WebGL within the spike, fall back to **option B** (Tauri webview wrapping a web frontend) — kernel/backend/protocol stay intact across the swap.

**Deliverable:** a single `sot` binary that:
1. Opens a native local window (winit + wgpu).
2. Runs a ratatui chrome layer (the mode tree, status bar, focus) via a custom `Backend` impl that paints cells with cosmic-text into the wgpu surface.
3. Renders a preview pane in three modes:
   - PNG via `image` → wgpu texture
   - Markdown via comrak → cosmic-text laid out lines
   - Inline math via MathJax-served SVG → resvg → wgpu texture
4. Connects to a backend instance running in a named tmux session on a remote host via SSH-forwarded per-session Unix socket; reconnects after frontend kill restoring tree position and last preview.

**Files (sketch):**
- `rust/frontend/src/main.rs` — winit event loop, wgpu surface init, connect-and-spawn flow.
- `rust/frontend/src/chrome.rs` — ratatui custom `Backend` impl.
- `rust/frontend/src/preview/mod.rs` — preview-layer dispatch on MIME.
- `rust/frontend/src/preview/{png,markdown,svg}.rs` — three concrete renderers.
- `rust/frontend/src/transport.rs` — SSH-spawn + Unix-socket connect + reconnect loop.
- `rust/backend/src/main.rs` — Unix-socket listener, session-id handshake, kernel supervisor.
- `rust/backend/src/session.rs` — session state (tree cursor, preview cache, revision counter for reconnect).
- `rust/backend/src/mathjax.rs` — Node sidecar wrapper (math snippet → SVG).
- `julia/kernel/src/ShipToolsKernel.jl` — minimal NDJSON dispatch returning fixed payloads for the spike.
- `rust/protocol/src/lib.rs` — Request/Response/Event enums for the spike's ops only (`hello`, `preview.get`, `tree.root`).

**Acceptance:**
- Render PNG side-by-side with a real CairoMakie figure, eyeball quality.
- Render a markdown file with embedded LaTeX (`$\int_0^\infty e^{-x^2}\,dx$`); math appears typeset, not as a code box.
- Kill the frontend with the backend running; relaunch; tree cursor and preview restore from `last_seen_revision`.
- Run on Linux (local → local-loopback) and Windows (Windows local → Linux remote) end to end.

If any of those fail in a way the architecture can't fix, switch to option B before continuing.

### M2 — Files mode populated, file watching (3–4 days)

**Deliverable:** real filesystem navigation. `.md` rendered, `.png` rendered, `.jl` shown as syntax-highlighted text. External edits update the tree within 500 ms.

**Files:**
- `julia/plugins/files-mode/src/FilesMode.jl` — `tree_root`, `tree_children`, `preview_for`.
- `julia/plugins/markdown/src/MarkdownPreview.jl` — `preview(::Type{MarkdownDoc}, path) → PreviewPayload(mime="text/markdown", data=bytes)`.
- `julia/plugins/png/src/PngPreview.jl` — read bytes → `image/png` payload.
- `julia/plugins/julia-source/src/JuliaSourcePreview.jl` — read + syntect-friendly metadata → `text/plain` with extras.
- `rust/backend/src/watcher.rs` — `notify` watcher → `file.changed` events.

**Acceptance:** open the Ship of Tools repo, navigate to `requirements.md`, see it rendered. Drop a PNG, navigate, see it. Edit a file externally; tree refreshes.

### M3 — Modules mode (read-only) + AST hash (3–4 days)

**Deliverable:** hotkey switches root tree from Files to Modules. Columns: modules → functions → methods. Preview shows method source.

**Files:**
- `julia/plugins/julia-source/src/JuliaSourceParser.jl` — `parse_entities` walks `JuliaSyntax.parseall`.
- `julia/plugins/julia-source/src/Index.jl` — project index keyed by `(path, mtime, size)`; incremental rebuild on `file.changed`.
- `julia/plugins/modules-mode/src/ModulesMode.jl` — mode-tree dispatch.
- `core/src/ASTHash.jl` — hash function.
- `core/test/test_ast_hash.jl` — property test.
- `rust/frontend/src/state.rs` — per-mode cursor preservation; modes switch via `f` / `m` / `s` / `h` in nav focus.

**Acceptance:** open the Ship of Tools repo, switch to Modules mode, navigate to a method, see source. Property test passes.

### M4 — Persistent REPL with code dispatch (3–4 days)

**Deliverable:** toggleable REPL pane (`~`). `<Enter>` evaluates the line at cursor; `<S-Enter>` evaluates the surrounding top-level block; `<C-Enter>` includes the whole file. CairoMakie figures render inline through the preview-layer (NOT through any terminal protocol).

> Shipped (diverged from the original plan): the REPL toggles with `Ctrl+J`; whole-file run landed as Files-mode `r` (fresh REPL) / `R` (current session); at the prompt `Enter` submits and `Shift+Enter` inserts a newline. Per-line / per-block dispatch is still planned.

**Files:**
- `repl/Project.toml`, `repl/src/ShipToolsRepl.jl`.
- `repl/src/DisplayShim.jl` — `MIMEDisplay` emits framed JSON.
- `rust/backend/src/repl.rs` — REPL supervisor; `repl.eval` streams `repl.frame` events tagged with `eval_id`.
- `rust/frontend/src/repl_pane.rs` — consumes `repl.frame`, dispatches to preview-layer for inline images.
- `rust/frontend/src/dispatch.rs` — block-span computation via kernel round-trip.

**Acceptance:** `x = 1+1` → `2`. Plot returns a wgpu texture inline. Killing REPL via UI doesn't kill kernel; restart works.

### M5 — `.concept/` annotations with AST-hash provenance (2–3 days)

**Deliverable:** Modules-mode preview shows source on top, matching `.concept/` annotation below. Stale badge when AST hash mismatches. `<S-e>` opens annotation in `$EDITOR`. `<r>` refreshes provenance. *(As built: the annotation is edited inline with `e` / `Ctrl+S` rather than via `$EDITOR`, and the one-key `<r>` provenance refresh was not implemented — see the [concept layer](../guide/concept-layer.md) guide.)*

**Files:**
- `julia/plugins/concept/src/ConceptStore.jl` — `read_annotation`, `write_annotation`, `is_stale`. YAML frontmatter via `YAML.jl`.
- `julia/plugins/concept/src/Provenance.jl` — stamps `synced_against` and `synced_at` on write.
- `julia/plugins/modules-mode/src/ModulesMode.jl` — extend `preview_for` to compose source + annotation + badge.
- `rust/frontend/src/preview/composite.rs` — multi-pane preview composition (source on top, annotation below, badge overlay).
- `rust/backend/src/editor.rs` — shell out to `$EDITOR`, await exit, signal kernel re-read.

**Acceptance:** create a method, write annotation, save. Modify the body; reopen → annotation shows stale badge. `<r>` clears it. *(As built, the badge is cleared by re-editing and re-saving the annotation, not by a dedicated `<r>` keypress.)*

### M6 — Orchestrator chat with tool dispatch (4–5 days)

**Deliverable:** chat pane (`<Space>c`). User types; orchestrator streams. Tools: `read_file`, `write_file`, `read_annotation`, `write_annotation`, `repl_eval`, `list_modules`. Demo: "summarize module X" → tool calls visible as collapsed breadcrumbs → optional annotation written.

**Files:**
- `rust/backend/src/orchestrator/mod.rs` — Anthropic client, SSE streaming, tool-use loop.
- `rust/backend/src/orchestrator/tools.rs` — tool spec construction.
- `rust/backend/src/orchestrator/cache.rs` — four-block cache scheme.
- `rust/frontend/src/chat_pane.rs` — input area, transcript, streaming render via cosmic-text, collapsed tool-call breadcrumbs.
- `core/src/ToolSpec.jl` — `tool_spec` and `tool_call` dispatch surfaces *defined but unwired*. Phase 2 seam.

**Acceptance:** ask the orchestrator to "create an annotation for function X." File appears with proper frontmatter; reopening Modules mode shows the new annotation.

### M7 — Demo external plugin: `HDF5Preview.jl` (1–2 days)

**Deliverable:** separate package at `examples/plugins/HDF5Preview/` that, when listed in a project's `[sot].extensions`, makes `.h5` files previewable.

**Files:**
- `examples/plugins/HDF5Preview/Project.toml` — depends on `ConceptExplorerCore`, `HDF5`.
- `examples/plugins/HDF5Preview/src/HDF5Preview.jl` — `struct H5File <: FileType end`, dispatches `preview`.
- `examples/plugins/README.md`.

**Acceptance:** drop an `.h5`, add `HDF5Preview` to `[sot].extensions`, navigate, see structured preview. **Zero Rust changes were required.** That's the test of the ABI.

## Phase 2 seams

Noted but not built:

- **Multi-laptop / multi-client** — `client_id` already in handshake; first-wins is the M1+ default; richer policies later.
- **Multi-agent** — `chat.send` already takes a session id (always `"main"` in Phase 1).
- **Remote transport variants** — protocol is transport-agnostic; backend just listens on a different socket.
- **Types/Math/Outputs modes** — `mode.list` already returns a vector.
- **Background staleness sweep** — `is_stale` is on-demand in M5; future is a kernel timer.
- **Plugin-defined tools** — `core/src/ToolSpec.jl` defined, unwired in M6; Phase 2 adds `kernel.tools.list`.
- **Full LaTeX docs (`tectonic`)** — only inline math via MathJax in M1; document-level TeX comes when a plugin needs it.
- **Option B fallback path** — Tauri/wry wrapping a web frontend. The kernel, backend, line protocol, core IR are unchanged across that swap; only the frontend surface differs. Kept warm in case M1 spike findings demote C.

## Working agreement

- Plan amendments go through PR (this file is committed; argue in the diff).
- Decisions get documented — never in code comments or commit messages alone.
- Working-session handoff state lives outside the public repo in the private ops sidecar.
- Within-session granular tracking via TaskCreate (ephemeral by design).
