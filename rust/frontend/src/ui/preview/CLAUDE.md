# rust/frontend/src/ui/preview: the preview pane (fe-ui)

The preview pane draws what the kernel sends for the cursored node: markdown and source text shaped by cosmic-text,
PNG and SVG bitmaps as wgpu quads, and the in-pane editor's buffer. Part of fe-ui; charter: rust/frontend/src/ui/CLAUDE.md.

## Files
- `mod.rs`: declares the subfolders, `pane`, `concept` and quad, and re-exports `png`, `svg` and `highlight` at their old `preview::` paths.
- `pane.rs`: which node the pane shows and how a reply's bytes route to a renderer (`render_preview_source`), plus the pin and the path routing for `o`/`W`/`O`.
- `concept.rs`: the concept-annotation slot beside the preview: the `concept.read` request, the frontmatter split and the `file.parse` drift check.
- `fetch.rs`: the cursor-follow `preview.get` (`maybe_fire_preview`) and the request-generation counters for the preview and concept slots.
- `open.rs`: the external opens of the previewed file: the right outside tool (`open_path_external`), the docs page and the quarto execute.
- `image/`: image previews (PNG decode, SVG rasterization).
- `markdown/`: markdown and source text shaped by cosmic-text, with tree-sitter highlighting.
- `editor/`: in-pane editing.

## Start here
`pane.rs` `render_preview_source` for a new kind of preview; `markdown/mod.rs` for text previews; `image/png.rs` for bitmaps. The shared quad pipeline is in
rust/frontend/src/ui/render/quad.rs.

## Rules
- The old paths `crate::preview::{markdown, highlight, png, svg, quad}` resolve through this mod.rs's re-exports, because
  main.rs declares `use ui::preview;`.
- quad lives in ui/render/quad.rs; this mod.rs re-exports it (`pub(crate) use crate::ui::render::quad`) so `super::quad` still resolves in png.rs and svg.rs.
- Text over 512 KiB, or bytes that look binary, are summarized, never shaped (`render_preview_source`, `PREVIEW_TEXT_CAP`,
  `looks_binary`).
- Opening the previewed file acts on the installed preview, never on a fired request or a pin (`resolve_previewed_path`).
- A `preview.changed` event counts only when its workspace is the active one or its path is under the known active root
  (`resolve_preview_changed`).
- The concept read fires once per cursored node; a failed drift parse is re-armed after a 2 s then 4 s backoff and
  stops at `FILE_PARSE_MAX_RETRIES` attempts in all (`maybe_fire_concept_read`).
- The scroll clamp measures only the buffer on screen (`preview_scroll_target`).
- A pinned preview suppresses the cursor-follow fetch (`maybe_fire_preview`).
- Every `preview.get` carries a fresh generation, so only the newest reply installs (`next_preview_gen`).
