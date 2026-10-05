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
mod tests {
    use super::*;

    #[test]
    fn without_test_modules_keeps_code_below_a_mid_file_test_module() {
        let text = "fn a() {}\n// a comment\n#[cfg(test)]\nmod t {\n    fn t() {}\n}\nfn after() {}\nfn last() {}\n";
        let view = without_test_modules(text);
        assert_eq!(view.lines().count(), text.lines().count());
        assert_eq!(view.lines().nth(6), Some("fn after() {}"), "code after a mid-file test module moved");
        assert_eq!(view.lines().nth(7), Some("fn last() {}"));
        assert_eq!((view.lines().nth(2), view.lines().nth(3), view.lines().nth(5)), (Some(""), Some(""), Some("")));
        assert!(!view.contains("fn t()"), "the test module stayed in: {view}");
    }

    #[test]
    fn a_cfg_all_test_module_is_blanked() {
        let text = "fn a() {}\n#[cfg(all(test, unix))]\nmod t {\n    fn t() {}\n}\nfn after() {}\n";
        let view = without_test_modules(text);
        assert_eq!(view.lines().count(), text.lines().count());
        assert_eq!(view.lines().nth(5), Some("fn after() {}"));
        assert!(!view.contains("fn t()"), "the cfg(all(test, ..)) module stayed in: {view}");
        let kept = "#[cfg(all(unix, windows))]\nmod m {\n    fn m() {}\n}\n";
        assert!(without_test_modules(kept).contains("fn m()"), "a cfg without test was removed");
    }

    #[test]
    fn production_sources_reads_no_test_path() {
        let files = production_sources();
        assert!(files.iter().all(|(path, _)| !is_test_path(path)), "a test path was returned");
        assert!(files.iter().any(|(path, _)| path == "rust/backend/src/main.rs"), "main.rs is not among them");
    }

    /// No scan cuts a file at its first `#[cfg(test)]`: the text of that attribute followed at once by a double
    /// quote is what a string-literal cut (`find`, `split`, `==`) contains, and such a cut misses every production
    /// line after a test-only item in the middle of a file. It sees a cut where the attribute text is followed at
    /// once by a double quote. A literal that runs on past the attribute (`"#[cfg(test)]\n…"`) is not seen; that
    /// limit is accepted (M4 recheck 3, N2).
    #[test]
    fn no_scan_cuts_a_file_at_its_first_cfg_test() {
        // Built with `concat!`, so this file does not hold the text it looks for.
        let needle = concat!("#[cfg(test)]", "\"");
        let hits: Vec<String> = rust_sources()
            .into_iter()
            .filter(|(path, text)| path != "rust/log/src/test_scan.rs" && text.contains(needle))
            .map(|(path, _)| path)
            .collect();
        assert!(hits.is_empty(), "a scan cuts a file at its first test attribute (use test_scan::without_test_modules): {hits:?}");
    }

    /// No test takes the system folders out of the process `PATH` or changes `SHELL`, so a bare program name always
    /// resolves; code under test takes both from its caller. The one `PATH` writer left puts a stub `ssh` first.
    #[test]
    fn no_test_changes_the_process_path_or_shell() {
        // Built with `concat!`, so this file does not hold the texts it looks for.
        let words = [
            concat!("set_var(\"", "PATH\""),
            concat!("remove_var(\"", "PATH\""),
            concat!("capture(\"", "PATH\")"),
            concat!("set_var(\"", "SHELL\""),
            concat!("remove_var(\"", "SHELL\""),
            concat!("capture(\"", "SHELL\")"),
        ];
        // The two prepend writers and their guards, by trimmed line and count.
        let allowed: [(&str, &str, usize); 5] = [
            ("rust/backend/src/topology/dial.rs", concat!("std::env::set_var(\"", "PATH\", std::env::join_paths(std::iter::once(dir.to_path_buf()).chain(std::env::split_paths(&real))).expect(\"join PATH\"));"), 1),
            ("rust/backend/src/topology/dial.rs", concat!("let _path_guard = EnvGuard::capture(\"", "PATH\");"), 3),
            ("rust/backend/src/comm/mail/forward.rs", concat!("let _path_guard = EnvGuard::capture(\"", "PATH\");"), 1),
            ("rust/backend/tests/lane_bridge/dial.rs", concat!("std::env::set_var(\"", "PATH\", new_path);"), 1),
            ("rust/backend/tests/lane_bridge/dial.rs", concat!("std::env::set_var(\"", "PATH\", &self.0);"), 1),
        ];
        let mut found = Vec::new();
        for (rel, text) in rust_sources() {
            if rel == "rust/log/src/test_scan.rs" {
                continue;
            }
            let mut used = std::collections::HashMap::new();
            for (n, line) in text.lines().enumerate() {
                if !words.iter().any(|w| line.contains(w)) {
                    continue;
                }
                let seen = used.entry(line.trim().to_string()).or_insert(0usize);
                *seen += 1;
                let room = allowed.iter().find(|(f, l, _)| *f == rel && *l == line.trim()).map_or(0, |(_, _, c)| *c);
                if *seen > room {
                    found.push(format!("{rel}:{}: {}", n + 1, line.trim()));
                }
            }
        }
        assert!(found.is_empty(), "a test changes the process PATH or SHELL:\n{}", found.join("\n"));
    }
}
