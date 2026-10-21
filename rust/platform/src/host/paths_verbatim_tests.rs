use super::simplify_verbatim;
use std::path::PathBuf;

#[test]
fn strips_drive_verbatim() {
    assert_eq!(
        simplify_verbatim(PathBuf::from(r"\\?\C:\Users\k\proj")),
        PathBuf::from(r"C:\Users\k\proj")
    );
}

#[test]
fn strips_unc_verbatim() {
    assert_eq!(
        simplify_verbatim(PathBuf::from(r"\\?\UNC\srv\share\x")),
        PathBuf::from(r"\\srv\share\x")
    );
}

#[test]
fn strips_single_backslash_drive_verbatim() {
    // The shape `toml_unquote` (`rows/store/codec.rs`) produces for a raw,
    // never-escaped legacy write of `\\?\C:\...`: its leading `\\`
    // reads as one escaped backslash, halving the prefix to `\?\`.
    assert_eq!(
        simplify_verbatim(PathBuf::from(r"\?\C:\Users\k\proj")),
        PathBuf::from(r"C:\Users\k\proj")
    );
}

#[test]
fn leaves_plain_and_odd_verbatim_alone() {
    assert_eq!(
        simplify_verbatim(PathBuf::from(r"C:\plain")),
        PathBuf::from(r"C:\plain")
    );
    // A verbatim path whose remainder isn't a drive path stays verbatim
    // rather than being mangled.
    assert_eq!(
        simplify_verbatim(PathBuf::from(r"\\?\Volume{guid}\x")),
        PathBuf::from(r"\\?\Volume{guid}\x")
    );
}
