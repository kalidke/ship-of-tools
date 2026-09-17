//! `sotd status` (topology plan §E, Q5/Q6) — the ONE query that answers
//! "what does the whole system look like right now": declared (the list)
//! fused with observed (asked of every daemon this box can reach).
//!
//! Deliberately a SEPARATE subcommand from `sotd topology status`
//! (`topology_cli.rs`), not a fold-in: that command is a pure, offline read
//! of the declared file — no network I/O, no timeout, no concurrency, safe
//! to run from a shell profile on every login. This one fans out to every
//! reachable daemon at once, each probe under its own hard timeout
//! ([`PROBE_TIMEOUT`]), which needs the daemon's own tokio runtime (`sotd`
//! is already `#[tokio::main]`) — a genuinely different runtime shape, so
//! it earns its own module rather than growing `topology_cli`'s pure-query
//! arm a network stack.
//!
//! Split three ways so the derivation logic (declared list + raw per-host
//! probe results -> what each row says) is exercised WITHOUT a daemon:
//!   - [`Probe`] — one host's raw outcome, the fixture type unit tests
//!     build by hand (an `Up` daemon, an `Unreachable` one, and a host with
//!     no daemon at all — exactly the plan's own worked example).
//!   - [`report`] — pure: `(&Topology, &BTreeMap<String, Probe>) -> Report`.
//!     All the relay-honesty derivation (item 3: a session is live on the
//!     relay iff its bridge client is attached to the HUB's own roster,
//!     never a unit state or a log) lives here.
//!   - [`render_text`] — pure: `&Report -> String`, the table layout.
//! `gather` is the only I/O: one concurrent, timed probe per dialable host.

use std::collections::BTreeMap;
use std::time::Duration;

use sot_protocol::ops::{ClientVersion, WorkspaceListEntry};
use sot_protocol::topology::{self, HostDecl, Topology};

/// Hard per-host bound (item 4): one dead host must never hang the whole
/// command. Generous for a loopback or already-tunnelled socket, short
/// enough that a handful of dead hosts still returns quickly since every
/// probe runs concurrently (`gather`'s `JoinSet`), not in series.
const PROBE_TIMEOUT: Duration = Duration::from_secs(3);

const USAGE: &str = "\
Usage: sotd status [--json]

  One command for the whole system (topology plan §E): the declared list
  fused with what's actually alive right now. For every host this box can
  reach (its own socket; every other DIALABLE host — daemon, not frontend,
  D8 — via the same endpoint `sotd topology plan` resolves) it asks
  version.query (build, host, attached clients) and workspace.list (rows).
  A declared host this box cannot ask is still printed, with the reason —
  never silently omitted. `--json` prints the same facts as one object.

  `sotd topology status` stays the pure, offline, declared-only view.";

/// One host's raw probe outcome — the fixture type for [`report`]'s own
/// unit tests. `Unreachable`'s `String` is the reason shown verbatim
/// (item 1: never omit an unreachable host, never omit WHY). There is no
/// `NotDialable` variant: a `daemon = true` host structurally excluded from
/// this box's dial list (D8) never gets a [`Probe`] at all — `gather` only
/// ever inserts an entry for a host it actually tried — so `report` reads
/// that state straight off the declared flags (no probe for the name) via
/// `host_row`'s own `(true, None)` match arm.
#[derive(Debug, Clone)]
pub enum Probe {
    /// Dialled and it did not answer in time, refused, or answered
    /// something this box couldn't parse.
    Unreachable(String),
    /// Answered both `version.query` and `workspace.list`.
    Up { build: String, reported_host: String, clients: Vec<ClientVersion>, rows: Vec<WorkspaceListEntry> },
}

