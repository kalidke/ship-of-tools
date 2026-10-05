//! Test-only (feature `test-support`): the one walker of the workspace's Rust sources, shared by every source scan
//! (the inbox-append pin, the handle-binding scan, the permit scan, `update.rs`'s exit check).

use std::path::{Path, PathBuf};

fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for e in entries.flatten() {
        let p = e.path();
        if p.is_dir() {
            walk(&p, out);
        } else if p.extension().is_some_and(|x| x == "rs") {
            out.push(p);
        }
    }
}

/// Every `.rs` file under `src/` and `tests/` of every workspace member that rust/Cargo.toml lists (vt100 excluded),
/// as (repo-relative path with `/`, text). A new member is scanned because the list is read, not copied.
pub fn rust_sources() -> Vec<(String, String)> {
    let rust = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    let manifest = std::fs::read_to_string(rust.join("Cargo.toml")).expect("read the workspace manifest");
    let members = manifest.split("members = [").nth(1).and_then(|s| s.split(']').next()).expect("the workspace members");
    let mut files = Vec::new();
    for member in members.split(',').map(|m| m.trim().trim_matches('"')).filter(|m| !m.is_empty() && *m != "vt100") {
        for sub in ["src", "tests"] {
            walk(&rust.join(member).join(sub), &mut files);
        }
    }
    let out: Vec<(String, String)> = files
        .iter()
        .map(|f| {
            let rel = f.strip_prefix(&rust).expect("under rust/").to_string_lossy().replace('\\', "/");
            (format!("rust/{rel}"), std::fs::read_to_string(f).expect("read a source file"))
        })
        .collect();
    assert!(out.len() > 300, "the scan read only {} files", out.len());
    out
}

/// Whether an attribute line is a `#[cfg(...)]` whose predicate requires `test`: `cfg(test)`, or `cfg(all(...))`
/// with `test` as one operand.
fn requires_test(line: &str) -> bool {
    let Some(inner) = line.trim().strip_prefix("#[cfg(").and_then(|s| s.strip_suffix(")]")) else {
        return false;
    };
    if inner.trim() == "test" {
        return true;
    }
    let Some(operands) = inner.trim().strip_prefix("all(").and_then(|s| s.strip_suffix(')')) else {
        return false;
    };
    let mut depth = 0usize;
    let mut start = 0;
    let mut parts = Vec::new();
    for (i, c) in operands.char_indices() {
        match c {
            '(' => depth += 1,
            ')' => depth = depth.saturating_sub(1),
            ',' if depth == 0 => {
                parts.push(&operands[start..i]);
                start = i + 1;
            }
            _ => {}
        }
    }
    parts.push(&operands[start..]);
    parts.iter().any(|p| p.trim() == "test")
}

/// `text` without its test modules and its comment lines. A removed line is blanked, not deleted, so line N of the
/// view is line N of the file. A test module is a `#[cfg(...)]` that requires `test` followed at once by a `mod`,
/// and it ends at the first `}` line at its own indentation (counting braces would miscount the ones inside string
/// literals). A test item that is not a `mod` stays in as production text: a false alarm, never a miss.
pub fn without_test_modules(text: &str) -> String {
    let mut out = String::new();
    let mut lines = text.lines().peekable();
    while let Some(line) = lines.next() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("//") {
            out.push('\n');
            continue;
        }
        let next_is_mod = lines.peek().is_some_and(|next| {
            let next = next.trim_start();
            next.starts_with("mod ") || next.starts_with("pub mod ") || next.starts_with("pub(crate) mod ")
        });
        if requires_test(trimmed) && next_is_mod {
            out.push('\n');
            let header = lines.next().unwrap_or_default();
            out.push('\n');
            if header.trim_end().ends_with(';') || header.trim_end().ends_with('}') {
                continue;
            }
            let close = format!("{}}}", &header[..header.len() - header.trim_start().len()]);
            for skipped in lines.by_ref() {
                out.push('\n');
                if skipped.trim_end() == close {
                    break;
                }
            }
            continue;
        }
        out.push_str(line);
        out.push('\n');
    }
    out
}

/// Every non-test `.rs` file under a member's `src/` as (repo-relative path, its text through
/// [`without_test_modules`]). A test file is by name: inside a `tests` folder, `tests.rs`, `*_tests.rs`,
/// `test_support.rs`, `tests_*.rs` or `*_tests_*.rs`.
pub fn production_sources() -> Vec<(String, String)> {
    rust_sources()
        .into_iter()
        .filter(|(path, _)| {
            let Some((_, under_src)) = path.split_once("/src/") else { return false };
            let name = path.rsplit('/').next().unwrap_or("");
            let in_tests_folder = under_src.rsplit_once('/').is_some_and(|(dirs, _)| dirs.split('/').any(|d| d == "tests"));
            !(in_tests_folder
                || name.ends_with("_tests.rs")
                || name == "tests.rs"
                || name == "test_support.rs"
                || name.starts_with("tests_")
                || name.contains("_tests_"))
        })
        .map(|(path, text)| (path, without_test_modules(&text)))
        .collect()
}

/// Whether `c` can be part of an identifier.
pub fn is_ident(c: Option<char>) -> bool {
    c.is_some_and(|c| c.is_alphanumeric() || c == '_')
}

/// The last `fn <name>` or `struct <name>` that starts before `pos`.
pub fn enclosing(text: &str, pos: usize) -> String {
    let before = &text[..pos];
    let mut best: Option<(usize, String)> = None;
    for kw in ["fn ", "struct "] {
        let mut from = 0;
        while let Some(at) = before[from..].find(kw) {
            let at = from + at;
            from = at + kw.len();
            if is_ident(before[..at].chars().next_back()) {
                continue;
            }
            let name: String = before[at + kw.len()..].chars().take_while(|c| is_ident(Some(*c))).collect();
            if !name.is_empty() && best.as_ref().map_or(true, |(b, _)| at > *b) {
                best = Some((at, name));
            }
        }
    }
    best.map(|(_, n)| n).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn without_test_modules_keeps_line_numbers() {
        let text = "fn a() {}\n// a comment\n#[cfg(test)]\nmod tests {\n    fn t() {}\n}\nfn after() {}\n#[cfg(all(test, unix))]\nmod unix_tests {\n    fn u() {}\n}\nfn last() {}\n";
        let view = without_test_modules(text);
        assert_eq!(view.lines().count(), text.lines().count());
        assert_eq!(view.lines().nth(6), Some("fn after() {}"), "production text after a mid-file test module moved");
        assert_eq!(view.lines().nth(11), Some("fn last() {}"));
        assert!(!view.contains("fn t()") && !view.contains("fn u()"), "a test module stayed in: {view}");
    }

    /// No source scan cuts a file at its first test attribute: the text `#[cfg(test)]` followed at once by a double
    /// quote is what a string-literal cut on the attribute (`find`, `split`, `==`) contains, and such a cut misses
    /// every production line after a test-only item in the middle of a file.
    #[test]
    fn no_source_scan_cuts_at_a_test_attribute() {
        // Built with `concat!`, so this file does not hold the text it looks for.
        let needle = concat!("#[cfg(test)]", "\"");
        let hits: Vec<String> = rust_sources()
            .into_iter()
            // start.rs still carries its own copy of the rule until the merge of the release line replaces it.
            .filter(|(path, text)| path != "rust/log/src/test_scan.rs" && path != "rust/backend/src/rows/run/start.rs" && text.contains(needle))
            .map(|(path, _)| path)
            .collect();
        assert!(hits.is_empty(), "a scan cuts a file at its first test attribute (use test_scan::without_test_modules): {hits:?}");
    }
}
