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
