//! Slack's inline styles in a message's text: `*bold*`, `_italic_`, `~strike~` and `` `code` ``, read over wire's one
//! tokenizer (`text_segments`), whose tokens (a mention, a link) are each one non-space atom here. Styling is the window's
//! presentation, so it is this module's and no other reader's. Pure.

use sot_protocol::chat::TextSegment;

pub(super) const BOLD: u8 = 1;
pub(super) const ITALIC: u8 = 2;
pub(super) const STRIKE: u8 = 4;
pub(super) const CODE: u8 = 8;

/// A run of shown text in one style (the bits above); `markup` is a token, kept whole.
#[derive(Debug, PartialEq)]
pub(super) struct Piece {
    pub text: String,
    pub markup: bool,
    pub style: u8,
}

enum Atom {
    Ch(char),
    Tok(String),
}

fn space(a: &Atom) -> bool {
    matches!(a, Atom::Ch(c) if c.is_whitespace())
}

/// Where a style may open after and close before: the text's end, whitespace or ASCII punctuation. A token is neither.
fn edge(a: Option<&Atom>) -> bool {
    a.is_none_or(|a| matches!(a, Atom::Ch(c) if c.is_whitespace() || c.is_ascii_punctuation()))
}

/// The text's pieces with its inline styles applied: the markers leave, and a marker no rule styles stays as typed.
pub(super) fn styled(segments: impl IntoIterator<Item = TextSegment>) -> Vec<Piece> {
    let atoms: Vec<Atom> = segments.into_iter().flat_map(|s| match s {
        TextSegment::Text(t) => t.chars().map(Atom::Ch).collect(),
        TextSegment::Markup(t) => vec![Atom::Tok(t)],
    }).collect();
    let mut out = Vec::new();
    parse(&atoms, 0, &mut out);
    out
}

/// `a` joins the last piece when both are text in one style; `flat` shows a token as its text (inside code).
fn push(out: &mut Vec<Piece>, a: &Atom, style: u8, flat: bool) {
    let (text, markup) = match a {
        Atom::Ch(c) => (c.to_string(), false),
        Atom::Tok(t) => (t.clone(), !flat),
    };
    match out.last_mut() {
        Some(p) if !markup && !p.markup && p.style == style => p.text.push_str(&text),
        _ => out.push(Piece { text, markup, style }),
    }
}

/// The marker closing the `c` at `open`: the nearest `c` on its line that follows a non-space and precedes an edge, and is
/// not next to the opener (an empty style never closes).
fn close(atoms: &[Atom], open: usize, c: char) -> Option<usize> {
    let line = atoms[open..].iter().position(|a| matches!(a, Atom::Ch('\n'))).map_or(atoms.len(), |n| open + n);
    (open + 2..line).find(|&j| matches!(atoms[j], Atom::Ch(x) if x == c) && !space(&atoms[j - 1]) && edge(atoms.get(j + 1)))
}

fn parse(atoms: &[Atom], style: u8, out: &mut Vec<Piece>) {
    let mut i = 0;
    while i < atoms.len() {
        let c = if let Atom::Ch(c) = atoms[i] { c } else { '\0' };
        let bit = match c { '*' => BOLD, '_' => ITALIC, '~' => STRIKE, '`' => CODE, _ => 0 };
        if bit == CODE && matches!(atoms.get(i + 1), Some(Atom::Ch('`'))) {
            // A run of backticks is a fence, literal until code blocks are drawn.
            let run = atoms[i..].iter().take_while(|a| matches!(a, Atom::Ch('`'))).count();
            (i..i + run).for_each(|k| push(out, &atoms[k], style, false));
            i += run;
            continue;
        }
        let opens = bit != 0 && edge(atoms.get(i.wrapping_sub(1))) && atoms.get(i + 1).is_some_and(|a| !space(a));
        if let Some(j) = opens.then(|| close(atoms, i, c)).flatten() {
            if bit == CODE {
                atoms[i + 1..j].iter().for_each(|a| push(out, a, style | CODE, true));
            } else {
                parse(&atoms[i + 1..j], style | bit, out);
            }
            i = j + 1;
        } else {
            push(out, &atoms[i], style, false);
            i += 1;
        }
    }
}
