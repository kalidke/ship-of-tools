use super::*;

/// A probe is bounded as a whole by `PROBE_TIMEOUT`, its two dials together: a daemon that answers
/// `version.query` after 2 s and never answers `workspace.list` ends the probe within the one 3 s bound, an `Up`
/// row with no rows.
#[cfg(unix)]
#[test]
fn a_probe_ends_within_its_one_bound() {
    use std::io::{BufRead, Write};
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("p.sock");
    let listener = std::os::unix::net::UnixListener::bind(&path).unwrap();
    let _daemon = std::thread::spawn(move || {
        let mut held = Vec::new();
        for conn in listener.incoming().take(2) {
            let mut conn = conn.unwrap();
            let mut reader = std::io::BufReader::new(conn.try_clone().unwrap());
            let mut answer = |frame: sot_protocol::Frame| {
                let mut line = serde_json::to_vec(&frame).unwrap();
                line.push(b'\n');
                conn.write_all(&line).unwrap();
            };
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            answer(sot_protocol::Frame::res(0, sot_protocol::op::HELLO, serde_json::json!({})));
            line.clear();
            reader.read_line(&mut line).unwrap();
            if line.contains(sot_protocol::op::VERSION_QUERY) {
                std::thread::sleep(Duration::from_secs(2));
                let daemon = serde_json::json!({ "app_version": "probe", "protocol": 4, "lane_build": "probe", "host": "probe-host" });
                answer(sot_protocol::Frame::res(1, sot_protocol::op::VERSION_QUERY, serde_json::json!({ "daemon": daemon, "clients": [] })));
            }
            held.push(reader);
        }
        std::thread::sleep(Duration::from_secs(20));
    });
    let began = std::time::Instant::now();
    let probe = probe_blocking(&format!("unix:{}", path.display()), "probe-host");
    let took = began.elapsed();
    println!("probe-bound: {probe:?} after {took:?}");
    assert!(matches!(&probe, Probe::Up { rows, .. } if rows.is_empty()), "{probe:?}");
    assert!(took < Duration::from_millis(3500), "the probe took {took:?}, past its one {PROBE_TIMEOUT:?} bound");
}

fn host(name: &str, daemon: bool, frontend: bool) -> HostDecl {
    HostDecl { name: name.to_string(), daemon, frontend }
}

fn cv(role: &str, host: &str, active: bool) -> ClientVersion {
    ClientVersion {
        client_id: format!("{role}-{host}"),
        app_version: "0.6.0".to_string(),
        protocol: sot_protocol::PROTOCOL_VERSION,
        host: Some(host.to_string()),
        role: role.to_string(),
        instance: None,
        name: Some(format!("{role}@{host}")),
        active,
        sessions: None,
    }
}

fn ws(phase: Option<&str>, account: &str) -> WorkspaceListEntry {
    WorkspaceListEntry {
        workspace_id: "ws-1".to_string(),
        slug: "proj".to_string(),
        label: "proj".to_string(),
        project_root: "/tmp/proj".to_string(),
        canonical_root: None,
        session_name: "sot-be-proj".to_string(),
        kernel_running: false,
        is_default: true,
        autostart_claude: false,
        agent: "none".to_string(),
        agent_name: String::new(),
        agent_handle: String::new(),
        task: String::new(),
        agent_state: String::new(),
        agent_summary: String::new(),
        agent_status_at: String::new(),
        active_at: "2026-01-01T00:00:00Z".to_string(),
        repl_state: String::new(),
        runtime: "tmux".to_string(),
        state_dir: None,
        phase: phase.map(str::to_string),
        activation_error: None,
        account: account.to_string(),
        not_woken_by_mail: false,
    }
}

