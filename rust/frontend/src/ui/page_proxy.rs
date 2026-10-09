//! Arming a local listener so a remote daemon's page opens. The pages
//! subsystem's window half, held under ui/ because it reads `State`'s
//! private fields.

use super::*;

/// Outcome of `State::resolve_proxy_target` — see its doc for the three
/// cases. `Dial` carries the recipe to spawn for the remote leg
/// (`pipe_one`, `pages.rs`) — no longer a resolved `SocketAddr`:
/// the daemon has no TCP listener to resolve one for (C3 as amended §3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::ui) enum ProxyTarget {
    NotNeeded,
    Refused(String),
    Dial(crate::pages::PageDial, Option<String>),
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
        let token = host_transports.get(host).and_then(|c| c.token.clone());
        match host_resolved_dial.get(host) {
            Some(ResolvedDial::Ssh(recipe)) => {
                ProxyTarget::Dial(crate::pages::PageDial::Ssh(recipe.clone()), token)
            }
            Some(ResolvedDial::Relay(path)) => {
                ProxyTarget::Dial(crate::pages::PageDial::Relay(path.clone()), token)
            }
            _ => ProxyTarget::Refused(format!(
                "'{host}' has no resolved connection to proxy its pages through"
            )),
        }
    }

    /// This window's listener for the daemon page `url`, its port, and the URL the browser opens to reach it. The listener is
    /// 127.0.0.1 at a port the OS assigns, never the URL's own port number: that number names a port on the daemon's
    /// computer, and a window box that runs its own daemon holds the same preferred page ports (PAGE-PORT). The URL
    /// keeps everything but its port. Non-blocking, as the manager's `from_std` needs.
    pub(in crate::ui) fn bind_proxy_listener(url: &str) -> std::io::Result<(std::net::TcpListener, u16, String)> {
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0))?;
        listener.set_nonblocking(true)?;
        let local = listener.local_addr()?.port();
        let opened = sot_protocol::page_url::with_loopback_port(url, local).ok_or_else(|| {
            std::io::Error::new(std::io::ErrorKind::InvalidInput, "not a loopback page URL")
        })?;
        Ok((listener, local, opened))
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
    /// proxied against THAT daemon, not silently skipped.
    ///
    /// Returns the URL the caller opens in the browser, or `None` when it must
    /// not open one. A proxied page's URL names this frontend's listener
    /// (`bind_proxy_listener`), so it differs from the daemon's in its port alone;
    /// any other URL is returned as given. Every path that returns `None` has
    /// already written why to `self.status`, or logged it.
    /// `#[must_use]` so a new call site can't open the daemon's URL, whose port
    /// on this computer is someone else's.
    #[must_use]
    pub(in crate::ui) fn ensure_proxy_for_url(&mut self, host: &HostKey, url: &str) -> Option<String> {
        let target = Self::resolve_proxy_target(
            host,
            &self.proxy_capable_hosts,
            &self.hosts.host_resolved_dial,
            &self.hosts.host_transports,
        );
        let (dial, token) = match target {
            ProxyTarget::NotNeeded => return Some(url.to_string()),
            ProxyTarget::Refused(reason) => {
                tracing::warn!(%host, %reason, "proxy: refusing — no dial to proxy through");
                self.status = reason;
                self.window.request_redraw();
                return None;
            }
            ProxyTarget::Dial(dial, token) => (dial, token),
        };
        let gate = self.hosts.link_gates.entry(host.clone()).or_default().clone();
        let tx = self.proxy_listener_tx.as_ref()?; // past NotNeeded a proxy IS needed, and there's no manager to arm one
        let Some(daemon_port) = sot_protocol::page_url::loopback_port_from_url(url) else {
            return Some(url.to_string()); // nothing to proxy, so nothing to arm
        };
        let key = (host.clone(), daemon_port);
        if let Some((local, arm)) = self.proxy_ensured.get(&key) {
            arm.reopen(); // dial again: the daemon may have refused the port since
            return sot_protocol::page_url::with_loopback_port(url, *local);
        }
        let (listener, local, opened) = match Self::bind_proxy_listener(url) {
            Ok(bound) => bound,
            Err(e) => {
                tracing::warn!(daemon_port, error = %e, "proxy: bind failed; not opening");
                self.status = format!("page proxy: could not bind a local port · {e}");
                self.window.request_redraw();
                return None;
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
            tracing::warn!(daemon_port, "proxy: manager gone; not arming");
            return None;
        }
        self.proxy_ensured.insert(key, (local, arm));
        tracing::info!(daemon_port, local, %dial, "proxy: bound local listener for backend page");
        Some(opened)
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
                assert_eq!(got, crate::pages::PageDial::Ssh(recipe));
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
                assert_eq!(got, crate::pages::PageDial::Ssh(recipe));
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

    /// A generated hub relay is a remote page target: the proxy dials the exact path the control connection
    /// resolved, with the host's token, and a `Local` resolution still refuses.
    #[test]
    fn a_resolved_relay_is_a_remote_page_target() {
        let host = "gpu-box".to_string();
        let capable = std::collections::HashSet::from([host.clone()]);
        let path = std::path::PathBuf::from("/run/user/1000/sot-host-gpu-box.sock");
        let resolved = HashMap::from([(host.clone(), ResolvedDial::Relay(path.clone()))]);
        let transports = HashMap::from([(
            host.clone(),
            crate::net::transport::TransportConfig {
                dial: crate::net::transport::Dial::Relay(path.clone()),
                token: Some("tok".to_string()),
            },
        )]);
        assert_eq!(
            State::resolve_proxy_target(&host, &capable, &resolved, &transports),
            ProxyTarget::Dial(crate::pages::PageDial::Relay(path), Some("tok".to_string()))
        );
        let local = HashMap::from([(host.clone(), ResolvedDial::Local)]);
        assert!(matches!(
            State::resolve_proxy_target(&host, &capable, &local, &transports),
            ProxyTarget::Refused(_)
        ));
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

    /// PAGE-PORT: the daemon's page port held on this computer (as a window box's own daemon holds 1235 and 1236) no
    /// longer stops the page: the window's listener takes a port of its own, and the URL it opens differs from the
    /// daemon's in that port alone.
    #[test]
    fn a_page_whose_port_is_held_here_opens_at_the_windows_own_port() {
        let holder = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let held = holder.local_addr().unwrap().port();
        let url = format!("http://127.0.0.1:{held}/site/index.html?secret=s#top");
        let (listener, local, opened) =
            State::bind_proxy_listener(&url).expect("a held daemon port must not stop the page");
        assert_eq!(listener.local_addr().unwrap().port(), local);
        assert_ne!(local, held);
        assert_eq!(opened, format!("http://127.0.0.1:{local}/site/index.html?secret=s#top"));
        assert!(State::bind_proxy_listener("http://192.0.2.5:1236/").is_err(), "only a loopback page URL is rewritten");
    }
}
