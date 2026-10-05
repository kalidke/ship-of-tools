//! Per-host connection helpers: the default and monitor host, a not-yet-spawned
//! transport, the lane dial a row's attach uses.

use std::collections::HashMap;
use std::sync::Arc;

use crate::net::dial::HostKey;
use crate::net::transport::{OutgoingReq, ResolvedDial};

/// The connection `default_host`/startup `active_host` resolve to (ADR
/// 0042 L2a): the first connection in `conns`' display order (local-first,
/// then `--dial` argument order), else `fallback` (offline mode, no
/// connections at all). "Local first" is the TREE order, not a separate
/// selection rule — a daily launch lands on whichever connection sorts
/// first, matching topology plan/`--dial` (lane D): there is no more
/// configured `hosts.toml` `default_host` to prefer over it. (Pre-lane-D
/// this took a `configured_default: Option<HostKey>` that every caller had
/// already been passing `None` for since the `--dial` migration — deleted
/// outright rather than kept as unused plumbing; see git history if a
/// future `--default-host`-shaped flag needs the old preference-with-
/// fallback logic back.)
pub(crate) fn resolve_default_host(
    conns: &[(HostKey, tokio::sync::mpsc::UnboundedSender<OutgoingReq>)],
    fallback: HostKey,
) -> HostKey {
    conns
        .first()
        .map(|(h, _)| h.clone())
        .unwrap_or(fallback)
}

/// The connection the monitor drawer subscribes to: the declared hub (the
/// one daemon that executes the monitor roster), else the drawer's
/// existing default. Still FIXED — ADR 0042 L2a's invariant is that the
/// drawer does not follow the active host; this changes what it resolves
/// to, not whether it moves.
pub(crate) fn resolve_monitor_host(
    conns: &[(HostKey, tokio::sync::mpsc::UnboundedSender<OutgoingReq>)],
    hub: Option<&str>,
    fallback: HostKey,
) -> HostKey {
    hub.and_then(|h| conns.iter().find(|(c, _)| c == h).map(|(c, _)| c.clone()))
        .unwrap_or_else(|| resolve_default_host(conns, fallback))
}

/// One host's not-yet-spawned transport: the endpoint config plus the
/// receiver half of that host's own outgoing-request channel. Held on `App`
/// until `resumed()` has a window (and thus an `Arc<Window>` to hand each
/// transport task) — ADR 0042 L2a, the N-host generalisation of the old
/// single `req_rx`.
pub(crate) type PendingTransport = (
    crate::net::dial::HostKey,
    crate::net::transport::TransportConfig,
    tokio::sync::mpsc::UnboundedReceiver<crate::net::transport::OutgoingReq>,
);

/// Which `LaneDial` `spawn_pane_attach_term` should use for a host, given
/// its own `TransportConfig` (for `dial`/`token`) and the CONTROL
/// transport's own resolved selection for it (never re-derived: `Ssh`
/// carries the exact recipe the control connection already resolved and
/// dialed, so this never re-parses `config.dial` a second time).
/// Pulled out so the choice is unit-tested without a live `State`/window.
/// `None` when `resolved` says `Local` but `config.dial` holds `Ssh` —
/// shouldn't happen (the control connection could not have resolved
/// `Local` without a configured pipe), but degrading to "no dial" rather
/// than panicking matches this codebase's fail-soft convention throughout.
pub(crate) fn lane_dial(
    config: &crate::net::transport::TransportConfig,
    resolved: ResolvedDial,
    gate: &sot_protocol::topology::ssh_bridge::LinkGate,
) -> Option<(sot_protocol::topology::lane_client::LaneDial, Option<String>)> {
    match resolved {
        ResolvedDial::Local => match &config.dial {
            crate::net::transport::Dial::Pipe(path) => Some((
                sot_protocol::topology::lane_client::LaneDial::Local(path.clone()),
                config.token.clone(),
            )),
            crate::net::transport::Dial::Ssh(_) => None,
        },
        ResolvedDial::Ssh(recipe) => {
            Some((sot_protocol::topology::lane_client::LaneDial::Ssh(recipe, gate.clone()), config.token.clone()))
        }
    }
}

