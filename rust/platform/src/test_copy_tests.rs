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
