//! The window's view of its connection set: which connection a request goes
//! to, and the window's per-host names. fe-net's data, held under ui/
//! because these `State` methods read its private fields.

use super::*;

impl State {
    /// Send `req` on `active_host`'s connection — the routing for every
    /// "current view" operation (cursor state, the active tree, anything
    /// keyed by `active_workspace_id`). ADR 0042 L2a: the mechanical
    /// replacement for the old single `self.send(req)` — same
    /// signature, so every such call site becomes `self.send(req)` with no
    /// other change. A host absent from `conns` (offline mode, or a name
    /// that never resolved to a connection) reports the request as
    /// undeliverable via the same `SendError` shape `UnboundedSender::send`
    /// itself returns, so existing `if let Err(e) = ...` call sites need no
    /// change to their error handling either.
    pub(in crate::ui) fn send(
        &self,
        req: OutgoingReq,
    ) -> Result<(), tokio::sync::mpsc::error::SendError<OutgoingReq>> {
        self.send_to(&self.active_host.clone(), req)
    }

    /// Send `req` on `host`'s connection — for ops that target a specific
    /// tree row rather than whatever's currently active (switch, destroy,
    /// pty.open/attach on a non-active row; ADR 0042 L2a).
    pub(in crate::ui) fn send_to(
        &self,
        host: &crate::dial::HostKey,
        req: OutgoingReq,
    ) -> Result<(), tokio::sync::mpsc::error::SendError<OutgoingReq>> {
        match self.conns.iter().find(|(h, _)| h == host) {
            Some((_, tx)) => tx.send(req),
            None => Err(tokio::sync::mpsc::error::SendError(req)),
        }
    }
}

impl State {
    /// The drawer's fixed home (Terminal/Monitor/Repl overlay pane, ADR
    /// 0041's fixed-tenant "one drawer"). Distinct from `active_host`:
    /// switching to a workspace on another host moves `active_host`, but
    /// the drawer never follows — "no drawer host switching". Delegates to
    /// `resolve_default_host` — the same resolution `State::new` uses for
    /// the STARTUP `active_host` (`conns.first()`, local-first then
    /// `--dial` argument order).
    pub(in crate::ui) fn default_host(&self) -> HostKey {
        resolve_default_host(&self.conns, self.active_host.clone())
    }

    /// The monitor drawer's subscribe target (2.1): the declared hub when
    /// it's among today's connections, else `default_host`'s fallback. The
    /// three former `state.default_host()` call sites at the monitor
    /// subscribe/history/tab-label points use this instead.
    pub(in crate::ui) fn monitor_host(&self) -> HostKey {
        resolve_monitor_host(&self.conns, self.monitor_hub.as_deref(), self.active_host.clone())
    }

    /// Every connection in display order (ADR 0042 L2a) — local-first,
    /// then --dial argument order, from `conns` (fixed at startup). Deliberately
    /// NOT filtered to hosts that have answered a `workspace.list` yet: L2's
    /// acceptance criterion is that an unreachable (or still-mid-hello) host
    /// is a VISIBLE node marked unreachable, not an absent one.
    /// `fresh_workspace_caches` is what skips a host with no list — it has
    /// nothing to contribute to the caches, but it still gets a tree node.
    pub(in crate::ui) fn ordered_hosts(&self) -> Vec<HostKey> {
        self.conns.iter().map(|(h, _)| h.clone()).collect()
    }

    /// Record dial `label`'s declared identity (ADR 0046 decision 1) for
    /// display only (Hosts mode, Sessions labels, the status line, and
    /// connect/disconnect logs — see `host_label`) — `HostKey` itself is
    /// never re-homed, and this map feeds no other decision. Manager
    /// review (S8, Codex finding B1): a duplicate declaration is NOT
    /// refused here — refusing it would require actually closing the
    /// newcomer's transport, and no such shutdown path exists today (a
    /// spawned transport task's `JoinHandle` is discarded; there is no
    /// cancellation mechanism reachable from the GPU thread). Restoring a
    /// half-built lifecycle around a decision this code cannot enforce
    /// would be worse than not detecting the collision at all — the
    /// static same-port skip in `dial::resolve_connections` is what
    /// prevents two dials from ever reaching the same daemon in the first
    /// place, restored for exactly this reason.
    pub(in crate::ui) fn record_declared_host(&mut self, label: &HostKey, declared: String) {
        self.declared_host.insert(label.clone(), declared);
    }
}
