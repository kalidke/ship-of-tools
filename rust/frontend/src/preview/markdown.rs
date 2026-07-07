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
/// Collapse CRLF / lone-CR line endings to LF before handing text to the
/// shaper. cosmic-text treats a `\r` as its own line break, so a Windows
/// CRLF (`\r\n`) source renders a blank line between *every* real line —
/// the "double-spaced code in module mode" bug, which only showed up for
/// repos checked out with CRLF endings (e.g. RJTrack) while LF repos
/// rendered fine. Display-only normalization: the edit buffer and the
/// clipboard-yank paths read their own bytes elsewhere, so this doesn't
/// touch what gets written or copied. Returns a borrow when there's no
/// CR to strip (the common LF case) to avoid a needless allocation.
fn normalize_newlines(s: &str) -> std::borrow::Cow<'_, str> {
    if s.contains('\r') {
        std::borrow::Cow::Owned(s.replace("\r\n", "\n").replace('\r', "\n"))
    } else {
        std::borrow::Cow::Borrowed(s)
    }
}
/// Given the source split into lines and the 0-indexed line of a definition
/// (`def_idx`), return the 0-indexed line where the item "begins" for preview
/// anchoring: the first line of an *immediately preceding* Julia docstring if
/// one is attached, else `def_idx` itself. Handles multi-line `"""…"""`,
/// single-line `"""…"""`, and single-quoted `"…"` docstrings. Heuristic and
/// deliberately conservative — a blank line above, a comment, or anything
/// ambiguous falls back to `def_idx`, so anchoring never lands somewhere
/// surprising. (Documenter/Base convention: the docstring sits directly above
/// the definition with no blank line between, which is what we key off.)
fn item_anchor_line(lines: &[&str], def_idx: usize) -> usize {
    if def_idx == 0 {
        return 0;
    }
    let above = lines[def_idx - 1].trim();
    if above.is_empty() {
        return def_idx; // blank line between -> no attached docstring
    }
    // Single-line triple-quoted docstring: `"""text"""`
    if above.starts_with("\"\"\"") && above.len() > 3 && above.ends_with("\"\"\"") {
        return def_idx - 1;
    }
    // Multi-line docstring: the line above is its closing `"""`. Walk up to
    // the line that opens it (first line, scanning up, whose trim starts with
    // `"""`).
    if above.ends_with("\"\"\"") {
        let mut i = def_idx - 1;
        while i > 0 {
            i -= 1;
            if lines[i].trim_start().starts_with("\"\"\"") {
                return i;
            }
        }
        return 0;
    }
    // Single-line single-quoted docstring: `"text"`
    if above.len() > 1 && above.starts_with('"') && above.ends_with('"') {
        return def_idx - 1;
    }
    def_idx
}
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
    /// GFM ~~strike~~ — cosmic-text Attrs doesn't expose a real strike
    /// channel, so the Text arm appends U+0336 (combining long stroke
    /// overlay) after every non-space char in any Text descendant of a
    /// Strikethrough node. Approximation, but renders visibly.
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

fn metrics_for_heading(level: u8, scale: f32) -> Metrics {
    match level {
        1 => Metrics::new(24.0 * scale, 30.0 * scale),
        2 => Metrics::new(20.0 * scale, 26.0 * scale),
        3 => Metrics::new(17.0 * scale, 23.0 * scale),
        _ => Metrics::new(BODY_SIZE * scale, BODY_LINE_H * scale),
    }
}

fn attrs_for(ctx: Ctx) -> Attrs<'static> {
    let mut a = Attrs::new();
    a = a.family(if ctx.code || ctx.monospace {
        Family::Monospace
    } else {
        Family::SansSerif
    });
    if ctx.bold || ctx.heading > 0 {
        a = a.weight(Weight::BOLD);
    }
    if ctx.italic {
        a = a.style(Style::Italic);
    }
    if ctx.heading > 0 {
        a = a.metrics(metrics_for_heading(ctx.heading, ctx.scale));
    } else if ctx.code {
        // Code reads ~15% smaller than body — matches GitHub's `font-size:
        // 85%` convention and lets a typical fenced block fit more
        // characters per line before the soft-wrap kicks in. Applies to
        // both inline `<code>` and fenced CodeBlock; the chrome's
        // CODE_BLOCK_FLAG-aware rect math handles the smaller line
        // height automatically since the LayoutRun reports the actual
        // shaped height.
        // Line height tracks the dense CODE_LINE_H ratio (like
        // new_tokens / new_plain), NOT prose BODY_LINE_H: a fenced block
        // read double-spaced because BODY_LINE_H * 0.85 just shrank the
        // airy 1.47 prose ratio instead of tightening it. No effect on
        // inline code — the taller body run drives that paragraph's line
        // box, so only all-code (fenced) lines get denser.
        a = a.metrics(Metrics::new(
            BODY_SIZE * 0.85 * ctx.scale,
            CODE_LINE_H * 0.85 * ctx.scale,
        ));
    } else {
        a = a.metrics(Metrics::new(BODY_SIZE * ctx.scale, BODY_LINE_H * ctx.scale));
    }
    // Tag code + strike spans so the chrome's LayoutRun walk can
    // paint the bg quad behind code and the 1-px line through strike
    // without re-walking the AST. Bitset so both can coexist (e.g.
    // `~~Vec<u8>~~`).
    let mut meta: usize = 0;
    if ctx.code {
        meta |= CODE_GLYPH_FLAG;
        if ctx.code_block {
            meta |= CODE_BLOCK_FLAG;
            meta |= ctx.code_block_id << CODE_BLOCK_ID_SHIFT;
        }
        // Default code colour = VS Code Dark+ neutral fg (#cccccc).
        // The earlier peach tint (206,145,120) was the JuliaSource
        // "string" colour applied uniformly so plain code stood out
        // against the body's near-white; it also made every
        // un-tokenised identifier / operator look like a string when
        // syntax colouring landed. Per-token Julia colouring at the
        // CodeBlock walk still overrides this via `Attrs::color`.
        a = a.color(Color::rgb(204, 204, 204));
    }
    if ctx.strike {
        meta |= STRIKE_GLYPH_FLAG;
    }
    if meta != 0 {
        a = a.metadata(meta);
    }
    a
}


/// Stable per-fence cache key. `DefaultHasher` is `SipHash-1-3`; collision
/// space at u64 is enough for code-block-sized inputs that we're caching
/// (we'd need ~2³² distinct fences to expect one collision, and the
/// frontend would have run out of memory long before then). Matches
/// what `State::markdown_token_cache` keys by.
fn hash_source(s: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    s.hash(&mut h);
    h.finish()
}

