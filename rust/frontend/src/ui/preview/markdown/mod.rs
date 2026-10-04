// preview/markdown.rs — flowed markdown via comrak → cosmic-text rich text.
//
// Per ADR 0011: the preview-layer renders into a ratatui-allocated rect
// without going through the cell stream. Markdown is text-only, so it reuses
// the glyphon stack from text.rs rather than introducing a new pipeline.
//
// The walk turns a comrak AST into a flat list of (text, attrs) spans which
// cosmic-text consumes via `set_rich_text`. Inline math is intentionally left
// to a later step — for now `$...$` arrives as plain text so the spike can
// see what untransformed math looks like in this pane.
//
// Heading sizes are encoded as per-span metric overrides (`Attrs::metrics`)
// rather than baking them into the buffer's default Metrics, so a single
// Buffer can hold multiple text sizes.
//
// Layout uses cosmic-text's wrapping inside the rect width passed to `new` /
// `resize`; redraws after a window resize must call `resize` so the buffer
// re-shapes against the new width.

pub(crate) mod highlight;
mod buffer;
mod prepare;
pub(in crate::ui) mod media;
mod replies;
mod spans;
mod table;
mod walk;
#[cfg(test)]
mod tests;

#[cfg(test)]
use buffer::{item_anchor_line, normalize_newlines};
use spans::{attrs_for, hash_source, merge_highlight_spans, push_block_margin, push_break};
use table::{build_table_block, TABLE_FONT_SCALE};
use walk::walk;

use std::collections::HashMap;

use comrak::{
    nodes::{AstNode, ListType, NodeValue},
    parse_document, Arena, Options,
};
use cosmic_text::{Attrs, Buffer, Color, Family, FontSystem, Metrics, Shaping, Style, Weight};

/// Body font em-size in unscaled pixels. Exposed so the chrome can
/// convert MathJax SVG ex-units into the same unscaled-pixel space the
/// markdown walk uses, before passing them in via `MathMetricsMap`.
pub const BODY_SIZE: f32 = 15.0;
const BODY_LINE_H: f32 = 22.0;
/// Line height for *code* previews (`.jl` token spans, plain source/log
/// text), in unscaled pixels. Prose `BODY_LINE_H` (22px @ 15px font,
/// ratio ~1.47) is intentionally airy for reading; code reads better
/// dense, so we tighten to ~1.27 — close to the chrome's own monospace
/// grid (14/18 ≈ 1.29) — which kills the "double-spaced" look in the
/// `.jl` preview. Tracked by `MarkdownPreview::body_line_h` so the
/// scroll/paint math uses the same value the buffer was shaped with.
const CODE_LINE_H: f32 = 19.0;
/// Fallback vertical space reserved per display-math placeholder, in
/// unscaled pixels, used on the *first* walk before any MathRendered
/// SVG has come back. Generous so the user gets a stable layout while
/// the sidecar is rendering; replaced per-block by the cached natural
/// height on the second walk (triggered by State::needs_md_reflow).
const MATH_BLOCK_H_DEFAULT: f32 = 80.0;
/// Fallback inline-math placeholder width in body em-spaces, applied
/// before the SVG lands so the line doesn't jump when the real SVG
/// width takes its place. Two em-spaces is a reasonable "looks like a
/// short token" stand-in.
const INLINE_MATH_FALLBACK_EM_SPACES: usize = 2;
/// Fallback vertical space reserved per figure placeholder, in unscaled
/// pixels, used on the *first* walk before the figure's image bytes
/// arrive. Sized generously — most figures will be much taller than a
/// math block; the second walk (triggered by `State::needs_md_reflow`)
/// uses the cached natural height once decoded.
const FIGURE_BLOCK_H_DEFAULT: f32 = 200.0;

