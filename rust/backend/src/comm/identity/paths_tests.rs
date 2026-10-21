use super::*;

#[test]
fn every_windows_spelling_of_one_folder_is_one_root() {
    for spelled in [r"C:\Users\me\repo", "C:/Users/me/repo", "/c/Users/me/repo", r"\\?\C:\Users\me\repo", "c:/users/ME/repo/"] {
        assert!(windows_same_root(spelled, "/C/Users/me/repo"), "{spelled}");
    }
    assert!(!windows_same_root("C:/Users/me/repo", "D:/Users/me/repo"));
    assert!(!windows_same_root("C:/Users/me/repo", "C:/Users/me/repo2"));
    assert_eq!(windows_native("/c/Users/me"), "C:/Users/me");
    assert_eq!(windows_native("/c"), "C:/");
    assert_eq!(windows_native(r"\\?\D:\x"), "D:/x");
    assert_eq!(windows_native("/cx/y"), "/cx/y", "only a one-letter top folder is a drive");
}

#[test]
fn off_windows_a_root_is_its_own_spelling() {
    if cfg!(windows) {
        return;
    }
    assert!(same_root("/a/b", "/a/b"));
    assert!(!same_root("/a/b", "/a/B"), "a case-sensitive file system keeps two folders apart");
    assert!(!same_root("/a/b", "/a/b/"));
    assert_eq!(native(r"/a\b"), r"/a\b");
}

#[test]
fn basename_and_dirname_read_a_root_as_the_shell_does() {
    assert_eq!(basename("/home/me/MyRepo"), "MyRepo");
    assert_eq!(basename("/home/me/MyRepo/"), "MyRepo");
    assert_eq!(basename("C:/Users/me/repo"), "repo");
    assert_eq!(dirname("/home/me/MyRepo"), "/home/me");
    assert_eq!(basename(&dirname("/home/me/MyRepo")), "me");
    assert_eq!(dirname("/top"), "/");
    assert_eq!(dirname("rel"), ".");
}

#[test]
fn a_root_that_cannot_be_resolved_is_refused_by_name() {
    let err = canonical("/no/such/folder/for/sure").unwrap_err();
    assert!(err.contains("/no/such/folder/for/sure") && err.contains("refusing"), "{err}");
    let dir = tempfile::tempdir().unwrap();
    let got = canonical(&dir.path().to_string_lossy()).unwrap();
    assert!(same_root(&got, &native(&std::fs::canonicalize(dir.path()).unwrap().to_string_lossy())));
}