/// One attached client, grouped by (role, host) with a count — the CLIENTS
/// column's unit (item 2: "each attached client as `role@host`"). Several
/// identical (role, host) connections (e.g. nine `bridge` clients from nine
/// different hosts are NOT grouped together — only true duplicates are)
/// fold into one line with a `×N` count, mirroring `sot-fe version`'s own
/// existing dedup convention.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientGroup {
    pub role: String,
    pub host: String,
    pub count: usize,
    /// True when this group contains the one connection `resolve_active`
    /// (`clients.rs`) marked as the daemon's active frontend.
    pub active: bool,
}

/// DAEMON column for one row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DaemonCell {
    /// No `daemon = true` here — nothing to ask.
    NotDeclared,
    NotDialable,
    Unreachable(String),
    Up { build: String, reported_host: String },
}

/// One fully-derived row — everything [`render_text`] and the `--json`
/// writer both need, so neither re-derives the relay-honesty or grouping
/// rules that live in [`report`] alone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    pub host: String,
    pub declared: String,
    pub daemon: DaemonCell,
    /// `None` when there was no daemon to ask; `Some((total, by_phase,
    /// by_account))` otherwise — both sorted by name for a stable
    /// render; `by_account` carries only non-default rows ([`phase_counts`]'s
    /// own doc).
    pub rows: Option<(usize, Vec<(String, usize)>, Vec<(String, usize)>)>,
    pub clients: Vec<ClientGroup>,
}

/// The whole answer: every declared row (hosts, then monitor-only labels,
/// each list-order preserved) plus the hub's own active frontend (Q5).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Report {
    pub rows: Vec<Row>,
    /// `Some("fe@<host>")` — the hub's own `resolve_active` pick, read off
    /// its roster; `None` when the hub answered but nothing is active, or
    /// the hub itself could not be asked (indistinguishable in text: both
    /// print "none" — see [`render_text`]'s own doc for why that's honest).
    pub active_frontend_of_hub: Option<String>,
    pub hub_reachable: bool,
}

/// Pure derivation: declared list + raw per-host probes -> the full,
/// grouped, relay-honest [`Report`]. No I/O — every fact `render_text` (or
/// a `--json` caller) needs is already decided here.
///
/// Relay honesty (item 3): a host with NO daemon of its own (or one this
/// box can't dial) borrows its CLIENTS from the HUB's own roster, filtered
/// to clients whose declared `host` matches — the hub is the only place a
/// `bridge` connection (a session's comm-relay client) is ever visible, so
/// this is the only way to answer "is X live on the relay" honestly (never
/// a systemd unit state, never a log grep).
pub fn report(topo: &Topology, probes: &BTreeMap<String, Probe>) -> Report {
    let hub_probe = probes.get(&topo.hub);
    let hub_clients: Option<&[ClientVersion]> = match hub_probe {
        Some(Probe::Up { clients, .. }) => Some(clients.as_slice()),
        _ => None,
    };
    let hub_reachable = hub_clients.is_some();
    let active_frontend_of_hub = hub_clients.and_then(|clients| {
        clients.iter().find(|c| c.role == "fe" && c.active).map(|c| format!("fe@{}", c.host.clone().unwrap_or_else(|| "?".to_string())))
    });

    let mut rows = Vec::new();
    for h in &topo.hosts {
        rows.push(host_row(topo, h, probes.get(&h.name), hub_clients));
    }
    for (label, target) in topo.monitor_targets() {
        if topo.host(&label).is_none() && topo.host(&target).is_none() {
            rows.push(Row {
                host: label,
                declared: format!("monitor-only {target}"),
                daemon: DaemonCell::NotDeclared,
                rows: None,
                clients: Vec::new(),
            });
        }
    }
    Report { rows, active_frontend_of_hub, hub_reachable }
}

