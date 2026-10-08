//! Remaining ADR 0049 source guards; Rust listener admission is checked by native behavior at the page, session and capsule owners.

use std::path::{Path, PathBuf};

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
const OPENER_HOME: &str = "rust/frontend/src/browser_open.rs";

fn rust_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("sot-log sits under rust/")
        .to_path_buf()
}

/// Every line of the production source, as `(repo-relative path, line number, text)`: the workspace's one
/// definition, `sot_log::test_scan::production_sources()`, which leaves out test files, test modules and comment
/// lines. Empty lines are skipped.
fn production_source() -> Vec<(String, usize, String)> {
    let mut out = Vec::new();
    for (rel, text) in sot_log::test_scan::production_sources() {
        for (n, line) in text.lines().enumerate() {
            if !line.trim().is_empty() {
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
        .filter(|(rel, _, line)| {
            rel != OPENER_HOME && OPENER_LITERALS.iter().any(|lit| line.contains(lit))
        })
        .map(|(rel, n, line)| format!("{rel}:{n}: {}", line.trim()))
        .collect();
    assert!(
        breaches.is_empty(),
        "a browser opener outside {OPENER_HOME} (ADR 0049, User isolation):\n{}",
        breaches.join("\n")
    );
}

/// ADR 0049, User isolation (MAC-ID): macOS reads a peer's account only from the credential the kernel cached for the
/// connection. Pin the production `LOCAL_PEERTOKEN`/`TOK_EUID` spellings and every whitespace-normalized `.val`
/// access in the private `AuditToken` module to pid/pidversion, including the index constants. This is a source
/// spelling guard, not an analysis of arbitrary equivalent Rust or numeric socket-option calls; tests are excluded.
#[test]
fn the_macos_peer_token_has_one_reader() {
    let source = production_source();
    let found: Vec<(String, String)> = source
        .iter()
        .filter(|(_, _, line)| line.contains("LOCAL_PEERTOKEN") || line.contains("TOK_EUID"))
        .map(|(rel, _, line)| (rel.clone(), line.trim().to_string()))
        .collect();
    let at = "rust/log/src/identity/challenge_macos.rs".to_string();
    let expected = vec![
        (at.clone(), "libc::LOCAL_PEERTOKEN,".to_string()),
        (
            at.clone(),
            r#"format!("LOCAL_PEERTOKEN returned {len} bytes, not a whole audit_token_t"),"#
                .to_string(),
        ),
    ];
    assert_eq!(
        found, expected,
        "an unlisted macOS peer-token spelling or production TOK_EUID"
    );
    let module: Vec<String> = source
        .iter()
        .filter(|(rel, _, _)| rel == &at)
        .map(|(_, _, line)| {
            line.chars()
                .filter(|c| !c.is_whitespace())
                .collect::<String>()
        })
        .collect();
    let indices: Vec<&str> = module
        .iter()
        .filter(|line| line.starts_with("constTOK_"))
        .map(String::as_str)
        .collect();
    assert_eq!(
        indices,
        vec!["constTOK_PID:usize=5;", "constTOK_PIDVERSION:usize=7;",],
        "a changed pid/pidversion index or an added token-word constant"
    );
    // Join before matching, so splitting the field or index across lines cannot hide an access.
    let compact = module.concat();
    let words: Vec<&str> = compact
        .split(".val")
        .skip(1)
        .map(|tail| tail.split(']').next().expect("split has at least one part"))
        .collect();
    assert_eq!(
        words,
        vec!["[TOK_PID", "[TOK_PIDVERSION", "[TOK_PIDVERSION"],
        "an unlisted token-word access (only pid/pidversion indices are allowed)"
    );
}

/// Count the lines of the files under `dirs` (below the repository root) with one of `files`' extensions that hold one of
/// `marks`, skipping comment lines (those starting with `comment`) and any path with a test folder; as `(path, count)`.
fn marked_lines(
    dirs: &[&str],
    extensions: &[&str],
    comment: &str,
    marks: &[&str],
) -> Vec<(String, usize)> {
    let repo = rust_root()
        .parent()
        .expect("rust/ sits in the repository")
        .to_path_buf();
    let mut files = Vec::new();
    for dir in dirs {
        let path = repo.join(dir);
        if path.is_dir() {
            collect_with(&path, extensions, &mut files);
        }
    }
    assert!(
        !files.is_empty(),
        "no {extensions:?} files under {dirs:?}: is the layout still what the guard reads?"
    );
    let mut out = Vec::new();
    for file in files {
        let rel = file
            .strip_prefix(&repo)
            .unwrap()
            .to_string_lossy()
            .replace('\\', "/");
        if rel
            .split('/')
            .any(|part| part == "test" || part == "tests" || part == "node_modules")
        {
            continue;
        }
        let text = std::fs::read_to_string(&file).unwrap_or_default();
        let count = text
            .lines()
            .filter(|l| !l.trim_start().starts_with(comment) && marks.iter().any(|m| l.contains(m)))
            .count();
        if count > 0 {
            out.push((rel, count));
        }
    }
    out.sort();
    out
}

fn collect_with(dir: &Path, extensions: &[&str], out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).unwrap_or_else(|e| panic!("read {}: {e}", dir.display())) {
        let path = entry.unwrap().path();
        if path.is_dir() {
            collect_with(&path, extensions, out);
        } else if path
            .extension()
            .is_some_and(|e| extensions.iter().any(|x| e == *x))
        {
            out.push(path);
        }
    }
}

/// ADR 0049, User isolation: the listeners the Julia children and the Node helpers open in their source are listed, by
/// plain substring: Pluto's probe (bind then close) and its server, and `wglshow`'s Bonito server. Nothing else in
/// `julia/`, `src/`, `core/`, the MathJax helper or the Claude SDK helper may listen.
#[test]
fn every_julia_and_node_listener_is_listed() {
    let julia = marked_lines(
        &["julia", "src", "core"],
        &["jl"],
        "#",
        &[
            "listen(",
            "listenany(",
            "HTTP.serve",
            "HTTP.listen",
            "Bonito.Server",
            "Pluto.run",
            "addprocs",
            "Malt.Worker",
        ],
    );
    let mut node_marks = vec!["createServer(", ".listen("];
    let modules = ["net", "http", "https", "http2", "dgram", "tls"];
    let quoted: Vec<String> = modules
        .iter()
        .flat_map(|m| {
            ["'", "\""]
                .iter()
                .flat_map(move |q| [format!("{q}{m}{q}"), format!("{q}node:{m}{q}")])
        })
        .collect();
    node_marks.extend(quoted.iter().map(String::as_str));
    let node = marked_lines(
        &[
            "rust/backend/sidecars/mathjax",
            "rust/log/claude-sdk-helper/src",
        ],
        &["mjs", "js", "ts"],
        "//",
        &node_marks,
    );
    let found: Vec<(String, usize)> = julia.into_iter().chain(node).collect();
    let expected: Vec<(String, usize)> = vec![
        ("julia/pluto/start.jl".to_string(), 3), // pick_port's two binds and Pluto.run!
        ("julia/repl/src/wgl.jl".to_string(), 1), // page_server's Bonito.Server
    ];
    assert_eq!(found, expected, "the listeners of the Julia children or the Node helpers changed (ADR 0049, User isolation)");
}
