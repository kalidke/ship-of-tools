//! Slack's inline styles in a message's text: `*bold*`, `_italic_`, `~strike~` and `` `code` ``, read over wire's one
//! tokenizer (`text_segments`), whose tokens (a mention, a link) are each one non-space atom here, shown as Slack shows them
//! (`resolve`); and its blocks, a fenced code block and a quote (`>` lines, or `>>>` and the rest), whose pieces carry the bits
//! below. Styling is the window's presentation, so it is this module's and no other reader's. Pure.

use sot_protocol::chat::{ChatName, TextSegment};
use sot_protocol::page_reply::unsafe_char;

pub(super) const BOLD: u8 = 1;
pub(super) const ITALIC: u8 = 2;
pub(super) const STRIKE: u8 = 4;
pub(super) const CODE: u8 = 8;
/// What a piece is part of, beside its styles: a quote line; a code block, which takes no style or token; every other code
/// block, so two that meet stay two panels; the one space an empty line inside either is shaped as, which a copy leaves out.
pub(super) const QUOTE: u8 = 16;
pub(super) const BLOCK: u8 = 32;
pub(super) const ALT: u8 = 64;
pub(super) const FILL: u8 = 128;

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

/// A token carries, for a person's or a channel's, the name the record holds for its id, cleaned (`named`).
enum Atom {
    Ch(char),
    Tok(String, Option<String>),
}

fn space(a: &Atom) -> bool {
    matches!(a, Atom::Ch(c) if c.is_whitespace())
}

/// Where a style may open after and close before: the text's end, whitespace or ASCII punctuation. A token is neither.
fn edge(a: Option<&Atom>) -> bool {
    a.is_none_or(|a| matches!(a, Atom::Ch(c) if c.is_whitespace() || c.is_ascii_punctuation()))
}

/// The text's pieces: blocks found first (a fence opens a code block and the next closes it, across lines; an unclosed one is
/// as typed), then each line outside them read as a quote or as text with its inline styles applied. The markers leave, and a
/// marker no rule styles stays as typed. A block starts and ends its own line.
pub(super) fn styled(segments: impl IntoIterator<Item = TextSegment>, names: &[ChatName]) -> Vec<Piece> {
    let atoms: Vec<Atom> = segments.into_iter().flat_map(|s| match s {
        TextSegment::Text(t) => t.chars().map(Atom::Ch).collect(),
        TextSegment::Markup(t) => { let name = named(&t, names); vec![Atom::Tok(t, name)] }
    }).collect();
    let mut out = Vec::new();
    // `at` starts the text not yet shown, `quoted` is whether a `>>>` quotes the rest, and `blocks` counts the blocks shown.
    let (mut at, mut quoted, mut blocks) = (0, false, 0_u32);
    loop {
        // A fence with no closing one leaves the rest as text, and no later fence is scanned for.
        let found = fence(&atoms, at).and_then(|open| Some((open, fence(&atoms, open + 3)?)));
        let mut outside = &atoms[at..found.map_or(atoms.len(), |f| f.0)];
        // The text after a block ends the block's line, with its own line end if it starts with one.
        if blocks > 0 && !outside.is_empty() {
            let rest = &outside[usize::from(matches!(outside[0], Atom::Ch('\n')))..];
            if !rest.is_empty() {
                push(&mut out, &Atom::Ch('\n'), 0, false);
            }
            outside = rest;
        }
        lines(outside, &mut quoted, &mut out);
        let Some((open, close)) = found else { return out };
        if out.last().is_some_and(|p| !p.text.ends_with('\n')) {
            push(&mut out, &Atom::Ch('\n'), 0, false);
        }
        // One line end after the opening fence and one before the closing one are the fences', not the code's.
        let lf = |a: Option<&Atom>| matches!(a, Some(Atom::Ch('\n')));
        let code = &atoms[open + 3..close];
        let code = if lf(code.first()) { &code[1..] } else { code };
        let code = if lf(code.last()) { &code[..code.len() - 1] } else { code };
        blocks += 1;
        let style = BLOCK | if blocks % 2 == 0 { ALT } else { 0 };
        for (n, line) in code.split(|a| matches!(a, Atom::Ch('\n'))).enumerate() {
            if n > 0 {
                push(&mut out, &Atom::Ch('\n'), style, true);
            }
            line.iter().for_each(|a| push(&mut out, a, style, true));
            if line.is_empty() {
                push(&mut out, &Atom::Ch(' '), style | FILL, true);
            }
        }
        at = close + 3;
    }
}

