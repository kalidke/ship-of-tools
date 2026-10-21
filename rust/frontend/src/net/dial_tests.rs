use super::*;

#[test]
fn parse_dial_arg_host_name_table() {
    for h in ["a", "0", "host-2", "a.b_c-d", "9lives"] {
        assert!(parse_dial_arg(&format!("{h}=ssh:hub")).is_ok(), "{h}");
    }
    for h in ["", "A", "aB", "-a", ".a", "_a", "a b", "a@b", "a:b", "é", "aé"] {
        assert!(parse_dial_arg(&format!("{h}=ssh:hub")).is_err(), "{h:?}");
    }
}

#[test]
fn parse_dial_arg_good_unix() {
    let (host, cfg) = parse_dial_arg("host-2=unix:/run/user/1234/sot/sessions/sot.sock")
        .expect("valid");
    assert_eq!(host, "host-2");
    assert_eq!(
        cfg.dial,
        crate::net::transport::Dial::Pipe(PathBuf::from("/run/user/1234/sot/sessions/sot.sock"))
    );
}

#[test]
fn parse_dial_arg_good_ssh_no_host() {
    let (host, cfg) = parse_dial_arg("host-1=ssh:hub").expect("valid");
    assert_eq!(host, "host-1");
    let crate::net::transport::Dial::Ssh(recipe) = &cfg.dial else {
        panic!("expected an ssh dial")
    };
    assert_eq!(recipe.target(), "hub");
    assert_eq!(recipe.host(), None);
}

#[test]
fn parse_dial_arg_good_ssh_with_host() {
    let (host, cfg) = parse_dial_arg("host-3=ssh:hub/host-3").expect("valid");
    assert_eq!(host, "host-3");
    let crate::net::transport::Dial::Ssh(recipe) = &cfg.dial else {
        panic!("expected an ssh dial")
    };
    assert_eq!(recipe.target(), "hub");
    assert_eq!(recipe.host(), Some("host-3"));
}

#[test]
fn parse_dial_arg_good_pipe() {
    let (host, cfg) = parse_dial_arg(r"host-4=pipe:\\.\pipe\sot-host-4").expect("valid");
    assert_eq!(host, "host-4");
    assert_eq!(
        cfg.dial,
        crate::net::transport::Dial::Pipe(PathBuf::from(r"\\.\pipe\sot-host-4"))
    );
}

#[test]
fn parse_dial_arg_malformed_missing_equals() {
    assert!(parse_dial_arg("host-2-ssh:hub").is_err());
}

#[test]
fn parse_dial_arg_malformed_unknown_scheme() {
    assert!(parse_dial_arg("host-2=tcp:somewhere:1").is_err());
}

#[test]
fn parse_dial_arg_malformed_empty_endpoint() {
    assert!(parse_dial_arg("host-2=ssh:").is_err());
    assert!(parse_dial_arg("host-2=unix:").is_err());
}

#[test]
fn parse_dial_arg_malformed_ssh_grammar() {
    // A leading dash would be read as an ssh OPTION; an `@`/space/`;`
    // falls outside the plain-host-name grammar either half must meet.
    assert!(parse_dial_arg("host-2=ssh:-oProxyCommand=x").is_err());
    assert!(parse_dial_arg("host-2=ssh:hub/-oProxyCommand=x").is_err());
    assert!(parse_dial_arg("host-2=ssh:user@hub").is_err());
}

#[test]
fn parse_dial_arg_unknown_host_rejects_bad_syntax() {
    // Uppercase, a leading dash, and an embedded space are all outside
    // the plain-host-name grammar `sotd topology plan` emits hosts in.
    assert!(parse_dial_arg("Alpha=ssh:hub").is_err());
    assert!(parse_dial_arg("-host-2=ssh:hub").is_err());
    assert!(parse_dial_arg("al pha=ssh:hub").is_err());
    assert!(parse_dial_arg("=ssh:hub").is_err());
}

#[test]
fn resolve_connections_repeated_dial_last_one_wins() {
    let dials = vec![
        parse_dial_arg("host-2=ssh:hub-a").unwrap(),
        parse_dial_arg("host-2=ssh:hub-b").unwrap(),
    ];
    let out = resolve_connections(&dials, &CliOverride::default());
    assert_eq!(out.len(), 1);
    let crate::net::transport::Dial::Ssh(recipe) = &out[0].1.dial else {
        panic!("expected an ssh dial")
    };
    assert_eq!(recipe.target(), "hub-b");
}

