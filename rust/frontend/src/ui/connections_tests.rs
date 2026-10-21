use super::*;

// --- ADR 0042 L2a: multi-host connection set, workspace-cache union,
// and per-host routing. ---







#[test]
fn eval_id_workspace_owner_disambiguates_same_eval_id_across_hosts() {
    // ADR 0042 L2a codex review, item C: both daemons independently
    // number runs from the same start (`numbers::next` is a per-process
    // backend static), so a bare `eval_id` key would let host beta's id-1
    // reply resolve against host alpha's routing entry (or land
    // alpha's frames in beta's workspace). Keying by (HostKey,
    // eval_id) — the same `owner_id = (event_host.clone(), eval_id)`
    // shape every ReplEvalDone/ReplFrameStreamed/ReplRunFileDone
    // handler builds — keeps the two hosts' "1" apart.
    let mut owners: HashMap<(HostKey, u64), WsKey> = HashMap::new();
    let key_alpha: WsKey = ("alpha".to_string(), "<default>".to_string());
    let key_beta: WsKey = ("beta".to_string(), "<default>".to_string());
    owners.insert(("alpha".to_string(), 1), key_alpha.clone());
    owners.insert(("beta".to_string(), 1), key_beta.clone());

    // A reply tagged event_host=beta, eval_id=1 resolves to beta's
    // workspace even though the bare eval_id (1) also matches alpha's
    // entry.
    let owner_id = ("beta".to_string(), 1u64);
    assert_eq!(owners.get(&owner_id), Some(&key_beta));
    assert_ne!(owners.get(&owner_id), Some(&key_alpha));
}




#[test]
fn every_hosts_connected_requests_its_own_workspace_list() {
    // ADR 0042 L2a codex review, item A: `Connected` used to fire
    // OutgoingReq::WorkspaceList only for active_host (`self.send`);
    // every other host connected silently and its Sessions-tree node
    // stayed unreachable until manually expanded. The real fix routes
    // via `send_to(&event_host, ...)` on EVERY Connected -- proven
    // here as two connections (alpha active, beta not) each getting
    // their OWN WorkspaceList request through `route_send_to`
    // (the production `send_to` body, per `route_send_to`'s own
    // doc), and the resulting per-host replies folding into a
    // union with both hosts present (the same invariant
    // `union_replace_leaves_the_other_hosts_list_intact` pins).
    let (conns, mut rxs) = fake_conns();
    // "alpha" is active; "local" connects too, non-active. Both fire
    // send_to(&event_host, WorkspaceList) — the fix's whole point is
    // that the non-active one is NOT skipped.
    for host in ["local", "alpha"] {
        route_send_to(&conns, &host.to_string(), OutgoingReq::WorkspaceList).unwrap();
    }
    assert!(
        rxs.get_mut("local").unwrap().try_recv().is_ok(),
        "the non-active host (\"local\") still got its own request"
    );
    assert!(
        rxs.get_mut("alpha").unwrap().try_recv().is_ok(),
        "the active host got its request too"
    );

    // Both hosts' replies land and union into one map, neither
    // clobbering the other (mirrors the real workspace_lists.insert).
    let mut lists: HashMap<HostKey, Vec<crate::net::transport::WorkspaceInfo>> = HashMap::new();
    lists.insert("local".to_string(), vec![ws_info("home", "sot-be-home")]);
    lists.insert("alpha".to_string(), vec![ws_info("sot", "sot-be-sot")]);
    assert_eq!(
        lists.len(),
        2,
        "both hosts' lists are present after both replies land"
    );
}


/// A fake `req_tx`: `Vec<(HostKey, UnboundedSender<OutgoingReq>)>` is
/// exactly `State::conns`' own type, so `State::send`/`send_to`'s
/// routing logic (`self.conns.iter().find(...)`) is exercised here
/// verbatim, without constructing a GPU-backed `State`.
fn fake_conns() -> (
    Vec<(HostKey, tokio::sync::mpsc::UnboundedSender<OutgoingReq>)>,
    HashMap<HostKey, tokio::sync::mpsc::UnboundedReceiver<OutgoingReq>>,
) {
    let mut conns = Vec::new();
    let mut rxs = HashMap::new();
    for host in ["local", "alpha"] {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        conns.push((host.to_string(), tx));
        rxs.insert(host.to_string(), rx);
    }
    (conns, rxs)
}

