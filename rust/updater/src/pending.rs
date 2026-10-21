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
//!
//! The install is the authority on what is installed, not any process: on a
//! shared `$HOME` one prefix serves every host's daemon, and a long-lived one
//! knows only the version it booted with. So an automatic arm reads
//! `<prefix>/install.json` fresh under the lock and arms only a version strictly above it (it refuses over one that
//! exists and cannot be read, which may hold a pin), and every pointer records the install it was armed over
//! (`armed_over_*`) so an apply can tell a pointer that no longer fits the install it would land on (and refuses a
//! pointer with such a record when the install cannot be read).

use std::cmp::Ordering;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::identity::ReleaseIdentity;
use crate::manifest::{missing_record, InstallFile, InstallManifest, UnreadableInstall, INSTALL_MANIFEST};
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
    /// `install.json`'s version and commit when this was armed: the install it was armed over. Absent in a pointer
    /// armed before the record existed, or with no install manifest to read.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub armed_over_version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub armed_over_commit: Option<String>,
}

/// What `<prefix>/install.json` says is installed: its version and commit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstalledBuild {
    pub version: String,
    pub commit: Option<String>,
    /// The install's run named what to install, so no automatic writer moves it.
    pub pinned: bool,
}

impl InstalledBuild {
    /// Read fresh from the install beside `updates_root` (`<prefix>/updates`), in three states: `Ok(None)` is a layout with
    /// no install (a dev box, a test root, an `install.json` that is absent with no Windows install beside it, or names
    /// no version), where nothing is installed to guard; `Err` is an `install.json` that exists and cannot be read (cut
    /// short by an interrupted write, not JSON, or saying `"pinned": true` with no version to pin) or, on Windows, one
    /// that is missing with `bin\sotd.exe` beside it ([`missing_record`]), which may hold a pin that an automatic writer
    /// must not lose. Each caller states its choice for the `Err`.
    pub fn try_read(updates_root: &Path) -> std::result::Result<Option<Self>, UnreadableInstall> {
        let Some(prefix) = updates_root.parent() else { return Ok(None) };
        let path = prefix.join(INSTALL_MANIFEST);
        match InstallManifest::load(&path) {
            InstallFile::Absent => missing_record(prefix).map_or(Ok(None), Err),
            InstallFile::Unreadable(u) => Err(u),
            InstallFile::Read(m) => match m.version {
                Some(version) => Ok(Some(Self { version, commit: m.commit, pinned: m.pinned == Some(true) })),
                None if m.pinned == Some(true) => Err(UnreadableInstall { path, cause: "it says pinned but records no version".into(), missing: false }),
                None => Ok(None),
            },
        }
    }

    /// The installed version read fresh, or `fallback` (the running process's) when there is no install to read or it
    /// cannot be: every caller that acts on the answer has been stopped first by the unreadable-install guard (the
    /// check's basis, the window's guard, the arm), so the fallback only names a channel for a status. The channel a
    /// check follows and the version it compares with are the install's, not the boot version of a process that may be
    /// older than the install.
    pub fn version_or(updates_root: &Path, fallback: &str) -> String {
        Self::try_read(updates_root).ok().flatten().map_or_else(|| fallback.to_string(), |i| i.version)
    }

    /// Is the install a build that is not a release (a `+` in the version, the test `install-manifest.ps1` uses)? A
    /// release is never armed into one, and a pointer with no record never applies onto one.
    pub fn is_dev_build(&self) -> bool {
        self.version.contains('+')
    }

    /// Why no automatic writer moves this install, or `None`: a pinned install stays where the run that named its
    /// version put it.
    pub fn pin_note(&self) -> Option<String> {
        self.pinned.then(|| {
            format!(
                "pinned at {}: automatic updates stay off for this install until a run that names no version",
                self.version
            )
        })
    }

    /// May an automatic arm offer `version`: only a release strictly above the installed build, not into a dev
    /// build and not into a pinned install.
    pub fn admits(&self, version: &str) -> bool {
        !self.is_dev_build() && !self.pinned && compare_versions(version, &self.version) == Ordering::Greater
    }
}

/// Who writes the pointer. An automatic writer (the daemon's daily check, the window's self-update) offers only a
/// release strictly above the install; an operator names the folder and may move the install anywhere, so
/// `sotd update stage` is the one operator.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Writer {
    Automatic,
    Operator,
}

/// What an [`arm`] did when it did not fail.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ArmOutcome {
    Armed,
    /// A pointer for a newer release is already armed over this install.
    KeptNewer,
    /// The version crash-looped after an apply and is marked bad.
    MarkedBad,
    /// An automatic writer offered a version at or below the installed build.
    NotAboveInstall,
    /// An automatic writer offered a release for an install that is a dev build.
    DevInstall,
    /// An automatic writer offered a release for an install pinned by the run that named its version.
    Pinned,
    /// An automatic writer found an `install.json` it cannot read, so it cannot tell whether the install is pinned.
    UnreadableInstall(UnreadableInstall),
}

impl ArmOutcome {
    /// Why nothing was armed, or `None` when it was: the one sentence every caller logs or prints, so none paraphrases
    /// the outcome.
    pub fn not_armed_because(&self) -> Option<String> {
        Some(match self {
            Self::Armed => return None,
            Self::KeptNewer => "a newer release is already armed".into(),
            Self::MarkedBad => "this release crash-looped after an apply and is marked bad".into(),
            Self::NotAboveInstall => "the release is not above the installed build".into(),
            Self::DevInstall => "the install is a dev build".into(),
            Self::Pinned => "the install is pinned".into(),
            Self::UnreadableInstall(u) => u.sentence(),
        })
    }
}