/// The plan's own worked example (§E): a hub answering directly, a
/// shell-only sampled host with no daemon at all (its CLIENTS borrowed
/// from the hub's bridge roster), and a `frontend`+`daemon` host this
/// box structurally can't dial (D8) whose CLIENTS instead show it
/// attached to the hub as an `fe`. No daemon involved — every input is
/// a hand-built fixture.
fn example_topology() -> Topology {
    Topology {
        hub: "hub-a".to_string(),
        hosts: vec![host("hub-a", true, false), host("server-a", false, false), host("laptop", true, true)],
        monitor: vec![("server-a".to_string(), String::new())],
        warnings: Vec::new(),
    }
}

#[test]
fn renders_the_plans_worked_example_declared_and_derived_columns() {
    let topo = example_topology();
    let mut probes = BTreeMap::new();
    probes.insert(
        "hub-a".to_string(),
        Probe::Up {
            build: "0.6.0-rc.29".to_string(),
            reported_host: "hub-a".to_string(),
            clients: vec![cv("fe", "desktop", true), cv("fe", "laptop", false), cv("bridge", "server-a", false), cv("bridge", "server-a", false)],
            rows: vec![ws(Some("READY"), ""); 3],
            update: None,
        },
    );
    // `laptop` never gets a `Probe` at all — this fixture simply omits
    // one, as `gather` does for a declared-daemon host it has no
    // endpoint for (`frontend=true` is NOT such a case any more:
    // `dialable_hosts` is `daemon` alone). `report` must read
    // `NotDialable` purely from the declared flags plus that absence.
    let rep = report(&topo, &probes);
    let text = render_text(&rep);

    assert!(text.contains("hub-a hub,daemon 0.6.0-rc.29 up (host: hub-a) 3 (ready:3)"), "{text}");
    assert!(text.contains("server-a shell,sampled - -"), "{text}");
    assert!(text.contains("bridge@server-a \u{d7}2"), "server-a should show the hub's bridge count: {text}");
    assert!(text.contains("laptop daemon,frontend not dialable here - "), "{text}");
    assert!(text.contains("fe@laptop"), "laptop should show attached-to-hub-as-fe from the hub's own roster: {text}");
    assert!(text.contains("fe@desktop \u{d7}1 ACTIVE") || text.contains("fe@desktop ACTIVE"), "the active fe must be marked: {text}");
    assert!(text.ends_with("active frontend of the hub: fe@desktop\n"), "{text}");
}

/// Accounts brief (v0.6.0): the ROWS cell names a non-default
/// account, but a table with only default-account rows stays exactly
/// as compact as it always has (no trailing "accounts: " noise at
/// all — never an empty parenthetical).
#[test]
fn rows_cell_names_a_non_default_account_but_stays_compact_without_one() {
    let topo = Topology { hub: "hub-a".to_string(), hosts: vec![host("hub-a", true, false)], monitor: Vec::new(), warnings: Vec::new() };
    let mut probes = BTreeMap::new();
    probes.insert(
        "hub-a".to_string(),
        Probe::Up {
            build: "0.6.0-rc.29".to_string(),
            reported_host: "hub-a".to_string(),
            clients: Vec::new(),
            rows: vec![ws(Some("READY"), ""), ws(Some("READY"), "team"), ws(Some("READY"), "team")],
            update: None,
        },
    );
    let text = render_text(&report(&topo, &probes));
    assert!(text.contains("3 (ready:3) accounts: team:2"), "{text}");

    let mut default_only_probes = BTreeMap::new();
    default_only_probes.insert(
        "hub-a".to_string(),
        Probe::Up { build: "0.6.0-rc.29".to_string(), reported_host: "hub-a".to_string(), clients: Vec::new(), rows: vec![ws(Some("READY"), ""); 2], update: None },
    );
    let default_only_text = render_text(&report(&topo, &default_only_probes));
    assert!(default_only_text.contains("2 (ready:2) - not reported\n"), "{default_only_text}");
    assert!(!default_only_text.contains("accounts:"), "{default_only_text}");
}

