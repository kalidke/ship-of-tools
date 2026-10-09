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
        let Some(tx) = self.proxy_listener_tx.as_ref() else {
            tracing::warn!(%host, "proxy: no listener manager to arm one; not opening");
            return None;
        };
        let Some(daemon_port) = sot_protocol::page_url::loopback_port_from_url(url) else {
            return Some(url.to_string()); // nothing to proxy, so nothing to arm
        };
        if let Some(opened) = Self::armed_url(&self.proxy_ensured, host, daemon_port, url, source) {
            return Some(opened);
        }
        let (listener, local, opened) = match Self::bind_proxy_listener(url, source) {
            Ok(bound) => bound,
            Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => {
                // An Announced page keeps the daemon's number, and something here already holds it. Occupancy is not
                // ownership: opening would render whatever that is, looking like the page meant. Not cached, so the
                // next open binds if the holder has gone.
                tracing::warn!(
                    daemon_port,
                    "proxy: port already bound by an UNKNOWN local listener — not ours; not opening"
                );
                let ours = self.proxy_ensured.iter().find(|(_, (local, _))| *local == daemon_port);
                self.status = match ours {
                    Some(((other, _), _)) => format!(
                        "port {daemon_port} is held by this window's page from '{other}' — not opening (it would show that page)"
                    ),
                    None => format!(
                        "port {daemon_port} is held by another local process — not opening (could be the wrong page)"
                    ),
                };
                self.window.request_redraw();
                return None;
            }
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
        self.proxy_ensured.insert((host.clone(), daemon_port), (local, arm));
        tracing::info!(daemon_port, local, ?source, %dial, "proxy: bound local listener for backend page");
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

    /// PAGE-PORT: a Served page whose daemon port is held on this computer (as a window box's own daemon holds 1235
    /// and 1236) still opens: the window's listener takes a port of its own, and the URL it opens differs from the
    /// daemon's in that port alone.
    #[test]
    fn a_served_page_whose_port_is_held_here_opens_at_the_windows_own_port() {
        let holder = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let held = holder.local_addr().unwrap().port();
        let url = format!("http://127.0.0.1:{held}/site/index.html?secret=s#top");
        let (listener, local, opened) = State::bind_proxy_listener(&url, PageSource::Served)
            .expect("a held daemon port must not stop a served page");
        assert_eq!(listener.local_addr().unwrap().port(), local);
        assert_ne!(local, held);
        assert_eq!(opened, format!("http://127.0.0.1:{local}/site/index.html?secret=s#top"));
        assert!(
            State::bind_proxy_listener("http://192.0.2.5:1236/", PageSource::Served).is_err(),
            "only a loopback page URL is rewritten"
        );
    }

    /// An Announced page may name its own address, so it is reached at the daemon's port number or not at all: a held
    /// number is `AddrInUse` (which `ensure_proxy_for_url` refuses visibly), and a free one is bound as is.
    #[test]
    fn an_announced_page_keeps_the_daemons_port_number() {
        let holder = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let held = holder.local_addr().unwrap().port();
        let refused = State::bind_proxy_listener(&format!("http://127.0.0.1:{held}/"), PageSource::Announced);
        assert_eq!(refused.err().map(|e| e.kind()), Some(std::io::ErrorKind::AddrInUse));
        drop(holder);
        let url = format!("http://127.0.0.1:{held}/app");
        let (_listener, local, opened) = State::bind_proxy_listener(&url, PageSource::Announced).unwrap();
        assert_eq!((local, opened), (held, url));
    }

    /// Two daemons serving one port number reach two listeners: the reuse path is keyed by host and daemon port, and
    /// it re-arms the listener it finds.
    #[test]
    fn two_hosts_on_one_daemon_port_reach_two_listeners() {
        let arm_a = std::sync::Arc::new(crate::pages::Arm::default());
        let arm_b = std::sync::Arc::new(crate::pages::Arm::default());
        let ensured = HashMap::from([
            (("host-a".to_string(), 1236), (50001, std::sync::Arc::clone(&arm_a))),
            (("host-b".to_string(), 1236), (50002, std::sync::Arc::clone(&arm_b))),
        ]);
        let url = "http://127.0.0.1:1236/n/index.html";
        let open = |host: &str| State::armed_url(&ensured, &host.to_string(), 1236, url, PageSource::Served);
        assert_eq!(open("host-a").as_deref(), Some("http://127.0.0.1:50001/n/index.html"));
        assert_eq!(open("host-b").as_deref(), Some("http://127.0.0.1:50002/n/index.html"));
        assert_eq!(open("host-c"), None);
        assert_eq!(State::armed_url(&ensured, &"host-a".to_string(), 1235, url, PageSource::Served), None);
    }

    /// An Announced page may name its own address, so it reuses only a listener at the daemon's own number: a Served
    /// listener armed for that daemon port at another local port does not fit it, and one at the number does.
    #[test]
    fn an_announced_page_reuses_only_a_listener_at_the_daemons_number() {
        let arm = std::sync::Arc::new(crate::pages::Arm::default());
        let host = "host-a".to_string();
        let url = "http://127.0.0.1:41000/app";
        let moved = HashMap::from([((host.clone(), 41000), (50001, std::sync::Arc::clone(&arm)))]);
        assert_eq!(State::armed_url(&moved, &host, 41000, url, PageSource::Announced), None);
        assert!(State::armed_url(&moved, &host, 41000, url, PageSource::Served).is_some());
        let same = HashMap::from([((host.clone(), 41000), (41000, arm))]);
        assert_eq!(State::armed_url(&same, &host, 41000, url, PageSource::Announced).as_deref(), Some(url));
    }
}