/// Per-equation pixel dimensions extracted from the MathJax SVG once
/// it arrives, expressed in *unscaled* pixels so the walk can scale
/// them by `Ctx::scale` itself. The chrome (gpu.rs) builds this from
/// `math_cache` before calling `MarkdownPreview::new`; the walk uses
/// it to size display-math line heights and inline-math placeholders.
#[derive(Clone, Copy, Debug)]
pub struct MathMetrics {
    /// Pixel width at scale 1.0.
    pub width_px: f32,
    /// Pixel height at scale 1.0.
    pub height_px: f32,
    /// Pixels the SVG hangs below the text baseline (positive). For
    /// display blocks this is informational; inline blocks use it to
    /// push the paint rect below the baseline so the equation sits on
    /// the line correctly.
    #[allow(dead_code)]
    pub baseline_drop_px: f32,
}

pub type MathMetricsMap = HashMap<(String, bool), MathMetrics>;

/// Per-figure natural pixel dimensions, populated by the chrome once
/// the image bytes have been fetched + decoded. Keyed by the literal
/// URL string the markdown source contained (we don't resolve
/// relative paths inside this module — the chrome does that against
/// the current markdown file's directory).
#[derive(Clone, Copy, Debug)]
pub struct FigureMetrics {
    /// Held for future "reserve horizontal space too" passes (e.g. an
    /// `align="right"` flow). The walk only consults height today,
    /// since the row reservation always claims the full preview width.
    #[allow(dead_code)]
    pub width_px: f32,
    pub height_px: f32,
}

pub type FigureMetricsMap = HashMap<String, FigureMetrics>;
/// The OBJECT REPLACEMENT CHARACTER cosmic-text uses as a placeholder
/// glyph for display-math regions. After layout we walk LayoutRuns
/// looking for this codepoint to find each math placeholder's
/// on-screen rect, then paint the cached SVG over it. Inline `$...$`
/// regions don't use this — they stay as raw text until A4.
pub const MATH_PLACEHOLDER: char = '\u{FFFC}';

/// Per-glyph metadata flags routed through cosmic-text's `Attrs.metadata`
/// channel. Bitset so a span can be both code and struck-through if a
/// fixture ever combines them. Chrome reads the bits in `code_glyph_rects`
/// / `strike_glyph_rects` to paint the slate code panel and the strike
/// line quad respectively, without re-walking the AST.
pub const CODE_GLYPH_FLAG: usize = 0x01;
pub const STRIKE_GLYPH_FLAG: usize = 0x02;
/// Set in addition to `CODE_GLYPH_FLAG` for fenced `<pre><code>` blocks.
/// Inline `<code>` carries only `CODE_GLYPH_FLAG`. The chrome uses this
/// distinction to render block code as a full-pane-width panel (like
/// GitHub / VS Code) while inline code stays a text-sized pill.
pub const CODE_BLOCK_FLAG: usize = 0x04;
/// Bits used for the flag bitset above; everything at or above
/// `CODE_BLOCK_ID_SHIFT` carries a 1-based fenced-block identifier so
/// `code_block_rects()` can stitch multiple per-line layout runs (and
/// the empty runs of blank lines inside the fence) into one continuous
/// panel. Without an id, two adjacent code blocks merge and a blank
/// line inside a single block visually splits the panel.
pub const CODE_BLOCK_ID_SHIFT: usize = 16;

