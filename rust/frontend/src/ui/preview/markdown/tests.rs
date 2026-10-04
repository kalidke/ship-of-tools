//! Tests for the markdown preview: walk output, media blocks, front matter and anchor lines.

use super::*;

use cosmic_text::FontSystem;

#[test]
fn whole_line_bottom_drops_a_partial_last_line() {
    let src = (0..50)
        .map(|i| format!("line {i}"))
        .collect::<Vec<_>>()
        .join("\n");
    let p = MarkdownPreview::new_plain(&mut FontSystem::new(), &src, 800.0, 1.0);
    let h = p.line_height();
    assert!((p.whole_line_bottom(0.0, 10.5 * h) - 10.0 * h).abs() < 0.5);
    assert!((p.whole_line_bottom(3.0 * h, 10.5 * h) - 10.0 * h).abs() < 0.5);
    assert_eq!(p.whole_line_bottom(0.0, 0.5 * h), 0.5 * h);
}

fn make_preview(source: &str) -> MarkdownPreview {
    make_preview_with_figures(source, FigureMetricsMap::new())
}

fn make_preview_with_figures(source: &str, figures: FigureMetricsMap) -> MarkdownPreview {
    let mut fs = FontSystem::new();
    let math: MathMetricsMap = HashMap::new();
    let tokens = HashMap::new();
    let highlight = crate::preview::highlight::HighlightService::new()
        .expect("highlight service init");
    MarkdownPreview::new(&mut fs, source, 800.0, 600.0, 1.0, &math, &figures, &highlight, &tokens)
}

fn figure_blocks(p: &MarkdownPreview) -> usize {
    p.media_blocks
        .iter()
        .filter(|b| matches!(b, MediaBlock::Figure { .. }))
        .count()
}

fn layout_height(p: &mut MarkdownPreview) -> f32 {
    p.buffer
        .layout_runs()
        .map(|r| r.line_top + r.line_height)
        .fold(0.0_f32, f32::max)
}

