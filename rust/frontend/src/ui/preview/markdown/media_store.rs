//! The markdown media the walk and the paint read: wide tables' buffers and their shared scroll, rendered math,
//! per-fence token overlays, and the one-shot reflow a fresh cache entry asks for.

use std::collections::{HashMap, HashSet};

use super::media::{MathSvg, TableBufferEntry, MATHJAX_EX_FACTOR};
use super::{MathMetrics, MathMetricsMap, BODY_SIZE as MD_BODY_SIZE};
use crate::net::transport::MarkdownToken;

/// Math and token requests go out only for keys neither cached nor in flight; a reply caches its entry, settles the
/// key and asks for one reflow, which the frame takes once.
#[derive(Default)]
pub(in crate::ui) struct MarkdownMedia {
    /// Horizontal scroll for wide markdown tables. Shared across every
    /// `MediaBlock::Table` on the current preview — Windows's (e) work
    /// item ships Path 1, where each table buffer is at natural width
    /// and we shift it left by this many pixels then let `TextBounds`
    /// clip the overflow to the preview pane. One scroll var keeps the
    /// state model trivially small; in practice a markdown doc only
    /// ever has one wide table on screen at a time. h/l (Preview focus)
    /// step ±~1 cell; Shift+wheel-Y also adjusts. Reset to 0 whenever
    /// the shaped buffer is rebuilt so navigating between docs starts fresh.
    table_scroll_px: f32,
    /// Per-table cosmic-text buffers hosted as ExtraAreas. Indexed in
    /// `media_blocks` Table-encounter order so multiple tables on one
    /// page each get their own slot. `rendered` is the source text the
    /// buffer was built from; on every redraw the chrome compares it
    /// against the current `media_blocks` so navigating to a doc with
    /// the same table count but different content still rebuilds.
    /// Each buffer is laid out at huge width (no soft-wrap) and
    /// `natural_w_px` is the measured max line width — used by the
    /// scroll clamp.
    tables: Vec<TableBufferEntry>,
    /// Cache of MathJax-rendered SVGs keyed by `(latex, display)`.
    /// Populated by `IncomingEvt::MathRendered`; consumed by the
    /// markdown render path to paint quads over FFFC placeholders.
    /// Survives navigation so re-opening a doc with the same math
    /// doesn't re-roundtrip.
    math: HashMap<(String, bool), MathSvg>,
    /// In-flight math.render requests, same key shape as `math`.
    /// Prevents duplicate dispatch when a markdown doc has the same
    /// equation twice or when the user re-loads a partially-fetched
    /// doc before the first replies land.
    math_pending: HashSet<(String, bool)>,
    /// Per-fence semantic-overlay cache keyed by `(lang, source_hash)`.
    /// Populated by `IncomingEvt::MarkdownTokens`; consumed by the
    /// CodeBlock walk to overlay tree-sitter base spans with backend-
    /// derived semantic spans (function-def, call-site, type, etc.).
    /// Survives navigation so re-rendering identical fences in another
    /// doc (Julia ecosystem reuses the same snippets) skips the round
    /// trip.
    tokens: HashMap<(String, u64), Vec<MarkdownToken>>,
    /// In-flight markdown.tokenize requests, same key shape as
    /// `tokens`. Prevents duplicate dispatch when the
    /// same fence appears twice in a doc or the user re-renders before
    /// the reply lands.
    tokens_pending: HashSet<(String, u64)>,
    /// Set when a fresh SVG, figure or token overlay is cached. The redraw loop checks this and rebuilds
    /// the shaped buffer from the installed source so the markdown walk can size
    /// per-block placeholders to the SVG's natural dimensions instead
    /// of the pre-render letterbox default. Coalesces a burst of
    /// MathRendered events into a single rebuild per frame.
    reflow: bool,
}

impl MarkdownMedia {
    pub(in crate::ui) fn tables(&self) -> &[TableBufferEntry] { &self.tables }

    /// Whether the table buffers were built from exactly these sources, in order.
    pub(in crate::ui) fn tables_match(&self, sources: &[(String, f32, f32)]) -> bool {
        sources.len() == self.tables.len()
            && sources.iter().zip(self.tables.iter()).all(|((r, _, _), e)| e.built_from(r))
    }

    /// New table buffers replace the old; the shared scroll returns to the left edge.
    pub(in crate::ui) fn replace_tables(&mut self, tables: Vec<TableBufferEntry>) {
        self.tables = tables;
        self.table_scroll_px = 0.0;
    }

    pub(in crate::ui) fn clear_tables(&mut self) { self.tables.clear(); }

