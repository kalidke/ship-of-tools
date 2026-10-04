//! The declared topology: `hosts.toml`, grammar v2 — ONE parser, ONE
//! search rule, shared by the daemon (the `[monitor]` sampling list), the
//! `sotd topology` CLI (what the launcher consumes) and, later, the
//! frontend's dial list.
//!
//! ```toml
//! hub = "<host>"        # exactly one: the relay daemon; invariant: one handle namespace
//! [host.<name>]         # <name> == host_name() there == its ssh alias on every box
//! daemon   = true       # runs sotd: frontends dial it, it owns rows (default false)
//! frontend = true       # runs a frontend+launcher: dials the hub, never dialled (default false)
//! [monitor]             # sampling targets (ADR 0020) — NOT hosts; any box, user, or OS
//! <label> = "<ssh target>"
//! ```
//!
//! Rules: exactly one `hub`, and it names a listed host; a section key is a
//! plain host name (`[a-z0-9][a-z0-9._-]*`, i.e. what `host_name()` yields);
//! an unknown TOP-LEVEL key or section is an error naming it — this is also
//! the schema check: an old-grammar file (`default_host` at top level)
//! fails on its first unknown key, naming the line. An unknown key INSIDE
//! `[host.<name>]` (the old `ssh_alias`/`remote_repo`/`tcp_port`/
//! `remote_socket`/`socket`/`remote_home` shape included) is instead
//! collected as a WARNING, not fatal — the host still parses with its known
//! keys applied, and `sotd topology status` prints the warning, naming the
//! host and the key. This is forward compatibility: the hub's canonical
//! file can grow a per-host key an older box's build does not know yet
//! without that box losing its whole topology (a rollout, not a fleet
//! outage). A key given twice in a section is still an error; a hub must be
//! a `daemon` host.
//!
//! **Search order** (the only one): `$SOT_HOSTS` when set (tests, scratch
//! daemons), else `<config dir>/hosts.toml` where the config dir is
//! `sot_log::state_dir::sot_config_dir` (`~/.config/sot`,
//! `%LOCALAPPDATA%\sot\config`). No repo-local layer: the hub's own copy is
//! canonical and every other box's copy is a `topology sync` fetch of it.
//!
//! The TOML subset accepted is exactly the grammar above: `[section]`
//! headers, `key = "string" | true | false | <integer>` lines, `#` comments
//! (whole-line or trailing). Nothing else is needed and nothing else parses,
//! so the reader is ~100 lines and pulls in no TOML crate.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

pub mod endpoint;
pub mod lane_client;
pub mod relay_units;
pub mod ssh_bridge;

#[cfg(test)]
mod tests;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostDecl {
    pub name: String,
    pub daemon: bool,
    pub frontend: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Topology {
    pub hub: String,
    /// In file order — the ordinal port series depends on it.
    pub hosts: Vec<HostDecl>,
    /// `(label, ssh target)`; an empty target means "the label".
    pub monitor: Vec<(String, String)>,
    /// Unknown keys seen inside a `[host.<name>]` block, one message per
    /// key, in file order. Collected rather than fatal — forward
    /// compatibility (module doc) — and empty on a file with none.
    /// `sotd topology status` is where these surface; never `plan`, whose
    /// output both launchers parse line-by-line.
    pub warnings: Vec<String>,
}

impl Topology {
    pub fn host(&self, name: &str) -> Option<&HostDecl> {
        self.hosts.iter().find(|h| h.name == name)
    }

    /// Sampling targets, resolved: `[monitor]` labels with an empty target
    /// take the label itself as the ssh target.
    pub fn monitor_targets(&self) -> Vec<(String, String)> {
        self.monitor
            .iter()
            .map(|(l, t)| (l.clone(), if t.is_empty() { l.clone() } else { t.clone() }))
            .collect()
    }
}

/// Where the file is looked for: `$SOT_HOSTS`, else the config dir. The one
/// search order — documented in the module doc, implemented here only.
pub fn locate() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("SOT_HOSTS").filter(|v| !v.is_empty()) {
        return Some(PathBuf::from(p));
    }
    sot_log::state_dir::sot_config_dir().map(|d| d.join("hosts.toml"))
}

