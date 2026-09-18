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
//! any other key or section is an error naming it — this is also the
//! schema check: an old-grammar file (`default_host`, per host
//! `ssh_alias`/`remote_repo`/`tcp_port`/`remote_socket`/`socket`/
//! `remote_home`) fails on its first unknown key, naming the line; a key
//! given twice in a section is an error; a hub must be a `daemon` host.
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
                    other => return Err(format!("line {n}: unknown key `{other}` in [host.{}]", host.name)),
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
    Ok(Topology { hub, hosts, monitor })
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
        let run = crate::runtime_sot_dir();
        let base = run.parent().map(PathBuf::from).unwrap_or(run);
        format!("unix:{}", base.join("sot-relay.sock").display())
    })
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
/// dial <host> <endpoint>             # one per dialable host (daemon, not frontend —
///                                    # D8: a frontend box's daemon is never dialled
///                                    # from elsewhere): this box's own socket for
///                                    # itself, tcp:127.0.0.1:<port> for others
/// tunnel <host> <port>               # one per dialable host except self:
///                                    # ssh -L <port>:$(ssh <host> sotd session-socket-path sot) <host>
/// ```
pub fn plan(topo: &Topology, self_host: &str) -> Result<String, String> {
    let relay = relay_endpoint(topo, self_host)?;
    let mut out = format!("self {self_host}\nhub {}\nrelay-endpoint {relay}\n", topo.hub);
    for (name, endpoint) in dial_endpoints(topo, self_host) {
        out.push_str(&format!("dial {name} {endpoint}\n"));
    }
    for h in dialable_hosts(topo).filter(|h| h.name != self_host) {
        out.push_str(&format!("tunnel {} {}\n", h.name, topo.local_port(&h.name).expect("listed")));
    }
    Ok(out)
}

/// Every "dialable" host — `daemon && !frontend` (D8: a `frontend` box's
/// daemon is never dialled from elsewhere) — in file order.
pub fn dialable_hosts(topo: &Topology) -> impl Iterator<Item = &HostDecl> {
    topo.hosts.iter().filter(|h| h.daemon && !h.frontend)
}

