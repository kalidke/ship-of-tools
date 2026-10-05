//! Test-only: the production sources a source-scanning test reads.

use std::path::Path;

/// Every non-test `.rs` file under `src/` as `(path relative to src/ with '/' separators, its text without test
/// modules and comment lines)`, sorted by path. Panics if it read no file, so a scan never passes on nothing.
pub(crate) fn production_sources() -> Vec<(String, String)> {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    let mut pending = vec![src.clone()];
    while let Some(dir) = pending.pop() {
        for entry in std::fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                pending.push(path);
            } else if path.extension().and_then(|e| e.to_str()) == Some("rs") {
                files.push(path);
            }
        }
    }
    files.sort();
    let mut out = Vec::new();
    for path in files {
        // Test files have no `#[cfg(test)] mod` wrapper for `without_test_modules` to strip.
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
        let in_tests_folder = path.strip_prefix(&src).is_ok_and(|rel| {
            rel.parent().is_some_and(|dirs| dirs.components().any(|c| c.as_os_str() == "tests"))
        });
        if in_tests_folder
            || name.ends_with("_tests.rs")
            || name == "tests.rs"
            || name == "test_support.rs"
            || name.starts_with("tests_")
            || name.contains("_tests_")
        {
            continue;
        }
        let rel = path.strip_prefix(&src).unwrap().to_string_lossy().replace('\\', "/");
        out.push((rel, without_test_modules(&std::fs::read_to_string(&path).unwrap())));
    }
    assert!(!out.is_empty(), "the scan read no source files under {}", src.display());
    out
}

/// `source` without its `#[cfg(test)]` modules and its comment lines.
/// A module ends at the first `}` line at its own indentation; counting
/// braces would miscount the ones inside string literals.
pub(crate) fn without_test_modules(source: &str) -> String {
    let mut out = String::new();
    let mut lines = source.lines().peekable();
    while let Some(line) = lines.next() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("//") {
            continue;
        }
        let next_is_mod = lines.peek().is_some_and(|next| {
            let next = next.trim_start();
            next.starts_with("mod ") || next.starts_with("pub mod ") || next.starts_with("pub(crate) mod ")
        });
        if trimmed == "#[cfg(test)]" && next_is_mod {
            let header = lines.next().unwrap_or_default();
            if header.trim_end().ends_with(';') || header.trim_end().ends_with('}') {
                continue;
            }
            let close = format!("{}}}", &header[..header.len() - header.trim_start().len()]);
            for skipped in lines.by_ref() {
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