#[test]
fn a_host_with_no_daemon_flag_at_all_is_never_probed_and_renders_a_dash() {
    let topo = Topology { hub: "hub-a".to_string(), hosts: vec![host("hub-a", true, false), host("shell-only", false, false)], monitor: Vec::new(), warnings: Vec::new() };
    let mut probes = BTreeMap::new();
    probes.insert("hub-a".to_string(), Probe::Up { build: "0.6.0".to_string(), reported_host: "hub-a".to_string(), clients: Vec::new(), rows: Vec::new(), update: None });
    let rep = report(&topo, &probes);
    let row = rep.rows.iter().find(|r| r.host == "shell-only").expect("row present");
    assert_eq!(row.daemon, DaemonCell::NotDeclared);
    assert_eq!(row.rows, None);
    assert!(row.clients.is_empty());
}

#[test]
fn an_unreachable_daemon_host_is_printed_never_omitted() {
    let topo = Topology { hub: "hub-a".to_string(), hosts: vec![host("hub-a", true, false), host("dead-box", true, false)], monitor: Vec::new(), warnings: Vec::new() };
    let mut probes = BTreeMap::new();
    probes.insert("hub-a".to_string(), Probe::Up { build: "0.6.0".to_string(), reported_host: "hub-a".to_string(), clients: Vec::new(), rows: Vec::new(), update: None });
    probes.insert("dead-box".to_string(), Probe::Unreachable("connection refused".to_string()));
    let rep = report(&topo, &probes);
    let text = render_text(&rep);
    assert!(text.contains("dead-box daemon unreachable: connection refused"), "{text}");
}

#[test]
fn hub_unreachable_makes_active_frontend_honestly_unknown_not_none() {
    let topo = example_topology();
    let probes: BTreeMap<String, Probe> = BTreeMap::new(); // nothing answered
    let rep = report(&topo, &probes);
    assert!(!rep.hub_reachable);
    assert_eq!(rep.active_frontend_of_hub, None);
    assert!(render_text(&rep).ends_with("active frontend of the hub: unknown (hub not reachable from here)\n"));
}

#[test]
fn json_round_trips_the_same_facts_as_the_text_table() {
    let topo = example_topology();
    let mut probes = BTreeMap::new();
    probes.insert("hub-a".to_string(), Probe::Up { build: "0.6.0-rc.29".to_string(), reported_host: "hub-a".to_string(), clients: vec![cv("fe", "desktop", true)], rows: vec![ws(None, "")], update: None });
    let rep = report(&topo, &probes);
    let json = render_json(&rep);
    let v: serde_json::Value = serde_json::from_str(&json).expect("valid json");
    assert_eq!(v["active_frontend_of_hub"], "fe@desktop");
    assert_eq!(v["hub_reachable"], true);
    let hub_row = v["rows"].as_array().unwrap().iter().find(|r| r["host"] == "hub-a").unwrap();
    assert_eq!(hub_row["daemon_state"], "up");
    assert_eq!(hub_row["rows_total"], 1);
}

