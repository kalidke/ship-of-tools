//! The question-line convention, until the registry gains structured answer fields. The grammar is rows'.

use crate::pages::site::verbs::PageOption;

#[derive(Default)]
pub(crate) struct Parts {
    pub question: String,
    pub options: Vec<PageOption>,
    pub recommend: String,
    pub page: String,
}

/// Page first, then the first Options section, then Recommend[ed][:] [(]letter[)].
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
    for (at, _) in options.match_indices("Recommend") {
        let rest = options[at + "Recommend".len()..].strip_prefix("ed").unwrap_or(&options[at + "Recommend".len()..]);
        let rest = rest.trim_start().strip_prefix(':').unwrap_or(rest.trim_start()).trim_start();
        let rest = rest.strip_prefix('(').unwrap_or(rest);
        if let Some(letter) = rest.chars().next().filter(|letter| letter.is_ascii_alphabetic()) {
            out.recommend = letter.to_ascii_lowercase().to_string();
            options = &options[..at];
            break;
        }
    }
    let marks: Vec<_> = options.as_bytes().windows(3).enumerate()
        .filter(|(_, w)| w[0] == b'(' && w[1].is_ascii_alphabetic() && w[2] == b')')
        .map(|(at, w)| (at, (w[1] as char).to_string())).collect();
    for (i, (at, key)) in marks.iter().enumerate() {
        let end = marks.get(i + 1).map_or(options.len(), |(at, _)| *at);
        let text = options[at + 3..end].trim_matches(|c: char| c.is_whitespace() || c == ';' || c == '.');
        out.options.push(PageOption { key: key.clone(), text: text.to_string() });
    }
    out
}

#[cfg(test)]
#[path = "question_tests.rs"]
mod tests;