/// The first fence (three backticks) at or after `from`.
fn fence(atoms: &[Atom], from: usize) -> Option<usize> {
    (from..atoms.len().saturating_sub(2)).find(|&i| {
        #[cfg(test)]
        SCANNED.with(|s| s.set(s.get() + 1));
        atoms[i..i + 3].iter().all(|a| matches!(a, Atom::Ch('`')))
    })
}

/// Text outside the blocks, a line at a time: the line's end is a piece of its own, and a final one ends no further line.
fn lines(atoms: &[Atom], quoted: &mut bool, out: &mut Vec<Piece>) {
    let mut at = 0;
    while at < atoms.len() {
        let end = atoms[at..].iter().position(|a| matches!(a, Atom::Ch('\n'))).map_or(atoms.len(), |n| at + n);
        line(&atoms[at..end], quoted, out);
        if end < atoms.len() {
            push(out, &Atom::Ch('\n'), 0, false);
        }
        at = end + 1;
    }
}

/// One line: a quote line if it starts with `>` (shown without it and one space), or with `>>>`, which quotes this line and every
/// one after it outside the blocks. An empty quote line is one flagged space; text and tokens work inside a quote.
fn line(atoms: &[Atom], quoted: &mut bool, out: &mut Vec<Piece>) {
    let is = |i: usize, c: char| matches!(atoms.get(i), Some(Atom::Ch(x)) if *x == c);
    let skip = |i: usize| i + usize::from(is(i, ' '));
    let start = if *quoted {
        Some(0)
    } else if (0..3).all(|i| is(i, '>')) {
        *quoted = true;
        Some(skip(3))
    } else {
        is(0, '>').then(|| skip(1))
    };
    match start {
        None => parse(atoms, 0, out),
        Some(s) if s >= atoms.len() => push(out, &Atom::Ch(' '), QUOTE | FILL, false),
        Some(s) => parse(&atoms[s..], QUOTE, out),
    }
}

/// A label on one line: a line end is a space and any other control U+FFFD.
fn one_line(s: &str) -> String {
    s.chars().map(|c| match c { '\n' | '\r' => ' ', c if c.is_control() => '\u{fffd}', c => c }).collect()
}

/// The name `names` holds for a person's or a channel's token (`<@U..>`, `<@W..>`, `<#C..>`, `<#G..>`), without the
/// invisible and direction characters a header's name loses and trimmed; `None` for any other token, an id with no entry,
/// or a name nothing visible is left of. `resolve` applies it only to a token of the id's own kind.
fn named(tok: &str, names: &[ChatName]) -> Option<String> {
    let id = tok.strip_prefix("<@").or_else(|| tok.strip_prefix("<#"))?.strip_suffix('>')?.split('|').next()?;
    let name: String = names.iter().find(|n| n.id == id)?.name.chars().filter(|c| !unsafe_char(*c)).collect();
    Some(name.trim().to_string()).filter(|n| !n.is_empty())
}

/// A token as Slack shows it, with its role: a link its label or its URL; a user (`<@U..>`, `<@W..>`) `@` and the name, else
/// the label, else the id; a channel (`<#C..>`, `<#G..>`) `#` and the name, else the label, else the id; `<!here>`, `<!channel>` and `<!everyone>` by name; a group its label; a date its
/// fallback. A name loses the invisible and direction characters a header's name loses, and one left empty shows the id. A
/// token not so (or one without what it shows) is its decoded text, as typed.
fn resolve(tok: &str, name: Option<&str>) -> (String, Role) {
    let shown = || {
        let inner = tok.strip_prefix('<')?.strip_suffix('>')?;
        let (head, label) = inner.split_once('|').map_or((inner, None), |(h, l)| (h, Some(l)));
        // The label on one line, and for a name without the invisible and direction characters; `None` when none is left.
        let line = |name: bool| {
            let l: String = label.map(one_line)?.chars().filter(|c| !name || !unsafe_char(*c)).collect();
            (!l.is_empty()).then_some(l)
        };
        let id = |i: &str, first: &[char]| i.starts_with(first) && i.bytes().all(|b| b.is_ascii_alphanumeric());
        let mention = |sigil, i: &str, name: Option<&str>| (format!("{sigil}{}", name.or(line(true).as_deref()).unwrap_or(i)), Role::Mention);
        Some(match (head.get(..1)?, head.get(1..)?) {
            ("@", i) if id(i, &['U', 'W']) => mention('@', i, name),
            ("#", i) if id(i, &['C', 'G']) => mention('#', i, name),
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
        Atom::Tok(t, _) if flat => (t.clone(), Role::Text),
        Atom::Tok(t, name) => resolve(t, name.as_deref()),
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
            // A run of backticks at an opener is a fence `styled` found no closing one for, as typed.
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
