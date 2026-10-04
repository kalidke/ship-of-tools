//! Markdown figures: fetch-reply decode, node-id resolution, failure bookkeeping and metrics.

use crate::ui::*;

/// One cached markdown-figure image. `natural_w_px / natural_h_px`
/// are the source bitmap's dimensions (before any preview-pane
/// downscale), used by the walk to size the FFFC placeholder line so
/// the figure paints at its native aspect inside the row. `quad` is
/// the GPU-side texture; cached so navigation doesn't re-upload.
pub(in crate::ui) struct FigureCacheEntry {
    pub(in crate::ui) quad: Quad,
    pub(in crate::ui) natural_w_px: u32,
    pub(in crate::ui) natural_h_px: u32,
}

/// Decode the bytes from a `figure.get` reply into a FigureCacheEntry
/// — a GPU quad sized to the source bitmap's natural dimensions plus
/// the dimensions themselves (so the markdown walk can reserve the
/// right placeholder height on the next reflow). Routes raster mimes
/// through the `image` crate and SVG through resvg's pipeline; any
/// other mime is rejected so an unknown response doesn't silently
/// upload garbage.
pub(in crate::ui) fn decode_figure_bytes(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    pipeline: &crate::ui::render::quad::QuadPipeline,
    mime: &str,
    bytes: &[u8],
) -> anyhow::Result<FigureCacheEntry> {
    if mime == "image/svg+xml" {
        // Rasterise at a reasonable natural size — SVG has no
        // intrinsic pixel dimensions, so we pick something sensible
        // and the paint pass downscales to fit the preview pane.
        // 800×600 is plenty for figures in markdown; tighter than the
        // 2k×2k that resvg would happily render if we let it scale to
        // an arbitrary target.
        let w: u32 = 800;
        let h: u32 = 600;
        let quad = crate::ui::preview::image::svg::quad_from_svg_bytes(device, queue, pipeline, bytes, w, h)?;
        Ok(FigureCacheEntry {
            quad,
            natural_w_px: w,
            natural_h_px: h,
        })
    } else if mime.starts_with("image/") {
        let (quad, w, h) =
            crate::ui::preview::image::png::quad_and_dims_from_bytes(device, queue, pipeline, bytes)?;
        Ok(FigureCacheEntry {
            quad,
            natural_w_px: w,
            natural_h_px: h,
        })
    } else {
        anyhow::bail!("unsupported figure mime: {mime}");
    }
}

/// Resolve a markdown image URL against the current markdown file's
/// node id to produce a files-mode node id the backend can fetch.
///
/// Inputs:
/// - `md_node_id`: `Some("files:examples/preview/foo.md")` when a md
///   file is open, `None` otherwise. We need this to know the
///   directory the relative url is anchored to.
/// - `url`: whatever was inside the `![](…)` parens.
///
/// Returns `None` for URLs we can't fetch via `files:` node ids:
///   - remote schemes (`http://`, `https://`, `file://`, etc.)
///   - URLs that walk out of the project root via `..`
///   - any url when `md_node_id` is missing
///
/// Forward slashes are canonical on the wire — files_mode's
/// node_id_to_path splits on both, but generating slashes here keeps
/// captures comparable across Linux + Windows.
fn resolve_figure_node_id(md_node_id: &Option<String>, url: &str) -> Option<String> {
    if url.is_empty() {
        return None;
    }
    // Remote / data URLs — we don't fetch over network.
    if url.contains("://") || url.starts_with("data:") {
        return None;
    }
    let md = md_node_id.as_ref()?;
    let md_rel = md.strip_prefix("files:")?;
    // Parent dir of the markdown file. `foo.md` → `""`, `docs/foo.md` →
    // `docs`. We split on both separators to stay defensive even though
    // node_ids should already use `/`.
    let parent: String = match md_rel.rsplit_once(['/', '\\']) {
        Some((p, _)) => p.replace('\\', "/"),
        None => String::new(),
    };
    // Absolute (project-rooted) urls like `/figures/foo.png` map to
    // `files:figures/foo.png` — drop the leading slash and treat the
    // remainder as a root-relative path. Otherwise it's relative to the
    // markdown file's parent.
    let combined = if let Some(rest) = url.strip_prefix('/') {
        rest.to_string()
    } else if parent.is_empty() {
        url.to_string()
    } else {
        format!("{parent}/{url}")
    };
    // Normalise `.` / `..` segments. `..` walking past the root rejects
    // — files_mode would reject it on the backend anyway, but failing
    // here saves the round-trip.
    let mut stack: Vec<&str> = Vec::new();
    for seg in combined.split(['/', '\\']) {
        match seg {
            "" | "." => continue,
            ".." => {
                if stack.pop().is_none() {
                    return None;
                }
            }
            other => stack.push(other),
        }
    }
    if stack.is_empty() {
        return None;
    }
    Some(format!("files:{}", stack.join("/")))
}

/// Move `url` out of `pending` and into `failed` — the one terminal state
/// a figure fetch can land in, whether the bytes never arrived at all
/// (`figure.get` error / parse failure) or arrived but wouldn't decode.
/// Shared by both so there's a single place that defines "this figure is
/// done, stop waiting on it."
pub(in crate::ui) fn fail_figure(
    pending: &mut std::collections::HashSet<String>,
    failed: &mut std::collections::HashSet<String>,
    url: String,
) {
    pending.remove(&url);
    failed.insert(url);
}

