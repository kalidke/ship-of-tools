// support_tests.rs — fixtures both account test modules share.

use std::path::Path;

pub(super) fn touch_dir(path: &Path) {
    std::fs::create_dir_all(path).unwrap();
}
pub(super) fn touch_file(path: &Path) {
    std::fs::write(path, b"").unwrap();
}
