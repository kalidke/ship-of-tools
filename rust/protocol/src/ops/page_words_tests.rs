use serde_json::{json, Value};

use super::*;

/// The document the PDF plugin's test pins byte for byte (`julia/plugins/pdf-file/test/words.jl`, `WORDS_DOCUMENT`).
const PLUGIN_DOCUMENT: &str = r#"{"page":2,"cut":false,"lines":[[[13107,13107,26214,26214,"alpha"],[39321,13107,52428,26214,"beta"]],[[13107,39321,26214,52428,"gamma"]]]}"#;

fn word(left: u16, top: u16, right: u16, bottom: u16, text: &str) -> PageWord {
    PageWord { left, top, right, bottom, text: text.into() }
}

/// A document of `lines` lines of `each` words, as a decoder is handed it.
fn document(lines: usize, each: usize) -> Value {
    json!({"page": 1, "cut": false, "lines": vec![vec![json!([1, 2, 3, 4, "w"]); each]; lines]})
}

fn refused(v: Value) -> bool {
    serde_json::from_value::<PageWords>(v).is_err()
}

#[test]
fn the_plugins_document_is_the_wires() {
    assert_eq!(PAGE_WORDS_MIME, "application/vnd.sot.page-words+json");
    let doc: PageWords = serde_json::from_str(PLUGIN_DOCUMENT).expect("the plugin's document decodes");
    assert_eq!(
        doc,
        PageWords {
            page: 2,
            cut: false,
            lines: vec![
                vec![word(13107, 13107, 26214, 26214, "alpha"), word(39321, 13107, 52428, 26214, "beta")],
                vec![word(13107, 39321, 26214, 52428, "gamma")],
            ],
        }
    );
    assert_eq!(serde_json::to_string(&doc).unwrap(), PLUGIN_DOCUMENT, "written as the plugin writes it");
    // A page with no words is a document with no lines, and a cut one says so.
    let empty: PageWords = serde_json::from_str(r#"{"page":2,"cut":true,"lines":[]}"#).unwrap();
    assert_eq!((empty.page, empty.cut, empty.lines.len()), (2, true, 0));
}

#[test]
fn page_words_are_bounded_at_decode() {
    // The two numbers are written out: the plugin's test states the same two, and a change to either constant breaks one.
    assert_eq!((MAX_PAGE_WORDS, MAX_PAGE_WORD_BYTES), (16384, 1024));
    assert!(!refused(document(1, 16384)), "exactly the most words");
    assert!(refused(document(1, 16385)), "one word more");
    assert!(refused(document(16385, 1)), "one word more, in lines of one");
    let mut v = document(1, 1);
    v["lines"][0][0][4] = json!("w".repeat(1024));
    assert!(!refused(v.clone()), "a word of exactly the most bytes");
    v["lines"][0][0][4] = json!("w".repeat(1025));
    assert!(refused(v.clone()), "a word of one byte more");
    // The bound is in bytes, not characters.
    v["lines"][0][0][4] = json!("é".repeat(512));
    assert!(!refused(v.clone()), "512 two-byte characters are 1024 bytes");
    v["lines"][0][0][4] = json!("é".repeat(513));
    assert!(refused(v), "a word of more bytes than characters");
    let mut v = document(1, 1);
    v["lines"] = json!([[]]);
    assert!(refused(v.clone()), "an empty line, which the plugin leaves out");
    v["page"] = json!(0);
    v["lines"] = json!([]);
    assert!(refused(v), "page 0: pages count from 1");
    // A box is ordered, and each side is a fraction of 65535.
    for (b, ok) in [([5, 5, 5, 5], true), ([5, 5, 6, 9], true), ([6, 5, 5, 9], false), ([5, 9, 6, 8], false), ([0, 0, 65535, 65535], true)] {
        let mut v = document(1, 1);
        v["lines"][0][0] = json!([b[0], b[1], b[2], b[3], "w"]);
        assert_eq!(!refused(v), ok, "box {b:?}");
    }
    for v in [json!([0, 0, 65536, 1, "w"]), json!([-1, 0, 1, 1, "w"]), json!([0, 0, 1.5, 1, "w"]), json!([0, 0, 1, 1]), json!([0, 0, 1, 1, "w", 2]), json!([0, 0, 1, 1, 7])] {
        let mut d = document(1, 1);
        d["lines"][0][0] = v.clone();
        assert!(refused(d), "{v}");
    }
}

/// The request grows by `words`: an ordinary one is written as an older daemon reads it, a words one says so, and an
/// older request reads as no words.
#[test]
fn a_words_request_is_the_ordinary_one_with_one_more_field() {
    let ordinary = PreviewGetReq { node_id: "files:a.pdf".into(), workspace_id: None, page: Some(2), fit_w: None, fit_h: None, words: false };
    assert_eq!(serde_json::to_value(&ordinary).unwrap(), json!({"node_id": "files:a.pdf", "page": 2}), "false is not written");
    let asked = PreviewGetReq { words: true, ..ordinary.clone() };
    assert_eq!(serde_json::to_value(&asked).unwrap(), json!({"node_id": "files:a.pdf", "page": 2, "words": true}));
    let old: PreviewGetReq = serde_json::from_value(json!({"node_id": "files:a.pdf", "page": 2, "fit_w": 800})).unwrap();
    assert!(!old.words, "an older window's request asks for no words");
    assert_eq!(serde_json::from_value::<PreviewGetReq>(json!({"node_id": "n", "words": true})).unwrap().words, true);
}

/// The type, not the plugin alone, guarantees a word's text is clean: no empty word, no control character (C0, DEL, C1
/// and the escape of a terminal sequence among them) and no line or paragraph separator.
#[test]
fn a_words_text_is_clean_at_decode() {
    for (text, ok) in [("w", true), ("é日本", true), ("a b", true), ("", false), ("\u{1b}[2J", false), ("a\nb", false), ("a\tb", false), ("\u{7f}", false), ("\u{85}", false), ("a\u{2028}b", false), ("\u{2029}", false)] {
        let mut v = document(1, 1);
        v["lines"][0][0][4] = json!(text);
        assert_eq!(!refused(v), ok, "{text:?}");
    }
}

/// The document is the one object the plugin writes: a positional array of the same values, a document with no `cut` or
/// no `lines` or no `page` (each required: a missing `cut` read as false would show a cut page as whole) and a value that
/// is no object are refused, and a field this reader does not know is ignored.
#[test]
fn the_document_is_one_object_with_all_three_fields() {
    assert!(refused(json!([2, false, []])), "a positional array");
    assert!(refused(json!([2, false, [[[1, 2, 3, 4, "w"]]]])), "a positional array with a word");
    assert!(refused(json!({"page": 2, "lines": []})), "no cut");
    assert!(refused(json!({"page": 2, "cut": false})), "no lines");
    assert!(refused(json!({"cut": false, "lines": []})), "no page");
    assert!(refused(json!("page")) && refused(json!(null)) && refused(json!(2)), "no object");
    assert!(!refused(json!({"page": 2, "cut": false, "lines": [], "later": [1, {"a": null}]})), "a field of a later reader");
    // Through the bytes the window reads, as the array was not refused before.
    assert!(serde_json::from_slice::<PageWords>(b"[2,false,[]]").is_err());
    assert!(serde_json::from_slice::<PageWords>(br#"{"lines":[],"cut":true,"page":2}"#).is_ok(), "any key order");
    // A field named twice is refused, whichever value comes second: a second `cut` must not read a cut page as whole.
    for twice in [
        br#"{"page":2,"cut":true,"lines":[],"cut":false}"#.as_slice(),
        br#"{"page":2,"cut":true,"lines":[],"cut":true}"#,
        br#"{"page":2,"page":3,"cut":false,"lines":[]}"#,
        br#"{"page":2,"cut":false,"lines":[],"lines":[[[1,2,3,4,"w"]]]}"#,
        br#"{"lines":[],"page":2,"lines":[],"cut":false}"#,
    ] {
        assert!(serde_json::from_slice::<PageWords>(twice).is_err(), "{}", String::from_utf8_lossy(twice));
    }
    assert!(serde_json::from_slice::<PageWords>(br#"{"page":2,"cut":false,"lines":[],"later":1,"later":2}"#).is_ok(), "a field this reader does not know, twice");
}

/// The text of a range of words is the words of a line joined by one space and the lines by LF, numbered over all lines;
/// a range that is not inside the document, or runs backward, is none.
#[test]
fn a_range_of_words_is_their_text_by_line_and_space() {
    let doc: PageWords = serde_json::from_str(PLUGIN_DOCUMENT).unwrap();
    assert_eq!(doc.word_count(), 3);
    for (first, last, want) in [
        (0, 2, Some("alpha beta\ngamma")),
        (0, 0, Some("alpha")),
        (1, 1, Some("beta")),
        (2, 2, Some("gamma")),
        (0, 1, Some("alpha beta")),
        (1, 2, Some("beta\ngamma")),
        (2, 1, None),
        (0, 3, None),
        (3, 3, None),
        (usize::MAX, usize::MAX, None),
    ] {
        assert_eq!(doc.text(first, last).as_deref(), want, "{first}..={last}");
    }
    let none = PageWords { page: 1, cut: false, lines: vec![] };
    assert_eq!((none.word_count(), none.text(0, 0)), (0, None), "a page with no words has no range");
}

fn page(lines: Vec<Vec<PageWord>>) -> PageWords {
    PageWords { page: 1, cut: false, lines }
}

/// Two words of a line that touch are joined with no space: a subscript or a superscript the PDF tool made a word of its
/// own (its box starts where its letter's ends, or a little after, as an exponent after an italic letter does), and two
/// boxes that overlap. A gap of a tenth of the smaller of the two words' widths per character or more is a space, so a
/// thin space is kept, and so is the half point TeX sets after a subscript, as the tool's own plain text keeps it. The
/// boxes are the text tool's, as the plugin writes them, on the first page of `examples/preview/sample.pdf` and on a
/// generated page.
#[test]
fn a_word_that_touches_the_one_before_it_is_joined_to_it_with_no_space() {
    for (line, want) in [
        // Each subscript starts where its letter ends; each comma is 54 (half a point) after its subscript.
        (
            vec![
                word(31029, 26315, 35448, 27117, "latitudes"),
                word(35882, 26315, 36578, 27117, "ϕ"),
                word(36578, 26619, 37031, 27205, "1"),
                word(37085, 26315, 37409, 27117, ","),
                word(37604, 26315, 38300, 27117, "ϕ"),
                word(38300, 26619, 38753, 27205, "2"),
                word(39240, 26315, 41123, 27117, "and"),
            ],
            "latitudes ϕ1 , ϕ2 and",
        ),
        // An exponent that starts where `sin` ends; one 38 after an italic `y`, under a tenth of its own 426.
        (vec![word(45974, 43400, 47409, 44201, "sin"), word(47409, 43196, 47862, 43782, "2"), word(48044, 43213, 48554, 43799, "x")], "sin2 x"),
        (vec![word(10900, 12552, 12618, 13147, "and"), word(12891, 12415, 13414, 13147, "y"), word(13452, 12287, 13878, 12800, "2")], "and y2"),
        // A thin space, 203 against a tenth of the 453 a character of `θ;`.
        (vec![word(48922, 30214, 49830, 31016, "="), word(50155, 30214, 51042, 31016, "R"), word(51245, 30214, 52151, 31016, "θ;")], "= R θ;"),
        // Boxes that overlap touch; a line read right to left keeps its space, the gap being measured either way.
        (vec![word(1000, 100, 3000, 900, "ab"), word(2900, 100, 4000, 900, "c")], "abc"),
        (vec![word(6000, 100, 8000, 900, "b"), word(1000, 100, 5000, 900, "a")], "b a"),
    ] {
        let last = line.len() - 1;
        assert_eq!(page(vec![line]).text(0, last).as_deref(), Some(want));
    }
    // A selection of the subscript alone, or of its letter alone, gives that word.
    let doc = page(vec![vec![word(35882, 26315, 36578, 27117, "ϕ"), word(36578, 26619, 37031, 27205, "1")]]);
    assert_eq!((doc.text(0, 0).as_deref(), doc.text(1, 1).as_deref()), (Some("ϕ"), Some("1")));
}

/// A line that is only a section's number goes on with the next line when that line starts beside it on its right, on
/// the same band of the page (the two boxes share half the shorter's height) and near it (within four of its heights), so
/// a heading the PDF tool gave as two lines, its number and its title a column's gap apart, is one line of text; every
/// other break between lines stays. Two columns are not joined, whether the tool gave them a column at a time or a line of each in turn on one band. The
/// heading's boxes are the tool's, as the plugin writes them, on the first page of `examples/preview/sample.pdf`.
#[test]
fn a_headings_number_and_its_title_are_one_line() {
    let title = || vec![word(10302, 24103, 14644, 25157, "Setup")];
    let heading = |number: Vec<PageWord>| {
        let last = number.len();
        page(vec![number, title()]).text(0, last).unwrap()
    };
    let doc = page(vec![vec![word(7710, 24103, 8574, 25157, "1")], title(), vec![word(7710, 26315, 9414, 27117, "Let"), word(9847, 26315, 11665, 27117, "two")]]);
    assert_eq!(doc.text(0, 3).as_deref(), Some("1 Setup\nLet two"));
    assert_eq!(doc.text(1, 2).as_deref(), Some("Setup\nLet"), "a selection that starts at the title");
    for (number, want) in [
        (vec![word(7710, 24103, 8574, 25157, "2.1")], "2.1 Setup"),
        (vec![word(7710, 24103, 8574, 25157, "3.")], "3. Setup"),
        // Set higher than its title by a third of its height, the number is on the title's band; a line lower, it is not.
        (vec![word(7710, 23752, 8574, 24806, "1")], "1 Setup"),
        (vec![word(7710, 25157, 8574, 26211, "1")], "1\nSetup"),
        // A mark the tool put before the words it follows, which are on its left.
        (vec![word(15000, 24103, 15400, 25157, "1")], "1\nSetup"),
        // No section's number, or not alone on its line.
        (vec![word(7710, 24103, 8574, 25157, "(1)")], "(1)\nSetup"),
        (vec![word(7710, 24103, 8574, 25157, "A")], "A\nSetup"),
        (vec![word(7710, 24103, 8574, 25157, "1..")], "1..\nSetup"),
        (vec![word(5000, 24103, 7000, 25157, "Table"), word(7710, 24103, 8574, 25157, "1")], "Table 1\nSetup"),
    ] {
        assert_eq!(heading(number), want);
    }
    // Two columns given a column at a time, and a line of each in turn on one band (as the tool gave a generated page).
    let columns = page(vec![
        vec![word(7710, 12553, 9263, 13148, "Let")],
        vec![word(7710, 13500, 12000, 14096, "be")],
        vec![word(33260, 12693, 49023, 13289, "Vivamus")],
        vec![word(33260, 13641, 40000, 14237, "viverra")],
    ]);
    assert_eq!(columns.text(0, 3).as_deref(), Some("Let\nbe\nVivamus\nviverra"));
    let turns = page(vec![
        vec![word(7710, 23434, 11276, 24030, "egestas.")],
        vec![word(33260, 23574, 44022, 24170, "tate")],
        vec![word(12208, 23434, 19072, 24030, "Mauris")],
    ]);
    assert_eq!(turns.text(0, 2).as_deref(), Some("egestas.\ntate\nMauris"));
}

/// A line of text set at 90 or 270 degrees is one line of the tool whose words are stacked down the page: their boxes share
/// one horizontal range, so the gap between them is measured down the page, and so is a word's length per character, and
/// the spaces stay. Read upward (the first word lowest) and read downward (the first word highest); two words that meet
/// down the page still touch. The boxes are the shape the plugin wrote for a sideways table's caption.
#[test]
fn a_line_of_words_stacked_down_the_page_keeps_its_spaces() {
    let upward = vec![word(33465, 40000, 35006, 41000, "Mean"), word(33465, 39200, 35006, 39700, "of"), word(33465, 38200, 35006, 38900, "signal")];
    let downward = vec![word(33465, 38000, 35006, 39000, "Mean"), word(33465, 39300, 35006, 39800, "of"), word(33465, 40100, 35006, 40800, "signal")];
    let meeting = vec![word(33465, 40000, 35006, 41000, "ab"), word(33465, 39500, 35006, 40000, "c")];
    for (line, want) in [(upward, "Mean of signal"), (downward, "Mean of signal"), (meeting, "abc")] {
        let last = line.len() - 1;
        assert_eq!(page(vec![line]).text(0, last).as_deref(), Some(want));
    }
}

/// A line that is only a number goes on with a line beside it only when that line starts within four of the number's
/// heights: a heading's title is under two (the sample's is 1.6), and another column's line, or a sub- or superscript the
/// tool gave a line of its own beside another, is further.
#[test]
fn a_lone_number_goes_on_only_with_a_line_within_four_heights() {
    // The number is 1054 high; its title starts 1728 (1.6 heights), 2108 (2), 4216 (4), 5270 (5) and 19000 from its right.
    for (title_left, want) in [(10302, "1 Setup"), (10682, "1 Setup"), (12790, "1 Setup"), (13844, "1\nSetup"), (27574, "1\nSetup")] {
        let doc = page(vec![vec![word(7710, 24103, 8574, 25157, "1")], vec![word(title_left, 24103, title_left + 4342, 25157, "Setup")]]);
        assert_eq!(doc.text(0, 1).as_deref(), Some(want), "a title starting at {title_left}");
    }
    // A heading's number, and the other column's line the tool gave between it and its title.
    let columns = page(vec![
        vec![word(7710, 32566, 8555, 34263, "2")],
        vec![word(34327, 32507, 36896, 33102, "Fusce"), word(37388, 32507, 40834, 33102, "mauris.")],
        vec![word(10245, 32566, 16706, 34263, "Methods")],
    ]);
    assert_eq!(columns.text(0, 3).as_deref(), Some("2\nFusce mauris.\nMethods"));
}

/// The number and its title of a heading are joined by one space, even where the title's box starts at the number's right.
#[test]
fn a_heading_join_is_one_space_whatever_the_boxes_say() {
    let doc = page(vec![vec![word(1000, 100, 2000, 900, "1")], vec![word(2000, 100, 5000, 900, "Setup")]]);
    assert_eq!(doc.text(0, 1).as_deref(), Some("1 Setup"));
}

/// The bound of `touch` is a tenth of a word's width per character, not of its whole width: two words of four characters
/// 300 apart, each 4000 wide, are a space apart (a tenth of 1000), though 300 is under a tenth of 4000.
#[test]
fn the_touch_bound_is_per_character() {
    let line = vec![word(1000, 100, 5000, 900, "abcd"), word(5300, 100, 9300, 900, "efgh")];
    assert_eq!(page(vec![line]).text(0, 1).as_deref(), Some("abcd efgh"));
}

/// The bound is the smaller of the two words' tenths: a word of two characters 100 wide (a tenth of 50 each) and one of
/// four 8000 wide (a tenth of 2000 each), 100 apart, are a space apart in either order, though 100 is under the second's.
#[test]
fn the_touch_bound_is_the_smaller_of_the_two_words() {
    let small_first = vec![word(1000, 100, 1100, 900, "ab"), word(1200, 100, 9200, 900, "cdef")];
    let small_last = vec![word(1000, 100, 9000, 900, "cdef"), word(9100, 100, 9200, 900, "ab")];
    assert_eq!(page(vec![small_first]).text(0, 1).as_deref(), Some("ab cdef"));
    assert_eq!(page(vec![small_last]).text(0, 1).as_deref(), Some("cdef ab"));
}

/// A word's length per character is taken along the line, so down the page for a line set at 90 or 270 degrees: TeX's half
/// point after a subscript is a space there as on a horizontal line (a word's thickness per character, across the page,
/// would be wider than a tenth of that gap). The boxes are the plugin's, from a generated page's line set at 90 degrees.
#[test]
fn a_rotated_words_length_per_character_is_measured_along_the_line() {
    let line = vec![word(48109, 16119, 49056, 16610, "ϕ"), word(48491, 15790, 49154, 16119, "1"), word(47723, 15520, 49264, 15749, ",")];
    assert_eq!(page(vec![line]).text(0, 2).as_deref(), Some("ϕ1 ,"));
}
