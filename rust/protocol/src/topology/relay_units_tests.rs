use super::super::tests::V2;
use super::*;

const APPLY_FIXTURE: &str = r#"
hub = "hub"

[host.hub]
daemon = true

[host.remote-a]
daemon = true

[host.remote-b]

[host.fe]
daemon = true
frontend = true
"#;

/// The comm relay's reverse tunnels are a separate concern from who is
/// dialable, and this is what pins them apart: `fe` is dialable (it
/// declares `daemon`) and still carries no tunnel, `remote-b` carries
/// a tunnel and is not dialable at all.
#[test]
fn tunnel_hosts_excludes_hub_and_frontend() {
    let t = parse(APPLY_FIXTURE).unwrap();
    assert_eq!(tunnel_hosts(&t), vec!["remote-a", "remote-b"]);
    assert_eq!(relay_hosts(&t), vec!["remote-a", "fe"]);
    // gamma in V2 is daemon AND frontend: no tunnel, but dialable.
    let v2 = parse(V2).unwrap();
    assert_eq!(tunnel_hosts(&v2), vec!["beta"]);
    assert_eq!(relay_hosts(&v2), vec!["gamma"]);
}

#[test]
fn require_hub_refuses_elsewhere() {
    let t = parse(APPLY_FIXTURE).unwrap();
    assert!(require_hub(&t, "hub", "apply").is_ok());
    let e = require_hub(&t, "remote-a", "relay-sockets").unwrap_err();
    assert!(e.contains("`remote-a`") && e.contains("`hub`") && e.contains("relay-sockets"), "{e}");
}

#[test]
fn apply_plan_enables_disables_and_settles() {
    let t = parse(APPLY_FIXTURE).unwrap();
    // Nothing enabled yet: enable everything each family wants.
    let p = apply_plan(&t, &[], &[]);
    assert_eq!(p.tunnels.enable, vec!["remote-a", "remote-b"]);
    assert!(p.tunnels.disable.is_empty());
    // Exactly one relay per dialable non-hub host, and no other.
    assert_eq!(p.relays.enable, vec!["remote-a", "fe"]);
    assert!(p.relays.disable.is_empty());
    // A stale instance (no longer declared) alongside one still wanted,
    // in each family independently.
    let p = apply_plan(&t, &["remote-a".to_string(), "gone".to_string()], &["fe".to_string(), "remote-b".to_string()]);
    assert_eq!(p.tunnels.enable, vec!["remote-b"]);
    assert_eq!(p.tunnels.disable, vec!["gone"]);
    assert_eq!(p.relays.enable, vec!["remote-a"]);
    // `remote-b` carries a tunnel but declares no daemon: a relay for
    // it is exactly the kind of stale instance apply takes away.
    assert_eq!(p.relays.disable, vec!["remote-b"]);
    // Second run, now converged: a true no-op.
    let p = apply_plan(
        &t,
        &["remote-a".to_string(), "remote-b".to_string()],
        &["remote-a".to_string(), "fe".to_string()],
    );
    assert!(p.is_empty(), "{p:?}");
}

#[test]
fn unit_names_and_pin_paths() {
    assert_eq!(tunnel_unit("remote-a"), "sot-relay-tunnel@remote-a");
    assert_eq!(relay_unit("remote-a"), "sot-host-relay-remote-a.socket");
    assert_eq!(relay_service_unit_file("remote-a"), "sot-host-relay-remote-a@.service");
    let t = parse(APPLY_FIXTURE).unwrap();
    assert_eq!(daemon_pin(&t).0, "sotd.service.d/topology.conf");
    let paths: Vec<String> = hub_pins(&t, &["remote-a"]).into_iter().map(|(p, _)| p).collect();
    assert_eq!(paths, ["sot-relay-tunnel@.service.d/topology.conf", "sot-host-relay-remote-a.socket.d/topology.conf"]);
    let no_tunnel = parse("hub = \"hub\"\n[host.hub]\ndaemon = true\n[host.fe]\nfrontend = true\n").unwrap();
    let paths: Vec<String> = hub_pins(&no_tunnel, &[]).into_iter().map(|(p, _)| p).collect();
    assert_eq!(paths, ["sot-relay-tunnel@.service.d/topology.conf"], "a tunnel runs nowhere but the hub, wanted or not");
}

#[test]
fn a_pin_names_each_host_and_its_domain_forms_only() {
    let set = |pin: String| pin.lines().filter(|l| !l.starts_with('#')).map(str::to_string).collect::<Vec<_>>();
    assert_eq!(set(host_pin(&["a", "b"])), ["[Unit]", "ConditionHost=|a", "ConditionHost=|a.*", "ConditionHost=|b", "ConditionHost=|b.*"]);
    let t = parse(APPLY_FIXTURE).unwrap();
    assert_eq!(daemon_pin(&t).1, host_pin(&["hub", "remote-a", "fe"]));
    assert!(hub_pins(&t, &["remote-a"]).iter().all(|(_, text)| *text == host_pin(&["hub"])));
    let (path, text) = install_pin("a");
    assert_eq!((path, set(text)), (daemon_pin(&t).0, vec!["[Unit]".to_string(), "ConditionHost=|a".into(), "ConditionHost=|a.*".into()]));
}

