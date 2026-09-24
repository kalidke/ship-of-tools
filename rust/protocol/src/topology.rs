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

/// The relay's forward port on a frontend box, and the base of the ordinal
/// port series `topology plan` hands the launcher — **per OS user**, not a
/// single constant: two OS users sharing one Windows box each run their own
/// launcher and tunnel, and a fixed port let the second user's launcher find
/// the first user's tunnel already open and dial that user's backend as its
/// own (field report, 2026-09-17). `18743 + (h % 100)`, `h` an FNV-1a 32-bit
/// hash of the OS user name; no user name found keeps the old fixed `18743`.
/// [`hub_local_port_for`] is the pure half, for tests.
pub fn hub_local_port() -> u16 {
    let user = std::env::var("USER")
        .or_else(|_| std::env::var("USERNAME"))
        .unwrap_or_else(|_| {
            std::env::var_os("LOGNAME")
                .map(|v| v.to_string_lossy().into_owned())
                .unwrap_or_default()
        });
    hub_local_port_for(&user)
}

/// Pure half of [`hub_local_port`]: FNV-1a 32-bit over `user`'s bytes,
/// folded to a two-digit offset above `18743`. Written inline — no new
/// crate — and kept free of the environment so a test can predict a port
/// without mutating process-global state.
pub fn hub_local_port_for(user: &str) -> u16 {
    if user.is_empty() {
        return 18743;
    }
    let mut hash: u32 = 0x811c_9dc5;
    for b in user.as_bytes() {
        hash ^= u32::from(*b);
        hash = hash.wrapping_mul(0x0100_0193);
    }
    18743 + (hash % 100) as u16
}

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

    /// Ordinal local port for a host: the hub gets [`hub_local_port`] (this
    /// box's OS user, not a fixed number), every other host that plus
    /// `1 + its position among the non-hub hosts` (file order). A property
    /// of the position, not of the flags, so flipping `daemon` on one host
    /// renumbers nobody; inserting a host does (plan §G).
    pub fn local_port(&self, name: &str) -> Option<u16> {
        if name == self.hub {
            return Some(hub_local_port());
        }
        self.hosts
            .iter()
            .filter(|h| h.name != self.hub)
            .position(|h| h.name == name)
            .map(|i| hub_local_port() + 1 + i as u16)
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
                if !is_plain_host_name(h) {
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

fn is_plain_host_name(s: &str) -> bool {
    let mut chars = s.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_lowercase() || c.is_ascii_digit())
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '.' | '_' | '-'))
}

// ─── What a box derives from the list ────────────────────────────────────

/// This box's own control endpoint for `label`, in the `unix:`/`pipe:`
/// spelling the comm scripts and the frontend already speak.
pub fn local_endpoint(label: &str) -> String {
    let p = crate::session_socket_path(label);
    if cfg!(windows) { format!("pipe:{}", p.display()) } else { format!("unix:{}", p.display()) }
}

