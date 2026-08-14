//! sot-updater — release discovery, verification, and staging primitives for
//! Ship of Tools auto-update (ADR 0030 Phase C).
//!
//! Pure mechanism, no policy: update modes, dev-build guards, scheduling,
//! notification, and apply/restart ownership all live with the binaries that
//! embed this crate (backend policy in `sot-backend`'s `update.rs`; the
//! frontend gains its own thin layer in Phase C2). What lives HERE is the part
//! both sides must agree on:
//!
//! - release discovery from `SHA256SUMS` (one documented HTTPS request, no
//!   Releases API) pinned into a full [`identity::ReleaseIdentity`],
//! - fetch backends (`curl` default / `gh` for private forks / local dir for
//!   tests and sideload),
//! - cross-process staging: filesystem lock → unique temp dir → download →
//!   streamed sha256 verify → allowlist-validated extraction → ready manifest
//!   → atomic rename into `<updates-root>/<tag>/`.
//!
//! A stage is complete iff its ready manifest parses and matches the wanted
//! identity — never because a marker file merely exists.

pub mod archive;
pub mod fetch;
pub mod hash;
pub mod identity;
pub mod lock;
pub mod manifest;
pub mod pending;
pub mod platform;
pub mod prepare;
pub mod semver;
pub mod sums;

use std::cmp::Ordering;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{bail, Context, Result};

pub use fetch::Fetcher;
pub use identity::ReleaseIdentity;
pub use manifest::{resolve_updates_root, InstallManifest, ReadyManifest};

/// How long a stage will wait for another process's stage to finish before
/// giving up on the lock.
const LOCK_WAIT: Duration = Duration::from_secs(600);

/// Everything a check/stage needs to know. Callers construct it; policy
/// (modes, dev guards) stays theirs.
#[derive(Debug, Clone)]
pub struct UpdaterConfig {
    /// `owner/repo` release source.
    pub repo: String,
    /// Running product version (bare or `-dev+sha` stamped).
    pub current_version: String,
    pub fetcher: Fetcher,
    /// Root of the staging area (see [`resolve_updates_root`]).
    pub updates_root: PathBuf,
}

/// Outcome of a release check. Structured, never an Err: callers surface
/// `status` verbatim.
#[derive(Debug, Clone)]
pub struct CheckOutcome {
    /// Full identity of the latest release's asset for THIS platform, when
    /// the check succeeded and the release ships it.
    pub identity: Option<ReleaseIdentity>,
    /// Latest released version (bare `X.Y.Z`), empty when unknown.
    pub latest: String,
    /// True when `latest` is strictly newer than `current_version`.
    pub update_available: bool,
    /// `"ok"` or the reason the check couldn't produce an identity.
    pub status: String,
}

fn outcome_err(status: String) -> CheckOutcome {
    CheckOutcome {
        identity: None,
        latest: String::new(),
        update_available: false,
        status,
    }
}

/// Query the latest release and pin this platform's identity.
pub async fn check(cfg: &UpdaterConfig) -> CheckOutcome {
    let Some(target) = platform::this_platform() else {
        return outcome_err(format!(
            "platform {}-{} is not in the release matrix",
            platform::TARGET_OS,
            platform::TARGET_ARCH
        ));
    };
    let latest = match cfg.fetcher.latest(&cfg.repo).await {
        Ok(l) => l,
        Err(e) => return outcome_err(format!("check unavailable: {e}")),
    };
    let entries = match sums::parse_sums(&latest.sums_text) {
        Ok(v) => v,
        Err(e) => return outcome_err(format!("bad SHA256SUMS: {e}")),
    };
    let identity = match sums::discover(&entries, &cfg.repo, target) {
        Ok(id) => id,
        Err(e) => return outcome_err(format!("{e}")),
    };
    // A fetch backend that knows the tag authoritatively must agree with the
    // sums-derived one — a mismatch means a moved tag or a half-published
    // release, and we refuse to act on it.
    if let Some(tag) = &latest.tag {
        if *tag != identity.tag {
            return outcome_err(format!(
                "release tag {tag} does not match SHA256SUMS contents ({})",
                identity.tag
            ));
        }
    }
    let update_available =
        semver::compare_versions(&identity.version, &cfg.current_version) == Ordering::Greater;
    CheckOutcome {
        latest: identity.version.clone(),
        identity: Some(identity),
        update_available,
        status: "ok".into(),
    }
}

