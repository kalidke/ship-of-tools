use super::simplify_verbatim;
use std::path::PathBuf;

#[test]
fn leaves_plain_path_unchanged() {
    let p = PathBuf::from("/home/u/proj");
    assert_eq!(simplify_verbatim(p.clone()), p);
}

#[test]
fn leaves_windows_style_verbatim_string_unchanged() {
    let p = PathBuf::from(r"\\?\C:\Users\k\proj");
    assert_eq!(simplify_verbatim(p.clone()), p);
}
