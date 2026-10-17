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
            std::fs::copy(entry.path(), target).expect("copy a source file");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_four_manifest_names_stay_out_and_the_rest_comes_along() {
        let src = tempfile::tempdir().unwrap();
        let dst = tempfile::tempdir().unwrap();
        std::fs::create_dir(src.path().join("ext")).unwrap();
        for name in ["Manifest.toml", "Manifest-v1.13.toml", "JuliaManifest.toml", "JuliaManifest-v1.13.toml"] {
            std::fs::write(src.path().join(name), "x").unwrap();
            std::fs::write(src.path().join("ext").join(name), "x").unwrap();
        }
        for name in ["Project.toml", "ext/A.jl"] {
            std::fs::write(src.path().join(name), "x").unwrap();
        }
        copy_release_folder(src.path(), dst.path());
        let mut left: Vec<String> = walk(dst.path());
        left.sort();
        assert_eq!(left, ["Project.toml", "ext/A.jl"]);
    }

    fn walk(root: &Path) -> Vec<String> {
        let mut out = Vec::new();
        let mut stack = vec![root.to_path_buf()];
        while let Some(dir) = stack.pop() {
            for e in std::fs::read_dir(dir).unwrap().flatten() {
                if e.file_type().unwrap().is_dir() {
                    stack.push(e.path());
                } else {
                    out.push(e.path().strip_prefix(root).unwrap().to_string_lossy().replace('\\', "/"));
                }
            }
        }
        out
    }
}
