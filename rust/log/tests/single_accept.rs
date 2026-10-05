//! Source guards for ADR 0049, User isolation. That every TCP accept goes through
//! `sot_log::identity::peer_owner::serve_own` is a lint, not a text search (`rust/clippy.toml`, rust.yml's "TCP accepts"
//! step); what a lint cannot see is here: each test walks the production source of every Rust crate that opens a socket
//! and names file:line for each breach.

use std::path::{Path, PathBuf};

/// The crates whose `src/` the guards read, under `rust/`.
const TREES: [&str; 5] = ["backend", "frontend", "log", "protocol", "updater"];

/// The platform opener literals only `browser_open.rs` may spell.
const OPENER_LITERALS: [&str; 8] = [
    "Command::new(\"xdg-open\")",
    "Command::new(\"open\")",
    "Command::new(\"rundll32\")",
    "Command::new(\"explorer\")",
    "Command::new(\"explorer.exe\")",
    "\"xdg-open\"",
    "\"rundll32\"",
    "\"explorer.exe\"",
];
const OPENER_HOME: &str = "frontend/src/browser_open.rs";

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

/// Every line of every non-test source file the guards read, as `(path under rust/, line number, text)`; comment-only
/// lines are left out. A file is test code as a whole or not at all: an inline test module is read like the rest.
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
        for (n, line) in source.lines().enumerate() {
            if !line.trim_start().starts_with("//") {
                out.push((rel.clone(), n + 1, line.to_string()));
            }
        }
    }
    out
}

/// ADR 0049, User isolation: no code but the page opener starts a browser. A page address on an opener's command line
/// is readable by other accounts, so every served page goes through `browser_open::open_page`.
#[test]
fn no_browser_opener_outside_browser_open() {
    let breaches: Vec<String> = production_source()
        .into_iter()
        .filter(|(rel, _, line)| rel != OPENER_HOME && OPENER_LITERALS.iter().any(|lit| line.contains(lit)))
        .map(|(rel, n, line)| format!("{rel}:{n}: {}", line.trim()))
        .collect();
    assert!(
        breaches.is_empty(),
        "a browser opener outside {OPENER_HOME} (ADR 0049, User isolation):\n{}",
        breaches.join("\n")
    );
}

/// ADR 0049, User isolation: no page secret is put on a command line. Pluto's and `wglshow`'s secrets are minted
/// inside their Julia children, so no argument that spawns one may name a secret, token, nonce or address, and the
/// opener (the one place an address is passed) is `browser_open.rs`'s, which hands the browser only its own redirect.
#[test]
fn no_secret_is_passed_as_a_command_line_argument() {
    let breaches: Vec<String> = production_source()
        .into_iter()
        .filter(|(rel, _, line)| {
            let lower = line.to_ascii_lowercase();
            rel != OPENER_HOME
                && (lower.contains(".arg(") || lower.contains(".args("))
                && ["secret", "token", "nonce", "url"].iter().any(|word| lower.contains(word))
        })
        .map(|(rel, n, line)| format!("{rel}:{n}: {}", line.trim()))
        .collect();
    assert!(
        breaches.is_empty(),
        "a command-line argument that names a secret, token, nonce or address (ADR 0049, User isolation):\n{}",
        breaches.join("\n")
    );
}
