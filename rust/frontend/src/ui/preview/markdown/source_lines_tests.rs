//! Tests for the source-line map: which file lines a range of the rendered buffer was rendered from.

use super::*;

use cosmic_text::FontSystem;

const FIXTURE: &str = include_str!("testdata/selection_probe.md");

fn preview(source: &str, maps_source: bool) -> MarkdownPreview {
    let highlight = crate::ui::preview::markdown::highlight::HighlightService::new().expect("highlight service init");
    MarkdownPreview::new(&mut FontSystem::new(), source, 300.0, 600.0, 1.0, &Default::default(), &Default::default(),
        &highlight, &Default::default(), maps_source)
}

/// The file lines of the buffer's first `needle` (a range over exactly its bytes) in `source`'s rendering.
fn lines_of(source: &str, needle: &str) -> Option<(usize, usize)> {
    let p = preview(source, true);
    let TextOrigin::Rendered(map) = p.origin() else { panic!("no map") };
    let (n, line) = p.buffer.lines.iter().enumerate().find(|(_, l)| l.text().contains(needle)).expect(needle);
    let at = line.text().find(needle).unwrap();
    map.lines_of(((n, at), (n, at + needle.len())))
}

fn both_forms(f: impl Fn(&str)) {
    f(FIXTURE);
    f(&FIXTURE.replace('\n', "\r\n"));
}

#[test]
fn a_block_of_prose_maps_to_its_file_lines() {
    both_forms(|src| {
        assert_eq!(lines_of(src, "Selection probe"), Some((5, 5)));
        for word in ["First line", "a code span", "https://bare.example/url", "last line of it."] {
            assert_eq!(lines_of(src, word), Some((7, 10)), "{word}");
        }
        assert_eq!(lines_of(src, "continued"), Some((12, 13)));
        assert_eq!(lines_of(src, "item two"), Some((14, 14)));
        assert_eq!(lines_of(src, "quoted line two"), Some((16, 17)));
        assert_eq!(lines_of(src, "after it."), Some((38, 39)));
        assert_eq!(lines_of(src, "The note."), Some((41, 41)));
    });
    // comrak starts a paragraph after link reference definitions at the first definition's line.
    assert_eq!(lines_of("[r]: /u\nText\n", "Text"), Some((1, 2)));
}

#[test]
fn each_code_line_maps_to_its_own_file_line() {
    both_forms(|src| {
        assert_eq!(lines_of(src, "alpha = 1"), Some((28, 28)));
        assert_eq!(lines_of(src, "beta = 2"), Some((30, 30)));
        assert_eq!(lines_of(src, "indented two"), Some((34, 34)));
        let p = preview(src, true);
        let TextOrigin::Rendered(map) = p.origin() else { panic!("no map") };
        let n = p.buffer.lines.iter().position(|l| l.text().contains("alpha = 1")).unwrap();
        // The blank line between the two fenced lines is a padded line of its own.
        assert_eq!(map.lines_of(((n + 1, 0), (n + 1, 1))), Some((29, 29)));
    });
}

#[test]
fn decorations_and_media_hold_no_line_and_a_range_across_them_covers_them() {
    both_forms(|src| {
        for decoration in ["• ", "│ ", "───"] {
            assert_eq!(lines_of(src, decoration), None, "{decoration:?}");
        }
        let p = preview(src, true);
        let TextOrigin::Rendered(map) = p.origin() else { panic!("no map") };
        let whole = |n: usize| ((n, 0), (n, p.buffer.lines[n].text().len()));
        // Both the table and the equation reserve a line of only the placeholder.
        let media: Vec<usize> =
            (0..p.buffer.lines.len()).filter(|&n| p.buffer.lines[n].text().trim() == "\u{FFFC}").collect();
        assert_eq!(media.len(), 2, "a table and an equation");
        for n in media {
            assert_eq!(map.lines_of(whole(n)), None);
        }
        assert_eq!(map.lines_of(((1, 0), (1, 0))), None, "an empty line");
        let find = |needle: &str| p.buffer.lines.iter().position(|l| l.text().contains(needle)).unwrap();
        let end_of = |needle: &str| {
            let n = find(needle);
            (n, p.buffer.lines[n].text().find(needle).unwrap() + needle.len())
        };
        let start_of = |needle: &str| {
            let n = find(needle);
            (n, p.buffer.lines[n].text().find(needle).unwrap())
        };
        assert_eq!(map.lines_of((start_of("quoted line two"), end_of("alpha = 1"))), Some((16, 28)));
        assert_eq!(map.lines_of((start_of("indented two"), end_of("Tail with"))), Some((34, 39)));
    });
}

#[test]
fn the_map_is_kept_only_when_it_lines_up_with_the_buffer() {
    let rendered = |source: &str| matches!(preview(source, true).origin(), TextOrigin::Rendered(_));
    both_forms(|src| assert!(rendered(src)));
    for source in ["", "no final newline", "one\u{2029}two\n\nthree\n"] {
        assert!(rendered(source), "{source:?}");
    }
    assert!(matches!(preview(FIXTURE, false).origin(), TextOrigin::Unshared));
    let plain = MarkdownPreview::new_plain(&mut FontSystem::new(), "a\nb", 300.0, 1.0);
    assert!(matches!(plain.origin(), TextOrigin::File));
    let tokens = MarkdownPreview::new_tokens(&mut FontSystem::new(), &[("a".into(), "text".into())], 300.0, 1.0);
    assert!(matches!(tokens.origin(), TextOrigin::File));

    // `build` over spans of `abc` against a buffer shaped from other text.
    let span = |text: &str| vec![(text.to_string(), Attrs::new())];
    let marks = [(0..1, Lines::Block(1, 1))];
    let shaped = |text: &str| MarkdownPreview::new_plain(&mut FontSystem::new(), text, 300.0, 1.0);
    assert!(SourceLines::build("abc", &span("abc"), &marks, &shaped("abcd").buffer).is_none(), "longer line");
    assert!(SourceLines::build("abc", &span("abc"), &marks, &shaped("abd").buffer).is_none(), "other bytes");
    assert!(SourceLines::build("abc", &span("abc"), &marks, &shaped("abc").buffer).is_some());
}
