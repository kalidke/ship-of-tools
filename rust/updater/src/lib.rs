//! sot-updater — release discovery, verification, and staging primitives for
//! Ship of Tools auto-update (ADR 0030 Phase C).
//!
//! Pure mechanism, no policy: update modes, dev-build guards, scheduling,
//! notification, and apply/restart ownership all live with the binaries that
//! embed this crate (backend policy in `sot-backend`'s `update.rs`; the
//! frontend gains its own thin layer in Phase C2). What lives HERE is the part
//! both sides must agree on:
//!
//! - release discovery: list releases, pick a tag via [`select::select_target`]
//!   (channel implied by the installed version — ADR 0030 §4 amendment
//!   2026-09-08), then pin the chosen tag's `SHA256SUMS` into a full
//!   [`identity::ReleaseIdentity`],
//! - fetch backends (`curl` default / `gh` for private forks / local dir for
//!   tests and sideload),
//! - cross-process staging: filesystem lock → unique temp dir → download →
//!   streamed sha256 verify → allowlist-validated extraction → ready manifest
//!   → atomic rename into `<updates-root>/<tag>/`.
//!
//! A stage is complete iff its ready manifest parses and matches the wanted
//! identity — never because a marker file merely exists.

pub mod fetch;
pub mod identity;
pub mod lock;
pub mod manifest;
pub mod pending;
pub mod platform;
pub mod prepare;
pub mod select;
pub mod semver;
pub mod unique;

pub use fetch::{archive, hash, sums};

use std::cmp::Ordering;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{bail, Context, Result};

pub use fetch::Fetcher;
pub use identity::ReleaseIdentity;
pub use manifest::{resolve_updates_root, InstallManifest, ReadyManifest};
pub use select::select_target;

/// How long a stage will wait for another process's stage to finish before
/// giving up on the lock. Must exceed a worst-case stage hold (a 900s
/// download plus hashing plus a 300s extract); a giver-upper only warns and
/// retries on the next daily cycle, but waiting through a sibling's stage is
/// strictly better.
const LOCK_WAIT: Duration = Duration::from_secs(3600);

/// Backoff before giving up on the final commit rename (Defect 0c): on
/// Windows, antivirus scanning the freshly extracted `.exe`s can hold one of
/// them open for a few seconds, and `rename` on the whole tree fails with
/// "Access is denied" until it lets go. About one minute total. A `const`
/// slice so a test can substitute a fast one via [`commit_stage`]'s
/// `backoff` parameter.
const RENAME_BACKOFF: &[Duration] = &[
    Duration::from_secs(1),
    Duration::from_secs(2),
    Duration::from_secs(4),
    Duration::from_secs(8),
    Duration::from_secs(16),
    Duration::from_secs(30),
];

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

