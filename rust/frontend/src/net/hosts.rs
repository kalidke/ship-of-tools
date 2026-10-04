//! Per-host connection helpers: the default and monitor host, a not-yet-spawned
//! transport, the lane dial a row's attach uses.

use crate::dial::HostKey;
use crate::transport::{OutgoingReq, ResolvedDial};

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
    crate::dial::HostKey,
    crate::transport::TransportConfig,
    tokio::sync::mpsc::UnboundedReceiver<crate::transport::OutgoingReq>,
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
    config: &crate::transport::TransportConfig,
    resolved: ResolvedDial,
    gate: &sot_protocol::ssh_bridge::LinkGate,
) -> Option<(sot_protocol::lane_client::LaneDial, Option<String>)> {
    match resolved {
        ResolvedDial::Local => match &config.dial {
            crate::transport::Dial::Pipe(path) => Some((
                sot_protocol::lane_client::LaneDial::Local(path.clone()),
                config.token.clone(),
            )),
            crate::transport::Dial::Ssh(_) => None,
        },
        ResolvedDial::Ssh(recipe) => {
            Some((sot_protocol::lane_client::LaneDial::Ssh(recipe, gate.clone()), config.token.clone()))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lane_dial_matches_the_resolved_control_transport_selection() {
        let recipe = sot_protocol::ssh_bridge::SshRecipe::new("hub", None).unwrap();
        let pipe_config = crate::transport::TransportConfig {
            dial: crate::transport::Dial::Pipe(std::path::PathBuf::from("/tmp/sock")),
            token: Some("tok".to_string()),
        };
        // A pipe-configured host whose control connection resolved LOCAL —
        // the lane dial follows.
        match lane_dial(&pipe_config, ResolvedDial::Local, &Default::default()) {
            Some((sot_protocol::lane_client::LaneDial::Local(path), token)) => {
                assert_eq!(path, std::path::PathBuf::from("/tmp/sock"));
                assert_eq!(token.as_deref(), Some("tok"));
            }
            Some((sot_protocol::lane_client::LaneDial::Ssh(..), _)) => {
                panic!("must follow the resolved Local selection, not guess ssh")
            }
            Some((sot_protocol::lane_client::LaneDial::Tcp(_), _)) => {
                panic!("lane_dial never produces Tcp (C3 as amended)")
            }
            None => panic!("a resolved+configured pipe must dial, got None"),
        }
        // An ssh-configured host whose control connection resolved SSH —
        // the lane dial follows, carrying the resolved recipe verbatim
        // (never a second, independent read of `config.dial`).
        let ssh_config = crate::transport::TransportConfig {
            dial: crate::transport::Dial::Ssh(recipe.clone()),
            token: Some("tok".to_string()),
        };
        match lane_dial(&ssh_config, ResolvedDial::Ssh(recipe.clone()), &Default::default()) {
            Some((sot_protocol::lane_client::LaneDial::Ssh(got, _), token)) => {
                assert_eq!(got, recipe);
                assert_eq!(token.as_deref(), Some("tok"));
            }
            Some((sot_protocol::lane_client::LaneDial::Local(_), _)) => {
                panic!("must follow the resolved Ssh selection, not guess local")
            }
            Some((sot_protocol::lane_client::LaneDial::Tcp(_), _)) => {
                panic!("lane_dial never produces Tcp (C3 as amended)")
            }
            None => panic!("a resolved ssh connection must dial, got None"),
        }
        // Resolved Local but `config.dial` holds Ssh (shouldn't happen —
        // the control connection could not have resolved Local without a
        // configured pipe) degrades to no dial rather than panicking.
        assert!(lane_dial(&ssh_config, ResolvedDial::Local, &Default::default()).is_none());
    }
}
