use super::*;

/// A unique throwaway dir under the OS temp dir. Unique by construction,
/// not by clock: two tests sharing a `tag` would otherwise share a
/// directory whenever their timestamps landed in one tick, which is the
/// flake that reddened a macOS leg from `fetch::tempdir`.
fn temp_dir(tag: &str) -> PathBuf {
    let d = std::env::temp_dir()
        .join(format!("sot_dl_test_{tag}_{}", sot_updater::unique::suffix()));
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn touch(path: &Path) {
    std::fs::write(path, b"").unwrap();
}

#[test]
fn split_filename_handles_ext_dotfile_and_none() {
    assert_eq!(split_filename("circles.png"), ("circles", Some("png")));
    assert_eq!(split_filename("archive.tar.gz"), ("archive.tar", Some("gz")));
    assert_eq!(split_filename(".bashrc"), (".bashrc", None));
    assert_eq!(split_filename("Makefile"), ("Makefile", None));
}

#[test]
fn free_name_returned_unchanged() {
    let dir = temp_dir("free");
    let got = non_clobbering_path(&dir, "circles.png");
    assert_eq!(got, dir.join("circles.png"));
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn collision_inserts_suffix_before_extension() {
    let dir = temp_dir("collide");
    touch(&dir.join("circles.png"));
    let got = non_clobbering_path(&dir, "circles.png");
    assert_eq!(got, dir.join("circles (1).png"));
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn multiple_collisions_increment() {
    let dir = temp_dir("incr");
    touch(&dir.join("circles.png"));
    touch(&dir.join("circles (1).png"));
    let got = non_clobbering_path(&dir, "circles.png");
    assert_eq!(got, dir.join("circles (2).png"));
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn dotfile_and_extensionless_suffix_appends_whole_name() {
    let dir = temp_dir("dotfile");
    touch(&dir.join(".bashrc"));
    touch(&dir.join("Makefile"));
    assert_eq!(non_clobbering_path(&dir, ".bashrc"), dir.join(".bashrc (1)"));
    assert_eq!(non_clobbering_path(&dir, "Makefile"), dir.join("Makefile (1)"));
    std::fs::remove_dir_all(&dir).ok();
}