/// `(host, endpoint)` for every [`dialable_hosts`] entry, resolved for
/// `self_host`: its own local socket for itself, `tcp:127.0.0.1:<ordinal
/// port>` for every other one. This is `plan`'s own `dial` line
/// resolution, factored out so a caller that wants it as DATA — `sotd
/// status` (topology plan §E), which actually dials each entry rather than
/// printing it — shares the identical mapping instead of re-deriving it or
/// re-parsing `plan`'s text.
pub fn dial_endpoints(topo: &Topology, self_host: &str) -> Vec<(String, String)> {
    dialable_hosts(topo)
        .map(|h| {
            let endpoint = if h.name == self_host {
                local_endpoint("sot")
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

/// Refuse anywhere but the hub, naming it — `apply` enables/disables
/// systemd units and must not run on a box's say-so alone.
pub fn require_hub(topo: &Topology, self_host: &str) -> Result<(), String> {
    if self_host != topo.hub {
        return Err(format!(
            "this box (`{self_host}`) is not the hub (`{}`); run `sotd topology apply` there instead",
            topo.hub
        ));
    }
    Ok(())
}

/// What `apply` does to the hub's `sot-relay-tunnel@*` instances: enable
/// what's wanted and not yet enabled, disable what's enabled and no longer
/// wanted. Pure — `enabled_now` is whatever the caller already queried
/// from `systemctl`, so this is testable against a fixture with no
/// systemd, and a second call with `enabled_now == tunnel_hosts(topo)` is
/// a no-op by construction (both lists come back empty).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ApplyPlan {
    pub enable: Vec<String>,
    pub disable: Vec<String>,
}

pub fn apply_plan(topo: &Topology, enabled_now: &[String]) -> ApplyPlan {
    let want: Vec<String> = tunnel_hosts(topo).into_iter().map(str::to_string).collect();
    let enable = want.iter().filter(|h| !enabled_now.iter().any(|e| e == *h)).cloned().collect();
    let disable = enabled_now.iter().filter(|e| !want.contains(e)).cloned().collect();
    ApplyPlan { enable, disable }
}

/// The name apply's own drop-in owns inside `sot-relay-tunnel@<host>.service.d/`
/// — fixed, so a hand-made drop-in beside it under any other name is never
/// touched by a re-apply.
pub const TUNNEL_DROPIN_FILE: &str = "topology.conf";

/// The `ConditionHost=` drop-in text for one tunnel instance: the shared
/// home means the same `[host.*]` list — and the same
/// `~/.config/systemd/user` — is visible on every box that shares it, so
/// enabling `sot-relay-tunnel@<host>` there would start it on all of them.
/// This pins execution to the box whose hostname is the hub (host names
/// equal `host_name()`, module doc, so a plain match is enough).
pub fn tunnel_dropin(hub: &str) -> String {
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
    fn unknown_key_names_the_key() {
        let e = parse("hub = \"a\"\n[host.a]\ncolour = \"red\"\n").unwrap_err();
        assert!(e.contains("line 3") && e.contains("unknown key `colour`"), "{e}");
        let e = parse("hub = \"a\"\nsize = 3\n[host.a]\n").unwrap_err();
        assert!(e.contains("unknown key `size`"), "{e}");
    }

    #[test]
    fn bad_section_key_fails() {
        let e = parse("hub = \"a\"\n[host.a]\n[host.Two Words]\n").unwrap_err();
        assert!(e.contains("line 3") && e.contains("`[host.Two Words]` is not a plain host name"), "{e}");
        let e = parse("hub = \"a\"\n[host.a]\n[hosts]\n").unwrap_err();
        assert!(e.contains("unknown section `[hosts]`"), "{e}");
    }

    /// The v1 shim is gone: an old-grammar file (real shape: `default_host`
    /// at top level, a host carrying `socket`) is now a loud parse error at
    /// its first unknown key, naming the line — never a silently-kept file.
    #[test]
    fn old_grammar_file_fails_on_first_unknown_key() {
        let text = "\u{feff}default_host = \"hub\"\n\n[host.hub]\nsocket = \"/run/user/1000/sot/sessions/sot.sock\"\n";
        let e = parse(text).unwrap_err();
        assert!(e.contains("line 1") && e.contains("unknown key `default_host`"), "{e}");
        let e = parse("hub = \"a\"\n[host.a]\nssh_alias = \"a\"\n").unwrap_err();
        assert!(e.contains("unknown key `ssh_alias`"), "{e}");
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
        // gamma is daemon+frontend: never dialled from elsewhere (D8), and
        // its own daemon is the launcher's implicit local connection.
        assert_eq!(
            plan(&t, "gamma").unwrap(),
            format!("self gamma\nhub alpha\nrelay-endpoint tcp:127.0.0.1:{base}\ndial alpha tcp:127.0.0.1:{base}\ntunnel alpha {base}\n")
        );
        assert_eq!(
            plan(&t, "alpha").unwrap(),
            format!("self alpha\nhub alpha\nrelay-endpoint {own}\ndial alpha {own}\n")
        );
        let beta = plan(&t, "beta").unwrap();
        assert!(beta.lines().nth(2).unwrap().starts_with("relay-endpoint unix:"), "{beta}");
        assert!(beta.lines().nth(2).unwrap().ends_with("sot-relay.sock"), "{beta}");
        assert!(plan(&t, "nobody").unwrap_err().contains("`nobody` is not a listed host"));
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

[host.remote-b]

[host.fe]
frontend = true
"#;

    #[test]
    fn tunnel_hosts_excludes_hub_and_frontend() {
        let t = parse(APPLY_FIXTURE).unwrap();
        assert_eq!(tunnel_hosts(&t), vec!["remote-a", "remote-b"]);
        // gamma in V2 is daemon AND frontend: still excluded (frontend wins).
        let v2 = parse(V2).unwrap();
        assert_eq!(tunnel_hosts(&v2), vec!["beta"]);
    }

    #[test]
    fn require_hub_refuses_elsewhere() {
        let t = parse(APPLY_FIXTURE).unwrap();
        assert!(require_hub(&t, "hub").is_ok());
        let e = require_hub(&t, "remote-a").unwrap_err();
        assert!(e.contains("`remote-a`") && e.contains("`hub`"), "{e}");
    }

    #[test]
    fn apply_plan_enables_disables_and_settles() {
        let t = parse(APPLY_FIXTURE).unwrap();
        // Nothing enabled yet: enable both, disable nothing.
        let p = apply_plan(&t, &[]);
        assert_eq!(p.enable, vec!["remote-a", "remote-b"]);
        assert!(p.disable.is_empty());
        // A stale instance (no longer declared) alongside one still wanted.
        let p = apply_plan(&t, &["remote-a".to_string(), "gone".to_string()]);
        assert_eq!(p.enable, vec!["remote-b"]);
        assert_eq!(p.disable, vec!["gone"]);
        // Second run, now converged: a true no-op.
        let p = apply_plan(&t, &["remote-a".to_string(), "remote-b".to_string()]);
        assert!(p.enable.is_empty() && p.disable.is_empty());
    }

    #[test]
    fn tunnel_dropin_text_is_exact() {
        assert_eq!(
            tunnel_dropin("hub"),
            "# Generated by `sotd topology apply` — regenerated on every apply, do not edit by hand.\n[Unit]\nConditionHost=hub\n"
        );
        assert_eq!(tunnel_unit("remote-a"), "sot-relay-tunnel@remote-a");
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