/// Read and parse the located file. `Ok(None)` when there is no file (the
/// ordinary state on a box that never declared a topology); `Err` names the
/// path and the offender for an unreadable or invalid one.
pub fn load() -> Result<Option<(PathBuf, Topology)>, String> {
    let Some(path) = locate() else { return Ok(None) };
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("{}: {e}", path.display())),
    };
    parse(&text)
        .map(|t| Some((path.clone(), t)))
        .map_err(|e| format!("{}: {e}", path.display()))
}

/// Parse grammar v2 (module doc). Duplicate keys in a section (a
/// `[monitor]` label included — two labels would spawn two samplers) are
/// errors, not last-wins.
pub fn parse(text: &str) -> Result<Topology, String> {
    #[derive(PartialEq)]
    enum Section {
        Top,
        Host(usize),
        Monitor,
    }
    let mut hub: Option<String> = None;
    let mut hosts: Vec<HostDecl> = Vec::new();
    let mut monitor: Vec<(String, String)> = Vec::new();
    let mut warnings: Vec<String> = Vec::new();
    let mut section = Section::Top;
    let mut seen: Vec<String> = Vec::new();

    // A UTF-8 byte-order mark: PowerShell's `Set-Content`/`Out-File` writes
    // one and `trim` does not strip U+FEFF, so the first key of such a file
    // never matched (the 2026-09-08 monitor-drawer incident).
    let text = text.trim_start_matches('\u{feff}');
    for (i, raw) in text.lines().enumerate() {
        let n = i + 1;
        let line = strip_comment(raw).trim();
        if line.is_empty() {
            continue;
        }
        if let Some(inner) = line.strip_prefix('[') {
            seen.clear();
            let Some(name) = inner.strip_suffix(']') else {
                return Err(format!("line {n}: malformed section header `{line}`"));
            };
            let name = name.trim();
            section = if name == "monitor" {
                Section::Monitor
            } else if let Some(h) = name.strip_prefix("host.") {
                let h = h.trim();
                if !endpoint::is_plain_host_name(h) {
                    return Err(format!(
                        "line {n}: `[host.{h}]` is not a plain host name (expected [a-z0-9][a-z0-9._-]*)"
                    ));
                }
                if hosts.iter().any(|x| x.name == h) {
                    return Err(format!("line {n}: host `{h}` declared twice"));
                }
                hosts.push(HostDecl { name: h.to_string(), daemon: false, frontend: false });
                Section::Host(hosts.len() - 1)
            } else {
                return Err(format!("line {n}: unknown section `[{name}]` (expected [host.<name>] or [monitor])"));
            };
            continue;
        }
        let Some((k, v)) = line.split_once('=') else {
            return Err(format!("line {n}: expected `key = value`, got `{line}`"));
        };
        let (key, val) = (k.trim(), Value::parse(v.trim()).ok_or_else(|| format!("line {n}: bad value for `{}`: {}", k.trim(), v.trim()))?);
        if seen.iter().any(|s| s == key) {
            return Err(format!("line {n}: `{key}` given twice in this section"));
        }
        seen.push(key.to_string());
        match section {
            Section::Top => match key {
                "hub" => {
                    let name = val.string().ok_or_else(|| format!("line {n}: `{key}` must be a quoted host name"))?;
                    if hub.is_some() {
                        return Err(format!("line {n}: `hub` declared twice (exactly one hub)"));
                    }
                    hub = Some(name.to_string());
                }
                other => return Err(format!("line {n}: unknown key `{other}`")),
            },
            Section::Host(idx) => {
                let host = &mut hosts[idx];
                match key {
                    "daemon" | "frontend" => {
                        let b = val.bool().ok_or_else(|| format!("line {n}: `{key}` must be true or false"))?;
                        if key == "daemon" { host.daemon = b } else { host.frontend = b }
                    }
                    // A per-host key stays unknown to THIS build without
                    // costing the box its whole topology — collected, not
                    // fatal (module doc: forward compatibility).
                    other => warnings.push(format!("line {n}: unknown key `{other}` in [host.{}] — ignored by this build", host.name)),
                }
            }
            Section::Monitor => {
                let target = val.string().ok_or_else(|| format!("line {n}: `{key}` must be a quoted ssh target"))?;
                monitor.push((key.to_string(), target.to_string()));
            }
        }
    }

    let hub = hub.ok_or_else(|| "no `hub = \"<host>\"` declared (exactly one hub)".to_string())?;
    let Some(hub_host) = hosts.iter().find(|h| h.name == hub) else {
        return Err(format!("hub `{hub}` is not a listed host (add `[host.{hub}]`)"));
    };
    if !hub_host.daemon {
        return Err(format!("hub `{hub}` has `daemon = false`; the hub runs the relay daemon, so `[host.{hub}]` needs `daemon = true`"));
    }
    Ok(Topology { hub, hosts, monitor, warnings })
}

