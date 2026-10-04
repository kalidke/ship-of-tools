// files_mode.rs — Files-mode tree walking + filesystem-backed previews.
//
// Per requirements.md: Files mode is the simplest of the seven mode roots.
// Col 1 = parent dir, Col 2 = current dir, Col 3 = contents. For the backend
// we don't yet model the column layout — we just answer `tree.root` and
// `tree.children` against an actual filesystem, plus serve real bytes via
// `preview.get`.
//
// Node id format:
//   files:                       → the project root itself
//   files:<relative-path>        → any descendant of the project root
// Relative-path uses forward slashes on all OSes for stability on the wire.
// Both resolvers reject `..` segments and absolute paths in the id itself.
// READS (tree.children, preview.get, file.read) FOLLOW symlinks — including
// out-of-root ones like `data/results/` → NAS, this deployment's convention.
// MUTATIONS (file.write, file.delete) use `node_id_to_path_confined`, which
// additionally canonicalizes and refuses paths escaping the root.
//
// Directory listings skip hidden entries (leading `.`) UNLESS the per-
// workspace `show_hidden` flag is set — the frontend flips it with the `.`
// key via the `nav.toggle_hidden` op. Sort order is directories first, then
// files, both case-insensitive alphabetical.
//
// Kinds the frontend can specialise on:
//   "dir", "mdfile", "jlfile", "tomlfile", "imagefile", "svgfile", "file"

use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::{anyhow, Context, Result};
use sot_protocol::TreeNode;

pub struct FilesMode {
    root: PathBuf,
    /// When true, directory listings include hidden entries (leading `.`).
    /// Interior mutability: `FilesMode` is shared as `Arc<FilesMode>` and
    /// never held `&mut`, so the toggle op flips this atomically in place.
    show_hidden: AtomicBool,
}

const ID_PREFIX: &str = "files:";

impl FilesMode {
    pub fn new(root: PathBuf) -> Result<Self> {
        // simplify_verbatim: canonicalize yields `\\?\` verbatim paths on
        // Windows, which disable Win32 slash normalization — this root is
        // ADVERTISED (HelloRes.project_root) and every FE/kernel path
        // composition builds on it, so a verbatim root poisons them all
        // (the concept-stale drift-badge saga). De-verbatim at the source.
        let canon = crate::paths::simplify_verbatim(
            std::fs::canonicalize(&root)
                .with_context(|| format!("canonicalize project root {root:?}"))?,
        );
        if !canon.is_dir() {
            return Err(anyhow!("project root {canon:?} is not a directory"));
        }
        Ok(Self {
            root: canon,
            show_hidden: AtomicBool::new(false),
        })
    }

    pub fn root_path(&self) -> &Path {
        &self.root
    }

    /// Whether directory listings currently include hidden entries.
    pub fn show_hidden(&self) -> bool {
        self.show_hidden.load(Ordering::Relaxed)
    }

    /// Flip the show-hidden flag and return the NEW value. `fetch_xor`
    /// returns the previous value, so the new value is its negation.
    pub fn toggle_hidden(&self) -> bool {
        !self.show_hidden.fetch_xor(true, Ordering::Relaxed)
    }

    pub fn root_node(&self) -> TreeNode {
        let label = self
            .root
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("/")
            .to_string();
        TreeNode {
            id: ID_PREFIX.to_string(),
            label,
            kind: "dir".to_string(),
            has_children: true,
            badges: Vec::new(),
            payload: serde_json::Map::new(),
        }
    }

    pub fn children_of(&self, node_id: &str) -> Result<Vec<TreeNode>> {
        let path = self.node_id_to_path(node_id)?;
        if !path.is_dir() {
            return Err(anyhow!("not a directory: {node_id}"));
        }
        list_dir(&path, &self.root, self.show_hidden())
    }

    /// Reverse of `node_id_to_path`: given an absolute path under the root,
    /// derive its files-mode node id. Returns `None` if the path is outside
    /// the root (e.g. the user has a symlink jumping out, or notify reports
    /// a path the FilesMode never exposed). Forward slashes regardless of
    /// platform to match `node_id_to_path`'s splitter.
    pub fn path_to_node_id(&self, path: &Path) -> Option<String> {
        // Symmetry with the de-verbatimed root (paths::simplify_verbatim in
        // `new`): callers hand us canonicalized paths, which on Windows are
        // `\\?\` verbatim and would never prefix-match the plain root.
        let path = crate::paths::simplify_verbatim(path.to_path_buf());
        let rel = path.strip_prefix(&self.root).ok()?;
        if rel.as_os_str().is_empty() {
            return Some(ID_PREFIX.to_string());
        }
        let mut s = String::from(ID_PREFIX);
        let joined = rel.to_string_lossy().replace('\\', "/");
        s.push_str(&joined);
        Some(s)
    }

