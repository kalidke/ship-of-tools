//! Source guards for ADR 0049, User isolation: every TCP connection Ship of Tools accepts goes through
//! `sot_log::identity::peer_owner::serve_own`, which checks its owner before a byte is read. Each test walks the
//! production source of every Rust crate that opens a socket and names file:line for each breach.

use std::path::{Path, PathBuf};

/// The crates whose `src/` the guards read, under `rust/`.
const TREES: [&str; 5] = ["backend", "frontend", "log", "protocol", "updater"];

/// The only `.accept(` calls outside `serve_own`, each with its reason: neither is a TCP accept.
const ACCEPT_EXCEPTIONS: [(&str, &str); 3] = [
    ("log/src/identity/peer_owner/mod.rs", "`serve_own`, the one TCP accept loop"),
    ("backend/src/server/listen.rs", "the daemon's session socket or pipe, not TCP"),
    ("log/src/lane/socket_unix/accept.rs", "the lanes' Unix socket, not TCP"),
];

fn rust_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).parent().expect("sot-log sits under rust/").to_path_buf()
}

fn collect(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).unwrap_or_else(|e| panic!("read {}: {e}", dir.display())) {
        let path = entry.unwrap().path();
        if path.is_dir() {
            collect(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

/// A file that is test code as a whole: `tests.rs`, `*_tests.rs`, `test_support.rs`, or anything under a `tests` folder.
fn is_test_file(rel: &str) -> bool {
    let name = rel.rsplit('/').next().unwrap_or(rel);
    rel.split('/').any(|part| part == "tests")
        || name == "tests.rs"
        || name == "test_support.rs"
        || name.ends_with("_tests.rs")
}

/// Whether a cfg predicate holds only when `test` is set: `test`, or `all(..)` with such an operand.
fn requires_test(pred: &str) -> bool {
    let pred = pred.trim();
    if pred == "test" {
        return true;
    }
    let Some(inner) = pred.strip_prefix("all(").and_then(|p| p.strip_suffix(')')) else {
        return false;
    };
    let (mut depth, mut cur, mut operands) = (0i32, String::new(), Vec::new());
    for ch in inner.chars() {
        if ch == ',' && depth == 0 {
            operands.push(std::mem::take(&mut cur));
            continue;
        }
        depth += i32::from(ch == '(') - i32::from(ch == ')');
        cur.push(ch);
    }
    operands.push(cur);
    operands.iter().any(|o| requires_test(o))
}

fn is_test_cfg(attr: &str) -> bool {
    let attr = attr.trim();
    let attr = attr.split("//").next().unwrap_or(attr).trim_end();
    attr.strip_prefix("#[")
        .and_then(|a| a.trim_start().strip_prefix("cfg"))
        .and_then(|a| a.trim_start().strip_prefix('('))
        .and_then(|a| a.strip_suffix(")]"))
        .is_some_and(requires_test)
}

fn opens_mod(line: &str) -> bool {
    let line = match line.strip_prefix("pub") {
        Some(rest) if rest.starts_with('(') => rest.split_once(')').map_or(line, |(_, r)| r.trim_start()),
        Some(rest) if rest.starts_with(char::is_whitespace) => rest.trim_start(),
        _ => line,
    };
    line.strip_prefix("mod").is_some_and(|r| r.starts_with(char::is_whitespace))
}

/// The lines of `source` outside test modules: a column-0 `#[cfg(..)]` whose predicate requires `test`, followed by a
/// `mod` line, is skipped to the next line that is exactly `}` (or just the declaration when it ends in `;`).
fn production_lines(source: &str) -> Vec<(usize, &str)> {
    let lines: Vec<&str> = source.lines().collect();
    let mut keep = Vec::new();
    let mut k = 0;
    while k < lines.len() {
        if !lines[k].starts_with("#[") {
            keep.push((k + 1, lines[k]));
            k += 1;
            continue;
        }
        let mut j = k;
        while j < lines.len() && (lines[j].trim().is_empty() || lines[j].trim_start().starts_with("#[")) {
            j += 1;
        }
        let test_mod = j < lines.len()
            && opens_mod(lines[j])
            && lines[k..j].iter().any(|a| a.starts_with("#[") && is_test_cfg(a));
        if test_mod {
            let mut end = j;
            if !lines[j].trim_end().ends_with(';') {
                end = j + 1;
                while end < lines.len() && lines[end] != "}" {
                    end += 1;
                }
            }
            k = end + 1;
        } else {
            for (n, line) in lines.iter().enumerate().take(j).skip(k) {
                keep.push((n + 1, *line));
            }
            k = j;
        }
    }
    keep
}

/// Every production line of every source file the guards read, as `(path under rust/, line number, text)`; comment-only
/// lines are left out.
fn production_source() -> Vec<(String, usize, String)> {
    let root = rust_root();
    let mut files = Vec::new();
    for tree in TREES {
        collect(&root.join(tree).join("src"), &mut files);
    }
    assert!(files.len() >= 100, "the walk found only {} source files: is the layout still rust/<crate>/src?", files.len());
    let mut out = Vec::new();
    for file in files {
        let rel = file.strip_prefix(&root).unwrap().to_string_lossy().replace('\\', "/");
        if is_test_file(&rel) {
            continue;
        }
        let source = std::fs::read_to_string(&file).unwrap_or_else(|e| panic!("read {rel}: {e}"));
        for (n, line) in production_lines(&source) {
            if !line.trim_start().starts_with("//") {
                out.push((rel.clone(), n, line.to_string()));
            }
        }
    }
    out
}

/// ADR 0049, User isolation: no TCP accept loop but `serve_own`'s. A listener that accepts on its own reads or writes a
/// connection before its owner is known.
#[test]
fn no_tcp_accept_outside_peer_owner() {
    let breaches: Vec<String> = production_source()
        .into_iter()
        .filter(|(rel, _, line)| line.contains(".accept(") && !ACCEPT_EXCEPTIONS.iter().any(|(path, _)| path == rel))
        .map(|(rel, n, line)| format!("{rel}:{n}: {}", line.trim()))
        .collect();
    assert!(
        breaches.is_empty(),
        "a TCP accept outside `sot_log::identity::peer_owner::serve_own` (ADR 0049, User isolation); the only exceptions \
         are {ACCEPT_EXCEPTIONS:?}:\n{}",
        breaches.join("\n")
    );
}
