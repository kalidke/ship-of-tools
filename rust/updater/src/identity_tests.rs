use super::*;

fn identity() -> ReleaseIdentity {
    ReleaseIdentity {
        repo: "kalidke/ship-of-tools".into(),
        tag: "v0.6.0".into(),
        version: "0.6.0".into(),
        target: "linux-x86_64".into(),
        asset: "sot-0.6.0-linux-x86_64.tar.gz".into(),
        asset_sha256: "a".repeat(64),
    }
}

#[test]
fn valid_identity_passes() {
    identity().validate().unwrap();
}

#[test]
fn tag_validation_rejects_path_material() {
    assert!(validate_tag("v0.6.0").is_ok());
    assert!(validate_tag("v1.0.0-rc.1").is_ok());
    assert!(validate_tag("0.6.0").is_err()); // no leading v
    assert!(validate_tag("v../evil").is_err());
    assert!(validate_tag("v0.6.0/../..").is_err());
    assert!(validate_tag("v0.6").is_err()); // not semver
    assert!(validate_tag("latest").is_err());
    assert!(validate_tag("v0.6.0\u{0}").is_err());
}

#[test]
fn repo_validation_rejects_url_material() {
    assert!(validate_repo("kalidke/ship-of-tools").is_ok());
    assert!(validate_repo("a_b/c.d-e").is_ok());
    assert!(validate_repo("noslash").is_err());
    assert!(validate_repo("a/b/c").is_err());
    assert!(validate_repo("/b").is_err());
    assert!(validate_repo("a/").is_err());
    assert!(validate_repo("a/../b").is_err());
    assert!(validate_repo("a/b?x=1").is_err());
}

#[test]
fn identity_cross_checks() {
    let mut id = identity();
    id.version = "0.6.1".into();
    assert!(id.validate().is_err()); // tag/version mismatch
    let mut id = identity();
    id.asset = "sot-0.6.1-linux-x86_64.tar.gz".into();
    assert!(id.validate().is_err()); // asset encodes another version
    let mut id = identity();
    id.target = "windows-x86_64".into();
    assert!(id.validate().is_err()); // asset is for another target
    let mut id = identity();
    id.asset_sha256 = "xyz".into();
    assert!(id.validate().is_err());
}
