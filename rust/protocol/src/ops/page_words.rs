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
/// `MAX_PAGE_WORD_BYTES` bytes, not empty, and holds no control character, U+2028 or U+2029.
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
        if text.is_empty() || text.chars().any(|c| c.is_control() || c == '\u{2028}' || c == '\u{2029}') {
            return Err("a word that is empty or holds a control character or a line separator".to_string());
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
struct RawPageWords {
    page: u32,
    cut: bool,
    lines: Vec<Vec<PageWord>>,
}

/// Read as the one object the document is: a derived reader would take `[2,false,[]]` for it too. Each of the three
/// fields is required and at most once, and a field this reader does not know is ignored.
impl<'de> Deserialize<'de> for RawPageWords {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        use serde::de::{Error, IgnoredAny, MapAccess, Visitor};
        struct Object;
        impl<'de> Visitor<'de> for Object {
            type Value = RawPageWords;
            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("an object with page, cut and lines")
            }
            fn visit_map<A: MapAccess<'de>>(self, mut m: A) -> Result<RawPageWords, A::Error> {
                let (mut page, mut cut, mut lines) = (None, None, None);
                // A field named twice is refused, as a derived reader would: the second must not quietly replace the first.
                fn once<'de, T: Deserialize<'de>, A: MapAccess<'de>>(slot: &mut Option<T>, name: &'static str, m: &mut A) -> Result<(), A::Error> {
                    if slot.is_some() {
                        return Err(A::Error::duplicate_field(name));
                    }
                    *slot = Some(m.next_value()?);
                    Ok(())
                }
                while let Some(key) = m.next_key::<String>()? {
                    match key.as_str() {
                        "page" => once(&mut page, "page", &mut m)?,
                        "cut" => once(&mut cut, "cut", &mut m)?,
                        "lines" => once(&mut lines, "lines", &mut m)?,
                        _ => drop(m.next_value::<IgnoredAny>()?),
                    }
                }
                Ok(RawPageWords {
                    page: page.ok_or_else(|| A::Error::missing_field("page"))?,
                    cut: cut.ok_or_else(|| A::Error::missing_field("cut"))?,
                    lines: lines.ok_or_else(|| A::Error::missing_field("lines"))?,
                })
            }
        }
        d.deserialize_map(Object)
    }
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

impl PageWords {
    /// The words in all lines.
    pub fn word_count(&self) -> usize {
        self.lines.iter().map(Vec::len).sum()
    }

    /// The text of the words `first..=last`, numbered over all lines in order: the words a line gives joined by one space
    /// and the lines by LF (a line only some of whose words are in the range gives those). The one definition of the text
    /// a selection of words is: what the window copies, and what the share's digest is of. `None` when `first` is after
    /// `last` or `last` is no word of the document.
    pub fn text(&self, first: usize, last: usize) -> Option<String> {
        if first > last || last >= self.word_count() {
            return None;
        }
        let (mut out, mut at) = (String::new(), 0);
        for line in &self.lines {
            let (lo, hi) = (first.max(at), last.min((at + line.len()).saturating_sub(1)));
            if !line.is_empty() && lo <= hi {
                if !out.is_empty() {
                    out.push('\n');
                }
                out.push_str(&line[lo - at..=hi - at].iter().map(|w| w.text.as_str()).collect::<Vec<_>>().join(" "));
            }
            at += line.len();
        }
        Some(out)
    }
}

#[cfg(test)]
#[path = "page_words_tests.rs"]
mod page_words_tests;
