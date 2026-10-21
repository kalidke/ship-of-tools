//! The preview pane's subject: which node is shown, how its bytes route to a renderer, and its pin.

use crate::ui::*;

/// Pure resolution behind `State::previewed_files_path`: routes from
/// `installed` alone — never a merely-REQUESTED node id, which is the
/// field report round-2 bug (`o`/`W`/`O` pressed between firing a new
/// preview request and its reply landing used to act on the file about to
/// be shown, not the one on screen). Callers pass
/// `State::preview_src_node_id` — the node the currently-installed
/// `preview_src` reply answered — as `installed`; this function's
/// signature has no room for a "fired but not yet replied" id at all,
/// which is what makes that class of bug structurally unreachable here
/// rather than merely avoided.
///
/// Does NOT also check a pin. `pinned_preview_node_id`
/// is stamped straight from the CURSOR row (`toggle_pin`), not from an
/// installed reply — pinning a row before its preview arrives (cursor
/// moved fast, or `maybe_fire_preview`'s pinned-preview guard means it
/// never will) would let a pin outrun what's on screen exactly like a
/// fired-but-not-replied request does, and persistently since a pin
/// suppresses the normal refetch. `o`/`W`/`O` act on what is VISIBLE,
/// period — when a pinned preview IS what's installed, `installed`
/// already equals it, so nothing is lost in the common case.
pub(in crate::ui) fn resolve_previewed_path(installed: Option<&str>, root: Option<&str>) -> Option<String> {
    let id = installed?;
    let rel = id.strip_prefix("files:")?;
    if rel.is_empty() {
        return None;
    }
    let root = root?;
    let trimmed = root.trim_end_matches(['/', '\\']);
    Some(format!("{trimmed}/{rel}"))
}

/// Cap on bytes the preview pane will shape as text. A binary blob (or a
/// multi-hundred-KB text file) shaped via cosmic-text/comrak on the single
/// render thread freezes every pane — this is the guard against e.g. opening
/// a multi-GB `.h5` whose backend fallback returns raw bytes. Above this,
/// or if the content looks binary, the pane shows a one-line summary instead.
const PREVIEW_TEXT_CAP: usize = 512 * 1024;

/// Heuristic: does this blob look like binary (not safe to shape as text)?
/// A NUL byte in the leading window is the classic giveaway — text files
/// don't contain `0x00`, but HDF5 / images / archives / most binaries do
/// near the start. Cheap: scans at most the first 8 KiB.
fn looks_binary(bytes: &[u8]) -> bool {
    let window = &bytes[..bytes.len().min(8192)];
    window.contains(&0)
}

/// Translate a backend-absolute file path into the ACTIVE workspace's
/// `files:<rel>` node id, when the path lies inside `root`. Returns `None`
/// for paths outside the root, the root itself, and lookalike siblings
/// (`/a/ws` vs `/a/wsx` — the boundary separator is required).
///
/// Accepts EITHER `/` or `\` as the boundary/rel separator and always
/// normalizes the derived id to `/`: every OTHER convention in this module
/// is unix-style (node ids are unix-style on the wire regardless of host OS,
/// per files/tree.rs), but a native Windows `notify` watcher event path is
/// `\`-separated (`to_string_lossy()` off the raw OS path, never
/// canonicalized) — a `/`-only rule silently matched nothing there.
///
/// This is the `preview.changed` addressing scheme: workspaces overlap (an
/// umbrella workspace registered over the same tree, watch budgets capping
/// a watcher's coverage), so the event copy tagged with the active slug may
/// never exist — but every copy carries the absolute path, and any copy
/// whose path is inside the active root is ours to act on.
///
/// Lexical, so `root` must be in the daemon's spelling (`workspace.list`'s
/// `canonical_root`, or the hello's `project_root`): a watcher reports its
/// paths under the root its file tree resolved, while a row keeps the
/// spelling it was created with (a link above the folder, as macOS's /tmp
/// and /var have; another case or an 8.3 name on Windows).
fn files_node_id_under_root(path: &str, root: &str) -> Option<String> {
    let root = root.trim_end_matches(['/', '\\']);
    let rel = path.strip_prefix(root)?.strip_prefix(['/', '\\'])?;
    if rel.is_empty() {
        return None;
    }
    Some(format!("files:{}", rel.replace('\\', "/")))
}

