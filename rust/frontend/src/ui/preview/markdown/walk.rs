//! `walk`: the comrak AST becomes (text, attrs) spans, media blocks and pending fences.

use super::*;

#[path = "walk_code.rs"]
mod code;
#[path = "walk_media.rs"]
mod media;

use code::walk_code_block;
use media::{walk_image, walk_math, walk_table};

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
        NodeValue::CodeBlock(cb) => walk_code_block(ctx, cb, out, state),
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
        NodeValue::Table(_) => walk_table(node, ctx, out, media),
        NodeValue::SoftBreak => {
            out.push((" ".to_string(), attrs_for(ctx)));
        }
        NodeValue::LineBreak => {
            push_break(out, "\n", ctx.scale);
        }
        NodeValue::Math(m) => walk_math(ctx, m, out, media, metrics),
        NodeValue::Image(link) => walk_image(node, ctx, link, out, media, figures),
        _ => {
            for ch in node.children() {
                walk(ch, ctx, out, media, metrics, figures, state);
            }
        }
    }
}
