// topology_set.rs — the `topology.set` op handler (plan §B "Editing the
// master list"). Authorisation is the dial itself: whoever can reach this
// daemon's socket may send this op — the 0700 socket and ssh identity are
// the only gate, no second credential asked here.
//
// Only the daemon that is, in its OWN currently-loaded file, the declared
// `hub` applies an edit. Every other daemon refuses (`not_hub`) naming the
// hub — its own file is a `topology sync` CACHE, never writable. The hub
// re-reads its file on every call (`TopologyStore::refresh`, on-demand,
// never a watcher), applies the edit, re-validates the result through the
// ONE grammar (`sot_protocol::topology::parse`), writes tmp+rename, and
// broadcasts `topology.changed` through the SAME re-read path a hand edit
// on disk would take — one code path either way.

use anyhow::Result;
use serde_json::json;
use sot_protocol::topology::TopologyEdit;
use sot_protocol::{op, Frame, TopologySetRes};
use tokio::sync::broadcast;

use crate::handlers::HandlerOutput;
use crate::topology_store::{TopologyChanged, TopologyStore};
use crate::workspaces::{Phase, Workspaces};

/// Any registered row whose supervisor phase means "actually running" —
/// the guard behind refusing to remove/un-daemon a host out from under
/// work it's currently hosting. `Starting`/`Ready` only: `Stopped`,
/// `Ending`, `EndedNoRespawn`, `Terminal`, `Unreachable`, and `Foreign`
/// are every shade of "not actually running right now".
pub(crate) fn has_running_rows(workspaces: &Workspaces) -> bool {
    workspaces.list().iter().any(|w| matches!(w.phase(), Phase::Starting | Phase::Ready))
}

fn refused(req_id: u64, error: impl Into<String>, code: &str) -> HandlerOutput {
    vec![(
        Frame::res(req_id, op::TOPOLOGY_SET, json!({ "error": error.into(), "code": code })),
        None,
    )]
}