/// Merge tree-sitter base spans with backend overlay spans into one
/// non-overlapping sequence of `(start, end, scope)`. Overlay wins
/// inside its own byte range — if a base span overlaps an overlay span,
/// the overlapped portion gets the overlay's scope, the non-overlapped
/// pre/post slivers retain the base scope.
///
/// Both inputs are assumed to be sorted by start and individually
/// non-overlapping (cosmic-text's tree-sitter highlighter guarantees
/// this; the backend's `tokenize_julia_source` sorts at the end).
/// Algorithm:
///   - Walk both lists with a position cursor.
///   - At each position, the active scope is the overlay span containing
///     it (if any), else the base span containing it (if any), else no
///     scope (caller emits default-coloured).
///   - Emit a `(start, end, scope)` whenever the active scope changes.
fn merge_highlight_spans(
    base: &[crate::preview::highlight::HighlightSpan],
    overlay: &[crate::transport::MarkdownToken],
) -> Vec<(usize, usize, String)> {
    if overlay.is_empty() {
        return base
            .iter()
            .map(|s| (s.start, s.end, s.scope.to_string()))
            .collect();
    }
    // Build a list of cut points where the active scope can change.
    let mut cuts: Vec<usize> = Vec::with_capacity((base.len() + overlay.len()) * 2);
    for s in base {
        cuts.push(s.start);
        cuts.push(s.end);
    }
    for s in overlay {
        cuts.push(s.start);
        cuts.push(s.end);
    }
    cuts.sort_unstable();
    cuts.dedup();
    // For each [cuts[i], cuts[i+1]) interval, find the active scope.
    // Overlay first; fall back to base.
    let mut out: Vec<(usize, usize, String)> = Vec::new();
    for w in cuts.windows(2) {
        let (s, e) = (w[0], w[1]);
        if s >= e {
            continue;
        }
        let scope_overlay = overlay
            .iter()
            .find(|o| o.start <= s && e <= o.end)
            .map(|o| o.kind.clone());
        let scope_base = base
            .iter()
            .find(|b| b.start <= s && e <= b.end)
            .map(|b| b.scope.to_string());
        if let Some(scope) = scope_overlay.or(scope_base) {
            // Coalesce with previous if same scope + adjacent.
            if let Some(last) = out.last_mut() {
                if last.1 == s && last.2 == scope {
                    last.1 = e;
                    continue;
                }
            }
            out.push((s, e, scope));
        }
    }
    out
}