fn host_row(topo: &Topology, h: &HostDecl, probe: Option<&Probe>, hub_clients: Option<&[ClientVersion]>) -> Row {
    let declared = topology::declared_words(topo, h).join(",");
    let (daemon, rows, own_clients) = match (h.daemon, probe) {
        (false, _) => (DaemonCell::NotDeclared, None, None),
        (true, None) => (DaemonCell::NotDialable, None, None),
        (true, Some(Probe::Unreachable(reason))) => (DaemonCell::Unreachable(reason.clone()), None, None),
        (true, Some(Probe::Up { build, reported_host, clients, rows })) => {
            (DaemonCell::Up { build: build.clone(), reported_host: reported_host.clone() }, Some(phase_counts(rows)), Some(clients.as_slice()))
        }
    };
    // A directly-probed daemon shows its OWN roster; anything else borrows
    // the hub's, filtered to clients that declared THIS host — the only
    // honest source for "is a bridge/frontend for this host live" (item 3).
    let clients = match own_clients {
        Some(c) => group_clients(c.iter()),
        None => match hub_clients {
            Some(c) => group_clients(c.iter().filter(|c| c.host.as_deref() == Some(h.name.as_str()))),
            None => Vec::new(),
        },
    };
    Row { host: h.name.clone(), declared, daemon, rows, clients }
}

fn group_clients<'a>(clients: impl Iterator<Item = &'a ClientVersion>) -> Vec<ClientGroup> {
    let mut groups: BTreeMap<(String, String), (usize, bool)> = BTreeMap::new();
    for c in clients {
        let key = (c.role.clone(), c.host.clone().unwrap_or_else(|| "?".to_string()));
        let entry = groups.entry(key).or_insert((0, false));
        entry.0 += 1;
        entry.1 |= c.active;
    }
    groups.into_iter().map(|((role, host), (count, active))| ClientGroup { role, host, count, active }).collect()
}

/// `(total, by_phase, by_account)` — `by_phase` buckets `WorkspaceListEntry::phase`
/// (lower-cased; a `None` phase — an ordinary `tmux`-runtime row, which
/// carries no supervisor-lane phase at all — buckets as `"tmux"`, never
/// dropped), sorted by phase name so the render is deterministic.
/// `by_account` (accounts brief, v0.6.0) buckets `WorkspaceListEntry::account`
/// the SAME way, but ONLY the non-default rows — an empty `account`
/// (the common case) is never counted at all, so a host with no
/// non-default rows adds nothing to the table (item: "keeping the
/// table compact").
fn phase_counts(rows: &[WorkspaceListEntry]) -> (usize, Vec<(String, usize)>, Vec<(String, usize)>) {
    let mut by_phase: BTreeMap<String, usize> = BTreeMap::new();
    let mut by_account: BTreeMap<String, usize> = BTreeMap::new();
    for r in rows {
        let key = r.phase.clone().unwrap_or_else(|| "tmux".to_string()).to_lowercase();
        *by_phase.entry(key).or_insert(0) += 1;
        if !r.account.is_empty() {
            *by_account.entry(r.account.clone()).or_insert(0) += 1;
        }
    }
    (rows.len(), by_phase.into_iter().collect(), by_account.into_iter().collect())
}

