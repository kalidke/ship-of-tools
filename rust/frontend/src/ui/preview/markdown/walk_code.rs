//! A fenced code block in the markdown walk: tree-sitter base spans with the kernel's overlay.

use super::*;

use comrak::nodes::NodeCodeBlock;

pub(super) fn walk_code_block<'b>(ctx: Ctx, cb: NodeCodeBlock, out: &mut Vec<(String, Attrs<'static>)>, state: &mut WalkState<'b>) {
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
