//! `MarkdownPreview`'s methods (build, resize, scroll and hit-test math) and its text helpers.

use super::*;

/// Collapse CRLF / lone-CR line endings to LF before handing text to the
/// shaper. cosmic-text treats a `\r` as its own line break, so a Windows
/// CRLF (`\r\n`) source renders a blank line between *every* real line —
/// the "double-spaced code in module mode" bug, which only showed up for
/// repos checked out with CRLF endings (e.g. RJTrack) while LF repos
/// rendered fine. Display-only normalization: the edit buffer and the
/// clipboard-yank paths read their own bytes elsewhere, so this doesn't
/// touch what gets written or copied. Returns a borrow when there's no
/// CR to strip (the common LF case) to avoid a needless allocation.
pub(super) fn normalize_newlines(s: &str) -> std::borrow::Cow<'_, str> {
    if s.contains('\r') {
        std::borrow::Cow::Owned(s.replace("\r\n", "\n").replace('\r', "\n"))
    } else {
        std::borrow::Cow::Borrowed(s)
    }
}
/// Given the source split into lines and the 0-indexed line of a definition
/// (`def_idx`), return the 0-indexed line where the item "begins" for preview
/// anchoring: the first line of an *immediately preceding* Julia docstring if
/// one is attached, else `def_idx` itself. Handles multi-line `"""…"""`,
/// single-line `"""…"""`, and single-quoted `"…"` docstrings. Heuristic and
/// deliberately conservative — a blank line above, a comment, or anything
/// ambiguous falls back to `def_idx`, so anchoring never lands somewhere
/// surprising. (Documenter/Base convention: the docstring sits directly above
/// the definition with no blank line between, which is what we key off.)
pub(super) fn item_anchor_line(lines: &[&str], def_idx: usize) -> usize {
    if def_idx == 0 {
        return 0;
    }
    let above = lines[def_idx - 1].trim();
    if above.is_empty() {
        return def_idx; // blank line between -> no attached docstring
    }
    // Single-line triple-quoted docstring: `"""text"""`
    if above.starts_with("\"\"\"") && above.len() > 3 && above.ends_with("\"\"\"") {
        return def_idx - 1;
    }
    // Multi-line docstring: the line above is its closing `"""`. Walk up to
    // the line that opens it (first line, scanning up, whose trim starts with
    // `"""`).
    if above.ends_with("\"\"\"") {
        let mut i = def_idx - 1;
        while i > 0 {
            i -= 1;
            if lines[i].trim_start().starts_with("\"\"\"") {
                return i;
            }
        }
        return 0;
    }
    // Single-line single-quoted docstring: `"text"`
    if above.len() > 1 && above.starts_with('"') && above.ends_with('"') {
        return def_idx - 1;
    }
    def_idx
}