/// `me` is this daemon's own declared host (`crate::workspaces::
/// declared_host()` at the call site — threaded in rather than read here
/// so the handler stays a pure function of its arguments, independent of
/// the process-global `OnceLock`). `hello_host` is the REQUESTING
/// connection's own declared host (`HelloReq.host`, captured at that
/// connection's hello) — `None` for a peer that predates the field, which
/// simply never trips the "can't remove yourself" refusal.
pub async fn handle_topology_set(
    req_id: u64,
    payload_json: serde_json::Value,
    store: &TopologyStore,
    workspaces: &Workspaces,
    me: &str,
    hello_host: Option<&str>,
    topo_tx: &broadcast::Sender<TopologyChanged>,
) -> Result<HandlerOutput> {
    let req: sot_protocol::TopologySetReq = match serde_json::from_value(payload_json) {
        Ok(r) => r,
        Err(e) => return Ok(refused(req_id, format!("topology.set payload: {e}"), "bad_request")),
    };

    let before = store.refresh();
    if let Some(err) = before.error {
        return Ok(refused(
            req_id,
            format!("the on-disk topology is currently invalid; fix it by hand first: {err}"),
            "invalid",
        ));
    }
    let Some(topo) = before.topo else {
        return Ok(refused(req_id, format!("no hosts.toml at {}", store.path().display()), "no_topology"));
    };

    if me != topo.hub {
        return Ok(refused(
            req_id,
            format!("this daemon (`{me}`) is not the hub (`{}`); topology.set only applies on the hub", topo.hub),
            "not_hub",
        ));
    }

    if let TopologyEdit::RemoveHost { name } = &req.edit {
        if hello_host.is_some_and(|h| h == name) {
            return Ok(refused(
                req_id,
                format!("`{name}` is the host this connection declared as its own; it cannot remove itself"),
                "remove_self",
            ));
        }
    }

    // "removing or clearing `daemon` on a host with running rows" — the
    // hub checks its OWN rows only (plan §B: it cannot see another
    // daemon's rows; a non-hub box's `sotd topology set` CLI does the
    // equivalent check against ITS OWN local daemon before ever dialing
    // the hub — see `topology_cli::run`).
    let clears_own_daemon = match &req.edit {
        TopologyEdit::RemoveHost { name } => name == me,
        TopologyEdit::SetFlag { name, key, value } => name == me && key == "daemon" && !*value,
        _ => false,
    };
    if clears_own_daemon && has_running_rows(workspaces) {
        return Ok(refused(
            req_id,
            format!("`{me}` has running capsule rows; stop them before removing or un-daemoning this host"),
            "has_running_rows",
        ));
    }

    let candidate = match sot_protocol::topology::apply(&topo, &req.edit) {
        Ok(t) => t,
        Err(e) => return Ok(refused(req_id, e, "invalid")),
    };
    let text = sot_protocol::topology::serialize(&candidate);
    // Re-validate through the ONE grammar (plan §B): this is what catches
    // things `apply` has no authority to special-case, e.g. clearing
    // `daemon` on the hub.
    if let Err(e) = sot_protocol::topology::parse(&text) {
        return Ok(refused(req_id, e, "invalid"));
    }

    if let Err(e) = crate::topology_store::write_atomic(store.path(), &text) {
        return Ok(refused(req_id, e, "io"));
    }

    // Same re-read path a hand edit takes: this is what fires
    // `topology.changed` exactly once, from the one place that ever fires
    // it.
    let after = store.refresh();
    let hash = after.hash.unwrap_or_default();
    if after.changed {
        let _ = topo_tx.send(TopologyChanged { hash: hash.clone() });
    }
    Ok(vec![(
        Frame::res(req_id, op::TOPOLOGY_SET, serde_json::to_value(TopologySetRes { ok: true, hash })?),
        None,
    )])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workspaces::{Observation, SupervisorIdentity, Workspace};
    use std::path::PathBuf;

    fn tempdir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "sot-topology-set-test-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn store_with(tag: &str, text: &str) -> TopologyStore {
        let dir = tempdir(tag);
        let path = dir.join("hosts.toml");
        std::fs::write(&path, text).unwrap();
        TopologyStore::new(path)
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
        let store = store_with("add-flag-remove", V2);
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
        let store = store_with("non-hub", V2);
        let workspaces = Workspaces::new();
        let (tx, _rx) = broadcast::channel::<TopologyChanged>(4);
        let add = json!({"edit": {"kind": "add_host", "name": "gamma"}});
        let out = handle_topology_set(1, add, &store, &workspaces, "beta", None, &tx).await.unwrap();
        assert_eq!(out[0].0.payload.get("code").and_then(|v| v.as_str()), Some("not_hub"));
        assert!(out[0].0.payload.get("error").unwrap().as_str().unwrap().contains("alpha"));
    }

    #[tokio::test]
    async fn refuses_removing_the_hub() {
        let store = store_with("remove-hub", V2);
        let workspaces = Workspaces::new();
        let (tx, _rx) = broadcast::channel::<TopologyChanged>(4);
        let remove = json!({"edit": {"kind": "remove_host", "name": "alpha"}});
        let out = handle_topology_set(1, remove, &store, &workspaces, "alpha", None, &tx).await.unwrap();
        assert_eq!(out[0].0.payload.get("code").and_then(|v| v.as_str()), Some("invalid"));
    }

    #[tokio::test]
    async fn refuses_removing_the_requesters_own_declared_host() {
        let store = store_with("remove-self", V2);
        let workspaces = Workspaces::new();
        let (tx, _rx) = broadcast::channel::<TopologyChanged>(4);
        let remove = json!({"edit": {"kind": "remove_host", "name": "beta"}});
        let out = handle_topology_set(1, remove, &store, &workspaces, "alpha", Some("beta"), &tx).await.unwrap();
        assert_eq!(out[0].0.payload.get("code").and_then(|v| v.as_str()), Some("remove_self"));
    }

    #[tokio::test]
    async fn refuses_clearing_daemon_on_the_hub_with_running_rows() {
        let store = store_with("running-rows", V2);
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
        let store2 = store_with("running-rows-2", "hub = \"gamma\"\n\n[host.gamma]\ndaemon = true\n\n[host.alpha]\ndaemon = true\n");
        let remove = json!({"edit": {"kind": "remove_host", "name": "alpha"}});
        let out = handle_topology_set(1, remove, &store2, &workspaces, "alpha", None, &tx).await.unwrap();
        assert_eq!(out[0].0.payload.get("code").and_then(|v| v.as_str()), Some("not_hub"), "alpha isn't the hub of this file, so not_hub fires before the rows check even applies");
    }

    #[tokio::test]
    async fn hand_edit_on_disk_is_picked_up_by_the_next_op_and_broadcasts_once() {
        let store = store_with("hand-edit", V2);
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
        let store = store_with("malformed", V2);
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
}