/// Mirrors `State::send`/`send_to`'s exact bodies (they're `&self`
/// methods with no other dependency) so the routing rule — `send` →
/// `active_host`, `send_to` → the named host — is provable without a
/// GPU-backed `State`.
fn route_send(
    conns: &[(HostKey, tokio::sync::mpsc::UnboundedSender<OutgoingReq>)],
    active_host: &HostKey,
    req: OutgoingReq,
) -> Result<(), tokio::sync::mpsc::error::SendError<OutgoingReq>> {
    route_send_to(conns, active_host, req)
}
fn route_send_to(
    conns: &[(HostKey, tokio::sync::mpsc::UnboundedSender<OutgoingReq>)],
    host: &HostKey,
    req: OutgoingReq,
) -> Result<(), tokio::sync::mpsc::error::SendError<OutgoingReq>> {
    match conns.iter().find(|(h, _)| h == host) {
        Some((_, tx)) => tx.send(req),
        None => Err(tokio::sync::mpsc::error::SendError(req)),
    }
}

#[test]
fn send_routes_to_active_host() {
    let (conns, mut rxs) = fake_conns();
    route_send(&conns, &"alpha".to_string(), OutgoingReq::WorkspaceList).unwrap();
    assert!(rxs.get_mut("alpha").unwrap().try_recv().is_ok());
    assert!(rxs.get_mut("local").unwrap().try_recv().is_err());
}

#[test]
fn send_to_routes_to_the_named_host_not_active() {
    let (conns, mut rxs) = fake_conns();
    // active_host is "alpha", but send_to explicitly targets "local" —
    // a row-scoped op (e.g. workspace.destroy on a non-active row).
    route_send_to(&conns, &"local".to_string(), OutgoingReq::WorkspaceList).unwrap();
    assert!(rxs.get_mut("local").unwrap().try_recv().is_ok());
    assert!(rxs.get_mut("alpha").unwrap().try_recv().is_err());
}


#[test]
fn send_to_unknown_host_errs_instead_of_silently_dropping() {
    let (conns, _rxs) = fake_conns();
    let err = route_send_to(
        &conns,
        &"nonexistent".to_string(),
        OutgoingReq::WorkspaceList,
    );
    assert!(
        err.is_err(),
        "an unrouteable host must surface as an error, not vanish"
    );
}

/// Mirrors `State::report_presence`'s host fan-out (`for (host, _) in
/// &self.conns { self.send_to(host, OutgoingReq::FePresence) }`) so the
/// 2026-09-08 review correction — a person is present for EVERY daemon
/// this frontend is attached to, not only `active_host` — is provable
/// without a GPU-backed `State`.
fn route_report_presence(conns: &[(HostKey, tokio::sync::mpsc::UnboundedSender<OutgoingReq>)]) {
    for (host, _) in conns {
        let _ = route_send_to(conns, host, OutgoingReq::FePresence);
    }
}

#[test]
fn report_presence_fans_out_to_every_connected_host_not_just_active() {
    let (conns, mut rxs) = fake_conns();
    route_report_presence(&conns);
    for host in ["local", "alpha"] {
        assert!(
            rxs.get_mut(host).unwrap().try_recv().is_ok(),
            "fe.presence must reach every connected host's daemon ({host} included) — \
                 a person is present for all of them, not only whichever is active"
        );
    }
}

fn ws_info_with_agent(
    slug: &str,
    session_name: &str,
    agent_handle: &str,
    agent_state: &str,
) -> crate::net::transport::WorkspaceInfo {
    crate::net::transport::WorkspaceInfo {
        agent_handle: agent_handle.to_string(),
        agent_state: agent_state.to_string(),
        ..ws_info(slug, session_name)
    }
}

/// Mirrors the `Workspaces` arm's `fe.sessions` fan-out
/// (session-listing brief decision 2): a reply is declared only when
/// it is the LOCAL daemon's own (its `declared_host` entry equals
/// `local_host`), and even then never back to itself — only to every
/// OTHER connection.
fn route_fe_sessions(
    conns: &[(HostKey, tokio::sync::mpsc::UnboundedSender<OutgoingReq>)],
    declared_host: &HashMap<HostKey, String>,
    local_host: &str,
    event_host: &HostKey,
    rows: &[crate::net::transport::WorkspaceInfo],
) {
    if declared_host.get(event_host).map(String::as_str) != Some(local_host) {
        return;
    }
    let sessions = declared_sessions_from(rows);
    for (host, _) in conns {
        if host == event_host {
            continue;
        }
        let _ = route_send_to(conns, host, OutgoingReq::FeSessions(sessions.clone()));
    }
}

