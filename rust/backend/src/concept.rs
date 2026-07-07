// concept.rs — read/write/list for the per-project `.concept/` annotation
// sidecar tree.
//
// Per CLAUDE.md "Concept layer", the LLM-maintained annotations live at
// `<project_root>/.concept/<target>.md` with YAML frontmatter that carries
// the `synced_against` AST hash. The backend shuttles file bytes; the
// frontend (or kernel) parses the frontmatter and compares against the
// kernel's live `ast_hash` to render drift indicators.
//
// Targets use forward slashes and never include `.md` — that suffix is
// always added by the backend so a single target string like
// `modules/MyModule` always maps to `.concept/modules/MyModule.md`.
//
// Like FilesMode, this rejects `..` segments and absolute paths in the
// target to keep callers inside the configured `.concept/` directory.

use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};

pub struct ConceptStore {
    root: PathBuf,         // `<project_root>/.concept`
    project_root: PathBuf, // canonical project root, for the escape guard below
}

impl ConceptStore {
    pub fn new(project_root: &Path) -> Self {
        // Canonicalize when possible — the symlink-escape guard in
        // `target_to_path` needs a stable, resolved root to compare against
        // (security review). Falls back to the raw path if canonicalize
        // fails (e.g. it doesn't exist yet) so construction stays infallible;
        // in production `project_root` always exists (workspace creation
        // validates it), so the fallback is dead code there.
        let canon_root = project_root
            .canonicalize()
            .unwrap_or_else(|_| project_root.to_path_buf());
        let mut root = canon_root.clone();
        root.push(".concept");
        Self { root, project_root: canon_root }
    }

    pub fn read(&self, target: &str) -> Result<(bool, String)> {
        let path = self.target_to_path(target)?;
        match std::fs::read_to_string(&path) {
            Ok(s) => Ok((true, s)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok((false, String::new())),
            Err(e) => Err(anyhow!(e)).with_context(|| format!("read {path:?}")),
        }
    }

    /// Extract `synced_against` from the on-disk annotation's YAML
    /// frontmatter. Returns `Ok(None)` if the file doesn't exist, has no
    /// frontmatter, or the frontmatter doesn't carry the field — those all
    /// mean "nothing to be stale against." Errors only on IO failure.
    pub fn read_synced_against(&self, target: &str) -> Result<Option<String>> {
        let (exists, content) = self.read(target)?;
        if !exists {
            return Ok(None);
        }
        Ok(parse_synced_against(&content))
    }

    /// Atomic write via temp file + rename. Returns `(absolute path, byte
    /// count)` on success.
    pub fn write(&self, target: &str, content: &str) -> Result<(PathBuf, u64)> {
        let path = self.target_to_path(target)?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("create_dir_all {parent:?}"))?;
        }
        let tmp = path.with_extension("md.tmp");
        std::fs::write(&tmp, content).with_context(|| format!("write {tmp:?}"))?;
        std::fs::rename(&tmp, &path).with_context(|| format!("rename {tmp:?} -> {path:?}"))?;
        Ok((path.clone(), content.len() as u64))
    }

    /// Recursively walk `.concept/` and return every target string that has
    /// content on disk. Used by the frontend to populate the Concept-mode
    /// tree without requiring Files-mode traversal of hidden directories.
    ///
    /// Symlink escape guard (security review): `target_to_path` validates
    /// each individual read/write target against the project root, but
    /// `list()` used to walk `self.root` directly without ever validating
    /// `self.root` itself — i.e. it never confirmed the `.concept` directory
    /// wasn't ITSELF a symlink escaping the project (planted by an attacker
    /// with write access to the project root, e.g. before or after this
    /// `ConceptStore` was constructed). `std::fs::read_dir` follows a
    /// symlinked directory transparently, so a symlinked `.concept` would
    /// have quietly enumerated and returned target strings for whatever
    /// directory the symlink pointed at, disclosing filenames outside the
    /// project. (A follow-up `read()` of one of those leaked targets is
    /// separately safe: `target_to_path` canonicalizes the FULL candidate
    /// path — not just `self.root` — and rejects anything that resolves
    /// outside `project_root`, so this guard closes an enumeration/
    /// information-disclosure hole, not a read/write bypass.) Unlike
    /// `target_to_path`'s existing-ancestor walk (needed there because a
    /// write target may not exist yet), `self.root` is already known to
    /// exist at this point, so canonicalizing it directly is equally strong
    /// and simpler; `canonicalize_existing_ancestor` is reused anyway to
    /// stay consistent with the rest of the file and to tolerate the rare
    /// TOCTOU window where `.concept` is removed between the `.exists()`
    /// check above and this canonicalize.
    pub fn list(&self) -> Result<Vec<String>> {
        let mut out = Vec::new();
        if !self.root.exists() {
            return Ok(out);
        }
        if let Some(canon) = crate::paths::canonicalize_existing_ancestor(&self.root) {
            if !canon.starts_with(&self.project_root) {
                return Err(anyhow!(
                    ".concept resolves outside the project root (symlink?): {:?}",
                    self.root
                ));
            }
        }
        walk(&self.root, &self.root, &mut out)?;
        out.sort();
        Ok(out)
    }

    fn target_to_path(&self, target: &str) -> Result<PathBuf> {
        if target.is_empty() {
            return Err(anyhow!("empty target"));
        }
        if target.starts_with('/') || target.starts_with('\\') {
            return Err(anyhow!("absolute path in target: {target}"));
        }
        for seg in target.split(['/', '\\']) {
            if seg == ".." {
                return Err(anyhow!("parent-dir segment in target: {target}"));
            }
            if seg.is_empty() {
                return Err(anyhow!("empty segment in target: {target}"));
            }
        }
        let mut p = self.root.clone();
        for seg in target.split('/') {
            p.push(seg);
        }
        // Append `.md` only if the target doesn't already end with `.md`.
        // `Path::set_extension` would REPLACE any existing extension, which
        // collapses `files/foo.rs` and `files/foo.py` into the same on-disk
        // path. Appending preserves the source-file extension as part of
        // the filename so multi-lang annotation namespaces (per Windows-side
        // `files/<rel-path>` convention) survive.
        if !target.ends_with(".md") {
            let mut s = p.into_os_string();
            s.push(".md");
            p = PathBuf::from(s);
        }
        // Symlink escape guard (security review), mirrors `files_mode`'s
        // identical check: canonicalize the longest EXISTING ancestor and
        // confirm it's still under the canonical PROJECT root — not
        // literally `self.root` (`.concept/` itself might not exist yet on a
        // fresh project, and checking against the project root instead of
        // requiring `.concept/` to already exist avoids false-rejecting the
        // very first concept write, while still catching a symlinked
        // `.concept` dir itself, since canonicalizing walks through it too).
        if let Some(canon) = crate::paths::canonicalize_existing_ancestor(&p) {
            if !canon.starts_with(&self.project_root) {
                return Err(anyhow!(
                    "target resolves outside the project root (symlink?): {target}"
                ));
            }
        }
        Ok(p)
    }
}