enum Value<'a> {
    Str(&'a str),
    Bool(bool),
    Int,
}

impl<'a> Value<'a> {
    fn parse(v: &'a str) -> Option<Self> {
        if let Some(inner) = v.strip_prefix('"').and_then(|s| s.strip_suffix('"')) {
            return (!inner.contains('"')).then_some(Value::Str(inner));
        }
        match v {
            "true" => Some(Value::Bool(true)),
            "false" => Some(Value::Bool(false)),
            _ if !v.is_empty() && v.bytes().all(|b| b.is_ascii_digit()) => Some(Value::Int),
            _ => None,
        }
    }
    fn string(&self) -> Option<&'a str> {
        match self { Value::Str(s) => Some(s), _ => None }
    }
    fn bool(&self) -> Option<bool> {
        match self { Value::Bool(b) => Some(*b), _ => None }
    }
}

/// Cut a `#` comment outside quotes. Sound because the grammar has no
/// escapes inside strings.
fn strip_comment(raw: &str) -> &str {
    let mut quoted = false;
    for (i, c) in raw.char_indices() {
        match c {
            '"' => quoted = !quoted,
            '#' if !quoted => return &raw[..i],
            _ => {}
        }
    }
    raw
}


// ─── What a box derives from the list ────────────────────────────────────


/// The relay endpoint for `self_host` (plan §C): the hub's own socket on
/// the hub; on a `frontend` box an ssh child into the hub, which runs
/// `sotd stdio-bridge` with no argument there (`ssh:<hub>` — C1's
/// no-argument form always means THAT box's own daemon, so naming the hub
/// as the ssh target is enough); everywhere else the hub's reverse tunnel,
/// which lands on `<runtime dir>/sot-relay.sock`
/// (`/run/user/<uid>/sot-relay.sock` — the path `sot-relay-tunnel@` has
/// always used). Path derivations are this box's own, so ask on the box in
/// question.
pub fn relay_endpoint(topo: &Topology, self_host: &str) -> Result<String, String> {
    let host = topo.host(self_host).ok_or_else(|| format!("`{self_host}` is not a listed host"))?;
    Ok(if self_host == topo.hub {
        endpoint::local_endpoint()
    } else if host.frontend {
        format!("ssh:{}", topo.hub)
    } else {
        format!("unix:{}", runtime_relay_dir().join("sot-relay.sock").display())
    })
}

/// The directory the hub's sockets live in: the runtime dir's PARENT
/// (`/run/user/<uid>`), not the `sot/` subdirectory under it — the place
/// `sot-relay-tunnel@` has always landed `sot-relay.sock`, and now also
/// the hub's per-host sockets ([`relay_socket_path`]).
fn runtime_relay_dir() -> PathBuf {
    let run = crate::runtime_sot_dir();
    run.parent().map(PathBuf::from).unwrap_or(run)
}

