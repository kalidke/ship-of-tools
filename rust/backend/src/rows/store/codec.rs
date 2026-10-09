//! The hand toml codec of the row file: scalar and section readers, the canonical-key stripper, quote and unquote.

use std::collections::HashMap;

/// Hand-rolled scalar `key = "value"` parser scoped to *top-level*
/// (everything before the first `[section]`). Section bodies are
/// ignored so a section key with the same name as a canonical key
/// can't be mistaken for one. Numeric values (created, started) come
/// through as bare digits and are returned as the raw string.
pub(super) fn parse_kv(text: &str) -> HashMap<String, String> {
    let mut out = HashMap::new();
    for line in text.lines() {
        let t = line.trim();
        if t.is_empty() || t.starts_with('#') {
            continue;
        }
        if t.starts_with('[') && t.ends_with(']') {
            // Hit the first section — stop. The frontend's persisted
            // sections live below, and we don't want their keys to leak
            // into top-level resolution.
            break;
        }
        let Some((k, v)) = t.split_once('=') else { continue };
        out.insert(k.trim().to_string(), toml_unquote(strip_quotes(v.trim())));
    }
    out
}

/// Like `parse_kv` but scoped to a `[section]` block.
pub(super) fn parse_section(text: &str, section: &str) -> HashMap<String, String> {
    let mut out = HashMap::new();
    let mut in_section = false;
    for line in text.lines() {
        let t = line.trim();
        if t.is_empty() || t.starts_with('#') {
            continue;
        }
        if t.starts_with('[') && t.ends_with(']') {
            let name = &t[1..t.len() - 1];
            in_section = name == section;
            continue;
        }
        if !in_section {
            continue;
        }
        let Some((k, v)) = t.split_once('=') else { continue };
        out.insert(k.trim().to_string(), toml_unquote(strip_quotes(v.trim())));
    }
    out
}

/// Remove the canonical (top-level) `workspace_id/slug/label/project_root/
/// session_name/created` keys *and* the `[kernel]` section so we can
/// rewrite them. Everything else (e.g. `[nav_state]`, `[layout]`) is
/// preserved verbatim.
pub(super) fn strip_canonical_top_and_kernel(text: &str) -> String {
    const TOP_KEYS: &[&str] = &[
        "workspace_id",
        "slug",
        "label",
        "project_root",
        "session_name",
        // Pre-protocol-2 spelling, dropped on rewrite; deletable one
        // release after 0.6.0 final.
        "tmux_session",
        "created",
        "autostart_claude",
        "agent",
        "agent_name",
        "task",
        "runtime",
        "agent_handle",
        // `account` was MISSING here while `save` wrote it into the canonical
        // block below — so every save preserved the previous file's line and
        // appended it after the new one, one more copy each time. A row on this
        // box had reached 64 of them in 139 lines; every row file had at least
        // two. It went unnoticed because `parse_kv` is a hand-rolled reader
        // rather than a TOML parser, so duplicate keys never raised an error —
        // and because it takes the LAST value before a section, a freshly
        // written account was silently overruled by the stale copies beneath
        // it. That is not cosmetic: it would have reverted an account switch on
        // the next load while the running session looked correct.
        "account",
    ];
    let mut out = String::new();
    let mut in_top = true;
    let mut skipping_kernel = false;
    for line in text.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with('[') && trimmed.contains(']') {
            in_top = false;
            skipping_kernel = trimmed.starts_with("[kernel]");
            if skipping_kernel {
                continue;
            }
            out.push_str(line);
            out.push('\n');
            continue;
        }
        if skipping_kernel {
            continue;
        }
        if in_top {
            // Drop canonical top-level keys; preserve any others.
            if let Some((k, _)) = trimmed.split_once('=') {
                if TOP_KEYS.contains(&k.trim()) {
                    continue;
                }
            }
        }
        out.push_str(line);
        out.push('\n');
    }
    out
}

