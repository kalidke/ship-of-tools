//! The markdown figures: fetched bitmaps, requests in flight and terminal failures, keyed by the literal URL.

use std::collections::{HashMap, HashSet};

use super::figures::{fail_figure, figure_already_handled, FigureCacheEntry, FIGURE_SVG_SIZE};
use super::svg_draws::{Done, SvgDraws};
use super::svg_run::{Job, Outcome};
use super::svg_wire::Ask;
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
    /// SVG figures with the drawing worker, and the generation each was submitted under. A URL here is also in
    /// `pending`: it stays there until its outcome settles it.
    drawing: HashMap<String, u64>,
}

/// What a figure reply did: an SVG is with the worker, a raster is for the window to decode now.
pub(in crate::ui) enum Reply {
    /// With the worker: nothing for the window to do until its outcome.
    Submitted,
    /// The worker would not take it, and the figure has failed: the window reflows.
    Failed,
    /// Not an SVG: its URL is settled, and these bytes are for the window to decode and `install` or `fail`.
    Raster { url: String, bytes: Vec<u8> },
}

/// What a drawing outcome did to its figure; the window acts on all but `Ignored`.
pub(in crate::ui) enum Settled {
    /// The key is not a figure drawing, or the outcome is of a generation that is not the one pending.
    Ignored,
    /// Drawn: the pixels for the window to upload and `install`; the URL is pending until it does.
    Drawn { w: u32, h: u32, rgba: Vec<u8> },
    /// Refused: the figure failed, terminally until a fresh markdown reply. The words are for the log.
    Failed(String),
    /// Stale: the URL is unhandled again, so the next walk fetches it under the pane's current generation.
    Unhandled,
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

    pub(in crate::ui) fn install(&mut self, url: String, entry: FigureCacheEntry) {
        self.pending.remove(&url);
        self.cache.insert(url, entry);
    }

    /// Where a `figure.get` reply goes: an SVG is never drawn here but submitted to the worker (`submit_svg`) and its
    /// URL stays pending until the outcome; any other reply settles its URL and comes back for the window to decode.
    pub(in crate::ui) fn route_reply(
        &mut self, draws: &mut SvgDraws, generation: u64, url: String, mime: &str, bytes: Vec<u8>,
    ) -> Reply {
        if mime == "image/svg+xml" {
            return if self.submit_svg(draws, url, generation, bytes) { Reply::Submitted } else { Reply::Failed };
        }
        self.settle(&url);
        Reply::Raster { url, bytes }
    }

    /// The top of a frame: the worker is told the pane's `generation`, and every outcome it has returned settles its
    /// figure; the window acts on each that is not `Ignored`.
    pub(in crate::ui) fn settle_frame(&mut self, draws: &mut SvgDraws, generation: u64) -> Vec<(String, Settled)> {
        draws.advance(generation);
        let mut done = Vec::new();
        draws.drain(|each| done.push(each));
        let mut settled = Vec::new();
        for each in done {
            let url = each.key.clone();
            match self.settle_drawing(each) {
                Settled::Ignored => {}
                other => settled.push((url, other)),
            }
        }
        settled
    }

    /// An SVG reply: one job for the worker keyed by `url`, which stays pending until its outcome. Refused by the
    /// worker (a full queue, no thread), the figure fails at once.
    fn submit_svg(&mut self, draws: &mut SvgDraws, url: String, generation: u64, svg: Vec<u8>) -> bool {
        let job = Job { ask: Ask::Stretch(FIGURE_SVG_SIZE.0, FIGURE_SVG_SIZE.1), svg: svg.into() };
        match draws.submit(url.clone(), generation, job) {
            Ok(()) => {
                self.drawing.insert(url, generation);
                true
            }
            Err(why) => {
                tracing::warn!(%url, why, "figure svg not drawn");
                self.fail(url);
                false
            }
        }
    }

    /// One outcome from the worker. Only the outcome of the generation this URL's job was submitted under counts: a
    /// departed generation's `Stale` can arrive after the new one has submitted the same URL, and must not undo it.
    pub(in crate::ui) fn settle_drawing(&mut self, done: Done) -> Settled {
        if self.drawing.get(&done.key) != Some(&done.generation) {
            return Settled::Ignored;
        }
        self.drawing.remove(&done.key);
        match done.outcome {
            Outcome::Drawn { w, h, rgba } => Settled::Drawn { w, h, rgba },
            Outcome::Refused(why) | Outcome::Ended(why) => {
                self.fail(done.key);
                Settled::Failed(why)
            }
            Outcome::Stale => {
                self.pending.remove(&done.key);
                Settled::Unhandled
            }
        }
    }

    /// The fetch or decode failed terminally.
    pub(in crate::ui) fn fail(&mut self, url: String) { fail_figure(&mut self.pending, &mut self.failed, url); }

    /// A fresh markdown reply is new evidence: its failures are tried again.
    pub(in crate::ui) fn clear_failed(&mut self) { self.failed.clear(); }

    /// The failures a row parks with: they belong to its documents, and a row's restore brings them back.
    pub(in crate::ui) fn park_failed(&self) -> HashSet<String> { self.failed.clone() }
    pub(in crate::ui) fn restore_failed(&mut self, failed: HashSet<String>) { self.failed = failed; }
}
