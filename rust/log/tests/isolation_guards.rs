//! Source guards for ADR 0049, User isolation, that a lint cannot make. That every accept of the Rust processes is a
//! listed listener is a lint (`rust/clippy.toml`, rust.yml's "Disallowed methods" step) plus the list below: each test
//! walks the production source and names file:line for each breach.

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

/// The Rust listeners' allowed statements, one entry per `#[allow(clippy::disallowed_methods, reason = "listener:
/// <name>: <guard>")]` on a statement that accepts or constructs a listener, as (file under rust/, name): the one TCP
/// accept (`serve_own`), the daemon's session socket or pipe (its constructor and its accept), and the capsule's lane
/// socket and lane pipe (the pipe's accept and its constructor). Another account can reach none of the last three (a
/// private folder or an owner-only pipe), and `serve_own` checks the owner of every connection before a byte is
/// read. One entry per statement, so a second allowed accept in a listed file under a listed name fails too.
const RUST_LISTENERS: [(&str, &str); 6] = [
    ("log/src/identity/peer_owner/mod.rs", "page (TCP)"),
    ("backend/src/server/listen.rs", "session socket or pipe"),
    ("backend/src/server/listen.rs", "session socket or pipe"),
    ("log/src/lane/socket_unix/accept.rs", "capsule lane socket"),
    ("log/src/lane/pipe_win/accept.rs", "capsule lane pipe"),
    ("log/src/lane/pipe_win/registry.rs", "capsule lane pipe"),
];

/// What marks a statement as one that accepts.
const ACCEPT_MARKS: [&str; 8] =
    ["accept(", "incoming(", "ConnectNamedPipe(", "CreateNamedPipe", "create_tokio", "create_sync", "PipeListenerOptions", "ServerOptions"];

/// ADR 0049, User isolation: every accept of the Rust processes is a listed listener. A statement under a
/// `disallowed_methods` allow that accepts must be named `listener: <name>: <guard>` and be in `RUST_LISTENERS`, one
/// entry per statement, and the list holds no listener that is gone.
#[test]
fn every_rust_listener_is_listed() {
    let source = production_source();
    let mut found: Vec<(String, String)> = Vec::new();
    let mut unnamed = Vec::new();
    for (i, (rel, n, line)) in source.iter().enumerate() {
        if !line.contains("#[allow(clippy::disallowed_methods") {
            continue;
        }
        // The statement the allow covers: the following lines up to the one that ends it.
        let mut statement = String::new();
        for (r, _, l) in source[i + 1..].iter().take(14) {
            if r != rel {
                break;
            }
            statement.push_str(l);
            statement.push('\n');
            if l.trim_end().ends_with(';') {
                break;
            }
        }
        if !ACCEPT_MARKS.iter().any(|m| statement.contains(m)) {
            continue; // an allow for something else (a connector): not a listener
        }
        let reason = line.split("reason = \"").nth(1).and_then(|r| r.split('"').next()).unwrap_or("");
        match reason.strip_prefix("listener: ").and_then(|r| r.split_once(": ")) {
            Some((name, _guard)) => found.push((rel.clone(), name.to_string())),
            None => unnamed.push(format!("{rel}:{n}: {}", line.trim())),
        }
    }
    assert!(unnamed.is_empty(), "an allowed accept without a `listener: <name>: <guard>` reason:\n{}", unnamed.join("\n"));
    found.sort();
    let mut expected: Vec<(String, String)> =
        RUST_LISTENERS.iter().map(|(f, n)| (f.to_string(), n.to_string())).collect();
    expected.sort();
    assert_eq!(found, expected, "the listeners of the Rust processes changed: list the new one (ADR 0049, User isolation)");
}

/// Count the lines of the files under `dirs` (below the repository root) with one of `files`' extensions that hold one of
/// `marks`, skipping comment lines (those starting with `comment`) and any path with a test folder; as `(path, count)`.
fn marked_lines(dirs: &[&str], extensions: &[&str], comment: &str, marks: &[&str]) -> Vec<(String, usize)> {
    let repo = rust_root().parent().expect("rust/ sits in the repository").to_path_buf();
    let mut files = Vec::new();
    for dir in dirs {
        let path = repo.join(dir);
        if path.is_dir() {
            collect_with(&path, extensions, &mut files);
        }
    }
    assert!(!files.is_empty(), "no {extensions:?} files under {dirs:?}: is the layout still what the guard reads?");
    let mut out = Vec::new();
    for file in files {
        let rel = file.strip_prefix(&repo).unwrap().to_string_lossy().replace('\\', "/");
        if rel.split('/').any(|part| part == "test" || part == "tests" || part == "node_modules") {
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
        } else if path.extension().is_some_and(|e| extensions.iter().any(|x| e == *x)) {
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
        &["listen(", "listenany(", "HTTP.serve", "HTTP.listen", "Bonito.Server", "Pluto.run", "addprocs", "Malt.Worker"],
    );
    let mut node_marks = vec!["createServer(", ".listen("];
    let modules = ["net", "http", "https", "http2", "dgram", "tls"];
    let quoted: Vec<String> =
        modules.iter().flat_map(|m| ["'", "\""].iter().flat_map(move |q| [format!("{q}{m}{q}"), format!("{q}node:{m}{q}")])).collect();
    node_marks.extend(quoted.iter().map(String::as_str));
    let node = marked_lines(
        &["rust/backend/sidecars/mathjax", "rust/log/claude-sdk-helper/src"],
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
