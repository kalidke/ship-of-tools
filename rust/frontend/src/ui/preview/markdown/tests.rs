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
    let highlight = crate::ui::preview::markdown::highlight::HighlightService::new()
        .expect("highlight service init");
    MarkdownPreview::new(&mut fs, source, 800.0, 600.0, 1.0, &math, &figures, &highlight, &tokens, false)
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
fn a_paragraph_separator_starts_a_line_and_the_shaper_survives_it() {
    // cosmic-text panics on a line holding a second bidi paragraph; a previewed file or a figure caption may hold
    // U+2029, U+2028 or U+0085 before right-to-left text, and each must start a line instead.
    let mut fonts = FontSystem::new();
    let md = MarkdownPreview::new_plain(&mut fonts, "AA\u{2029}אב\u{2028}ג\u{85}ד", 800.0, 1.0);
    assert_eq!(md.buffer.lines.len(), 4);
    assert_eq!(normalize_newlines("a\u{2029}b\u{2028}c\u{85}d").as_ref(), "a\nb\nc\nd");
}

#[test]
fn crlf_source_does_not_double_space() {
    // A CRLF (`\r\n`) file must shape to the same number of visual lines
    // as the LF equivalent. cosmic-text treats a stray `\r` as its own
    // line break, so without normalization a CRLF source rendered a
    // blank line between every line (the double-spaced code bug, only
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
    let highlight = crate::ui::preview::markdown::highlight::HighlightService::new().expect("highlight service init");
    // The first julia fence gets a cached overlay: its key is the one a
    // walk with an empty cache asks for first.
    let first = MarkdownPreview::new(&mut FontSystem::new(), EVERY_ARM, 800.0, 600.0, 1.5, &math, &figures, &highlight, &HashMap::new(), false);
    let (lang, hash, _) = first.pending_token_fences[0].clone();
    let mut tokens = HashMap::new();
    tokens.insert((lang, hash), vec![crate::net::transport::MarkdownToken { start: 1, end: 2, kind: "variable".to_string() }]);
    let p = MarkdownPreview::new(&mut FontSystem::new(), EVERY_ARM, 800.0, 600.0, 1.5, &math, &figures, &highlight, &tokens, false);
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

#[test]
fn a_plain_preview_is_one_bidi_paragraph_per_line() {
    // cosmic-text ends a buffer line only at `\r` and `\n` and panics on a line holding two bidi paragraphs of
    // different directions; a previewed file, a fatal error's text or a figure's label may hold U+001C, U+001D or
    // U+001E, each of which ends a paragraph, before right-to-left text.
    let md = MarkdownPreview::new_plain(&mut FontSystem::new(), "AA\u{1c}א\u{1d}ב\u{1e}ג\nEE", 800.0, 1.0);
    let lines: Vec<&str> = md.buffer.lines.iter().map(|line| line.text()).collect();
    assert_eq!(lines, ["AA א ב ג", "EE"]);
}

#[test]
fn a_table_cell_is_one_bidi_paragraph_per_line() {
    // A table is shaped as its own buffer from its rendered text, whose cells may hold any bidi paragraph separator
    // before right-to-left text: each table row stays one line.
    let p = make_preview("| h |\n|---|\n| AA\u{1c}א\u{85}ב\u{2029}ג |\n");
    let (rendered, font_px, line_h_px) = p
        .media_blocks
        .iter()
        .find_map(|block| match block {
            MediaBlock::Table { rendered, font_px, line_h_px, .. } => Some((rendered.clone(), *font_px, *line_h_px)),
            _ => None,
        })
        .expect("a table block");
    let rows = rendered.lines().count();
    let entry = media::table_buffer(&mut FontSystem::new(), rendered, font_px, line_h_px);
    let lines: Vec<&str> = entry.buffer.lines.iter().map(|line| line.text()).collect();
    assert!(lines.iter().any(|line| line.contains("AA א ב ג")), "{lines:?}");
    assert_eq!(lines.iter().filter(|line| !line.is_empty()).count(), rows, "{lines:?}");
}

#[test]
fn a_table_row_is_one_drawn_row_however_wide() {
    // The walk reserves a table's place from its line count alone, so the place is known before the table is shaped;
    // a row wider than the buffer's 10,000 px must not wrap into a second drawn row below its place.
    let wide = format!("| h | {} |\n|---|---|\n| a | b c |\n", "word ".repeat(400).trim());
    let p = make_preview(&wide);
    let (rendered, n_lines, font_px, line_h_px) = p
        .media_blocks
        .iter()
        .find_map(|block| match block {
            MediaBlock::Table { rendered, n_lines, font_px, line_h_px } => Some((rendered.clone(), *n_lines, *font_px, *line_h_px)),
            _ => None,
        })
        .expect("a table block");
    let entry = media::table_buffer(&mut FontSystem::new(), rendered, font_px, line_h_px);
    assert!(entry.natural_w_px > 10_000.0, "the table is {} px wide", entry.natural_w_px);
    assert_eq!(entry.buffer.layout_runs().count(), n_lines, "each line of the table is one drawn row");
}

#[test]
fn a_tables_width_is_known_before_it_is_shaped_and_shaping_never_lowers_it() {
    // The scroll clamp reads each table's natural width whether or not it is shaped: before shaping it is the widest
    // line's characters at the monospace advance, which is the shaped width for text the face draws at that advance,
    // and a table whose shaped lines are narrower (a combining mark is a character with no advance) keeps the larger.
    let mut fs = FontSystem::new();
    let (font_px, line_h_px) = (12.75, 18.7);
    for (text, same) in [("| abc | de |\n| fgh | ij |\n", true), ("| e\u{301}e\u{301}e\u{301} |\n", false)] {
        let mut entry = media::table_entry(&mut fs, text.to_owned(), font_px, line_h_px);
        assert!(entry.buffer.lines.iter().all(|line| line.layout_opt().is_none()));
        let before = entry.natural_w_px;
        assert!(before > 0.0, "{text:?}");
        let rows = entry.buffer.lines.len();
        assert_eq!(entry.show(&mut fs, 0..rows, true), rows);
        let measured = entry.buffer.layout_runs().map(|run| run.line_w).fold(0.0, f32::max);
        assert!(measured > 0.0, "{text:?}");
        assert_eq!(entry.natural_w_px, before.max(measured), "{text:?}");
        assert_eq!((measured - before).abs() < 0.5, same, "{text:?}: {measured} shaped against {before}");
    }
}

#[test]
fn a_tables_place_is_paired_by_its_index_among_the_tables() {
    let table = |rendered: &str| MediaBlock::Table { rendered: rendered.into(), n_lines: 1, line_h_px: 10.0, font_px: 8.0 };
    let blocks = [MediaBlock::Math { latex: "x".into(), display: true }, table("a"), table("b"), table("c")];
    let at = |y: f32| crate::ui::render::quad::ScreenRect { x: 0.0, y, w: 100.0, h: 10.0 };
    // The second table's place is missing: the third is still the third.
    let places = media::table_places(&blocks, &[(0, at(0.0)), (1, at(20.0)), (3, at(60.0))]);
    assert_eq!(places.iter().map(|(table, rect)| (*table, rect.y)).collect::<Vec<_>>(), [(0, 20.0), (2, 60.0)]);
}

/// A table of `rows` rows of 8 cells of a tag repeated to about 110 characters, as the native fixture's.
fn repeated_table(rows: usize) -> String {
    let mut table = String::new();
    for r in 0..rows {
        let cells: Vec<String> = (0..8).map(|c| format!("r{r}c{c} ").repeat(18).trim().to_owned()).collect();
        table.push_str(&format!("| {} |\n", cells.join(" | ")));
        if r == 0 {
            table.push_str(&format!("|{}\n", "---|".repeat(8)));
        }
    }
    table
}

/// A table of `rows` rows of `cols` cells of ordinary words, 3 to 9 a cell, the same at every run.
fn words_table(rows: usize, cols: usize) -> String {
    const WORDS: &[&str] = &["the", "result", "shows", "that", "each", "sample", "was", "measured", "after", "run",
        "and", "before", "next", "change", "value", "error", "rate", "mean", "first", "second", "column", "report",
        "signal", "noise", "ratio", "model", "bound", "small", "large", "within", "across", "nearly", "several", "kept"];
    let mut seed: u64 = 0x2545_f491_4f6c_dd1d;
    let mut next = |n: usize| {
        seed = seed.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1_442_695_040_888_963_407);
        ((seed >> 33) as usize) % n
    };
    let mut table = String::new();
    for r in 0..rows {
        let cells: Vec<String> = (0..cols).map(|_| (0..3 + next(7)).map(|_| WORDS[next(WORDS.len())]).collect::<Vec<_>>().join(" ")).collect();
        table.push_str(&format!("| {} |\n", cells.join(" | ")));
        if r == 0 {
            table.push_str(&format!("|{}\n", "---|".repeat(cols)));
        }
    }
    table
}

