//! The question-line convention, until the registry gains structured answer fields. The grammar is rows'.

use crate::pages::site::verbs::PageOption;

#[derive(Default)]
pub(crate) struct Parts {
    pub question: String,
    pub options: Vec<PageOption>,
    pub recommend: String,
    pub page: String,
}

/// Page first, then the first Options section, then Recommend[ed][:] [(]letter[)] in any case, a leading "I" or "We"
/// left out of the last option. An option starts at a mark `(x)` whose letter follows the one before it (the first mark
/// any letter), so a letter repeated in an option's text stays text; an option with no text is left out.
pub(crate) fn parts(text: &str) -> Parts {
    let mut out = Parts::default();
    let mut text = text.to_string();
    let page = text.match_indices("Page:").find_map(|(at, _)| {
        let after = at + "Page:".len();
        let rest = &text[after..];
        if !rest.starts_with(char::is_whitespace) { return None; }
        let start = text.len() - rest.trim_start().len();
        let end = start + text[start..].find(char::is_whitespace).unwrap_or(text.len() - start);
        (start < end).then_some((at, start, end))
    });
    if let Some((at, start, end)) = page {
        out.page = text[start..end].strip_suffix('.').unwrap_or(&text[start..end]).to_string();
        text.replace_range(at..end, "");
    }
    let Some((question, options)) = text.split_once("Options:") else {
        out.question = text.trim().to_string();
        return out;
    };
    out.question = question.trim().to_string();
    let mut options = options;
    // ASCII lowercasing keeps every byte where it was, so an index into `lower` is one into `options`.
    let lower = options.to_ascii_lowercase();
    for (at, _) in lower.match_indices("recommend") {
        let after = at + "recommend".len();
        let rest = &options[after + if lower[after..].starts_with("ed") { 2 } else { 0 }..];
        let rest = rest.trim_start().strip_prefix(':').unwrap_or(rest.trim_start()).trim_start();
        let rest = rest.strip_prefix('(').unwrap_or(rest);
        if let Some(letter) = rest.chars().next().filter(|letter| letter.is_ascii_alphabetic()) {
            out.recommend = letter.to_ascii_lowercase().to_string();
            options = without_speaker(&options[..at]);
            break;
        }
    }
    let mut marks: Vec<(usize, u8)> = Vec::new();
    for (at, w) in options.as_bytes().windows(3).enumerate() {
        let follows = marks.last().map_or(true, |(_, last)| w[1].to_ascii_lowercase() == last.to_ascii_lowercase() + 1);
        if w[0] == b'(' && w[1].is_ascii_alphabetic() && w[2] == b')' && follows {
            marks.push((at, w[1]));
        }
    }
    for (i, (at, key)) in marks.iter().enumerate() {
        let end = marks.get(i + 1).map_or(options.len(), |(at, _)| *at);
        let text = options[at + 3..end].trim_matches(|c: char| c.is_whitespace() || c == ';' || c == '.');
        if !text.is_empty() {
            out.options.push(PageOption { key: (*key as char).to_string(), text: text.to_string() });
        }
    }
    out
}

/// Why a blocked question owes a decision page it does not name (`Page:`), or `None`. The maintainer, 2026-10-27: "we
/// need a decision page when I need more context than a sentence or two to make the decision", and a merge, a tag or
/// a registration (an `Irreversible:` clause, or an older line's `Pane only:`) always carries one. The context is the
/// question before `Options:`; more than two sentences of it owe a page.
pub(crate) fn page_owed(text: &str) -> Option<&'static str> {
    let p = parts(text);
    if !p.page.is_empty() {
        return None;
    }
    if text.contains("Irreversible:") || text.contains("Pane only:") {
        return Some("an Irreversible question (a merge, a tag, a registration) always carries a decision page");
    }
    (sentences(&p.question) > 2).then_some("the question runs past two sentences, so its context belongs on a decision page")
}

/// Sentences, counted by their ends: a `.`, `?` or `!` followed by a space or the end (a run such as "?!" or "..."
/// ends one; a version's "0.7.0" or a path's dot ends none).
fn sentences(text: &str) -> usize {
    let cs: Vec<char> = text.trim_end().chars().collect();
    (0..cs.len()).filter(|&i| matches!(cs[i], '.' | '?' | '!') && cs.get(i + 1).map_or(true, |c| c.is_whitespace())).count()
}

/// The options before a recommendation, without the word that opens it ("... measured. I recommend (a)").
fn without_speaker(head: &str) -> &str {
    let head = head.trim_end();
    for word in ["I", "i", "We", "we"] {
        if let Some(before) = head.strip_suffix(word) {
            if before.is_empty() || before.ends_with(|c: char| c.is_whitespace() || c == '.' || c == ';') {
                return before;
            }
        }
    }
    head
}

#[cfg(test)]
#[path = "question_tests.rs"]
mod tests;