/// The hub's own socket for `host` — `<runtime base>/sot-host-<host>.sock`,
/// a sibling of the comm relay's `sot-relay.sock`. The hub binds one per
/// host it serves ([`relay_units::relay_hosts`], unit [`relay_units::relay_unit`]) and every peer
/// forwards to it; the last inch beyond it is a `sotd stdio-bridge` on the
/// box that owns the endpoint, so no caller ever learns that box's
/// endpoint shape. A host name is already restricted to the characters a
/// filename wants (`is_plain_host_name`), so the name IS the slug.
///
/// Two things this path is NOT. It is not derivable off the hub — like
/// every path here it belongs to the box that owns it, so a peer asks the
/// hub for it (`sotd topology relay-sockets`) exactly as it already asks
/// for the hub's own socket. And it is not a liveness claim: systemd
/// creates the socket whether or not the box behind it is up, and only a
/// completed hello ever proves the latter.
pub fn relay_socket_path(host: &str) -> PathBuf {
    runtime_relay_dir().join(format!("sot-host-{host}.sock"))
}

/// `sotd topology plan --self <host>` — plain lines, one fact per line,
/// stable order. Every line is `<word> <word> <rest of line>`: a reader
/// splits on the first one or two spaces only, because the rest may itself
/// contain spaces (a Windows pipe path carries the username verbatim). A
/// reader must ignore lines whose first word it does not know, so a later
/// release can add facts without breaking an older launcher. Documented
/// here and nowhere else:
///
/// ```text
/// self <host>
/// hub <host>
/// relay-endpoint <endpoint>          # what SOT_RELAY_ENDPOINT is on this box
/// dial <host> <endpoint>             # one per dialable host (every box declaring
///                                    # `daemon`): this box's own socket for itself;
///                                    # ON THE HUB, the hub's own relay socket for a
///                                    # remote host (unix:<path>, no ssh needed — it
///                                    # is right there); ssh:<hub> for the hub itself
///                                    # and ssh:<hub>/<host> for any other host,
///                                    # everywhere else — an ssh child into the hub
///                                    # runs `sotd stdio-bridge [--host <host>]` there.
/// ```
pub fn plan(topo: &Topology, self_host: &str) -> Result<String, String> {
    let relay = relay_endpoint(topo, self_host)?;
    let mut out = format!("self {self_host}\nhub {}\nrelay-endpoint {relay}\n", topo.hub);
    for (name, endpoint) in dial_endpoints(topo, self_host) {
        out.push_str(&format!("dial {name} {endpoint}\n"));
    }
    Ok(out)
}

/// Every "dialable" host — every box declaring `daemon` — in file order.
///
/// This was `daemon && !frontend` (D8), on the assumption that a box
/// running a frontend is a personal machine whose rows are nobody else's
/// business, and which nothing could reach anyway without ssh access to
/// it. Both halves of that assumption are retired. A box can run a
/// frontend AND host rows others want — an instrument machine with a
/// screen on it is the shape that forced this — and reaching one now
/// costs ssh access from the HUB only: a peer forwards to the hub's
/// socket for that host ([`relay_socket_path`]), never to the host. So
/// `daemon` means what it says: this box runs a daemon, so it can be
/// dialled, screen or no screen.
///
/// [`relay_units::tunnel_hosts`] is a SEPARATE list and still skips `frontend` boxes —
/// the comm relay's reverse tunnels are not this.
pub fn dialable_hosts(topo: &Topology) -> impl Iterator<Item = &HostDecl> {
    topo.hosts.iter().filter(|h| h.daemon)
}

