use super::{dedup_upload_name, is_safe_upload_name};

#[test]
fn upload_name_safety_rejects_traversal() {
    // Accept plain basenames (incl. spaces + the de-dup suffix shape).
    assert!(is_safe_upload_name("data.csv"));
    assert!(is_safe_upload_name("my report (1).txt"));
    // Reject anything that could escape the target dir.
    assert!(!is_safe_upload_name(""));
    assert!(!is_safe_upload_name("   "));
    assert!(!is_safe_upload_name("../etc/passwd"));
    assert!(!is_safe_upload_name("a/b.txt"));
    assert!(!is_safe_upload_name("a\\b.txt"));
    assert!(!is_safe_upload_name("."));
    assert!(!is_safe_upload_name(".."));
}

#[test]
fn dedup_suffixes_on_collision() {
    let dir = std::env::temp_dir().join(format!("sot-ul-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();

    // Free name → unchanged.
    assert_eq!(dedup_upload_name(&dir, "x.txt"), "x.txt");
    // Collisions insert ` (n)` before the extension.
    std::fs::write(dir.join("x.txt"), b"").unwrap();
    assert_eq!(dedup_upload_name(&dir, "x.txt"), "x (1).txt");
    std::fs::write(dir.join("x (1).txt"), b"").unwrap();
    assert_eq!(dedup_upload_name(&dir, "x.txt"), "x (2).txt");
    // Extensionless names get the suffix at the end.
    assert_eq!(dedup_upload_name(&dir, "data"), "data");
    std::fs::write(dir.join("data"), b"").unwrap();
    assert_eq!(dedup_upload_name(&dir, "data"), "data (1)");

    std::fs::remove_dir_all(&dir).ok();
}