/// Pure: [`Report`] -> the printed table (item 2's columns) plus the final
/// active-frontend-of-hub line. No I/O, no knowledge of how the `Report`
/// was gathered — this is what the fixture-driven unit tests exercise
/// directly, no daemon involved.
pub fn render_text(r: &Report) -> String {
    let mut out = String::from("HOST DECLARED DAEMON ROWS CLIENTS\n");
    for row in &r.rows {
        let daemon = match &row.daemon {
            DaemonCell::NotDeclared => "-".to_string(),
            DaemonCell::NotDialable => "not dialable here".to_string(),
            DaemonCell::Unreachable(reason) => format!("unreachable: {reason}"),
            DaemonCell::Up { build, reported_host } => format!("{build} up (host: {reported_host})"),
        };
        let rows = match &row.rows {
            None => "-".to_string(),
            Some((total, by_phase, by_account)) => {
                let mut cell = if by_phase.is_empty() {
                    total.to_string()
                } else {
                    let detail = by_phase.iter().map(|(p, n)| format!("{p}:{n}")).collect::<Vec<_>>().join(", ");
                    format!("{total} ({detail})")
                };
                // Accounts brief: only non-default rows ever reach
                // `by_account` (`phase_counts`'s own doc) — a host with
                // none adds nothing to the cell.
                if !by_account.is_empty() {
                    let detail = by_account.iter().map(|(a, n)| format!("{a}:{n}")).collect::<Vec<_>>().join(", ");
                    cell.push_str(&format!(" accounts: {detail}"));
                }
                cell
            }
        };
        let clients = if row.clients.is_empty() {
            "-".to_string()
        } else {
            row.clients
                .iter()
                .map(|g| {
                    let n = if g.count > 1 { format!(" \u{d7}{}", g.count) } else { String::new() };
                    let a = if g.active { " ACTIVE" } else { "" };
                    format!("{}@{}{n}{a}", g.role, g.host)
                })
                .collect::<Vec<_>>()
                .join(", ")
        };
        out.push_str(&format!("{} {} {} {} {}\n", row.host, row.declared, daemon, rows, clients));
    }
    let active = match (&r.active_frontend_of_hub, r.hub_reachable) {
        (Some(fe), _) => fe.clone(),
        (None, true) => "none".to_string(),
        (None, false) => "unknown (hub not reachable from here)".to_string(),
    };
    out.push_str(&format!("active frontend of the hub: {active}\n"));
    out
}

/// `--json`: the same [`Report`], as one small object — no separate
/// derivation path, so it can never disagree with the table.
pub fn render_json(r: &Report) -> String {
    #[derive(serde::Serialize)]
    struct JsonRow<'a> {
        host: &'a str,
        declared: &'a str,
        daemon_state: &'static str,
        daemon_build: Option<&'a str>,
        daemon_host: Option<&'a str>,
        daemon_unreachable_reason: Option<&'a str>,
        rows_total: Option<usize>,
        rows_by_phase: &'a [(String, usize)],
        rows_by_account: &'a [(String, usize)],
        clients: Vec<serde_json::Value>,
    }
    let empty: Vec<(String, usize)> = Vec::new();
    let rows: Vec<JsonRow> = r
        .rows
        .iter()
        .map(|row| {
            let (state, build, host, reason) = match &row.daemon {
                DaemonCell::NotDeclared => ("not_declared", None, None, None),
                DaemonCell::NotDialable => ("not_dialable", None, None, None),
                DaemonCell::Unreachable(why) => ("unreachable", None, None, Some(why.as_str())),
                DaemonCell::Up { build, reported_host } => ("up", Some(build.as_str()), Some(reported_host.as_str()), None),
            };
            let (total, by_phase, by_account) =
                row.rows.as_ref().map(|(t, p, a)| (Some(*t), p, a)).unwrap_or((None, &empty, &empty));
            JsonRow {
                host: &row.host,
                declared: &row.declared,
                daemon_state: state,
                daemon_build: build,
                daemon_host: host,
                daemon_unreachable_reason: reason,
                rows_total: total,
                rows_by_phase: by_phase,
                rows_by_account: by_account,
                clients: row
                    .clients
                    .iter()
                    .map(|g| serde_json::json!({"role": g.role, "host": g.host, "count": g.count, "active": g.active}))
                    .collect(),
            }
        })
        .collect();
    serde_json::to_string_pretty(&serde_json::json!({
        "rows": rows,
        "active_frontend_of_hub": r.active_frontend_of_hub,
        "hub_reachable": r.hub_reachable,
    }))
    .expect("Report serializes")
}

// ─── I/O: gather (the only part `report`/`render_text` above never touch) ─

