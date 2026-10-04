//! Image overlays: the figure caption store and the scalebar and caption draw geometry.

use crate::ui::*;
use super::view::png_zoom_max;

/// Hard cap on a stored figure caption, in CHARACTERS (not bytes — truncating
/// UTF-8 by byte offset panics mid-codepoint). A caption names what a figure is;
/// past a couple of lines it stops being a caption and starts covering the image
/// it describes. The CLI warns and truncates at the same limit; this is the
/// backstop for every other route onto the wire.
pub(in crate::ui) const CAPTION_MAX_CHARS: usize = 300;

/// Cap a caption at [`CAPTION_MAX_CHARS`], marking a truncation with an ellipsis
/// so a cut-off caption reads as cut off rather than as a complete sentence that
/// happens to end oddly.
pub(in crate::ui) fn truncate_caption(s: &str) -> String {
    if s.chars().count() <= CAPTION_MAX_CHARS {
        return s.to_string();
    }
    let mut out: String = s.chars().take(CAPTION_MAX_CHARS - 1).collect();
    out.push('…');
    out
}

/// How many (workspace, file) captions to retain. Captions are small and
/// arrive one per agent badge, but the FE runs for days across many workspaces
/// — an unbounded map is a slow leak. FIFO eviction: the oldest badge is the
/// one whose figure the user is least likely to still be looking at.
const CAPTION_STORE_CAP: usize = 256;

/// Sticky per-(workspace, file) figure captions, with bounded FIFO eviction.
/// Keyed by the SAME normalized workspace key the render path looks up with
/// (`ws_key_of`), so a caption addressed to the default workspace by slug and
/// one addressed to it as "default" land in one slot instead of two.
///
/// Keying by workspace (rather than storing "the current caption" on `State`)
/// is what makes the cross-workspace badge work AND makes the ADR-0034 F2
/// hazard — workspace A's annotation rendered over workspace B's image —
/// unrepresentable: a lookup can only ever return the active workspace's
/// caption for the file actually on screen.
#[derive(Default)]
pub(in crate::ui) struct CaptionStore {
    map: HashMap<(String, String), String>,
    order: std::collections::VecDeque<(String, String)>,
}

impl CaptionStore {
    pub(in crate::ui) fn set(&mut self, ws_key: String, node_id: String, text: String) {
        let key = (ws_key, node_id);
        if self.map.insert(key.clone(), text).is_none() {
            self.order.push_back(key);
        }
        while self.order.len() > CAPTION_STORE_CAP {
            if let Some(old) = self.order.pop_front() {
                self.map.remove(&old);
            }
        }
    }

    pub(in crate::ui) fn clear_one(&mut self, ws_key: &str, node_id: &str) {
        let key = (ws_key.to_string(), node_id.to_string());
        if self.map.remove(&key).is_some() {
            self.order.retain(|k| k != &key);
        }
    }

    pub(in crate::ui) fn get(&self, ws_key: &str, node_id: &str) -> Option<&String> {
        self.map.get(&(ws_key.to_string(), node_id.to_string()))
    }
}

/// One axis of a raster's physical scale (ADR 0034). `per_px` is the physical
/// length of one *source* pixel, in the payload's `unit`. `name` is the axis
/// label (`"x"`, `"z"`, …) so an anisotropic (XZ) view labels each bar.
#[derive(Debug, Clone, PartialEq)]
struct ScaleAxis {
    name: String,
    per_px: f64,
}

/// A raster preview's physical scale, from `extras.physical_scale` (ADR 0034).
/// `axes[0]` is the horizontal (x) image axis. Isotropic sources ship two
/// equal axes; Phase 1 renders one bar from `axes[0]`.
#[derive(Debug, Clone, PartialEq)]
pub(in crate::ui) struct PhysicalScale {
    axes: Vec<ScaleAxis>,
    unit: String,
}

