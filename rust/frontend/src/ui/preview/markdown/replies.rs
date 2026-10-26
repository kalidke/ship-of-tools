//! Markdown replies: rendered math SVGs and semantic token spans, each cached with a reflow.

use crate::ui::*;

impl State {
    /// A render that will not come: stop waiting on the equation and say why. A failure forces no reflow, so the
    /// removed key cannot loop; the next rebuild of the preview asks for the equation again.
    pub(crate) fn on_math_render_failed(&mut self, latex: String, display: bool, message: String) {
        self.md_media.math_failed(&(latex, display));
        self.status.set(format!("math · {message}"));
    }

    pub(crate) fn on_math_rendered(
        &mut self,
        latex: String,
        svg_bytes: Vec<u8>,
        ex: f32,
        display: bool,
    ) {
        // Stash in the (latex, display)-keyed cache for the
        // A3 paint pass. Parse the SVG's ex-unit width/height
        // up front so the rasterise step can size the pixmap
        // relative to body font instead of fit-stretching the
        // SVG into a fixed-pixel letterbox (which is what
        // made every display block render ~4× oversized).
        let (width_ex, height_ex, vertical_align_ex) = parse_math_svg_dims(&svg_bytes);
        let key = (latex.clone(), display);
        // Caching it forces a one-shot rebuild of the shaped
        // buffer before the next paint so the walk consults the
        // freshly-cached dims when reserving each block's
        // vertical space.
        self.md_media.install_math(
            key,
            MathSvg {
                svg_bytes: svg_bytes.clone(),
                ex,
                width_ex,
                height_ex,
                vertical_align_ex,
                rasterised: None,
            },
        );
        self.window.request_redraw();
        // Also keep the old standalone math-pane preview
        // path alive for the M1 acceptance test fixture
        // (`requirements.md` has a canonical integral
        // expectation against the math SVG). Soon the
        // pane will be retired in favour of inline
        // markdown placement.
        match quad_from_svg_bytes(
            &self.device,
            &self.queue,
            &self.quad_pipeline,
            &svg_bytes,
            1024,
            256,
        ) {
            Ok(q) => {
                tracing::info!(bytes = svg_bytes.len(), "math SVG rasterised");
                self.shaped.show_svg(q);
            }
            Err(e) => tracing::warn!(error = %e, "math SVG rasterise failed"),
        }
    }

    pub(crate) fn on_markdown_tokens(
        &mut self,
        lang: String,
        source_hash: u64,
        spans: Vec<crate::net::transport::MarkdownToken>,
    ) {
        // Backend semantic overlay landed. Stash in the per-fence
        // cache, clear in-flight pending, and ask for a reflow so
        // the next redraw consumes the cache instead of relying
        // on the tree-sitter base alone.
        let key = (lang.clone(), source_hash);
        let n = spans.len();
        self.md_media.install_tokens(key, spans);
        tracing::debug!(
            %lang,
            source_hash,
            spans = n,
            "markdown.tokens received → cache + reflow"
        );
        self.window.request_redraw();
    }
}
