//! Release identity — the immutable description of ONE release for ONE
//! platform that every updater stage (check → stage → arm → apply) must agree
//! on. Carrying the full identity (not just a version string) is what stops a
//! frontend from arming vN+2 while the backend prepared vN+1, or two machines
//! silently tracking different repos (Codex review of the Phase C plan,
//! MUST-FIX 5).
//!
//! Also home to the strict-validation helpers for the two untrusted strings
//! that get joined into filesystem paths and URLs: the release tag and the
//! `owner/repo` slug (MUST-FIX 11).
//!
//! Also the one place that decides which release repo a box watches
//! (`DEFAULT_REPO`, `repo_from_env`), so the daemon and the window agree.

use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};

use crate::semver::{parse_semver, strip_v};

/// Default release repo. Overridable via `SOT_UPDATE_REPO` for testing against
/// a fork.
pub const DEFAULT_REPO: &str = "kalidke/ship-of-tools";

pub fn repo_from_env() -> String {
    std::env::var("SOT_UPDATE_REPO")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| DEFAULT_REPO.to_string())
}

/// One release for one platform, pinned by digest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReleaseIdentity {
    /// `owner/repo` slug the release came from.
    pub repo: String,
    /// Full tag, `vX.Y.Z`.
    pub tag: String,
    /// Bare version, `X.Y.Z` (tag without the leading `v`).
    pub version: String,
    /// Release-matrix platform string, e.g. `linux-x86_64`.
    pub target: String,
    /// Platform archive filename, e.g. `sot-0.6.0-linux-x86_64.tar.gz`.
    pub asset: String,
    /// Lowercase hex sha256 of the platform archive, from `SHA256SUMS`.
    pub asset_sha256: String,
}

/// Validate a release tag for use in paths and URLs: strict `v<semver>`,
/// nothing else. Rejects path separators, `..`, and anything that isn't a
/// plain three-component (optionally pre-release) version. Returns the bare
/// version on success.
pub fn validate_tag(tag: &str) -> Result<String> {
    if !tag.starts_with('v') {
        bail!("release tag {tag:?} does not start with 'v'");
    }
    let ok_chars = tag
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '+'));
    if !ok_chars {
        bail!("release tag {tag:?} contains characters outside [A-Za-z0-9.+-]");
    }
    if parse_semver(tag).is_none() {
        bail!("release tag {tag:?} is not a v<semver> tag");
    }
    Ok(strip_v(tag).to_string())
}

/// Validate an `owner/repo` slug for use in URLs: exactly one `/`, both parts
/// non-empty, GitHub-safe characters only.
pub fn validate_repo(repo: &str) -> Result<()> {
    let Some((owner, name)) = repo.split_once('/') else {
        bail!("repo {repo:?} is not an owner/repo slug");
    };
    let part_ok = |s: &str| {
        !s.is_empty()
            && s.chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))
            && s != "."
            && s != ".."
    };
    if !part_ok(owner) || !part_ok(name) || name.contains('/') {
        bail!("repo {repo:?} contains invalid characters");
    }
    Ok(())
}

/// Validate a lowercase-hex sha256 digest string.
pub fn validate_sha256(hex: &str) -> Result<()> {
    if hex.len() != 64 || !hex.chars().all(|c| c.is_ascii_hexdigit()) {
        bail!("not a sha256 hex digest: {hex:?}");
    }
    Ok(())
}

impl ReleaseIdentity {
    /// Check internal consistency: tag ↔ version agree, all fields pass the
    /// strict validators, and the asset name embeds this exact version.
    pub fn validate(&self) -> Result<()> {
        validate_repo(&self.repo)?;
        let bare = validate_tag(&self.tag)?;
        if bare != self.version {
            bail!(
                "identity tag {} does not match version {}",
                self.tag,
                self.version
            );
        }
        validate_sha256(&self.asset_sha256.to_ascii_lowercase())?;
        match crate::platform::parse_asset_name(&self.asset) {
            Some((v, plat)) if v == self.version && plat == self.target => Ok(()),
            _ => bail!(
                "asset {} does not encode version {} for target {}",
                self.asset,
                self.version,
                self.target
            ),
        }
    }
}

#[cfg(test)]
#[path = "identity_tests.rs"]
mod tests;