/// Parse `extras.physical_scale` into a [`PhysicalScale`]. Shape (ADR 0034 §2):
/// `{"axes":[{"name","nm_per_px"}],"unit"}`. `None` for any reply without the
/// key (so it clears like `preview_page`) or a malformed/empty axes array.
pub(in crate::ui) fn parse_physical_scale(extras: &serde_json::Value) -> Option<PhysicalScale> {
    let ps = extras.get("physical_scale")?;
    let unit = ps
        .get("unit")
        .and_then(|v| v.as_str())
        .unwrap_or("nm")
        .to_string();
    let axes_v = ps.get("axes")?.as_array()?;
    let mut axes = Vec::with_capacity(axes_v.len());
    for a in axes_v {
        let per_px = a.get("nm_per_px").and_then(|v| v.as_f64())?;
        let name = a
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        axes.push(ScaleAxis { name, per_px });
    }
    if axes.is_empty() {
        return None;
    }
    Some(PhysicalScale { axes, unit })
}

/// Snap a positive length to a "nice" `1/2/5 × 10ⁿ` value — the map-scalebar
/// convention (ADR 0034 §3). Returns `1.0` for non-finite / non-positive input.
fn snap_1_2_5(x: f64) -> f64 {
    if !x.is_finite() || x <= 0.0 {
        return 1.0;
    }
    let base = 10f64.powf(x.log10().floor());
    let f = x / base; // in [1, 10)
    let nice = if f < 1.5 {
        1.0
    } else if f < 3.5 {
        2.0
    } else if f < 7.5 {
        5.0
    } else {
        10.0
    };
    nice * base
}

/// Format a snapped scale value for the label: integer when whole, else a
/// trimmed decimal (`500`, `2.5`), so "500 nm" / "2.5 µm" read cleanly.
fn fmt_scale_value(v: f64) -> String {
    if (v - v.round()).abs() < 1e-9 {
        format!("{}", v.round() as i64)
    } else {
        let s = format!("{v:.2}");
        s.trim_end_matches('0').trim_end_matches('.').to_string()
    }
}

/// Render a snapped bar length as a human-readable label, auto-scaling the
/// unit so the number stays readable (maintainer, 2026-07-20).
///
/// Only `nm`-based scales are promoted: at/above 1000 nm the label switches to
/// µm, so an SMLM render still reads `200 nm` while a 0.1 µm/px camera frame
/// reads `50 µm` instead of `50000 nm`. Any other unit is passed through
/// untouched — we don't know its ladder. Promotion is display-only; the wire
/// and sidecar stay in nm (ADR 0034 §2), so nothing about the stored
/// calibration changes.
fn fmt_scale_label(value: f64, unit: &str) -> String {
    if unit.eq_ignore_ascii_case("nm") && value >= 1000.0 {
        return format!("{} µm", fmt_scale_value(value / 1000.0));
    }
    format!("{} {}", fmt_scale_value(value), unit)
}

/// Parse a user-typed pixel size in NANOMETRES (ADR 0034 §4 live entry).
///
/// nm is both the entry unit (maintainer, 2026-07-20 — an SMLM pixel is ~10 nm,
/// which reads far better than `0.01` µm) and what the schema and sidecar
/// store, so there is no conversion: the typed number IS `nm_per_px`. Rejects
/// non-positive and non-finite input so a typo can't install a nonsense
/// calibration.
pub(in crate::ui) fn parse_nm_pixel_size(input: &str) -> Option<f64> {
    let v: f64 = input.trim().parse().ok()?;
    if !v.is_finite() || v <= 0.0 {
        return None;
    }
    Some(v)
}

/// Bar + label geometry for the scalebar overlay (ADR 0034), all in physical
/// screen px. `backing` is a dark box drawn under the white `bar` and the
/// (separately shaped) label so the overlay reads on any raster.
#[derive(Debug, Clone, Copy)]
pub(in crate::ui) struct ScalebarDraw {
    pub(in crate::ui) backing: ScreenRect,
    pub(in crate::ui) bar: ScreenRect,
    pub(in crate::ui) label_x: f32,
    pub(in crate::ui) label_y: f32,
}

