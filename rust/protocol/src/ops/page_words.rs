// the words of one PDF page and where they were drawn: what the PDF plugin answers a `preview.get` that asks for words

use super::*;

/// The mime of the plugin's answer to a words request, the `PreviewGetRes`'s and the blob's.
pub const PAGE_WORDS_MIME: &str = "application/vnd.sot.page-words+json";
/// The most words a page's document may hold in all lines together.
pub const MAX_PAGE_WORDS: usize = 16384;
/// The longest text of one word.
pub const MAX_PAGE_WORD_BYTES: usize = 1024;

/// A word's box and text on the wire, `[x0,y0,x1,y1,"text"]`.
type RawWord = (u16, u16, u16, u16, String);

/// One word: its box, as fractions of the page as drawn (each `/ 65535`, the convention of a drawing's points, 0 at the
/// left and the top), and its text. The box has `left <= right` and `top <= bottom`; the text is at most
/// `MAX_PAGE_WORD_BYTES` bytes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "RawWord", into = "RawWord")]
pub struct PageWord {
    pub left: u16,
    pub top: u16,
    pub right: u16,
    pub bottom: u16,
    pub text: String,
}

impl TryFrom<RawWord> for PageWord {
    type Error = String;
    fn try_from((left, top, right, bottom, text): RawWord) -> Result<Self, String> {
        if left > right || top > bottom {
            return Err("a word's box is not ordered".to_string());
        }
        if text.len() > MAX_PAGE_WORD_BYTES {
            return Err(format!("a word of more than {MAX_PAGE_WORD_BYTES} bytes"));
        }
        Ok(PageWord { left, top, right, bottom, text })
    }
}

impl From<PageWord> for RawWord {
    fn from(w: PageWord) -> RawWord {
        (w.left, w.top, w.right, w.bottom, w.text)
    }
}

/// What a decoder reads before it checks the bounds a `PageWords` keeps.
#[derive(Deserialize)]
struct RawPageWords {
    page: u32,
    cut: bool,
    lines: Vec<Vec<PageWord>>,
}

/// The document `application/vnd.sot.page-words+json` is: `{"page":P,"cut":false,"lines":[[[x0,y0,x1,y1,"text"],...],...]}`.
/// `page` is the 1-based page; a line is a list of words in the reading order the PDF tool guessed, and a word's place in
/// the document, its index over all lines in order, is how a selection names it. `cut` says the plugin stopped before the
/// page's last words. No line is empty and there are at most `MAX_PAGE_WORDS` words in all. Decoding refuses a document
/// that breaks these or holds a word that breaks its own.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "RawPageWords")]
pub struct PageWords {
    pub page: u32,
    pub cut: bool,
    pub lines: Vec<Vec<PageWord>>,
}

impl TryFrom<RawPageWords> for PageWords {
    type Error = String;
    fn try_from(r: RawPageWords) -> Result<Self, String> {
        if r.page == 0 {
            return Err("page is not 1 or more".to_string());
        }
        if r.lines.iter().any(Vec::is_empty) {
            return Err("an empty line".to_string());
        }
        if r.lines.iter().map(Vec::len).sum::<usize>() > MAX_PAGE_WORDS {
            return Err(format!("more than {MAX_PAGE_WORDS} words"));
        }
        Ok(PageWords { page: r.page, cut: r.cut, lines: r.lines })
    }
}

#[cfg(test)]
#[path = "page_words_tests.rs"]
mod page_words_tests;