/// Whether `dispatch_pending_figures` should skip firing a fetch for
/// `url` — already resolved (`cache_hit`), already in flight (`pending`),
/// or terminally failed (`failed`). `failed` membership is the durable
/// half of this check: it holds until the ONE seam that clears it (a
/// fresh `IncomingEvt::Preview` markdown reply) runs, which is what makes
/// a terminal failure actually terminal instead of refiring on every
/// cached-bytes reflow (field report round 2 — see the note in
/// `render_preview_source`).
fn figure_already_handled(
    cache_hit: bool,
    pending: &std::collections::HashSet<String>,
    failed: &std::collections::HashSet<String>,
    url: &str,
) -> bool {
    cache_hit || pending.contains(url) || failed.contains(url)
}

impl State {
    /// Translate `figure_cache` into the per-figure pixel metrics map
    /// the markdown walk consumes. Walk uses these to size each
    /// `![](url)` placeholder's reserved line height so the layout
    /// doesn't reshape when the figure finishes loading. Natural
    /// dimensions are scale-1 pixels — the walk applies `Ctx::scale`
    /// itself.
    pub(in crate::ui) fn build_figure_metrics(&self) -> FigureMetricsMap {
        let mut out = FigureMetricsMap::new();
        for (url, entry) in self.figure_cache.iter() {
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
        for url in self.figure_failed.iter() {
            out.entry(url.clone()).or_insert(FigureMetrics {
                width_px: 0.0,
                height_px: 0.0,
            });
        }
        out
    }

    /// Fire `figure.get` for every `MediaBlock::Figure` in the latest
    /// markdown preview that isn't already cached or in flight.
    /// Resolves the literal URL against the current markdown file's
    /// directory (`preview_node_id_fired`) so a `![](sample.png)` in
    /// `examples/preview/foo.md` lands as
    /// `files:examples/preview/sample.png`. URLs we can't resolve
    /// (remote, absolute, `..`-walking, no current md file) are
    /// silently skipped — better than a request the backend will
    /// reject.
    pub(in crate::ui) fn dispatch_pending_figures(&mut self) {
        let blocks = self.preview_md.media_blocks.clone();
        let md_node_id = self.current_md_node_id.clone();
        let workspace_id = self.current_md_workspace_id.clone();
        for block in blocks {
            let crate::ui::preview::markdown::MediaBlock::Figure { url, .. } = block else {
                continue;
            };
            if figure_already_handled(
                self.figure_cache.contains_key(&url),
                &self.figure_pending,
                &self.figure_failed,
                &url,
            ) {
                continue;
            }
            let Some(node_id) = resolve_figure_node_id(&md_node_id, &url) else {
                // Local-but-unresolvable (walks out of root, no current md
                // file): terminal — mark failed so the layout collapses its
                // reservation instead of holding an empty box forever.
                // (Remote URLs never get this far: the walk renders them as
                // the compact fallback and pushes no MediaBlock.)
                tracing::debug!(%url, "figure unresolvable — collapsing to compact fallback");
                self.figure_failed.insert(url);
                self.needs_md_reflow = true;
                continue;
            };
            if let Err(e) = self.send(crate::net::transport::OutgoingReq::FigureGet {
                url: url.clone(),
                node_id,
                workspace_id: workspace_id.clone(),
            }) {
                tracing::warn!(error = %e, %url, "drop figure.get request — channel closed");
                continue;
            }
            self.figure_pending.insert(url);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- Bug 1: an early `figure.get` failure must reach a terminal state
    // (not warn-and-drop), and a markdown reload must make a previously-
    // failed figure URL retryable — but nothing SHORT of that reload (in
    // particular, a cached-bytes reflow) may clear it, or the failure
    // refires every frame forever (round-2 field report). ---

    #[test]
    fn fail_figure_clears_pending_and_marks_failed() {
        let mut pending: std::collections::HashSet<String> =
            ["figures/x.png".to_string()].into_iter().collect();
        let mut failed: std::collections::HashSet<String> = std::collections::HashSet::new();
        fail_figure(&mut pending, &mut failed, "figures/x.png".to_string());
        assert!(!pending.contains("figures/x.png"));
        assert!(failed.contains("figures/x.png"));
    }

    #[test]
    fn failed_figure_stays_skipped_across_repeated_dispatch_until_seam_clears_it() {
        // This is the storm regression: `dispatch_pending_figures` calls
        // `figure_already_handled` on every walk, including the
        // cached-bytes reflow passes (apply_text_scale, needs_md_reflow,
        // workspace-switch restore) that `render_preview_source` also
        // serves. None of those may un-skip a failed url — only clearing
        // `failed` (which only the fresh IncomingEvt::Preview seam does)
        // may. Simulating N reflow passes with `failed` untouched must
        // keep skipping; only an explicit clear (the seam) reopens it.
        let pending: std::collections::HashSet<String> = std::collections::HashSet::new();
        let mut failed: std::collections::HashSet<String> = std::collections::HashSet::new();
        failed.insert("figures/dead.png".to_string());
        for _ in 0..5 {
            assert!(
                figure_already_handled(false, &pending, &failed, "figures/dead.png"),
                "a terminally-failed url must stay skipped across repeated reflow passes"
            );
        }
        failed.clear(); // the one thing a genuine markdown reload does
        assert!(!figure_already_handled(
            false,
            &pending,
            &failed,
            "figures/dead.png"
        ));
    }
}