/// Most lines a figure caption may occupy. Past this the band stops growing and
/// the remainder is clipped: a caption reserves space from the figure it
/// describes, so an unbounded one would shrink the image toward nothing.
///
/// LOAD-BEARING, and sits right AT the boundary — not a backstop. Measured on a
/// 4096px-wide frontend (PR #72 review, revised): a 1680px preview pane holds
/// ~145 chars per line, so a caption at the full [`CAPTION_MAX_CHARS`] wraps to
/// EXACTLY 3 lines. The two caps therefore bind together at a typical pane
/// width rather than one shadowing the other; lower this and full-length
/// captions start losing their tail.
///
/// (An earlier revision of this comment claimed ~103 chars/line and concluded
/// the clip was unexercised. That measurement was wrong and so was the
/// conclusion — recorded here because "this limit never fires" is exactly the
/// kind of belief that gets a limit quietly removed.)
const CAPTION_MAX_LINES: usize = 3;

/// Geometry for the figure-caption band (all physical screen px). `backing` is
/// the full-pane-width box at the BOTTOM of the preview pane; the image is
/// letterboxed into the rect above it, so unlike the scalebar this is reserved
/// space, not an overlay — the caption never covers the figure.
#[derive(Debug, Clone, Copy)]
pub(in crate::ui) struct CaptionDraw {
    pub(in crate::ui) backing: ScreenRect,
    pub(in crate::ui) text_x: f32,
    pub(in crate::ui) text_y: f32,
    pub(in crate::ui) clip_bottom: f32,
}

