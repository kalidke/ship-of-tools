//! Test-only (feature `test-support`): the one way a test copies a project folder of the release.

use std::path::Path;

/// Whether `name` is a manifest Julia reads (base/loading.jl: `JuliaManifest-vX.Y.toml`, `Manifest-vX.Y.toml`,
/// `JuliaManifest.toml`, `Manifest.toml`; the lookup is case-sensitive on every platform).
fn is_manifest(name: &str) -> bool {
    (name.starts_with("Manifest") || name.starts_with("JuliaManifest")) && name.ends_with(".toml")
}

/// Copies the folder `source` to `destination`, whole, leaving out every Julia manifest: a package operation writes
/// one, the repository ignores it and a release does not ship it. In a checkout "the whole folder" includes the
/// untracked files Julia reads (`JuliaProject.toml`, `LocalPreferences.toml`); only an export of the release equals
/// what it ships.
pub fn copy_release_folder(source: &Path, destination: &Path) {
    std::fs::create_dir_all(destination).expect("create the copy's folder");
    for entry in std::fs::read_dir(source).expect("read the source folder") {
        let entry = entry.expect("a source entry");
        let target = destination.join(entry.file_name());
        if entry.file_type().expect("a source entry's kind").is_dir() {
            copy_release_folder(&entry.path(), &target);
        } else if !is_manifest(&entry.file_name().to_string_lossy()) {
            #[allow(clippy::disallowed_methods, reason = "a data copy of a source file, no program is run from it")]
            std::fs::copy(entry.path(), target).expect("copy a source file");
        }
    }
}

#[cfg(test)]
#[path = "test_copy_tests.rs"]
mod tests;
