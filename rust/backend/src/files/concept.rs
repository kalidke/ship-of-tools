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
        Ok(sot_protocol::annotation::synced_against(&content))
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
        if let Some(canon) = crate::files::confine::canonicalize_existing_ancestor(&self.root) {
            if !crate::files::confine::path_within_root(&canon, &self.project_root) {
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
        // Symlink escape guard (security review), mirrors `files::tree`'s
        // identical check: canonicalize the longest EXISTING ancestor and
        // confirm it's still under the canonical PROJECT root — not
        // literally `self.root` (`.concept/` itself might not exist yet on a
        // fresh project, and checking against the project root instead of
        // requiring `.concept/` to already exist avoids false-rejecting the
        // very first concept write, while still catching a symlinked
        // `.concept` dir itself, since canonicalizing walks through it too).
        if let Some(canon) = crate::files::confine::canonicalize_existing_ancestor(&p) {
            if !crate::files::confine::path_within_root(&canon, &self.project_root) {
                return Err(anyhow!(
                    "target resolves outside the project root (symlink?): {target}"
                ));
            }
        }
        Ok(p)
    }
}

#[cfg(test)]
#[path = "concept_tests.rs"]
mod tests;

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
