//! Slack's inline styles in a message's text: `*bold*`, `_italic_`, `~strike~` and `` `code` ``, read over wire's one
//! tokenizer (`text_segments`), whose tokens (a mention, a link) are each one non-space atom here, shown as Slack shows them
//! (`resolve`). Styling is the window's presentation, so it is this module's and no other reader's. Pure.

use sot_protocol::chat::TextSegment;
use sot_protocol::page_reply::unsafe_char;

pub(super) const BOLD: u8 = 1;
pub(super) const ITALIC: u8 = 2;
pub(super) const STRIKE: u8 = 4;
pub(super) const CODE: u8 = 8;

/// What a piece is: typed text (and a date's fallback), a link's label or URL, a mention's name, or a token not read, which
/// shows as its decoded text.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) enum Role {
    Text,
    Link,
    Mention,
    Markup,
}

/// A run of shown text in one style (the bits above); a token is a piece of its own, kept whole.
#[derive(Debug, PartialEq)]
pub(super) struct Piece {
    pub text: String,
    pub role: Role,
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

/// A label on one line: a line end is a space and any other control U+FFFD.
fn one_line(s: &str) -> String {
    s.chars().map(|c| match c { '\n' | '\r' => ' ', c if c.is_control() => '\u{fffd}', c => c }).collect()
}

/// A token as Slack shows it, with its role: a link its label or its URL; a user (`<@U..>`, `<@W..>`) `@` and the label or the
/// id; a channel `#` and the label or the id; `<!here>`, `<!channel>` and `<!everyone>` by name; a group its label; a date its
/// fallback. A name loses the invisible and direction characters a header's name loses, and one left empty shows the id. A
/// token not so (or one without what it shows) is its decoded text, as typed.
fn resolve(tok: &str) -> (String, Role) {
    let shown = || {
        let inner = tok.strip_prefix('<')?.strip_suffix('>')?;
        let (head, label) = inner.split_once('|').map_or((inner, None), |(h, l)| (h, Some(l)));
        // The label on one line, and for a name without the invisible and direction characters; `None` when none is left.
        let line = |name: bool| {
            let l: String = label.map(one_line)?.chars().filter(|c| !name || !unsafe_char(*c)).collect();
            (!l.is_empty()).then_some(l)
        };
        let id = |i: &str, first: &[char]| i.starts_with(first) && i.bytes().all(|b| b.is_ascii_alphanumeric());
        let mention = |sigil, i: &str| (format!("{sigil}{}", line(true).as_deref().unwrap_or(i)), Role::Mention);
        Some(match (head.get(..1)?, head.get(1..)?) {
            ("@", i) if id(i, &['U', 'W']) => mention('@', i),
            ("#", i) if id(i, &['C']) => mention('#', i),
            ("!", s @ ("here" | "channel" | "everyone")) => (format!("@{s}"), Role::Mention),
            ("!", s) if s.starts_with("subteam^") => (line(true)?, Role::Mention),
            ("!", s) if s.starts_with("date^") => (line(false)?, Role::Text),
            _ if ["http://", "https://", "mailto:"].iter().any(|p| head.strip_prefix(p).is_some_and(|u| !u.is_empty())) => {
                (line(false).unwrap_or_else(|| one_line(head)), Role::Link)
            }
            _ => return None,
        })
    };
    shown().unwrap_or_else(|| (tok.to_string(), Role::Markup))
}

/// `a` joins the last piece when both are text in one style; `flat` shows a token as its decoded text (inside code).
fn push(out: &mut Vec<Piece>, a: &Atom, style: u8, flat: bool) {
    let (text, role) = match a {
        Atom::Ch(c) => (c.to_string(), Role::Text),
        Atom::Tok(t) if flat => (t.clone(), Role::Text),
        Atom::Tok(t) => resolve(t),
    };
    match out.last_mut() {
        Some(p) if role == Role::Text && p.role == Role::Text && p.style == style => p.text.push_str(&text),
        _ => out.push(Piece { text, role, style }),
    }
}

// The atoms the closer scans have stepped over on this thread, for the test that holds the parse linear.
#[cfg(test)]
thread_local!(static SCANNED: std::cell::Cell<usize> = const { std::cell::Cell::new(0) });
#[cfg(test)]
pub(super) fn scanned_reset() {
    SCANNED.with(|s| s.set(0));
}
#[cfg(test)]
pub(super) fn scanned() -> usize {
    SCANNED.with(|s| s.get())
}

/// The marker closing the `c` at `open`: the nearest `c` before `eol` (the line's end) that follows a non-space and precedes an
/// edge, and is not next to the opener (an empty style never closes).
fn close(atoms: &[Atom], open: usize, c: char, eol: usize) -> Option<usize> {
    (open + 2..eol).find(|&j| {
        #[cfg(test)]
        SCANNED.with(|s| s.set(s.get() + 1));
        matches!(atoms[j], Atom::Ch(x) if x == c) && !space(&atoms[j - 1]) && edge(atoms.get(j + 1))
    })
}

fn parse(atoms: &[Atom], style: u8, out: &mut Vec<Piece>) {
    // `eol` is the end of the line `i` is on, found once a line; `failed` holds, per marker, the line a scan for its closer
    // failed on: a closer for a later opener is one for this, so no later opener of that marker on that line scans again.
    let (mut i, mut eol, mut failed) = (0, 0, [usize::MAX; 4]);
    while i < atoms.len() {
        if i >= eol {
            eol = atoms[i..].iter().position(|a| matches!(a, Atom::Ch('\n'))).map_or(atoms.len(), |n| i + n);
        }
        let c = if let Atom::Ch(c) = atoms[i] { c } else { '\0' };
        let bit = match c { '*' => BOLD, '_' => ITALIC, '~' => STRIKE, '`' => CODE, _ => 0 };
        if bit == CODE && matches!(atoms.get(i + 1), Some(Atom::Ch('`'))) {
            // A run of backticks at an opener is a fence, literal until code blocks are drawn.
            let run = atoms[i..].iter().take_while(|a| matches!(a, Atom::Ch('`'))).count();
            (i..i + run).for_each(|k| push(out, &atoms[k], style, false));
            i += run;
            continue;
        }
        // An opener is followed by a non-space that is not its own marker (`**x**` opens at the second asterisk).
        let opens = bit != 0 && edge(atoms.get(i.wrapping_sub(1))) && atoms.get(i + 1).is_some_and(|a| !space(a) && !matches!(a, Atom::Ch(x) if *x == c));
        let slot = bit.trailing_zeros() as usize;
        let closer = if opens && failed[slot] != eol { close(atoms, i, c, eol) } else { None };
        if let Some(j) = closer {
            if bit == CODE {
                atoms[i + 1..j].iter().for_each(|a| push(out, a, style | CODE, true));
            } else {
                parse(&atoms[i + 1..j], style | bit, out);
            }
            i = j + 1;
        } else {
            if opens {
                failed[slot] = eol;
            }
            push(out, &atoms[i], style, false);
            i += 1;
        }
    }
}
