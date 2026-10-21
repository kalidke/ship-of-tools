use super::*;
use crate::rows::workspace::{Observation, SupervisorIdentity};
use crate::rows::Workspace;
use std::path::PathBuf;

/// The store's file in a fresh folder. The caller binds the guard, so the
/// folder is removed when the test ends and also when an assertion panics.
fn store_with(tag: &str, text: &str) -> (tempfile::TempDir, TopologyStore) {
    let dir = tempfile::Builder::new()
        .prefix(&format!("sot-topology-set-test-{tag}-"))
        .tempdir()
        .unwrap();
    let path = dir.path().join("hosts.toml");
    std::fs::write(&path, text).unwrap();
    (dir, TopologyStore::new(path))
}

fn mk_ws(label: &str) -> Workspace {
    Workspace::from_label(label, PathBuf::from("/p"), false, "none".into(), String::new(), String::new())
}

fn mark_running(ws: &Workspace) {
    let id = SupervisorIdentity { pid: 1, created: 1 };
    ws.begin_supervisor_epoch(id);
    assert!(ws.apply_phase_observation(Observation::Phase { phase: Phase::Ready, supervisor: id, voyage: None }));
}

const V2: &str = "hub = \"alpha\"\n\n[host.alpha]\ndaemon = true\n\n[host.beta]\ndaemon = true\n";

#[tokio::test]
async fn add_flag_remove_each_land_atomically_and_broadcast() {
    let (_dir, store) = store_with("add-flag-remove", V2);
    let workspaces = Workspaces::new();
    let (tx, mut rx) = broadcast::channel::<TopologyChanged>(4);

    let add = json!({"edit": {"kind": "add_host", "name": "gamma", "daemon": true}});
    let out = handle_topology_set(1, add, &store, &workspaces, "alpha", None, &tx).await.unwrap();
    assert_eq!(out[0].0.payload.get("ok").and_then(|v| v.as_bool()), Some(true));
    let evt = rx.try_recv().expect("topology.changed on add");
    let text_after_add = std::fs::read_to_string(store.path()).unwrap();
    assert!(text_after_add.contains("[host.gamma]"));
    assert_eq!(sot_protocol::topology::hash_text(&text_after_add), evt.hash);

    let flag = json!({"edit": {"kind": "set_flag", "name": "beta", "key": "frontend", "value": true}});
    let out = handle_topology_set(2, flag, &store, &workspaces, "alpha", None, &tx).await.unwrap();
    assert_eq!(out[0].0.payload.get("ok").and_then(|v| v.as_bool()), Some(true));
    rx.try_recv().expect("topology.changed on flag");
    assert!(std::fs::read_to_string(store.path()).unwrap().contains("[host.beta]\ndaemon = true\nfrontend = true\n"));

    let remove = json!({"edit": {"kind": "remove_host", "name": "gamma"}});
    let out = handle_topology_set(3, remove, &store, &workspaces, "alpha", None, &tx).await.unwrap();
    assert_eq!(out[0].0.payload.get("ok").and_then(|v| v.as_bool()), Some(true));
    rx.try_recv().expect("topology.changed on remove");
    assert!(!std::fs::read_to_string(store.path()).unwrap().contains("gamma"));
}

#[tokio::test]
async fn non_hub_daemon_refuses() {
    let (_dir, store) = store_with("non-hub", V2);
    let workspaces = Workspaces::new();
    let (tx, _rx) = broadcast::channel::<TopologyChanged>(4);
    let add = json!({"edit": {"kind": "add_host", "name": "gamma"}});
    let out = handle_topology_set(1, add, &store, &workspaces, "beta", None, &tx).await.unwrap();
    assert_eq!(out[0].0.payload.get("code").and_then(|v| v.as_str()), Some("not_hub"));
    assert!(out[0].0.payload.get("error").unwrap().as_str().unwrap().contains("alpha"));
}

#[tokio::test]
async fn refuses_removing_the_hub() {
    let (_dir, store) = store_with("remove-hub", V2);
    let workspaces = Workspaces::new();
    let (tx, _rx) = broadcast::channel::<TopologyChanged>(4);
    let remove = json!({"edit": {"kind": "remove_host", "name": "alpha"}});
    let out = handle_topology_set(1, remove, &store, &workspaces, "alpha", None, &tx).await.unwrap();
    assert_eq!(out[0].0.payload.get("code").and_then(|v| v.as_str()), Some("invalid"));
}