#[test]
fn resolve_connections_local_sorts_first() {
    let dials = vec![
        parse_dial_arg("host-2=ssh:hub").unwrap(),
        parse_dial_arg("local=unix:/run/user/1234/sot/sessions/sot.sock").unwrap(),
    ];
    let out = resolve_connections(&dials, &CliOverride::default());
    let names: Vec<&str> = out.iter().map(|(h, _)| h.as_str()).collect();
    assert_eq!(names, vec!["local", "host-2"]);
}

#[test]
fn resolve_connections_cli_override_wins_over_an_explicit_dial_local() {
    let dials = vec![parse_dial_arg("local=unix:/stale.sock").unwrap()];
    let cli = CliOverride {
        socket: Some(PathBuf::from("/fresh.sock")),
        token: Some("secret".to_string()),
    };
    let out = resolve_connections(&dials, &cli);
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].0, "local");
    assert_eq!(
        out[0].1.dial,
        crate::net::transport::Dial::Pipe(PathBuf::from("/fresh.sock")),
        "cli override replaces the stale dial entirely"
    );
}

#[test]
fn resolve_connections_cli_only_synthesizes_local() {
    let cli = CliOverride { socket: Some(PathBuf::from("/x.sock")), token: None };
    let out = resolve_connections(&[], &cli);
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].0, "local");
}

#[test]
fn resolve_connections_duplicate_ssh_endpoint_skips_the_later_host() {
    let dials = vec![
        parse_dial_arg("beta=ssh:hub").unwrap(),
        parse_dial_arg("alpha=ssh:hub").unwrap(),
    ];
    let out = resolve_connections(&dials, &CliOverride::default());
    let names: Vec<&str> = out.iter().map(|(h, _)| h.as_str()).collect();
    assert_eq!(names, vec!["beta"]);
}

#[test]
fn resolve_connections_same_target_different_host_suffix_does_not_collide() {
    // `ssh:hub/gamma` and `ssh:hub/delta` share a target but name
    // DIFFERENT far daemons — not the same collision `Display`'s
    // `ssh:<target>/<host>` spelling is keyed to guard against.
    let dials = vec![
        parse_dial_arg("gamma=ssh:hub/gamma").unwrap(),
        parse_dial_arg("delta=ssh:hub/delta").unwrap(),
    ];
    let out = resolve_connections(&dials, &CliOverride::default());
    let names: Vec<&str> = out.iter().map(|(h, _)| h.as_str()).collect();
    assert_eq!(names, vec!["gamma", "delta"]);
}

#[test]
fn resolve_connections_no_dial_no_cli_is_empty() {
    let out = resolve_connections(&[], &CliOverride::default());
    assert!(out.is_empty(), "no --dial and no --socket/--tcp must yield no connections at all");
}

/// A hub's generated relay sockets lead to remote daemons: parsing keeps them out of the lease hosts, whatever
/// the dial label, while this computer's own endpoint and other local sockets stay leased.
#[test]
fn hub_relay_is_remote_at_every_consumer() {
    let topo = sot_protocol::topology::parse(
        "hub = \"alpha\"\n[host.alpha]\ndaemon = true\n[host.beta]\ndaemon = true\n[host.gamma]\ndaemon = true\nfrontend = true\n",
    )
    .unwrap();
    let plan = sot_protocol::topology::dial_endpoints(&topo, "alpha");
    let beta_relay = sot_protocol::topology::relay_socket_path("beta");
    let mut conns = Vec::new();
    for (host, endpoint) in &plan {
        conns.push(
            parse_dial_arg(&format!("{host}={endpoint}")).expect("a planned dial parses"),
        );
    }
    // An aliased label for the same generated path, an ordinary local socket and a relay-looking path in
    // another folder.
    for arg in [
        format!("gpu=unix:{}", beta_relay.display()),
        // The generated name in a folder below the relay folder: on a host whose relay folder is /tmp this
        // is the one place a fixed /tmp path would itself be the generated path.
        format!(
            "scratch=unix:{}",
            beta_relay
                .parent()
                .unwrap()
                .join("elsewhere")
                .join("sot-host-beta.sock")
                .display()
        ),
        "plain=unix:/run/user/1/sot/sessions/other.sock".to_string(),
    ] {
        conns.push(parse_dial_arg(&arg).expect("valid"));
    }
    assert_eq!(
        crate::lease::pipe_hosts(&conns),
        vec![
            "alpha".to_string(),
            "scratch".to_string(),
            "plain".to_string()
        ]
    );
}