#[test]
fn remote_image_collapses_to_compact_line() {
    // README banner case: remote URLs are never fetched by the
    // chrome, so they must not reserve a FIGURE_BLOCK_H_DEFAULT box
    // (one badge row used to push first content nearly off-screen).
    // Compact path: dim text line, NO MediaBlock::Figure.
    let src = "\
![CI](https://img.shields.io/badge/ci-passing-green)
![](https://example.com/banner.png)

First paragraph.
";
    let mut p = make_preview_with_figures(src, FigureMetricsMap::new());
    assert_eq!(figure_blocks(&p), 0, "remote images must not emit Figure blocks");
    let h = layout_height(&mut p);
    assert!(
        h < FIGURE_BLOCK_H_DEFAULT,
        "two remote images + a paragraph should lay out under one \
         figure reservation, got {h}px"
    );
}

#[test]
fn failed_local_image_collapses_to_compact_line() {
    // Terminal failure (decode error / unresolvable path) is
    // reported by the chrome as 0-size metrics; the walk collapses
    // the reservation instead of holding an empty box forever.
    let mut figures = FigureMetricsMap::new();
    figures.insert(
        "missing.png".to_string(),
        FigureMetrics { width_px: 0.0, height_px: 0.0 },
    );
    let p = make_preview_with_figures("![alt text](missing.png)\n", figures);
    assert_eq!(figure_blocks(&p), 0, "failed image must not emit a Figure block");
}

#[test]
fn pending_local_image_keeps_reservation() {
    // In-flight local images keep the visible block reservation so
    // text doesn't reflow when the bytes land (existing behavior).
    let mut p = make_preview_with_figures("![fig](images/plot.png)\n", FigureMetricsMap::new());
    assert_eq!(figure_blocks(&p), 1, "pending local image keeps its Figure block");
    let h = layout_height(&mut p);
    assert!(
        h >= FIGURE_BLOCK_H_DEFAULT,
        "pending image keeps the default reservation, got {h}px"
    );
}

#[test]
fn table_emits_media_block_not_inline_text() {
    // Path 1 of (e): tables route through `MediaBlock::Table` so
    // the chrome can host them in a separate per-table buffer at
    // natural width. If the walk regresses to in-buffer rendering,
    // wide tables will start soft-wrapping again.
    let src = "\
| Col A | Col B |
|-------|-------|
| x     | y     |
";
    let p = make_preview(src);
    let tables: Vec<&MediaBlock> = p
        .media_blocks
        .iter()
        .filter(|b| matches!(b, MediaBlock::Table { .. }))
        .collect();
    assert_eq!(tables.len(), 1, "exactly one table block expected");
    let MediaBlock::Table { rendered, n_lines, line_h_px, font_px } = tables[0] else {
        unreachable!("filter above guarantees Table variant")
    };
    assert!(*line_h_px > 0.0, "line_h_px must be positive");
    assert!(*font_px > 0.0, "font_px must be positive");
    // Rendered block ends with a `\n` per row and includes a
    // bottom border line. For this fixture: top border, header
    // row, separator, body row, bottom border = 5 lines.
    assert_eq!(*n_lines, 5, "rendered line count");
    assert!(rendered.contains("Col A"), "header text preserved");
    assert!(rendered.contains("┌"), "top-left corner present");
    assert!(rendered.contains("└"), "bottom-left corner present");
    assert!(rendered.contains("═"), "header separator (double-line) present");
    assert_eq!(rendered.lines().count(), 5);
}

#[test]
fn empty_table_skipped() {
    // A degenerate `| |` row with no cells parses as a table with
    // zero columns — the walk should drop it instead of producing
    // a zero-row MediaBlock that the chrome would have to special-
    // case in `ensure_table_buffers`.
    let src = "|  |\n|--|\n";
    let p = make_preview(src);
    // Either no MediaBlock at all, or a one-column degenerate
    // table — but never zero columns.
    for b in &p.media_blocks {
        if let MediaBlock::Table { n_lines, .. } = b {
            assert!(*n_lines > 0, "table block with 0 lines slipped through");
        }
    }
}

#[test]
fn multiple_tables_get_distinct_media_blocks() {
    // Each table on a page is its own MediaBlock — chrome maps them
    // to distinct TableBufferEntry slots in encounter order.
    let src = "\
| A |
|---|
| 1 |

Some text in between.

| B |
|---|
| 2 |
";
    let p = make_preview(src);
    let tables: Vec<&MediaBlock> = p
        .media_blocks
        .iter()
        .filter(|b| matches!(b, MediaBlock::Table { .. }))
        .collect();
    assert_eq!(tables.len(), 2, "one MediaBlock per table");
    let MediaBlock::Table { rendered: r1, .. } = tables[0] else { unreachable!() };
    let MediaBlock::Table { rendered: r2, .. } = tables[1] else { unreachable!() };
    assert!(r1.contains("A"));
    assert!(r2.contains("B"));
    // Source order preserved.
    assert!(!r1.contains("B"));
    assert!(!r2.contains("A"));
}

#[test]
fn qmd_front_matter_is_skipped_not_rendered_as_heading() {
    // A Quarto `.qmd` always leads with a YAML header. With
    // front_matter_delimiter enabled it parses to a FrontMatter node the
    // walk ignores, so the header text must NOT appear in the rendered
    // spans (without it, comrak reads `title: ...` + `---` as a setext H2).
    let src = "---\ntitle: My Report\nformat: html\n---\n\n# Section One\n\nBody paragraph.\n";
    let p = make_preview(src);
    let rendered: String = p._spans.iter().map(|(s, _)| s.as_str()).collect();
    assert!(
        !rendered.contains("title:") && !rendered.contains("format:"),
        "front matter leaked into render: {rendered:?}"
    );
    assert!(rendered.contains("Section One"), "body heading missing");
    assert!(rendered.contains("Body paragraph"), "body text missing");
}

#[test]
fn code_previews_are_tighter_than_prose() {
    // The `.jl` token preview (and plain source/log) must use the
    // dense code line-height, not prose's airy `BODY_LINE_H`, or the
    // preview looks double-spaced. `line_height()` feeds the chrome's
    // scroll clamp + paint step, so it must report the same value the
    // buffer was shaped with — assert both code constructors agree.
    let mut fs = FontSystem::new();
    let scale = 1.0;
    let prose = make_preview("hello world");
    let plain = MarkdownPreview::new_plain(&mut fs, "x = 1\ny = 2\n", 800.0, scale);
    let tokens = MarkdownPreview::new_tokens(
        &mut fs,
        &[("x".into(), "variable".into()), (" = 1".into(), "text".into())],
        800.0,
        scale,
    );

    assert_eq!(prose.line_height(), BODY_LINE_H * scale, "prose stays airy");
    assert_eq!(plain.line_height(), CODE_LINE_H * scale, "plain source is dense");
    assert_eq!(tokens.line_height(), CODE_LINE_H * scale, ".jl tokens are dense");
    assert!(
        plain.line_height() < prose.line_height(),
        "code must be tighter than prose ({} !< {})",
        plain.line_height(),
        prose.line_height(),
    );
}

#[test]
fn normalize_newlines_collapses_crlf() {
    use std::borrow::Cow;
    assert_eq!(normalize_newlines("a\r\nb\r\nc").as_ref(), "a\nb\nc");
    assert_eq!(normalize_newlines("a\rb").as_ref(), "a\nb", "lone CR -> LF");
    assert_eq!(normalize_newlines("a\nb").as_ref(), "a\nb", "LF untouched");
    assert!(
        matches!(normalize_newlines("plain text"), Cow::Borrowed(_)),
        "no CR -> borrow, no allocation",
    );
}

#[test]
fn crlf_source_does_not_double_space() {
    // A CRLF (`\r\n`) file must shape to the same number of visual lines
    // as the LF equivalent. cosmic-text treats a stray `\r` as its own
    // line break, so without normalization a CRLF source rendered a
    // blank line between every line (the "double-spaced code" bug, only
    // visible for CRLF-checked-out repos like RJTrack). Guards both code
    // constructors.
    let mut fs = FontSystem::new();
    let src_lf = "fn a() {\n    let x = 1;\n    x\n}\n";
    let src_crlf = "fn a() {\r\n    let x = 1;\r\n    x\r\n}\r\n";

    let lf = MarkdownPreview::new_plain(&mut fs, src_lf, 800.0, 1.0);
    let crlf = MarkdownPreview::new_plain(&mut fs, src_crlf, 800.0, 1.0);
    assert_eq!(
        crlf.buffer.layout_runs().count(),
        lf.buffer.layout_runs().count(),
        "new_plain: CRLF must shape to the same line count as LF",
    );

    // Same source as a single coalesced whitespace-bearing token span,
    // mirroring how JuliaSource emits `\r\n` runs.
    let tok_lf = MarkdownPreview::new_tokens(&mut fs, &[(src_lf.into(), "text".into())], 800.0, 1.0);
    let tok_crlf =
        MarkdownPreview::new_tokens(&mut fs, &[(src_crlf.into(), "text".into())], 800.0, 1.0);
    assert_eq!(
        tok_crlf.buffer.layout_runs().count(),
        tok_lf.buffer.layout_runs().count(),
        "new_tokens: CRLF must shape to the same line count as LF",
    );
}

#[test]
fn item_anchor_line_detects_docstrings() {
    let multi = vec![
        "\"\"\"",        // 0  opening
        "    foo(x)",     // 1
        "",                // 2
        "Description.",    // 3
        "\"\"\"",        // 4  closing
        "function foo(x)", // 5  <- def
    ];
    assert_eq!(item_anchor_line(&multi, 5), 0, "multi-line docstring -> opening");

    let single = vec!["\"\"\"one liner\"\"\"", "bar() = 1"];
    assert_eq!(item_anchor_line(&single, 1), 0, "single-line triple docstring");

    let sq = vec!["\"short\"", "baz() = 2"];
    assert_eq!(item_anchor_line(&sq, 1), 0, "single-quoted docstring");

    let none = vec!["x = 1", "", "qux() = 3"];
    assert_eq!(item_anchor_line(&none, 2), 2, "blank line above -> def line");
    assert_eq!(item_anchor_line(&none, 0), 0, "first line -> itself");

    let comment = vec!["# a comment", "quux() = 4"];
    assert_eq!(item_anchor_line(&comment, 1), 1, "comment above -> def line");
}

#[test]
fn anchor_scroll_for_def_line_maps_to_docstring_top() {
    let mut fs = FontSystem::new();
    // 0 module M | 1 blank | 2 export foo | 3 blank | 4 """ | 5 body |
    // 6 """ | 7 function foo() | 8 end
    let src = "module M\n\nexport foo\n\n\"\"\"\nfoo docstring\n\"\"\"\nfunction foo()\nend\n";
    let p = MarkdownPreview::new_plain(&mut fs, src, 2000.0, 1.0);
    // `function foo()` is source line 8 (1-indexed); its docstring opens at
    // buffer line 4 -> scroll 4 body-lines (no wrapping at 2000px wide).
    assert_eq!(p.anchor_scroll_for_def_line(8), 4);
    // Out of range -> 0 (caller's clamp keeps EOF items on-screen).
    assert_eq!(p.anchor_scroll_for_def_line(999), 0);
}

/// A fixture that reaches every arm of `walk`, including the cached and
/// uncached branches of fences, math and figures.
const EVERY_ARM: &str = r#"---
title: Front matter
---

# Heading one

## Heading two

### Heading three

#### Heading four

Plain *emphasis*, **strong**, ~~struck~~, `inline code`, <b>html</b> and a [link with *style*](https://example.com).
Soft break here
and a hard break here\
after a backslash. A footnote[^1].

> Quoted *line* one
>
> > Nested quote

- bullet one
- bullet two
  1. nested ordered
  2. nested ordered again

7. seventh
8. eighth

- [ ] open task
- [x] done task

***

```julia
x = 1  # cached fence
```

```julia
y = 2  # uncached fence
```

```rust
fn main() {}
```

```
plain fence
```

| A | B |
|---|:-:|
| 1 | 2 |
| 3 | 4 |

Inline $a+b$ cached and $c+d$ uncached.

$$E = mc^2$$

$$\int_0^1 x\,dx$$

![cached figure](figs/cached.png)

![pending figure](figs/pending.png)

![failed figure](figs/failed.png)

![remote badge](https://img.example.com/badge.svg?style=flat)

![](https://img.example.com/path/banner.png)

![an alt text that is longer than sixty characters so the label is truncated](data:image/png;base64,AAAA)

[^1]: The note.
"#;

/// The walk's whole output for `EVERY_ARM` (spans with their attributes,
/// media blocks, fence sources and the fences still to tokenize) matches
/// `testdata/walk_every_arm.golden`. To regenerate it after a deliberate
/// rendering change, run this test once with `SOT_BLESS_GOLDEN=1` and
/// review the diff.
#[test]
fn walk_output_for_every_arm_matches_the_golden() {
    let mut math: MathMetricsMap = HashMap::new();
    math.insert(
        ("a+b".to_string(), false),
        MathMetrics { width_px: 40.0, height_px: 18.0, baseline_drop_px: 4.0 },
    );
    math.insert(
        ("E = mc^2".to_string(), true),
        MathMetrics { width_px: 90.0, height_px: 30.0, baseline_drop_px: 6.0 },
    );
    let mut figures = FigureMetricsMap::new();
    figures.insert("figs/cached.png".to_string(), FigureMetrics { width_px: 120.0, height_px: 80.0 });
    figures.insert("figs/failed.png".to_string(), FigureMetrics { width_px: 0.0, height_px: 0.0 });
    let highlight = crate::preview::highlight::HighlightService::new().expect("highlight service init");
    // The first julia fence gets a cached overlay: its key is the one a
    // walk with an empty cache asks for first.
    let first = MarkdownPreview::new(&mut FontSystem::new(), EVERY_ARM, 800.0, 600.0, 1.5, &math, &figures, &highlight, &HashMap::new());
    let (lang, hash, _) = first.pending_token_fences[0].clone();
    let mut tokens = HashMap::new();
    tokens.insert((lang, hash), vec![crate::transport::MarkdownToken { start: 1, end: 2, kind: "variable".to_string() }]);
    let p = MarkdownPreview::new(&mut FontSystem::new(), EVERY_ARM, 800.0, 600.0, 1.5, &math, &figures, &highlight, &tokens);
    let mut lines: Vec<String> = p._spans.iter().map(|s| format!("span {s:?}")).collect();
    lines.extend(p.media_blocks.iter().map(|m| format!("media {m:?}")));
    lines.extend(p.code_block_sources.iter().map(|c| format!("source {c:?}")));
    lines.extend(p.pending_token_fences.iter().map(|(lang, _, src)| format!("fence {lang:?} {src:?}")));
    let actual = lines.join("\n") + "\n";
    let golden = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/ui/preview/markdown/testdata/walk_every_arm.golden");
    if std::env::var_os("SOT_BLESS_GOLDEN").is_some() {
        std::fs::create_dir_all(golden.parent().unwrap()).unwrap();
        std::fs::write(&golden, &actual).unwrap();
    }
    let want = std::fs::read_to_string(&golden).expect("golden missing: run once with SOT_BLESS_GOLDEN=1");
    assert!(actual == want, "walk output differs from {}:\n{actual}", golden.display());
}
