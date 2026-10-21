// topology/set.rs — the `topology.set` op handler (plan §B "Editing the
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

use crate::server::reply::HandlerOutput;
use crate::topology::store::{TopologyChanged, TopologyStore};
use crate::rows::workspace::Phase;
use crate::rows::Workspaces;

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

/// `me` is this daemon's own declared host (`crate::rows::store::
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

    if !super::cli::self_is_hub(me, &topo.hub) {
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
    // the hub — see `topology::cli::run`).
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

    if let Err(e) = crate::topology::store::write_atomic(store.path(), &text) {
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
#[path = "set_tests.rs"]
mod tests;
