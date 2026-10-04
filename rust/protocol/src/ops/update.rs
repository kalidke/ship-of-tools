// update.check and update.apply

use super::*;

// ─── Auto-update (ADR 0030 §4, Phase C) ─────────────────────────────────

/// `update.check` request — no fields today (the backend always checks its
/// configured repo against its own embedded version). Kept as a struct so a
/// future forced-channel / forced-repo override can land additively.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct UpdateCheckReq {}

/// `update.check` response. `current` is the running product version
/// (`app_version()`); `latest` is the newest release's version with the tag's
/// leading `v` stripped (empty when the check couldn't run). `update_available`
/// is true only when `latest` is a strictly newer semver than `current`.
/// `staged` is true once the platform asset for `latest` has been downloaded,
/// sha256-verified, and unpacked into the staging dir. `status` is a
/// human/structured string: `"ok"`, `"disabled: dev build"`,
/// `"disabled: update mode off"`, or `"check unavailable: <why>"`.
/// The Phase-C identity fields (`tag` … `asset_sha256`) pin the exact release
/// the backend saw so a frontend on another machine can stage the SAME
/// release rather than re-resolving "latest" (which may have moved). All
/// default so older peers interoperate.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct UpdateCheckRes {
    pub current: String,
    pub latest: String,
    pub update_available: bool,
    pub staged: bool,
    pub status: String,
    /// Full release tag (`vX.Y.Z`) of `latest`; empty when no check ran.
    #[serde(default)]
    pub tag: String,
    /// `owner/repo` the backend checks against.
    #[serde(default)]
    pub repo: String,
    /// The BACKEND's release-matrix platform (its own asset's target).
    #[serde(default)]
    pub target: String,
    /// sha256 of the backend-platform asset, from the release's SHA256SUMS.
    #[serde(default)]
    pub asset_sha256: String,
    /// True once the versioned checkout + Julia envs for `latest` are
    /// prepared on the backend host (Phase C2 transactional prepare).
    #[serde(default)]
    pub prepared: bool,
    /// True once the pending pointer arms `latest` for apply at next launch.
    #[serde(default)]
    pub armed: bool,
}

/// `update.apply` request — no fields (applies whatever is armed).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct UpdateApplyReq {}

/// `update.apply` response, sent BEFORE the daemon exits. `ok` false means
/// nothing was armed (or the arm failed validation) and the daemon stays up;
/// `status` says why. On `ok` true, `tag` is the release being applied and
/// `will_restart` tells the frontend whether the backend comes back on its
/// own (systemd) or needs the user's next launch.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct UpdateApplyRes {
    pub ok: bool,
    pub tag: String,
    pub will_restart: bool,
    pub status: String,
}