/// `(host, endpoint)` for every [`dialable_hosts`] entry, resolved for
/// `self_host`: its own local socket for itself; on the hub, the hub's own
/// socket for a remote host ([`relay_socket_path`] — no forward needed,
/// it is right there); everywhere else `ssh:<hub>` for the hub itself
/// (an ssh child there runs `sotd stdio-bridge` with no argument — its own
/// daemon, which IS the hub's) and `ssh:<hub>/<host>` for any other host
/// (the ssh child's `--host <host>` reaches the hub's relay socket for it).
/// This is `plan`'s own `dial` line resolution, factored out so a caller
/// that wants it as DATA — `sotd status` (topology plan §E), which
/// actually dials each entry rather than printing it — shares the
/// identical mapping instead of re-deriving it or re-parsing `plan`'s
/// text.
pub fn dial_endpoints(topo: &Topology, self_host: &str) -> Vec<(String, String)> {
    dialable_hosts(topo)
        .map(|h| {
            let endpoint = if h.name == self_host {
                endpoint::local_endpoint()
            } else if self_host == topo.hub {
                format!("unix:{}", relay_socket_path(&h.name).display())
            } else if h.name == topo.hub {
                format!("ssh:{}", topo.hub)
            } else {
                format!("ssh:{}/{}", topo.hub, h.name)
            };
            (h.name.clone(), endpoint)
        })
        .collect()
}

/// FNV-1a 64-bit hash of the file's exact bytes, lowercase zero-padded
/// hex. Same algorithm as the backend's `file_io::content_version`
/// (deterministic, dependency-free, stable across rebuilds) — redefined
/// here rather than imported because `sot-backend` depends on
/// `sot-protocol`, never the reverse, and both the daemon (`topology.set`'s
/// broadcast, `version.query`'s `hosts_toml_hash`) and the CLI
/// (`topology status`'s "cache diverged" line) need the identical hash of
/// the identical bytes to ever agree.
pub fn hash_text(text: &str) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in text.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{h:016x}")
}

/// Write `topo` back out in canonical grammar-v2 form: `hub`, then one
/// `[host.<name>]` per host (only the `true` flags, in file order), then
/// `[monitor]` if non-empty. This is a CLEAN rewrite, not a lossless
/// editor — comments and exact key order in the original file are not
/// preserved (plan §B "Editing the master list" allows this: "if you
/// cannot, say so ... and write a clean canonical file").
pub fn serialize(topo: &Topology) -> String {
    let mut out = format!("hub = \"{}\"\n\n", topo.hub);
    for h in &topo.hosts {
        out.push_str(&format!("[host.{}]\n", h.name));
        if h.daemon {
            out.push_str("daemon = true\n");
        }
        if h.frontend {
            out.push_str("frontend = true\n");
        }
        out.push('\n');
    }
    if !topo.monitor.is_empty() {
        out.push_str("[monitor]\n");
        for (label, target) in &topo.monitor {
            out.push_str(&format!("{label} = \"{target}\"\n"));
        }
    }
    out
}

/// One edit `topology.set` (plan §B) carries: add a host, remove one, flip
/// one of its two boolean flags, or add/remove a `[monitor]` label.
/// `#[serde(tag = "kind")]` so the wire shape is self-describing
/// (`{"kind": "add_host", ...}`) and a CLI can build one from a few plain
/// words (`sotd topology set add <host>`, `remove <host>`, `flag <host>
/// daemon|frontend true|false`, `monitor-add <label> <target>`,
/// `monitor-remove <label>`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TopologyEdit {
    AddHost {
        name: String,
        #[serde(default)]
        daemon: bool,
        #[serde(default)]
        frontend: bool,
    },
    RemoveHost {
        name: String,
    },
    /// `key` is `"daemon"` or `"frontend"` — the only two editable flags.
    SetFlag {
        name: String,
        key: String,
        value: bool,
    },
    MonitorAdd {
        label: String,
        target: String,
    },
    MonitorRemove {
        label: String,
    },
}

