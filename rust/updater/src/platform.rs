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
mod tests {
    use super::*;

    #[test]
    fn asset_name_derivation_per_triple() {
        assert_eq!(
            asset_name_for("0.2.0", "linux", "x86_64").as_deref(),
            Some("sot-0.2.0-linux-x86_64.tar.gz")
        );
        assert_eq!(
            asset_name_for("0.2.0", "windows", "x86_64").as_deref(),
            Some("sot-0.2.0-windows-x86_64.zip")
        );
        assert_eq!(
            asset_name_for("0.2.0", "macos", "aarch64").as_deref(),
            Some("sot-0.2.0-macos-aarch64.tar.gz")
        );
        // Off-matrix triples yield no asset (ADR 0030 §3).
        assert_eq!(asset_name_for("0.2.0", "macos", "x86_64"), None);
        assert_eq!(asset_name_for("0.2.0", "linux", "aarch64"), None);
        assert_eq!(asset_name_for("0.2.0", "freebsd", "x86_64"), None);
        // Version string is interpolated verbatim (pre-release tags included).
        assert_eq!(
            asset_name_for("0.3.0-rc.1", "linux", "x86_64").as_deref(),
            Some("sot-0.3.0-rc.1-linux-x86_64.tar.gz")
        );
    }

    #[test]
    fn platform_asset_matches_this_build() {
        // Whatever this test binary's triple is, platform_asset must agree with
        // the direct derivation (or both be None on an off-matrix triple).
        assert_eq!(
            platform_asset("0.2.0"),
            asset_name_for("0.2.0", TARGET_OS, TARGET_ARCH)
        );
    }

    #[test]
    fn asset_name_roundtrip() {
        for (ver, os, arch) in [
            ("0.5.6", "linux", "x86_64"),
            ("0.5.6", "windows", "x86_64"),
            ("1.0.0-rc.1", "macos", "aarch64"),
        ] {
            let name = asset_name_for(ver, os, arch).unwrap();
            let (parsed_ver, plat) = parse_asset_name(&name).unwrap();
            assert_eq!(parsed_ver, ver);
            assert_eq!(Some(plat), platform_for(os, arch).map(|(p, _)| p));
        }
        // Non-archive release files parse as None.
        assert_eq!(parse_asset_name("SHA256SUMS"), None);
        assert_eq!(parse_asset_name("sot--linux-x86_64.tar.gz"), None);
        assert_eq!(parse_asset_name("other-0.2.0-linux-x86_64.tar.gz"), None);
    }
}
