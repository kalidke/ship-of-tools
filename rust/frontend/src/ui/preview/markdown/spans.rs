//! Span building for the walk: heading metrics, attrs per context, highlight-overlay merge and block breaks.

use super::*;

fn metrics_for_heading(level: u8, scale: f32) -> Metrics {
    match level {
        1 => Metrics::new(24.0 * scale, 30.0 * scale),
        2 => Metrics::new(20.0 * scale, 26.0 * scale),
        3 => Metrics::new(17.0 * scale, 23.0 * scale),
        _ => Metrics::new(BODY_SIZE * scale, BODY_LINE_H * scale),
    }
}

pub(super) fn attrs_for(ctx: Ctx) -> Attrs<'static> {
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
pub(super) fn hash_source(s: &str) -> u64 {
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
pub(super) fn merge_highlight_spans(
    base: &[crate::ui::preview::markdown::highlight::HighlightSpan],
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

pub(super) fn push_break(out: &mut Vec<(String, Attrs<'static>)>, s: &str, scale: f32) {
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
pub(super) fn push_block_margin(out: &mut Vec<(String, Attrs<'static>)>, scale: f32) {
    out.push((
        "\n".to_string(),
        Attrs::new().metrics(Metrics::new(0.1 * scale, 10.0 * scale)),
    ));
}
