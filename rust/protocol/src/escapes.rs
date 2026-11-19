//! Terminal escape sequences in text a program printed: the one grammar for reading them and for taking them out.
//!
//! The grammar is ECMA-48's, in one place: ESC `[` is a CSI sequence (parameter bytes 0x30-0x3F, intermediate bytes
//! 0x20-0x2F, one final byte 0x40-0x7E); ESC `]` (OSC), `P` (DCS), `X` (SOS), `^` (PM) and `_` (APC) open a string that
//! ends at ST (ESC `\`), an OSC also at BEL; ESC and intermediate bytes 0x20-0x2F and a final byte 0x30-0x7E is a
//! sequence (`ESC ( B`), and so is ESC and one final byte 0x30-0x7E (`ESC 7`, `ESC c`, a stray ST). A sequence is at
//! most [`MAX_SEQUENCE`] bytes, ESC through its last byte: one that is longer, or that the text ends in the middle of,
//! is data from its ESC on (and a string opener's introducer is data with it), and the scan goes on after that ESC.
//! A stream cut into chunks can keep from splitting a sequence by holding back the tail the chunk ends in the middle of
//! ([`open_tail`]); `julia/repl/test/escape_cases.tsv` and `escape_random.tsv` hold the cases for that rule and for
//! the reading of whole sequences. Every scan here is linear in the text.

use std::borrow::Cow;

/// The longest sequence, in bytes from its ESC through its last byte. Longer is data.
pub const MAX_SEQUENCE: usize = 4096;

