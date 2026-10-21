use super::*;
use std::sync::atomic::{AtomicU64, Ordering};

static COUNTER: AtomicU64 = AtomicU64::new(0);

struct Tmp(PathBuf);
impl Tmp {
    fn new() -> Self {
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        let p = std::env::temp_dir().join(format!(
            "sot-fm-test-{}-{}-{}",
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
fn path_to_node_id_round_trips() {
    let dir = Tmp::new();
    let fm = FilesMode::new(dir.path().to_path_buf()).unwrap();
    std::fs::create_dir_all(dir.path().join("src/inner")).unwrap();
    std::fs::write(dir.path().join("src/inner/lib.rs"), b"//").unwrap();
    let abs = std::fs::canonicalize(dir.path().join("src/inner/lib.rs")).unwrap();
    let id = fm.path_to_node_id(&abs).expect("inside root");
    assert!(id.starts_with("files:"));
    let back = fm.node_id_to_path(&id).unwrap();
    // The round-trip yields the DE-VERBATIMED form by design (the root is
    // simplified at construction, so composed paths are plain even when
    // the input was `\\?\` verbatim). Identity on non-Windows.
    assert_eq!(back, sot_platform::host::paths::simplify_verbatim(abs));
}

#[test]
fn path_to_node_id_root_returns_bare_prefix() {
    let dir = Tmp::new();
    let fm = FilesMode::new(dir.path().to_path_buf()).unwrap();
    let id = fm.path_to_node_id(fm.root_path()).unwrap();
    assert_eq!(id, "files:");
}

#[test]
fn path_to_node_id_rejects_outside_root() {
    let dir = Tmp::new();
    let fm = FilesMode::new(dir.path().to_path_buf()).unwrap();
    let outside = std::env::temp_dir().join("sot-fm-test-definitely-outside-xyz");
    assert!(fm.path_to_node_id(&outside).is_none());
}

/// Security regression (WRITE resolver): a symlink INSIDE the project
/// root pointing OUTSIDE it must not resolve for mutations — the
/// `..`/absolute-path string checks can't catch this, only the
/// canonicalize+confine guard can. Unix-only (`std::os::unix::fs::symlink`).
#[test]
#[cfg(unix)]
fn confined_resolver_rejects_symlink_escape() {
    let dir = Tmp::new();
    let outside = Tmp::new();
    std::fs::write(outside.path().join("secret.txt"), b"outside").unwrap();
    std::os::unix::fs::symlink(outside.path(), dir.path().join("escape")).unwrap();

    let fm = FilesMode::new(dir.path().to_path_buf()).unwrap();
    // The symlink itself resolves outside the root — rejected for writes.
    assert!(fm.node_id_to_path_confined("files:escape").is_err());
    // So does a path THROUGH it to a real file on the other side.
    assert!(fm
        .node_id_to_path_confined("files:escape/secret.txt")
        .is_err());
}

/// The READ resolver deliberately FOLLOWS out-of-root symlinks — the
/// 2026-07-10 nav bug: `data/results/` → NAS is this deployment's core
/// convention, and the old unconditional guard listed such dirs as
/// silently empty. Browsing + preview through a user symlink must work;
/// string-level `..`/absolute injections stay rejected on both resolvers.
#[test]
#[cfg(unix)]
fn read_resolver_follows_symlink_and_lists_children() {
    let dir = Tmp::new();
    let outside = Tmp::new();
    std::fs::write(outside.path().join("data1.txt"), b"one").unwrap();
    std::fs::write(outside.path().join("data2.txt"), b"two").unwrap();
    std::os::unix::fs::symlink(outside.path(), dir.path().join("nas_share")).unwrap();

    let fm = FilesMode::new(dir.path().to_path_buf()).unwrap();

    // The symlinked dir resolves for reads…
    let p = fm.node_id_to_path("files:nas_share").unwrap();
    assert!(p.is_dir());
    // …lists its (out-of-root) children with root-relative node ids…
    let kids = fm.children_of("files:nas_share").unwrap();
    let labels: Vec<&str> = kids.iter().map(|n| n.label.as_str()).collect();
    assert!(labels.contains(&"data1.txt") && labels.contains(&"data2.txt"));
    assert!(kids.iter().all(|n| n.id.starts_with("files:nas_share/")));
    // …and a file THROUGH the symlink resolves + reads for preview.
    let f = fm.node_id_to_path("files:nas_share/data1.txt").unwrap();
    assert_eq!(std::fs::read(f).unwrap(), b"one");

    // Injection checks hold on the read resolver too.
    assert!(fm.node_id_to_path("files:../up").is_err());
    assert!(fm.node_id_to_path("files:/abs").is_err());
}

/// Codex aggregate review (v0.4.4): the escape family where a segment makes
/// `PathBuf::push` REPLACE the workspace root (`..`, absolute, Windows drive
/// / drive-relative / UNC). The composition now walks `std::path::Component`,
/// so validation and push use ONE platform's parser and agree by
/// construction. The cross-platform invariant — traversal + absolute paths
/// are rejected by BOTH resolvers — is asserted here; the Windows-only drive
/// / UNC forms (dangerous only on a Windows daemon) are in the `#[cfg(windows)]`
/// test below.
#[test]
fn resolvers_reject_traversal_and_absolute() {
    let dir = Tmp::new();
    let fm = FilesMode::new(dir.path().to_path_buf()).unwrap();
    for id in ["files:../up", "files:sub/../../etc", "files:/etc/passwd"] {
        assert!(fm.node_id_to_path(id).is_err(), "read must reject {id}");
        assert!(
            fm.node_id_to_path_confined(id).is_err(),
            "write must reject {id}"
        );
    }
    // A colon inside a filename is legal on unix and is NOT a drive; it must
    // compose to an in-root path (a Windows daemon parses it as a `Prefix`
    // and rejects — see the cfg(windows) test).
    #[cfg(unix)]
    {
        let root = sot_platform::host::paths::simplify_verbatim(std::fs::canonicalize(dir.path()).unwrap());
        let p = fm.node_id_to_path("files:a:b.png").unwrap();
        assert_eq!(p, root.join("a:b.png"));
        // A Windows drive STRING can't escape on unix — backslash and colon
        // are ordinary filename bytes, so `push` never replaces the root; it
        // composes to a harmless in-root name. (On Windows it's a `Prefix`
        // and rejected.) This documents the platform-consistent contract.
        let d = fm.node_id_to_path("files:D:/z").unwrap();
        assert!(d.starts_with(&root), "drive string stayed in-root on unix");
    }
}

/// On a Windows daemon, a node id from a Windows FE can carry a drive,
/// drive-relative, UNC, or backslash-absolute prefix — each makes
/// `PathBuf::push` REPLACE the root. `Component::Prefix`/`RootDir` must
/// reject them on BOTH resolvers (set_scale writes through the READ one).
#[test]
#[cfg(windows)]
fn resolvers_reject_windows_drive_and_unc() {
    let dir = Tmp::new();
    let fm = FilesMode::new(dir.path().to_path_buf()).unwrap();
    for id in [
        "files:C:\\outside\\x.png", // drive-absolute
        "files:C:outside",          // drive-RELATIVE (the len==2 check missed this)
        "files:\\\\server\\share\\x", // UNC
        "files:\\outside\\x",       // backslash root-absolute
        "files:sub\\..\\..\\out",   // backslash traversal
        "files:sub/C:../outside.png", // NESTED drive-relative: `C:..` walks as an
        // in-context Normal, but push() reparses it as a drive prefix and
        // replaces the root — caught by the final starts_with guard.
        "files:a/C:..\\..\\x",
    ] {
        assert!(fm.node_id_to_path(id).is_err(), "read must reject {id}");
        assert!(
            fm.node_id_to_path_confined(id).is_err(),
            "write must reject {id}"
        );
    }
}

/// A brand-new (not-yet-created) path under a legitimately-in-root
/// directory must still resolve normally — the escape guard walks up to
/// the nearest EXISTING ancestor, which here is the root itself, and
/// must not false-reject a normal `file.write` to a new file.
#[test]
fn node_id_to_path_allows_new_file_under_root() {
    let dir = Tmp::new();
    let fm = FilesMode::new(dir.path().to_path_buf()).unwrap();
    let p = fm.node_id_to_path("files:brand/new/file.txt").unwrap();
    // Compare against the canonicalized, de-verbatimed root (matches
    // `path_to_node_id_round_trips`'s pattern above) — `dir.path()` itself
    // may not be canonical on every platform (e.g. macOS's `/tmp` ->
    // `/private/tmp`), and Windows canonicalization adds a `\\?\` prefix
    // that FilesMode deliberately strips.
    let root = sot_platform::host::paths::simplify_verbatim(std::fs::canonicalize(dir.path()).unwrap());
    assert_eq!(p, root.join("brand").join("new").join("file.txt"));
}

#[test]
fn list_dir_hides_dotfiles_until_toggled() {
    let dir = Tmp::new();
    std::fs::write(dir.path().join("visible.jl"), b"# hi").unwrap();
    std::fs::write(dir.path().join(".hidden"), b"secret").unwrap();
    let fm = FilesMode::new(dir.path().to_path_buf()).unwrap();

    // Default: hidden entries excluded.
    assert!(!fm.show_hidden());
    let names: Vec<String> = fm
        .children_of("files:")
        .unwrap()
        .into_iter()
        .map(|n| n.label)
        .collect();
    assert!(names.iter().any(|l| l == "visible.jl"));
    assert!(!names.iter().any(|l| l == ".hidden"));

    // Toggle on: hidden entries now appear. `toggle_hidden` returns the
    // NEW value.
    assert!(fm.toggle_hidden());
    assert!(fm.show_hidden());
    let names: Vec<String> = fm
        .children_of("files:")
        .unwrap()
        .into_iter()
        .map(|n| n.label)
        .collect();
    assert!(names.iter().any(|l| l == "visible.jl"));
    assert!(names.iter().any(|l| l == ".hidden"));

    // Toggle back off.
    assert!(!fm.toggle_hidden());
    assert!(!fm.show_hidden());
}

#[test]
fn mime_for_path_recognises_html() {
    // Regression: .html/.htm fell through to text/plain, which
    // silently disabled the frontend's `o` open-in-browser handler.
    assert_eq!(mime_for_path(Path::new("foo.html")), "text/html");
    assert_eq!(mime_for_path(Path::new("foo.htm")), "text/html");
    assert_eq!(mime_for_path(Path::new("FOO.HTML")), "text/html");
}