/// The completed-stage directory for a tag.
pub fn stage_dir(updates_root: &Path, tag: &str) -> PathBuf {
    updates_root.join(tag)
}

/// True when `updates_root/<tag>` holds a completed stage of exactly `id`.
pub async fn is_staged(updates_root: &Path, id: &ReleaseIdentity) -> bool {
    ReadyManifest::matches(&stage_dir(updates_root, &id.tag), id).await
}

/// Download → verify → validate → extract → commit one release for this
/// machine. Idempotent (a matching completed stage short-circuits) and
/// serialized across processes via the filesystem lock. Returns `Ok(true)`
/// when the stage is present afterward.
pub async fn stage(cfg: &UpdaterConfig, id: &ReleaseIdentity) -> Result<bool> {
    id.validate()?;
    if is_staged(&cfg.updates_root, id).await {
        return Ok(true);
    }
    let lock = lock::StageLock::acquire(&cfg.updates_root, LOCK_WAIT).await?;
    let result = stage_locked(cfg, id).await;
    lock.release();
    result
}

async fn stage_locked(cfg: &UpdaterConfig, id: &ReleaseIdentity) -> Result<bool> {
    // Re-check under the lock: a concurrent stager may have finished while we
    // waited.
    if is_staged(&cfg.updates_root, id).await {
        return Ok(true);
    }
    let dest = stage_dir(&cfg.updates_root, &id.tag);
    if dest.exists() {
        // Present but not a matching completed stage: a partial from a
        // crashed run, or different contents under the same tag. Rebuild it.
        tracing::warn!(dir = %dest.display(), "removing incomplete/mismatched stage dir");
        tokio::fs::remove_dir_all(&dest)
            .await
            .with_context(|| format!("removing stale stage dir {}", dest.display()))?;
    }
    sweep_stale_tmp(&cfg.updates_root).await;

    let tmp = cfg.updates_root.join(format!(
        "tmp-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    tokio::fs::create_dir_all(&tmp)
        .await
        .with_context(|| format!("creating staging temp dir {}", tmp.display()))?;

    let result = async {
        let archive_path = tmp.join(&id.asset);
        cfg.fetcher
            .download(&id.repo, &id.tag, &id.asset, &archive_path)
            .await
            .context("downloading release asset")?;
        let got = hash::sha256_file(&archive_path).await?;
        if got != id.asset_sha256.to_ascii_lowercase() {
            bail!(
                "sha256 mismatch for {}: expected {}, got {got}",
                id.asset,
                id.asset_sha256
            );
        }
        let top = release_top_dir(&id.asset)?;
        archive::extract_validated(&archive_path, &tmp, &top)
            .await
            .context("extracting release archive")?;
        ReadyManifest::new(id.clone()).write(&tmp).await?;
        Ok::<(), anyhow::Error>(())
    }
    .await;

    if let Err(e) = result {
        let _ = tokio::fs::remove_dir_all(&tmp).await;
        return Err(e);
    }
    tokio::fs::rename(&tmp, &dest)
        .await
        .with_context(|| format!("committing stage into {}", dest.display()))?;
    tracing::info!(tag = %id.tag, asset = %id.asset, dir = %dest.display(), "update staged");
    Ok(true)
}

/// The single top-level dir a release archive extracts to
/// (`sot-<ver>-<target>`, the asset name minus its archive extension).
fn release_top_dir(asset: &str) -> Result<String> {
    for ext in [".tar.gz", ".zip", ".tgz"] {
        if let Some(top) = asset.strip_suffix(ext) {
            return Ok(top.to_string());
        }
    }
    bail!("asset {asset:?} has no recognized archive extension");
}

/// Best-effort cleanup of abandoned `tmp-*` staging dirs older than a day.
async fn sweep_stale_tmp(root: &Path) {
    const MAX_AGE: Duration = Duration::from_secs(24 * 3600);
    let Ok(mut rd) = tokio::fs::read_dir(root).await else {
        return;
    };
    while let Ok(Some(entry)) = rd.next_entry().await {
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        if !name.starts_with("tmp-") {
            continue;
        }
        let stale = entry
            .metadata()
            .await
            .ok()
            .and_then(|m| m.modified().ok())
            .and_then(|m| std::time::SystemTime::now().duration_since(m).ok())
            .map(|age| age > MAX_AGE)
            .unwrap_or(false);
        if stale {
            tracing::info!(dir = %entry.path().display(), "sweeping abandoned staging temp dir");
            let _ = tokio::fs::remove_dir_all(entry.path()).await;
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    /// Build a fake release dir (SHA256SUMS + one platform archive) and run
    /// the full check → stage flow against it via the Dir fetcher.
    #[tokio::test]
    async fn check_and_stage_end_to_end() {
        let base = std::env::temp_dir().join(format!("sot-updater-e2e-{}", std::process::id()));
        let _ = tokio::fs::remove_dir_all(&base).await;
        let release = base.join("release");
        let updates = base.join("updates");
        tokio::fs::create_dir_all(&release).await.unwrap();

        let target = platform::this_platform().expect("test host must be in the release matrix");
        let version = "9.9.9";
        let asset = platform::platform_asset(version).unwrap();
        let top = format!("sot-{version}-{target}");

        // Assemble the archive exactly like release CI does.
        let build = base.join("build").join(&top);
        tokio::fs::create_dir_all(&build).await.unwrap();
        tokio::fs::write(build.join("sot"), b"fe-binary").await.unwrap();
        tokio::fs::write(build.join("sotd"), b"be-binary").await.unwrap();
        tokio::fs::write(build.join("sotd.service"), b"unit").await.unwrap();
        let archive = release.join(&asset);
        let st = std::process::Command::new("tar")
            .args([
                "-czf",
                &archive.to_string_lossy(),
                "-C",
                &base.join("build").to_string_lossy(),
                &top,
            ])
            .status()
            .unwrap();
        assert!(st.success());
        let digest = hash::sha256_file(&archive).await.unwrap();
        tokio::fs::write(
            release.join("SHA256SUMS"),
            format!("{digest}  {asset}\n"),
        )
        .await
        .unwrap();

        let cfg = UpdaterConfig {
            repo: "kalidke/ship-of-tools".into(),
            current_version: "0.1.0".into(),
            fetcher: Fetcher::Dir(release.clone()),
            updates_root: updates.clone(),
        };

        let out = check(&cfg).await;
        assert_eq!(out.status, "ok");
        assert!(out.update_available);
        let id = out.identity.unwrap();
        assert_eq!(id.tag, format!("v{version}"));
        assert_eq!(id.asset, asset);

        // A wrong pinned digest refuses to stage and leaves nothing ready.
        let mut bad = id.clone();
        bad.asset_sha256 = "0".repeat(64);
        assert!(stage(&cfg, &bad).await.is_err());
        assert!(!is_staged(&updates, &bad).await);

        assert!(!is_staged(&updates, &id).await);
        assert!(stage(&cfg, &id).await.unwrap());
        assert!(is_staged(&updates, &id).await);
        // Idempotent.
        assert!(stage(&cfg, &id).await.unwrap());

        let staged_bin = stage_dir(&updates, &id.tag).join(&top).join("sot");
        assert_eq!(tokio::fs::read(&staged_bin).await.unwrap(), b"fe-binary");
        let manifest = ReadyManifest::read(&stage_dir(&updates, &id.tag)).await.unwrap();
        assert_eq!(manifest.identity, id);

        // A running check against the staged version reports no update.
        let cfg_current = UpdaterConfig {
            current_version: version.into(),
            ..cfg.clone()
        };
        let out2 = check(&cfg_current).await;
        assert_eq!(out2.status, "ok");
        assert!(!out2.update_available);

        tokio::fs::remove_dir_all(&base).await.unwrap();
    }
}
