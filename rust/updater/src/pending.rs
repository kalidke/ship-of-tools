//! The pending pointer — the single, atomic "apply this at next launch" arm.
//!
//! `<updates-root>/pending.json` names the ONE release the apply owners (the
//! launcher / ExecStartPre / `update.apply`, Phase C3) may flip to. It is a
//! pointer to immutable prepared state, never copied executables: apply reads
//! it, re-verifies the named stage + worktree, flips, clears. Arming enforces
//! newer-wins — an older stage task finishing late can never overwrite a
//! newer armed release.

use std::cmp::Ordering;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::identity::ReleaseIdentity;
use crate::semver::compare_versions;

pub const PENDING_POINTER: &str = "pending.json";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PendingPointer {
    pub schema: u32,
    #[serde(flatten)]
    pub identity: ReleaseIdentity,
    /// The prepared worktree apply will flip `repo/current` to.
    pub checkout: PathBuf,
    /// The tag's commit (re-verified against the worktree HEAD at apply).
    pub commit: String,
    /// Unix seconds when armed.
    pub armed_at: u64,
}

/// Marker naming a version that crash-looped right after an apply; the
/// launcher writes it during rollback (Phase C3) and [`arm`] refuses to
/// re-arm that version until a NEWER release supersedes it.
pub fn bad_marker(updates_root: &Path, tag: &str) -> PathBuf {
    updates_root.join(format!("bad-{tag}"))
}

/// Arm `identity` for apply. Newer-wins: when a pending pointer for a NEWER
/// version already exists, it is kept and `Ok(false)` is returned; same or
/// older pendings are replaced. A version marked bad (crash-loop rollback)
/// is never armed again.
pub async fn arm(
    updates_root: &Path,
    identity: &ReleaseIdentity,
    checkout: &Path,
    commit: &str,
) -> Result<bool> {
    identity.validate()?;
    if bad_marker(updates_root, &identity.tag).exists() {
        tracing::warn!(tag = %identity.tag, "version is marked bad (crash-loop rollback) — not arming");
        return Ok(false);
    }
    if let Ok(Some(existing)) = read(updates_root).await {
        if compare_versions(&existing.identity.version, &identity.version) == Ordering::Greater {
            tracing::info!(
                existing = %existing.identity.tag,
                offered = %identity.tag,
                "pending pointer already arms a newer release — keeping it"
            );
            return Ok(false);
        }
    }
    let ptr = PendingPointer {
        schema: 1,
        identity: identity.clone(),
        checkout: checkout.to_path_buf(),
        commit: commit.to_string(),
        armed_at: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0),
    };
    let tmp = updates_root.join(format!("{PENDING_POINTER}.tmp"));
    tokio::fs::write(&tmp, serde_json::to_string_pretty(&ptr)?)
        .await
        .context("writing pending pointer")?;
    tokio::fs::rename(&tmp, updates_root.join(PENDING_POINTER))
        .await
        .context("renaming pending pointer into place")?;
    tracing::info!(tag = %identity.tag, "pending pointer armed — applies at next launch");
    Ok(true)
}

/// Read the pending pointer; `Ok(None)` when nothing is armed. A pointer that
/// exists but fails to parse or validate is reported as an error (apply must
/// not guess).
pub async fn read(updates_root: &Path) -> Result<Option<PendingPointer>> {
    let path = updates_root.join(PENDING_POINTER);
    let text = match tokio::fs::read_to_string(&path).await {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
    };
    let ptr: PendingPointer =
        serde_json::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
    ptr.identity.validate()?;
    Ok(Some(ptr))
}

/// Disarm (after a successful apply, or to cancel).
pub async fn clear(updates_root: &Path) -> Result<()> {
    match tokio::fs::remove_file(updates_root.join(PENDING_POINTER)).await {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e).context("removing pending pointer"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn identity(ver: &str) -> ReleaseIdentity {
        ReleaseIdentity {
            repo: "kalidke/ship-of-tools".into(),
            tag: format!("v{ver}"),
            version: ver.into(),
            target: "linux-x86_64".into(),
            asset: format!("sot-{ver}-linux-x86_64.tar.gz"),
            asset_sha256: "cd".repeat(32),
        }
    }

    #[tokio::test]
    async fn arm_read_clear_and_newer_wins() {
        let root = std::env::temp_dir().join(format!("sot-updater-pending-{}", std::process::id()));
        let _ = tokio::fs::remove_dir_all(&root).await;
        tokio::fs::create_dir_all(&root).await.unwrap();

        assert!(read(&root).await.unwrap().is_none());

        let co = PathBuf::from("/x/repo/versions/v0.6.0");
        assert!(arm(&root, &identity("0.6.0"), &co, "aaa").await.unwrap());
        let got = read(&root).await.unwrap().unwrap();
        assert_eq!(got.identity.tag, "v0.6.0");

        // Newer replaces older…
        assert!(arm(&root, &identity("0.7.0"), &co, "bbb").await.unwrap());
        // …but an older late-finisher cannot clobber a newer arm.
        assert!(!arm(&root, &identity("0.6.1"), &co, "ccc").await.unwrap());
        assert_eq!(read(&root).await.unwrap().unwrap().identity.tag, "v0.7.0");

        clear(&root).await.unwrap();
        assert!(read(&root).await.unwrap().is_none());
        clear(&root).await.unwrap(); // idempotent

        tokio::fs::remove_dir_all(&root).await.unwrap();
    }

    #[tokio::test]
    async fn corrupt_pointer_is_an_error_not_none() {
        let root =
            std::env::temp_dir().join(format!("sot-updater-pending-bad-{}", std::process::id()));
        let _ = tokio::fs::remove_dir_all(&root).await;
        tokio::fs::create_dir_all(&root).await.unwrap();
        tokio::fs::write(root.join(PENDING_POINTER), b"{not json")
            .await
            .unwrap();
        assert!(read(&root).await.is_err());
        tokio::fs::remove_dir_all(&root).await.unwrap();
    }
}
