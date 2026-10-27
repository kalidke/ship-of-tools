//! Arming a local listener so a remote daemon's page opens. The pages
//! subsystem's window half, held under ui/ because it reads `State`'s
//! private fields.

use super::*;
use super::proxy_arms::ArmOutcome;

/// Outcome of `State::resolve_proxy_target` — see its doc for the three
/// cases. `Dial` carries the token for the remote leg (`pipe_one`,
/// `pages.rs`), which opens a channel on the host's link — no longer a
/// resolved `SocketAddr`: the daemon has no TCP listener to resolve one for
/// (C3 as amended §3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::ui) enum ProxyTarget {
    NotNeeded,
    Refused(String),
    Dial(Option<String>),
}

/// Where a page URL came from, which decides the port this window may serve it on (PAGE-PORT). `Served`: the reply of a
/// daemon page op (`docs.open`, `video.open`, `pluto.open`), whose servers name no origin of their own, so the page opens
/// at any port. `Announced`: a URL user code announced (`wglshow`, a `BrowserView`) or an `open_url` command named; such
/// a page may name its own address, so it is reached at the daemon's port number or not at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::ui) enum PageSource {
    Served,
    Announced,
}

impl State {
    /// Where (if anywhere) a proxy listener for `host`'s pages should dial.
    /// Pulled out of `ensure_proxy_for_url` so the decision is unit-tested
    /// without a live `State`/window — same shape as `lane_dial` /
    /// `resolve_default_host` elsewhere in this file.
    ///
    /// `NotNeeded`: `host` isn't a proxy-capable remote (a local daemon, or
    /// one that never advertised `proxy`/never connected remotely) — its
    /// pages resolve directly, silently, same as always.
    /// `Refused`: `host` IS proxy-capable but this FE holds no resolved
    /// remote dial for it right now (`host_resolved_dial` — ADR 0045
    /// decision 1 — has no entry, or the entry is `Local`, which cannot
    /// coexist with a proxy-capable remote and means state has desynced).
    /// Visible, not a silent drop: the page would otherwise fail with no
    /// explanation.
    /// `Dial`: the host's browser connections go over its link (with its
    /// token, from `host_transports`) — the SAME link the control transport
    /// holds for this host, never a second independent guess.
    pub(in crate::ui) fn resolve_proxy_target(
        host: &HostKey,
        proxy_capable_hosts: &std::collections::HashSet<HostKey>,
        host_resolved_dial: &HashMap<HostKey, ResolvedDial>,
        host_transports: &HashMap<HostKey, crate::net::transport::TransportConfig>,
    ) -> ProxyTarget {
        if !proxy_capable_hosts.contains(host) {
            return ProxyTarget::NotNeeded;
        }
        let token = host_transports.get(host).and_then(|c| c.token.clone());
        match host_resolved_dial.get(host) {
            Some(ResolvedDial::Ssh(_) | ResolvedDial::Relay(_)) => ProxyTarget::Dial(token),
            _ => ProxyTarget::Refused(format!(
                "'{host}' has no resolved connection to proxy its pages through"
            )),
        }
    }

    /// This window's listener for the daemon page `url`, its port, and the URL the browser opens to reach it. A `Served`
    /// page's listener is 127.0.0.1 at a port the OS assigns, since the URL's own number names a port on the daemon's
    /// computer and a window box that runs its own daemon holds the same preferred page ports; its URL keeps everything
    /// but the port. An `Announced` page's listener takes the URL's own number, so a taken number is an `AddrInUse`
    /// error. Non-blocking, as the manager's `from_std` needs.
    pub(in crate::ui) fn bind_proxy_listener(
        url: &str,
        source: PageSource,
    ) -> std::io::Result<(std::net::TcpListener, u16, String)> {
        let not_a_page = || std::io::Error::new(std::io::ErrorKind::InvalidInput, "not a loopback page URL");
        let daemon_port = sot_protocol::page_url::loopback_port_from_url(url).ok_or_else(not_a_page)?;
        let port = match source {
            PageSource::Served => 0,
            PageSource::Announced => daemon_port,
        };
        let listener = std::net::TcpListener::bind(("127.0.0.1", port))?;
        listener.set_nonblocking(true)?;
        let local = listener.local_addr()?.port();
        let opened = sot_protocol::page_url::with_loopback_port(url, local).ok_or_else(not_a_page)?;
        Ok((listener, local, opened))
    }

