// files/tree.rs — Files-mode tree walking + filesystem-backed previews.
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
        if let Some(canon) = crate::files::confine::canonicalize_existing_ancestor(&p) {
            if !crate::files::confine::path_within_root(&canon, &self.root) {
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
#[path = "tree_tests.rs"]
mod tests;
