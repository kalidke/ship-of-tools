//! The markdown pane's media: math and table caches, the lazy math.render and markdown.tokenize requests,
//! and the placeholder-to-rect pass that places each formula, figure and table.

use crate::ui::*;

/// One per-table cosmic-text buffer hosted as an ExtraArea. `rendered`
/// is the source text the buffer was last built from; on every redraw
/// we compare it against the matching MediaBlock::Table so navigating
/// to a doc with the same table count but different content still
/// rebuilds. `natural_w_px` is the measured widest LayoutRun, used by
/// the scroll clamp to keep the user from scrolling past the table's
/// right edge.
pub(in crate::ui) struct TableBufferEntry {
    rendered: String,
    pub(in crate::ui) buffer: cosmic_text::Buffer,
    pub(in crate::ui) natural_w_px: f32,
}

/// Bottom of the last whole `pitch`-tall row of a buffer whose first row's
/// top is at `top`, clipped at `limit`: a table reaching the pane bottom stops
/// at a row boundary instead of slicing one. `top` (nothing drawn) when not
/// even the first row fits; `limit` for a degenerate pitch.
pub(in crate::ui) fn whole_row_bottom(top: f32, pitch: f32, limit: f32) -> f32 {
    if pitch <= 0.0 {
        return limit;
    }
    top + ((limit - top + 0.5) / pitch).floor().max(0.0) * pitch
}

/// One cached MathJax-rendered math span. SVG bytes survive across
/// re-renders so navigation/scroll doesn't re-roundtrip; the
/// rasterised quad is built lazily on first paint and held in the
/// `rasterised` slot so subsequent frames skip the SVG decode.
pub(in crate::ui) struct MathSvg {
    pub(in crate::ui) svg_bytes: Vec<u8>,
    /// The `ex` pixel value the sidecar told MathJax to use when
    /// emitting this SVG (see `rust/backend/sidecars/mathjax/render.mjs`
    /// — currently hardcoded to 8). Combined with [`MATHJAX_EX_FACTOR`]
    /// it's how we recover the SVG's intended size relative to body
    /// text. Held but not directly consumed; [`width_ex`/`height_ex`]
    /// below carry the per-block geometry.
    #[allow(dead_code)]
    pub(in crate::ui) ex: f32,
    /// SVG root `width="N ex"` parsed at insert time. `None` if the
    /// `<svg>` tag couldn't be parsed (defensive — falls back to the
    /// pre-fix letterbox sizing path).
    pub(in crate::ui) width_ex: Option<f32>,
    /// SVG root `height="N ex"` — same parse path as `width_ex`.
    pub(in crate::ui) height_ex: Option<f32>,
    /// SVG root `style="vertical-align: N ex"`. Negative (the SVG
    /// hangs `N ex` below the text baseline).
    pub(in crate::ui) vertical_align_ex: Option<f32>,
    /// Lazily-rasterised quad. Populated on first paint inside the
    /// markdown pane; reset to None when the cache entry is replaced
    /// (response superseded by a re-render) so the next frame
    /// re-rasterises at the current pixel size.
    pub(in crate::ui) rasterised: Option<Quad>,
}

/// MathJax's `ex_factor` — the ratio of x-height to em-height for its
/// bundled TeX font. SVG dimensions come back in `ex` units; pixel size
/// at body font `F` (px) is `value_ex * MATHJAX_EX_FACTOR * F`.
/// MathJax-src's `CommonOutputJax.OPTIONS` defaults this to 0.5, and
/// that's what we bake in: the sidecar uses MathJax defaults
/// (`em: 16, ex: 8`, exactly 0.5), and any per-body-font override is a
/// future refinement once we measure x-height from fontdb.
pub(in crate::ui) const MATHJAX_EX_FACTOR: f32 = 0.5;

