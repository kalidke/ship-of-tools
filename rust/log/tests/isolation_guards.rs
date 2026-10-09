//! Remaining ADR 0049 source guards; Rust listener admission is checked by native behavior at the page, session and capsule owners.

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