impl MarkdownPreview {
    pub fn new(
        font_system: &mut FontSystem,
        source: &str,
        width: f32,
        height: f32,
        scale: f32,
        math_metrics: &MathMetricsMap,
        figure_metrics: &FigureMetricsMap,
        highlight: &crate::ui::preview::markdown::highlight::HighlightService,
        token_cache: &std::collections::HashMap<
            (String, u64),
            Vec<crate::net::transport::MarkdownToken>,
        >,
    ) -> Self {
        let mut buffer = Buffer::new(
            font_system,
            Metrics::new(BODY_SIZE * scale, BODY_LINE_H * scale),
        );
        // Width bounds wrapping; height is left unbounded (None) so
        // `LayoutRunIter` doesn't stop at the visible-rect height —
        // content past the rect needs to be laid out for scroll to
        // reveal it. Render-time clipping happens via the TextArea's
        // bounds, not the buffer's height.
        let _ = height;
        buffer.set_size(font_system, Some(width.max(1.0)), None);

        let arena = Arena::new();
        let mut opts = Options::default();
        // GFM extensions — match VS Code's markdown-it baseline so
        // GitHub-style markdown round-trips visually. `math_dollars` is
        // a comrak addition not in the GFM spec but standard in
        // Documenter.jl / Julia ecosystems and required for our math
        // pane. `tagfilter` is left off because we don't render raw
        // HTML anyway.
        opts.extension.math_dollars = true;
        opts.extension.table = true;
        opts.extension.strikethrough = true;
        opts.extension.tasklist = true;
        opts.extension.autolink = true;
        opts.extension.footnotes = true;
        // Recognise a leading `---` YAML block as front matter so it's
        // parsed into a (non-rendered) FrontMatter node instead of a setext
        // H2. Required for Quarto `.qmd` (always has a YAML header) and
        // harmless for plain `.md`. The walk's catch-all arm renders nothing
        // for the FrontMatter node, so the header is cleanly skipped.
        opts.extension.front_matter_delimiter = Some("---".to_string());
        let root = parse_document(&arena, source, &opts);

        let mut spans: Vec<(String, Attrs<'static>)> = Vec::new();
        let mut media_blocks: Vec<MediaBlock> = Vec::new();
        let mut walk_state = WalkState {
            block_counter: 0,
            block_sources: Vec::new(),
            highlight,
            token_cache,
            pending_token_fences: Vec::new(),
        };
        let mut root_ctx = Ctx::default();
        root_ctx.scale = scale;
        walk(
            root,
            root_ctx,
            &mut spans,
            &mut media_blocks,
            math_metrics,
            figure_metrics,
            &mut walk_state,
        );

        let default_attrs = Attrs::new()
            .family(Family::SansSerif)
            .metrics(Metrics::new(BODY_SIZE * scale, BODY_LINE_H * scale));
        let span_iter = spans.iter().map(|(s, a)| (s.as_str(), *a));
        buffer.set_rich_text(font_system, span_iter, default_attrs, Shaping::Advanced);
        buffer.shape_until_scroll(font_system, false);

        Self {
            buffer,
            _spans: spans,
            scale,
            body_line_h: BODY_LINE_H,
            media_blocks,
            code_block_sources: walk_state.block_sources,
            pending_token_fences: walk_state.pending_token_fences,
        }
    }

    /// Plain monospace buffer — code, data, log, anything that isn't
    /// markdown. Skips the comrak walk; the whole `source` is one span
    /// in the default sans... wait, monospace family and metrics.
    pub fn new_plain(
        font_system: &mut FontSystem,
        source: &str,
        width: f32,
        scale: f32,
    ) -> Self {
        let metrics = Metrics::new(BODY_SIZE * scale, CODE_LINE_H * scale);
        let mut buffer = Buffer::new(font_system, metrics);
        buffer.set_size(font_system, Some(width.max(1.0)), None);
        let attrs = Attrs::new().family(Family::Monospace).metrics(metrics);
        buffer.set_text(font_system, &normalize_newlines(source), attrs, Shaping::Advanced);
        buffer.shape_until_scroll(font_system, false);
        Self {
            buffer,
            _spans: Vec::new(),
            scale,
            body_line_h: CODE_LINE_H,
            media_blocks: Vec::new(),
            code_block_sources: Vec::new(),
            pending_token_fences: Vec::new(),
        }
    }

    /// Plain monospace buffer like `new_plain`, but each span carries a
    /// `selected` flag — selected spans render in an accent foreground so the
    /// concept editor can show a Shift+motion selection without a cell grid.
    /// Concatenated span text is the rendered string (same composition as
    /// `new_plain`: header, body+cursor, footer).
    pub fn new_plain_spans(
        font_system: &mut FontSystem,
        spans: &[(String, bool)],
        width: f32,
        scale: f32,
    ) -> Self {
        let metrics = Metrics::new(BODY_SIZE * scale, CODE_LINE_H * scale);
        let mut buffer = Buffer::new(font_system, metrics);
        buffer.set_size(font_system, Some(width.max(1.0)), None);
        // Amber selection tint — distinct from the default body colour and
        // from anything the plain-rendered body might contain.
        let sel_color = Color::rgb(0xFF, 0xD7, 0x4A);
        let owned: Vec<(String, Attrs<'static>)> = spans
            .iter()
            .map(|(text, selected)| {
                let mut a = Attrs::new().family(Family::Monospace).metrics(metrics);
                if *selected {
                    a = a.color(sel_color);
                }
                (normalize_newlines(text).into_owned(), a)
            })
            .collect();
        let default_attrs = Attrs::new().family(Family::Monospace).metrics(metrics);
        let iter = owned.iter().map(|(s, a)| (s.as_str(), *a));
        buffer.set_rich_text(font_system, iter, default_attrs, Shaping::Advanced);
        buffer.shape_until_scroll(font_system, false);
        Self {
            buffer,
            _spans: Vec::new(),
            scale,
            body_line_h: CODE_LINE_H,
            media_blocks: Vec::new(),
            code_block_sources: Vec::new(),
            pending_token_fences: Vec::new(),
        }
    }

    /// Tokenised monospace buffer — `.jl` source via JuliaSource's
    /// `application/vnd.sot.tokens+json` mime, one cosmic-text span
    /// per token. Per-kind colours are applied via `Attrs::color`;
    /// "text" / "ident" / unknown kinds fall through to the default
    /// pane colour. Concatenated span text must reproduce the file
    /// (verified live on the dev backend).
    pub fn new_tokens(
        font_system: &mut FontSystem,
        spans: &[(String, String)],
        width: f32,
        scale: f32,
    ) -> Self {
        let metrics = Metrics::new(BODY_SIZE * scale, CODE_LINE_H * scale);
        let mut buffer = Buffer::new(font_system, metrics);
        buffer.set_size(font_system, Some(width.max(1.0)), None);

        let owned: Vec<(String, Attrs<'static>)> = spans
            .iter()
            .map(|(text, kind)| {
                let mut a = Attrs::new().family(Family::Monospace).metrics(metrics);
                if let Some(c) = crate::ui::preview::markdown::highlight::color_for_scope(kind) {
                    a = a.color(c);
                }
                (normalize_newlines(text).into_owned(), a)
            })
            .collect();

        let default_attrs = Attrs::new().family(Family::Monospace).metrics(metrics);
        let iter = owned.iter().map(|(s, a)| (s.as_str(), *a));
        buffer.set_rich_text(font_system, iter, default_attrs, Shaping::Advanced);
        buffer.shape_until_scroll(font_system, false);

        Self {
            buffer,
            _spans: owned,
            scale,
            body_line_h: CODE_LINE_H,
            media_blocks: Vec::new(),
            code_block_sources: Vec::new(),
            pending_token_fences: Vec::new(),
        }
    }

    pub fn resize(&mut self, font_system: &mut FontSystem, width: f32, height: f32) {
        // Same reasoning as in `new`: height stays None so content
        // past the visible rect gets laid out and is available for
        // scroll to reveal.
        let _ = height;
        self.buffer
            .set_size(font_system, Some(width.max(1.0)), None);
        self.buffer.shape_until_scroll(font_system, false);
    }

    /// Body line-height in physical pixels — the unit row-based scroll
    /// maths uses. Heading lines are taller in display, but the scroll
    /// step stays body-sized so wheel-rows feel consistent regardless
    /// of which heading is at the top of the viewport.
    pub fn line_height(&self) -> f32 {
        self.body_line_h * self.scale
    }

    /// Bottom, px below the viewport top, of the last laid-out line that fits
    /// whole in a `visible_px`-tall viewport scrolled `scroll_px` in: the chrome
    /// clips there so no row is drawn sliced. `visible_px` when none fits whole.
    pub fn whole_line_bottom(&self, scroll_px: f32, visible_px: f32) -> f32 {
        let fit = self
            .buffer
            .layout_runs()
            .map(|r| r.line_top + r.line_height - scroll_px)
            .take_while(|&bot| bot <= visible_px + 0.5)
            .fold(0.0_f32, f32::max);
        if fit > 0.0 {
            fit
        } else {
            visible_px
        }
    }

    /// Body em (font size) in physical pixels. Used by the math-paint
    /// pass to size SVG bitmaps relative to body text: a MathJax SVG
    /// reports `width="N ex"` and the pixel width is `N * ex_factor *
    /// body_em`.
    pub fn body_em(&self) -> f32 {
        BODY_SIZE * self.scale
    }

    /// Scroll offset (in `body_line_h` units — what `preview_scroll` stores)
    /// that puts the item defined at `def_line` (1-indexed source line) at the
    /// top of the pane, anchored to its docstring start when one is attached.
    /// Walks the shaped buffer for the target line's layout run and converts
    /// its pixel top to body-line units. Returns 0 if the line is out of range
    /// or hasn't been laid out; the caller's scroll clamp keeps items near EOF
    /// on-screen. Only meaningful for the code shapers (`new_plain` /
    /// `new_tokens`), where one buffer line == one source line.
    pub fn anchor_scroll_for_def_line(&self, def_line: usize) -> u16 {
        if def_line == 0 {
            return 0;
        }
        let lines: Vec<&str> = self.buffer.lines.iter().map(|l| l.text()).collect();
        if def_line > lines.len() {
            return 0;
        }
        let target = item_anchor_line(&lines, def_line - 1);
        let line_h = self.line_height().max(1.0);
        let top = self
            .buffer
            .layout_runs()
            .find(|r| r.line_i == target)
            .map(|r| r.line_top)
            .unwrap_or(0.0);
        (top / line_h).round().max(0.0) as u16
    }

    /// Total pixel height of the laid-out document, accounting for
    /// per-line `line_height_opt` overrides emitted by tall placeholder
    /// spans (display math, embedded figures). `total_visual_lines`
    /// alone undercounts when figures stretch a single BufferLine to
    /// many body-line heights, leaving `preview_scroll`'s clamp short
    /// of the actual bottom. `body_line_h` is the fallback for any
    /// LayoutLine that didn't carry its own override.
    ///
    /// Unshaped BufferLines contribute exactly `body_line_h` (one body
    /// line, conservative — matches `total_visual_lines`'s 1-line
    /// pessimism). The next shape pass populates them and a follow-up
    /// call yields the precise total.
    pub fn total_visual_pixels(&self, body_line_h: f32) -> f32 {
        self.buffer
            .lines
            .iter()
            .map(|line| match line.layout_opt() {
                Some(layout) if !layout.is_empty() => layout
                    .iter()
                    .map(|ll| ll.line_height_opt.unwrap_or(body_line_h))
                    .sum::<f32>(),
                _ => body_line_h,
            })
            .sum()
    }

    /// Per-contiguous-code-run rects in *buffer-local* coords as
    /// `(x, y, w, h)`. The chrome offsets by the markdown pane origin
    /// and the scroll to get screen rects, then paints a bg quad under
    /// each one before the text layer renders. Glyphs are marked code
    /// via `CODE_GLYPH_META` in their Attrs metadata; consecutive
    /// code-glyphs in the same `LayoutRun` merge into one rect, with
    /// breaks across non-code runs or line boundaries. Inline `<code>`
    /// and fenced `CodeBlock` both flow through this path.
    /// Per-contiguous-strike-run rects in *buffer-local* coords. Same
    /// shape as `code_glyph_rects` but the chrome paints these as a
    /// thin (1–2 px) horizontal quad at the glyph's x-height midline
    /// rather than a full-cell panel. Driven by `STRIKE_GLYPH_FLAG`.
    pub fn strike_glyph_rects(&self) -> Vec<(f32, f32, f32, f32)> {
        let mut rects = Vec::new();
        for run in self.buffer.layout_runs() {
            let top = run.line_top;
            let h = run.line_height;
            let mut current: Option<(f32, f32)> = None;
            for g in run.glyphs.iter() {
                let is_strike = (g.metadata & STRIKE_GLYPH_FLAG) != 0;
                let gx = g.x;
                let gw = g.w.max(0.0);
                if is_strike {
                    let span = current
                        .map(|(s, _)| (s, gx + gw))
                        .unwrap_or((gx, gx + gw));
                    current = Some(span);
                } else if let Some((s, e)) = current.take() {
                    rects.push((s, top, e - s, h));
                }
            }
            if let Some((s, e)) = current.take() {
                rects.push((s, top, e - s, h));
            }
        }
        rects
    }

    /// Per-LayoutRun rects for any glyphs carrying `CODE_BLOCK_FLAG`,
    /// returned in buffer-local coords. One rect per line that
    /// contains any block-code glyph — the chrome expands each to the
    /// full preview-pane width when rendering so the panel reads as a
    /// proper block instead of a text-width pill. `y` is the run's
    /// `line_top`, `h` is the run's `line_height`; `x` and `w` cover
    /// the line's full layout width (rendered range) so the per-line
    /// rect already maps to one painted strip.
    pub fn code_block_line_rects(&self) -> Vec<(f32, f32, f32, f32)> {
        let mut rects = Vec::new();
        for run in self.buffer.layout_runs() {
            let has_block = run
                .glyphs
                .iter()
                .any(|g| (g.metadata & CODE_BLOCK_FLAG) != 0);
            if !has_block {
                continue;
            }
            rects.push((0.0, run.line_top, run.line_w, run.line_height));
        }
        rects
    }

    /// One `(y_top, height)` per fenced code block — covers every
    /// rendered line in the block as a single continuous strip, including
    /// any blank lines inside the fence (which have no glyphs of their
    /// own and would otherwise leave a visible gap in the panel). Grouped
    /// by the block id packed into `Attrs::metadata` above
    /// `CODE_BLOCK_ID_SHIFT`. Chrome paints these at full markdown-pane
    /// width.
    pub fn code_block_rects(&self) -> Vec<(f32, f32)> {
        // (top, bottom) per block_id, in first-seen order.
        let mut order: Vec<usize> = Vec::new();
        let mut bounds: HashMap<usize, (f32, f32)> = HashMap::new();
        for run in self.buffer.layout_runs() {
            let mut block_id: usize = 0;
            for g in run.glyphs.iter() {
                if (g.metadata & CODE_BLOCK_FLAG) != 0 {
                    block_id = g.metadata >> CODE_BLOCK_ID_SHIFT;
                    if block_id != 0 {
                        break;
                    }
                }
            }
            if block_id == 0 {
                continue;
            }
            let top = run.line_top;
            let bot = run.line_top + run.line_height;
            match bounds.get_mut(&block_id) {
                Some(entry) => {
                    if top < entry.0 {
                        entry.0 = top;
                    }
                    if bot > entry.1 {
                        entry.1 = bot;
                    }
                }
                None => {
                    order.push(block_id);
                    bounds.insert(block_id, (top, bot));
                }
            }
        }
        order
            .into_iter()
            .filter_map(|id| bounds.get(&id).copied())
            .map(|(top, bot)| (top, bot - top))
            .collect()
    }

    /// Inline-only code rects — same shape as before but now excludes
    /// fenced-block glyphs (those land in `code_block_line_rects`
    /// instead). Splitting the two lets the chrome render inline as a
    /// text-sized pill and block as a full-pane-width panel.
    pub fn code_glyph_rects(&self) -> Vec<(f32, f32, f32, f32)> {
        let mut rects = Vec::new();
        for run in self.buffer.layout_runs() {
            let top = run.line_top;
            let h = run.line_height;
            // (x_start, x_end) of the in-progress code run; None when
            // we're between code spans.
            let mut current: Option<(f32, f32)> = None;
            for g in run.glyphs.iter() {
                // Inline-only: block code is reported via
                // `code_block_line_rects` instead so the chrome can
                // expand to pane width.
                let is_code = (g.metadata & CODE_GLYPH_FLAG) != 0
                    && (g.metadata & CODE_BLOCK_FLAG) == 0;
                let gx = g.x;
                let gw = g.w.max(0.0);
                if is_code {
                    let span = current
                        .map(|(s, _)| (s, gx + gw))
                        .unwrap_or((gx, gx + gw));
                    current = Some(span);
                } else if let Some((s, e)) = current.take() {
                    rects.push((s, top, e - s, h));
                }
            }
            if let Some((s, e)) = current.take() {
                rects.push((s, top, e - s, h));
            }
        }
        rects
    }
}