/// Pull `width="N ex"`, `height="N ex"`, and `style="vertical-align: N ex"`
/// out of the SVG's root tag. MathJax-SVG emits these as ASCII float
/// literals with a literal `ex` suffix, so a regex-free scan is enough.
/// Returns `None` for any attribute that isn't present or doesn't parse,
/// leaving the caller to fall back to letterbox sizing.
pub(in crate::ui) fn parse_math_svg_dims(svg_bytes: &[u8]) -> (Option<f32>, Option<f32>, Option<f32>) {
    let Ok(text) = std::str::from_utf8(svg_bytes) else {
        return (None, None, None);
    };
    // Limit the scan to the root opening tag — MathJax SVGs are huge
    // (one path per glyph), and the dims we want are always on the first
    // `<svg ...>` tag.
    let tag = match text.find("<svg") {
        Some(start) => {
            let end = text[start..]
                .find('>')
                .map(|n| start + n + 1)
                .unwrap_or(text.len());
            &text[start..end]
        }
        None => return (None, None, None),
    };
    let parse_ex_attr = |attr: &str| -> Option<f32> {
        // attr is something like `width="`. After the `=` we look for
        // `"`-delimited content ending in `ex`.
        let needle = format!("{attr}=\"");
        let i = tag.find(&needle)?;
        let rest = &tag[i + needle.len()..];
        let j = rest.find('"')?;
        let val = &rest[..j];
        let val = val.trim();
        let val = val.strip_suffix("ex")?.trim();
        val.parse::<f32>().ok()
    };
    let w = parse_ex_attr("width");
    let h = parse_ex_attr("height");
    // vertical-align lives inside the `style="..."` attribute, not as
    // its own attribute. Pull it out separately.
    let v = (|| -> Option<f32> {
        let i = tag.find("style=\"")?;
        let rest = &tag[i + "style=\"".len()..];
        let j = rest.find('"')?;
        let style = &rest[..j];
        let k = style.find("vertical-align:")?;
        let after = &style[k + "vertical-align:".len()..];
        let after = after.trim_start();
        // Strip up to the next `;`, end-of-style, or whitespace.
        let stop = after.find([';', ' ']).unwrap_or(after.len());
        let v = after[..stop].trim();
        let v = v.strip_suffix("ex")?.trim();
        v.parse::<f32>().ok()
    })();
    (w, h, v)
}

