//! `walk`: the comrak AST becomes (text, attrs) spans, media blocks and pending fences.

use super::*;

pub(super) fn walk<'a, 'b>(
    node: &'a AstNode<'a>,
    ctx: Ctx,
    out: &mut Vec<(String, Attrs<'static>)>,
    media: &mut Vec<MediaBlock>,
    metrics: &MathMetricsMap,
    figures: &FigureMetricsMap,
    state: &mut WalkState<'b>,
) {
    let nv = node.data.borrow().value.clone();
    match nv {
        NodeValue::Document => {
            for ch in node.children() {
                walk(ch, ctx, out, media, metrics, figures, state);
            }
        }
        NodeValue::Heading(h) => {
            let mut c = ctx;
            c.heading = h.level;
            for ch in node.children() {
                walk(ch, c, out, media, metrics, figures, state);
            }
            push_break(out, "\n\n", ctx.scale);
        }
        NodeValue::Paragraph => {
            for ch in node.children() {
                walk(ch, ctx, out, media, metrics, figures, state);
            }
            // Comrak emits one Paragraph per list-item line even for
            // "tight" lists; double-newline here gives a blank line
            // between every entry. Inside an Item, collapse to a single
            // line break so the list renders compactly.
            push_break(out, if ctx.inside_item { "\n" } else { "\n\n" }, ctx.scale);
        }
        NodeValue::Text(s) => {
            // Strike is rendered as a 1-px overlay quad in the chrome
            // (paint_strike_rects in gpu.rs); the text itself stays
            // untouched so the combining-char fallbacks (U+0335 / U+0336)
            // are gone and the strike sits at a font-metric-correct
            // y-position regardless of which font fontdb picks.
            out.push((s, attrs_for(ctx)));
        }
        NodeValue::Emph => {
            let mut c = ctx;
            c.italic = true;
            for ch in node.children() {
                walk(ch, c, out, media, metrics, figures, state);
            }
        }
        NodeValue::Strong => {
            let mut c = ctx;
            c.bold = true;
            for ch in node.children() {
                walk(ch, c, out, media, metrics, figures, state);
            }
        }
        NodeValue::Strikethrough => {
            let mut c = ctx;
            c.strike = true;
            for ch in node.children() {
                walk(ch, c, out, media, metrics, figures, state);
            }
        }
        NodeValue::Code(code) => {
            let mut c = ctx;
            c.code = true;
            out.push((code.literal, attrs_for(c)));
        }
        NodeValue::CodeBlock(cb) => {
            // Fenced code block — full-width panel via CODE_BLOCK_FLAG
            // on every glyph; chrome expands the rect to the pane
            // width before rendering the slate bg quad. Per-line
            // leading space gives the first character left padding.
            //
            // When the fence info names a language we recognise
            // (currently just `julia` / `jl`), tokenise the literal
            // and emit per-token spans with per-kind colours so the
            // block reads as syntax-highlighted code rather than
            // uniform peach text. Other languages fall through to the
            // default peach tint applied in `attrs_for`.
            state.block_counter = state.block_counter.saturating_add(1);
            state.block_sources.push(cb.literal.clone());
            let mut c = ctx;
            c.code = true;
            c.code_block = true;
            c.code_block_id = state.block_counter;
            push_break(out, "\n", ctx.scale);
            push_block_margin(out, ctx.scale);
            // Normalise the fence info string to its language alias
            // — strips any space-separated tail like `julia title="..."`
            // so the dispatcher only sees the first token. Empty info
            // strings fall through to plain rendering.
            let info_raw = cb.info.trim().to_ascii_lowercase();
            let lang_alias: &str = info_raw
                .split(|c: char| c.is_whitespace())
                .next()
                .unwrap_or("");
            // Re-assemble the block with a leading-space gutter on
            // every line so the panel has breathing room on the left
            // (the bg quad pads on the right at render time).
            let mut padded = String::with_capacity(cb.literal.len() + 8);
            for line in cb.literal.split_inclusive('\n') {
                padded.push(' ');
                padded.push_str(line);
            }
            // Tree-sitter base layer — non-overlapping scope spans in
            // source order. Synchronous + always-available; paints
            // keywords / strings / numbers / comments correctly for
            // any registered language.
            let base_spans = state.highlight.highlight(lang_alias, &padded);
            // Backend semantic-overlay lookup — only Julia today; the
            // overlay wins within its byte range, the base fills the
            // rest. Miss → push the fence into `pending_token_fences`
            // so the caller fires `markdown.tokenize`; the *next*
            // redraw (after the reply) gets the overlay.
            let overlay_key_lang =
                if matches!(lang_alias, "julia" | "jl") { Some("julia") } else { None };
            let overlay_spans: &[crate::transport::MarkdownToken] =
                if let Some(lk) = overlay_key_lang {
                    let h = hash_source(&padded);
                    let key = (lk.to_string(), h);
                    match state.token_cache.get(&key) {
                        Some(v) => v.as_slice(),
                        None => {
                            state.pending_token_fences.push((
                                lk.to_string(),
                                h,
                                padded.clone(),
                            ));
                            &[]
                        }
                    }
                } else {
                    &[]
                };
            let merged = merge_highlight_spans(&base_spans, overlay_spans);
            if merged.is_empty() {
                out.push((padded, attrs_for(c)));
            } else {
                let mut cursor = 0usize;
                for (s, e, scope) in &merged {
                    if *s > cursor {
                        out.push((padded[cursor..*s].to_string(), attrs_for(c)));
                    }
                    let mut a = attrs_for(c);
                    if let Some(col) =
                        crate::preview::highlight::color_for_scope(scope)
                    {
                        a = a.color(col);
                    }
                    out.push((padded[*s..*e].to_string(), a));
                    cursor = *e;
                }
                if cursor < padded.len() {
                    out.push((padded[cursor..].to_string(), attrs_for(c)));
                }
            }
            push_block_margin(out, ctx.scale);
            push_break(out, "\n", ctx.scale);
        }
        NodeValue::List(nl) => {
            let mut c = ctx;
            c.list_depth = ctx.list_depth.saturating_add(1);
            c.list_ordered = nl.list_type == ListType::Ordered;
            // Comrak's `start` is the user-supplied first ordinal (1 for
            // `1. ...`, 7 for `7. ...`). Fall back to 1 for malformed
            // input so we never render `0.` or wrap into usize::MAX.
            let start = nl.start.max(1);
            // Enumerate so each Item knows its 1-based offset from the
            // list's `start`. Mutating per-iteration is fine — `c` is
            // already a local copy.
            for (idx, ch) in node.children().enumerate() {
                c.list_number = start + idx;
                walk(ch, c, out, media, metrics, figures, state);
            }
            push_break(out, "\n", ctx.scale);
        }
        NodeValue::Item(_) => {
            let indent = "  ".repeat(ctx.list_depth.saturating_sub(1) as usize);
            let marker = if ctx.list_ordered {
                format!("{}. ", ctx.list_number)
            } else {
                "• ".to_string()
            };
            out.push((format!("{indent}  {marker}"), attrs_for(ctx)));
            let mut c = ctx;
            c.inside_item = true;
            for ch in node.children() {
                walk(ch, c, out, media, metrics, figures, state);
            }
        }
        NodeValue::TaskItem(checked) => {
            let indent = "  ".repeat(ctx.list_depth.saturating_sub(1) as usize);
            let mark = if checked.is_some() { "☑" } else { "☐" };
            out.push((format!("{indent}  {mark} "), attrs_for(ctx)));
            let mut c = ctx;
            c.inside_item = true;
            for ch in node.children() {
                walk(ch, c, out, media, metrics, figures, state);
            }
        }
        NodeValue::BlockQuote => {
            // VS Code-style left gutter: each level adds `│ `, then the
            // child blocks render normally indented behind it. `│`
            // (U+2502 BOX DRAWINGS LIGHT VERTICAL) chosen over `▎`
            // (U+258E LEFT VERTICAL BLOCK) because the latter renders
            // as tofu in Cascadia/Consolas fallback. Gutter is dimmed
            // so it reads as a quote indicator without competing with
            // the quoted text. Italic applied to the quoted prose.
            let mut c = ctx;
            c.quote_depth = ctx.quote_depth.saturating_add(1);
            c.italic = true;
            for ch in node.children() {
                let gutter: String = (0..c.quote_depth).map(|_| "│ ").collect();
                out.push((
                    gutter,
                    attrs_for(c).color(Color::rgb(102, 102, 102)),
                ));
                walk(ch, c, out, media, metrics, figures, state);
            }
        }
        NodeValue::ThematicBreak => {
            push_break(out, "\n", ctx.scale);
            // 60 box-drawing horizontals — long enough to span the
            // preview pane at typical widths, soft-wrap is acceptable.
            out.push(("─".repeat(60), attrs_for(ctx)));
            push_break(out, "\n\n", ctx.scale);
        }
        NodeValue::Link(link) => {
            // Render link children normally but tag with a visible
            // color so the user sees that text is a link. cosmic-text
            // Attrs supports per-span color; we use the VS Code-Dark+
            // anchor blue. Underline waits on the bg-quad pipeline.
            let _ = link.url;
            for ch in node.children() {
                let mut buf: Vec<(String, Attrs<'static>)> = Vec::new();
                walk(ch, ctx, &mut buf, media, metrics, figures, state);
                for (s, a) in buf {
                    out.push((s, a.color(Color::rgb(59, 142, 234))));
                }
            }
        }
        NodeValue::Table(_) => {
            // Build the box-drawing block at natural width; the chrome
            // will host it in a separate cosmic-text buffer so wide
            // tables don't soft-wrap against the preview pane (that
            // wrap destroys the box-drawing column alignment — the
            // whole reason for Path 1 of (e)).
            let Some((rendered, n_lines)) = build_table_block(node) else {
                return;
            };
            let font_px = BODY_SIZE * TABLE_FONT_SCALE * ctx.scale;
            let line_h_px = BODY_LINE_H * TABLE_FONT_SCALE * ctx.scale;
            let reserved_h = (n_lines as f32) * line_h_px;
            push_break(out, "\n", ctx.scale);
            // FFFC placeholder mirrors the math/figure pattern: zero-
            // alpha colour so the glyph doesn't bleed through, metric
            // override sets the line height to the table's full reserved
            // vertical span so vertical scroll past the table works
            // through the existing `preview_scroll_px` math without
            // chrome-side awareness of table block geometry.
            let placeholder_attrs = Attrs::new()
                .family(Family::SansSerif)
                .color(Color::rgba(0, 0, 0, 0))
                .metrics(Metrics::new(BODY_SIZE * ctx.scale, reserved_h));
            out.push((MATH_PLACEHOLDER.to_string(), placeholder_attrs));
            push_break(out, "\n\n", ctx.scale);
            media.push(MediaBlock::Table {
                rendered,
                n_lines,
                line_h_px,
                font_px,
            });
        }
        NodeValue::SoftBreak => {
            out.push((" ".to_string(), attrs_for(ctx)));
        }
        NodeValue::LineBreak => {
            push_break(out, "\n", ctx.scale);
        }
        NodeValue::Math(m) => {
            let cached = metrics.get(&(m.literal.clone(), m.display_math)).copied();
            if m.display_math {
                // $$...$$ — reserve a placeholder line whose height is
                // the SVG's natural pixel height (once cached) or a
                // conservative default otherwise. The chrome locates
                // FFFC in the layout and paints the pre-rendered SVG
                // centred over it.
                push_break(out, "\n", ctx.scale);
                let line_h = cached
                    .map(|c| c.height_px.max(BODY_LINE_H))
                    .unwrap_or(MATH_BLOCK_H_DEFAULT);
                // Fully transparent color so the OBJECT REPLACEMENT
                // CHARACTER glyph itself never bleeds through next to
                // the SVG we overpaint at this rect.
                let placeholder_attrs = Attrs::new()
                    .family(Family::SansSerif)
                    .color(Color::rgba(0, 0, 0, 0))
                    .metrics(Metrics::new(BODY_SIZE * ctx.scale, line_h * ctx.scale));
                out.push((MATH_PLACEHOLDER.to_string(), placeholder_attrs));
                push_break(out, "\n\n", ctx.scale);
                media.push(MediaBlock::Math {
                    latex: m.literal,
                    display: true,
                });
            } else {
                // Inline `$...$` — emit FFFC (paint anchor, body em so
                // shaping is undisturbed) followed by enough U+2003 EM
                // SPACE characters to reserve the SVG's natural width.
                // Mixing one custom-size em-space caused cosmic-text to
                // give that line a giant ascent (font_size, not
                // line_height, drives ascent) and stack the next line on
                // top of it. So all spacer ems stay at body font_size:
                // total advance is rounded UP to the next body em,
                // worst case ~1 em of trailing whitespace per inline
                // equation — acceptable; tightening further needs a
                // glyph-advance probe of FFFC + body em-space, future
                // work.
                let body_em_px = BODY_SIZE * ctx.scale;
                let line_h_px = BODY_LINE_H * ctx.scale;
                let svg_w = match cached {
                    Some(c) => (c.width_px * ctx.scale).max(1.0),
                    None => body_em_px * INLINE_MATH_FALLBACK_EM_SPACES as f32,
                };
                // Account for FFFC's own ~0.7 em advance so the
                // em-space count doesn't double-reserve. Floor + clamp
                // so a very short equation (e.g. `$x$`) still gets at
                // least one em of spacer between FFFC and following
                // text — looks better than the SVG abutting the next
                // glyph.
                let remaining = (svg_w - body_em_px * 0.7).max(body_em_px);
                let em_spaces = (remaining / body_em_px.max(1.0)).ceil().max(1.0) as usize;
                let placeholder_attrs = Attrs::new()
                    .family(Family::SansSerif)
                    .color(Color::rgba(0, 0, 0, 0))
                    .metrics(Metrics::new(body_em_px, line_h_px));
                let mut s = String::with_capacity(1 + em_spaces * 3);
                s.push(MATH_PLACEHOLDER);
                for _ in 0..em_spaces {
                    s.push('\u{2003}');
                }
                out.push((s, placeholder_attrs));
                media.push(MediaBlock::Math {
                    latex: m.literal,
                    display: false,
                });
            }
        }
        NodeValue::Image(link) => {
            // `![alt](url)` — reserve a block-level placeholder line.
            // The chrome fetches `link.url` (resolved relative to the
            // current markdown file) and paints the decoded bitmap over
            // the FFFC. Children of an Image node are the alt text;
            // we deliberately don't recurse into them so the alt text
            // doesn't show up next to the rendered image. While the
            // bytes are in flight the rect stays blank — the user
            // gets a visible reservation rather than reflowing text
            // when the image lands.
            //
            // Images that will NEVER paint — remote/data URLs (the
            // chrome only fetches workspace-local files) and terminal
            // failures (0-size metrics from the chrome's failed set) —
            // collapse to one compact dim line instead: README banner
            // images and badges otherwise reserve a screenful of
            // permanently empty FIGURE_BLOCK_H_DEFAULT boxes.
            let mut alt = String::new();
            for ch in node.children() {
                if let NodeValue::Text(s) = &ch.data.borrow().value {
                    alt.push_str(s);
                }
            }
            let cached = figures.get(&link.url).copied();
            let never_paints = link.url.contains("://")
                || link.url.starts_with("data:")
                || cached.is_some_and(|c| c.height_px <= 0.0);
            if never_paints {
                let mut label = alt.trim().to_string();
                if label.is_empty() {
                    label = link
                        .url
                        .rsplit('/')
                        .next()
                        .unwrap_or("image")
                        .split('?')
                        .next()
                        .unwrap_or("image")
                        .to_string();
                }
                if label.len() > 60 {
                    label.truncate(60);
                    label.push('…');
                }
                // Single \n breaks (not the figure path's \n\n): a badge
                // row collapses to adjacent one-liners, not a column of
                // double-spaced gaps.
                push_break(out, "\n", ctx.scale);
                out.push((
                    format!("⟦image: {label}⟧"),
                    Attrs::new()
                        .family(Family::SansSerif)
                        .color(Color::rgb(102, 102, 102))
                        .metrics(Metrics::new(
                            BODY_SIZE * ctx.scale,
                            BODY_LINE_H * ctx.scale,
                        )),
                ));
                push_break(out, "\n", ctx.scale);
                // No MediaBlock: the chrome must not pair a paint rect
                // (or fire a fetch) for a figure that can't load —
                // FFFC runs and media_blocks pair by index.
                return;
            }
            push_break(out, "\n", ctx.scale);
            let line_h = cached
                .map(|c| c.height_px.max(BODY_LINE_H))
                .unwrap_or(FIGURE_BLOCK_H_DEFAULT);
            let placeholder_attrs = Attrs::new()
                .family(Family::SansSerif)
                .color(Color::rgba(0, 0, 0, 0))
                .metrics(Metrics::new(BODY_SIZE * ctx.scale, line_h * ctx.scale));
            out.push((MATH_PLACEHOLDER.to_string(), placeholder_attrs));
            push_break(out, "\n\n", ctx.scale);
            media.push(MediaBlock::Figure {
                url: link.url,
                alt,
            });
        }
        _ => {
            for ch in node.children() {
                walk(ch, ctx, out, media, metrics, figures, state);
            }
        }
    }
}