#[tokio::test]
async fn refuses_removing_the_requesters_own_declared_host() {
    let (_dir, store) = store_with("remove-self", V2);
    let workspaces = Workspaces::new();
    let (tx, _rx) = broadcast::channel::<TopologyChanged>(4);
    let remove = json!({"edit": {"kind": "remove_host", "name": "beta"}});
    let out = handle_topology_set(1, remove, &store, &workspaces, "alpha", Some("beta"), &tx).await.unwrap();
    assert_eq!(out[0].0.payload.get("code").and_then(|v| v.as_str()), Some("remove_self"));
}

#[tokio::test]
async fn refuses_clearing_daemon_on_the_hub_with_running_rows() {
    let (_dir, store) = store_with("running-rows", V2);
    let workspaces = Workspaces::new();
    let ws = workspaces.insert(mk_ws("running"));
    mark_running(&ws);
    let (tx, _rx) = broadcast::channel::<TopologyChanged>(4);
    // Clearing `daemon` on the hub itself would also fail grammar
    // validation (a v2 hub must be a daemon host) -- the running-rows
    // check must fire FIRST, with its own distinct code, not just
    // surface as a generic `invalid`.
    let flag = json!({"edit": {"kind": "set_flag", "name": "alpha", "key": "daemon", "value": false}});
    let out = handle_topology_set(1, flag, &store, &workspaces, "alpha", None, &tx).await.unwrap();
    assert_eq!(out[0].0.payload.get("code").and_then(|v| v.as_str()), Some("has_running_rows"));

    // Same guard on removal of a non-hub, non-hub-daemon host that IS
    // this daemon's own declared name (the daemon checking itself).
    let (_dir2, store2) = store_with("running-rows-2", "hub = \"gamma\"\n\n[host.gamma]\ndaemon = true\n\n[host.alpha]\ndaemon = true\n");
    let remove = json!({"edit": {"kind": "remove_host", "name": "alpha"}});
    let out = handle_topology_set(1, remove, &store2, &workspaces, "alpha", None, &tx).await.unwrap();
    assert_eq!(out[0].0.payload.get("code").and_then(|v| v.as_str()), Some("not_hub"), "alpha isn't the hub of this file, so not_hub fires before the rows check even applies");
}

#[tokio::test]
async fn hand_edit_on_disk_is_picked_up_by_the_next_op_and_broadcasts_once() {
    let (_dir, store) = store_with("hand-edit", V2);
    let workspaces = Workspaces::new();
    let (tx, mut rx) = broadcast::channel::<TopologyChanged>(4);

    // Simulate a hand edit between two unrelated ops: the daemon never
    // touched the file itself.
    std::fs::write(store.path(), "hub = \"alpha\"\n\n[host.alpha]\ndaemon = true\n\n[host.delta]\n").unwrap();
    let f = std::fs::File::open(store.path()).unwrap();
    let _ = f.set_modified(std::time::SystemTime::now() + std::time::Duration::from_secs(2));

    // The NEXT op (any `topology.*` op, exercised here as a `set`)
    // must read the hand edit off disk before applying its own edit —
    // proven by the written result carrying BOTH — and must fire
    // exactly one broadcast for the whole call, not one for the
    // noticed hand edit and a second for its own write.
    let add = json!({"edit": {"kind": "add_host", "name": "epsilon"}});
    let out = handle_topology_set(1, add, &store, &workspaces, "alpha", None, &tx).await.unwrap();
    assert_eq!(out[0].0.payload.get("ok").and_then(|v| v.as_bool()), Some(true));
    let written = std::fs::read_to_string(store.path()).unwrap();
    assert!(written.contains("[host.delta]"), "the hand edit must survive");
    assert!(written.contains("[host.epsilon]"), "this op's own edit must land too");
    rx.try_recv().expect("exactly one topology.changed for the whole call");
    assert!(rx.try_recv().is_err(), "must not double-broadcast");
}

#[tokio::test]
async fn malformed_file_on_disk_is_refused_without_losing_previous_content() {
    let (_dir, store) = store_with("malformed", V2);
    let workspaces = Workspaces::new();
    let (tx, _rx) = broadcast::channel::<TopologyChanged>(4);
    // Prime the good cache.
    store.refresh();

    std::fs::write(store.path(), "hub = \"alpha\"\n[host.alpha]\ncolour = \"red\"\n").unwrap();
    let f = std::fs::File::open(store.path()).unwrap();
    let _ = f.set_modified(std::time::SystemTime::now() + std::time::Duration::from_secs(2));

    let add = json!({"edit": {"kind": "add_host", "name": "gamma"}});
    let out = handle_topology_set(1, add, &store, &workspaces, "alpha", None, &tx).await.unwrap();
    assert_eq!(out[0].0.payload.get("code").and_then(|v| v.as_str()), Some("invalid"));
    // The file on disk is untouched -- still the malformed hand edit,
    // never overwritten by a refused request.
    assert!(std::fs::read_to_string(store.path()).unwrap().contains("colour"));
}