impl State {
    /// Compute the dynamic scalebar's bar + label geometry for this frame and
    /// shape its label buffer (ADR 0034). Called once per render, BEFORE the
    /// render pass so the label is ready for `text.prepare`. Returns `None`
    /// (and clears `scalebar_label`) unless the toggle is on, an image raster
    /// is shown, and a physical scale is present.
    ///
    /// The bar length keys off the **source→screen** mapping (`canvas_w /
    /// src_w`), NEVER the raster buffer size — zoom re-rasters the PNG larger
    /// but the physical mapping is unchanged, so keying off the buffer would
    /// break the bar after every zoom (the load-bearing detail, ADR 0034 §3;
    /// same rationale as source-px ROI, ADR 0022). Phase 1 renders one bar
    /// from `axes[0]` (the horizontal x axis); equal axes collapse naturally.
    ///
    /// `image_rect` is the pane rect MINUS any reserved figure-caption band
    /// (ADR 0025, 2026-07-25) — the rect the image is actually letterboxed
    /// into. Taking it rather than the full pane rect is what puts the bar
    /// above a caption instead of behind it, with no inset arithmetic here.
    pub(in crate::ui) fn build_scalebar(
        &mut self,
        png_rect: Option<ScreenRect>,
        image_rect: ScreenRect,
    ) -> Option<ScalebarDraw> {
        // Cheap early-outs; every miss clears the stale label.
        if !self.scalebar_on {
            self.scalebar_label = None;
            return None;
        }
        let clear_and_none = |s: &mut Self| -> Option<ScalebarDraw> {
            s.scalebar_label = None;
            None
        };
        let (Some(letterbox_rect), Some(quad_px)) =
            (png_rect, self.preview_png.as_ref().map(|q| q.size_px))
        else {
            return clear_and_none(self);
        };
        let is_image = self
            .preview_node_id_fired
            .as_deref()
            .map(Self::is_image_node_id)
            .unwrap_or(false);
        if !is_image {
            return clear_and_none(self);
        }
        let Some(scale) = self.preview_scale.clone() else {
            return clear_and_none(self);
        };
        let Some(axis) = scale.axes.first() else {
            return clear_and_none(self);
        };
        if axis.per_px <= 0.0 {
            return clear_and_none(self);
        }
        // Source→screen mapping (must match the draw block's `canvas_w`).
        let zoom_max = png_zoom_max(image_rect.w, image_rect.h, quad_px);
        let zoom = self.preview_png_zoom.clamp(1.0, zoom_max);
        let canvas_w = letterbox_rect.w * zoom;
        // Measure against the AS-SERVED width, not the texture width: an image
        // over `max_texture_dimension_2d` is GPU-downsampled for display, but
        // the wire's `nm_per_px` still describes the served pixels. `canvas_w`
        // spans the whole image either way, so pairing it with the served width
        // is what keeps the bar physically true (Codex review R2 — a 20000 px
        // raster capped to 16384 otherwise reads ~1.22x short).
        let (src_w, _src_h) = self
            .preview_png_src_dims
            .or(self.preview_png_dims)
            .unwrap_or(quad_px);
        if src_w == 0 || canvas_w <= 0.0 {
            return clear_and_none(self);
        }
        let screen_px_per_src_px = canvas_w / src_w as f32;
        let screen_px_per_unit = screen_px_per_src_px / axis.per_px as f32;
        if !screen_px_per_unit.is_finite() || screen_px_per_unit <= 0.0 {
            return clear_and_none(self);
        }
        // Adaptive length: aim ~15% of the pane, snap to 1/2/5×10ⁿ.
        let target_px = image_rect.w * 0.15;
        let nice = snap_1_2_5((target_px / screen_px_per_unit) as f64);
        let bar_len = nice as f32 * screen_px_per_unit;
        // Skip a degenerate / pane-overflowing bar (e.g. absurd scale value).
        if bar_len <= 1.0 || bar_len > image_rect.w {
            return clear_and_none(self);
        }
        // Shape the label OUTSIDE the render pass (a persistent buffer on self).
        let scale_f = self.scale;
        let label = fmt_scale_label(nice, &scale.unit);
        let label_buf = crate::ui::preview::markdown::MarkdownPreview::new_plain(
            self.text.font_system_mut(),
            &label,
            (400.0 * scale_f).max(1.0),
            scale_f,
        );
        let label_w = label_buf
            .buffer
            .layout_runs()
            .next()
            .map(|r| r.line_w)
            .unwrap_or(0.0);
        let label_h = label_buf.buffer.metrics().line_height;
        self.scalebar_label = Some(label_buf);
        // Geometry: bottom-left corner, label above the bar, dark backing box.
        let m = 10.0 * scale_f;
        let bar_h = (4.0 * scale_f).max(2.0);
        let gap = 3.0 * scale_f;
        let pad = 4.0 * scale_f;
        let bar_x = image_rect.x + m;
        let bar_y = image_rect.y + image_rect.h - m - bar_h;
        let label_x = bar_x;
        let label_y = bar_y - gap - label_h;
        let content_right = (bar_x + bar_len).max(label_x + label_w);
        let backing = ScreenRect {
            x: bar_x - pad,
            y: label_y - pad,
            w: (content_right - bar_x) + 2.0 * pad,
            h: (bar_y + bar_h - label_y) + 2.0 * pad,
        };
        Some(ScalebarDraw {
            backing,
            bar: ScreenRect {
                x: bar_x,
                y: bar_y,
                w: bar_len,
                h: bar_h,
            },
            label_x,
            label_y,
        })
    }

