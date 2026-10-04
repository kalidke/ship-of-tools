//! Arming a local listener so a remote daemon's page opens. The pages
//! subsystem's window half, held under ui/ because it reads `State`'s
//! private fields.

use super::*;

/// Outcome of `State::resolve_proxy_target` — see its doc for the three
/// cases. `Dial` carries the recipe to spawn for the remote leg
/// (`pipe_one`, `proxy_listen.rs`) — no longer a resolved `SocketAddr`:
/// the daemon has no TCP listener to resolve one for (C3 as amended §3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::ui) enum ProxyTarget {
    NotNeeded,
    Refused(String),
    Dial(sot_protocol::topology::ssh_bridge::SshRecipe, Option<String>),
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
    /// `Dial`: the exact recipe (and its token, from `host_transports`) to
    /// spawn an ssh child through for this host's browser connections —
    /// the SAME connection the control transport already resolved for this
    /// host, never a second independent guess.
    pub(in crate::ui) fn resolve_proxy_target(
        host: &HostKey,
        proxy_capable_hosts: &std::collections::HashSet<HostKey>,
        host_resolved_dial: &HashMap<HostKey, ResolvedDial>,
        host_transports: &HashMap<HostKey, crate::net::transport::TransportConfig>,
    ) -> ProxyTarget {
        if !proxy_capable_hosts.contains(host) {
            return ProxyTarget::NotNeeded;
        }
        match host_resolved_dial.get(host) {
            Some(ResolvedDial::Ssh(recipe)) => {
                let token = host_transports.get(host).and_then(|c| c.token.clone());
                ProxyTarget::Dial(recipe.clone(), token)
            }
            _ => ProxyTarget::Refused(format!(
                "'{host}' has no resolved connection to proxy its pages through"
            )),
        }
    }

    /// Whether the caller may go on to open a browser tab, given the proxy
    /// `target` `resolve_proxy_target` already decided and — only when that
    /// target needed a bind attempt — how `TcpListener::bind` for its port
    /// went. Pulled out of `ensure_proxy_for_url`'s tail so the exact case
    /// the field report hit — a `Dial` target whose bind came back
    /// `AddrInUse` — is unit-tested without a live `State`/window, same
    /// shape as `resolve_proxy_target` itself.
    ///
    /// `target` alone answers it when no bind was attempted: `NotNeeded`
    /// means the URL never routes through a proxy at all, so it's always
    /// safe; `Refused` means there is no dial to proxy through, so it's
    /// never safe. For `Dial`, `bind` carries what the port attempt found —
    /// `Ok` means this frontend is about to serve it; any `Err`, most
    /// pointedly `AddrInUse`, means something else already answers on that
    /// port and opening now would show whatever THAT is, looking exactly
    /// like the page the caller meant to show.
    pub(in crate::ui) fn proxy_open_permitted(
        target: &ProxyTarget,
        bind: Option<&std::io::Result<std::net::TcpListener>>,
    ) -> bool {
        match target {
            ProxyTarget::NotNeeded => true,
            ProxyTarget::Refused(_) => false,
            ProxyTarget::Dial(..) => bind.is_some_and(|r| r.is_ok()),
        }
    }

    /// ADR 0035: before opening a backend-served loopback URL in the browser,
    /// make sure its port is reachable. On a REMOTE, proxy-capable FE this
    /// lazily binds a local listener (once per port) that pipes to the daemon
    /// through the control tunnel; on a local FE, or an older daemon, it's a
    /// no-op and the URL resolves directly / via the launcher's ssh forward.
    /// The bind is synchronous (`std::net::TcpListener`, sub-millisecond) so
    /// the port is listening by the time the caller launches the browser.
    /// `host` is the row's OWNING host (every call site already carries this
    /// — `event_host` off the announcement, or the `OpenUrl` command's
    /// `from_host`), so a figure served by a non-default host's daemon is
    /// proxied against THAT daemon, not silently skipped.
    ///
    /// Returns whether the caller may go on to open `url` in the browser.
    /// `true` means the page will be served by something this frontend
    /// itself arranged — no proxy was ever needed, this FE already armed
    /// the port earlier, or it just bound it now. `false` means the page
    /// will NOT be served by anything of ours, and every path that returns
    /// it has already written why to `self.status` — opening the browser
    /// anyway is worse than not opening: a port held by an unknown local
    /// listener renders someone else's page looking entirely normal,
    /// indistinguishable from the one the caller meant to show.
    /// `#[must_use]` so a new call site can't quietly repeat the bug this
    /// fixed: opening unconditionally after a refusal this function itself
    /// computed and discarded.
    #[must_use]
    pub(in crate::ui) fn ensure_proxy_for_url(&mut self, host: &HostKey, url: &str) -> bool {
        let target = Self::resolve_proxy_target(
            host,
            &self.proxy_capable_hosts,
            &self.hosts.host_resolved_dial,
            &self.hosts.host_transports,
        );
        let (recipe, token) = match &target {
            ProxyTarget::NotNeeded => return true,
            ProxyTarget::Refused(reason) => {
                tracing::warn!(%host, %reason, "proxy: refusing — no dial to proxy through");
                self.status = reason.clone();
                self.window.request_redraw();
                return false;
            }
            ProxyTarget::Dial(recipe, token) => (recipe.clone(), token.clone()),
        };
        let gate = self.hosts.link_gates.entry(host.clone()).or_default().clone();
        let Some(tx) = self.proxy_listener_tx.as_ref() else {
            return false; // past NotNeeded a proxy IS needed, and there's no manager to arm one
        };
        let Some(port) = crate::pages::proxy_port_from_url(url) else {
            return true; // nothing to proxy, so nothing to arm
        };
        if let Some(arm) = self.proxy_ensured.get(&port) {
            arm.reopen();
            return true; // already bound by this frontend; dial again, the daemon may have refused it since
        }
        let arm = std::sync::Arc::new(crate::pages::Arm::default());
        self.proxy_ensured.insert(port, std::sync::Arc::clone(&arm));
        let bind = std::net::TcpListener::bind(("127.0.0.1", port));
        let permit_open = Self::proxy_open_permitted(&target, Some(&bind));
        match bind {
            Ok(listener) => {
                if let Err(e) = listener.set_nonblocking(true) {
                    tracing::warn!(port, error = %e, "proxy: set_nonblocking failed; not arming");
                    self.proxy_ensured.remove(&port);
                    return false;
                }
                if tx.send((listener, recipe.clone(), token, gate, arm)).is_err() {
                    tracing::warn!(port, "proxy: manager gone; not arming");
                    self.proxy_ensured.remove(&port);
                    return false;
                }
                tracing::info!(port, %recipe, "proxy: bound local listener for backend page");
            }
            Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => {
                // Something already holds the port. Do NOT cache this: un-mark
                // so the next open re-probes. If the holder later exits, a
                // subsequent open binds and proxies instead of being wedged
                // connection-refused forever. A re-probe is a cheap bind.
                self.proxy_ensured.remove(&port);
                // Who the holder is decides whether this is benign.
                //
                // When the daemon advertises the proxy, the launchers no longer
                // forward these ports (ADR 0035's aux `-L` retirement), so
                // "already bound" is NOT the old legacy-forward case — it is an
                // UNKNOWN local listener, and opening the browser at it would
                // render someone else's page looking entirely normal. Occupancy
                // is not proof of ownership, so say so instead of proceeding
                // quietly.
                tracing::warn!(
                    port,
                    "proxy: port already bound by an UNKNOWN local listener — not ours. \
                     Refusing to treat occupancy as ownership; the page may be someone else's."
                );
                self.status = format!(
                    "port {port} is held by another local process — not opening (could be the wrong page)"
                );
                self.window.request_redraw();
            }
            Err(e) => {
                tracing::warn!(port, error = %e, "proxy: bind failed");
                self.proxy_ensured.remove(&port);
            }
        }
        permit_open
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Cross-host figure defect (topology plan step 7): `resolve_proxy_target`
    /// must open the proxy against the daemon that OWNS the announcing row,
    /// not only a single default host. A row on a non-default host with a
    /// resolved ssh dial must proxy through ITS OWN recipe — this fails
    /// against the old `proxy_host == Some(default_host)`-only gate, which
    /// left every non-default host's `Dial` case unreachable.
    #[test]
    fn resolve_proxy_target_dials_a_non_default_hosts_own_resolved_address() {
        let host = "gpu-box".to_string();
        let mut proxy_capable_hosts = std::collections::HashSet::new();
        proxy_capable_hosts.insert(host.clone());
        let recipe = sot_protocol::topology::ssh_bridge::SshRecipe::new("hub", Some(&host)).unwrap();
        let mut host_resolved_dial = HashMap::new();
        host_resolved_dial.insert(host.clone(), ResolvedDial::Ssh(recipe.clone()));
        let mut host_transports = HashMap::new();
        host_transports.insert(
            host.clone(),
            crate::net::transport::TransportConfig {
                dial: crate::net::transport::Dial::Ssh(
                    sot_protocol::topology::ssh_bridge::SshRecipe::new("ignored", None).unwrap(),
                ),
                token: Some("tok-gpu".to_string()),
            },
        );
        match State::resolve_proxy_target(&host, &proxy_capable_hosts, &host_resolved_dial, &host_transports)
        {
            ProxyTarget::Dial(got, token) => {
                assert_eq!(got, recipe);
                assert_eq!(token.as_deref(), Some("tok-gpu"));
            }
            other => panic!("expected Dial for a proxy-capable host with a resolved ssh recipe, got {other:?}"),
        }
    }

    /// The default host must still proxy — this fix must not regress the
    /// existing single-host case, only widen it.
    #[test]
    fn resolve_proxy_target_still_dials_the_default_host() {
        let host = "hub".to_string();
        let mut proxy_capable_hosts = std::collections::HashSet::new();
        proxy_capable_hosts.insert(host.clone());
        let recipe = sot_protocol::topology::ssh_bridge::SshRecipe::new("hub", None).unwrap();
        let mut host_resolved_dial = HashMap::new();
        host_resolved_dial.insert(host.clone(), ResolvedDial::Ssh(recipe.clone()));
        let host_transports = HashMap::new();
        match State::resolve_proxy_target(&host, &proxy_capable_hosts, &host_resolved_dial, &host_transports)
        {
            ProxyTarget::Dial(got, token) => {
                assert_eq!(got, recipe);
                assert_eq!(token, None);
            }
            other => panic!("expected Dial for the default host, got {other:?}"),
        }
    }

    /// A proxy-capable host with no resolved dial (state otherwise
    /// desynced — a recipe cannot fail to be observed the way a tcp peer
    /// address once could) is a
    /// visible refusal, never a silent drop — the caller surfaces
    /// `Refused`'s reason to the status line.
    #[test]
    fn resolve_proxy_target_refuses_visibly_when_no_dial_is_held() {
        let host = "orphan".to_string();
        let mut proxy_capable_hosts = std::collections::HashSet::new();
        proxy_capable_hosts.insert(host.clone());
        let host_resolved_dial = HashMap::new(); // never resolved a dial
        let host_transports = HashMap::new();
        match State::resolve_proxy_target(&host, &proxy_capable_hosts, &host_resolved_dial, &host_transports)
        {
            ProxyTarget::Refused(reason) => assert!(reason.contains(&host)),
            other => panic!("expected a visible Refused reason, got {other:?}"),
        }
    }

    /// A host that never advertised proxy capability (a local daemon, or
    /// one that hasn't connected remotely) needs no proxying at all — its
    /// pages resolve directly, so this stays silent (`NotNeeded`), not a
    /// refusal.
    #[test]
    fn resolve_proxy_target_is_not_needed_for_a_non_proxy_capable_host() {
        let host = "local-box".to_string();
        let proxy_capable_hosts = std::collections::HashSet::new();
        let host_resolved_dial = HashMap::new();
        let host_transports = HashMap::new();
        assert_eq!(
            State::resolve_proxy_target(&host, &proxy_capable_hosts, &host_resolved_dial, &host_transports),
            ProxyTarget::NotNeeded
        );
    }

    /// The field report's exact shape: a `Dial` target (this host IS
    /// proxy-capable and holds a resolved dial) whose bind attempt comes
    /// back `AddrInUse` must refuse the open — the port already answers to
    /// an unknown local listener, and opening would show whatever THAT
    /// serves, looking exactly like the page the caller meant to show.
    #[test]
    fn proxy_open_permitted_refuses_an_addr_in_use_dial() {
        let holder = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = holder.local_addr().unwrap().port();
        let bind = std::net::TcpListener::bind(("127.0.0.1", port));
        assert!(matches!(
            bind.as_ref().err().map(std::io::Error::kind),
            Some(std::io::ErrorKind::AddrInUse)
        ));
        let target = ProxyTarget::Dial(sot_protocol::topology::ssh_bridge::SshRecipe::new("hub", None).unwrap(), None);
        assert!(!State::proxy_open_permitted(&target, Some(&bind)));
    }

    /// The success half of the same `Dial` case — a bind that actually
    /// lands permits the open: this frontend is the one about to serve it.
    #[test]
    fn proxy_open_permitted_allows_a_successful_dial_bind() {
        let bind = std::net::TcpListener::bind(("127.0.0.1", 0));
        assert!(bind.is_ok());
        let target = ProxyTarget::Dial(sot_protocol::topology::ssh_bridge::SshRecipe::new("hub", None).unwrap(), None);
        assert!(State::proxy_open_permitted(&target, Some(&bind)));
    }

    /// `NotNeeded` and `Refused` never reach a bind attempt at all — the
    /// decision is `target` alone with no `bind` result to consult.
    #[test]
    fn proxy_open_permitted_decides_not_needed_and_refused_without_a_bind() {
        assert!(State::proxy_open_permitted(&ProxyTarget::NotNeeded, None));
        assert!(!State::proxy_open_permitted(
            &ProxyTarget::Refused("no dial".to_string()),
            None
        ));
    }
}