/// Resolve a `preview.changed` event to a node id in the ACTIVE workspace's
/// `files:` space, or `None` when the event isn't ours to render.
///
/// Acceptance is two-path (2026-08-17, live-debugged against a peer FE):
///
/// 1. **Workspace-tag match** — the emitting workspace IS the active one
///    (slug equality; callers normalize a `None` active to the default
///    slug): trust the carried `node_id` verbatim. Needs no project-root
///    knowledge, so it stays correct while `workspace_project_roots` is
///    unpopulated (fresh connection, `workspace.list` in flight) — the gap
///    that previously left a non-default workspace deaf to its own file
///    events.
/// 2. **Path translation** — workspaces overlap (umbrella roots, watch
///    budgets), so OUR files can arrive tagged with another workspace's
///    slug; any copy whose absolute path lies under the active root is
///    ours, re-addressed into our id space (the 2026-07-17 scheme). This
///    requires the KNOWN active root — never a fallback root, which
///    mistranslates foreign paths into phantom refreshes of the active
///    tree.
pub(in crate::ui) fn resolve_preview_changed(
    event_ws: Option<&str>,
    event_node_id: Option<&str>,
    event_path: Option<&str>,
    active_ws: Option<&str>,
    known_active_root: Option<&str>,
) -> Option<String> {
    if let (Some(ews), Some(nid)) = (event_ws, event_node_id) {
        if Some(ews) == active_ws && nid.starts_with("files:") && nid.len() > "files:".len() {
            return Some(nid.to_string());
        }
    }
    files_node_id_under_root(event_path?, known_active_root?)
}

/// Offline-mode markdown placeholder. The real preview content comes from
/// the backend via `preview.get` once transport connects; this string is
/// only what the pane shows when no connection resolved.
pub(in crate::ui) const SAMPLE_MARKDOWN: &str = r##"## Offline

`sot` is running without a backend.

Pass `--socket <path>` (Unix socket / Windows named pipe) or
`--dial <host>=<endpoint>` to connect, or set `$SOT_SOCKET`. Use a planned
split-launch setup for two-terminal runs.
"##;

/// The preview's scroll limit in body lines: `shown` scrolled until its last
/// line meets the bottom of `visible_px`.
pub(in crate::ui) fn preview_max_scroll(line_h: f32, visible_px: f32, shown: &MarkdownPreview) -> u16 {
    ((shown.total_visual_pixels(line_h) - visible_px).max(0.0) / line_h).ceil() as u16
}

/// The one preview buffer on screen and the height, px, of the rect it is
/// drawn in: the edit buffer (whole preview rect) while editing, else the
/// markdown body (inset md rect). The scroll clamp measures only this buffer,
/// so a hidden one never scrolls the pane into blank space.
pub(in crate::ui) fn preview_scroll_target<'a>(
    show_edit: bool,
    md: &'a MarkdownPreview,
    md_h: f32,
    edit: Option<&'a MarkdownPreview>,
    edit_h: f32,
) -> (&'a MarkdownPreview, f32) {
    match edit.filter(|_| show_edit) {
        Some(e) => (e, edit_h),
        None => (md, md_h),
    }
}

/// Wire shape for `application/vnd.sot.tokens+json` from the
/// kernel-side JuliaSource plugin (`53a46d8`). Concatenating
/// every span's `text` is guaranteed to reproduce the source file
/// byte-for-byte — kinds are advisory for colouring.
#[derive(Debug, serde::Deserialize)]
struct TokensPayload {
    spans: Vec<TokenSpan>,
}