/// The four lines the route actually rides on. The host is in the unit
/// NAME (systemd #19071 — see [`relay_unit`]), the listener is exactly
/// the path `relay-sockets` prints, `Accept=yes` is what gives each
/// connection its own bridge, and ssh's `%C` must reach ssh as `%C` and
/// not be eaten as a systemd specifier.
#[test]
fn relay_units_carry_the_host_and_the_declared_path() {
    let sock = relay_socket_unit("remote-a");
    assert!(sock.contains(&format!("ListenStream={}\n", relay_socket_path("remote-a").display())), "{sock}");
    assert!(sock.contains("\nAccept=yes\n"), "{sock}");
    // ADR 0049, User isolation: a route into another box's daemon is owner-only.
    assert!(sock.contains("\nSocketMode=0600\n"), "{sock}");
    assert!(sock.contains("sot-host-relay-remote-a@.service"), "{sock}");

    let svc = relay_service_unit("remote-a");
    assert!(svc.contains("Environment=SOT_RELAY_TARGET=remote-a\n"), "{svc}");
    assert!(!svc.contains("SOT_RELAY_LABEL"), "{svc}");
    assert!(relay_command_dropin().contains(" stdio-bridge\n"));
    assert!(!svc.contains("KillMode"), "{svc}");
    assert!(svc.contains("\nStandardInput=socket\n") && svc.contains("\nStandardOutput=socket\n"), "{svc}");
}

#[test]
fn relay_service_unit_collects_from_the_unit_section() {
    let svc = relay_service_unit("remote-a");
    let (unit, service, collect) = (svc.find("\n[Unit]\n"), svc.find("\n[Service]\n"), svc.find("\nCollectMode=inactive-or-failed\n"));
    assert!(unit < collect && collect < service, "CollectMode belongs in [Unit], systemd ignores it in [Service]:\n{svc}");
}

/// The generated ssh line, run through real `ssh -G` against the block an
/// older ENROLLING-A-HOST.md told operators to put in the hub's
/// ~/.ssh/config: sharing must read off however the config sets it.
#[cfg(target_os = "linux")]
#[test]
fn relay_ssh_turns_multiplexing_off_over_an_ssh_config_that_turns_it_on() {
    const OLD_ADVICE: &str = "Host remote-a\n    ControlMaster auto\n    ControlPath ~/.ssh/cm-%C\n    ControlPersist 10m\n";
    let svc = relay_command_dropin();
    let exec = svc.lines().find_map(|l| l.strip_prefix("ExecStart=/usr/bin/ssh ")).expect("ExecStart runs /usr/bin/ssh");
    let opts: Vec<&str> = exec.split_whitespace().take_while(|t| *t != "${SOT_RELAY_TARGET}").collect();
    let dir = std::env::temp_dir().join(format!("sot-f3-sshg-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let cfg = dir.join("config");
    std::fs::write(&cfg, OLD_ADVICE).unwrap();
    // The OpenSSH client `ssh` must be on PATH for this test: a missing one fails the run, naming it.
    let ran = sot_log::test_wait::run_within(
        std::process::Command::new("ssh").arg("-F").arg(&cfg).arg("-G").args(&opts).arg("remote-a"),
        std::time::Duration::from_secs(30),
    );
    let _ = std::fs::remove_dir_all(&dir);
    let ran = ran.ok();
    let eff = &ran.stdout;
    let has = |want: &str| eff.lines().any(|l| l == want);
    assert!(has("controlmaster false"), "opts {opts:?}\n{eff}");
    assert!(has("controlpersist no"), "opts {opts:?}\n{eff}");
    assert!(!eff.lines().any(|l| l.starts_with("controlpath ")), "opts {opts:?}\n{eff}");
}

/// Measured with systemd-analyze verify, systemd 249: drop-ins apply in byte order of file name.
#[test]
fn relay_command_is_set_once_and_last() {
    let [_, (svc_name, svc), (name, dropin)] = relay_files("remote-a");
    assert!(!svc.lines().any(|l| l.starts_with("ExecStart")), "{svc}");
    assert!(svc.contains(RELAY_COMMAND_DROPIN), "{svc}");
    assert_eq!(name, format!("{svc_name}.d/{RELAY_COMMAND_DROPIN}"));
    let set: Vec<&str> = dropin.lines().filter(|l| !l.is_empty() && !l.starts_with('#')).collect();
    assert!(set.len() == 3 && set[..2] == ["[Service]", "ExecStart="], "{set:?}");
    assert!(set[2].starts_with("ExecStart=/usr/bin/ssh ") && set[2].ends_with(" stdio-bridge"), "{set:?}");
    for older in ["local.conf", "no-mux.conf", "override.conf", PIN_FILE] {
        assert!(older < RELAY_COMMAND_DROPIN, "{older}");
    }
}
