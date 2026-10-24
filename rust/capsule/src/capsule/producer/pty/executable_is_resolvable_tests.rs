use super::executable_is_resolvable;

#[test]
fn a_real_program_resolves_both_by_path_and_by_bare_name() {
    assert!(
        executable_is_resolvable("/bin/sh"),
        "/bin/sh must exist and be executable on every Linux CI image"
    );
    assert!(
        executable_is_resolvable("sh"),
        "a bare name must resolve via PATH the same way execvp would"
    );
}

#[test]
fn a_nonexistent_program_never_resolves() {
    assert!(!executable_is_resolvable("/nonexistent/no_such_exe_lu2b"));
    assert!(!executable_is_resolvable("no_such_exe_lu2b_bare_9f31"));
}

#[test]
fn a_non_executable_regular_file_does_not_resolve() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("not_executable");
    std::fs::write(&path, b"not a real program").unwrap();
    assert!(!executable_is_resolvable(path.to_str().unwrap()));
}