    pub(in crate::ui) fn table_scroll_px(&self) -> f32 { self.table_scroll_px }
    pub(in crate::ui) fn clamp_table_scroll(&mut self, max: f32) {
        self.table_scroll_px = self.table_scroll_px.clamp(0.0, max);
    }
    /// Move by `px`, stopping at the left edge.
    pub(in crate::ui) fn scroll_tables(&mut self, px: f32) { self.table_scroll_px = (self.table_scroll_px + px).max(0.0); }
    pub(in crate::ui) fn scroll_tables_left(&mut self, px: f32) {
        self.table_scroll_px = (self.table_scroll_px - px).max(0.0);
    }
    /// Unclamped here; the next frame clamps to the widest table.
    pub(in crate::ui) fn scroll_tables_right(&mut self, px: f32) { self.table_scroll_px += px; }
    pub(in crate::ui) fn reset_table_scroll(&mut self) { self.table_scroll_px = 0.0; }

    /// The per-block pixel metrics the markdown walk consumes. Each cached SVG's ex-unit dimensions
    /// become unscaled pixels in body-font space:
    /// `value_ex * MATHJAX_EX_FACTOR * MD_BODY_SIZE`. The walk applies
    /// the per-context `scale` itself, so the values stored here are
    /// scale-1 reference numbers — same coordinate system as
    /// `MATH_BLOCK_H_DEFAULT` and friends. Entries without parsed
    /// dimensions are skipped (the walk will see no cache hit and use
    /// its fallback path).
    pub(in crate::ui) fn math_metrics(&self) -> MathMetricsMap {
        let mut out = MathMetricsMap::new();
        for (key, entry) in self.math.iter() {
            let (Some(w_ex), Some(h_ex)) = (entry.width_ex, entry.height_ex) else {
                continue;
            };
            let width_px = w_ex * MATHJAX_EX_FACTOR * MD_BODY_SIZE;
            let height_px = h_ex * MATHJAX_EX_FACTOR * MD_BODY_SIZE;
            // vertical_align_ex is negative when the SVG hangs below
            // the baseline. Drop is the positive distance below; default
            // 0 for display blocks that don't always include the style.
            let baseline_drop_px = entry
                .vertical_align_ex
                .map(|v| (-v) * MATHJAX_EX_FACTOR * MD_BODY_SIZE)
                .unwrap_or(0.0);
            out.insert(
                key.clone(),
                MathMetrics {
                    width_px,
                    height_px,
                    baseline_drop_px,
                },
            );
        }
        out
    }

    pub(in crate::ui) fn math(&self, key: &(String, bool)) -> Option<&MathSvg> { self.math.get(key) }
    pub(in crate::ui) fn math_handled(&self, key: &(String, bool)) -> bool {
        self.math.contains_key(key) || self.math_pending.contains(key)
    }
    pub(in crate::ui) fn mark_math_pending(&mut self, key: (String, bool)) { self.math_pending.insert(key); }

    /// A rendered SVG: cached, its key settled, and a reflow asked so the walk sizes its block from the new dims.
    pub(in crate::ui) fn install_math(&mut self, key: (String, bool), svg: MathSvg) {
        self.math.insert(key.clone(), svg);
        self.math_pending.remove(&key);
        self.reflow = true;
    }

    /// A render that will not come: the key is settled with no reflow, so it cannot loop.
    pub(in crate::ui) fn math_failed(&mut self, key: &(String, bool)) { self.math_pending.remove(key); }

    /// A cached SVG not yet rasterized at the pane's size.
    pub(in crate::ui) fn math_to_rasterise(&self, key: &(String, bool)) -> Option<&MathSvg> {
        self.math.get(key).filter(|entry| entry.rasterised.is_none())
    }

    pub(in crate::ui) fn set_math_raster(&mut self, key: &(String, bool), quad: crate::ui::render::quad::Quad) {
        if let Some(entry) = self.math.get_mut(key) {
            entry.rasterised = Some(quad);
        }
    }

    pub(in crate::ui) fn token_cache(&self) -> &HashMap<(String, u64), Vec<MarkdownToken>> { &self.tokens }
    pub(in crate::ui) fn tokens_handled(&self, key: &(String, u64)) -> bool {
        self.tokens.contains_key(key) || self.tokens_pending.contains(key)
    }
    pub(in crate::ui) fn mark_tokens_pending(&mut self, key: (String, u64)) { self.tokens_pending.insert(key); }

    /// A fence's semantic overlay: cached, its key settled, and a reflow asked.
    pub(in crate::ui) fn install_tokens(&mut self, key: (String, u64), spans: Vec<MarkdownToken>) {
        self.tokens.insert(key.clone(), spans);
        self.tokens_pending.remove(&key);
        self.reflow = true;
    }

    /// A figure landed or failed: the walk must size its reservation again.
    pub(in crate::ui) fn request_reflow(&mut self) { self.reflow = true; }

    /// The frame's one reflow, taken.
    pub(in crate::ui) fn take_reflow(&mut self) -> bool { std::mem::take(&mut self.reflow) }
}
