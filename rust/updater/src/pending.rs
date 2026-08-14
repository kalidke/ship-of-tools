//! The pending pointer — the single, atomic "apply this at next launch" arm.
//!
//! `<updates-root>/pending-<target>.json` names the ONE release the apply
//! owners (the launcher / ExecStartPre / `update.apply`, Phase C3) may flip
//! to on machines of that platform. It is a pointer to immutable prepared
//! state, never copied executables: apply reads it, re-verifies the named
//! stage + worktree, flips, clears.
//!
//! Concurrency contract (second Codex review): ALL transaction state is
//! namespaced per target — a shared `$HOME` serves Linux and macOS installs
//! from one updates root, and a global pointer would let equal-version arms
//! overwrite each other (and a Linux applier install Mach-O binaries). And
//! `arm` serializes through the staging lock, so its newer-wins
//! read-modify-write can't race a concurrent armer; `sot-apply` holds the
//! same (mkdir) lock across read → verify → flip → clear.

use std::cmp::Ordering;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::identity::ReleaseIdentity;
use crate::semver::compare_versions;

/// The per-target pointer filename.
pub fn pointer_path(updates_root: &Path, target: &str) -> PathBuf {
    updates_root.join(format!("pending-{target}.json"))
}

/// Marker naming a version that crash-looped right after an apply on this
/// target; the launcher writes it during rollback and [`arm`] refuses to
/// re-arm that version until a NEWER release supersedes it.
pub fn bad_marker(updates_root: &Path, tag: &str, target: &str) -> PathBuf {
    updates_root.join(format!("bad-{tag}-{target}"))
}

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

/// Arm `identity` for apply on its target. Newer-wins: when a pending pointer
/// for a NEWER version already exists, it is kept and `Ok(false)` is
/// returned; same or older pendings are replaced. A version marked bad
/// (crash-loop rollback) is never armed again. Serialized across processes
/// via the staging lock.
pub async fn arm(
    updates_root: &Path,
    identity: &ReleaseIdentity,
    checkout: &Path,
    commit: &str,
) -> Result<bool> {
    identity.validate()?;
    let lock = crate::lock::StageLock::acquire(updates_root, Duration::from_secs(120)).await?;
    let result = arm_locked(updates_root, identity, checkout, commit).await;
    lock.release();
    result
}

async fn arm_locked(
    updates_root: &Path,
    identity: &ReleaseIdentity,
    checkout: &Path,
    commit: &str,
) -> Result<bool> {
    if bad_marker(updates_root, &identity.tag, &identity.target).exists() {
        tracing::warn!(tag = %identity.tag, "version is marked bad (crash-loop rollback) — not arming");
        return Ok(false);
    }
    if let Ok(Some(existing)) = read(updates_root, &identity.target).await {
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
    // Unique temp name (two same-target armers are excluded by the lock, but
    // a unique name keeps even a broken-lock scenario from corrupting).
    let tmp = updates_root.join(format!(
        "pending-{}.json.tmp-{}",
        identity.target,
        std::process::id()
    ));
    tokio::fs::write(&tmp, serde_json::to_string_pretty(&ptr)?)
        .await
        .context("writing pending pointer")?;
    tokio::fs::rename(&tmp, pointer_path(updates_root, &identity.target))
        .await
        .context("renaming pending pointer into place")?;
    tracing::info!(tag = %identity.tag, target = %identity.target, "pending pointer armed — applies at next launch");
    Ok(true)
}

/// Read the pending pointer for a target; `Ok(None)` when nothing is armed.
/// A pointer that exists but fails to parse or validate is reported as an
/// error (apply must not guess). The pointer's own target must match the
/// requested one.
pub async fn read(updates_root: &Path, target: &str) -> Result<Option<PendingPointer>> {
    let path = pointer_path(updates_root, target);
    let text = match tokio::fs::read_to_string(&path).await {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
    };
    let ptr: PendingPointer =
        serde_json::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
    ptr.identity.validate()?;
    if ptr.identity.target != target {
        anyhow::bail!(
            "pending pointer {} carries target {} — refusing",
            path.display(),
            ptr.identity.target
        );
    }
    Ok(Some(ptr))
}

/// Disarm a target's pointer (after a successful apply, or to cancel).
pub async fn clear(updates_root: &Path, target: &str) -> Result<()> {
    match tokio::fs::remove_file(pointer_path(updates_root, target)).await {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e).context("removing pending pointer"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const T: &str = "linux-x86_64";

    fn identity(ver: &str) -> ReleaseIdentity {
        ReleaseIdentity {
            repo: "kalidke/ship-of-tools".into(),
            tag: format!("v{ver}"),
            version: ver.into(),
            target: T.into(),
            asset: format!("sot-{ver}-{T}.tar.gz"),
            asset_sha256: "cd".repeat(32),
        }
    }

    #[tokio::test]
    async fn arm_read_clear_newer_wins_and_bad_marker() {
        let root = std::env::temp_dir().join(format!("sot-updater-pending-{}", std::process::id()));
        let _ = tokio::fs::remove_dir_all(&root).await;
        tokio::fs::create_dir_all(&root).await.unwrap();

        assert!(read(&root, T).await.unwrap().is_none());

        let co = PathBuf::from("/x/repo/versions/v0.6.0");
        assert!(arm(&root, &identity("0.6.0"), &co, "aaa").await.unwrap());
        let got = read(&root, T).await.unwrap().unwrap();
        assert_eq!(got.identity.tag, "v0.6.0");
        // Another target sees nothing.
        assert!(read(&root, "macos-aarch64").await.unwrap().is_none());

        // Newer replaces older…
        assert!(arm(&root, &identity("0.7.0"), &co, "bbb").await.unwrap());
        // …but an older late-finisher cannot clobber a newer arm.
        assert!(!arm(&root, &identity("0.6.1"), &co, "ccc").await.unwrap());
        assert_eq!(read(&root, T).await.unwrap().unwrap().identity.tag, "v0.7.0");

        // A bad-marked version refuses to arm.
        std::fs::write(bad_marker(&root, "v0.8.0", T), b"").unwrap();
        assert!(!arm(&root, &identity("0.8.0"), &co, "ddd").await.unwrap());

        clear(&root, T).await.unwrap();
        assert!(read(&root, T).await.unwrap().is_none());
        clear(&root, T).await.unwrap(); // idempotent

        tokio::fs::remove_dir_all(&root).await.unwrap();
    }

    #[tokio::test]
    async fn corrupt_pointer_is_an_error_not_none() {
        let root =
            std::env::temp_dir().join(format!("sot-updater-pending-bad-{}", std::process::id()));
        let _ = tokio::fs::remove_dir_all(&root).await;
        tokio::fs::create_dir_all(&root).await.unwrap();
        tokio::fs::write(pointer_path(&root, T), b"{not json")
            .await
            .unwrap();
        assert!(read(&root, T).await.is_err());
        tokio::fs::remove_dir_all(&root).await.unwrap();
    }
}
