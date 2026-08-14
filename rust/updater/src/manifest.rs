//! The two manifests the updater lives by.
//!
//! **Install manifest** (`<prefix>/install.json`, written by `install.sh`):
//! what THIS machine's install is — role, prefix, config dir, service mode,
//! and the installed release identity. It's how a binary finds its own
//! install layout instead of guessing from XDG env vars (Codex review,
//! MUST-FIX 10): the updates root, the checkout, and the bin dir all hang off
//! `prefix`.
//!
//! **Ready manifest** (`<updates-root>/<tag>/manifest.json`, written LAST by
//! a stage): the staged release's full identity. "Staged" means the manifest
//! is present, parses, and matches the wanted identity — a bare marker file's
//! existence proves nothing about WHAT is in the dir.

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

use crate::identity::ReleaseIdentity;

pub const INSTALL_MANIFEST: &str = "install.json";
pub const READY_MANIFEST: &str = "manifest.json";
pub const INSTALL_SCHEMA: u32 = 1;

/// `<prefix>/install.json`. Fields the updater doesn't know yet default so a
/// newer installer can extend the schema additively.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstallManifest {
    pub schema: u32,
    /// `local` | `remote` | `be-only`.
    pub role: String,
    /// Install prefix, e.g. `~/.local/share/sot`.
    pub prefix: PathBuf,
    /// Config dir, e.g. `~/.config/sot`.
    #[serde(default)]
    pub config: Option<PathBuf>,
    /// `systemd` | `none` (launchd reserved).
    #[serde(default)]
    pub service: Option<String>,
    /// Installed product version (`X.Y.Z`) and tag (`vX.Y.Z`).
    #[serde(default)]
    pub version: Option<String>,
    #[serde(default)]
    pub tag: Option<String>,
    /// Commit the checkout was verified at during install.
    #[serde(default)]
    pub commit: Option<String>,
}

impl InstallManifest {
    /// Read from an explicit path.
    pub fn read(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("reading {}", path.display()))?;
        let m: Self = serde_json::from_str(&text)
            .with_context(|| format!("parsing {}", path.display()))?;
        if m.schema > INSTALL_SCHEMA {
            tracing::warn!(
                schema = m.schema,
                "install manifest schema is newer than this binary understands"
            );
        }
        Ok(m)
    }

    /// Locate the manifest for the running binary: `<exe>/../../install.json`
    /// (binaries live in `<prefix>/bin/`). `None` for layouts without one —
    /// dev builds from `target/`, pre-manifest installs.
    ///
    /// The exe location is the AUTHORITY on the prefix: a manifest whose
    /// recorded `prefix` disagrees (copied install, edited file) is corrected
    /// to the exe-derived prefix rather than trusted — otherwise a stray
    /// manifest could redirect staging (and later the apply flip) to an
    /// arbitrary directory.
    pub fn for_current_exe() -> Option<Self> {
        let exe = std::env::current_exe().ok()?;
        let prefix = exe.parent()?.parent()?;
        let path = prefix.join(INSTALL_MANIFEST);
        if !path.is_file() {
            return None;
        }
        match Self::read(&path) {
            Ok(mut m) => {
                if m.prefix != prefix {
                    tracing::warn!(
                        recorded = %m.prefix.display(),
                        actual = %prefix.display(),
                        "install manifest prefix disagrees with the binary's location — using the binary's"
                    );
                    m.prefix = prefix.to_path_buf();
                }
                Some(m)
            }
            Err(e) => {
                tracing::warn!(error = %e, path = %path.display(), "unreadable install manifest — ignoring");
                None
            }
        }
    }

    /// The updates root for this install: `<prefix>/updates`.
    pub fn updates_root(&self) -> PathBuf {
        self.prefix.join("updates")
    }
}

/// `<updates-root>/<tag>/manifest.json` — written last, atomically (tmp +
/// rename), so a partial stage never reads as ready.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReadyManifest {
    pub schema: u32,
    #[serde(flatten)]
    pub identity: ReleaseIdentity,
    /// The source commit the release's binaries were built from, when the
    /// release publishes a `COMMIT` file (sums-verified). Prepare refuses a
    /// tag whose commit disagrees — a moved tag must not execute code the
    /// verified binaries weren't built from.
    #[serde(default)]
    pub source_commit: Option<String>,
    /// Unix seconds when the stage completed.
    pub staged_at: u64,
}

impl ReadyManifest {
    pub fn new(identity: ReleaseIdentity, source_commit: Option<String>) -> Self {
        Self {
            schema: INSTALL_SCHEMA,
            identity,
            source_commit,
            staged_at: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0),
        }
    }

    /// Atomic write into `dir` (tmp file + rename).
    pub async fn write(&self, dir: &Path) -> Result<()> {
        self.identity.validate()?;
        let tmp = dir.join(format!("{READY_MANIFEST}.tmp"));
        let text = serde_json::to_string_pretty(self).context("serializing ready manifest")?;
        tokio::fs::write(&tmp, text)
            .await
            .with_context(|| format!("writing {}", tmp.display()))?;
        tokio::fs::rename(&tmp, dir.join(READY_MANIFEST))
            .await
            .context("renaming ready manifest into place")?;
        Ok(())
    }

    /// Read + validate from `dir`; error when absent or inconsistent.
    pub async fn read(dir: &Path) -> Result<Self> {
        let path = dir.join(READY_MANIFEST);
        let text = tokio::fs::read_to_string(&path)
            .await
            .with_context(|| format!("reading {}", path.display()))?;
        let m: Self = serde_json::from_str(&text)
            .with_context(|| format!("parsing {}", path.display()))?;
        m.identity.validate()?;
        Ok(m)
    }

    /// True when `dir` holds a completed stage of exactly `identity`.
    pub async fn matches(dir: &Path, identity: &ReleaseIdentity) -> bool {
        match Self::read(dir).await {
            Ok(m) => m.identity == *identity,
            Err(_) => false,
        }
    }
}

