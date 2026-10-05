//! Test-only (feature `test-support`): the one walker of the workspace's Rust sources, shared by every source scan
//! (`test_log`, `test_exec`, and the scans that pin a rule on every test).

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
