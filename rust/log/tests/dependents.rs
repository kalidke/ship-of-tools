//! A guard on the dependency graph, read through cargo: no crate but the backend, the window and topology depends on the
//! capsule, and the capsule depends on no crate of ours but platform and wire. Which modules inside `sotd` and `sot` name
//! the capsule is convention until rows and the window's agent pane are crates of their own; no source scan holds it.

use std::path::Path;
use std::process::Command;
use std::time::Duration;

/// The workspace's packages, read through cargo from `rust/`.
fn workspace_packages() -> Vec<serde_json::Value> {
    let rust = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    let ran = sot_platform::test_wait::run_within(
        Command::new(env!("CARGO")).args(["metadata", "--no-deps", "--format-version", "1", "--offline"]).current_dir(&rust),
        Duration::from_secs(60),
    )
    .ok();
    let meta: serde_json::Value = serde_json::from_str(&ran.stdout).expect("cargo metadata is JSON");
    meta["packages"].as_array().expect("packages").clone()
}

/// The crate is `sot-capsule`, in `rust/capsule/`, and no package is still named for the log it no longer keeps.
#[test]
fn the_capsule_is_sot_capsule_in_rust_capsule() {
    let packages = workspace_packages();
    let manifest = |p: &serde_json::Value| p["manifest_path"].as_str().unwrap_or("").replace('\\', "/");
    let capsule = packages.iter().find(|p| p["name"] == "sot-capsule").expect("a package named sot-capsule");
    assert!(manifest(capsule).ends_with("rust/capsule/Cargo.toml"), "sot-capsule lives in rust/capsule/: {}", manifest(capsule));
    let old = concat!("sot-", "log");
    assert!(packages.iter().all(|p| p["name"] != old), "a package is still named {old}");
}

#[test]
fn only_the_backend_the_window_and_topology_depend_on_the_capsule() {
    let packages = workspace_packages();
    let capsule = packages
        .iter()
        .find(|p| p["manifest_path"].as_str().unwrap_or("").replace('\\', "/").ends_with("rust/log/Cargo.toml"))
        .expect("the capsule's package");
    let capsule_name = capsule["name"].as_str().expect("its name");
    let normal = |dep: &serde_json::Value| dep["kind"].is_null();
    let mut dependents: Vec<&str> = packages
        .iter()
        .filter(|p| p["dependencies"].as_array().unwrap().iter().any(|d| d["name"] == capsule_name && normal(d)))
        .map(|p| p["name"].as_str().unwrap())
        .collect();
    dependents.sort();
    assert_eq!(dependents, ["sot-backend", "sot-frontend", "sot-topology"], "who depends on the capsule");
    let mut dependencies: Vec<&str> = capsule["dependencies"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|d| normal(d) && d["path"].is_string())
        .map(|d| d["name"].as_str().unwrap())
        .collect();
    dependencies.sort();
    dependencies.dedup();
    assert_eq!(dependencies, ["sot-platform", "sot-protocol"], "what the capsule depends on");
}
