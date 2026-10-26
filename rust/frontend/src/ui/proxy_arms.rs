//! The page proxy's window half (ADR 0035): which hosts' pages are proxied, the manager that accepts for the
//! listeners, and the listener armed per (host, daemon port).

use super::page_proxy::PageSource;
use super::State;
use crate::net::dial::HostKey;

/// What arming a listener for a page came to.
pub(in crate::ui) enum ArmOutcome {
    /// Open this URL in the browser.
    Open(String),
    /// Not opened, and already logged: no manager, or the manager is gone.
    Dropped,
    /// An `Announced` page's port is held: by this window's listener for `owner`, or by an unknown local process.
    PortHeld { daemon_port: u16, owner: Option<HostKey> },
    /// The local bind failed.
    BindFailed(std::io::Error),
}

#[derive(Default)]
pub(in crate::ui) struct ProxyArms {
    /// Every host whose loopback pages this FE will
    /// proxy — inserted/removed per host from ITS OWN `Connected`/next
    /// `Connected` evt (`!matches!(resolved, ResolvedDial::Local) && proxy`),
    /// never FE-global: a box that also runs a local (pipe) daemon (ADR 0042)
    /// gets a `Connected{resolved: ResolvedDial::Local, ..}` from that one
    /// too, and a global flag let it silently switch proxying
    /// off for every OTHER host's pages (2026-09-10 field incident: every
    /// backend page "can't be reached" on a FE box with a local sotd, with
    /// nothing logged — the gate returned before the first log line). A later
    /// gate that scoped this to `default_host` alone fixed that incident but
    /// broke proxying for every non-default row's figures (cross-host figure
    /// defect) — per-host tracking fixes both: no host's evt can touch
    /// another's entry, and no host is structurally excluded. The actual
    /// dial for a proxy-capable host is read from `host_resolved_dial`
    /// (ADR 0045 decision 1) at arm time — never re-derived or restricted to
    /// one host — so `ensure_proxy_for_url` opens the proxy against the SAME
    /// daemon connection that owns the row, whichever host that is.
    capable: std::collections::HashSet<HostKey>,
    /// Hands GPU-thread-bound `std` listeners to the
    /// runtime accept loop, each tagged with its daemon port and the dial + token to
    /// reach that daemon through (`None` when no host has a
    /// runtime at all).
    manager: Option<tokio::sync::mpsc::UnboundedSender<crate::pages::PageListener>>,
    /// Per (host, daemon port), the port of the listener this
    /// frontend bound for it and the `Arm` that listener shares; opening a page
    /// on it re-arms it, since the daemon may have refused the port (`bad_port`) since.
    ensured: std::collections::HashMap<(HostKey, u16), (u16, std::sync::Arc<crate::pages::Arm>)>,
}

impl ProxyArms {
    pub(in crate::ui) fn capable(&self) -> &std::collections::HashSet<HostKey> { &self.capable }

    /// A host's own `Connected`: whether its pages are proxied from now on.
    pub(in crate::ui) fn note_connected(&mut self, host: &HostKey, proxied: bool) {
        if proxied {
            self.capable.insert(host.clone());
        } else {
            self.capable.remove(host);
        }
    }

    pub(in crate::ui) fn start_manager(&mut self, tx: tokio::sync::mpsc::UnboundedSender<crate::pages::PageListener>) {
        self.manager = Some(tx);
    }

    /// Arm (or re-arm) the listener for `url` on `host`, which proxies through `dial` with `token` behind `gate`.
    /// The bind is synchronous, so the port is listening by the time the caller launches the browser.
    pub(in crate::ui) fn arm(
        &mut self, host: &HostKey, url: &str, source: PageSource, dial: crate::pages::PageDial, token: Option<String>,
        gate: sot_topology::ssh_bridge::LinkGate,
    ) -> ArmOutcome {
        let Some(tx) = self.manager.as_ref() else {
            tracing::warn!(target: "sot::ui::page_proxy", %host, "proxy: no listener manager to arm one; not opening");
            return ArmOutcome::Dropped;
        };
        let Some(daemon_port) = sot_protocol::page_url::loopback_port_from_url(url) else {
            return ArmOutcome::Open(url.to_string()); // nothing to proxy, so nothing to arm
        };
        if let Some(opened) = State::armed_url(&self.ensured, host, daemon_port, url, source) {
            return ArmOutcome::Open(opened);
        }
        let (listener, local, opened) = match State::bind_proxy_listener(url, source) {
            Ok(bound) => bound,
            Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => {
                // An Announced page keeps the daemon's number, and something here already holds it. Occupancy is not
                // ownership: opening would render whatever that is, looking like the page meant. Not cached, so the
                // next open binds if the holder has gone.
                tracing::warn!(
                    target: "sot::ui::page_proxy",
                    daemon_port,
                    "proxy: port already bound by an UNKNOWN local listener — not ours; not opening"
                );
                let owner = self.ensured.iter().find(|(_, (local, _))| *local == daemon_port).map(|((h, _), _)| h.clone());
                return ArmOutcome::PortHeld { daemon_port, owner };
            }
            Err(e) => {
                tracing::warn!(target: "sot::ui::page_proxy", daemon_port, error = %e, "proxy: bind failed; not opening");
                return ArmOutcome::BindFailed(e);
            }
        };
        let arm = std::sync::Arc::new(crate::pages::Arm::default());
        let armed = crate::pages::PageListener {
            listener,
            daemon_port,
            dial: dial.clone(),
            token,
            gate,
            arm: std::sync::Arc::clone(&arm),
        };
        if tx.send(armed).is_err() {
            tracing::warn!(target: "sot::ui::page_proxy", daemon_port, "proxy: manager gone; not arming");
            return ArmOutcome::Dropped;
        }
        self.ensured.insert((host.clone(), daemon_port), (local, arm));
        tracing::info!(target: "sot::ui::page_proxy", daemon_port, local, ?source, %dial, "proxy: bound local listener for backend page");
        ArmOutcome::Open(opened)
    }
}
