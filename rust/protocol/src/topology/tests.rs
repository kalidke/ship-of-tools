//! Tests of the hosts.toml parser, search rule, status table and the endpoint derivations in `topology`.

use super::*;

pub(super) const V2: &str = r#"
hub = "alpha"   # the relay daemon

[host.alpha]
daemon = true

[host.beta]

[host.gamma]
daemon = true
frontend = true

[host.delta]
frontend = true

[monitor]
alpha = "alpha"
beta = ""
gpu-box = "other-user@gpu-box"
"#;

#[test]
fn v2_parses() {
    let t = parse(V2).unwrap();
    assert_eq!(t.hub, "alpha");
    assert_eq!(t.hosts.len(), 4);
    assert_eq!(t.host("gamma").unwrap(), &HostDecl { name: "gamma".into(), daemon: true, frontend: true });
    assert_eq!(t.host("beta").unwrap(), &HostDecl { name: "beta".into(), daemon: false, frontend: false });
    assert_eq!(t.monitor_targets()[1], ("beta".to_string(), "beta".to_string()));
    assert_eq!(t.monitor_targets()[2].1, "other-user@gpu-box");
}

#[test]
fn two_hubs_fail() {
    let e = parse("hub = \"a\"\nhub = \"b\"\n[host.a]\n").unwrap_err();
    assert!(e.contains("line 2") && e.contains("`hub` given twice"), "{e}");
}

#[test]
fn hub_must_be_listed() {
    let e = parse("hub = \"ghost\"\n[host.a]\n").unwrap_err();
    assert!(e.contains("hub `ghost` is not a listed host"), "{e}");
}

#[test]
fn unknown_top_level_key_still_errors() {
    // The top-level vocabulary is fixed and tiny (`hub` only) — a typo
    // there is a real mistake and still fails loudly, message unchanged.
    let e = parse("hub = \"a\"\nsize = 3\n[host.a]\n").unwrap_err();
    assert!(e.contains("unknown key `size`"), "{e}");
}

/// Was `unknown_key_names_the_key`'s host-level half, which asserted an
/// unknown `[host.<name>]` key was a hard error — that was the old
/// contract. Changed deliberately: it is now a collected warning, and
/// the host's known keys (`daemon` here) still apply.
#[test]
fn unknown_host_key_is_a_collected_warning_not_an_error() {
    let t = parse("hub = \"a\"\n[host.a]\ndaemon = true\ncolour = \"red\"\n").unwrap();
    assert!(t.host("a").unwrap().daemon, "the host's known keys still apply");
    assert_eq!(t.warnings.len(), 1);
    assert!(
        t.warnings[0].contains("line 4") && t.warnings[0].contains("unknown key `colour`") && t.warnings[0].contains("[host.a]"),
        "{:?}",
        t.warnings
    );
}

#[test]
fn several_unknown_host_keys_across_hosts_all_collected_in_order() {
    let text = "hub = \"a\"\n[host.a]\ndaemon = true\nfoo = \"x\"\n[host.b]\nbar = 1\nbaz = true\n";
    let t = parse(text).unwrap();
    assert_eq!(t.hosts.len(), 2);
    assert_eq!(t.warnings.len(), 3, "{:?}", t.warnings);
    assert!(t.warnings[0].contains("[host.a]") && t.warnings[0].contains("unknown key `foo`"), "{:?}", t.warnings);
    assert!(t.warnings[1].contains("[host.b]") && t.warnings[1].contains("unknown key `bar`"), "{:?}", t.warnings);
    assert!(t.warnings[2].contains("[host.b]") && t.warnings[2].contains("unknown key `baz`"), "{:?}", t.warnings);
}

#[test]
fn status_table_surfaces_host_key_warnings() {
    let t = parse("hub = \"a\"\n[host.a]\ndaemon = true\ncolour = \"red\"\n").unwrap();
    let out = status_table(&t);
    assert!(out.contains("WARNING") && out.contains("unknown key `colour`") && out.contains("[host.a]"), "{out}");
}

#[test]
fn bad_section_key_fails() {
    let e = parse("hub = \"a\"\n[host.a]\n[host.Two Words]\n").unwrap_err();
    assert!(e.contains("line 3") && e.contains("`[host.Two Words]` is not a plain host name"), "{e}");
    let e = parse("hub = \"a\"\n[host.a]\n[hosts]\n").unwrap_err();
    assert!(e.contains("unknown section `[hosts]`"), "{e}");
}