#[derive(Debug, serde::Deserialize)]
struct TokenSpan {
    text: String,
    kind: String,
}

/// Raster image mimes the preview pane decodes through the byte-sniffing quad
/// path (`ui/preview/image/png.rs`, `with_guessed_format`). Kept in sync with the raster
/// `image/*` outputs of the backend's `mime_for_path`: `image/svg+xml` is
/// excluded (vector — it has its own preview path) and `image/tiff` is omitted
/// because the backend never emits it.
pub(in crate::ui) fn is_raster_preview_mime(mime: &str) -> bool {
    matches!(
        mime,
        "image/png" | "image/jpeg" | "image/gif" | "image/webp" | "image/bmp"
    )
}

impl State {
    /// C2 pin-and-leave toggle. If a preview is pinned, `p` (from any row)
    /// UNPINS it and jumps the cursor to the formerly-pinned row, so the user
    /// lands back on it instead of having to hunt the tree for the pinned node
    /// to clear it. If nothing is pinned, `p` pins the cursor row and the
    /// preview then stays put as the cursor roams. Pinning is `files:`-only —
    /// modules/sessions rows have no backend preview, so pinning them would be
    /// a confusing no-op.
    pub(in crate::ui) fn toggle_pin(&mut self) {
        // Pinned → unpin from anywhere, and move the cursor to the pinned node
        // if it's in the current tree (so the user sees what was pinned).
        if let Some(pinned) = self.pinned_preview_node_id.take() {
            if let Some(idx) = self.tree.rows.iter().position(|r| r.node.id == pinned) {
                self.tree.selected = idx;
            }
            self.status = format!("unpinned · {pinned}");
            self.window.request_redraw();
            return;
        }
        // Nothing pinned → pin the cursor row (files: only).
        let Some(row) = self.tree.rows.get(self.tree.selected) else {
            return;
        };
        let id = row.node.id.clone();
        if !id.starts_with("files:") {
            return;
        }
        self.pinned_preview_node_id = Some(id.clone());
        self.status = format!("pinned · {id}");
        self.window.request_redraw();
    }

    /// Display name for the preview-pane title: the **basename** of the file
    /// the preview pane is *showing* (`preview_node_id_fired`), not the roaming
    /// cursor — so while pinned (C2) the title tracks the pinned file. Only the
    /// filename (not the full relative path) so it stays short enough for the
    /// pane; the full path is still recoverable via Ctrl+C in NavTree.
    /// `None` for non-file previews (sessions / hosts / workspace rows) and
    /// before any preview has fired.
    pub(in crate::ui) fn preview_pane_name(&self) -> Option<String> {
        let rel = self
            .preview_node_id_fired
            .as_deref()?
            .strip_prefix("files:")?;
        if rel.is_empty() {
            return None;
        }
        // Full workspace-relative path — not just the basename.
        // `middle_truncate` at render time keeps the head + the filename/ext
        // when the title overflows the pane width.
        // Paginated preview (ADR 0021): surface position + the page-turn
        // keys in the title so the affordance is discoverable.
        match self.preview_page {
            Some((page, count)) if count > 1 => Some(format!("{rel} · p {page}/{count} · n/p")),
            _ => Some(rel.to_string()),
        }
    }

    /// Preview-pane size in physical pixels, as the render-fit hint sent
    /// with `preview.get` (ADR 0021) — rasterizing plugins (PDF) produce
    /// the page at display resolution so the GPU samples ~1:1. `None`
    /// before the first layout pass (pane rect still zero), in which case
    /// the plugin falls back to its fixed DPI.
    pub(in crate::ui) fn preview_fit_px(&self) -> (Option<u32>, Option<u32>) {
        let w = (self.pane_rects.preview.width as f32 * self.cell_w) as u32;
        let h = (self.pane_rects.preview.height as f32 * self.cell_h) as u32;
        if w == 0 || h == 0 {
            (None, None)
        } else {
            (Some(w), Some(h))
        }
    }