    /// Compute the figure-caption band geometry for this frame and shape its
    /// text buffer. Called once per render BEFORE `png_rect` — the band is
    /// RESERVED space, so the image rect is derived by subtracting what this
    /// returns, and every downstream piece of image geometry keys off that
    /// reduced rect. Returns `None` — clearing any stale buffer — unless an
    /// image raster is on screen AND the active workspace has a caption stored
    /// for exactly that file.
    ///
    /// Gated on `is_image_node_id` for the same reason the scalebar is: a
    /// caption is a statement about a FIGURE. Carrying one over a code or
    /// markdown preview would be a claim about the wrong thing, so the store
    /// keeps it and the renderer simply declines to draw it.
    pub(in crate::ui) fn build_caption(&mut self, preview_rect: ScreenRect) -> Option<CaptionDraw> {
        let clear_and_none = |s: &mut Self| -> Option<CaptionDraw> {
            s.caption_label = None;
            None
        };
        let Some(node_id) = self.preview_node_id_fired.clone() else {
            return clear_and_none(self);
        };
        if !Self::is_image_node_id(&node_id) {
            return clear_and_none(self);
        }
        let ws_key = self.current_workspace_key();
        let Some(text) = self.preview_captions.get(&ws_key, &node_id).cloned() else {
            return clear_and_none(self);
        };
        let scale_f = self.scale;
        let pad = 6.0 * scale_f;
        let text_w = preview_rect.w - 2.0 * pad;
        if text_w <= 1.0 || preview_rect.h <= 1.0 {
            return clear_and_none(self);
        }
        let buf = crate::ui::preview::markdown::MarkdownPreview::new_plain(
            self.text.font_system_mut(),
            &text,
            text_w,
            scale_f,
        );
        let line_h = buf.buffer.metrics().line_height;
        let lines = buf.buffer.layout_runs().count().max(1);
        self.caption_label = Some(buf);
        // Cap the drawn height so a long caption can never swallow the figure it
        // describes: past CAPTION_MAX_LINES the overlay stops growing and the
        // remainder is clipped by the ExtraArea's `bottom`. The length cap makes
        // this rare; the clamp is what keeps a pathological caption survivable.
        let drawn_lines = lines.min(CAPTION_MAX_LINES);
        let text_h = line_h * drawn_lines as f32;
        let backing_h = text_h + 2.0 * pad;
        // Never let the overlay take more than half the pane — on a very short
        // preview pane even CAPTION_MAX_LINES is too much.
        if backing_h > preview_rect.h * 0.5 {
            return clear_and_none(self);
        }
        let backing = ScreenRect {
            x: preview_rect.x,
            y: preview_rect.y + preview_rect.h - backing_h,
            w: preview_rect.w,
            h: backing_h,
        };
        Some(CaptionDraw {
            backing,
            text_x: preview_rect.x + pad,
            text_y: backing.y + pad,
            clip_bottom: backing.y + backing_h - pad,
        })
    }
}

impl State {
    /// Open the pixel-size prompt for the previewed raster (ADR 0034 §4).
    /// Returns false when there's nothing to calibrate, so the caller can leave
    /// the keystroke alone.
    pub(in crate::ui) fn begin_scale_entry(&mut self) -> bool {
        let Some(node_id) = self.preview_node_id_fired.clone() else {
            return false;
        };
        if !Self::is_image_node_id(&node_id) {
            return false;
        }
        self.nav_prompt = Some(NavPrompt::ScaleEntry {
            node_id,
            input: String::new(),
        });
        // Ctrl+S fires from the PREVIEW focus arm, but every NavPrompt's
        // keystroke handling (chars / Backspace / Enter / Esc) lives in the
        // NavTree arm — so without this the prompt opens and then silently
        // swallows nothing: typing goes to the preview's zoom/pan keys and
        // Enter never reaches `confirm_scale_entry`. Ctrl+N doesn't need this
        // because it can only fire from NavTree focus in the first place.
        // Codex v0.4.4 gate. Remember where the user actually was so resolving
        // the prompt puts them back (5th-gate follow-up) — the focus move is
        // ours, not theirs, so it shouldn't outlive the prompt.
        self.scale_entry_prior_focus = Some(self.focus);
        self.set_focus(PaneFocus::NavTree);
        self.status = "pixel size (nm): ".to_string();
        self.window.request_redraw();
        true
    }