/// One host's probe, run on a blocking thread (the wire client
/// (`topology_dial::dial_and_call`) is plain blocking `std::net`/
/// `std::os::unix::net` I/O, same as every other CLI caller of it) under
/// [`PROBE_TIMEOUT`]. `spawn_blocking`'s own thread is not cancelled on
/// timeout — nothing here awaits it further, so a truly wedged connect can
/// leak one thread until this short-lived process exits, never hanging the
/// command itself (item 4).
async fn probe_one(endpoint: String, self_host: String) -> Probe {
    let ep = endpoint.clone();
    let sh = self_host.clone();
    match tokio::time::timeout(PROBE_TIMEOUT, tokio::task::spawn_blocking(move || probe_blocking(&ep, &sh))).await {
        Ok(Ok(probe)) => probe,
        Ok(Err(_)) => Probe::Unreachable("probe thread panicked".to_string()),
        Err(_) => Probe::Unreachable(format!("no reply within {}s", PROBE_TIMEOUT.as_secs())),
    }
}

fn probe_blocking(endpoint: &str, self_host: &str) -> Probe {
    let v = match crate::topology_dial::dial_and_call(endpoint, self_host, sot_protocol::op::VERSION_QUERY, serde_json::json!({})) {
        Ok(v) => v,
        Err(e) => return Probe::Unreachable(e),
    };
    let res: sot_protocol::ops::VersionQueryRes = match serde_json::from_value(v) {
        Ok(r) => r,
        Err(e) => return Probe::Unreachable(format!("bad version.query reply: {e}")),
    };
    // A daemon that answers version.query but not workspace.list still
    // gets an `Up` row (build/host/clients are real) — rows just read as
    // empty rather than sinking the whole probe to `Unreachable`.
    let rows = crate::topology_dial::dial_and_call(endpoint, self_host, sot_protocol::op::WORKSPACE_LIST, serde_json::json!({}))
        .ok()
        .and_then(|v| serde_json::from_value::<sot_protocol::ops::WorkspaceListRes>(v).ok())
        .map(|r| r.workspaces)
        .unwrap_or_default();
    Probe::Up { build: res.daemon.app_version, reported_host: res.daemon.host, clients: res.clients, rows }
}

/// Every [`topology::dialable_hosts`] entry, probed concurrently (item 4:
/// probes may run concurrently inside the command) via
/// [`topology::dial_endpoints`] — the SAME resolution `sotd topology plan`
/// hands the launcher, so `sotd status` never invents a second notion of
/// "how do I reach this host." A host flagged `daemon` but excluded from
/// that list (D8: `frontend`) is not probed at all — `report` renders it
/// `NotDialable` from the declared flags alone.
async fn gather(topo: &Topology, self_host: &str) -> BTreeMap<String, Probe> {
    let mut set = tokio::task::JoinSet::new();
    for (name, endpoint) in topology::dial_endpoints(topo, self_host) {
        let self_host = self_host.to_string();
        set.spawn(async move { (name, probe_one(endpoint, self_host).await) });
    }
    let mut out = BTreeMap::new();
    while let Some(res) = set.join_next().await {
        if let Ok((name, probe)) = res {
            out.insert(name, probe);
        }
    }
    out
}

