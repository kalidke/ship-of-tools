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
    assert!(!refused(document(1, MAX_PAGE_WORDS)), "exactly the most words");
    assert!(refused(document(1, MAX_PAGE_WORDS + 1)), "one word more");
    assert!(refused(document(MAX_PAGE_WORDS + 1, 1)), "one word more, in lines of one");
    let mut v = document(1, 1);
    v["lines"][0][0][4] = json!("w".repeat(MAX_PAGE_WORD_BYTES));
    assert!(!refused(v.clone()), "a word of exactly the most bytes");
    v["lines"][0][0][4] = json!("w".repeat(MAX_PAGE_WORD_BYTES + 1));
    assert!(refused(v.clone()), "a word of one byte more");
    // The bound is in bytes, not characters.
    v["lines"][0][0][4] = json!("é".repeat(MAX_PAGE_WORD_BYTES / 2 + 1));
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