/// Resolve the updates root, in order:
/// 1. the install manifest next to the running binary (`<prefix>/updates`) —
///    the CANONICAL root: `sot-apply` and the systemd `ExecStartPre` owner
///    read exactly this location, so on a release install nothing (not even
///    an env override) may point producers elsewhere or armed updates would
///    never be consumed;
/// 2. `SOT_UPDATE_ROOT` env (tests, manifest-less dev layouts);
/// 3. the legacy per-OS data dir (pre-manifest installs).
///
/// When none resolves this FAILS — there is deliberately no temp-dir
/// fallback, because staging executables into a world-writable temp dir is
/// how updaters get owned.
pub fn resolve_updates_root() -> Result<PathBuf> {
    if let Some(m) = InstallManifest::for_current_exe() {
        if std::env::var_os("SOT_UPDATE_ROOT").is_some() {
            tracing::warn!(
                "SOT_UPDATE_ROOT ignored: this is a release install — the manifest root is canonical (the apply owners read it)"
            );
        }
        return Ok(m.updates_root());
    }
    if let Some(root) = std::env::var_os("SOT_UPDATE_ROOT") {
        let p = PathBuf::from(root);
        if p.as_os_str().is_empty() {
            bail!("SOT_UPDATE_ROOT is set but empty");
        }
        return Ok(p);
    }
    legacy_updates_root()
}

/// The pre-manifest layout: `~/.local/share/sot/updates` (Linux/macOS,
/// honoring `XDG_DATA_HOME`) / `%LOCALAPPDATA%\sot\updates` (Windows).
fn legacy_updates_root() -> Result<PathBuf> {
    #[cfg(windows)]
    {
        if let Some(la) = std::env::var_os("LOCALAPPDATA") {
            return Ok(PathBuf::from(la).join("sot").join("updates"));
        }
        bail!("LOCALAPPDATA unset — cannot resolve an updates directory");
    }
    #[cfg(not(windows))]
    {
        if let Some(x) = std::env::var_os("XDG_DATA_HOME") {
            return Ok(PathBuf::from(x).join("sot").join("updates"));
        }
        if let Some(h) = std::env::var_os("HOME") {
            return Ok(PathBuf::from(h)
                .join(".local")
                .join("share")
                .join("sot")
                .join("updates"));
        }
        bail!("neither XDG_DATA_HOME nor HOME set — cannot resolve an updates directory");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn identity() -> ReleaseIdentity {
        ReleaseIdentity {
            repo: "kalidke/ship-of-tools".into(),
            tag: "v0.6.0".into(),
            version: "0.6.0".into(),
            target: "linux-x86_64".into(),
            asset: "sot-0.6.0-linux-x86_64.tar.gz".into(),
            asset_sha256: "ab".repeat(32),
        }
    }

    #[tokio::test]
    async fn ready_manifest_roundtrip_and_match() {
        let dir = std::env::temp_dir().join(format!("sot-updater-manifest-{}", std::process::id()));
        let _ = tokio::fs::remove_dir_all(&dir).await;
        tokio::fs::create_dir_all(&dir).await.unwrap();

        assert!(ReadyManifest::read(&dir).await.is_err()); // absent
        let m = ReadyManifest::new(identity(), Some("abc123".into()));
        m.write(&dir).await.unwrap();
        let back = ReadyManifest::read(&dir).await.unwrap();
        assert_eq!(back.identity, identity());
        assert!(ReadyManifest::matches(&dir, &identity()).await);

        let mut other = identity();
        other.tag = "v0.7.0".into();
        other.version = "0.7.0".into();
        other.asset = "sot-0.7.0-linux-x86_64.tar.gz".into();
        assert!(!ReadyManifest::matches(&dir, &other).await);

        tokio::fs::remove_dir_all(&dir).await.unwrap();
    }

    #[test]
    fn ready_manifest_parses_without_source_commit() {
        // Pre-commit-binding stages (legacy) must keep parsing.
        let text = r#"{
            "schema": 1,
            "repo": "kalidke/ship-of-tools",
            "tag": "v0.6.0",
            "version": "0.6.0",
            "target": "linux-x86_64",
            "asset": "sot-0.6.0-linux-x86_64.tar.gz",
            "asset_sha256": "abababababababababababababababababababababababababababababababab",
            "staged_at": 5
        }"#;
        let m: ReadyManifest = serde_json::from_str(text).unwrap();
        assert!(m.source_commit.is_none());
    }

    #[test]
    fn install_manifest_parses_with_unknown_fields() {
        let text = r#"{
            "schema": 1,
            "role": "local",
            "prefix": "/home/u/.local/share/sot",
            "config": "/home/u/.config/sot",
            "service": "systemd",
            "version": "0.6.0",
            "tag": "v0.6.0",
            "commit": "abc123",
            "future_field": {"x": 1}
        }"#;
        let m: InstallManifest = serde_json::from_str(text).unwrap();
        assert_eq!(m.role, "local");
        assert_eq!(m.updates_root(), PathBuf::from("/home/u/.local/share/sot/updates"));
    }
}