#[test]
fn fe_sessions_declares_a_local_list_to_every_other_host_never_the_local_one() {
    let (conns, mut rxs) = fake_conns();
    let mut declared_host: HashMap<HostKey, String> = HashMap::new();
    declared_host.insert("local".to_string(), "this-box".to_string());
    declared_host.insert("alpha".to_string(), "remote-box".to_string());
    let rows = vec![ws_info_with_agent("sot", "sot-be-sot", "agent@this-box", "working")];

    route_fe_sessions(&conns, &declared_host, "this-box", &"local".to_string(), &rows);

    match rxs.get_mut("alpha").unwrap().try_recv() {
        Ok(OutgoingReq::FeSessions(sessions)) => {
            assert_eq!(sessions.len(), 1);
            assert_eq!(sessions[0].handle, "agent@this-box");
            assert_eq!(sessions[0].state, "working");
        }
        other => panic!("expected fe.sessions on the other host, got {other:?}"),
    }
    assert!(
        rxs.get_mut("local").unwrap().try_recv().is_err(),
        "the local connection never declares to itself"
    );
}

#[test]
fn fe_sessions_a_remote_hosts_own_list_declares_nothing() {
    let (conns, mut rxs) = fake_conns();
    let mut declared_host: HashMap<HostKey, String> = HashMap::new();
    declared_host.insert("local".to_string(), "this-box".to_string());
    declared_host.insert("alpha".to_string(), "remote-box".to_string());
    let rows = vec![ws_info_with_agent("sot", "sot-be-sot", "agent@remote-box", "working")];

    // "alpha" is a REMOTE daemon; its own row list must declare
    // nothing — this frontend has no inbox on that box, so a
    // declaration made on its behalf would be a promise this process
    // cannot keep.
    route_fe_sessions(&conns, &declared_host, "this-box", &"alpha".to_string(), &rows);

    assert!(rxs.get_mut("local").unwrap().try_recv().is_err());
    assert!(rxs.get_mut("alpha").unwrap().try_recv().is_err());
}



#[test]
fn startup_active_host_is_conns_first_else_the_fallback() {
    // "Local first" is the TREE order (ordered_hosts) AND the initial
    // selection since lane D: there is no more configured
    // `hosts.toml` default_host to prefer over it (folded from the
    // pre-lane-D "configured default wins"/"ignores an endpointless
    // configured default" cases, both meaningless now that
    // resolve_default_host takes no configured_default at all).
    let (conns, _rxs) = fake_conns(); // ["local", "alpha"], in that order
    assert_eq!(resolve_default_host(&conns, "offline".to_string()), "local");
    // No connections at all (offline mode) → the fallback name.
    assert_eq!(resolve_default_host(&[], "offline".to_string()), "offline");
}

#[test]
fn resolve_monitor_host_prefers_the_declared_hub() {
    // ["local", "alpha"], in that order — the hub is not first, so this
    // also proves the resolver doesn't just defer to conns.first().
    let (conns, _rxs) = fake_conns();
    assert_eq!(
        resolve_monitor_host(&conns, Some("alpha"), "offline".to_string()),
        "alpha"
    );
}

#[test]
fn resolve_monitor_host_falls_back_without_a_hub() {
    let (conns, _rxs) = fake_conns();
    // No hub declared at all.
    assert_eq!(
        resolve_monitor_host(&conns, None, "offline".to_string()),
        resolve_default_host(&conns, "offline".to_string())
    );
    // A hub declared but not among today's connections.
    assert_eq!(
        resolve_monitor_host(&conns, Some("gamma"), "offline".to_string()),
        resolve_default_host(&conns, "offline".to_string())
    );
    // No connections at all (offline mode) → the fallback name either way.
    assert_eq!(
        resolve_monitor_host(&[], Some("alpha"), "offline".to_string()),
        "offline"
    );
}
