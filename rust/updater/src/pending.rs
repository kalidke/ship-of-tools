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
//! `<prefix>/install.json` fresh under the lock and arms only a version
//! strictly above it, and every pointer records the install it was armed over
//! (`armed_over_*`) so an apply can tell a pointer that no longer fits the
//! install it would land on.

use std::cmp::Ordering;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::identity::ReleaseIdentity;
use crate::manifest::{InstallManifest, INSTALL_MANIFEST};
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
    /// Read fresh from the install beside `updates_root` (`<prefix>/updates`). `None` when there is no readable
    /// manifest naming a version: a layout with no install (a dev box, a test root), where nothing is installed to
    /// guard.
    pub fn read(updates_root: &Path) -> Option<Self> {
        let path = updates_root.parent()?.join(INSTALL_MANIFEST);
        let manifest = InstallManifest::read(&path).ok()?;
        Some(Self { version: manifest.version?, commit: manifest.commit, pinned: manifest.pinned == Some(true) })
    }

    /// Read the installed version fresh, or `fallback` (the running process's) when there is no install to read. The
    /// channel a check follows and the version it compares with are the install's, not the boot version of a process
    /// that may be older than the install.
    pub fn version_or(updates_root: &Path, fallback: &str) -> String {
        Self::read(updates_root).map_or_else(|| fallback.to_string(), |i| i.version)
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
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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
    let installed = InstalledBuild::read(updates_root);
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

    /// A fresh `<prefix>/updates` (returned) beside a `<prefix>/install.json` naming `installed`, or none.
    async fn prefix(name: &str, installed: Option<(&str, &str)>) -> PathBuf {
        let prefix = std::env::temp_dir().join(format!("sot-updater-{name}-{}", std::process::id()));
        let _ = tokio::fs::remove_dir_all(&prefix).await;
        tokio::fs::create_dir_all(prefix.join("updates")).await.unwrap();
        if let Some((version, commit)) = installed {
            install(&prefix, version, commit, None).await;
        }
        prefix.join("updates")
    }

    /// Write `<prefix>/install.json` naming `version`, with `pinned` only when given. serde writes it, so a prefix holding
    /// a `\` (every Windows path) or a `"` is escaped as an installer escapes it, and the install must read back: a
    /// manifest that does not parse reads as no install, and every arm over it would pass for the wrong reason.
    async fn install(prefix: &Path, version: &str, commit: &str, pinned: Option<bool>) {
        let mut json = serde_json::json!({
            "schema": 1, "prefix": prefix, "version": version, "tag": format!("v{version}"), "commit": commit,
        });
        if let Some(pinned) = pinned {
            json["pinned"] = pinned.into();
        }
        tokio::fs::write(prefix.join(INSTALL_MANIFEST), json.to_string()).await.unwrap();
        let read = InstalledBuild::read(&prefix.join("updates")).expect("the fixture's install.json reads back");
        assert_eq!((read.version.as_str(), read.pinned), (version, pinned == Some(true)));
    }

    async fn cleanup(root: &Path) {
        tokio::fs::remove_dir_all(root.parent().unwrap()).await.unwrap();
    }

    #[tokio::test]
    async fn arm_read_clear_newer_wins_and_bad_marker() {
        let root = prefix("pending", None).await;

        assert!(read(&root, T).await.unwrap().is_none());

        let co = PathBuf::from("/x/repo/versions/v0.6.0");
        let offer = |v: &'static str, c: &'static str| {
            let (root, co) = (root.clone(), co.clone());
            async move { arm(&root, &identity(v), &co, c, Writer::Automatic).await.unwrap() }
        };
        assert_eq!(offer("0.6.0", "aaa").await, ArmOutcome::Armed);
        let got = read(&root, T).await.unwrap().unwrap();
        assert_eq!(got.identity.tag, "v0.6.0");
        // Another target sees nothing.
        assert!(read(&root, "macos-aarch64").await.unwrap().is_none());

        // Newer replaces older…
        assert_eq!(offer("0.7.0", "bbb").await, ArmOutcome::Armed);
        // …but an older late-finisher cannot clobber a newer arm.
        assert_eq!(offer("0.6.1", "ccc").await, ArmOutcome::KeptNewer);
        assert_eq!(read(&root, T).await.unwrap().unwrap().identity.tag, "v0.7.0");

        // A bad-marked version refuses to arm.
        std::fs::write(bad_marker(&root, "v0.8.0", T), b"").unwrap();
        assert_eq!(offer("0.8.0", "ddd").await, ArmOutcome::MarkedBad);

        cleanup(&root).await;
    }

    #[tokio::test]
    async fn an_automatic_arm_goes_only_above_the_install_and_records_it() {
        let co = PathBuf::from("/x/repo/versions/v0.6.6-rc9.13");
        // The red this pins: a stale process offers rc9.13 into an install that already moved past it.
        for installed in ["0.6.6-rc9.13", "0.6.6-rc9.14"] {
            let root = prefix("below", Some((installed, "c0"))).await;
            let out = arm(&root, &identity("0.6.6-rc9.13"), &co, "cc", Writer::Automatic).await.unwrap();
            assert_eq!(out, ArmOutcome::NotAboveInstall, "installed {installed}");
            assert!(read(&root, T).await.unwrap().is_none(), "installed {installed}: nothing armed");
            cleanup(&root).await;
        }
        // A dev build takes no release from an automatic writer, whether below or above it.
        for offered in ["0.6.6-rc9.13", "0.6.6-rc9.14", "0.6.7"] {
            let root = prefix("dev", Some(("0.6.6-rc9.14-dev+d94ed0fac", "c0"))).await;
            let out = arm(&root, &identity(offered), &co, "cc", Writer::Automatic).await.unwrap();
            assert_eq!(out, ArmOutcome::DevInstall, "offered {offered}");
            assert!(read(&root, T).await.unwrap().is_none(), "offered {offered}: nothing armed");
            cleanup(&root).await;
        }

        let root = prefix("above", Some(("0.6.6-rc9.13", "c0"))).await;
        let out = arm(&root, &identity("0.6.6-rc9.14"), &co, "cc", Writer::Automatic).await.unwrap();
        assert_eq!(out, ArmOutcome::Armed);
        let ptr = read(&root, T).await.unwrap().unwrap();
        assert_eq!(ptr.armed_over_version.as_deref(), Some("0.6.6-rc9.13"));
        assert_eq!(ptr.armed_over_commit.as_deref(), Some("c0"));
        let text = tokio::fs::read_to_string(pointer_path(&root, T)).await.unwrap();
        assert!(text.contains("\"armed_over_version\": \"0.6.6-rc9.13\""), "{text}");
        cleanup(&root).await;
    }

    // The run that named a version pinned the install: no release is armed over it automatically, the operator's still is.
    #[tokio::test]
    async fn a_pinned_install_takes_no_automatic_arm() {
        let co = PathBuf::from("/x/repo/versions/v0.6.6-rc9.14");
        let root = prefix("pinned", Some(("0.6.6-rc9.13", "c0"))).await;
        install(root.parent().unwrap(), "0.6.6-rc9.13", "c0", Some(true)).await;
        let out = arm(&root, &identity("0.6.6-rc9.14"), &co, "cc", Writer::Automatic).await.unwrap();
        assert_eq!(out, ArmOutcome::Pinned);
        assert!(read(&root, T).await.unwrap().is_none(), "nothing armed over a pinned install");
        let out = arm(&root, &identity("0.6.6-rc9.14"), &co, "cc", Writer::Operator).await.unwrap();
        assert_eq!(out, ArmOutcome::Armed, "the operator's arm still moves it");
        cleanup(&root).await;
    }

    // An install.json with no pin field (every install made before the field existed) is unpinned and follows its channel.
    #[tokio::test]
    async fn an_install_with_no_pin_field_follows_its_channel() {
        let co = PathBuf::from("/x/repo/versions/v0.6.6-rc9.14");
        let root = prefix("nopin", Some(("0.6.6-rc9.13", "c0"))).await;
        assert!(!InstalledBuild::read(&root).unwrap().pinned, "no field reads as unpinned");
        assert!(InstalledBuild::read(&root).unwrap().admits("0.6.6-rc9.14"));
        let out = arm(&root, &identity("0.6.6-rc9.14"), &co, "cc", Writer::Automatic).await.unwrap();
        assert_eq!(out, ArmOutcome::Armed);
        cleanup(&root).await;
        // An explicit false is unpinned too.
        let root = prefix("pinfalse", Some(("0.6.6-rc9.13", "c0"))).await;
        install(root.parent().unwrap(), "0.6.6-rc9.13", "c0", Some(false)).await;
        assert!(!InstalledBuild::read(&root).unwrap().pinned);
        cleanup(&root).await;
    }

    #[tokio::test]
    async fn the_operator_may_arm_the_same_or_an_older_build() {
        let co = PathBuf::from("/x/repo/versions/v0.6.6-rc9.13");
        let root = prefix("operator", Some(("0.6.6-rc9.14-dev+abc", "c0"))).await;
        let out = arm(&root, &identity("0.6.6-rc9.13"), &co, "cc", Writer::Operator).await.unwrap();
        assert_eq!(out, ArmOutcome::Armed);
        let ptr = read(&root, T).await.unwrap().unwrap();
        assert_eq!(ptr.armed_over_version.as_deref(), Some("0.6.6-rc9.14-dev+abc"));
        // The same stamp, another build of it.
        let out = arm(&root, &identity("0.6.6-rc9.14-dev+abd"), &co, "cd", Writer::Operator).await.unwrap();
        assert_eq!(out, ArmOutcome::Armed);
        cleanup(&root).await;
    }

    #[tokio::test]
    async fn the_operators_arm_replaces_a_newer_pointer() {
        let co = PathBuf::from("/x/repo/versions/v0.6.6-rc9.16");
        let root = prefix("operator-newer", Some(("0.6.6-rc9.13", "c0"))).await;
        let out = arm(&root, &identity("0.6.6-rc9.16"), &co, "cc", Writer::Automatic).await.unwrap();
        assert_eq!(out, ArmOutcome::Armed);
        // An automatic late finisher loses to it; the operator does not.
        let late = arm(&root, &identity("0.6.6-rc9.14"), &co, "cd", Writer::Automatic).await.unwrap();
        assert_eq!(late, ArmOutcome::KeptNewer);
        let out = arm(&root, &identity("0.6.6-rc9.14"), &co, "cd", Writer::Operator).await.unwrap();
        assert_eq!(out, ArmOutcome::Armed);
        assert_eq!(read(&root, T).await.unwrap().unwrap().identity.tag, "v0.6.6-rc9.14");
        cleanup(&root).await;
    }

    #[tokio::test]
    async fn a_pointer_that_no_longer_fits_the_install_does_not_hold_the_slot() {
        let co = PathBuf::from("/x/repo/versions/v0.6.6-rc9.15");
        let root = prefix("refit", Some(("0.6.6-rc9.13", "c0"))).await;
        let out = arm(&root, &identity("0.6.6-rc9.16"), &co, "cc", Writer::Automatic).await.unwrap();
        assert_eq!(out, ArmOutcome::Armed);
        // The install moves to another commit of rc9.14; the pointer was armed over rc9.13.
        let json = r#"{"schema":1,"prefix":"/p","version":"0.6.6-rc9.14","commit":"c1"}"#;
        tokio::fs::write(root.parent().unwrap().join(INSTALL_MANIFEST), json).await.unwrap();
        // A lower release, still above the install, replaces the stale pointer instead of losing to it.
        let out = arm(&root, &identity("0.6.6-rc9.15"), &co, "cd", Writer::Automatic).await.unwrap();
        assert_eq!(out, ArmOutcome::Armed);
        let ptr = read(&root, T).await.unwrap().unwrap();
        assert_eq!(ptr.identity.tag, "v0.6.6-rc9.15");
        assert_eq!(ptr.armed_over_commit.as_deref(), Some("c1"));
        cleanup(&root).await;
    }

    #[test]
    fn stale_reason_judges_a_pointer_by_its_record_or_by_the_version() {
        let ptr = |version: &str, over: Option<(&str, &str)>| PendingPointer {
            schema: 1,
            identity: identity(version),
            checkout: PathBuf::from("/x"),
            commit: "cc".into(),
            armed_at: 0,
            armed_over_version: over.map(|o| o.0.into()),
            armed_over_commit: over.map(|o| o.1.into()),
        };
        let at = |version: &str, commit: &str| InstalledBuild { version: version.into(), commit: Some(commit.into()), pinned: false };
        // A record: the install is still the one it was armed over.
        assert!(stale_reason(&ptr("0.6.6-rc9.14", Some(("0.6.6-rc9.13", "c0"))), &at("0.6.6-rc9.13", "c0")).is_none());
        // A same-stamp operator pointer is fine while the install is unchanged.
        assert!(stale_reason(&ptr("0.6.6-rc9.14-dev+a", Some(("0.6.6-rc9.14-dev+b", "c0"))), &at("0.6.6-rc9.14-dev+b", "c0")).is_none());
        // The install moved: another version, or another commit of the same version.
        assert!(stale_reason(&ptr("0.6.6-rc9.14", Some(("0.6.6-rc9.13", "c0"))), &at("0.6.6-rc9.14", "c0")).is_some());
        assert!(stale_reason(&ptr("0.6.6-rc9.14", Some(("0.6.6-rc9.13", "c0"))), &at("0.6.6-rc9.13", "c1")).is_some());
        // No record: only strictly above the install applies.
        assert!(stale_reason(&ptr("0.6.6-rc9.14", None), &at("0.6.6-rc9.13", "c0")).is_none());
        assert!(stale_reason(&ptr("0.6.6-rc9.13", None), &at("0.6.6-rc9.14-dev+d94", "c0")).is_some());
        assert!(stale_reason(&ptr("0.6.6-rc9.13", None), &at("0.6.6-rc9.13", "c0")).is_some());
        // Never onto a dev build, even a release above its stamp.
        let dev = at("0.6.6-rc9.14-dev+d94ed0fac", "c1");
        assert!(stale_reason(&ptr("0.6.6-rc9.14", None), &dev).unwrap().contains("dev build"));
        assert!(stale_reason(&ptr("0.6.7", None), &dev).is_some());
        // An operator's pointer carries a record, and that is what it is judged by.
        assert!(stale_reason(&ptr("0.6.6-rc9.14", Some(("0.6.6-rc9.14-dev+d94ed0fac", "c1"))), &dev).is_none());
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