/// What a table's buffer draws from an area at (`left`, `top`) clipped to rows between `clip`: each row more than
/// 0.01 px of which is inside, as its index, its top and each glyph's face, id and position.
fn drawn(buffer: &Buffer, left: f32, top: f32, clip: (f32, f32)) -> Vec<(usize, f32, Vec<(cosmic_text::fontdb::ID, u16, f32, f32)>)> {
    buffer
        .layout_runs()
        .filter(|run| top + run.line_top < clip.1 - 0.01 && top + run.line_top + run.line_height > clip.0 + 0.01)
        .map(|run| (run.line_i, top + run.line_top, run.glyphs.iter().map(|g| (g.font_id, g.glyph_id, left + g.x, top + run.line_y + g.y)).collect()))
        .collect()
}

#[test]
fn a_table_shaped_row_by_row_draws_what_whole_shaping_draws() {
    // A table shaped as the pane comes to its rows draws, at every position of the pane and every sideways scroll, the
    // rows, glyphs and places a table shaped whole draws, and has shaped exactly the rows the pane has shown.
    let mut fs = FontSystem::new();
    for (name, source) in [("repeated", repeated_table(40)), ("words", words_table(44, 10))] {
        let p = make_preview(&source);
        let (rendered, n_lines, font_px, line_h_px) = p
            .media_blocks
            .iter()
            .find_map(|block| match block {
                MediaBlock::Table { rendered, n_lines, font_px, line_h_px } => Some((rendered.clone(), *n_lines, *font_px, *line_h_px)),
                _ => None,
            })
            .expect("a table block");
        // Shaped whole, as a table was before its rows were shaped one by one.
        let metrics = Metrics::new(font_px, line_h_px);
        let mut whole = Buffer::new_empty(metrics);
        whole.set_wrap(&mut fs, cosmic_text::Wrap::None);
        whole.set_size(&mut fs, Some(10_000.0), None);
        set_buffer_text(&mut whole, &mut fs, &rendered, Attrs::new().family(Family::Monospace).metrics(metrics), Shaping::Advanced);
        whole.shape_until_scroll(&mut fs, false);
        let mut lazy = media::table_entry(&mut fs, rendered.clone(), font_px, line_h_px);
        let place = crate::ui::render::quad::ScreenRect { x: 37.0, y: 211.5, w: 900.0, h: n_lines as f32 * line_h_px };
        let mut seed: u64 = 7;
        let mut next = |n: u64| {
            seed = seed.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1_442_695_040_888_963_407);
            (seed >> 33) % n
        };
        let (mut shown, mut partial) = (std::collections::BTreeSet::new(), false);
        for step in 0..60 {
            let y = place.y - 300.0 + next((place.h + 600.0) as u64) as f32 + 0.25 * (step % 4) as f32;
            let pane = crate::ui::render::quad::ScreenRect { x: 20.0, y, w: 700.0, h: 100.0 + next(600) as f32 };
            let rows = lazy.rows_in(place, pane);
            lazy.show(&mut fs, rows.clone(), true);
            assert!(lazy.rows_shaped(&rows), "{name} step {step}");
            shown.extend(rows.clone());
            let shaped: std::collections::BTreeSet<usize> =
                lazy.buffer.lines.iter().enumerate().filter(|(_, line)| line.layout_opt().is_some()).map(|(row, _)| row).collect();
            assert_eq!(shaped, shown, "{name} step {step}: the rows shaped are the rows shown");
            partial |= !shaped.is_empty() && shaped.len() < n_lines;
            let clip = (pane.y, media::whole_row_bottom(place.y, line_h_px, pane.y + pane.h));
            for scroll in [0.0, 13.0, 250.5, 4_000.0] {
                let want = drawn(&whole, place.x - scroll, place.y, clip);
                let got = lazy.drawn_top(place.y, &rows).map_or_else(Vec::new, |top| drawn(&lazy.buffer, place.x - scroll, top, clip));
                assert_eq!(got.len(), want.len(), "{name} step {step} scroll {scroll}: rows drawn");
                for ((row, top, glyphs), (want_row, want_top, want_glyphs)) in got.iter().zip(&want) {
                    assert!(row == want_row && (top - want_top).abs() < 0.01, "{name} step {step}: row {row} at {top}, not {want_row} at {want_top}");
                    assert_eq!(glyphs.len(), want_glyphs.len(), "{name} step {step} row {row}");
                    for (g, w) in glyphs.iter().zip(want_glyphs) {
                        assert!(g.0 == w.0 && g.1 == w.1 && (g.2 - w.2).abs() < 0.01 && (g.3 - w.3).abs() < 0.01, "{name} step {step} row {row}: {g:?} against {w:?}");
                    }
                }
            }
        }
        assert!(partial, "{name}: no step left a row of the table unshaped");
    }
}

