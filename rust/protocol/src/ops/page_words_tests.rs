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

