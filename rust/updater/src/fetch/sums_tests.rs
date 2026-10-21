use super::*;

const SUMS: &str = "\
0101010101010101010101010101010101010101010101010101010101010101  sot-0.6.0-linux-x86_64.tar.gz
0202020202020202020202020202020202020202020202020202020202020202  sot-0.6.0-windows-x86_64.zip
0303030303030303030303030303030303030303030303030303030303030303 *sot-0.6.0-macos-aarch64.tar.gz
";

#[test]
fn parse_and_lookup() {
    let entries = parse_sums(SUMS).unwrap();
    assert_eq!(entries.len(), 3);
    assert_eq!(
        lookup(&entries, "sot-0.6.0-windows-x86_64.zip").unwrap(),
        "02".repeat(32)
    );
    assert!(lookup(&entries, "sot-0.6.0-linux-aarch64.tar.gz").is_err());
}

#[test]
fn discover_pins_platform_identity() {
    let entries = parse_sums(SUMS).unwrap();
    let id = discover(&entries, "kalidke/ship-of-tools", "linux-x86_64").unwrap();
    assert_eq!(id.tag, "v0.6.0");
    assert_eq!(id.asset, "sot-0.6.0-linux-x86_64.tar.gz");
    assert_eq!(id.asset_sha256, "01".repeat(32));
    id.validate().unwrap();
}

#[test]
fn discover_rejects_mixed_versions() {
    let mixed = format!(
        "{SUMS}0404040404040404040404040404040404040404040404040404040404040404  sot-0.7.0-linux-x86_64.tar.gz\n"
    );
    let entries = parse_sums(&mixed).unwrap();
    assert!(discover(&entries, "kalidke/ship-of-tools", "linux-x86_64").is_err());
}

#[test]
fn discover_reports_missing_platform() {
    let linux_only = "\
0101010101010101010101010101010101010101010101010101010101010101  sot-0.6.0-linux-x86_64.tar.gz
";
    let entries = parse_sums(linux_only).unwrap();
    let err = discover(&entries, "kalidke/ship-of-tools", "macos-aarch64").unwrap_err();
    assert!(err.to_string().contains("no asset for platform"));
}

#[test]
fn parse_rejects_bad_digest() {
    assert!(parse_sums("zz  sot-0.6.0-linux-x86_64.tar.gz\n").is_err());
}

#[test]
fn parse_rejects_empty() {
    assert!(parse_sums("\n# nothing\n").is_err());
}