    /// Branch on mime and route a preview blob to the right renderer.
    /// Called both from the live `Preview` event arm and from the
    /// font-rescale path (which replays against the cached source).
    #[allow(clippy::too_many_lines, reason = "routes a preview blob to its renderer by mime; predates the 100-line limit")]
    pub(in crate::ui) fn render_preview_source(&mut self, mime: &str, bytes: &[u8]) {
        let scale = self.scale * self.text_scale_mult;
        if is_raster_preview_mime(mime) {
            // Paginated document pages (preview_page set from this reply's
            // extras) filter Linear — rasterized text aliases hard under
            // Nearest. Standalone rasters keep Nearest per the 2026-05-22 ask.
            let sampler = if self.preview_page.is_some() {
                crate::ui::render::quad::SamplerKind::Linear
            } else {
                crate::ui::render::quad::SamplerKind::Nearest
            };
            match crate::ui::preview::image::png::quad_and_source_dims_from_png_bytes(
                &self.device,
                &self.queue,
                &self.quad_pipeline,
                bytes,
                sampler,
            ) {
                Ok((q, src_w, src_h)) => {
                    let dims = q.size_px;
                    self.preview_png = Some(q);
                    self.preview_png_dims = Some(dims);
                    // Pre-downsample size for the ADR-0034 scalebar (see field
                    // docs). Same as `dims` unless the GPU-fit shrank it.
                    self.preview_png_src_dims = Some((src_w, src_h));
                    if std::mem::take(&mut self.preview_reraster_keep_view) {
                        // Zoom re-raster of the same page (ADR 0021): the new
                        // bitmap is the same page at higher resolution. Fit-to-
                        // pane normalizes source resolution, so keeping zoom/pan
                        // leaves the on-screen view identical — only sharper.
                    } else {
                        // Restore a cached view if we've seen another image
                        // of the same size in the same directory (e.g. the
                        // next render in a time-step series). The cache
                        // holds a source-px ROI whose solve needs the live
                        // image rect — which doesn't exist here, and whose
                        // caption band at this moment is still the previous
                        // node's — so a hit only PARKS the restore for the
                        // render pass (see `pending_roi_restore`). Either
                        // way the view resets to fit now, so every miss and
                        // any pre-consume frame paint the default. Image
                        // nodes only: a PDF page turn shares (dir, dims)
                        // across pages and must reset, not inherit another
                        // page's view.
                        self.pending_roi_restore = self
                            .preview_node_id_fired
                            .as_deref()
                            .filter(|nid| Self::is_image_node_id(nid))
                            .and_then(|nid| {
                                let key = png_cache_key_from_node_id(Some(nid), dims)?;
                                let roi = *self.preview_png_cache.get(&key)?;
                                Some((nid.to_string(), roi))
                            });
                        self.preview_png_zoom = 1.0;
                        self.preview_png_pan_px = (0.0, 0.0);
                    }
                }
                Err(e) => tracing::warn!(error = %e, "preview-png decode failed"),
            }
        } else if mime.starts_with("application/vnd.sot.tokens+json") {
            match serde_json::from_slice::<TokensPayload>(bytes) {
                Ok(payload) => {
                    let pairs: Vec<(String, String)> = payload
                        .spans
                        .into_iter()
                        .map(|s| (s.text, s.kind))
                        .collect();
                    self.preview_md = MarkdownPreview::new_tokens(
                        self.text.font_system_mut(),
                        &pairs,
                        self.md_rect_px.w.max(1.0),
                        scale,
                    );
                    self.preview_png = None;
                    self.preview_svg = None;
                    self.preview_scroll = 0;
                }
                Err(e) => {
                    tracing::warn!(error = %e, "tokens+json decode failed");
                }
            }
        } else if bytes.len() > PREVIEW_TEXT_CAP || looks_binary(bytes) {
            // Oversized or binary payload: never feed it to comrak / the text
            // shaper — doing so on the single render thread freezes every pane
            // (this is what a multi-GB `.h5` whose backend fallback returned
            // raw bytes did). Show a one-line summary instead. The PNG/tokens
            // branches above still handle their real binary formats.
            let kind = if looks_binary(bytes) {
                "binary"
            } else {
                "large"
            };
            let msg = format!(
                "{kind} file — {} bytes\n\nNot previewed as text (mime: {mime}).",
                bytes.len()
            );
            self.preview_md = MarkdownPreview::new_plain(
                self.text.font_system_mut(),
                &msg,
                self.md_rect_px.w.max(1.0),
                scale,
            );
            self.preview_png = None;
            self.preview_svg = None;
            self.preview_scroll = 0;
        } else if mime == "text/markdown" || mime == "text/x-markdown" {
            if let Ok(s) = std::str::from_utf8(bytes) {
                // NOTE: `figure_failed` is deliberately NOT touched here.
                // This function is also the cached-source REFLOW callback
                // (apply_text_scale, the needs_md_reflow redraw path,
                // workspace-switch restore) — none of those are new
                // evidence a failed figure now exists, only a re-render of
                // bytes already on hand. Clearing here once caused a
                // request storm: a failure sets needs_md_reflow, the next
                // redraw re-entered this function, cleared the failure,
                // and dispatch_pending_figures refired the same doomed
                // request every frame, forever (field report round 2).
                // The clear lives at the actual fresh-reply seam instead —
                // the `IncomingEvt::Preview` handler, right before it
                // calls this function.
                let math_metrics = self.build_math_metrics();
                let figure_metrics = self.build_figure_metrics();
                self.preview_md = MarkdownPreview::new(
                    self.text.font_system_mut(),
                    s,
                    self.md_rect_px.w.max(1.0),
                    self.md_rect_px.h.max(1.0),
                    scale,
                    &math_metrics,
                    &figure_metrics,
                    &self.highlight_service,
                    &self.markdown_token_cache,
                );
                self.preview_png = None;
                self.preview_svg = None;
                self.preview_scroll = 0;
                // Fire math.render for any `$$...$$` blocks the walk
                // discovered. Replies populate `math_cache` and the
                // next paint pulls the SVG in via A3.
                self.dispatch_pending_math();
                // Fire figure.get for any `![](url)` regions. Replies
                // populate `figure_cache` and the next paint draws the
                // bitmap over the FFFC placeholder the walk reserved.
                self.dispatch_pending_figures();
                // Fire markdown.tokenize for any Julia fence that
                // didn't hit the per-fence cache. Replies overlay onto
                // tree-sitter's base on the next reflow.
                self.dispatch_pending_markdown_tokens();
            }
        } else if let Ok(s) = std::str::from_utf8(bytes) {
            self.preview_md = MarkdownPreview::new_plain(
                self.text.font_system_mut(),
                s,
                self.md_rect_px.w.max(1.0),
                scale,
            );
            self.preview_png = None;
            self.preview_svg = None;
            self.preview_scroll = 0;
        } else {
            tracing::debug!(
                %mime,
                len = bytes.len(),
                "preview blob is not UTF-8 and not an image — skipping"
            );
        }
    }

    /// Rebuild the refused-hello overlay buffer (ADR 0030 protocol mismatch, ADR 0049 account refusals) from
    /// `hello_refused` as a plain monospace
    /// buffer shaped to the preview width — so it reuses the same overlay paint
    /// path. No-op (clears the buffer) when no refusal is set.
    pub(in crate::ui) fn rebuild_fatal_overlay(&mut self) {
        let Some(body) = self.hello_refused.get(&self.active_host).cloned() else {
            self.preview_fatal = None;
            return;
        };
        let width = self.md_rect_px.w.max(1.0);
        let scale = self.scale * self.text_scale_mult;
        self.preview_fatal = Some(MarkdownPreview::new_plain(
            self.text.font_system_mut(),
            &body,
            width,
            scale,
        ));
    }
}

#[cfg(test)]
#[path = "pane_tests.rs"]
mod tests;