#[test]
fn a_table_whose_first_row_in_view_is_shaped_draws_its_rows_up_to_the_first_not_shaped() {
    // A deferred frame can find more rows in view than were shaped (an uncover that makes the pane taller): the table
    // draws the rows shaped from its first row in view on, each where a table shaped whole draws it, up to the first
    // not shaped; with its first row in view not shaped it draws nothing.
    use crate::ui::render::quad::ScreenRect;
    let mut fs = FontSystem::new();
    let p = make_preview(&repeated_table(40));
    let Some(MediaBlock::Table { rendered, font_px, line_h_px, .. }) =
        p.media_blocks.iter().find(|block| matches!(block, MediaBlock::Table { .. }))
    else {
        panic!("a table block")
    };
    let (font_px, line_h_px) = (*font_px, *line_h_px);
    let whole = media::table_buffer(&mut fs, rendered.clone(), font_px, line_h_px);
    let mut lazy = media::table_entry(&mut fs, rendered.clone(), font_px, line_h_px);
    let place = ScreenRect { x: 37.0, y: 211.5, w: 900.0, h: 42.0 * line_h_px };
    // A pane of 20 rows from row 5, of which a shorter pane shaped the first 8 before.
    let pane = ScreenRect { x: 20.0, y: place.y + 5.0 * line_h_px + 0.25, w: 700.0, h: 20.0 * line_h_px };
    let rows = lazy.rows_in(place, pane);
    assert_eq!(rows, 5..25);
    lazy.show(&mut fs, 5..13, true);
    lazy.show(&mut fs, rows.clone(), false);
    let clip = (pane.y, media::whole_row_bottom(place.y, line_h_px, pane.y + pane.h));
    let want: Vec<_> = drawn(&whole.buffer, place.x, place.y, clip).into_iter().filter(|(row, ..)| *row < 13).collect();
    let got = lazy.drawn_top(place.y, &rows).map_or_else(Vec::new, |top| drawn(&lazy.buffer, place.x, top, clip));
    assert_eq!(got.iter().map(|(row, ..)| *row).collect::<Vec<_>>(), (5..13).collect::<Vec<_>>(), "the rows drawn");
    for ((row, top, glyphs), (_, want_top, want_glyphs)) in got.iter().zip(&want) {
        assert!((top - want_top).abs() < 0.01 && glyphs.len() == want_glyphs.len(), "row {row} at {top}, not {want_top}");
        for (g, w) in glyphs.iter().zip(want_glyphs) {
            assert!(g.0 == w.0 && g.1 == w.1 && (g.2 - w.2).abs() < 0.01 && (g.3 - w.3).abs() < 0.01, "row {row}: {g:?} against {w:?}");
        }
    }
    // A pane two rows higher starts at row 3, which is not shaped: the table draws nothing.
    let higher = ScreenRect { y: pane.y - 2.0 * line_h_px, ..pane };
    let rows = lazy.rows_in(place, higher);
    assert_eq!(rows.start, 3);
    lazy.show(&mut fs, rows.clone(), false);
    assert_eq!(lazy.drawn_top(place.y, &rows), None);
}