/// `sotd status [--json]` — entry point. Async (unlike `topology_cli::run`)
/// because `gather` needs the daemon's own tokio runtime for concurrency.
pub async fn run(args: &[String]) -> i32 {
    if args.iter().any(|a| a == "--help" || a == "-h") {
        println!("{USAGE}");
        return 0;
    }
    let json = args.iter().any(|a| a == "--json");
    let me = match sot_log::state_dir::host_name() {
        Ok(h) => h,
        Err(e) => {
            eprintln!("sotd status: {e}");
            return 2;
        }
    };
    let (_, topo) = match topology::load() {
        Ok(Some(x)) => x,
        Ok(None) => {
            let looked = topology::locate().map(|p| p.display().to_string()).unwrap_or_default();
            eprintln!("sotd status: no hosts.toml at {looked} (run `sotd topology sync --hub <alias>`)");
            return 2;
        }
        Err(e) => {
            eprintln!("sotd status: {e}");
            return 2;
        }
    };
    let probes = gather(&topo, &me).await;
    let rep = report(&topo, &probes);
    if json {
        println!("{}", render_json(&rep));
    } else {
        print!("{}", render_text(&rep));
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;

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
        }
    }

    fn ws(phase: Option<&str>, account: &str) -> WorkspaceListEntry {
        WorkspaceListEntry {
            workspace_id: "ws-1".to_string(),
            slug: "proj".to_string(),
            label: "proj".to_string(),
            project_root: "/tmp/proj".to_string(),
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
            repl_state: String::new(),
            runtime: "tmux".to_string(),
            state_dir: None,
            phase: phase.map(str::to_string),
            activation_error: None,
            account: account.to_string(),
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
            },
        );
        // `laptop` never gets a `Probe` at all: it is `daemon=true` but
        // `frontend=true` too, so it never appears in `dial_endpoints` —
        // `report` must read `NotDialable` purely from the declared flags.
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
        let topo = Topology { hub: "hub-a".to_string(), hosts: vec![host("hub-a", true, false)], monitor: Vec::new() };
        let mut probes = BTreeMap::new();
        probes.insert(
            "hub-a".to_string(),
            Probe::Up {
                build: "0.6.0-rc.29".to_string(),
                reported_host: "hub-a".to_string(),
                clients: Vec::new(),
                rows: vec![ws(Some("READY"), ""), ws(Some("READY"), "team"), ws(Some("READY"), "team")],
            },
        );
        let text = render_text(&report(&topo, &probes));
        assert!(text.contains("3 (ready:3) accounts: team:2"), "{text}");

        let mut default_only_probes = BTreeMap::new();
        default_only_probes.insert(
            "hub-a".to_string(),
            Probe::Up { build: "0.6.0-rc.29".to_string(), reported_host: "hub-a".to_string(), clients: Vec::new(), rows: vec![ws(Some("READY"), ""); 2] },
        );
        let default_only_text = render_text(&report(&topo, &default_only_probes));
        assert!(default_only_text.contains("2 (ready:2) -\n"), "{default_only_text}");
        assert!(!default_only_text.contains("accounts:"), "{default_only_text}");
    }

    #[test]
    fn a_host_with_no_daemon_flag_at_all_is_never_probed_and_renders_a_dash() {
        let topo = Topology { hub: "hub-a".to_string(), hosts: vec![host("hub-a", true, false), host("shell-only", false, false)], monitor: Vec::new() };
        let mut probes = BTreeMap::new();
        probes.insert("hub-a".to_string(), Probe::Up { build: "0.6.0".to_string(), reported_host: "hub-a".to_string(), clients: Vec::new(), rows: Vec::new() });
        let rep = report(&topo, &probes);
        let row = rep.rows.iter().find(|r| r.host == "shell-only").expect("row present");
        assert_eq!(row.daemon, DaemonCell::NotDeclared);
        assert_eq!(row.rows, None);
        assert!(row.clients.is_empty());
    }

    #[test]
    fn an_unreachable_daemon_host_is_printed_never_omitted() {
        let topo = Topology { hub: "hub-a".to_string(), hosts: vec![host("hub-a", true, false), host("dead-box", true, false)], monitor: Vec::new() };
        let mut probes = BTreeMap::new();
        probes.insert("hub-a".to_string(), Probe::Up { build: "0.6.0".to_string(), reported_host: "hub-a".to_string(), clients: Vec::new(), rows: Vec::new() });
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
        probes.insert("hub-a".to_string(), Probe::Up { build: "0.6.0-rc.29".to_string(), reported_host: "hub-a".to_string(), clients: vec![cv("fe", "desktop", true)], rows: vec![ws(None, "")] });
        let rep = report(&topo, &probes);
        let json = render_json(&rep);
        let v: serde_json::Value = serde_json::from_str(&json).expect("valid json");
        assert_eq!(v["active_frontend_of_hub"], "fe@desktop");
        assert_eq!(v["hub_reachable"], true);
        let hub_row = v["rows"].as_array().unwrap().iter().find(|r| r["host"] == "hub-a").unwrap();
        assert_eq!(hub_row["daemon_state"], "up");
        assert_eq!(hub_row["rows_total"], 1);
    }
}