    /// Confirm the pixel-size prompt: validate the typed nanometres (no
    /// nm, and fire `preview.set_scale`. The backend writes the sidecar and
    /// replies with the re-rendered preview (carrying the F1-rescaled
    /// `physical_scale`), which lands in the normal preview path — so the bar
    /// appears from the authoritative value rather than a local guess that
    /// could be wrong for a downsampled raster.
    pub(in crate::ui) fn confirm_scale_entry(&mut self) {
        let (node_id, raw) = match self.nav_prompt.as_ref() {
            Some(NavPrompt::ScaleEntry { node_id, input }) => {
                (node_id.clone(), input.trim().to_string())
            }
            _ => return,
        };
        let Some(nm_per_px) = parse_nm_pixel_size(&raw) else {
            // Keep the prompt open so the user can correct the typo.
            self.status = "pixel size · enter a positive number in nm".to_string();
            self.window.request_redraw();
            return;
        };
        // Isotropic from a single entry: both axes get the same value. The
        // anisotropic (XZ) case is Phase 3 — one number can't describe it, and
        // guessing would be worse than the Phase-1 lateral bar.
        let generation = self.next_preview_gen();
        if let Err(e) = self.send(crate::transport::OutgoingReq::PreviewSetScale {
            node_id: node_id.clone(),
            nm_per_px,
            workspace_id: self.active_workspace_id.clone(),
            generation,
        }) {
            tracing::warn!(error = %e, %node_id, "drop preview.set_scale — channel closed");
            self.status = "pixel size · channel closed".to_string();
            self.window.request_redraw();
            return;
        }
        tracing::info!(%node_id, nm_per_px,
            "scale entry → preview.set_scale");
        // Arm the overlay so the bar is visible the moment the re-rendered
        // preview lands; without a scale present the toggle would otherwise
        // just re-open this prompt.
        self.scalebar_on = true;
        self.nav_prompt = None;
        // Gate the success message on THIS save: the reply arrives as an
        // ordinary preview install, so without a pending marker we'd either
        // leave "saving…" up forever or congratulate the user on every
        // unrelated preview.get.
        self.scale_save_pending = Some((node_id.clone(), raw.clone()));
        // Prompt resolved — hand focus back to the pane the user was actually
        // in (the Preview they're calibrating), so their next zoom/pan key
        // lands there rather than in the tree.
        if let Some(prior) = self.scale_entry_prior_focus.take() {
            self.set_focus(prior);
        }
        self.status = format!("pixel size {raw} nm · saving…");
        self.window.request_redraw();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn caption_store_is_keyed_by_workspace_and_evicts_fifo() {
        let mut s = CaptionStore::default();
        s.set("wsA".into(), "files:p.png".into(), "A's caption".into());
        s.set("wsB".into(), "files:p.png".into(), "B's caption".into());
        // Same FILE, different workspace → distinct entries. This is the ADR-0034
        // F2 hazard (one workspace's annotation over another's image) made
        // unrepresentable rather than merely avoided.
        assert_eq!(s.get("wsA", "files:p.png").unwrap(), "A's caption");
        assert_eq!(s.get("wsB", "files:p.png").unwrap(), "B's caption");
        assert_eq!(s.get("wsC", "files:p.png"), None);
        // Latest-wins on re-set, and no duplicate order entry.
        s.set("wsA".into(), "files:p.png".into(), "A's second".into());
        assert_eq!(s.get("wsA", "files:p.png").unwrap(), "A's second");
        assert_eq!(s.order.len(), 2);
        // Retiring one leaves the other alone.
        s.clear_one("wsA", "files:p.png");
        assert_eq!(s.get("wsA", "files:p.png"), None);
        assert_eq!(s.get("wsB", "files:p.png").unwrap(), "B's caption");
        assert_eq!(s.order.len(), 1);
        // Bounded: past the cap the oldest badge is evicted, newest retained.
        let mut s = CaptionStore::default();
        for i in 0..(CAPTION_STORE_CAP + 10) {
            s.set("ws".into(), format!("files:{i}.png"), format!("cap {i}"));
        }
        assert_eq!(s.map.len(), CAPTION_STORE_CAP);
        assert_eq!(s.get("ws", "files:0.png"), None, "oldest evicted");
        let newest = CAPTION_STORE_CAP + 9;
        assert_eq!(
            s.get("ws", &format!("files:{newest}.png")).unwrap(),
            &format!("cap {newest}")
        );
    }

    #[test]
    fn snap_1_2_5_hits_nice_values() {
        assert_eq!(snap_1_2_5(1.0), 1.0);
        assert_eq!(snap_1_2_5(1.3), 1.0);
        assert_eq!(snap_1_2_5(1.7), 2.0);
        assert_eq!(snap_1_2_5(3.0), 2.0);
        assert_eq!(snap_1_2_5(4.0), 5.0);
        assert_eq!(snap_1_2_5(9.0), 10.0);
        assert_eq!(snap_1_2_5(430.0), 500.0);
        assert_eq!(snap_1_2_5(1800.0), 2000.0);
        assert_eq!(snap_1_2_5(0.03), 0.02);
        // Degenerate inputs never panic.
        assert_eq!(snap_1_2_5(0.0), 1.0);
        assert_eq!(snap_1_2_5(-5.0), 1.0);
        assert_eq!(snap_1_2_5(f64::NAN), 1.0);
    }

    #[test]
    fn fmt_scale_value_trims_cleanly() {
        assert_eq!(fmt_scale_value(500.0), "500");
        assert_eq!(fmt_scale_value(2.0), "2");
        assert_eq!(fmt_scale_value(2.5), "2.5");
        assert_eq!(fmt_scale_value(0.02), "0.02");
    }

    #[test]
    fn fmt_scale_label_promotes_nm_to_um_only_past_1000() {
        // The two live-verified fixtures must keep reading exactly as they do.
        assert_eq!(fmt_scale_label(200.0, "nm"), "200 nm");
        assert_eq!(fmt_scale_label(500.0, "nm"), "500 nm");
        // Boundary: 1000 nm promotes.
        assert_eq!(fmt_scale_label(999.0, "nm"), "999 nm");
        assert_eq!(fmt_scale_label(1000.0, "nm"), "1 µm");
        assert_eq!(fmt_scale_label(2000.0, "nm"), "2 µm");
        // A 0.1 µm/px camera frame lands here rather than "50000 nm".
        assert_eq!(fmt_scale_label(50000.0, "nm"), "50 µm");
        // Fractional promotion trims cleanly.
        assert_eq!(fmt_scale_label(2500.0, "nm"), "2.5 µm");
        // Non-nm units pass through untouched — we don't know their ladder.
        assert_eq!(fmt_scale_label(5000.0, "px"), "5000 px");
        assert_eq!(fmt_scale_label(2.0, "µm"), "2 µm");
    }

    #[test]
    fn parse_nm_pixel_size_passes_through_and_rejects_junk() {
        // nm in, nm out — the entry unit IS the schema unit, so no conversion.
        // A typical SMLM pixel is ~10 nm, which is why nm beats µm for entry.
        assert_eq!(parse_nm_pixel_size("9.78"), Some(9.78));
        assert_eq!(parse_nm_pixel_size("100"), Some(100.0));
        assert_eq!(parse_nm_pixel_size("  65 "), Some(65.0));
        // A typo must not install a nonsense calibration.
        assert_eq!(parse_nm_pixel_size(""), None);
        assert_eq!(parse_nm_pixel_size("abc"), None);
        assert_eq!(parse_nm_pixel_size("0"), None);
        assert_eq!(parse_nm_pixel_size("-1"), None);
        assert_eq!(parse_nm_pixel_size("inf"), None);
    }

    #[test]
    fn parse_physical_scale_reads_axes_and_unit() {
        let v = serde_json::json!({
            "physical_scale": {
                "axes": [
                    {"name": "x", "nm_per_px": 2.0},
                    {"name": "y", "nm_per_px": 2.0}
                ],
                "unit": "nm"
            }
        });
        let ps = parse_physical_scale(&v).expect("parses");
        assert_eq!(ps.unit, "nm");
        assert_eq!(ps.axes.len(), 2);
        assert_eq!(ps.axes[0].name, "x");
        assert_eq!(ps.axes[0].per_px, 2.0);
    }

    #[test]
    fn parse_physical_scale_rejects_missing_or_empty() {
        // No physical_scale key → None (so it clears like preview_page).
        assert_eq!(parse_physical_scale(&serde_json::json!({"page": 1})), None);
        // Empty axes → None.
        assert_eq!(
            parse_physical_scale(&serde_json::json!({
                "physical_scale": {"axes": [], "unit": "nm"}
            })),
            None
        );
        // Axis missing nm_per_px → None (the whole parse fails, no partial).
        assert_eq!(
            parse_physical_scale(&serde_json::json!({
                "physical_scale": {"axes": [{"name": "x"}], "unit": "nm"}
            })),
            None
        );
    }
}