    /// Returns the absolute filesystem path for a node id — the READ
    /// resolver: symlinks are FOLLOWED, wherever they lead.
    ///
    /// Rationale (2026-07-10, user bug "following symlink in nav pane is
    /// broken"): out-of-root symlinks are this deployment's core convention
    /// (`data/results/` → NAS, `nas_share/<project>` → CIFS mounts), and the
    /// old unconditional canonicalize+confine guard rejected them all — a
    /// symlinked NAS directory listed as silently empty. A symlink inside the
    /// project root was created by the user; browsing through it is user
    /// intent, not an escape. Read-only callers (tree.children, preview.get,
    /// file.read, image.crop's source) use this. String-level `..`/absolute
    /// injections in the node id itself are still rejected.
    ///
    /// MUTATING callers (file.write, file.delete) must use
    /// [`node_id_to_path_confined`] instead, which keeps the full
    /// canonicalize+confine escape guard from the security review.
    pub fn node_id_to_path(&self, node_id: &str) -> Result<PathBuf> {
        self.compose_node_path(node_id)
    }

    /// The WRITE resolver: same string-level checks, plus the symlink escape
    /// guard (security review) — the `..`/absolute checks are string-level
    /// and can't catch a symlink INSIDE the root pointing outside it.
    /// Canonicalize the longest existing ancestor of the composed path (the
    /// target may not exist yet — e.g. a `file.write` to a brand-new path)
    /// and confirm it's still under `self.root`, which is already canonical
    /// (`FilesMode::new`). Mutations therefore can't escape the project root
    /// even through a user symlink; reads deliberately can (see
    /// [`node_id_to_path`]).
    pub fn node_id_to_path_confined(&self, node_id: &str) -> Result<PathBuf> {
        let p = self.compose_node_path(node_id)?;
        if let Some(canon) = crate::paths::canonicalize_existing_ancestor(&p) {
            if !crate::paths::path_within_root(&canon, &self.root) {
                return Err(anyhow!(
                    "node id resolves outside the project root (symlink?): {node_id}"
                ));
            }
        }
        Ok(p)
    }

    /// Shared string-level composition + injection checks for both resolvers.
    ///
    /// Walks the id's relative path with [`std::path::Component`] semantics —
    /// the SAME semantics [`PathBuf::push`] uses on this platform — so the
    /// validation can never disagree with the composition. Only `Normal`
    /// components are appended; anything that would make `push` REPLACE the
    /// workspace root is rejected:
    /// - `ParentDir` (`..`) — traversal above the root,
    /// - `RootDir` (`/x`, or `\x` on Windows) — absolute path,
    /// - `Prefix` (Windows drive `C:` / drive-relative `C:x`, or UNC
    ///   `\\server\share`) — root-replacing on Windows.
    ///
    /// This closes the whole `push`-replaces-the-root escape family at once,
    /// rather than enumerating exotic string forms (the prior `len == 2`
    /// drive check missed drive-relative `C:x`, backslash-absolute `\x`, and
    /// UNC — a node id can arrive from a Windows FE, and set_scale writes
    /// through this READ resolver since #43). Because the in-context walk and
    /// push use one platform's `Component` parser, they agree on structure: on
    /// a Linux daemon a stray `C:x` is a harmless in-root `Normal` filename; on
    /// a Windows daemon the same string parses as a `Prefix` and is rejected.
    ///
    /// One subtlety the walk alone doesn't cover: `PathBuf::push` RE-PARSES
    /// each segment IN ISOLATION, so a component that is `Normal` in whole-path
    /// context can be root-replacing on its own. On Windows `sub/C:../x` walks
    /// as `Normal("sub"), Normal("C:.."), Normal("x")` — mid-path `C:` is not a
    /// drive prefix — yet `push("C:..")` re-parses `C:..` as a drive-RELATIVE
    /// path (`Prefix` `C:`, no root) and REPLACES the accumulated root. The
    /// final `starts_with` guard catches this: a replacement can never
    /// re-establish `self.root` as a component prefix — a lone segment carries
    /// no separator, so it can only become drive-relative (`C:x`), never
    /// `C:\root\...` — so any push that escaped drops the root prefix and is
    /// rejected. (`..` is handled in-loop; it survives the lexical
    /// `starts_with`, so both checks are needed.)
    fn compose_node_path(&self, node_id: &str) -> Result<PathBuf> {
        let rel = node_id
            .strip_prefix(ID_PREFIX)
            .ok_or_else(|| anyhow!("not a files-mode id: {node_id}"))?;
        if rel.is_empty() {
            return Ok(self.root.clone());
        }
        let mut p = self.root.clone();
        for comp in Path::new(rel).components() {
            match comp {
                Component::Normal(seg) => p.push(seg),
                Component::CurDir => {} // `.` — no-op
                Component::ParentDir => {
                    return Err(anyhow!("parent-dir segment not allowed in node id: {node_id}"));
                }
                Component::RootDir | Component::Prefix(_) => {
                    return Err(anyhow!("absolute/rooted path not allowed in node id: {node_id}"));
                }
            }
        }
        // `push`'s per-segment reparse (see above) can drop the root out from
        // under us; a lexical containment check is the sound backstop.
        if !p.starts_with(&self.root) {
            return Err(anyhow!("node id escapes the workspace root: {node_id}"));
        }
        Ok(p)
    }
}