/// The v1 shim is gone at the TOP LEVEL: an old-grammar file's
/// `default_host` is still a loud parse error at its first unknown key,
/// naming the line — that vocabulary is fixed and tiny, so this half of
/// the old test is unchanged. Its second half asserted an old per-host
/// key (`ssh_alias`) was ALSO a hard error — that was the old contract;
/// changed deliberately (see `unknown_host_key_is_a_collected_warning_
/// not_an_error`): a per-host key this build does not know is now
/// forward-compatible, collected as a warning instead of losing the
/// whole file.
#[test]
fn old_grammar_top_level_key_still_fails_host_key_is_only_a_warning() {
    let text = "\u{feff}default_host = \"hub\"\n\n[host.hub]\nsocket = \"/run/user/1000/sot/sessions/sot.sock\"\n";
    let e = parse(text).unwrap_err();
    assert!(e.contains("line 1") && e.contains("unknown key `default_host`"), "{e}");

    let t = parse("hub = \"a\"\n[host.a]\ndaemon = true\nssh_alias = \"a\"\n").unwrap();
    assert_eq!(t.warnings.len(), 1);
    assert!(t.warnings[0].contains("unknown key `ssh_alias`") && t.warnings[0].contains("[host.a]"), "{:?}", t.warnings);
}

#[test]
fn v2_hub_must_be_a_daemon_host() {
    let e = parse("hub = \"a\"\n[host.a]\n[host.b]\ndaemon = true\n").unwrap_err();
    assert!(e.contains("hub `a` has `daemon = false`") && e.contains("[host.a]"), "{e}");
}

#[test]
fn duplicate_keys_and_labels_fail() {
    let e = parse("hub = \"a\"\n[host.a]\ndaemon = true\ndaemon = false\n").unwrap_err();
    assert!(e.contains("line 4") && e.contains("`daemon` given twice"), "{e}");
    let e = parse("hub = \"a\"\n[host.a]\ndaemon = true\n[monitor]\nx = \"x\"\nx = \"y\"\n").unwrap_err();
    assert!(e.contains("line 6") && e.contains("`x` given twice"), "{e}");
    // The same key in two sections is fine.
    parse("hub = \"a\"\n[host.a]\ndaemon = true\n[host.b]\ndaemon = true\n").unwrap();
}

#[test]
fn plan_lines_are_stable() {
    let t = parse(V2).unwrap();
    let own = local_endpoint();
    // gamma is daemon AND frontend: dialable all the same, and on its
    // own box that dial is the implicit local connection. Reaching the
    // hub (alpha) is `ssh:<hub>` with no `--host` — a no-argument
    // `stdio-bridge` on alpha already means alpha's own daemon.
    assert_eq!(
        plan(&t, "gamma").unwrap(),
        format!("self gamma\nhub alpha\nrelay-endpoint ssh:alpha\ndial alpha ssh:alpha\ndial gamma {own}\n")
    );
    // On the hub every dial is already local — its own socket for
    // itself, its own relay socket for gamma — no ssh child needed
    // for either.
    assert_eq!(
        plan(&t, "alpha").unwrap(),
        format!(
            "self alpha\nhub alpha\nrelay-endpoint {own}\ndial alpha {own}\ndial gamma unix:{}\n",
            relay_socket_path("gamma").display()
        )
    );
    // A peer reaches gamma the same way it reaches any other
    // non-hub host: an ssh child into the hub, `--host gamma`.
    let beta_plan = plan(&t, "beta").unwrap();
    assert!(beta_plan.contains("dial alpha ssh:alpha\n"), "{beta_plan}");
    assert!(beta_plan.contains("dial gamma ssh:alpha/gamma\n"), "{beta_plan}");
    let beta = beta_plan;
    assert!(beta.lines().nth(2).unwrap().starts_with("relay-endpoint unix:"), "{beta}");
    assert!(beta.lines().nth(2).unwrap().ends_with("sot-relay.sock"), "{beta}");
    assert!(plan(&t, "nobody").unwrap_err().contains("`nobody` is not a listed host"));
}

#[test]
fn relay_socket_path_is_a_sibling_of_the_relay_socket() {
    let a = relay_socket_path("gamma");
    assert_eq!(a.file_name().unwrap(), "sot-host-gamma.sock");
    let relay = crate::runtime_sot_dir();
    let base = relay.parent().map(PathBuf::from).unwrap_or(relay);
    assert_eq!(a.parent().unwrap(), base, "the hub's per-host sockets sit beside sot-relay.sock");
    assert_ne!(relay_socket_path("gamma"), relay_socket_path("delta"));
}