impl State {
    /// Translate `math_cache` into the per-block pixel-metrics map the
    /// markdown walk consumes. Each cached SVG's ex-unit dimensions
    /// become unscaled pixels in body-font space:
    /// `value_ex * MATHJAX_EX_FACTOR * MD_BODY_SIZE`. The walk applies
    /// the per-context `scale` itself, so the values stored here are
    /// scale-1 reference numbers — same coordinate system as
    /// `MATH_BLOCK_H_DEFAULT` and friends. Entries without parsed
    /// dimensions are skipped (the walk will see no cache hit and use
    /// its fallback path).
    pub(in crate::ui) fn build_math_metrics(&self) -> MathMetricsMap {
        let mut out = MathMetricsMap::new();
        for (key, entry) in self.math_cache.iter() {
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

    /// Fire `math.render` for every block in the latest markdown
    /// preview that isn't already cached or in flight. Called after
    /// `preview_md` is (re)built. Idempotent — repeated calls with
    /// the same blocks do nothing once the cache is warm.
    pub(in crate::ui) fn dispatch_pending_math(&mut self) {
        let blocks = self.preview_md.media_blocks.clone();
        for block in blocks {
            let crate::ui::preview::markdown::MediaBlock::Math { latex, display } = block else {
                continue;
            };
            let key = (latex.clone(), display);
            if self.math_cache.contains_key(&key) || self.math_pending.contains(&key) {
                continue;
            }
            if let Err(e) = self.send(crate::transport::OutgoingReq::MathRender {
                latex: latex.clone(),
                display,
            }) {
                tracing::warn!(error = %e,
                    "drop math.render request — channel closed");
                continue;
            }
            self.math_pending.insert(key);
        }
    }

    /// Build (or rebuild) the per-table cosmic-text Buffers that host
    /// wide GFM tables at natural width — see `TableBufferEntry`
    /// docstring. Compares the current `media_blocks` Table sources
    /// against the cached `rendered` strings; if any differ (or count
    /// changed), the whole `table_buffers` Vec is rebuilt and the
    /// horizontal scroll resets to 0 so navigation between docs starts
    /// fresh. No-op when buffers are already in sync — typical steady
    /// state across redraws.
    pub(in crate::ui) fn ensure_table_buffers(&mut self) {
        use crate::ui::preview::markdown::MediaBlock;
        // Snapshot the (rendered, font_px, line_h_px) of every Table in
        // current source order. Snapshot avoids the &mut self / &self
        // borrow conflict when we walk media_blocks then build buffers.
        let snapshots: Vec<(String, f32, f32)> = self
            .preview_md
            .media_blocks
            .iter()
            .filter_map(|b| match b {
                MediaBlock::Table {
                    rendered,
                    font_px,
                    line_h_px,
                    ..
                } => Some((rendered.clone(), *font_px, *line_h_px)),
                _ => None,
            })
            .collect();
        let in_sync = snapshots.len() == self.table_buffers.len()
            && snapshots
                .iter()
                .zip(self.table_buffers.iter())
                .all(|((r, _, _), e)| r == &e.rendered);
        if in_sync {
            return;
        }
        self.table_buffers.clear();
        self.md_table_scroll_px = 0.0;
        for (rendered, font_px, line_h_px) in snapshots {
            let metrics = cosmic_text::Metrics::new(font_px, line_h_px);
            let mut buf = cosmic_text::Buffer::new(self.text.font_system_mut(), metrics);
            // 10_000 px is wider than any realistic GFM table so the
            // box-drawing lines lay out without soft-wrap. `None` for
            // height matches the main preview buffer's "extend past
            // the rect" policy.
            buf.set_size(self.text.font_system_mut(), Some(10_000.0), None);
            let attrs = cosmic_text::Attrs::new()
                .family(cosmic_text::Family::Monospace)
                .metrics(metrics);
            buf.set_text(
                self.text.font_system_mut(),
                &rendered,
                attrs,
                cosmic_text::Shaping::Advanced,
            );
            buf.shape_until_scroll(self.text.font_system_mut(), false);
            let mut natural_w_px: f32 = 0.0;
            for run in buf.layout_runs() {
                if run.line_w > natural_w_px {
                    natural_w_px = run.line_w;
                }
            }
            self.table_buffers.push(TableBufferEntry {
                rendered,
                buffer: buf,
                natural_w_px,
            });
        }
    }

    /// Drain `preview_md.pending_token_fences` and fire
    /// `OutgoingReq::MarkdownTokenize` for each `(lang, source_hash)`
    /// that's not already in cache or in flight. Replies route through
    /// `IncomingEvt::MarkdownTokens`, populate the per-fence cache, and
    /// trigger a reflow so the next redraw consumes the overlay.
    pub(in crate::ui) fn dispatch_pending_markdown_tokens(&mut self) {
        let pending = std::mem::take(&mut self.preview_md.pending_token_fences);
        for (lang, source_hash, source) in pending {
            let key = (lang.clone(), source_hash);
            if self.markdown_token_cache.contains_key(&key)
                || self.markdown_token_pending.contains(&key)
            {
                continue;
            }
            if let Err(e) = self.send(crate::transport::OutgoingReq::MarkdownTokenize {
                lang: lang.clone(),
                source_hash,
                source,
            }) {
                tracing::warn!(error = %e,
                    "drop markdown.tokenize request — channel closed");
                continue;
            }
            self.markdown_token_pending.insert(key);
        }
    }

    /// Walk `preview_md.buffer`'s laid-out runs looking for the FFFC
    /// (OBJECT REPLACEMENT CHARACTER) placeholders that
    /// `walk` (ui/preview/markdown/walk.rs) emitted for `$$…$$` / `$…$` / `![](…)`
    /// regions. Returns one entry per FFFC glyph, in source order,
    /// paired with the screen rect to paint into (md_rect-relative
    /// + scroll-adjusted). The order matches `preview_md.media_blocks`
    /// so callers can zip the two by index without re-parsing the
    /// buffer text.
    ///
    /// `preview_scroll_px` is applied so a placeholder that's scrolled
    /// off-screen produces a rect with `y` < `md_rect.y` — caller culls.
    ///
    /// **Takes `md_rect`, NOT `preview_rect`** — and adds
    /// `EXTRA_TOP_PAD_PX` exactly as the text pass does (ui/render/text.rs, `TextLayer::prepare`).
    /// The two passes must share one origin: buffer coordinates are laid
    /// out inside `md_rect` (= preview_rect + pad), so anchoring media at
    /// `preview_rect` painted every SVG `pad_y + EXTRA_TOP_PAD_PX` too
    /// high and `pad_x` too far left. At a typical cell that vertical
    /// error is ~a full body em, which read as inline math being
    /// superscripted. Display math had the identical bug but hid it —
    /// it is centred in a tall reserved row, so the shift was invisible.
    pub(in crate::ui) fn collect_media_paint_targets(
        &self,
        md_rect: ScreenRect,
        preview_scroll_px: f32,
    ) -> Vec<(usize, ScreenRect)> {
        // The text pass shifts glyph origins down by this; media must match.
        let top = md_rect.y + crate::ui::render::text::EXTRA_TOP_PAD_PX;
        use crate::ui::preview::markdown::MediaBlock;
        let mut out = Vec::new();
        if self.preview_md.media_blocks.is_empty() {
            return out;
        }
        let body_em_px = self.preview_md.body_em().max(1.0);
        let mut fffc_idx: usize = 0;
        for run in self.preview_md.buffer.layout_runs() {
            for g in run.glyphs.iter() {
                // The glyph's start/end span the source text byte
                // range it represents; checking the slice avoids
                // having to know cosmic-text's glyph-id mapping for
                // FFFC.
                let end = g.end.min(run.text.len());
                if end <= g.start {
                    continue;
                }
                if &run.text[g.start..end] != "\u{FFFC}" {
                    continue;
                }
                let Some(block) = self.preview_md.media_blocks.get(fffc_idx) else {
                    fffc_idx += 1;
                    continue;
                };
                let rect = match block {
                    MediaBlock::Math { display: true, .. }
                    | MediaBlock::Figure { .. }
                    | MediaBlock::Table { .. } => {
                        // Block-level (display math / figure / table) —
                        // claim the full row the FFFC reserved. The
                        // paint pass centres the bitmap inside it for
                        // math/figure; for tables, the chrome hosts a
                        // separate cosmic-text buffer at natural width
                        // and lets TextBounds clip the overflow to the
                        // preview pane (Path 1 of (e)).
                        ScreenRect {
                            x: md_rect.x,
                            y: top + run.line_top - preview_scroll_px,
                            w: md_rect.w,
                            h: run.line_height,
                        }
                    }
                    MediaBlock::Math {
                        display: false,
                        latex,
                    } => {
                        // Inline: anchor at the FFFC glyph's x position, size
                        // to the cached SVG's natural pixel dimensions, and
                        // baseline-align using MathJax's `vertical-align`
                        // (negative ex → SVG bottom hangs N px below the
                        // text baseline). The cache lookup might miss on the
                        // first paint after a reflow if the SVG was dropped
                        // between then and now — fall back to a single-line
                        // anchor rect so we don't paint at (0, 0).
                        let key = (latex.clone(), false);
                        let entry = self.math_cache.get(&key);
                        let (svg_w_px, svg_h_px, drop_px) = match entry {
                            Some(e) => {
                                let w = e
                                    .width_ex
                                    .map(|v| v * MATHJAX_EX_FACTOR * body_em_px)
                                    .unwrap_or(body_em_px * 2.0);
                                let h = e
                                    .height_ex
                                    .map(|v| v * MATHJAX_EX_FACTOR * body_em_px)
                                    .unwrap_or(run.line_height);
                                let d = e
                                    .vertical_align_ex
                                    .map(|v| (-v) * MATHJAX_EX_FACTOR * body_em_px)
                                    .unwrap_or(0.0);
                                (w, h, d)
                            }
                            None => (body_em_px * 2.0, run.line_height, 0.0),
                        };
                        let baseline_y = top + run.line_y - preview_scroll_px;
                        let svg_bottom_y = baseline_y + drop_px;
                        let svg_top_y = svg_bottom_y - svg_h_px;
                        ScreenRect {
                            x: md_rect.x + g.x,
                            y: svg_top_y,
                            w: svg_w_px,
                            h: svg_h_px,
                        }
                    }
                };
                out.push((fffc_idx, rect));
                fffc_idx += 1;
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn whole_row_bottom_stops_at_the_last_whole_row() {
        assert_eq!(whole_row_bottom(100.0, 20.0, 175.0), 160.0);
        assert_eq!(whole_row_bottom(100.0, 20.0, 180.0), 180.0);
        assert_eq!(whole_row_bottom(100.0, 20.0, 179.6), 180.0);
        assert_eq!(whole_row_bottom(100.0, 20.0, 115.0), 100.0);
        assert_eq!(whole_row_bottom(40.0, 20.0, 175.0), 160.0);
        assert_eq!(whole_row_bottom(100.0, 0.0, 175.0), 175.0);
    }
}