/// The relay endpoint for `self_host` (plan §C): the hub's own socket on
/// the hub; on a `frontend` box the launcher's forward tunnel to the hub
/// (`tcp:127.0.0.1:<`[`hub_local_port`]`>`, per OS user); everywhere else
/// the hub's reverse tunnel, which lands on
/// `<runtime dir>/sot-relay.sock` (`/run/user/<uid>/sot-relay.sock` — the
/// path `sot-relay-tunnel@` has always used). Path derivations are this
/// box's own, so ask on the box in question.
pub fn relay_endpoint(topo: &Topology, self_host: &str) -> Result<String, String> {
    let host = topo.host(self_host).ok_or_else(|| format!("`{self_host}` is not a listed host"))?;
    Ok(if self_host == topo.hub {
        local_endpoint("sot")
    } else if host.frontend {
        format!("tcp:127.0.0.1:{}", hub_local_port())
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
/// host it serves ([`relay_hosts`], unit [`relay_unit`]) and every peer
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
///                                    # remote host; tcp:127.0.0.1:<port> elsewhere
/// tunnel <host> <port>               # one per dialable host except self, and none
///                                    # at all on the hub (whose `dial` endpoints are
///                                    # already local): forward this port TO THE HUB'S
///                                    # socket for that host —
///                                    # ssh -L <port>:<the hub's socket for <host>> <hub>
///                                    # — never an ssh into <host> itself. The hub's
///                                    # socket for the hub is its own session socket;
///                                    # for any other host it is `relay_socket_path`,
///                                    # which only the hub can derive (ask it with
///                                    # `sotd topology relay-sockets`).
/// ```
pub fn plan(topo: &Topology, self_host: &str) -> Result<String, String> {
    let relay = relay_endpoint(topo, self_host)?;
    let mut out = format!("self {self_host}\nhub {}\nrelay-endpoint {relay}\n", topo.hub);
    for (name, endpoint) in dial_endpoints(topo, self_host) {
        out.push_str(&format!("dial {name} {endpoint}\n"));
    }
    // Nothing to forward on the hub: every `dial` line it just emitted is
    // already a local socket. A `tunnel` line there would name a forward
    // from the hub to itself.
    if self_host != topo.hub {
        for h in dialable_hosts(topo).filter(|h| h.name != self_host) {
            out.push_str(&format!("tunnel {} {}\n", h.name, topo.local_port(&h.name).expect("listed")));
        }
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
/// [`tunnel_hosts`] is a SEPARATE list and still skips `frontend` boxes —
/// the comm relay's reverse tunnels are not this.
pub fn dialable_hosts(topo: &Topology) -> impl Iterator<Item = &HostDecl> {
    topo.hosts.iter().filter(|h| h.daemon)
}

/// `(host, endpoint)` for every [`dialable_hosts`] entry, resolved for
/// `self_host`: its own local socket for itself; on the hub, the hub's own
/// socket for a remote host ([`relay_socket_path`] — no forward needed,
/// it is right there); `tcp:127.0.0.1:<ordinal port>` on every other box,
/// where that port is a forward to the same hub socket. This is `plan`'s own `dial` line
/// resolution, factored out so a caller that wants it as DATA — `sotd
/// status` (topology plan §E), which actually dials each entry rather than
/// printing it — shares the identical mapping instead of re-deriving it or
/// re-parsing `plan`'s text.
pub fn dial_endpoints(topo: &Topology, self_host: &str) -> Vec<(String, String)> {
    dialable_hosts(topo)
        .map(|h| {
            let endpoint = if h.name == self_host {
                local_endpoint("sot")
            } else if self_host == topo.hub {
                format!("unix:{}", relay_socket_path(&h.name).display())
            } else {
                format!("tcp:127.0.0.1:{}", topo.local_port(&h.name).expect("listed host has a port"))
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
            if !is_plain_host_name(name) {
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

// ─── `sotd topology apply` (plan §C, §F step 5) ──────────────────────────

/// The `sot-relay-tunnel@<host>` unit name for a host's reverse tunnel.
pub fn tunnel_unit(host: &str) -> String {
    format!("sot-relay-tunnel@{host}")
}

/// Hosts the hub reverse-tunnels to: every listed host except the hub
/// itself and any `frontend` box (D8 — a frontend's own daemon is never
/// dialled, so nothing tunnels to it). Independent of `daemon`: D4 keeps
/// the lab servers `daemon = false` in 0.6 while they still carry a relay
/// tunnel (shell + relay + sampled). File order, like every other derived
/// list here.
pub fn tunnel_hosts(topo: &Topology) -> Vec<&str> {
    topo.hosts.iter().filter(|h| h.name != topo.hub && !h.frontend).map(|h| h.name.as_str()).collect()
}

/// The `sot-host-relay-<host>.socket` unit name for the hub's own socket
/// for a host: the listener a peer forwards to, whose per-connection
/// instance runs `sotd stdio-bridge` on that host.
///
/// The host sits in the unit's PREFIX rather than after an `@`, and that
/// is forced, not taste. A socket with `Accept=yes` instantiates
/// `<prefix>@<connection id>.service`, so a templated *socket*
/// (`sot-host-relay@<host>.socket`) hands its service the connection id
/// and never the host — systemd issue #19071, open against the systemd
/// the hub runs. A per-host prefix is the only spelling in which the
/// service that ssh's out knows which box it is ssh'ing to. Hence
/// [`relay_socket_unit`] / [`relay_service_unit`]: per-host unit text
/// written by `sotd topology apply`, instead of one hand-installed
/// template.
pub fn relay_unit(host: &str) -> String {
    format!("sot-host-relay-{host}.socket")
}

/// The per-connection service template [`relay_unit`]'s socket activates:
/// `<prefix>@.service`, one instance per accepted connection.
pub fn relay_service_unit_file(host: &str) -> String {
    format!("sot-host-relay-{host}@.service")
}

/// The first lines of every unit file `apply` writes: the file is
/// `apply`'s (a hand edit is lost on the next run), and where an override
/// does survive.
const GENERATED_UNIT_HEADER: &str = "\
# Generated by `sotd topology apply` — rewritten on every apply, never edited
# by hand: a per-host override belongs in a drop-in beside this file, which
# apply leaves alone except for its own topology.conf.
";

/// The hub's listener for `host` — the unit file [`relay_unit`] names.
/// Generated, like [`apply_dropin`], rather than shipped: every line of it
/// is derived, and the path is [`relay_socket_path`]'s, so the listener
/// and `sotd topology relay-sockets` cannot disagree about where a peer
/// forwards to.
pub fn relay_socket_unit(host: &str) -> String {
    format!(
        "{GENERATED_UNIT_HEADER}#
# One listener per enrolled host. A peer forwards its own dial port to this
# socket (`ssh -L <port>:<this path> <hub>`), and each accepted connection
# gets its own `sotd stdio-bridge` on the far box ({service}). The socket
# exists whether or not that box is up, so its presence is never a liveness
# claim — only a completed hello is.
[Unit]
Description=Ship of Tools cross-host relay socket for {host}

[Socket]
ListenStream={path}
Accept=yes
# A route into another box's daemon, so owner-only — the posture the daemon's
# own endpoints hold. /run/user/<uid> is already 0700; this is the belt to
# that brace.
SocketMode=0600

[Install]
WantedBy=sockets.target
",
        service = relay_service_unit_file(host),
        path = relay_socket_path(host).display(),
    )
}

/// The per-connection bridge to `host` — the unit file
/// [`relay_service_unit_file`] names.
pub fn relay_service_unit(host: &str) -> String {
    let control = relay_socket_path(host)
        .parent()
        .unwrap_or_else(|| std::path::Path::new("."))
        .join("sot-host-relay-ssh-%%C");
    format!(
        "{GENERATED_UNIT_HEADER}#
# One instance per accepted connection on {socket}. `%i` here is the
# connection id systemd made up, not the host — a templated socket cannot
# pass its own instance to the service it activates (systemd #19071), which
# is why this unit's NAME carries the host.
[Unit]
Description=Ship of Tools relay bridge to {host} (connection %i)

[Service]
# The accepted connection IS the bridge's stdin/stdout. `stdio-bridge` puts
# nothing else on stdout, so the far daemon's bytes arrive unmixed; ssh's own
# complaints go to the journal.
StandardInput=socket
StandardOutput=socket
StandardError=journal
# A drop-in overrides any of these three without regenerating the unit: a box
# whose `sotd` is not on a non-interactive ssh PATH (give the full path), a
# second daemon label, or an ssh alias that differs from the declared name.
Environment=SOT_RELAY_TARGET={host}
Environment=SOT_RELAY_SOTD=sotd
Environment=SOT_RELAY_LABEL=sot
# -T: no pty, so nothing rewrites the byte stream. BatchMode: never prompt —
# an unreachable box must fail at once rather than hang. The ServerAlive pair
# closes a wedged network in ~45s. ControlMaster/ControlPersist are what make
# the second and later connections a channel on one ssh instead of a fresh
# login each; ControlPath's `%%C` is ssh's own per-target hash.
ExecStart=/usr/bin/ssh -T -o BatchMode=yes -o ServerAliveInterval=15 -o ServerAliveCountMax=3 -o ControlMaster=auto -o ControlPath={control} -o ControlPersist=600 ${{SOT_RELAY_TARGET}} ${{SOT_RELAY_SOTD}} stdio-bridge --label ${{SOT_RELAY_LABEL}}
# The multiplexed master ssh OUTLIVES the instance that started it — that is
# what ControlPersist is for — so stopping this instance must not sweep its
# cgroup, or every connection would pay a fresh login after all.
KillMode=process
# A per-connection instance is garbage once it ends, failed or not; the
# journal keeps the reason.
CollectMode=inactive-or-failed
",
        socket = relay_unit(host),
        control = control.display(),
    )
}

/// Hosts the hub serves a socket for: every [`dialable_hosts`] entry but
/// the hub itself, whose endpoint is its own session socket and needs no
/// relay. File order, like every other derived list here.
pub fn relay_hosts(topo: &Topology) -> Vec<&str> {
    dialable_hosts(topo).filter(|h| h.name != topo.hub).map(|h| h.name.as_str()).collect()
}

/// Refuse anywhere but the hub, naming it and the subcommand — these
/// derivations are the HUB'S (it enables/disables the hub's systemd units,
/// or prints paths in the hub's own runtime dir), and answering them
/// elsewhere would be a confident wrong answer rather than a refusal.
pub fn require_hub(topo: &Topology, self_host: &str, subcommand: &str) -> Result<(), String> {
    if self_host != topo.hub {
        return Err(format!(
            "this box (`{self_host}`) is not the hub (`{}`); run `sotd topology {subcommand}` there instead",
            topo.hub
        ));
    }
    Ok(())
}

/// One unit family's convergence: enable what's wanted and not yet
/// enabled, disable what's enabled and no longer wanted. HOST names, not
/// unit names — the caller owns the `@`-template spelling.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct UnitDiff {
    pub enable: Vec<String>,
    pub disable: Vec<String>,
}

impl UnitDiff {
    pub fn is_empty(&self) -> bool {
        self.enable.is_empty() && self.disable.is_empty()
    }
}

fn converge(want: &[&str], enabled_now: &[String]) -> UnitDiff {
    UnitDiff {
        enable: want.iter().filter(|h| !enabled_now.iter().any(|e| e == *h)).map(|h| h.to_string()).collect(),
        disable: enabled_now.iter().filter(|e| !want.contains(&e.as_str())).cloned().collect(),
    }
}

/// What `apply` does to the hub's TWO unit families: `sot-relay-tunnel@*`
/// (the comm relay's reverse tunnels, [`tunnel_hosts`]) and
/// `sot-host-relay-*.socket` (the hub's own socket per dialable host,
/// [`relay_hosts`]). Two lists because the two families answer different
/// questions — a `frontend` box carries no relay tunnel but is dialable,
/// and a `daemon = false` server is the reverse — so one list could not
/// serve both without lying about one of them.
///
/// Pure: each `*_enabled_now` is whatever the caller already queried from
/// `systemctl`, so this is testable against a fixture with no systemd, and
/// a second call with the wanted lists is a no-op by construction.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ApplyPlan {
    pub tunnels: UnitDiff,
    pub relays: UnitDiff,
}

impl ApplyPlan {
    pub fn is_empty(&self) -> bool {
        self.tunnels.is_empty() && self.relays.is_empty()
    }
}

pub fn apply_plan(topo: &Topology, tunnels_enabled_now: &[String], relays_enabled_now: &[String]) -> ApplyPlan {
    ApplyPlan {
        tunnels: converge(&tunnel_hosts(topo), tunnels_enabled_now),
        relays: converge(&relay_hosts(topo), relays_enabled_now),
    }
}

/// The name apply's own drop-in owns inside an instance's `.service.d/`
/// — fixed, and the same in both unit families, so a hand-made drop-in
/// beside it under any other name is never touched by a re-apply.
pub const APPLY_DROPIN_FILE: &str = "topology.conf";

/// The `ConditionHost=` drop-in text for one instance of either family:
/// the shared home means the same `[host.*]` list — and the same
/// `~/.config/systemd/user` — is visible on every box that shares it, so
/// enabling an instance there would start it on all of them. This pins
/// execution to the box whose hostname is the hub (host names equal
/// `host_name()`, module doc, so a plain match is enough).
pub fn apply_dropin(hub: &str) -> String {
    format!(
        "# Generated by `sotd topology apply` — regenerated on every apply, do not edit by hand.\n[Unit]\nConditionHost={hub}\n"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Pins `USER` for [`hub_local_port`] so a test can predict its output
    /// without depending on whoever's running it, restored on drop. Both
    /// callers below pin the SAME name, so running them in parallel (the
    /// default test-runner behaviour) never races on the value, only on
    /// which one restores it last — harmless, since both write it back
    /// identically before that.
    struct PinnedUser(Option<std::ffi::OsString>);
    impl PinnedUser {
        fn set(name: &str) -> Self {
            let prev = std::env::var_os("USER");
            std::env::set_var("USER", name);
            PinnedUser(prev)
        }
    }
    impl Drop for PinnedUser {
        fn drop(&mut self) {
            match self.0.take() {
                Some(v) => std::env::set_var("USER", v),
                None => std::env::remove_var("USER"),
            }
        }
    }
    const TEST_USER: &str = "sot-test-user";

    const V2: &str = r#"
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
        let _pin = PinnedUser::set(TEST_USER);
        let base = hub_local_port_for(TEST_USER);
        let t = parse(V2).unwrap();
        assert_eq!(t.hub, "alpha");
        assert_eq!(t.hosts.len(), 4);
        assert_eq!(t.host("gamma").unwrap(), &HostDecl { name: "gamma".into(), daemon: true, frontend: true });
        assert_eq!(t.host("beta").unwrap(), &HostDecl { name: "beta".into(), daemon: false, frontend: false });
        assert_eq!(t.monitor_targets()[1], ("beta".to_string(), "beta".to_string()));
        assert_eq!(t.monitor_targets()[2].1, "other-user@gpu-box");
        assert_eq!(t.local_port("alpha"), Some(base));
        assert_eq!(t.local_port("beta"), Some(base + 1));
        assert_eq!(t.local_port("delta"), Some(base + 3));
    }

    #[test]
    fn hub_local_port_for_is_per_user() {
        // Pure function: no env involved, so no pinning needed here.
        assert_eq!(hub_local_port_for(""), 18743, "no user name keeps the pre-existing fixed value");
        let a = hub_local_port_for("alice");
        let b = hub_local_port_for("bob");
        assert_ne!(a, b, "two different OS users must not land on the same tunnel port");
        assert!((18743..18843).contains(&a) && (18743..18843).contains(&b));
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
        let _pin = PinnedUser::set(TEST_USER);
        let base = hub_local_port_for(TEST_USER);
        let t = parse(V2).unwrap();
        let own = local_endpoint("sot");
        let gamma_port = t.local_port("gamma").expect("gamma is listed");
        // gamma is daemon AND frontend: dialable all the same, and on its
        // own box that dial is the implicit local connection.
        assert_eq!(
            plan(&t, "gamma").unwrap(),
            format!("self gamma\nhub alpha\nrelay-endpoint tcp:127.0.0.1:{base}\ndial alpha tcp:127.0.0.1:{base}\ndial gamma {own}\ntunnel alpha {base}\n")
        );
        // On the hub every dial is already local — its own socket for
        // itself, its own relay socket for gamma — so no tunnel lines.
        assert_eq!(
            plan(&t, "alpha").unwrap(),
            format!(
                "self alpha\nhub alpha\nrelay-endpoint {own}\ndial alpha {own}\ndial gamma unix:{}\n",
                relay_socket_path("gamma").display()
            )
        );
        // A peer reaches gamma the same way it reaches any other host: a
        // forward to the hub, on gamma's own ordinal port.
        let beta_plan = plan(&t, "beta").unwrap();
        assert!(beta_plan.contains(&format!("dial gamma tcp:127.0.0.1:{gamma_port}\n")), "{beta_plan}");
        assert!(beta_plan.contains(&format!("tunnel gamma {gamma_port}\n")), "{beta_plan}");
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
    fn apply_dropin_text_is_exact() {
        assert_eq!(
            apply_dropin("hub"),
            "# Generated by `sotd topology apply` — regenerated on every apply, do not edit by hand.\n[Unit]\nConditionHost=hub\n"
        );
        assert_eq!(tunnel_unit("remote-a"), "sot-relay-tunnel@remote-a");
        assert_eq!(relay_unit("remote-a"), "sot-host-relay-remote-a.socket");
        assert_eq!(relay_service_unit_file("remote-a"), "sot-host-relay-remote-a@.service");
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
        assert!(sock.contains("sot-host-relay-remote-a@.service"), "{sock}");

        let svc = relay_service_unit("remote-a");
        assert!(svc.contains("Environment=SOT_RELAY_TARGET=remote-a\n"), "{svc}");
        assert!(svc.contains("stdio-bridge --label ${SOT_RELAY_LABEL}\n"), "{svc}");
        assert!(svc.contains("-o ControlPath=") && svc.contains("-ssh-%%C "), "{svc}");
        assert!(svc.contains("\nStandardInput=socket\n") && svc.contains("\nStandardOutput=socket\n"), "{svc}");
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
}