#[test]
fn status_table_is_declared_only() {
    let t = parse(V2).unwrap();
    assert_eq!(
        status_table(&t),
        "HOST DECLARED\nalpha hub,daemon,sampled\nbeta shell,sampled\ngamma daemon,frontend\ndelta frontend\ngpu-box monitor-only other-user@gpu-box\n"
    );
}

#[test]
fn hash_text_is_deterministic_and_content_sensitive() {
    assert_eq!(hash_text("a"), hash_text("a"));
    assert_ne!(hash_text("a"), hash_text("b"));
    assert_eq!(hash_text("a").len(), 16);
}

#[test]
fn serialize_round_trips_through_parse() {
    let t = parse(V2).unwrap();
    let text = serialize(&t);
    let reparsed = parse(&text).unwrap();
    assert_eq!(t.hub, reparsed.hub);
    assert_eq!(t.hosts, reparsed.hosts);
    assert_eq!(t.monitor, reparsed.monitor);
}

#[test]
fn apply_add_remove_flag_monitor() {
    let t = parse(V2).unwrap();
    let added = apply(&t, &TopologyEdit::AddHost { name: "epsilon".into(), daemon: true, frontend: false }).unwrap();
    assert_eq!(added.host("epsilon").unwrap(), &HostDecl { name: "epsilon".into(), daemon: true, frontend: false });

    let flagged = apply(&t, &TopologyEdit::SetFlag { name: "beta".into(), key: "daemon".into(), value: true }).unwrap();
    assert!(flagged.host("beta").unwrap().daemon);

    let removed = apply(&t, &TopologyEdit::RemoveHost { name: "beta".into() }).unwrap();
    assert!(removed.host("beta").is_none());

    let mon_added = apply(&t, &TopologyEdit::MonitorAdd { label: "extra".into(), target: "extra-box".into() }).unwrap();
    assert!(mon_added.monitor.iter().any(|(l, tgt)| l == "extra" && tgt == "extra-box"));

    let mon_removed = apply(&t, &TopologyEdit::MonitorRemove { label: "beta".into() }).unwrap();
    assert!(!mon_removed.monitor.iter().any(|(l, _)| l == "beta"));
}

#[test]
fn apply_refuses_structural_violations() {
    let t = parse(V2).unwrap();
    assert!(apply(&t, &TopologyEdit::RemoveHost { name: "alpha".into() }).unwrap_err().contains("is the hub"));
    assert!(apply(&t, &TopologyEdit::RemoveHost { name: "nobody".into() }).unwrap_err().contains("is not declared"));
    assert!(apply(&t, &TopologyEdit::AddHost { name: "beta".into(), daemon: false, frontend: false }).unwrap_err().contains("already declared"));
    assert!(apply(&t, &TopologyEdit::SetFlag { name: "nobody".into(), key: "daemon".into(), value: true }).unwrap_err().contains("is not declared"));
    assert!(apply(&t, &TopologyEdit::SetFlag { name: "beta".into(), key: "colour".into(), value: true }).unwrap_err().contains("not an editable flag"));
    assert!(apply(&t, &TopologyEdit::MonitorAdd { label: "alpha".into(), target: "x".into() }).unwrap_err().contains("already declared"));
    assert!(apply(&t, &TopologyEdit::MonitorRemove { label: "nobody".into() }).unwrap_err().contains("is not declared"));
    // Clearing the hub's own `daemon` flag is structurally legal HERE
    // (apply() has no authority/rows knowledge) but the grammar
    // rejects the result — proven by the daemon-side re-validate step,
    // not duplicated in this function.
    let cleared = apply(&t, &TopologyEdit::SetFlag { name: "alpha".into(), key: "daemon".into(), value: false }).unwrap();
    assert!(parse(&serialize(&cleared)).unwrap_err().contains("has `daemon = false`"));
}

#[test]
fn locate_honours_sot_hosts() {
    // Env is process-global; this test only reads the override path.
    std::env::set_var("SOT_HOSTS", "/nowhere/hosts.toml");
    assert_eq!(locate(), Some(PathBuf::from("/nowhere/hosts.toml")));
    std::env::remove_var("SOT_HOSTS");
}
