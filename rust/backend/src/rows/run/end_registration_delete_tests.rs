use super::*;

/// A pin, not a power loss (no test can cut the power): a delete whose
/// directory cannot be synced is not a removed registration.
#[test]
fn registration_delete_syncs_its_directory() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let toml = dir.path().join("row.toml");
    let mode = |m: u32| std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(m)).unwrap();
    std::fs::write(&toml, "x").unwrap();
    // Write and search but no read: the unlink succeeds, and only the
    // directory's open for its sync fails.
    mode(0o300);
    let removed = remove_registration(&[toml.clone(), dir.path().join("absent.toml")]);
    // The shutdown's retry finds the file gone; with its directory still
    // unsynced, that is not a removal either.
    let retried = remove_registration(&[toml.clone()]);
    mode(0o700);
    assert!(!toml.exists(), "the unlink itself failed");
    assert!(!removed, "a registration delete whose directory was not synced counted as removed");
    assert!(!retried, "a retry that found the file gone counted it removed while its directory was still unsynced");
    assert!(remove_registration(&[toml]), "a missing file is success");
}
