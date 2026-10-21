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
