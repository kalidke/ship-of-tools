//! Pagination of the shown preview: its page and count, the zoom its page bitmap was rasterized for, and the
//! one-shot keep-view of a zoom re-raster.

/// A paginated preview's transport state (ADR 0021). A reply's extras set the page; a re-raster reply of the same
/// page keeps the view and records its zoom, anything else resets to fit.
pub(in crate::ui) struct PageView {
    /// Pagination of the preview the pane is *showing* (ADR 0021):
    /// `(page, page_count)` from the reply's extras. `None` for
    /// unpaginated previews. Drives the `n`/`p` page-turn keys and the
    /// title's `p N/M` suffix; never inspected per-file-type — any plugin
    /// that reports page extras gets the transport for free.
    page: Option<(u32, u32)>,
    /// Zoom level the *current* page bitmap was rasterized for (1.0 = fit
    /// to pane). Zooming in past this re-requests the page at the larger
    /// pixel size so rasterized text stays crisp instead of stretching the
    /// fit-sized bitmap (ADR 0021 deferred follow-up). Reset to 1.0 on a
    /// fresh page/file.
    raster_zoom: f32,
    /// One-shot: the next `image/png` render is a zoom re-raster of the
    /// page already on screen, so the quad swap must preserve zoom/pan
    /// rather than reset to fit. Set by the Preview handler, consumed in
    /// `render_preview_source`.
    keep_view: bool,
}

impl Default for PageView {
    fn default() -> Self { Self { page: None, raster_zoom: 1.0, keep_view: false } }
}

impl PageView {
    pub(in crate::ui) fn page(&self) -> Option<(u32, u32)> { self.page }
    pub(in crate::ui) fn raster_zoom(&self) -> f32 { self.raster_zoom }

    /// A preview reply's page extras: present only when the serving plugin reported them; anything else clears the
    /// page, retiring the n/p keys.
    pub(in crate::ui) fn set_page(&mut self, page: Option<(u32, u32)>) { self.page = page; }

    /// The reply re-rastered the page on screen at `zoom` (`Some`): keep the view; else a fresh page resets to fit.
    pub(in crate::ui) fn settle_reply(&mut self, reraster_zoom: Option<f32>) {
        match reraster_zoom {
            Some(z) => {
                self.raster_zoom = z;
                self.keep_view = true;
            }
            None => {
                self.raster_zoom = 1.0;
                self.keep_view = false;
            }
        }
    }

    /// The raster install takes the keep-view, once.
    pub(in crate::ui) fn take_keep_view(&mut self) -> bool { std::mem::take(&mut self.keep_view) }
}
