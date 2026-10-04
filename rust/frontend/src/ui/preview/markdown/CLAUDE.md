# rust/frontend/src/ui/preview/markdown: markdown and source text (fe-ui)

A comrak syntax tree, or a source file, becomes a cosmic-text buffer of rich spans, with media blocks left as
placeholders for the pane to draw. Part of fe-ui; charter: rust/frontend/src/ui/CLAUDE.md.

## Files
- `mod.rs`: constants, metrics types, `MediaBlock`, `MarkdownPreview`, and the walk's `Ctx` and `WalkState`.
- `spans.rs`: heading metrics, attrs per context, the highlight-overlay merge and block breaks.
- `table.rs`: table blocks, flattened to text and sent as a media block.
- `walk.rs`: `walk`, which turns the comrak tree into spans, media blocks and pending fences.
- `walk_code.rs`: a fenced code block in the walk: tree-sitter spans with the kernel's overlay.
- `walk_media.rs`: tables, math and images in the walk: each reserves a placeholder and records a MediaBlock.
- `buffer.rs`: `MarkdownPreview`'s methods (build, resize, scroll and hit-test math) and newline and anchor helpers.
- `tests.rs`: tests for the walk, media blocks, front matter and anchor lines.
- `media.rs`: math and table caches, the lazy `math.render` and `markdown.tokenize` requests, and the placeholder-to-rect pass.
- `highlight.rs`: tree-sitter highlighting service and scope-to-color mapping.
- `testdata/`: the walk's golden output for a fixture that reaches every arm.

## Start here
`MarkdownPreview` in mod.rs; `walk` in walk.rs for how the tree becomes spans.

## Rules
- Heading sizes are per-span metrics, so one buffer holds several sizes (`attrs_for`, `metrics_for_heading`).
- The walk emits one U+FFFC per media block, in `media_blocks` order (`walk`, `MATH_PLACEHOLDER`).
- A cached token overlay is merged over tree-sitter's base spans (`merge_highlight_spans`).
- After a width change the buffer is re-shaped (`MarkdownPreview::resize`).
- Math and token overlays are requested lazily and keyed by content; an entry cached or in flight is not asked for again
  (`dispatch_pending_math`, `dispatch_pending_markdown_tokens`). Table buffers are rebuilt only when the block's source
  text changed (`ensure_table_buffers`).
- MathJax's ex units become pixels through `MATHJAX_EX_FACTOR` and the markdown body size (`build_math_metrics`).
- Media rects come from the U+FFFC placeholders in `media_blocks` order, anchored at the md rect plus
  `EXTRA_TOP_PAD_PX` as the text pass is (`collect_media_paint_targets`).
