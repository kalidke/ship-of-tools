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

#[cfg(test)]
mod tests {
    use super::*;

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
            ("rust/backend/src/topology/dial.rs", concat!("let _path_guard = EnvGuard::capture(\"", "PATH\");"), 2),
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