fn push_break(out: &mut Vec<(String, Attrs<'static>)>, s: &str, scale: f32) {
    out.push((
        s.to_string(),
        Attrs::new().metrics(Metrics::new(BODY_SIZE * scale, BODY_LINE_H * scale)),
    ));
}

/// Half-height blank line used as external margin around code blocks
/// (and any future "block element with its own panel"). ~10px of vertical
/// breathing room between the slate panel and surrounding prose, matching
/// the GitHub / VS Code "code block has its own breathing room" feel.
/// font_size > 0 because cosmic-text rejects zero metrics; ~0.1px is
/// effectively invisible.
fn push_block_margin(out: &mut Vec<(String, Attrs<'static>)>, scale: f32) {
    out.push((
        "\n".to_string(),
        Attrs::new().metrics(Metrics::new(0.1 * scale, 10.0 * scale)),
    ));
}

// (combining-char strikethrough retired — strike now paints as a real
// 1-px quad in the chrome via `strike_glyph_rects`.)

/// Flatten a node's text content, ignoring inline emphasis nodes. Used
/// by table rendering to measure cell widths before laying out the
/// rendered block — full inline-attr rendering inside table cells stays
/// deferred until the rendering moves off the row-string-padding model.
fn collect_inline_text<'a>(node: &'a AstNode<'a>, out: &mut String) {
    let nv = node.data.borrow().value.clone();
    match nv {
        NodeValue::Text(s) => out.push_str(&s),
        NodeValue::Code(c) => out.push_str(&c.literal),
        NodeValue::SoftBreak | NodeValue::LineBreak => out.push(' '),
        _ => {
            for ch in node.children() {
                collect_inline_text(ch, out);
            }
        }
    }
}

/// Per-em monospace size used for table cells. Matches the 0.85× code
/// metric so tables and code blocks read at the same density — useful
/// because cells often contain code-like identifiers.
const TABLE_FONT_SCALE: f32 = 0.85;

/// Render a GFM table to a box-drawing string + line count. The walk
/// previously pushed the rendered string straight into the main
/// markdown buffer; we now route it through a per-table cosmic-text
/// buffer instead so it can be laid out at *natural* width without
/// soft-wrapping against the preview pane (Windows reported wide
/// tables get the box-drawing mangled by the pane-width wrap).
///
/// Loses inline emphasis (bold, italic, math, links) inside cells —
/// acceptable v1, listed on the TODO under per-glyph rendering.
fn build_table_block<'a>(table_node: &'a AstNode<'a>) -> Option<(String, usize)> {
    let mut rows: Vec<(bool, Vec<String>)> = Vec::new();
    for row_node in table_node.children() {
        if let NodeValue::TableRow(is_header) = &row_node.data.borrow().value {
            let mut cells: Vec<String> = Vec::new();
            for cell_node in row_node.children() {
                if matches!(&cell_node.data.borrow().value, NodeValue::TableCell) {
                    let mut cell_text = String::new();
                    collect_inline_text(cell_node, &mut cell_text);
                    cells.push(cell_text.trim().to_string());
                }
            }
            rows.push((*is_header, cells));
        }
    }
    let ncols = rows.iter().map(|(_, r)| r.len()).max().unwrap_or(0);
    if ncols == 0 {
        return None;
    }
    let mut col_widths: Vec<usize> = vec![0; ncols];
    for (_, row) in &rows {
        for (i, cell) in row.iter().enumerate() {
            if i < ncols {
                col_widths[i] = col_widths[i].max(cell.chars().count());
            }
        }
    }

    let mut block = String::new();
    let mut n_lines: usize = 0;
    // Top border.
    block.push('┌');
    for (i, w) in col_widths.iter().enumerate() {
        for _ in 0..(w + 2) {
            block.push('─');
        }
        block.push(if i + 1 < ncols { '┬' } else { '┐' });
    }
    block.push('\n');
    n_lines += 1;
    for (ridx, (is_header, row)) in rows.iter().enumerate() {
        block.push('│');
        for (i, w) in col_widths.iter().enumerate() {
            let cell = row.get(i).map(|s| s.as_str()).unwrap_or("");
            block.push(' ');
            block.push_str(cell);
            for _ in cell.chars().count()..*w {
                block.push(' ');
            }
            block.push(' ');
            block.push('│');
        }
        block.push('\n');
        n_lines += 1;
        // Header separator (double-line) or row separator (single-line).
        if *is_header && ridx + 1 < rows.len() {
            block.push('├');
            for (i, w) in col_widths.iter().enumerate() {
                for _ in 0..(w + 2) {
                    block.push('═');
                }
                block.push(if i + 1 < ncols { '┼' } else { '┤' });
            }
            block.push('\n');
            n_lines += 1;
        }
    }
    // Bottom border.
    block.push('└');
    for (i, w) in col_widths.iter().enumerate() {
        for _ in 0..(w + 2) {
            block.push('─');
        }
        block.push(if i + 1 < ncols { '┴' } else { '┘' });
    }
    block.push('\n');
    n_lines += 1;
    Some((block, n_lines))
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

fn walk<'a, 'b>(
    node: &'a AstNode<'a>,
    ctx: Ctx,
    out: &mut Vec<(String, Attrs<'static>)>,
    media: &mut Vec<MediaBlock>,
    metrics: &MathMetricsMap,
    figures: &FigureMetricsMap,
    state: &mut WalkState<'b>,
) {
    let nv = node.data.borrow().value.clone();
    match nv {
        NodeValue::Document => {
            for ch in node.children() {
                walk(ch, ctx, out, media, metrics, figures, state);
            }
        }
        NodeValue::Heading(h) => {
            let mut c = ctx;
            c.heading = h.level;
            for ch in node.children() {
                walk(ch, c, out, media, metrics, figures, state);
            }
            push_break(out, "\n\n", ctx.scale);
        }
        NodeValue::Paragraph => {
            for ch in node.children() {
                walk(ch, ctx, out, media, metrics, figures, state);
            }
            // Comrak emits one Paragraph per list-item line even for
            // "tight" lists; double-newline here gives a blank line
            // between every entry. Inside an Item, collapse to a single
            // line break so the list renders compactly.
            push_break(out, if ctx.inside_item { "\n" } else { "\n\n" }, ctx.scale);
        }
        NodeValue::Text(s) => {
            // Strike is rendered as a 1-px overlay quad in the chrome
            // (paint_strike_rects in gpu.rs); the text itself stays
            // untouched so the combining-char fallbacks (U+0335 / U+0336)
            // are gone and the strike sits at a font-metric-correct
            // y-position regardless of which font fontdb picks.
            out.push((s, attrs_for(ctx)));
        }
        NodeValue::Emph => {
            let mut c = ctx;
            c.italic = true;
            for ch in node.children() {
                walk(ch, c, out, media, metrics, figures, state);
            }
        }
        NodeValue::Strong => {
            let mut c = ctx;
            c.bold = true;
            for ch in node.children() {
                walk(ch, c, out, media, metrics, figures, state);
            }
        }
        NodeValue::Strikethrough => {
            let mut c = ctx;
            c.strike = true;
            for ch in node.children() {
                walk(ch, c, out, media, metrics, figures, state);
            }
        }
        NodeValue::Code(code) => {
            let mut c = ctx;
            c.code = true;
            out.push((code.literal, attrs_for(c)));
        }
        NodeValue::CodeBlock(cb) => {
            // Fenced code block — full-width panel via CODE_BLOCK_FLAG
            // on every glyph; chrome expands the rect to the pane
            // width before rendering the slate bg quad. Per-line
            // leading space gives the first character left padding.
            //
            // When the fence info names a language we recognise
            // (currently just `julia` / `jl`), tokenise the literal
            // and emit per-token spans with per-kind colours so the
            // block reads as syntax-highlighted code rather than
            // uniform peach text. Other languages fall through to the
            // default peach tint applied in `attrs_for`.
            state.block_counter = state.block_counter.saturating_add(1);
            state.block_sources.push(cb.literal.clone());
            let mut c = ctx;
            c.code = true;
            c.code_block = true;
            c.code_block_id = state.block_counter;
            push_break(out, "\n", ctx.scale);
            push_block_margin(out, ctx.scale);
            // Normalise the fence info string to its language alias
            // — strips any space-separated tail like `julia title="..."`
            // so the dispatcher only sees the first token. Empty info
            // strings fall through to plain rendering.
            let info_raw = cb.info.trim().to_ascii_lowercase();
            let lang_alias: &str = info_raw
                .split(|c: char| c.is_whitespace())
                .next()
                .unwrap_or("");
            // Re-assemble the block with a leading-space gutter on
            // every line so the panel has breathing room on the left
            // (the bg quad pads on the right at render time).
            let mut padded = String::with_capacity(cb.literal.len() + 8);
            for line in cb.literal.split_inclusive('\n') {
                padded.push(' ');
                padded.push_str(line);
            }
            // Tree-sitter base layer — non-overlapping scope spans in
            // source order. Synchronous + always-available; paints
            // keywords / strings / numbers / comments correctly for
            // any registered language.
            let base_spans = state.highlight.highlight(lang_alias, &padded);
            // Backend semantic-overlay lookup — only Julia today; the
            // overlay wins within its byte range, the base fills the
            // rest. Miss → push the fence into `pending_token_fences`
            // so the caller fires `markdown.tokenize`; the *next*
            // redraw (after the reply) gets the overlay.
            let overlay_key_lang =
                if matches!(lang_alias, "julia" | "jl") { Some("julia") } else { None };
            let overlay_spans: &[crate::transport::MarkdownToken] =
                if let Some(lk) = overlay_key_lang {
                    let h = hash_source(&padded);
                    let key = (lk.to_string(), h);
                    match state.token_cache.get(&key) {
                        Some(v) => v.as_slice(),
                        None => {
                            state.pending_token_fences.push((
                                lk.to_string(),
                                h,
                                padded.clone(),
                            ));
                            &[]
                        }
                    }
                } else {
                    &[]
                };
            let merged = merge_highlight_spans(&base_spans, overlay_spans);
            if merged.is_empty() {
                out.push((padded, attrs_for(c)));
            } else {
                let mut cursor = 0usize;
                for (s, e, scope) in &merged {
                    if *s > cursor {
                        out.push((padded[cursor..*s].to_string(), attrs_for(c)));
                    }
                    let mut a = attrs_for(c);
                    if let Some(col) =
                        crate::preview::highlight::color_for_scope(scope)
                    {
                        a = a.color(col);
                    }
                    out.push((padded[*s..*e].to_string(), a));
                    cursor = *e;
                }
                if cursor < padded.len() {
                    out.push((padded[cursor..].to_string(), attrs_for(c)));
                }
            }
            push_block_margin(out, ctx.scale);
            push_break(out, "\n", ctx.scale);
        }
        NodeValue::List(nl) => {
            let mut c = ctx;
            c.list_depth = ctx.list_depth.saturating_add(1);
            c.list_ordered = nl.list_type == ListType::Ordered;
            // Comrak's `start` is the user-supplied first ordinal (1 for
            // `1. ...`, 7 for `7. ...`). Fall back to 1 for malformed
            // input so we never render `0.` or wrap into usize::MAX.
            let start = nl.start.max(1);
            // Enumerate so each Item knows its 1-based offset from the
            // list's `start`. Mutating per-iteration is fine — `c` is
            // already a local copy.
            for (idx, ch) in node.children().enumerate() {
                c.list_number = start + idx;
                walk(ch, c, out, media, metrics, figures, state);
            }
            push_break(out, "\n", ctx.scale);
        }
        NodeValue::Item(_) => {
            let indent = "  ".repeat(ctx.list_depth.saturating_sub(1) as usize);
            let marker = if ctx.list_ordered {
                format!("{}. ", ctx.list_number)
            } else {
                "• ".to_string()
            };
            out.push((format!("{indent}  {marker}"), attrs_for(ctx)));
            let mut c = ctx;
            c.inside_item = true;
            for ch in node.children() {
                walk(ch, c, out, media, metrics, figures, state);
            }
        }
        NodeValue::TaskItem(checked) => {
            let indent = "  ".repeat(ctx.list_depth.saturating_sub(1) as usize);
            let mark = if checked.is_some() { "☑" } else { "☐" };
            out.push((format!("{indent}  {mark} "), attrs_for(ctx)));
            let mut c = ctx;
            c.inside_item = true;
            for ch in node.children() {
                walk(ch, c, out, media, metrics, figures, state);
            }
        }
        NodeValue::BlockQuote => {
            // VS Code-style left gutter: each level adds `│ `, then the
            // child blocks render normally indented behind it. `│`
            // (U+2502 BOX DRAWINGS LIGHT VERTICAL) chosen over `▎`
            // (U+258E LEFT VERTICAL BLOCK) because the latter renders
            // as tofu in Cascadia/Consolas fallback. Gutter is dimmed
            // so it reads as a quote indicator without competing with
            // the quoted text. Italic applied to the quoted prose.
            let mut c = ctx;
            c.quote_depth = ctx.quote_depth.saturating_add(1);
            c.italic = true;
            for ch in node.children() {
                let gutter: String = (0..c.quote_depth).map(|_| "│ ").collect();
                out.push((
                    gutter,
                    attrs_for(c).color(Color::rgb(102, 102, 102)),
                ));
                walk(ch, c, out, media, metrics, figures, state);
            }
        }
        NodeValue::ThematicBreak => {
            push_break(out, "\n", ctx.scale);
            // 60 box-drawing horizontals — long enough to span the
            // preview pane at typical widths, soft-wrap is acceptable.
            out.push(("─".repeat(60), attrs_for(ctx)));
            push_break(out, "\n\n", ctx.scale);
        }
        NodeValue::Link(link) => {
            // Render link children normally but tag with a visible
            // color so the user sees that text is a link. cosmic-text
            // Attrs supports per-span color; we use the VS Code-Dark+
            // anchor blue. Underline waits on the bg-quad pipeline.
            let _ = link.url;
            for ch in node.children() {
                let mut buf: Vec<(String, Attrs<'static>)> = Vec::new();
                walk(ch, ctx, &mut buf, media, metrics, figures, state);
                for (s, a) in buf {
                    out.push((s, a.color(Color::rgb(59, 142, 234))));
                }
            }
        }
        NodeValue::Table(_) => {
            // Build the box-drawing block at natural width; the chrome
            // will host it in a separate cosmic-text buffer so wide
            // tables don't soft-wrap against the preview pane (that
            // wrap destroys the box-drawing column alignment — the
            // whole reason for Path 1 of (e)).
            let Some((rendered, n_lines)) = build_table_block(node) else {
                return;
            };
            let font_px = BODY_SIZE * TABLE_FONT_SCALE * ctx.scale;
            let line_h_px = BODY_LINE_H * TABLE_FONT_SCALE * ctx.scale;
            let reserved_h = (n_lines as f32) * line_h_px;
            push_break(out, "\n", ctx.scale);
            // FFFC placeholder mirrors the math/figure pattern: zero-
            // alpha colour so the glyph doesn't bleed through, metric
            // override sets the line height to the table's full reserved
            // vertical span so vertical scroll past the table works
            // through the existing `preview_scroll_px` math without
            // chrome-side awareness of table block geometry.
            let placeholder_attrs = Attrs::new()
                .family(Family::SansSerif)
                .color(Color::rgba(0, 0, 0, 0))
                .metrics(Metrics::new(BODY_SIZE * ctx.scale, reserved_h));
            out.push((MATH_PLACEHOLDER.to_string(), placeholder_attrs));
            push_break(out, "\n\n", ctx.scale);
            media.push(MediaBlock::Table {
                rendered,
                n_lines,
                line_h_px,
                font_px,
            });
        }
        NodeValue::SoftBreak => {
            out.push((" ".to_string(), attrs_for(ctx)));
        }
        NodeValue::LineBreak => {
            push_break(out, "\n", ctx.scale);
        }
        NodeValue::Math(m) => {
            let cached = metrics.get(&(m.literal.clone(), m.display_math)).copied();
            if m.display_math {
                // $$...$$ — reserve a placeholder line whose height is
                // the SVG's natural pixel height (once cached) or a
                // conservative default otherwise. The chrome locates
                // FFFC in the layout and paints the pre-rendered SVG
                // centred over it.
                push_break(out, "\n", ctx.scale);
                let line_h = cached
                    .map(|c| c.height_px.max(BODY_LINE_H))
                    .unwrap_or(MATH_BLOCK_H_DEFAULT);
                // Fully transparent color so the OBJECT REPLACEMENT
                // CHARACTER glyph itself never bleeds through next to
                // the SVG we overpaint at this rect.
                let placeholder_attrs = Attrs::new()
                    .family(Family::SansSerif)
                    .color(Color::rgba(0, 0, 0, 0))
                    .metrics(Metrics::new(BODY_SIZE * ctx.scale, line_h * ctx.scale));
                out.push((MATH_PLACEHOLDER.to_string(), placeholder_attrs));
                push_break(out, "\n\n", ctx.scale);
                media.push(MediaBlock::Math {
                    latex: m.literal,
                    display: true,
                });
            } else {
                // Inline `$...$` — emit FFFC (paint anchor, body em so
                // shaping is undisturbed) followed by enough U+2003 EM
                // SPACE characters to reserve the SVG's natural width.
                // Mixing one custom-size em-space caused cosmic-text to
                // give that line a giant ascent (font_size, not
                // line_height, drives ascent) and stack the next line on
                // top of it. So all spacer ems stay at body font_size:
                // total advance is rounded UP to the next body em,
                // worst case ~1 em of trailing whitespace per inline
                // equation — acceptable; tightening further needs a
                // glyph-advance probe of FFFC + body em-space, future
                // work.
                let body_em_px = BODY_SIZE * ctx.scale;
                let line_h_px = BODY_LINE_H * ctx.scale;
                let svg_w = match cached {
                    Some(c) => (c.width_px * ctx.scale).max(1.0),
                    None => body_em_px * INLINE_MATH_FALLBACK_EM_SPACES as f32,
                };
                // Account for FFFC's own ~0.7 em advance so the
                // em-space count doesn't double-reserve. Floor + clamp
                // so a very short equation (e.g. `$x$`) still gets at
                // least one em of spacer between FFFC and following
                // text — looks better than the SVG abutting the next
                // glyph.
                let remaining = (svg_w - body_em_px * 0.7).max(body_em_px);
                let em_spaces = (remaining / body_em_px.max(1.0)).ceil().max(1.0) as usize;
                let placeholder_attrs = Attrs::new()
                    .family(Family::SansSerif)
                    .color(Color::rgba(0, 0, 0, 0))
                    .metrics(Metrics::new(body_em_px, line_h_px));
                let mut s = String::with_capacity(1 + em_spaces * 3);
                s.push(MATH_PLACEHOLDER);
                for _ in 0..em_spaces {
                    s.push('\u{2003}');
                }
                out.push((s, placeholder_attrs));
                media.push(MediaBlock::Math {
                    latex: m.literal,
                    display: false,
                });
            }
        }
        NodeValue::Image(link) => {
            // `![alt](url)` — reserve a block-level placeholder line.
            // The chrome fetches `link.url` (resolved relative to the
            // current markdown file) and paints the decoded bitmap over
            // the FFFC. Children of an Image node are the alt text;
            // we deliberately don't recurse into them so the alt text
            // doesn't show up next to the rendered image. While the
            // bytes are in flight the rect stays blank — the user
            // gets a visible reservation rather than reflowing text
            // when the image lands.
            //
            // Images that will NEVER paint — remote/data URLs (the
            // chrome only fetches workspace-local files) and terminal
            // failures (0-size metrics from the chrome's failed set) —
            // collapse to one compact dim line instead: README banner
            // images and badges otherwise reserve a screenful of
            // permanently empty FIGURE_BLOCK_H_DEFAULT boxes.
            let mut alt = String::new();
            for ch in node.children() {
                if let NodeValue::Text(s) = &ch.data.borrow().value {
                    alt.push_str(s);
                }
            }
            let cached = figures.get(&link.url).copied();
            let never_paints = link.url.contains("://")
                || link.url.starts_with("data:")
                || cached.is_some_and(|c| c.height_px <= 0.0);
            if never_paints {
                let mut label = alt.trim().to_string();
                if label.is_empty() {
                    label = link
                        .url
                        .rsplit('/')
                        .next()
                        .unwrap_or("image")
                        .split('?')
                        .next()
                        .unwrap_or("image")
                        .to_string();
                }
                if label.len() > 60 {
                    label.truncate(60);
                    label.push('…');
                }
                // Single \n breaks (not the figure path's \n\n): a badge
                // row collapses to adjacent one-liners, not a column of
                // double-spaced gaps.
                push_break(out, "\n", ctx.scale);
                out.push((
                    format!("⟦image: {label}⟧"),
                    Attrs::new()
                        .family(Family::SansSerif)
                        .color(Color::rgb(102, 102, 102))
                        .metrics(Metrics::new(
                            BODY_SIZE * ctx.scale,
                            BODY_LINE_H * ctx.scale,
                        )),
                ));
                push_break(out, "\n", ctx.scale);
                // No MediaBlock: the chrome must not pair a paint rect
                // (or fire a fetch) for a figure that can't load —
                // FFFC runs and media_blocks pair by index.
                return;
            }
            push_break(out, "\n", ctx.scale);
            let line_h = cached
                .map(|c| c.height_px.max(BODY_LINE_H))
                .unwrap_or(FIGURE_BLOCK_H_DEFAULT);
            let placeholder_attrs = Attrs::new()
                .family(Family::SansSerif)
                .color(Color::rgba(0, 0, 0, 0))
                .metrics(Metrics::new(BODY_SIZE * ctx.scale, line_h * ctx.scale));
            out.push((MATH_PLACEHOLDER.to_string(), placeholder_attrs));
            push_break(out, "\n\n", ctx.scale);
            media.push(MediaBlock::Figure {
                url: link.url,
                alt,
            });
        }
        _ => {
            for ch in node.children() {
                walk(ch, ctx, out, media, metrics, figures, state);
            }
        }
    }
}

impl MarkdownPreview {
    pub fn new(
        font_system: &mut FontSystem,
        source: &str,
        width: f32,
        height: f32,
        scale: f32,
        math_metrics: &MathMetricsMap,
        figure_metrics: &FigureMetricsMap,
        highlight: &crate::preview::highlight::HighlightService,
        token_cache: &std::collections::HashMap<
            (String, u64),
            Vec<crate::transport::MarkdownToken>,
        >,
    ) -> Self {
        let mut buffer = Buffer::new(
            font_system,
            Metrics::new(BODY_SIZE * scale, BODY_LINE_H * scale),
        );
        // Width bounds wrapping; height is left unbounded (None) so
        // `LayoutRunIter` doesn't stop at the visible-rect height —
        // content past the rect needs to be laid out for scroll to
        // reveal it. Render-time clipping happens via the TextArea's
        // bounds, not the buffer's height.
        let _ = height;
        buffer.set_size(font_system, Some(width.max(1.0)), None);

        let arena = Arena::new();
        let mut opts = Options::default();
        // GFM extensions — match VS Code's markdown-it baseline so
        // GitHub-style markdown round-trips visually. `math_dollars` is
        // a comrak addition not in the GFM spec but standard in
        // Documenter.jl / Julia ecosystems and required for our math
        // pane. `tagfilter` is left off because we don't render raw
        // HTML anyway.
        opts.extension.math_dollars = true;
        opts.extension.table = true;
        opts.extension.strikethrough = true;
        opts.extension.tasklist = true;
        opts.extension.autolink = true;
        opts.extension.footnotes = true;
        // Recognise a leading `---` YAML block as front matter so it's
        // parsed into a (non-rendered) FrontMatter node instead of a setext
        // H2. Required for Quarto `.qmd` (always has a YAML header) and
        // harmless for plain `.md`. The walk's catch-all arm renders nothing
        // for the FrontMatter node, so the header is cleanly skipped.
        opts.extension.front_matter_delimiter = Some("---".to_string());
        let root = parse_document(&arena, source, &opts);

        let mut spans: Vec<(String, Attrs<'static>)> = Vec::new();
        let mut media_blocks: Vec<MediaBlock> = Vec::new();
        let mut walk_state = WalkState {
            block_counter: 0,
            block_sources: Vec::new(),
            highlight,
            token_cache,
            pending_token_fences: Vec::new(),
        };
        let mut root_ctx = Ctx::default();
        root_ctx.scale = scale;
        walk(
            root,
            root_ctx,
            &mut spans,
            &mut media_blocks,
            math_metrics,
            figure_metrics,
            &mut walk_state,
        );

        let default_attrs = Attrs::new()
            .family(Family::SansSerif)
            .metrics(Metrics::new(BODY_SIZE * scale, BODY_LINE_H * scale));
        let span_iter = spans.iter().map(|(s, a)| (s.as_str(), *a));
        buffer.set_rich_text(font_system, span_iter, default_attrs, Shaping::Advanced);
        buffer.shape_until_scroll(font_system, false);

        Self {
            buffer,
            _spans: spans,
            scale,
            body_line_h: BODY_LINE_H,
            media_blocks,
            code_block_sources: walk_state.block_sources,
            pending_token_fences: walk_state.pending_token_fences,
        }
    }

    /// Plain monospace buffer — code, data, log, anything that isn't
    /// markdown. Skips the comrak walk; the whole `source` is one span
    /// in the default sans... wait, monospace family and metrics.
    pub fn new_plain(
        font_system: &mut FontSystem,
        source: &str,
        width: f32,
        scale: f32,
    ) -> Self {
        let metrics = Metrics::new(BODY_SIZE * scale, CODE_LINE_H * scale);
        let mut buffer = Buffer::new(font_system, metrics);
        buffer.set_size(font_system, Some(width.max(1.0)), None);
        let attrs = Attrs::new().family(Family::Monospace).metrics(metrics);
        buffer.set_text(font_system, &normalize_newlines(source), attrs, Shaping::Advanced);
        buffer.shape_until_scroll(font_system, false);
        Self {
            buffer,
            _spans: Vec::new(),
            scale,
            body_line_h: CODE_LINE_H,
            media_blocks: Vec::new(),
            code_block_sources: Vec::new(),
            pending_token_fences: Vec::new(),
        }
    }

    /// Plain monospace buffer like `new_plain`, but each span carries a
    /// `selected` flag — selected spans render in an accent foreground so the
    /// concept editor can show a Shift+motion selection without a cell grid.
    /// Concatenated span text is the rendered string (same composition as
    /// `new_plain`: header, body+cursor, footer).
    pub fn new_plain_spans(
        font_system: &mut FontSystem,
        spans: &[(String, bool)],
        width: f32,
        scale: f32,
    ) -> Self {
        let metrics = Metrics::new(BODY_SIZE * scale, CODE_LINE_H * scale);
        let mut buffer = Buffer::new(font_system, metrics);
        buffer.set_size(font_system, Some(width.max(1.0)), None);
        // Amber selection tint — distinct from the default body colour and
        // from anything the plain-rendered body might contain.
        let sel_color = Color::rgb(0xFF, 0xD7, 0x4A);
        let owned: Vec<(String, Attrs<'static>)> = spans
            .iter()
            .map(|(text, selected)| {
                let mut a = Attrs::new().family(Family::Monospace).metrics(metrics);
                if *selected {
                    a = a.color(sel_color);
                }
                (normalize_newlines(text).into_owned(), a)
            })
            .collect();
        let default_attrs = Attrs::new().family(Family::Monospace).metrics(metrics);
        let iter = owned.iter().map(|(s, a)| (s.as_str(), *a));
        buffer.set_rich_text(font_system, iter, default_attrs, Shaping::Advanced);
        buffer.shape_until_scroll(font_system, false);
        Self {
            buffer,
            _spans: Vec::new(),
            scale,
            body_line_h: CODE_LINE_H,
            media_blocks: Vec::new(),
            code_block_sources: Vec::new(),
            pending_token_fences: Vec::new(),
        }
    }

    /// Tokenised monospace buffer — `.jl` source via JuliaSource's
    /// `application/vnd.sot.tokens+json` mime, one cosmic-text span
    /// per token. Per-kind colours are applied via `Attrs::color`;
    /// "text" / "ident" / unknown kinds fall through to the default
    /// pane colour. Concatenated span text must reproduce the file
    /// (verified live on the dev backend).
    pub fn new_tokens(
        font_system: &mut FontSystem,
        spans: &[(String, String)],
        width: f32,
        scale: f32,
    ) -> Self {
        let metrics = Metrics::new(BODY_SIZE * scale, CODE_LINE_H * scale);
        let mut buffer = Buffer::new(font_system, metrics);
        buffer.set_size(font_system, Some(width.max(1.0)), None);

        let owned: Vec<(String, Attrs<'static>)> = spans
            .iter()
            .map(|(text, kind)| {
                let mut a = Attrs::new().family(Family::Monospace).metrics(metrics);
                if let Some(c) = crate::preview::highlight::color_for_scope(kind) {
                    a = a.color(c);
                }
                (normalize_newlines(text).into_owned(), a)
            })
            .collect();

        let default_attrs = Attrs::new().family(Family::Monospace).metrics(metrics);
        let iter = owned.iter().map(|(s, a)| (s.as_str(), *a));
        buffer.set_rich_text(font_system, iter, default_attrs, Shaping::Advanced);
        buffer.shape_until_scroll(font_system, false);

        Self {
            buffer,
            _spans: owned,
            scale,
            body_line_h: CODE_LINE_H,
            media_blocks: Vec::new(),
            code_block_sources: Vec::new(),
            pending_token_fences: Vec::new(),
        }
    }

    pub fn resize(&mut self, font_system: &mut FontSystem, width: f32, height: f32) {
        // Same reasoning as in `new`: height stays None so content
        // past the visible rect gets laid out and is available for
        // scroll to reveal.
        let _ = height;
        self.buffer
            .set_size(font_system, Some(width.max(1.0)), None);
        self.buffer.shape_until_scroll(font_system, false);
    }

    /// Body line-height in physical pixels — the unit row-based scroll
    /// maths uses. Heading lines are taller in display, but the scroll
    /// step stays body-sized so wheel-rows feel consistent regardless
    /// of which heading is at the top of the viewport.
    pub fn line_height(&self) -> f32 {
        self.body_line_h * self.scale
    }

    /// Body em (font size) in physical pixels. Used by the math-paint
    /// pass to size SVG bitmaps relative to body text: a MathJax SVG
    /// reports `width="N ex"` and the pixel width is `N * ex_factor *
    /// body_em`.
    pub fn body_em(&self) -> f32 {
        BODY_SIZE * self.scale
    }

    /// Scroll offset (in `body_line_h` units — what `preview_scroll` stores)
    /// that puts the item defined at `def_line` (1-indexed source line) at the
    /// top of the pane, anchored to its docstring start when one is attached.
    /// Walks the shaped buffer for the target line's layout run and converts
    /// its pixel top to body-line units. Returns 0 if the line is out of range
    /// or hasn't been laid out; the caller's scroll clamp keeps items near EOF
    /// on-screen. Only meaningful for the code shapers (`new_plain` /
    /// `new_tokens`), where one buffer line == one source line.
    pub fn anchor_scroll_for_def_line(&self, def_line: usize) -> u16 {
        if def_line == 0 {
            return 0;
        }
        let lines: Vec<&str> = self.buffer.lines.iter().map(|l| l.text()).collect();
        if def_line > lines.len() {
            return 0;
        }
        let target = item_anchor_line(&lines, def_line - 1);
        let line_h = self.line_height().max(1.0);
        let top = self
            .buffer
            .layout_runs()
            .find(|r| r.line_i == target)
            .map(|r| r.line_top)
            .unwrap_or(0.0);
        (top / line_h).round().max(0.0) as u16
    }

    /// Total pixel height of the laid-out document, accounting for
    /// per-line `line_height_opt` overrides emitted by tall placeholder
    /// spans (display math, embedded figures). `total_visual_lines`
    /// alone undercounts when figures stretch a single BufferLine to
    /// many body-line heights, leaving `preview_scroll`'s clamp short
    /// of the actual bottom. `body_line_h` is the fallback for any
    /// LayoutLine that didn't carry its own override.
    ///
    /// Unshaped BufferLines contribute exactly `body_line_h` (one body
    /// line, conservative — matches `total_visual_lines`'s 1-line
    /// pessimism). The next shape pass populates them and a follow-up
    /// call yields the precise total.
    pub fn total_visual_pixels(&self, body_line_h: f32) -> f32 {
        self.buffer
            .lines
            .iter()
            .map(|line| match line.layout_opt() {
                Some(layout) if !layout.is_empty() => layout
                    .iter()
                    .map(|ll| ll.line_height_opt.unwrap_or(body_line_h))
                    .sum::<f32>(),
                _ => body_line_h,
            })
            .sum()
    }

    /// Per-contiguous-code-run rects in *buffer-local* coords as
    /// `(x, y, w, h)`. The chrome offsets by the markdown pane origin
    /// and the scroll to get screen rects, then paints a bg quad under
    /// each one before the text layer renders. Glyphs are marked code
    /// via `CODE_GLYPH_META` in their Attrs metadata; consecutive
    /// code-glyphs in the same `LayoutRun` merge into one rect, with
    /// breaks across non-code runs or line boundaries. Inline `<code>`
    /// and fenced `CodeBlock` both flow through this path.
    /// Per-contiguous-strike-run rects in *buffer-local* coords. Same
    /// shape as `code_glyph_rects` but the chrome paints these as a
    /// thin (1–2 px) horizontal quad at the glyph's x-height midline
    /// rather than a full-cell panel. Driven by `STRIKE_GLYPH_FLAG`.
    pub fn strike_glyph_rects(&self) -> Vec<(f32, f32, f32, f32)> {
        let mut rects = Vec::new();
        for run in self.buffer.layout_runs() {
            let top = run.line_top;
            let h = run.line_height;
            let mut current: Option<(f32, f32)> = None;
            for g in run.glyphs.iter() {
                let is_strike = (g.metadata & STRIKE_GLYPH_FLAG) != 0;
                let gx = g.x;
                let gw = g.w.max(0.0);
                if is_strike {
                    let span = current
                        .map(|(s, _)| (s, gx + gw))
                        .unwrap_or((gx, gx + gw));
                    current = Some(span);
                } else if let Some((s, e)) = current.take() {
                    rects.push((s, top, e - s, h));
                }
            }
            if let Some((s, e)) = current.take() {
                rects.push((s, top, e - s, h));
            }
        }
        rects
    }

    /// Per-LayoutRun rects for any glyphs carrying `CODE_BLOCK_FLAG`,
    /// returned in buffer-local coords. One rect per line that
    /// contains any block-code glyph — the chrome expands each to the
    /// full preview-pane width when rendering so the panel reads as a
    /// proper block instead of a text-width pill. `y` is the run's
    /// `line_top`, `h` is the run's `line_height`; `x` and `w` cover
    /// the line's full layout width (rendered range) so the per-line
    /// rect already maps to one painted strip.
    pub fn code_block_line_rects(&self) -> Vec<(f32, f32, f32, f32)> {
        let mut rects = Vec::new();
        for run in self.buffer.layout_runs() {
            let has_block = run
                .glyphs
                .iter()
                .any(|g| (g.metadata & CODE_BLOCK_FLAG) != 0);
            if !has_block {
                continue;
            }
            rects.push((0.0, run.line_top, run.line_w, run.line_height));
        }
        rects
    }

    /// One `(y_top, height)` per fenced code block — covers every
    /// rendered line in the block as a single continuous strip, including
    /// any blank lines inside the fence (which have no glyphs of their
    /// own and would otherwise leave a visible gap in the panel). Grouped
    /// by the block id packed into `Attrs::metadata` above
    /// `CODE_BLOCK_ID_SHIFT`. Chrome paints these at full markdown-pane
    /// width.
    pub fn code_block_rects(&self) -> Vec<(f32, f32)> {
        // (top, bottom) per block_id, in first-seen order.
        let mut order: Vec<usize> = Vec::new();
        let mut bounds: HashMap<usize, (f32, f32)> = HashMap::new();
        for run in self.buffer.layout_runs() {
            let mut block_id: usize = 0;
            for g in run.glyphs.iter() {
                if (g.metadata & CODE_BLOCK_FLAG) != 0 {
                    block_id = g.metadata >> CODE_BLOCK_ID_SHIFT;
                    if block_id != 0 {
                        break;
                    }
                }
            }
            if block_id == 0 {
                continue;
            }
            let top = run.line_top;
            let bot = run.line_top + run.line_height;
            match bounds.get_mut(&block_id) {
                Some(entry) => {
                    if top < entry.0 {
                        entry.0 = top;
                    }
                    if bot > entry.1 {
                        entry.1 = bot;
                    }
                }
                None => {
                    order.push(block_id);
                    bounds.insert(block_id, (top, bot));
                }
            }
        }
        order
            .into_iter()
            .filter_map(|id| bounds.get(&id).copied())
            .map(|(top, bot)| (top, bot - top))
            .collect()
    }

    /// Inline-only code rects — same shape as before but now excludes
    /// fenced-block glyphs (those land in `code_block_line_rects`
    /// instead). Splitting the two lets the chrome render inline as a
    /// text-sized pill and block as a full-pane-width panel.
    pub fn code_glyph_rects(&self) -> Vec<(f32, f32, f32, f32)> {
        let mut rects = Vec::new();
        for run in self.buffer.layout_runs() {
            let top = run.line_top;
            let h = run.line_height;
            // (x_start, x_end) of the in-progress code run; None when
            // we're between code spans.
            let mut current: Option<(f32, f32)> = None;
            for g in run.glyphs.iter() {
                // Inline-only: block code is reported via
                // `code_block_line_rects` instead so the chrome can
                // expand to pane width.
                let is_code = (g.metadata & CODE_GLYPH_FLAG) != 0
                    && (g.metadata & CODE_BLOCK_FLAG) == 0;
                let gx = g.x;
                let gw = g.w.max(0.0);
                if is_code {
                    let span = current
                        .map(|(s, _)| (s, gx + gw))
                        .unwrap_or((gx, gx + gw));
                    current = Some(span);
                } else if let Some((s, e)) = current.take() {
                    rects.push((s, top, e - s, h));
                }
            }
            if let Some((s, e)) = current.take() {
                rects.push((s, top, e - s, h));
            }
        }
        rects
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cosmic_text::FontSystem;

    fn make_preview(source: &str) -> MarkdownPreview {
        make_preview_with_figures(source, FigureMetricsMap::new())
    }

    fn make_preview_with_figures(source: &str, figures: FigureMetricsMap) -> MarkdownPreview {
        let mut fs = FontSystem::new();
        let math: MathMetricsMap = HashMap::new();
        let tokens = HashMap::new();
        let highlight = crate::preview::highlight::HighlightService::new()
            .expect("highlight service init");
        MarkdownPreview::new(&mut fs, source, 800.0, 600.0, 1.0, &math, &figures, &highlight, &tokens)
    }

    fn figure_blocks(p: &MarkdownPreview) -> usize {
        p.media_blocks
            .iter()
            .filter(|b| matches!(b, MediaBlock::Figure { .. }))
            .count()
    }

    fn layout_height(p: &mut MarkdownPreview) -> f32 {
        p.buffer
            .layout_runs()
            .map(|r| r.line_top + r.line_height)
            .fold(0.0_f32, f32::max)
    }

    #[test]
    fn remote_image_collapses_to_compact_line() {
        // README banner case: remote URLs are never fetched by the
        // chrome, so they must not reserve a FIGURE_BLOCK_H_DEFAULT box
        // (one badge row used to push first content nearly off-screen).
        // Compact path: dim text line, NO MediaBlock::Figure.
        let src = "\
![CI](https://img.shields.io/badge/ci-passing-green)
![](https://example.com/banner.png)

First paragraph.
";
        let mut p = make_preview_with_figures(src, FigureMetricsMap::new());
        assert_eq!(figure_blocks(&p), 0, "remote images must not emit Figure blocks");
        let h = layout_height(&mut p);
        assert!(
            h < FIGURE_BLOCK_H_DEFAULT,
            "two remote images + a paragraph should lay out under one \
             figure reservation, got {h}px"
        );
    }

    #[test]
    fn failed_local_image_collapses_to_compact_line() {
        // Terminal failure (decode error / unresolvable path) is
        // reported by the chrome as 0-size metrics; the walk collapses
        // the reservation instead of holding an empty box forever.
        let mut figures = FigureMetricsMap::new();
        figures.insert(
            "missing.png".to_string(),
            FigureMetrics { width_px: 0.0, height_px: 0.0 },
        );
        let p = make_preview_with_figures("![alt text](missing.png)\n", figures);
        assert_eq!(figure_blocks(&p), 0, "failed image must not emit a Figure block");
    }

    #[test]
    fn pending_local_image_keeps_reservation() {
        // In-flight local images keep the visible block reservation so
        // text doesn't reflow when the bytes land (existing behavior).
        let mut p = make_preview_with_figures("![fig](images/plot.png)\n", FigureMetricsMap::new());
        assert_eq!(figure_blocks(&p), 1, "pending local image keeps its Figure block");
        let h = layout_height(&mut p);
        assert!(
            h >= FIGURE_BLOCK_H_DEFAULT,
            "pending image keeps the default reservation, got {h}px"
        );
    }

    #[test]
    fn table_emits_media_block_not_inline_text() {
        // Path 1 of (e): tables route through `MediaBlock::Table` so
        // the chrome can host them in a separate per-table buffer at
        // natural width. If the walk regresses to in-buffer rendering,
        // wide tables will start soft-wrapping again.
        let src = "\
| Col A | Col B |
|-------|-------|
| x     | y     |
";
        let p = make_preview(src);
        let tables: Vec<&MediaBlock> = p
            .media_blocks
            .iter()
            .filter(|b| matches!(b, MediaBlock::Table { .. }))
            .collect();
        assert_eq!(tables.len(), 1, "exactly one table block expected");
        let MediaBlock::Table { rendered, n_lines, line_h_px, font_px } = tables[0] else {
            unreachable!("filter above guarantees Table variant")
        };
        assert!(*line_h_px > 0.0, "line_h_px must be positive");
        assert!(*font_px > 0.0, "font_px must be positive");
        // Rendered block ends with a `\n` per row and includes a
        // bottom border line. For this fixture: top border, header
        // row, separator, body row, bottom border = 5 lines.
        assert_eq!(*n_lines, 5, "rendered line count");
        assert!(rendered.contains("Col A"), "header text preserved");
        assert!(rendered.contains("┌"), "top-left corner present");
        assert!(rendered.contains("└"), "bottom-left corner present");
        assert!(rendered.contains("═"), "header separator (double-line) present");
        assert_eq!(rendered.lines().count(), 5);
    }

    #[test]
    fn empty_table_skipped() {
        // A degenerate `| |` row with no cells parses as a table with
        // zero columns — the walk should drop it instead of producing
        // a zero-row MediaBlock that the chrome would have to special-
        // case in `ensure_table_buffers`.
        let src = "|  |\n|--|\n";
        let p = make_preview(src);
        // Either no MediaBlock at all, or a one-column degenerate
        // table — but never zero columns.
        for b in &p.media_blocks {
            if let MediaBlock::Table { n_lines, .. } = b {
                assert!(*n_lines > 0, "table block with 0 lines slipped through");
            }
        }
    }

    #[test]
    fn multiple_tables_get_distinct_media_blocks() {
        // Each table on a page is its own MediaBlock — chrome maps them
        // to distinct TableBufferEntry slots in encounter order.
        let src = "\
| A |
|---|
| 1 |

Some text in between.

| B |
|---|
| 2 |
";
        let p = make_preview(src);
        let tables: Vec<&MediaBlock> = p
            .media_blocks
            .iter()
            .filter(|b| matches!(b, MediaBlock::Table { .. }))
            .collect();
        assert_eq!(tables.len(), 2, "one MediaBlock per table");
        let MediaBlock::Table { rendered: r1, .. } = tables[0] else { unreachable!() };
        let MediaBlock::Table { rendered: r2, .. } = tables[1] else { unreachable!() };
        assert!(r1.contains("A"));
        assert!(r2.contains("B"));
        // Source order preserved.
        assert!(!r1.contains("B"));
        assert!(!r2.contains("A"));
    }

    #[test]
    fn qmd_front_matter_is_skipped_not_rendered_as_heading() {
        // A Quarto `.qmd` always leads with a YAML header. With
        // front_matter_delimiter enabled it parses to a FrontMatter node the
        // walk ignores, so the header text must NOT appear in the rendered
        // spans (without it, comrak reads `title: ...` + `---` as a setext H2).
        let src = "---\ntitle: My Report\nformat: html\n---\n\n# Section One\n\nBody paragraph.\n";
        let p = make_preview(src);
        let rendered: String = p._spans.iter().map(|(s, _)| s.as_str()).collect();
        assert!(
            !rendered.contains("title:") && !rendered.contains("format:"),
            "front matter leaked into render: {rendered:?}"
        );
        assert!(rendered.contains("Section One"), "body heading missing");
        assert!(rendered.contains("Body paragraph"), "body text missing");
    }

    #[test]
    fn code_previews_are_tighter_than_prose() {
        // The `.jl` token preview (and plain source/log) must use the
        // dense code line-height, not prose's airy `BODY_LINE_H`, or the
        // preview looks double-spaced. `line_height()` feeds the chrome's
        // scroll clamp + paint step, so it must report the same value the
        // buffer was shaped with — assert both code constructors agree.
        let mut fs = FontSystem::new();
        let scale = 1.0;
        let prose = make_preview("hello world");
        let plain = MarkdownPreview::new_plain(&mut fs, "x = 1\ny = 2\n", 800.0, scale);
        let tokens = MarkdownPreview::new_tokens(
            &mut fs,
            &[("x".into(), "variable".into()), (" = 1".into(), "text".into())],
            800.0,
            scale,
        );

        assert_eq!(prose.line_height(), BODY_LINE_H * scale, "prose stays airy");
        assert_eq!(plain.line_height(), CODE_LINE_H * scale, "plain source is dense");
        assert_eq!(tokens.line_height(), CODE_LINE_H * scale, ".jl tokens are dense");
        assert!(
            plain.line_height() < prose.line_height(),
            "code must be tighter than prose ({} !< {})",
            plain.line_height(),
            prose.line_height(),
        );
    }

    #[test]
    fn normalize_newlines_collapses_crlf() {
        use std::borrow::Cow;
        assert_eq!(normalize_newlines("a\r\nb\r\nc").as_ref(), "a\nb\nc");
        assert_eq!(normalize_newlines("a\rb").as_ref(), "a\nb", "lone CR -> LF");
        assert_eq!(normalize_newlines("a\nb").as_ref(), "a\nb", "LF untouched");
        assert!(
            matches!(normalize_newlines("plain text"), Cow::Borrowed(_)),
            "no CR -> borrow, no allocation",
        );
    }

    #[test]
    fn crlf_source_does_not_double_space() {
        // A CRLF (`\r\n`) file must shape to the same number of visual lines
        // as the LF equivalent. cosmic-text treats a stray `\r` as its own
        // line break, so without normalization a CRLF source rendered a
        // blank line between every line (the "double-spaced code" bug, only
        // visible for CRLF-checked-out repos like RJTrack). Guards both code
        // constructors.
        let mut fs = FontSystem::new();
        let src_lf = "fn a() {\n    let x = 1;\n    x\n}\n";
        let src_crlf = "fn a() {\r\n    let x = 1;\r\n    x\r\n}\r\n";

        let lf = MarkdownPreview::new_plain(&mut fs, src_lf, 800.0, 1.0);
        let crlf = MarkdownPreview::new_plain(&mut fs, src_crlf, 800.0, 1.0);
        assert_eq!(
            crlf.buffer.layout_runs().count(),
            lf.buffer.layout_runs().count(),
            "new_plain: CRLF must shape to the same line count as LF",
        );

        // Same source as a single coalesced whitespace-bearing token span,
        // mirroring how JuliaSource emits `\r\n` runs.
        let tok_lf = MarkdownPreview::new_tokens(&mut fs, &[(src_lf.into(), "text".into())], 800.0, 1.0);
        let tok_crlf =
            MarkdownPreview::new_tokens(&mut fs, &[(src_crlf.into(), "text".into())], 800.0, 1.0);
        assert_eq!(
            tok_crlf.buffer.layout_runs().count(),
            tok_lf.buffer.layout_runs().count(),
            "new_tokens: CRLF must shape to the same line count as LF",
        );
    }

    #[test]
    fn item_anchor_line_detects_docstrings() {
        let multi = vec![
            "\"\"\"",        // 0  opening
            "    foo(x)",     // 1
            "",                // 2
            "Description.",    // 3
            "\"\"\"",        // 4  closing
            "function foo(x)", // 5  <- def
        ];
        assert_eq!(item_anchor_line(&multi, 5), 0, "multi-line docstring -> opening");

        let single = vec!["\"\"\"one liner\"\"\"", "bar() = 1"];
        assert_eq!(item_anchor_line(&single, 1), 0, "single-line triple docstring");

        let sq = vec!["\"short\"", "baz() = 2"];
        assert_eq!(item_anchor_line(&sq, 1), 0, "single-quoted docstring");

        let none = vec!["x = 1", "", "qux() = 3"];
        assert_eq!(item_anchor_line(&none, 2), 2, "blank line above -> def line");
        assert_eq!(item_anchor_line(&none, 0), 0, "first line -> itself");

        let comment = vec!["# a comment", "quux() = 4"];
        assert_eq!(item_anchor_line(&comment, 1), 1, "comment above -> def line");
    }

    #[test]
    fn anchor_scroll_for_def_line_maps_to_docstring_top() {
        let mut fs = FontSystem::new();
        // 0 module M | 1 blank | 2 export foo | 3 blank | 4 """ | 5 body |
        // 6 """ | 7 function foo() | 8 end
        let src = "module M\n\nexport foo\n\n\"\"\"\nfoo docstring\n\"\"\"\nfunction foo()\nend\n";
        let p = MarkdownPreview::new_plain(&mut fs, src, 2000.0, 1.0);
        // `function foo()` is source line 8 (1-indexed); its docstring opens at
        // buffer line 4 -> scroll 4 body-lines (no wrapping at 2000px wide).
        assert_eq!(p.anchor_scroll_for_def_line(8), 4);
        // Out of range -> 0 (caller's clamp keeps EOF items on-screen).
        assert_eq!(p.anchor_scroll_for_def_line(999), 0);
    }
}