/// Strips the surrounding `"..."` only — no unescaping. Every reader that
/// pulls a string value out of a workspace toml pairs this with
/// `toml_unquote` (its inverse escapes are `toml_quote`'s), never used
/// alone: a bare `strip_quotes` reproduced `toml_quote`'s doubled
/// backslashes verbatim on load, the bug this pairing fixes (field defect
/// 2026-09-04 — see `toml_unquote`'s own doc).
fn strip_quotes(s: &str) -> &str {
    let s = s.trim();
    let b = s.as_bytes();
    if b.len() >= 2 && b[0] == b'"' && b[b.len() - 1] == b'"' {
        &s[1..s.len() - 1]
    } else {
        s
    }
}

pub(super) fn toml_quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for ch in s.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            _ => out.push(ch),
        }
    }
    out.push('"');
    out
}

/// Inverse of `toml_quote`, applied to the inside of the quotes
/// (`strip_quotes`'s output): `\\`→`\`, `\"`→`"`, `\n`, `\r`, `\t`. An
/// escape this doesn't recognize (`\U`, `\k`, a lone trailing `\`, …) is
/// kept verbatim as backslash+char rather than dropped. A value written by
/// an older build that never escaped loads unchanged only when it holds none
/// of those five sequences: a raw `C:\Users\tom` loads with a tab.
///
/// Field defect (2026-09-04): before this existed, `load_toml` fed
/// `strip_quotes`'s output straight through, so every saved value that
/// `toml_quote` had escaped loaded back with the escapes still literal —
/// a `project_root` containing `\` round-tripped as doubled backslashes
/// (harmless on Windows, which tolerates repeated separators, so this hid
/// for months) and a saved Windows verbatim root (`\\?\C:\...`, doubled by
/// the writer to `\\\\?\\C:\\...`) never matched `paths::simplify_verbatim`
/// at all, so `CreateProcess` rejected it as a working directory
/// (`capsule supervisor spawn failed: The directory name is invalid. (os
/// error 267)`). `simplify_verbatim` also grew a second, single-backslash
/// prefix form to match: pass this function's OWN output through the
/// unescaper once (a raw, never-escaped legacy write's leading `\\`
/// reads as one escaped backslash, "halving" `\\?\` to `\?\`) and you can
/// see why both shapes are real on-disk data now, not just one.
fn toml_unquote(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(ch) = chars.next() {
        if ch != '\\' {
            out.push(ch);
            continue;
        }
        match chars.next() {
            Some('\\') => out.push('\\'),
            Some('"') => out.push('"'),
            Some('n') => out.push('\n'),
            Some('r') => out.push('\r'),
            Some('t') => out.push('\t'),
            Some(other) => {
                out.push('\\');
                out.push(other);
            }
            None => out.push('\\'),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_kv_top_level_only() {
        let text = r#"
workspace_id = "x"
slug         = "alpha"
[kernel]
status = "stopped"
"#;
        let kv = parse_kv(text);
        assert_eq!(kv.get("workspace_id").map(String::as_str), Some("x"));
        assert!(kv.get("status").is_none()); // inside [kernel], not top
    }

    #[test]
    fn parse_section_scoped() {
        let text = r#"
workspace_id = "x"

[backend]
session_id = "y"
label = "MyPkg"
project_dir = "/p"
"#;
        let b = parse_section(text, "backend");
        assert_eq!(b.get("session_id").map(String::as_str), Some("y"));
        assert_eq!(b.get("label").map(String::as_str), Some("MyPkg"));
        assert!(b.get("workspace_id").is_none());
    }

    #[test]
    fn strip_canonical_keeps_other_sections() {
        let input = r#"workspace_id = "old"
slug         = "alpha"
label        = "Alpha"
project_root = "/p"
session_name = "sot-be-alpha"
created      = 1700000000

[kernel]
status = "stopped"

[nav_state]
mode = "files"
cursor_path = "src/lib.jl"
"#;
        let stripped = strip_canonical_top_and_kernel(input);
        assert!(!stripped.contains("workspace_id"));
        assert!(!stripped.contains("[kernel]"));
        assert!(!stripped.contains("status = \"stopped\""));
        assert!(stripped.contains("[nav_state]"));
        assert!(stripped.contains("cursor_path = \"src/lib.jl\""));
    }
}