/// A piece of text a program printed, split at its whole escape sequences.
#[derive(Debug, PartialEq, Eq)]
pub enum Piece<'a> {
    /// Text with no sequence in it. A lone ESC (the last byte, or before a control or non-ASCII character), a sequence
    /// cut off at the end of the text and one past [`MAX_SEQUENCE`] are text: such a byte is data a program printed.
    Text(&'a str),
    /// A CSI sequence (a color is final byte `m` with its numbers in `params`, a cursor move another).
    Csi { params: &'a str, final_byte: char },
    /// Any other whole sequence: a string (a title, a link) or an ESC form.
    Other,
}

/// Where the next occurrence of a byte pattern is, searched forward only, so that however many openers ask, the text
/// is read once: a found position is reused until the scan passes it, and "none after here" is kept.
struct Seek {
    searched_from: usize,
    found: Option<usize>,
    known: bool,
}

impl Seek {
    fn new() -> Self {
        Seek { searched_from: 0, found: None, known: false }
    }

    /// The first position at or after `from` where `hit` holds.
    fn find(&mut self, bytes: &[u8], from: usize, hit: impl Fn(&[u8], usize) -> bool) -> Option<usize> {
        if self.known && from >= self.searched_from && self.found.is_none_or(|p| p >= from) {
            return self.found;
        }
        self.found = (from..bytes.len()).find(|&i| hit(bytes, i));
        (self.searched_from, self.known) = (from, true);
        self.found
    }
}

/// What the scan finds at an ESC.
enum Found<'a> {
    /// A whole sequence, and where it ends.
    Whole(Piece<'a>, usize),
    /// The text ends in the middle of what could still be a sequence within the bound: data now, and the part a
    /// stream cutter must hold for the next chunk.
    Open,
    /// Data: no sequence starts here, whatever follows.
    Data,
}

/// The one forward pass: the pieces, and where the first sequence the text ends in the middle of begins.
fn scan(text: &str) -> (Vec<Piece<'_>>, Option<usize>) {
    let bytes = text.as_bytes();
    let (mut out, mut text_from, mut i, mut open) = (Vec::new(), 0, 0, None);
    let (mut bel, mut st) = (Seek::new(), Seek::new());
    while let Some(off) = bytes[i..].iter().position(|&b| b == 0x1b) {
        let pos = i + off;
        let at = |k: usize| bytes.get(pos + k).copied();
        let limit = (pos + MAX_SEQUENCE).min(bytes.len());
        // The text ends here with room left in the bound: what is unfinished could still be finished.
        let ends_open = || if bytes.len() - pos < MAX_SEQUENCE { Found::Open } else { Found::Data };
        let found = match at(1) {
            None => Found::Open,
            Some(b'[') => {
                let params_end = (pos + 2..limit).find(|&j| !(0x30..=0x3f).contains(&bytes[j])).unwrap_or(limit);
                let end = (params_end..limit).find(|&j| !(0x20..=0x2f).contains(&bytes[j])).unwrap_or(limit);
                if end == limit {
                    ends_open()
                } else if (0x40..=0x7e).contains(&bytes[end]) {
                    Found::Whole(Piece::Csi { params: &text[pos + 2..params_end], final_byte: bytes[end] as char }, end + 1)
                } else {
                    Found::Data
                }
            }
            Some(intro @ (b']' | b'P' | b'X' | b'^' | b'_')) => {
                let by_st = st.find(bytes, pos + 2, |b, k| b[k] == 0x1b && b.get(k + 1) == Some(&b'\\')).map(|k| k + 2);
                let by_bel = if intro == b']' { bel.find(bytes, pos + 2, |b, k| b[k] == 0x07).map(|k| k + 1) } else { None };
                match by_st.into_iter().chain(by_bel).min() {
                    None => ends_open(),
                    Some(end) if end - pos <= MAX_SEQUENCE => Found::Whole(Piece::Other, end),
                    Some(_) => Found::Data,
                }
            }
            Some(0x20..=0x2f) => {
                let end = (pos + 2..limit).find(|&j| !(0x20..=0x2f).contains(&bytes[j])).unwrap_or(limit);
                if end == limit {
                    ends_open()
                } else if (0x30..=0x7e).contains(&bytes[end]) {
                    Found::Whole(Piece::Other, end + 1)
                } else {
                    Found::Data
                }
            }
            Some(0x30..=0x7e) => Found::Whole(Piece::Other, pos + 2),
            _ => Found::Data,
        };
        match found {
            Found::Whole(piece, end) => {
                if pos > text_from {
                    out.push(Piece::Text(&text[text_from..pos]));
                }
                out.push(piece);
                (text_from, i) = (end, end);
            }
            // Data from this ESC on: the scan goes on after it (after a string's introducer, which no later opener
            // can be part of).
            Found::Open | Found::Data => {
                if matches!(found, Found::Open) {
                    open.get_or_insert(pos);
                }
                i = pos + if matches!(at(1), Some(b']' | b'P' | b'X' | b'^' | b'_')) { 2 } else { 1 };
            }
        }
    }
    if text_from < text.len() {
        out.push(Piece::Text(&text[text_from..]));
    }
    (out, open)
}

/// `text` split into its text and its whole sequences, in order.
pub fn pieces(text: &str) -> Vec<Piece<'_>> {
    scan(text).0
}

/// Where the tail begins that a stream must hold back when `text` is the end of a chunk: the first ESC at a sequence
/// boundary that starts what the text ends in the middle of (a CSI without its final byte, a string without its end, an
/// ESC alone), if what follows it is shorter than [`MAX_SEQUENCE`] so that the next chunk can still finish it; else
/// `text.len()`. Everything before it is whole, so a chunk cut there never splits a sequence, and the held tail,
/// read again with the next chunk after it, is cut where the whole text would be.
pub fn open_tail(text: &str) -> usize {
    scan(text).1.unwrap_or(text.len())
}

/// `text` without its escape sequences. Whatever is not a whole sequence stays as it is, a lone ESC included:
/// removing it would change what a program printed. Text with no ESC comes back borrowed.
pub fn strip_escapes(text: &str) -> Cow<'_, str> {
    if !text.contains('\u{1b}') {
        return Cow::Borrowed(text);
    }
    Cow::Owned(pieces(text).into_iter().filter_map(|p| if let Piece::Text(t) = p { Some(t) } else { None }).collect())
}

#[cfg(test)]
#[path = "escapes_tests.rs"]
mod tests;