/// One embedded-media region the chrome must paint over a FFFC
/// placeholder. Ordered by appearance in the source so the chrome can
/// zip these with the FFFC glyphs found in the cosmic-text LayoutRuns
/// — math, figures, and tables share one ordered list because the
/// placeholder codepoint is the same for all, so source-order is the
/// only way to recover the kind at paint time.
#[derive(Debug, Clone)]
pub enum MediaBlock {
    /// `$…$` (inline) or `$$…$$` (display) latex routed through the
    /// MathJax sidecar. `display=false` means inline placement; the
    /// walk reserves x-advance for the SVG's natural width.
    Math { latex: String, display: bool },
    /// `![alt](url)` — a markdown image. `url` is whatever appeared
    /// inside the parens (relative path, absolute path, or remote URL);
    /// the chrome resolves it against the current markdown file's
    /// directory when firing the fetch. `alt` is collected from the
    /// node's child Text spans for a future hover-tooltip / `o`-open
    /// fallback; the current paint pass doesn't render it.
    Figure {
        url: String,
        #[allow(dead_code)]
        alt: String,
    },
    /// GFM table — rendered as a monospace box-drawing block in a
    /// *separate* cosmic-text buffer at natural width so the box-drawing
    /// rows aren't soft-wrapped against the preview pane. The chrome
    /// builds the per-table Buffer lazily, paints it as an ExtraArea
    /// shifted by the per-document horizontal scroll, and lets
    /// `TextBounds` clip the overflow to the preview pane. Reserved
    /// space in the main buffer is one FFFC glyph with `line_height =
    /// n_lines * line_h_px`, so wheel scroll past the table works
    /// without the chrome needing to know the table's natural width.
    Table {
        /// Box-drawing block (top border, rows + separators, bottom
        /// border, each terminated by `\n`). Verbatim from the walk;
        /// the chrome feeds this into its per-table Buffer unchanged.
        rendered: String,
        /// Number of `\n`-separated lines in `rendered`, so the chrome
        /// can sanity-check its laid-out row count.
        #[allow(dead_code)]
        n_lines: usize,
        /// Per-line height (scaled px) the walk reserved on the main
        /// buffer's FFFC. The per-table Buffer is built with the same
        /// metrics so the rendered block fits exactly inside the
        /// reservation.
        line_h_px: f32,
        /// Per-em monospace font size (scaled px). Same metric pair the
        /// FFFC reservation uses; chrome configures its per-table
        /// Buffer with `Metrics::new(font_px, line_h_px)`.
        font_px: f32,
    },
}