fn list_dir(dir: &Path, root: &Path, show_hidden: bool) -> Result<Vec<TreeNode>> {
    let mut entries: Vec<(PathBuf, bool, String)> = Vec::new();
    for entry in std::fs::read_dir(dir).with_context(|| format!("read_dir {dir:?}"))? {
        let entry = entry?;
        let name = entry.file_name();
        let name_str = match name.to_str() {
            Some(s) => s.to_string(),
            None => continue, // skip non-utf8 entries; the wire is JSON
        };
        // `.` and `..` are never returned by read_dir, so a leading dot here
        // always means a genuine hidden entry — skip unless the toggle is on.
        if !show_hidden && name_str.starts_with('.') {
            continue; // skip hidden
        }
        let path = entry.path();
        // Follow symlinks via metadata (not entry.file_type, which only
        // sees lstat on Linux) so a symlink to a directory shows up as a
        // navigable dir entry instead of a leaf "file". Broken symlinks
        // skip silently.
        let is_dir = match std::fs::metadata(&path) {
            Ok(m) => m.is_dir(),
            Err(_) => continue,
        };
        entries.push((path, is_dir, name_str));
    }
    entries.sort_by(|a, b| match (a.1, b.1) {
        (true, false) => std::cmp::Ordering::Less,
        (false, true) => std::cmp::Ordering::Greater,
        _ => a.2.to_lowercase().cmp(&b.2.to_lowercase()),
    });

    let mut out = Vec::with_capacity(entries.len());
    for (path, is_dir, name) in entries {
        let rel = path
            .strip_prefix(root)
            .map(|p| p.to_string_lossy().replace('\\', "/"))
            .unwrap_or_default();
        let kind = if is_dir {
            "dir"
        } else {
            kind_for_extension(&path)
        };
        let label = if is_dir { format!("{name}/") } else { name };
        out.push(TreeNode {
            id: format!("{ID_PREFIX}{rel}"),
            label,
            kind: kind.to_string(),
            has_children: is_dir,
            badges: Vec::new(),
            payload: serde_json::Map::new(),
        });
    }
    Ok(out)
}

fn kind_for_extension(path: &Path) -> &'static str {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase);
    match ext.as_deref() {
        Some("md") | Some("markdown") => "mdfile",
        Some("jl") => "jlfile",
        Some("toml") => "tomlfile",
        Some("png") | Some("jpg") | Some("jpeg") | Some("gif") | Some("webp") | Some("bmp") => {
            "imagefile"
        }
        Some("svg") => "svgfile",
        _ => "file",
    }
}

/// Best-effort mime guess for the preview blob. The frontend renders based
/// on the mime, not the extension, so anything we don't recognise gets
/// `text/plain; charset=utf-8` — which is the right fallback for source code.
pub fn mime_for_path(path: &Path) -> &'static str {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase);
    match ext.as_deref() {
        // Quarto docs preview as markdown in-pane (instant, no quarto
        // invocation) — `o`/`O` render the real thing via `quarto.open`.
        Some("md") | Some("markdown") | Some("qmd") => "text/markdown",
        Some("png") => "image/png",
        Some("jpg") | Some("jpeg") => "image/jpeg",
        Some("gif") => "image/gif",
        Some("webp") => "image/webp",
        Some("bmp") => "image/bmp",
        Some("svg") => "image/svg+xml",
        Some("json") => "application/json",
        Some("toml") => "text/x-toml",
        Some("jl") => "text/x-julia",
        Some("rs") => "text/x-rust",
        // The frontend's `o`-in-Preview handler is gated on `text/html`
        // exactly, so .html / .htm needs to advertise as such or the
        // browser-open keystroke silently no-ops.
        Some("html") | Some("htm") => "text/html",
        _ => "text/plain; charset=utf-8",
    }
}

#[cfg(test)]
mod tests {
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
        assert_eq!(back, crate::paths::simplify_verbatim(abs));
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
            let root = crate::paths::simplify_verbatim(std::fs::canonicalize(dir.path()).unwrap());
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
        let root = crate::paths::simplify_verbatim(std::fs::canonicalize(dir.path()).unwrap());
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
}
