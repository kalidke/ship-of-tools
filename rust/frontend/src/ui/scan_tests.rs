//! The window crate's own source, for the tests that scan it (00-common item 18).

use std::path::{Path, PathBuf};

fn rs_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).expect("read source folder") {
        let path = entry.expect("read source entry").path();
        if path.is_dir() {
            rs_files(&path, out);
        } else if path.to_string_lossy().ends_with(".rs") {
            out.push(path);
        }
    }
}

pub(super) fn crate_source() -> String {
    let mut files = Vec::new();
    rs_files(Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/src")), &mut files);
    assert!(!files.is_empty(), "no .rs file found under src");
    files.sort();
    files
        .iter()
        .map(|f| std::fs::read_to_string(f).expect("read source file"))
        .collect()
}
