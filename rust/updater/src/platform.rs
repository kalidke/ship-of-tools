//! Release-matrix platform naming (ADR 0030 §3): which target triples ship,
//! and the deterministic asset filename for a version + triple.

#[cfg(target_os = "linux")]
pub const TARGET_OS: &str = "linux";
#[cfg(target_os = "macos")]
pub const TARGET_OS: &str = "macos";
#[cfg(target_os = "windows")]
pub const TARGET_OS: &str = "windows";
#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
pub const TARGET_OS: &str = "unknown";

#[cfg(target_arch = "x86_64")]
pub const TARGET_ARCH: &str = "x86_64";
#[cfg(target_arch = "aarch64")]
pub const TARGET_ARCH: &str = "aarch64";
#[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
pub const TARGET_ARCH: &str = "unknown";

/// The release-matrix platform string for an (os, arch) pair, or `None` for a
/// triple the matrix doesn't ship (linux-x86_64 + windows-x86_64 blocking,
/// macos-aarch64 experimental).
pub fn platform_for(os: &str, arch: &str) -> Option<(&'static str, &'static str)> {
    match (os, arch) {
        ("linux", "x86_64") => Some(("linux-x86_64", "tar.gz")),
        ("windows", "x86_64") => Some(("windows-x86_64", "zip")),
        ("macos", "aarch64") => Some(("macos-aarch64", "tar.gz")),
        _ => None,
    }
}

/// This build's release-matrix platform string (`linux-x86_64`, …), or `None`
/// when the running triple isn't in the matrix.
pub fn this_platform() -> Option<&'static str> {
    platform_for(TARGET_OS, TARGET_ARCH).map(|(p, _)| p)
}

/// Release-asset filename for a given version + target triple, or `None` for a
/// platform the release matrix doesn't ship. `version` is the bare `X.Y.Z`
/// (no leading `v`). Examples:
///   ("0.2.0","linux","x86_64")   → "sot-0.2.0-linux-x86_64.tar.gz"
///   ("0.2.0","windows","x86_64") → "sot-0.2.0-windows-x86_64.zip"
///   ("0.2.0","macos","aarch64")  → "sot-0.2.0-macos-aarch64.tar.gz"
pub fn asset_name_for(version: &str, os: &str, arch: &str) -> Option<String> {
    let (plat, ext) = platform_for(os, arch)?;
    Some(format!("sot-{version}-{plat}.{ext}"))
}

/// Asset filename for the running platform, or `None` if this build's triple
/// isn't in the release matrix.
pub fn platform_asset(version: &str) -> Option<String> {
    asset_name_for(version, TARGET_OS, TARGET_ARCH)
}

/// Parse a release-asset filename back into `(version, platform)` — the
/// inverse of [`asset_name_for`], used to discover the released version from a
/// `SHA256SUMS` listing. `None` when the name isn't a `sot-` archive of a
/// known platform.
pub fn parse_asset_name(name: &str) -> Option<(String, &'static str)> {
    let rest = name.strip_prefix("sot-")?;
    for (plat, ext) in [
        ("linux-x86_64", "tar.gz"),
        ("windows-x86_64", "zip"),
        ("macos-aarch64", "tar.gz"),
    ] {
        if let Some(version) = rest.strip_suffix(&format!("-{plat}.{ext}")) {
            if !version.is_empty() {
                return Some((version.to_string(), plat));
            }
        }
    }
    None
}

#[cfg(test)]
#[path = "platform_tests.rs"]
mod tests;
