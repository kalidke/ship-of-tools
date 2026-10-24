//! Who names the capsule: the platform's crates below it and the daemon's services beside it do not. The capsule's
//! dependencies are platform and wire alone; only rows, topology's lane client and the window's agent pane name it.

use std::path::Path;
use std::process::Command;
use std::time::Duration;

/// Its crate's name as source writes it before `::`, in both spellings, built so no rename rewrites this file.
fn crate_names() -> [String; 2] {
    [concat!("sot", "_log").to_string(), concat!("sot", "_capsule").to_string()]
}

#[test]
fn no_crate_but_rows_topology_and_the_window_names_the_capsule() {
    let rust = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    // (a) the dependency graph.
    let ran = sot_platform::test_wait::run_within(
        Command::new(env!("CARGO")).args(["metadata", "--no-deps", "--format-version", "1", "--offline"]).current_dir(&rust),
        Duration::from_secs(60),
    )
    .ok();
    let meta: serde_json::Value = serde_json::from_str(&ran.stdout).expect("cargo metadata is JSON");
    let packages = meta["packages"].as_array().expect("packages");
    let capsule = packages
        .iter()
        .find(|p| {
            let manifest = p["manifest_path"].as_str().unwrap_or("").replace('\\', "/");
            manifest.ends_with("rust/log/Cargo.toml") || manifest.ends_with("rust/capsule/Cargo.toml")
        })
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

    // (b) the sources.
    let allowed = |path: &str| {
        path.starts_with("rust/log/")
            || path.starts_with("rust/capsule/")
            || path.starts_with("rust/backend/src/rows/")
            || path.starts_with("rust/frontend/src/ui/agent_pane/")
            || path == "rust/topology/src/lane_client.rs"
    };
    let names = crate_names();
    let mut breaches = Vec::new();
    for (path, text) in sot_platform::test_scan::production_sources() {
        if allowed(&path) {
            continue;
        }
        for (n, line) in text.lines().enumerate() {
            if names.iter().any(|name| line.contains(&format!("{name}::"))) {
                breaches.push(format!("{path}:{}: {}", n + 1, line.trim()));
            }
        }
    }
    assert!(breaches.is_empty(), "production code outside rows, topology's lane client and the agent pane names the capsule:\n{}", breaches.join("\n"));
}
