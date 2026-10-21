use super::ConceptStore;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static COUNTER: AtomicU64 = AtomicU64::new(0);

/// Per-test scratch project dir, removed on drop. A REAL directory (not
/// the old fake `"/p"` string) is required now: `target_to_path`'s
/// symlink-escape guard canonicalizes, which needs the root to actually
/// exist. Mirrors `files/tree.rs`'s `Tmp` helper.
struct Tmp(PathBuf);
impl Tmp {
    fn new() -> Self {
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        let p = std::env::temp_dir().join(format!(
            "sot-concept-test-{}-{}-{}",
            std::process::id(),
            n,
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&p).unwrap();
        Tmp(p)
    }
    fn path(&self) -> &Path {
        &self.0
    }
}
impl Drop for Tmp {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn target_to_path_appends_md_when_missing() {
    let dir = Tmp::new();
    let s = ConceptStore::new(dir.path());
    let root = std::fs::canonicalize(dir.path()).unwrap();
    assert_eq!(
        s.target_to_path("modules/MyModule").unwrap(),
        root.join(".concept/modules/MyModule.md")
    );
}

#[test]
fn target_to_path_preserves_source_extension() {
    let dir = Tmp::new();
    let s = ConceptStore::new(dir.path());
    let root = std::fs::canonicalize(dir.path()).unwrap();
    // Multi-lang namespace: `files/foo.rs` and `files/foo.py` should
    // not collapse to the same on-disk file. Append `.md` rather than
    // replacing the extension.
    assert_eq!(
        s.target_to_path("files/foo.rs").unwrap(),
        root.join(".concept/files/foo.rs.md")
    );
    assert_eq!(
        s.target_to_path("files/foo.py").unwrap(),
        root.join(".concept/files/foo.py.md")
    );
}

#[test]
fn target_to_path_no_double_md() {
    let dir = Tmp::new();
    let s = ConceptStore::new(dir.path());
    let root = std::fs::canonicalize(dir.path()).unwrap();
    // Target already ending in `.md` (e.g. a caller that wrote out the
    // suffix explicitly) shouldn't get a `.md.md` filename.
    assert_eq!(
        s.target_to_path("modules/MyModule.md").unwrap(),
        root.join(".concept/modules/MyModule.md")
    );
}

#[test]
fn target_to_path_rejects_parent_segment() {
    let dir = Tmp::new();
    let s = ConceptStore::new(dir.path());
    assert!(s.target_to_path("../escape").is_err());
    assert!(s.target_to_path("a/../b").is_err());
}

#[test]
fn target_to_path_rejects_absolute() {
    let dir = Tmp::new();
    let s = ConceptStore::new(dir.path());
    assert!(s.target_to_path("/etc/passwd").is_err());
}

/// Security regression: a symlink INSIDE `.concept/` pointing OUTSIDE the
/// project must not resolve — the `..`/absolute-path string checks can't
/// catch this, only the canonicalize+confine guard can. Unix-only.
#[test]
#[cfg(unix)]
fn target_to_path_rejects_symlink_escape() {
    let dir = Tmp::new();
    let outside = Tmp::new();
    std::fs::write(outside.path().join("secret.md"), b"outside").unwrap();
    std::fs::create_dir_all(dir.path().join(".concept")).unwrap();
    std::os::unix::fs::symlink(
        outside.path(),
        dir.path().join(".concept").join("escape"),
    )
    .unwrap();

    let s = ConceptStore::new(dir.path());
    assert!(s.target_to_path("escape/secret").is_err());
}

/// A brand-new (not-yet-created) target under a legitimately-in-project
/// `.concept/` must still resolve normally, including on the VERY FIRST
/// concept write for a project (where `.concept/` doesn't exist yet) —
/// the escape guard must not false-reject either case.
#[test]
fn target_to_path_allows_new_target_before_concept_dir_exists() {
    let dir = Tmp::new();
    // Deliberately do NOT create `.concept/` — this is the first write.
    let s = ConceptStore::new(dir.path());
    let root = std::fs::canonicalize(dir.path()).unwrap();
    assert_eq!(
        s.target_to_path("modules/Fresh").unwrap(),
        root.join(".concept/modules/Fresh.md")
    );
}

/// `list()` on a fresh project (no `.concept/` on disk yet) must return
/// an empty list, not an error — same "don't false-reject the first
/// write" concern as the `target_to_path` case above.
#[test]
fn list_is_empty_for_fresh_project() {
    let dir = Tmp::new();
    let s = ConceptStore::new(dir.path());
    assert_eq!(s.list().unwrap(), Vec::<String>::new());
}

/// `list()` on a normal, real `.concept/` directory with annotation
/// files returns their target strings.
#[test]
fn list_returns_targets_for_real_concept_dir() {
    let dir = Tmp::new();
    let s = ConceptStore::new(dir.path());
    s.write("modules/MyModule", "content").unwrap();
    s.write("functions/myfn", "content").unwrap();
    assert_eq!(
        s.list().unwrap(),
        vec!["functions/myfn".to_string(), "modules/MyModule".to_string()]
    );
}

/// Security regression: `.concept` ITSELF (not a target inside it) being
/// a symlink out of the project must not be silently walked. Distinct
/// from `target_to_path_rejects_symlink_escape` above, which plants the
/// symlink INSIDE a real `.concept/`; here the top-level directory the
/// old code passed straight to `walk()` is the escaping link. Unix-only.
#[test]
#[cfg(unix)]
fn list_rejects_symlinked_concept_dir() {
    let dir = Tmp::new();
    let outside = Tmp::new();
    std::fs::write(outside.path().join("secret.md"), b"outside").unwrap();
    std::os::unix::fs::symlink(outside.path(), dir.path().join(".concept")).unwrap();

    let s = ConceptStore::new(dir.path());
    assert!(s.list().is_err());
}

/// The stale-write guard reads the header the window hashed, whatever its line endings or indentation.
#[test]
fn stored_header_hash_is_read_in_every_shape() {
    let dir = Tmp::new();
    let s = ConceptStore::new(dir.path());
    let cases = [
        ("lf", "---\ntarget: x\nsynced_against: h1\n---\nbody\n"),
        (
            "crlf",
            "---\r\ntarget: x\r\nsynced_against: h1\r\n---\r\nbody\r\n",
        ),
        (
            "indented",
            "---\ntarget: x\n    synced_against: h1\n---\nbody\n",
        ),
        (
            "quoted with trailing space",
            "---\nsynced_against: \"h1\"  \n---\nbody\n",
        ),
        ("closing fence at eof", "---\nsynced_against: h1\n---"),
    ];
    for (label, content) in cases {
        s.write(label, content).unwrap();
        assert_eq!(
            s.read_synced_against(label).unwrap().as_deref(),
            Some("h1"),
            "{label}"
        );
    }
    s.write("no-close", "---\nsynced_against: h1\nbody\n")
        .unwrap();
    assert_eq!(s.read_synced_against("no-close").unwrap(), None);
}