/// Query the release this installed version's channel should track and pin
/// this platform's identity. Deliberately takes no staging root: a check
/// must work (and report availability) even on hosts where no updates root
/// resolves.
pub async fn check_release(repo: &str, current_version: &str, fetcher: &Fetcher) -> CheckOutcome {
    let Some(target) = platform::this_platform() else {
        return outcome_err(format!(
            "platform {}-{} is not in the release matrix",
            platform::TARGET_OS,
            platform::TARGET_ARCH
        ));
    };
    let latest = match fetcher.latest(repo, current_version).await {
        Ok(Some(l)) => l,
        // Nothing in this install's channel beats `current_version` — a
        // normal, non-error "already current" outcome, not a fetch failure.
        Ok(None) => {
            return CheckOutcome {
                identity: None,
                latest: String::new(),
                update_available: false,
                status: "ok".into(),
            }
        }
        Err(e) => return outcome_err(format!("check unavailable: {e}")),
    };
    let entries = match sums::parse_sums(&latest.sums_text) {
        Ok(v) => v,
        Err(e) => return outcome_err(format!("bad SHA256SUMS: {e}")),
    };
    let identity = match sums::discover(&entries, repo, target) {
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
        semver::compare_versions(&identity.version, current_version) == Ordering::Greater;
    CheckOutcome {
        latest: identity.version.clone(),
        identity: Some(identity),
        update_available,
        status: "ok".into(),
    }
}

/// Back-compat wrapper: check via a full config (ignores the root).
pub async fn check(cfg: &UpdaterConfig) -> CheckOutcome {
    check_release(&cfg.repo, &cfg.current_version, &cfg.fetcher).await
}

/// The completed-stage directory for one release identity. Keyed by tag AND
/// target: on a shared `$HOME`, machines of different platforms share one
/// updates root, and tag-only dirs would make them clobber each other's
/// completed stages in an endless re-download ping-pong.
pub fn stage_dir(updates_root: &Path, id: &ReleaseIdentity) -> PathBuf {
    updates_root.join(format!("{}-{}", id.tag, id.target))
}

/// True when the identity's stage dir holds a completed stage of exactly `id`.
pub async fn is_staged(updates_root: &Path, id: &ReleaseIdentity) -> bool {
    ReadyManifest::matches(&stage_dir(updates_root, id), id).await
}

/// Where an IN-PROGRESS stage of one release lives until it is committed.
///
/// Keyed by tag and target, not by pid: a staging task dies with its process
/// (a frontend's does on every converge restart), and the next attempt must
/// find the bytes the last one downloaded. Two identities can share this name
/// only by sharing tag and target — a re-cut release — and the reuse gate is
/// the content hash of the identity being staged, never the dir name, so that
/// case is discriminated by digest rather than by path.
fn partial_dir(updates_root: &Path, id: &ReleaseIdentity) -> PathBuf {
    updates_root.join(format!("tmp-{}-{}", id.tag, id.target))
}

/// Bytes of `id`'s asset sitting in an interrupted stage — what [`stage`]
/// would resume from. `None` when there is nothing to resume.
///
/// A progress reading, not a promise: these bytes are unverified here by
/// construction (the hash gate lives in `stage`, which is where trusting them
/// would matter). Two readings a minute apart tell an operator whether a box
/// is downloading or wedged — the question repeated converges were being used
/// to guess at.
pub async fn partial_asset_bytes(updates_root: &Path, id: &ReleaseIdentity) -> Option<u64> {
    tokio::fs::metadata(partial_dir(updates_root, id).join(&id.asset))
        .await
        .ok()
        .map(|m| m.len())
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
    let dest = stage_dir(&cfg.updates_root, id);
    if dest.exists() {
        // Present but not a matching completed stage: a partial from a
        // crashed run, or different contents under the same tag. Rebuild it.
        tracing::warn!(dir = %dest.display(), "removing incomplete/mismatched stage dir");
        tokio::fs::remove_dir_all(&dest)
            .await
            .with_context(|| format!("removing stale stage dir {}", dest.display()))?;
    }
    // Resumable from here on. The partial dir is keyed to the release, so an
    // attempt killed mid-download (every converge kills the frontend's) hands
    // its bytes to the next one instead of leaving them behind a dead pid;
    // `sweep_stale_tmp` must not reap the one we are about to resume.
    let tmp = partial_dir(&cfg.updates_root, id);
    sweep_stale_tmp(&cfg.updates_root, Some(tmp.as_path())).await;

    tokio::fs::create_dir_all(&tmp)
        .await
        .with_context(|| format!("creating staging temp dir {}", tmp.display()))?;

    let result = async {
        let archive_path = tmp.join(&id.asset);
        let want = id.asset_sha256.to_ascii_lowercase();
        // Resume, not trust. An asset left by a killed attempt counts only
        // when its WHOLE content hashes to the release's published digest, so
        // a truncated download can never be mistaken for a complete one —
        // this is the same gate the fresh-download path goes through, asked
        // first so it can skip the download instead of only confirming it.
        if hash::sha256_file(&archive_path).await.ok().as_deref() == Some(want.as_str()) {
            tracing::info!(tag = %id.tag, asset = %id.asset, "reusing the verified asset from an interrupted stage");
        } else {
            let _ = tokio::fs::remove_file(&archive_path).await;
            cfg.fetcher
                .download(&id.repo, &id.tag, &id.asset, &archive_path)
                .await
                .context("downloading release asset")?;
            let got = hash::sha256_file(&archive_path).await?;
            if got != want {
                bail!(
                    "sha256 mismatch for {}: expected {}, got {got}",
                    id.asset,
                    id.asset_sha256
                );
            }
        }
        let top = release_top_dir(&id.asset)?;
        // The asset is the only thing a partial dir may contribute. A tree a
        // previous extract left half-written is rebuilt, never resumed: it
        // carries no digest of its own, so nothing could tell it apart from a
        // complete one.
        let _ = tokio::fs::remove_dir_all(tmp.join(&top)).await;
        archive::extract_validated(&archive_path, &tmp, &top)
            .await
            .context("extracting release archive")?;

        // Per-file digests of the validated tree, in `sha256sum -c` format —
        // the shell applier re-verifies the ACTUAL binaries it installs
        // against these (the archive hash alone doesn't bind the extracted,
        // independently-mutable files).
        let mut sums_lines = String::new();
        let mut rd = tokio::fs::read_dir(tmp.join(&top)).await?;
        while let Some(entry) = rd.next_entry().await? {
            let name = entry.file_name().to_string_lossy().into_owned();
            let digest = hash::sha256_file(&entry.path()).await?;
            sums_lines.push_str(&format!("{digest}  {top}/{name}\n"));
        }
        tokio::fs::write(tmp.join("files.sha256"), sums_lines)
            .await
            .context("writing files.sha256")?;

        // Commit binding: releases that publish a COMMIT file (sums-listed)
        // pin the source commit the binaries were built from; prepare later
        // refuses a tag whose commit disagrees (moved-tag defense). Legacy
        // releases without one stage with a warning.
        let source_commit = fetch_source_commit(cfg, id, &tmp).await?;

        ReadyManifest::new(id.clone(), source_commit).write(&tmp).await?;
        Ok::<(), anyhow::Error>(())
    }
    .await;

    if let Err(e) = result {
        // Kept on purpose. When the asset itself finished downloading and a
        // LATER step failed (extract, digests, commit binding, manifest
        // write), the next attempt's hash check above finds a complete match
        // and skips straight to rebuilding what's derived from it — a failure
        // there costs one cycle, not the whole download. A download killed
        // mid-transfer still restarts from zero: there is no byte-range
        // resume here, only whole-file verification. `sweep_stale_tmp` reaps
        // this dir once the release stops being the one we chase.
        tracing::warn!(dir = %tmp.display(), "stage failed — keeping the partial dir so the next attempt resumes");
        return Err(e);
    }
    // The rename retries on its own (Defect 0c); a failure here means the
    // WHOLE backoff was exhausted, and `tmp` is deliberately left in place —
    // same reasoning as the branch above, so the next stage attempt (the
    // hash-match reuse path) picks it straight back up instead of
    // re-downloading.
    commit_stage(&dest, &cfg.updates_root, RENAME_BACKOFF, || async {
        tokio::fs::rename(&tmp, &dest).await
    })
    .await
    .with_context(|| format!("committing stage into {}", dest.display()))?;
    tracing::info!(tag = %id.tag, asset = %id.asset, dir = %dest.display(), "update staged");
    Ok(true)
}

/// Fetch + verify the release's `COMMIT` file (tag-pinned): re-download the
/// tag's own SHA256SUMS, and when it lists a COMMIT entry, download it,
/// verify its digest, and return the 40-hex commit. `Ok(None)` for releases
/// that predate commit publishing; a listed-but-unverifiable COMMIT is an
/// error (the stage retries next cycle).
async fn fetch_source_commit(
    cfg: &UpdaterConfig,
    id: &ReleaseIdentity,
    tmp: &Path,
) -> Result<Option<String>> {
    let sums_path = tmp.join("SHA256SUMS");
    cfg.fetcher
        .download(&id.repo, &id.tag, "SHA256SUMS", &sums_path)
        .await
        .context("downloading tag-pinned SHA256SUMS")?;
    let sums_text = tokio::fs::read_to_string(&sums_path).await?;
    let entries = sums::parse_sums(&sums_text)?;
    // Cross-check: the tag-pinned sums must agree with the discovery-time
    // digest for our asset (a half-moved release dies here).
    let pinned = sums::lookup(&entries, &id.asset)?;
    if pinned != id.asset_sha256.to_ascii_lowercase() {
        bail!(
            "tag-pinned SHA256SUMS digest for {} disagrees with discovery — refusing",
            id.asset
        );
    }
    let Ok(commit_digest) = sums::lookup(&entries, "COMMIT") else {
        tracing::warn!(tag = %id.tag, "release publishes no COMMIT file — source-commit binding unavailable (legacy release)");
        return Ok(None);
    };
    let commit_path = tmp.join("COMMIT");
    cfg.fetcher
        .download(&id.repo, &id.tag, "COMMIT", &commit_path)
        .await
        .context("downloading COMMIT")?;
    let got = hash::sha256_file(&commit_path).await?;
    if got != commit_digest {
        bail!("COMMIT file digest mismatch — refusing");
    }
    let commit = tokio::fs::read_to_string(&commit_path).await?.trim().to_string();
    if commit.len() != 40 || !commit.chars().all(|c| c.is_ascii_hexdigit()) {
        bail!("COMMIT file does not contain a commit hash: {commit:?}");
    }
    Ok(Some(commit))
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

/// Best-effort cleanup of abandoned `tmp-*` staging dirs older than a day,
/// except `keep` — the one this attempt is resuming, whose age says how long
/// ago the download started, not that it was abandoned.
///
/// These are now one per release-target rather than one per crashed run, so
/// what this reaps is a release we have stopped chasing.
async fn sweep_stale_tmp(root: &Path, keep: Option<&Path>) {
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
        if keep == Some(entry.path().as_path()) {
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

/// Remove every OTHER `tmp-*` dir in `root`, unconditionally — no age gate.
/// Called only right after a stage COMMITS: the one THIS stage used already
/// got renamed to `just_committed` and no longer carries a `tmp-` name, so
/// any `tmp-*` still found here can only be litter from an earlier release's
/// failed commit, never work in progress. This is what actually bounds the
/// accumulation Defect 0c's evidence describes: `sweep_stale_tmp`'s 24h age
/// gate never reaps the dir a box keeps re-chasing, so a release that never
/// manages to commit piles its tmp dir up forever; a successful commit for
/// ANY release is the first safe point to clear all of them out.
async fn sweep_all_tmp(root: &Path, just_committed: &Path) {
    let Ok(mut rd) = tokio::fs::read_dir(root).await else {
        return;
    };
    while let Ok(Some(entry)) = rd.next_entry().await {
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        if !name.starts_with("tmp-") {
            continue;
        }
        if entry.path() == just_committed {
            continue;
        }
        tracing::info!(dir = %entry.path().display(), "sweeping leftover staging temp dir after a successful commit");
        let _ = tokio::fs::remove_dir_all(entry.path()).await;
    }
}

/// Retry `attempt` until it succeeds, sleeping the next entry of `backoff`
/// between failures and returning the LAST error once the schedule is
/// exhausted. Generic over both the operation and the schedule so a test can
/// substitute a synthetic seam (fails on demand) and a fast table, without
/// touching the real filesystem or a real clock.
async fn retry_with_backoff<T, E, F, Fut>(mut backoff: &[Duration], mut attempt: F) -> Result<T, E>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<T, E>>,
{
    loop {
        match attempt().await {
            Ok(v) => return Ok(v),
            Err(e) => match backoff.split_first() {
                Some((&delay, rest)) => {
                    tokio::time::sleep(delay).await;
                    backoff = rest;
                }
                None => return Err(e),
            },
        }
    }
}

/// Commit a completed stage: retry `rename_once` per `backoff` (Defect 0c —
/// Windows antivirus can hold a just-extracted file open for a few seconds),
/// and on success sweep every other abandoned `tmp-*` dir out of
/// `updates_root`. `rename_once` is the seam: production passes the real
/// `tokio::fs::rename`, a test passes a closure that fails on demand.
async fn commit_stage<F, Fut>(
    dest: &Path,
    updates_root: &Path,
    backoff: &[Duration],
    rename_once: F,
) -> std::io::Result<()>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = std::io::Result<()>>,
{
    retry_with_backoff(backoff, rename_once).await?;
    sweep_all_tmp(updates_root, dest).await;
    Ok(())
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
        // ADR 0042 slice L1a: `sot-capsule` is a required file in every
        // release archive now (see `archive.rs`'s own `validate_tree`).
        tokio::fs::write(build.join("sot-capsule"), b"capsule-binary").await.unwrap();
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

        let staged_bin = stage_dir(&updates, &id).join(&top).join("sot");
        assert_eq!(tokio::fs::read(&staged_bin).await.unwrap(), b"fe-binary");
        let manifest = ReadyManifest::read(&stage_dir(&updates, &id)).await.unwrap();
        assert_eq!(manifest.identity, id);

        // A running check against the staged version reports no update.
        let cfg_current = UpdaterConfig {
            current_version: version.into(),
            ..cfg.clone()
        };
        let out2 = check(&cfg_current).await;
        assert_eq!(out2.status, "ok");
        assert!(!out2.update_available);

        // A converge restart kills the frontend's staging task mid-download.
        // What the next attempt may pick up is bounded by one rule: only the
        // asset, and only on a full-content digest match.
        let partial = updates.join(format!("tmp-{}-{}", id.tag, id.target));
        let asset_len = tokio::fs::metadata(&archive).await.unwrap().len();

        // Bytes that are NOT this release's are discarded, not trusted — the
        // truncated-download case, which a size or existence test would pass.
        tokio::fs::remove_dir_all(stage_dir(&updates, &id)).await.unwrap();
        tokio::fs::create_dir_all(&partial).await.unwrap();
        tokio::fs::write(partial.join(&asset), b"half a download").await.unwrap();
        assert!(stage(&cfg, &id).await.unwrap());
        assert!(is_staged(&updates, &id).await);
        assert_eq!(tokio::fs::read(&staged_bin).await.unwrap(), b"fe-binary");

        // The verified asset IS reused. Proven by making a download
        // impossible: the asset is removed from the source the fetcher pulls
        // from, so an attempt that started again instead of continuing fails.
        tokio::fs::remove_dir_all(stage_dir(&updates, &id)).await.unwrap();
        tokio::fs::create_dir_all(&partial).await.unwrap();
        tokio::fs::copy(&archive, partial.join(&asset)).await.unwrap();
        tokio::fs::remove_file(&archive).await.unwrap();
        assert_eq!(partial_asset_bytes(&updates, &id).await, Some(asset_len));
        assert!(stage(&cfg, &id).await.unwrap());
        assert!(is_staged(&updates, &id).await);
        assert_eq!(tokio::fs::read(&staged_bin).await.unwrap(), b"fe-binary");
        // Committing consumes the partial dir: nothing is left claiming to be
        // resumable once the stage it fed is complete.
        assert_eq!(partial_asset_bytes(&updates, &id).await, None);

        tokio::fs::remove_dir_all(&base).await.unwrap();
    }
}

/// Deterministic — no unix gate: the rename-retry-and-sweep mechanism itself
/// doesn't touch anything platform-specific, and Windows is exactly the
/// platform Defect 0c is about.
#[cfg(test)]
mod commit_retry_tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    /// Sub-millisecond entries — this schedule is under test for retry
    /// COUNT and eventual outcome, not real wall-clock backoff.
    const FAST_BACKOFF: &[Duration] = &[
        Duration::from_millis(1),
        Duration::from_millis(1),
        Duration::from_millis(1),
    ];

    fn tmp_dir_names(root: &Path) -> Vec<String> {
        std::fs::read_dir(root)
            .map(|rd| {
                rd.filter_map(|e| e.ok())
                    .map(|e| e.file_name().to_string_lossy().into_owned())
                    .filter(|n| n.starts_with("tmp-"))
                    .collect()
            })
            .unwrap_or_default()
    }

    fn scratch_root(case: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "sot-updater-retry-{case}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }

    /// The commit fails the first two attempts (simulating the file still
    /// being held), succeeds on the third — and only THEN does the tree
    /// change: through the failing attempts, `tmp` is untouched and no
    /// second `tmp-*` appears (item 2 — one tmp, reused, never re-created).
    /// Two abandoned `tmp-*` dirs from earlier, never-committed releases sit
    /// alongside it; the successful commit sweeps them (item 3), unlike the
    /// age-gated `sweep_stale_tmp` which would leave them for 24h.
    #[tokio::test]
    async fn commit_retries_then_succeeds_and_sweeps_stray_tmp_dirs() {
        let root = scratch_root("success");
        let _ = tokio::fs::remove_dir_all(&root).await;
        tokio::fs::create_dir_all(&root).await.unwrap();

        let tmp = root.join("tmp-v9.9.9-linux-x86_64");
        tokio::fs::create_dir_all(&tmp).await.unwrap();
        let stray_a = root.join("tmp-v9.9.7-linux-x86_64");
        let stray_b = root.join("tmp-v9.9.8-linux-x86_64");
        tokio::fs::create_dir_all(&stray_a).await.unwrap();
        tokio::fs::create_dir_all(&stray_b).await.unwrap();
        let dest = root.join("v9.9.9-linux-x86_64");

        let attempts = AtomicU32::new(0);
        let result = commit_stage(&dest, &root, FAST_BACKOFF, || {
            let n = attempts.fetch_add(1, Ordering::SeqCst);
            let tmp = tmp.clone();
            let dest = dest.clone();
            let root = root.clone();
            async move {
                if n < 2 {
                    // The rename never ran — the tree, including the exactly
                    // one live `tmp-*`, is exactly as it was.
                    assert_eq!(tmp_dir_names(&root).len(), 3);
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::PermissionDenied,
                        "Access is denied. (os error 5)",
                    ));
                }
                tokio::fs::rename(&tmp, &dest).await
            }
        })
        .await;

        assert!(result.is_ok(), "{result:?}");
        assert_eq!(attempts.load(Ordering::SeqCst), 3);
        assert!(dest.is_dir());
        assert!(!tmp.exists());
        assert!(!stray_a.exists());
        assert!(!stray_b.exists());
        assert!(tmp_dir_names(&root).is_empty());

        tokio::fs::remove_dir_all(&root).await.unwrap();
    }

    /// A commit that never succeeds exhausts the backoff and surfaces the
    /// last (real) error — the shape `rust/backend/src/update.rs` turns into
    /// `update blocked: <error>`.
    #[tokio::test]
    async fn commit_exhausts_backoff_and_surfaces_the_last_error() {
        let root = scratch_root("exhausted");
        let _ = tokio::fs::remove_dir_all(&root).await;
        tokio::fs::create_dir_all(&root).await.unwrap();
        let dest = root.join("v9.9.9-linux-x86_64");

        let attempts = AtomicU32::new(0);
        let result = commit_stage(&dest, &root, FAST_BACKOFF, || {
            attempts.fetch_add(1, Ordering::SeqCst);
            async {
                Err(std::io::Error::new(
                    std::io::ErrorKind::PermissionDenied,
                    "Access is denied. (os error 5)",
                ))
            }
        })
        .await;

        let err = result.expect_err("every attempt failed — must not report success");
        assert!(err.to_string().contains("Access is denied"), "{err}");
        // One initial attempt plus one retry per backoff entry.
        assert_eq!(attempts.load(Ordering::SeqCst), FAST_BACKOFF.len() as u32 + 1);
        assert!(!dest.exists());

        tokio::fs::remove_dir_all(&root).await.unwrap();
    }
}

/// The captain's real-world case (Defect 0c), Windows only: a directory
/// rename is unaffected by an open file descriptor on Unix, so this test
/// would pass on Linux/macOS without exercising anything — and the macOS CI
/// leg runs every unguarded `cfg(unix)` test, so it cannot be left unguarded.
#[cfg(all(test, windows))]
mod windows_lock_tests {
    use super::*;
    use std::os::windows::fs::OpenOptionsExt;

    /// No sharing at all — the shape an antivirus scanner's open handle
    /// takes. Held for 10s, then released; the commit must still succeed,
    /// via the real (non-fast) [`RENAME_BACKOFF`].
    #[tokio::test]
    async fn commit_survives_an_antivirus_style_open_handle() {
        let root = std::env::temp_dir().join(format!(
            "sot-updater-winlock-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = tokio::fs::remove_dir_all(&root).await;
        tokio::fs::create_dir_all(&root).await.unwrap();
        let tmp = root.join("tmp-v9.9.9-windows-x86_64");
        tokio::fs::create_dir_all(&tmp).await.unwrap();
        let locked_file = tmp.join("sotd.exe");
        tokio::fs::write(&locked_file, b"be-binary").await.unwrap();
        let dest = root.join("v9.9.9-windows-x86_64");

        const FILE_SHARE_NONE: u32 = 0;
        let handle = std::fs::OpenOptions::new()
            .read(true)
            .share_mode(FILE_SHARE_NONE)
            .open(&locked_file)
            .expect("must be able to open the file exclusively to simulate a scanner's lock");

        let releaser = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_secs(10)).await;
            drop(handle);
        });

        let result = commit_stage(&dest, &root, RENAME_BACKOFF, || async {
            tokio::fs::rename(&tmp, &dest).await
        })
        .await;

        releaser.await.unwrap();
        assert!(result.is_ok(), "{result:?}");
        assert!(dest.is_dir());

        tokio::fs::remove_dir_all(&root).await.unwrap();
    }
}