/// Apply one edit to `topo`, producing a candidate. Structural checks only
/// — a plain host name, no duplicate host/label, the edited host/label
/// must exist, and the hub is never removed here (the hub daemon does the
/// authority/self/running-rows checks the CLI cannot see, then re-validates
/// the result through [`parse`]`(`[`serialize`]`(candidate))` — the SAME
/// grammar every other write goes through, which is what catches "cleared
/// `daemon` on the hub" generically instead of this function special-casing
/// it).
pub fn apply(topo: &Topology, edit: &TopologyEdit) -> Result<Topology, String> {
    let mut t = topo.clone();
    match edit {
        TopologyEdit::AddHost { name, daemon, frontend } => {
            if !endpoint::is_plain_host_name(name) {
                return Err(format!("`{name}` is not a plain host name (expected [a-z0-9][a-z0-9._-]*)"));
            }
            if t.hosts.iter().any(|h| h.name == *name) {
                return Err(format!("host `{name}` is already declared"));
            }
            t.hosts.push(HostDecl { name: name.clone(), daemon: *daemon, frontend: *frontend });
        }
        TopologyEdit::RemoveHost { name } => {
            if *name == t.hub {
                return Err(format!("`{name}` is the hub; pick a new hub before removing it"));
            }
            let before = t.hosts.len();
            t.hosts.retain(|h| h.name != *name);
            if t.hosts.len() == before {
                return Err(format!("host `{name}` is not declared"));
            }
        }
        TopologyEdit::SetFlag { name, key, value } => {
            let host = t.hosts.iter_mut().find(|h| h.name == *name).ok_or_else(|| format!("host `{name}` is not declared"))?;
            match key.as_str() {
                "daemon" => host.daemon = *value,
                "frontend" => host.frontend = *value,
                other => return Err(format!("`{other}` is not an editable flag (daemon, frontend)")),
            }
        }
        TopologyEdit::MonitorAdd { label, target } => {
            if t.monitor.iter().any(|(l, _)| l == label) {
                return Err(format!("monitor label `{label}` is already declared"));
            }
            t.monitor.push((label.clone(), target.clone()));
        }
        TopologyEdit::MonitorRemove { label } => {
            let before = t.monitor.len();
            t.monitor.retain(|(l, _)| l != label);
            if t.monitor.len() == before {
                return Err(format!("monitor label `{label}` is not declared"));
            }
        }
    }
    Ok(t)
}

/// The DECLARED word list for one listed host: `hub`/`daemon`/`frontend`
/// (any subset, `shell` when none apply), plus `sampled` when it's also a
/// `[monitor]` target (by label or resolved target — [`Topology::
/// monitor_targets`]). Exactly the words `status_table` prints per host,
/// factored out so `sotd status` (topology plan §E's DECLARED column)
/// prints the identical words instead of re-deriving them.
pub fn declared_words(topo: &Topology, host: &HostDecl) -> Vec<&'static str> {
    let mut words = Vec::new();
    if host.name == topo.hub { words.push("hub") }
    if host.daemon { words.push("daemon") }
    if host.frontend { words.push("frontend") }
    if words.is_empty() { words.push("shell") }
    if topo.monitor_targets().iter().any(|(l, t)| *l == host.name || *t == host.name) {
        words.push("sampled")
    }
    words
}

/// `sotd topology status` — the declared table only; live columns come
/// from `version.query` on each daemon.
pub fn status_table(topo: &Topology) -> String {
    let mut out = String::from("HOST DECLARED\n");
    for h in &topo.hosts {
        out.push_str(&format!("{} {}\n", h.name, declared_words(topo, h).join(",")));
    }
    for (label, target) in topo.monitor_targets() {
        if topo.host(&label).is_none() && topo.host(&target).is_none() {
            out.push_str(&format!("{label} monitor-only {target}\n"));
        }
    }
    // Surfaced here and only here (module doc): the person who edited the
    // file is looking at `status`, not at `plan`'s line-oriented output.
    for w in &topo.warnings {
        out.push_str(&format!("WARNING {w}\n"));
    }
    out
}