pub struct MarkdownPreview {
    pub buffer: Buffer,
    /// Owned span storage — the Buffer borrows nothing from this Vec after
    /// `set_rich_text`, but holding it keeps the fields next to the buffer
    /// for any future re-shape.
    _spans: Vec<(String, Attrs<'static>)>,
    /// Raw fenced-block sources in source order. Populated by the walk
    /// from each `NodeValue::CodeBlock.literal`; the chrome's `y` keystroke
    /// copies these to the clipboard verbatim. Empty for `new_plain` /
    /// `new_tokens` buffers.
    pub code_block_sources: Vec<String>,
    /// Saved scale so callers can derive a single body-line height for
    /// row-based scrolling math (`scroll N rows == N * line_height` px).
    scale: f32,
    /// Unscaled body line-height the buffer was shaped with: `BODY_LINE_H`
    /// for prose (`new`), `CODE_LINE_H` for code (`new_plain` /
    /// `new_tokens`). `line_height()` returns this × `scale` so the
    /// chrome's scroll clamp and paint loop use the same step the glyphs
    /// were actually laid out at — feeding the prose 22px into a 19px
    /// code buffer would drift the scrollbar and clip the last lines.
    body_line_h: f32,
    /// Embedded-media regions discovered during the walk, in source
    /// order. Empty for `new_plain` / `new_tokens` buffers (markdown
    /// parser is the only producer). The chrome consumes this to
    /// fire `math.render` (or `preview.get` for figures) once per
    /// distinct key, and to paint cached bitmaps over the FFFC
    /// placeholders the walk emitted.
    pub media_blocks: Vec<MediaBlock>,
    /// Fences that were walked but not yet covered by the
    /// `markdown.tokenize` cache. Each entry is `(lang, source_hash,
    /// padded_source)` — caller (gpu.rs) drains this after construction
    /// to fire `OutgoingReq::MarkdownTokenize` for any not already
    /// in-flight. Empty in the cache-hit path (everything came from
    /// the overlay).
    pub pending_token_fences: Vec<(String, u64, String)>,
}

#[derive(Clone, Copy, Default)]
struct Ctx {
    bold: bool,
    italic: bool,
    code: bool,
    /// True when the span should render in the monospace family but
    /// must NOT receive the slate code-bg quad. Used by the GFM table
    /// renderer so its box-drawing borders align without lighting up
    /// every cell as inline code. `code = true` always implies
    /// monospace; this flag adds monospace without the code styling.
    monospace: bool,
    /// True for fenced `<pre><code>` blocks; set in addition to `code`.
    /// Inline `<code>` keeps `code_block = false`. Drives the
    /// `CODE_BLOCK_FLAG` metadata bit on glyphs so the chrome can paint
    /// the block as a full-width panel.
    code_block: bool,
    /// 1-based id of the enclosing fenced block, packed into the high
    /// bits of `Attrs::metadata` so the chrome can group per-line rects
    /// back into per-block panels. Zero means "not inside a block".
    code_block_id: usize,
    strike: bool,
    /// List-nesting depth — incremented by each List ancestor so nested
    /// Items render with leading whitespace proportional to depth.
    /// VS Code's markdown preview indents nested bullets ~2 char-widths
    /// per level; we match that.
    list_depth: u8,
    /// True when the immediate enclosing list is ordered (`1. 2. 3.`),
    /// false for bullet lists. Drives Item marker selection.
    list_ordered: bool,
    /// The number to render for the current Item when `list_ordered`.
    /// Set by the List arm as it enumerates children so each Item knows
    /// its 1-based ordinal relative to the parent list's `start`.
    list_number: usize,
    /// True while walking inside a list Item or TaskItem subtree, so a
    /// child Paragraph emits a single `\n` instead of `\n\n` (otherwise
    /// loose lists render with a blank line between every entry).
    inside_item: bool,
    /// Blockquote-nesting depth — incremented by each BlockQuote ancestor
    /// so quoted paragraphs render with a `▎ ` (LEFT VERTICAL BLOCK) gutter
    /// per level, matching VS Code's left-border style.
    quote_depth: u8,
    /// 0 = body, 1..=6 = heading level.
    heading: u8,
    /// Multiplier applied to every Metrics so the preview tracks the same
    /// scale as chrome (window DPR + `--scale`).
    scale: f32,
}
struct WalkState<'a> {
    /// Monotonic 1-based counter incremented on every fenced `CodeBlock`
    /// visited; the value becomes the block's `code_block_id` so the
    /// chrome can stitch per-line rects back into per-block panels.
    block_counter: usize,
    /// Raw fenced-block sources (literal text from comrak's `cb.literal`)
    /// in walk order. Used by the `y`-to-copy keystroke so the user gets
    /// the unmodified source — no leading-space gutter, no tokenizer
    /// rewriting — on the clipboard.
    block_sources: Vec<String>,
    /// Tree-sitter-backed syntax highlighter shared across the whole
    /// walk. Only used by the `NodeValue::CodeBlock` arm today; future
    /// inline-code highlighting can read from the same handle. Borrowed
    /// from `State::highlight_service`.
    highlight: &'a crate::preview::highlight::HighlightService,
    /// Per-fence semantic-overlay cache borrowed from
    /// `State::markdown_token_cache`. Keyed by `(lang, source_hash)`;
    /// when a fence hits, the walk overlays the backend's spans on top
    /// of tree-sitter's base. Miss → caller is asked (via
    /// `pending_token_fences`) to fire the round-trip.
    token_cache:
        &'a std::collections::HashMap<(String, u64), Vec<crate::transport::MarkdownToken>>,
    /// Drained by the caller after the walk to dispatch
    /// `OutgoingReq::MarkdownTokenize` for any fence that missed the
    /// cache. Each entry is `(lang, source_hash, padded_source)`.
    pending_token_fences: Vec<(String, u64, String)>,
}