    /// The URL to open for a page this window already armed a listener for, by (`host`, `daemon_port`), re-arming that
    /// listener since the daemon may have refused the port since; `None` when nothing fits. Keyed by host as well as
    /// port: two daemons serving one port number reach two listeners. An `Announced` page reuses only a listener that
    /// holds the daemon's own number, since it may name that address; a `Served` listener elsewhere does not fit it.
    pub(in crate::ui) fn armed_url(
        ensured: &HashMap<(HostKey, u16), (u16, std::sync::Arc<crate::pages::Arm>)>,
        host: &HostKey,
        daemon_port: u16,
        url: &str,
        source: PageSource,
    ) -> Option<String> {
        let (local, arm) = ensured.get(&(host.clone(), daemon_port))?;
        if source == PageSource::Announced && *local != daemon_port {
            return None;
        }
        arm.reopen();
        sot_protocol::page_url::with_loopback_port(url, *local)
    }

    /// ADR 0035: before opening a backend-served loopback URL in the browser,
    /// make sure it is reachable. On a REMOTE, proxy-capable FE this
    /// lazily binds a local listener (once per host and daemon port) that pipes
    /// to the daemon through the control tunnel; on a local FE, or an older daemon, it's a
    /// no-op and the URL resolves directly / via the launcher's ssh forward.
    /// The bind is synchronous (`std::net::TcpListener`, sub-millisecond) so
    /// the port is listening by the time the caller launches the browser.
    /// `host` is the row's OWNING host (every call site already carries this
    /// — `event_host` off the announcement, or the `OpenUrl` command's
    /// `from_host`), so a figure served by a non-default host's daemon is
    /// proxied against THAT daemon, not silently skipped. `source` says where
    /// the URL came from, which decides the listener's port (`PageSource`).
    ///
    /// Returns the URL the caller opens in the browser, or `None` when it must
    /// not open one. A proxied `Served` page's URL names this frontend's
    /// listener, so it differs from the daemon's in its port alone; any other
    /// URL is returned as given. Every path that returns `None` has already
    /// written why to `self.status`, or logged it: opening anyway at a port
    /// held by an unknown local listener renders someone else's page looking
    /// entirely normal. `#[must_use]` so a new call site can't open the
    /// daemon's URL after a refusal.
    #[must_use]
    pub(in crate::ui) fn ensure_proxy_for_url(
        &mut self,
        host: &HostKey,
        url: &str,
        source: PageSource,
    ) -> Option<String> {
        let target = Self::resolve_proxy_target(
            host,
            self.proxies.capable(),
            &self.hosts.host_resolved_dial,
            &self.hosts.host_transports,
        );
        let token = match target {
            ProxyTarget::NotNeeded => return Some(url.to_string()),
            ProxyTarget::Refused(reason) => {
                tracing::warn!(%host, %reason, "proxy: refusing — no dial to proxy through");
                self.status.set(reason);
                self.window.request_redraw();
                return None;
            }
            ProxyTarget::Dial(token) => token,
        };
        let link = self.hosts.links.entry(host.clone()).or_default().clone();
        match self.proxies.arm(host, url, source, token, link) {
            ArmOutcome::Open(opened) => Some(opened),
            ArmOutcome::Dropped => None,
            ArmOutcome::PortHeld { daemon_port, owner } => {
                self.status.set(match owner {
                    Some(other) => format!(
                        "port {daemon_port} is held by this window's page from '{other}' — not opening (it would show that page)"
                    ),
                    None => format!(
                        "port {daemon_port} is held by another local process — not opening (could be the wrong page)"
                    ),
                });
                self.window.request_redraw();
                None
            }
            ArmOutcome::BindFailed(e) => {
                self.status.set(format!("page proxy: could not bind a local port · {e}"));
                self.window.request_redraw();
                None
            }
        }
    }
}

#[cfg(test)]
#[path = "page_proxy_tests.rs"]
mod tests;