/// Each row of the table ends with what its daemon says of its own update, in the wire's one text; a daemon that
/// answered without one (a daemon built before it told one) reads `not reported`, and a row whose daemon was not asked or did not
/// answer has a dash. The five columns before it and the last line stay where they were, and `--json` carries the
/// same state as this sotd read it. A state its daemon could not read from disk just now says so, and a reason that
/// holds a newline or an escape sequence adds no row and moves no cursor.
#[test]
fn each_row_ends_with_its_update_state_and_an_older_daemon_reads_not_reported() {
    use sot_protocol::ops::UpdatePhase;
    let topo = Topology {
        hub: "hub-a".to_string(),
        hosts: vec![host("hub-a", true, false), host("old-box", true, false), host("dead-box", true, false), host("server-a", false, false), host("laptop", true, true)],
        monitor: vec![("gpu".to_string(), "other@gpu-box".to_string())],
        warnings: Vec::new(),
    };
    let ready = UpdateState {
        version: "0.7.4".to_string(),
        detail: "applies when that computer's sotd.service next starts (a start of Ship of Tools there, or a restart of the service)".to_string(),
        checked_s: Some(3 * 3600),
        ..UpdateState::of(UpdatePhase::Ready)
    };
    let up = |host: &str, update: Option<UpdateState>| Probe::Up { build: "0.7.3".to_string(), reported_host: host.to_string(), clients: Vec::new(), rows: Vec::new(), update };
    let mut probes = BTreeMap::new();
    probes.insert("hub-a".to_string(), up("hub-a", Some(ready.clone())));
    probes.insert("old-box".to_string(), up("old-box", None));
    probes.insert("dead-box".to_string(), Probe::Unreachable("connection refused".to_string()));
    let rep = report(&topo, &probes);

    let text = render_text(&rep);
    let lines: Vec<&str> = text.lines().collect();
    let want = [
        "HOST DECLARED DAEMON ROWS CLIENTS UPDATE",
        "hub-a hub,daemon 0.7.3 up (host: hub-a) 0 - 0.7.4 ready: applies when that computer's sotd.service next starts (a start of Ship of Tools there, or a restart of the service)",
        "old-box daemon 0.7.3 up (host: old-box) 0 - not reported",
        "dead-box daemon unreachable: connection refused - - -",
        "server-a shell - - - -",
        "laptop daemon,frontend not dialable here - - -",
        "gpu monitor-only other@gpu-box - - - -",
        "active frontend of the hub: none",
    ];
    assert_eq!(lines, want, "{text}");

    // An up-to-date daemon says when it was checked, and one with checks off says why.
    let cell = |state: UpdateState| {
        let mut probes = BTreeMap::new();
        probes.insert("hub-a".to_string(), up("hub-a", Some(state)));
        render_text(&report(&topo, &probes)).lines().nth(1).unwrap().to_string()
    };
    assert!(cell(UpdateState { checked_s: Some(3 * 3600), ..UpdateState::of(UpdatePhase::None) }).ends_with(" - up to date (checked 3 h ago)"));
    assert!(cell(UpdateState { detail: "dev build".to_string(), ..UpdateState::of(UpdatePhase::Off) }).ends_with(" - off: dev build"));
    assert!(cell(UpdateState { unread: true, ..UpdateState::of(UpdatePhase::Unchecked) }).ends_with(" - not checked yet; as last read, the disk did not answer in time"));

    // A reason as a file or a child's output can hold it, read from the wire as a probe reads it: one row, no control byte.
    let hostile: UpdateState = serde_json::from_value(serde_json::json!({
        "phase": "not_applied", "version": "9.9.9",
        "detail": "armed over 9.9.7\nforged-host hub,daemon 9.9.9 up (host: forged) 0 - up to date \u{1b}[2K\u{1b}[31mRED; it no longer fits",
    }))
    .unwrap();
    let mut probes = BTreeMap::new();
    probes.insert("hub-a".to_string(), up("hub-a", Some(hostile)));
    let table = render_text(&report(&topo, &probes));
    assert_eq!(table.lines().count(), want.len(), "a reason added a row: {table}");
    assert!(!table.chars().any(|c| c.is_control() && c != '\n'), "a control character reached the table: {table:?}");
    assert!(table.lines().nth(1).unwrap().ends_with("9.9.9 not applied: armed over 9.9.7 forged-host hub,daemon 9.9.9 up (host: forged) 0 - up to date  [2K [31mRED; it no longer fits"), "{table}");

    let v: serde_json::Value = serde_json::from_str(&render_json(&rep)).expect("valid json");
    let row = |host: &str| v["rows"].as_array().unwrap().iter().find(|r| r["host"] == host).unwrap().clone();
    assert_eq!(row("hub-a")["daemon_update"], serde_json::to_value(&ready).unwrap(), "the state as this sotd read it");
    assert_eq!(row("hub-a")["daemon_update"]["phase"], "ready");
    for none in ["old-box", "dead-box", "server-a", "laptop", "gpu"] {
        assert!(row(none)["daemon_update"].is_null(), "{none}: {}", row(none));
    }
}
