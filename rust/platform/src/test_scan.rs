//! Test-only (feature `test-support`): the one walker of the workspace's Rust sources, shared by every source scan.

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

/// Whether a path (`rust/<member>/...`) is a test file: inside a `tests` folder, or named `tests.rs`, `test_support.rs`,
/// `*_tests.rs`, `tests_*.rs`, `*_tests_*.rs` or `test_*.rs` (the last covers the `test-support` modules themselves).
fn is_test_path(rel: &str) -> bool {
    let name = rel.rsplit('/').next().unwrap_or("");
    let in_tests_folder = rel.rsplit_once('/').is_some_and(|(dirs, _)| dirs.split('/').any(|d| d == "tests"));
    in_tests_folder
        || name == "tests.rs"
        || name == "test_support.rs"
        || name.ends_with("_tests.rs")
        || name.starts_with("tests_")
        || name.contains("_tests_")
        || name.starts_with("test_")
}

/// Whether an attribute line is `#[cfg(test)]`, or `#[cfg(all(..))]` with `test` among its top-level arguments.
fn requires_test(attr: &str) -> bool {
    let Some(inner) = attr.trim().strip_prefix("#[cfg(").and_then(|s| s.strip_suffix(")]")) else {
        return false;
    };
    if inner.trim() == "test" {
        return true;
    }
    let Some(args) = inner.trim().strip_prefix("all(").and_then(|s| s.strip_suffix(')')) else {
        return false;
    };
    let mut depth = 0usize;
    let mut start = 0;
    let mut parts = Vec::new();
    for (i, c) in args.char_indices() {
        match c {
            '(' => depth += 1,
            ')' => depth = depth.saturating_sub(1),
            ',' if depth == 0 => {
                parts.push(&args[start..i]);
                start = i + 1;
            }
            _ => {}
        }
    }
    parts.push(&args[start..]);
    parts.iter().any(|p| p.trim() == "test")
}

/// `source` without its test modules and its comment lines.
/// A module ends at the first `}` line at its own indentation; counting
/// braces would miscount the ones inside string literals. A removed line becomes an empty line, so line N of the
/// result is line N of `source`.
pub fn without_test_modules(source: &str) -> String {
    let mut out = String::new();
    let mut lines = source.lines().peekable();
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
            let header = lines.next().unwrap_or_default();
            out.push_str("\n\n");
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

/// Every non-test `.rs` file under a member's `src/` (`is_test_path` is false), each text through
/// [`without_test_modules`], sorted by path (`rust/<member>/src/...`). Panics if it read too few files, so a scan
/// never passes on nothing.
pub fn production_sources() -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = rust_sources()
        .into_iter()
        .filter(|(path, _)| path.contains("/src/") && !is_test_path(path))
        .map(|(path, text)| (path, without_test_modules(&text)))
        .collect();
    out.sort();
    assert!(out.len() > 100, "the production scan read only {} files", out.len());
    out
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
#[path = "test_scan_tests.rs"]
mod tests;
