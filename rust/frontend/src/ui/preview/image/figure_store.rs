//! The markdown figures: fetched bitmaps, requests in flight and terminal failures, keyed by the literal URL.

use std::collections::{HashMap, HashSet};

use super::figures::{fail_figure, figure_already_handled, FigureCacheEntry};
use crate::ui::preview::markdown::{FigureMetrics, FigureMetricsMap};

/// Every figure URL is in at most one of three states: cached, pending or failed. A fetch is sent only for a URL in
/// none; a failure is terminal until a fresh markdown reply clears the failures.
#[derive(Default)]
pub(in crate::ui) struct FigureStore {
    /// Cache of fetched markdown-figure bitmaps keyed by the literal
    /// URL string that appeared in the source (`![](url)`). Populated
    /// by `IncomingEvt::FigureLoaded`; consumed by the markdown render
    /// path to paint quads over the FFFC placeholders the walk
    /// reserves for each figure. Survives navigation so re-opening a
    /// doc with the same figure doesn't re-roundtrip.
    cache: HashMap<String, FigureCacheEntry>,
    /// In-flight figure.get requests, keyed by the same URL string.
    /// Prevents duplicate dispatch when a markdown doc references the
    /// same figure multiple times or when the user re-loads a
    /// partially-fetched doc.
    pending: HashSet<String>,
    /// URLs whose figure fetch/decode terminally failed (bad bytes,
    /// unresolvable local path). Reported to the markdown walk as 0-size
    /// metrics so the layout collapses their reservation to the compact
    /// text fallback instead of holding an empty FIGURE_BLOCK_H_DEFAULT
    /// box forever.
    failed: HashSet<String>,
}

impl FigureStore {
    pub(in crate::ui) fn get(&self, url: &str) -> Option<&FigureCacheEntry> { self.cache.get(url) }

    /// The per-figure pixel metrics the markdown walk consumes. Walk uses these to size each
    /// `![](url)` placeholder's reserved line height so the layout
    /// doesn't reshape when the figure finishes loading. Natural
    /// dimensions are scale-1 pixels — the walk applies `Ctx::scale`
    /// itself.
    pub(in crate::ui) fn metrics(&self) -> FigureMetricsMap {
        let mut out = FigureMetricsMap::new();
        for (url, entry) in self.cache.iter() {
            out.insert(
                url.clone(),
                FigureMetrics {
                    width_px: entry.natural_w_px as f32,
                    height_px: entry.natural_h_px as f32,
                },
            );
        }
        // Terminal failures report as 0-size: the markdown walk reads
        // height_px <= 0 as "will never paint" and collapses the
        // reservation to its compact text fallback.
        for url in self.failed.iter() {
            out.entry(url.clone()).or_insert(FigureMetrics {
                width_px: 0.0,
                height_px: 0.0,
            });
        }
        out
    }

    /// Whether a fetch for `url` is already resolved, in flight or terminally failed.
    pub(in crate::ui) fn handled(&self, url: &str) -> bool {
        figure_already_handled(self.cache.contains_key(url), &self.pending, &self.failed, url)
    }

    pub(in crate::ui) fn mark_pending(&mut self, url: String) { self.pending.insert(url); }

    /// A local URL that resolves to no node: terminal.
    pub(in crate::ui) fn mark_unresolvable(&mut self, url: String) { self.failed.insert(url); }

    /// A reply for `url` arrived; it is no longer in flight.
    pub(in crate::ui) fn settle(&mut self, url: &str) { self.pending.remove(url); }

    pub(in crate::ui) fn install(&mut self, url: String, entry: FigureCacheEntry) { self.cache.insert(url, entry); }

    /// The fetch or decode failed terminally.
    pub(in crate::ui) fn fail(&mut self, url: String) { fail_figure(&mut self.pending, &mut self.failed, url); }

    /// A fresh markdown reply is new evidence: its failures are tried again.
    pub(in crate::ui) fn clear_failed(&mut self) { self.failed.clear(); }

    /// The failures a row parks with: they belong to its documents, and a row's restore brings them back.
    pub(in crate::ui) fn park_failed(&self) -> HashSet<String> { self.failed.clone() }
    pub(in crate::ui) fn restore_failed(&mut self, failed: HashSet<String>) { self.failed = failed; }
}
