//! The one grammar for page text, an offered line and a confirmed line.

pub const MARK: &str = "[sot-page] ";
pub const MAX_TEXT: usize = 512;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refusal(String);

impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Refusal {}

/// Trim ASCII spaces, refuse unsafe or oversized text, and add the visible provenance mark.
pub fn line(text: &str) -> Result<String, Refusal> {
    let text = text.trim_matches(' ');
    if text.is_empty() {
        return Err(Refusal("reply text is empty".into()));
    }
    if text.len() > MAX_TEXT {
        return Err(Refusal(format!("reply text exceeds {MAX_TEXT} bytes")));
    }
    for c in text.chars() {
        if c.is_control() || matches!(c,
            '\u{2028}' | '\u{2029}' | '\u{200b}'..='\u{200f}' | '\u{202a}'..='\u{202e}' |
            '\u{2060}'..='\u{2069}' | '\u{feff}' | '\u{fff9}'..='\u{fffb}' | '\u{fffe}' | '\u{ffff}')
        {
            return Err(Refusal(format!("reply text contains U+{:04X}", u32::from(c))));
        }
    }
    Ok(format!("{MARK}{text}"))
}

/// A confirmed line must already be exactly the canonical output of `line`.
pub fn check_line(offered: &str) -> Result<(), Refusal> {
    let text = offered.strip_prefix(MARK).ok_or_else(|| Refusal("reply line lacks [sot-page] mark".into()))?;
    if line(text)? != offered {
        return Err(Refusal("reply line is not canonical".into()));
    }
    Ok(())
}

#[cfg(test)]
#[path = "page_reply_tests.rs"]
mod tests;
