//! Tables, math and images in the markdown walk: each reserves a placeholder glyph and
//! records a MediaBlock the chrome paints.

use super::*;

use comrak::nodes::{NodeLink, NodeMath};

pub(super) fn walk_table<'a>(node: &'a AstNode<'a>, ctx: Ctx, out: &mut Vec<(String, Attrs<'static>)>, media: &mut Vec<MediaBlock>) {
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

pub(super) fn walk_math(ctx: Ctx, m: NodeMath, out: &mut Vec<(String, Attrs<'static>)>, media: &mut Vec<MediaBlock>, metrics: &MathMetricsMap) {
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

pub(super) fn walk_image<'a>(node: &'a AstNode<'a>, ctx: Ctx, link: NodeLink, out: &mut Vec<(String, Attrs<'static>)>, media: &mut Vec<MediaBlock>, figures: &FigureMetricsMap) {
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