/// The window's per-host connection table (fe-net). State holds it as `hosts`;
/// drain's Connected and Disconnected arms and the transport spawn write it.
pub(crate) struct HostTable {
    /// Per-host connection status, derived from each connection's own
    /// `Connected`/`Disconnected`/`ProtocolMismatch` events (ADR 0042 L2a —
    /// no new wire signal). Absent or `false` = unreachable (never
    /// connected, or currently reconnecting); `true` = connected. The
    /// Sessions tree's host nodes read this to badge status and grey an
    /// unreachable host's (retained) workspace rows.
    pub(crate) host_connected: HashMap<crate::net::dial::HostKey, bool>,
    /// ADR 0045 decision 1: each host's own `TransportConfig` (its
    /// `lane.connect` bridge dial), so the session pane's capsule attach
    /// can reach THAT row's daemon — never a supervisor socket or a
    /// state-dir path directly. Filled once, from the same
    /// `PendingTransport` list `conns` is built from, before `resumed()`
    /// consumes it (`spawn_pane_attach_term` is the only reader).
    pub(crate) host_transports: HashMap<crate::net::dial::HostKey, crate::net::transport::TransportConfig>,
    /// ADR 0045 decision 1 (Codex review, lane B5 discharge): which
    /// transport each host's CONTROL connection actually resolved to
    /// (`ResolvedDial`'s own doc) — recorded from every `Connected` evt,
    /// consulted by `lane_dial`/`spawn_pane_attach_term` so the capsule
    /// lane dials the SAME endpoint, never a second independent guess.
    /// Absent for a host that hasn't connected yet.
    pub(crate) host_resolved_dial: HashMap<crate::net::dial::HostKey, ResolvedDial>,
    /// One link gate per host (`sot_protocol::topology::ssh_bridge::LinkGate`): the
    /// host's control transport writes it, and every other site that starts
    /// an ssh login to the host (lane dials, the page proxy) asks it.
    /// Always taken through `entry().or_default()`, so there is exactly one.
    pub(crate) link_gates: HashMap<crate::net::dial::HostKey, sot_protocol::topology::ssh_bridge::LinkGate>,
    /// ADR 0046 decision 1 (revised): the daemon's own declared identity
    /// for each dial — `HostKey` stays the stable dial label. Read by
    /// `host_label` (display: Hosts mode, Sessions labels, the status
    /// line, log lines) AND, since the session-listing brief, by the
    /// `Workspaces`/`Connected` event arms to decide whether a dial is
    /// the LOCAL daemon (its declared host equals `frontend_identity().host`)
    /// — the only thing that gates `fe.sessions`. Absent for a host that
    /// hasn't completed hello yet.
    pub(crate) declared_host: HashMap<crate::net::dial::HostKey, String>,
    /// F5 fires this to collapse the transport's exponential-backoff
    /// sleep and attempt an immediate reconnect — useful when wifi
    /// flickers and the user knows it's back before the current
    /// backoff cycle would have noticed. Held on State (not App) so
    /// the keyboard handler reaches it via &mut state.
    pub(crate) reconnect_now: Arc<tokio::sync::Notify>,
}

/// Starts one transport task per host once the window exists (ADR 0042 L2a):
/// records each host's `TransportConfig` in `hosts`, then spawns the tasks, all
/// fanning in to the one `evt_tx`. `resumed` calls it.
pub(crate) fn spawn_transports(
    rt: &tokio::runtime::Runtime,
    transports: Vec<PendingTransport>,
    evt_tx: &std::sync::mpsc::Sender<(HostKey, crate::net::transport::IncomingEvt)>,
    window: &std::sync::Arc<winit::window::Window>,
    leases: &std::sync::Arc<crate::lease::Leases>,
    hosts: &mut HostTable,
) {
    // ADR 0045 decision 1: captured BEFORE the loop below
    // consumes `transports` — the session pane's capsule
    // attach (`spawn_pane_attach_term`) reads this to build
    // that row's own daemon dial.
    hosts.host_transports = transports
        .iter()
        .map(|(host, config, _)| (host.clone(), config.clone()))
        .collect();
    for (host, config, req_rx) in transports {
        let gate = hosts.link_gates.entry(host.clone()).or_default().clone();
        crate::net::transport::spawn(
            rt,
            host,
            config,
            evt_tx.clone(),
            req_rx,
            window.clone(),
            hosts.reconnect_now.clone(),
            gate,
            leases.clone(),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lane_dial_matches_the_resolved_control_transport_selection() {
        let recipe = sot_protocol::topology::ssh_bridge::SshRecipe::new("hub", None).unwrap();
        let pipe_config = crate::net::transport::TransportConfig {
            dial: crate::net::transport::Dial::Pipe(std::path::PathBuf::from("/tmp/sock")),
            token: Some("tok".to_string()),
        };
        // A pipe-configured host whose control connection resolved LOCAL —
        // the lane dial follows.
        match lane_dial(&pipe_config, ResolvedDial::Local, &Default::default()) {
            Some((sot_protocol::topology::lane_client::LaneDial::Local(path), token)) => {
                assert_eq!(path, std::path::PathBuf::from("/tmp/sock"));
                assert_eq!(token.as_deref(), Some("tok"));
            }
            Some((sot_protocol::topology::lane_client::LaneDial::Ssh(..), _)) => {
                panic!("must follow the resolved Local selection, not guess ssh")
            }
            None => panic!("a resolved+configured pipe must dial, got None"),
        }
        // An ssh-configured host whose control connection resolved SSH —
        // the lane dial follows, carrying the resolved recipe verbatim
        // (never a second, independent read of `config.dial`).
        let ssh_config = crate::net::transport::TransportConfig {
            dial: crate::net::transport::Dial::Ssh(recipe.clone()),
            token: Some("tok".to_string()),
        };
        match lane_dial(&ssh_config, ResolvedDial::Ssh(recipe.clone()), &Default::default()) {
            Some((sot_protocol::topology::lane_client::LaneDial::Ssh(got, _), token)) => {
                assert_eq!(got, recipe);
                assert_eq!(token.as_deref(), Some("tok"));
            }
            Some((sot_protocol::topology::lane_client::LaneDial::Local(_), _)) => {
                panic!("must follow the resolved Ssh selection, not guess local")
            }
            None => panic!("a resolved ssh connection must dial, got None"),
        }
        // Resolved Local but `config.dial` holds Ssh (shouldn't happen —
        // the control connection could not have resolved Local without a
        // configured pipe) degrades to no dial rather than panicking.
        assert!(lane_dial(&ssh_config, ResolvedDial::Local, &Default::default()).is_none());
    }
}