/// Minimal frontmatter parser: finds the `synced_against` field in a
/// `---\n…\n---\n` YAML block at the top of `content`. We don't depend on
/// a full YAML crate here — the field shape is fixed (`synced_against:
/// <hex-or-quoted-string>` on its own line), and any value that isn't a
/// plain string is treated as missing. Returns the trimmed value (quotes
/// stripped) or `None`.
fn parse_synced_against(content: &str) -> Option<String> {
    let body = content.strip_prefix("---\n")?;
    let end = body.find("\n---\n").or_else(|| body.find("\n---"))?;
    let block = &body[..end];
    for line in block.lines() {
        let line = line.trim_end();
        // strip leading indentation but bail on nested-mapping content —
        // the field we want sits at top-level of the frontmatter block.
        let line = line.strip_prefix("  ").unwrap_or(line);
        let line = line.strip_prefix("\t").unwrap_or(line);
        if let Some(rest) = line.strip_prefix("synced_against:") {
            let v = rest.trim();
            // strip surrounding quotes if present
            let v = v
                .strip_prefix('"')
                .and_then(|s| s.strip_suffix('"'))
                .or_else(|| v.strip_prefix('\'').and_then(|s| s.strip_suffix('\'')))
                .unwrap_or(v);
            if v.is_empty() {
                return None;
            }
            return Some(v.to_string());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::{parse_synced_against, ConceptStore};
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};

    static COUNTER: AtomicU64 = AtomicU64::new(0);

    /// Per-test scratch project dir, removed on drop. A REAL directory (not
    /// the old fake `"/p"` string) is required now: `target_to_path`'s
    /// symlink-escape guard canonicalizes, which needs the root to actually
    /// exist. Mirrors `files_mode.rs`'s `Tmp` helper.
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

    #[test]
    fn parses_plain_value() {
        let s = "---\ntarget: M.f\nsynced_against: abc123\nsynced_at: 2026-05-11\n---\n\nbody\n";
        assert_eq!(parse_synced_against(s).as_deref(), Some("abc123"));
    }

    #[test]
    fn parses_quoted_value() {
        let s = "---\nsynced_against: \"deadbeef\"\n---\nbody\n";
        assert_eq!(parse_synced_against(s).as_deref(), Some("deadbeef"));
    }

    #[test]
    fn returns_none_for_no_frontmatter() {
        assert_eq!(parse_synced_against("just a body\n"), None);
    }

    #[test]
    fn returns_none_for_missing_field() {
        let s = "---\ntarget: M.f\n---\nbody\n";
        assert_eq!(parse_synced_against(s), None);
    }

    #[test]
    fn returns_none_for_empty_value() {
        let s = "---\nsynced_against:\n---\nbody\n";
        assert_eq!(parse_synced_against(s), None);
    }

    #[test]
    fn ignores_field_outside_frontmatter() {
        let s = "no frontmatter\nsynced_against: imposter\n";
        assert_eq!(parse_synced_against(s), None);
    }
}

fn walk(dir: &Path, root: &Path, out: &mut Vec<String>) -> Result<()> {
    for entry in std::fs::read_dir(dir).with_context(|| format!("read_dir {dir:?}"))? {
        let entry = entry?;
        let path = entry.path();
        let ft = entry.file_type()?;
        if ft.is_dir() {
            walk(&path, root, out)?;
        } else if ft.is_file() {
            if path.extension().and_then(|e| e.to_str()) != Some("md") {
                continue;
            }
            let rel = path
                .strip_prefix(root)
                .map(|p| p.to_string_lossy().replace('\\', "/"))
                .unwrap_or_default();
            // Strip the `.md` suffix from the wire-side target.
            let target = rel.strip_suffix(".md").unwrap_or(&rel).to_string();
            out.push(target);
        }
    }
    Ok(())
}
