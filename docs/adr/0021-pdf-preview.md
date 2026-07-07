# ADR 0021: PDF preview — poppler rasterization behind a paged `preview.get`

**Status:** Accepted
**Date:** 2026-06-10

## Context

The user's projects are paper-heavy (`example-paper`, `example-project`,
…); PDFs are everywhere in the trees the Files mode browses, and today they fall
through to the bytes-level reader (raw binary → useless pane). Rendering
everything natively is the project's reason to exist, and PDF is the most common
file family we don't render.

Constraints that shaped the design:

1. **Files live where the backend lives** (often a remote host). Rasterizing
   must happen backend-side, like ffmpeg poster extraction (ADR 0018) — the
   frontend only ever receives an image payload.
2. **`pdftoppm` + `pdfinfo` (poppler) are already on the backend hosts**;
   `mutool`/`pdfium` are not. Shelling out to an external tool is established
   precedent (ffmpeg, ADR 0018) — no new install, no Julia/Rust PDF dependency.
3. **PDFs are multi-page**, unlike any preview we render today. The preview
   protocol has no notion of "which part of the file" — that's the actual gap,
   and it's generic (long CSVs, multi-image TIFFs, slide decks would all use it).
4. The plugin ABI is `preview(::Type{<:FileType}, path) :: PreviewPayload`.
   `PreviewPayload.extras` exists in core but has never been carried over the
   wire.

## Decision

### 1. A `PDFFile` plugin, sibling to `VideoFile`

`julia/plugins/pdf-file/` (`ShipToolsPDFFile`, `struct PDFFile <: FileType`),
claiming `.pdf`. `preview` shells `pdfinfo` for the page count and
`pdftoppm -png -r 144 -f N -l N <file> -` for the requested page, returning
`image/png` + `extras = {"page" => N, "page_count" => M}`. Missing poppler or a
failed rasterize returns a `text/markdown` note naming the problem — never a
silent blank pane (house rule: no quiet fallback).

144 DPI: a US-letter page lands ≈ 1224×1584 px — crisp at fit-to-pane and at
moderate zoom on the existing nearest-sampler quad, far under GPU texture caps.
Re-rasterizing at higher DPI on deep zoom is deferred; the hook (a `dpi` param)
falls out of the same params plumbing when wanted.

### 2. ABI: a params-carrying `preview` method with a core fallback

```julia
# core (ConceptExplorerCore)
preview(T::Type{<:FileType}, path, params::AbstractDict) = preview(T, path)
```

The kernel always calls the 3-arg form, passing the request's `params` (e.g.
`{"page": 3}`). Existing 2-arg plugins are untouched — the fallback drops the
params. A paged plugin overrides the 3-arg form. Pure dispatch, no registration;
this is the dogfooding path any future paginated FileType uses.

### 3. Wire: optional `page` up, opaque `extras` down

- `PreviewGetReq` gains `page: Option<u32>` (serde-default; absent = page 1 =
  today's behavior). The backend forwards it to the kernel as
  `file.preview {path, params: {page}}`.
- `PreviewGetRes` gains `extras: Option<serde_json::Value>` — the kernel emits
  `PreviewPayload.extras` when non-empty, the backend forwards it verbatim.
  **Rust stays ignorant of what's inside**; the frontend reads the two keys it
  knows (`page`, `page_count`) and ignores the rest (boundaries are
  serialization seams).
- `is_bounded_output_plugin` (backend input-size gate) adds `pdf`: input size
  doesn't proxy output size — a 50 MiB paper still yields one page-sized PNG.

### 4. Frontend: generic page transport, not a PDF feature

When the current preview's extras carry `page_count > 1` (and the pane isn't in
edit mode), `n`/`p` — and `PgDn`/`PgUp`, the natural reader keys — re-fire
`preview.get` for the same node with `page ± 1` (clamped), and the preview
title shows `name · p N/M`. On non-paginated previews `PgDn`/`PgUp` keep their
text-scroll meaning; the paginated block consumes them first only when page
extras are present (no autorepeat — each turn is a fresh rasterize). The rendered page reuses
the PNG quad wholesale — zoom, pan, reset, the per-`(node_id, dims)` zoom cache
— with zero new rendering code. Cursor moving to a different node resets to
page 1 (the fired-guard already keys per node).

The frontend never learns "PDF": it learns "paginated preview", driven entirely
by extras. A future multi-sheet or multi-frame plugin gets the keys for free.

## Consequences

- **Positive:** papers render inline where they live (remote-safe), with page
  navigation, at ~150 lines of new code across the stack; the long-dormant
  `extras` channel is finally load-bearing, and pagination is a reusable
  protocol capability rather than a PDF one-off.
- **Positive:** zero new dependencies anywhere (poppler already present;
  plugin is stdlib-only Julia).
- **Negative / accepted:** no text layer — no selection, copy, or search inside
  the rendered page (v2: poppler's `pdftotext -bbox` could feed an overlay).
  No open-in-browser `o` route for PDFs yet (trivial follow-up via the ADR 0018
  loopback server if wanted). Each page turn is a fresh `pdftoppm` run
  (~100–300 ms); no page cache until profiling says otherwise.
- **Negative / accepted:** rasterize DPI is fixed at 144 — deep zoom shows
  pixels rather than re-rendering vectors. The params channel makes a
  zoom-triggered re-rasterize a frontend-only change later.

## Alternatives considered

- **`pdfium-render` (Rust, in-process):** best fidelity and a long-term
  text-layer story, but adds a native-library distribution problem to the
  backend build for no v1 benefit, and isn't installed on the hosts. Poppler is
  already there and the shell-out pattern is proven.
- **Rasterize in the Rust backend instead of a Julia plugin:** fewer hops, but
  breaks the dogfooding rule — file-type knowledge belongs in dispatch-table
  plugins, and the kernel already owns the `file.preview` route. The backend's
  only PDF knowledge is the bounded-output extension row (same accepted wart as
  video/HDF5, noted there as a follow-up to make plugin-declared).
- **A dedicated `pdf.page` op:** explicit, but teaches the protocol a file
  type. A generic `page` param on `preview.get` covers every paginated preview
  with one concept.
- **Pre-rasterizing all pages on first open:** simpler navigation, but a
  300-page paper would melt the wire and the kernel for pages never viewed.
  Lazy per-page matches the reactive-over-eager convention.