/// Why `ptr` must not apply onto `installed`, or `None` when it may. A pointer that records the install it was armed
/// over applies only while that install is still the one on disk. A pointer with no record (armed before the record
/// existed) applies only strictly above the installed version, the one thing it can still be judged by, and never
/// onto a dev build.
pub fn stale_reason(ptr: &PendingPointer, installed: &InstalledBuild) -> Option<String> {
    match &ptr.armed_over_version {
        Some(over) => {
            let same = *over == installed.version && ptr.armed_over_commit == installed.commit;
            (!same).then(|| {
                format!(
                    "armed over {over} but the install is now {}; it no longer fits",
                    installed.version
                )
            })
        }
        None if installed.is_dev_build() => Some(format!(
            "the installed {} is a dev build, which a pointer with no record never applies onto",
            installed.version
        )),
        None => (!installed.admits(&ptr.identity.version)).then(|| {
            format!(
                "{} is not above the installed {}",
                ptr.identity.version, installed.version
            )
        }),
    }
}

/// Why `ptr` must not apply onto the install `installed` says now, or `None` when it may. A pointer that records the
/// install it was armed over (every pointer a Rust writer armed over a readable install) cannot be checked against one
/// that cannot be read, and is refused; a pointer with no record meets the apply's exact-tag check, as it does with no
/// install at all.
pub fn pointer_refusal(ptr: &PendingPointer, installed: std::result::Result<Option<InstalledBuild>, UnreadableInstall>) -> Option<String> {
    match installed {
        Ok(installed) => stale_reason(ptr, &installed?),
        Err(u) => ptr.armed_over_version.as_ref().map(|over| format!("armed over {over}, which cannot be checked: {}", u.sentence())),
    }
}

/// Arm `identity` for apply on its target. A version marked bad (crash-loop
/// rollback) is never armed again. An [`Writer::Automatic`] writer arms only a
/// version strictly above the install read from `install.json` now, under the
/// lock, and never into a dev build. Newer-wins, for an automatic writer: when a
/// pending pointer for a NEWER version that still fits the install already
/// exists, it is kept; same, older and no-longer-fitting pendings are replaced.
/// The operator's arm replaces any pointer. The pointer records the install it was armed over.
/// Serialized across processes via the staging lock.
pub async fn arm(
    updates_root: &Path,
    identity: &ReleaseIdentity,
    checkout: &Path,
    commit: &str,
    writer: Writer,
) -> Result<ArmOutcome> {
    identity.validate()?;
    let lock = crate::lock::StageLock::acquire(updates_root, Duration::from_secs(120)).await?;
    let result = arm_locked(updates_root, identity, checkout, commit, writer).await;
    lock.release();
    result
}

async fn arm_locked(
    updates_root: &Path,
    identity: &ReleaseIdentity,
    checkout: &Path,
    commit: &str,
    writer: Writer,
) -> Result<ArmOutcome> {
    if bad_marker(updates_root, &identity.tag, &identity.target).exists() {
        tracing::warn!(tag = %identity.tag, "version is marked bad (crash-loop rollback) — not arming");
        return Ok(ArmOutcome::MarkedBad);
    }
    let installed = match (InstalledBuild::try_read(updates_root), writer) {
        (Ok(installed), _) => installed,
        (Err(unreadable), Writer::Automatic) => {
            tracing::warn!(offered = %identity.tag, "not arming: {}", unreadable.sentence());
            return Ok(ArmOutcome::UnreadableInstall(unreadable));
        }
        (Err(_), Writer::Operator) => None,
    };
    if let (Writer::Automatic, Some(installed)) = (writer, &installed) {
        if installed.is_dev_build() {
            tracing::info!(installed = %installed.version, offered = %identity.tag, "the install is a dev build — not arming a release into it");
            return Ok(ArmOutcome::DevInstall);
        }
        if let Some(note) = installed.pin_note() {
            tracing::info!(offered = %identity.tag, "{note} — not arming");
            return Ok(ArmOutcome::Pinned);
        }
        if !installed.admits(&identity.version) {
            tracing::info!(
                installed = %installed.version,
                offered = %identity.tag,
                "offered release is not above the installed build — not arming"
            );
            return Ok(ArmOutcome::NotAboveInstall);
        }
    }
    // The operator's arm replaces whatever is armed: it named the folder, so it is not the late finisher newer-wins keeps out.
    let existing = match writer {
        Writer::Automatic => read(updates_root, &identity.target).await,
        Writer::Operator => Ok(None),
    };
    if let Ok(Some(existing)) = existing {
        let fits = installed.as_ref().is_none_or(|i| stale_reason(&existing, i).is_none());
        if fits && compare_versions(&existing.identity.version, &identity.version) == Ordering::Greater {
            tracing::info!(
                existing = %existing.identity.tag,
                offered = %identity.tag,
                "pending pointer already arms a newer release — keeping it"
            );
            return Ok(ArmOutcome::KeptNewer);
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
        armed_over_version: installed.as_ref().map(|i| i.version.clone()),
        armed_over_commit: installed.and_then(|i| i.commit),
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
    Ok(ArmOutcome::Armed)
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

#[cfg(test)]
#[path = "pending_tests.rs"]
mod tests;
