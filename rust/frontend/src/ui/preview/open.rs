//! External opens of the previewed file: the right outside tool, the docs page, the quarto execute.

use crate::ui::*;

impl State {
    /// `o` — open `abs` in the right external tool: an html preview body
    /// with a real fs source → the same `docs.open` request `W` sends
    /// (full CSS/JS/image fidelity, via `previewed_files_path`); a
    /// sourceless html preview → temp file + OS browser (nothing reaches
    /// this today — `.html`/`.htm` is the only route to `text/html`, and
    /// the Quarto `--embed-resources` quick-open path is a separate
    /// `IncomingEvt::QuartoOpened` handler that never sets `preview_src`
    /// — but the fallback is the honest thing to do if that ever
    /// changes); `.jl` → backend `pluto.open` (header-checked there);
    /// video → backend `video.open` (browser HTML5 playback); `.qmd` →
    /// quick Quarto render (no execution). Shared by the NavTree and
    /// Preview key arms (same behavior on the cursored / shown file).
    pub(in crate::ui) fn open_path_external(&mut self, abs: Option<String>) {
        let preview_mime = self.preview_src.as_ref().map(|(m, _)| m.clone());
        if preview_mime.as_deref() == Some("text/html") {
            // Field report: this used to always write the cached
            // preview bytes to a temp file — relative CSS/JS/images/
            // page links then resolve under the temp dir and 404. When
            // the preview traces to a real on-disk file, route through
            // `docs.open` instead (ADR 0024's site server, full asset
            // fidelity) — the same request `W` dispatches today.
            if let Some(path) = self.previewed_files_path() {
                self.docs_open_external(path);
            } else if let Some((_, bytes)) = self.preview_src.as_ref() {
                if let Err(e) = open_html_in_browser(bytes) {
                    tracing::warn!(error = %e, "failed to open preview in browser");
                }
            }
        } else if let Some(abs) = abs.as_deref() {
            let lower = abs.to_ascii_lowercase();
            let is_video = sot_protocol::video_path::video_mime(abs).is_some();
            if abs.ends_with(".jl") {
                if let Err(e) = self.send(crate::net::transport::OutgoingReq::PlutoOpen {
                    path: abs.to_string(),
                }) {
                    tracing::warn!(error = %e, "failed to dispatch pluto.open");
                }
            } else if is_video {
                // Video plays in the browser (HTML5 <video>, native HW
                // decode) — the pane only shows the poster still.
                if let Err(e) = self.send(crate::net::transport::OutgoingReq::VideoOpen {
                    path: abs.to_string(),
                }) {
                    tracing::warn!(error = %e, "failed to dispatch video.open");
                }
            } else if lower.ends_with(".qmd") {
                // Quarto: `o` = quick render (no code execution) →
                // self-contained HTML in the browser. `O` runs chunks.
                if let Err(e) = self.send(crate::net::transport::OutgoingReq::QuartoOpen {
                    path: abs.to_string(),
                    execute: false,
                }) {
                    tracing::warn!(error = %e, "failed to dispatch quarto.open");
                } else {
                    self.status = "quarto · rendering (quick)…".to_string();
                }
            } else {
                tracing::debug!(path = %abs, "`o` ignored — no handler for this file type");
            }
        }
    }

    /// `W` — open the project's built Documenter site in the OS browser
    /// (ADR 0024), deep-linking `path` when it's a built docs page.
    pub(in crate::ui) fn docs_open_external(&mut self, path: String) {
        if let Err(e) = self.send(crate::net::transport::OutgoingReq::DocsOpen { path }) {
            tracing::warn!(error = %e, "failed to dispatch docs.open");
        } else {
            self.status = "docs · opening…".to_string();
        }
    }

    /// `O` — full Quarto render WITH code execution for a `.qmd`.
    pub(in crate::ui) fn quarto_open_execute(&mut self, abs: Option<String>) {
        if let Some(abs) = abs.as_deref() {
            if abs.to_ascii_lowercase().ends_with(".qmd") {
                if let Err(e) = self.send(crate::net::transport::OutgoingReq::QuartoOpen {
                    path: abs.to_string(),
                    execute: true,
                }) {
                    tracing::warn!(error = %e, "failed to dispatch quarto.open (execute)");
                } else {
                    self.status = "quarto · rendering (run chunks)…".to_string();
                }
            }
        }
    }
}
