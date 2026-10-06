# rust/frontend/src/ui/preview: the preview pane (fe-ui)

The preview pane draws what the kernel sends for the cursored node: markdown and source text shaped by cosmic-text,
PNG and SVG bitmaps as wgpu quads, and the in-pane editor's buffer. Part of fe-ui; charter: rust/frontend/src/ui/CLAUDE.md.

## Files
- `mod.rs`: declares the subfolders and the files below.
- `pane.rs`: which node the pane shows and how a reply's bytes route to a renderer (`render_preview_source`), plus the pin and the path routing for `o`/`W`/`O`.
- `concept.rs`: the concept-annotation slot beside the preview: the `concept.read` request, the frontmatter split and the `file.parse` drift check.
- `fetch.rs`: the cursor-follow `preview.get` (`maybe_fire_preview`), the request-generation counters for the preview and concept slots, and which reply counts (`reply_is_current`).
- `keys.rs`: What a key does in the preview pane, in order: the editor, page turns, the image, then actions and scroll.
- `open.rs`: the external opens of the previewed file: the right outside tool (`open_path_external`), the docs page and the quarto execute.
- `image/`: image previews (PNG decode, SVG rasterization).
- `markdown/`: markdown and source text shaped by cosmic-text, with tree-sitter highlighting.
- `editor/`: in-pane editing.
- `replies.rs`: preview, preview.changed, concept, browser-open and refused-hello replies
- `layout.rs`: the pane's pixel layout for one frame (`State::preview_shows`, `layout_figure`, `layout_markdown`,
  `layout_concept`, `clamp_preview_scroll`).

## Start here
`pane.rs` `render_preview_source` for a new kind of preview; `markdown/mod.rs` for text previews; `image/png.rs` for bitmaps. The shared quad pipeline is in
rust/frontend/src/ui/render/quad.rs.

## Rules
- The quad pipeline lives in ui/render/quad.rs; png.rs and svg.rs name it by `crate::ui::render::quad`.
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
- A preview or concept reply is installed only if its generation is the latest and its host and workspace are still the active ones (`reply_is_current`).
- A figure caption reserves a band at the pane's foot, and every image rect is derived from the reduced rect
  (`layout_figure`, `image_rect_for_caption`).
