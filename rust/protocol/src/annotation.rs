//! The annotation header grammar: the `---` fences and `synced_against`, shared by the window and the daemon.

/// Split an annotation into its header and body, both slices of `s`. The header runs from the opening `---`
/// line through the first later `---` line, newline included when the file has one; the body is the rest.
/// A fence is a line that is `---` once trimmed, so `---x` closes nothing. With no opening fence or no closing
/// fence there is no header.
pub fn split_frontmatter(s: &str) -> Option<(&str, &str)> {
    let mut end = 0;
    for (n, line) in s.split_inclusive('\n').enumerate() {
        end += line.len();
        match (n, line.trim() == "---") {
            (0, false) => return None,
            (1.., true) => return Some(s.split_at(end)),
            _ => {}
        }
    }
    None
}

/// The first `synced_against` value in the header: the line trimmed (indentation and CRLF), the value trimmed,
/// one matching quote pair removed. An empty value is no hash; a file with no complete header has none.
pub fn synced_against(s: &str) -> Option<String> {
    let (header, _) = split_frontmatter(s)?;
    let value = header
        .lines()
        .find_map(|line| line.trim().strip_prefix("synced_against:"))?
        .trim();
    let value = [('"', '"'), ('\'', '\'')]
        .iter()
        .find_map(|&(open, close)| value.strip_prefix(open)?.strip_suffix(close))
        .unwrap_or(value)
        .trim();
    (!value.is_empty()).then(|| value.to_string())
}

#[cfg(test)]
#[path = "annotation_tests.rs"]
mod tests;
