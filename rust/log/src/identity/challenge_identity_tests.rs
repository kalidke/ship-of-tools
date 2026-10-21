#[allow(unused_imports)]
use super::*;

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn boot_identity_is_stable_and_nonempty() {
    let a = boot_identity().unwrap();
    assert!(!a.is_empty());
    assert_eq!(a, boot_identity().unwrap());
}

#[cfg(target_os = "macos")]
#[test]
fn boot_identity_is_a_uuid_on_macos() {
    let a = boot_identity().unwrap();
    assert_eq!(a.len(), 36, "{a:?}");
    assert_eq!(a.matches('-').count(), 4, "{a:?}");
    assert_eq!(a, boot_identity().unwrap());
}

#[test]
fn boot_from_bootid_table() {
    assert_eq!(boot_from_bootid(Some(80)), "80");
    assert_eq!(boot_from_bootid(None), "");
}
