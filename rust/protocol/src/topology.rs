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
//! any other key or section is an error naming it. The v1 keys
//! (`default_host`, `ssh_alias`, `tcp_port`, `remote_socket`, `remote_repo`,
//! `remote_home`) are accepted for THIS release with one warning each naming
//! the replacement — see [`parse`] for what each still does — and are
//! deleted at the next rc.
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

use std::path::PathBuf;

/// The relay's forward port on a frontend box, and the base of the ordinal
/// port series `topology plan` hands the launcher: the hub is always
/// `18743`, the remaining hosts count up from `18744` in file order.
pub const HUB_LOCAL_PORT: u16 = 18743;

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
    /// One line per accepted v1 key, naming the replacement.
    pub warnings: Vec<String>,
}

impl Topology {
    pub fn host(&self, name: &str) -> Option<&HostDecl> {
        self.hosts.iter().find(|h| h.name == name)
    }

    /// Ordinal local port for a host: the hub `18743`, every other host
    /// `18744 + its position among the non-hub hosts` (file order). A
    /// property of the position, not of the flags, so flipping `daemon` on
    /// one host renumbers nobody; inserting a host does (plan §G).
    pub fn local_port(&self, name: &str) -> Option<u16> {
        if name == self.hub {
            return Some(HUB_LOCAL_PORT);
        }
        self.hosts
            .iter()
            .filter(|h| h.name != self.hub)
            .position(|h| h.name == name)
            .map(|i| HUB_LOCAL_PORT + 1 + i as u16)
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

/// Parse grammar v2 (module doc). v1 compatibility, one warning each:
/// `default_host` sets `hub`; `ssh_alias` marks the host `daemon = true`
/// (in v1 every listed host was a dialled backend) and must equal the
/// section key; `tcp_port`, `remote_socket`, `remote_repo`, `remote_home`
/// are ignored (ordinal, queried, never started by path, served by
/// `workspace.list`).
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
    let mut warnings = Vec::new();
    let mut section = Section::Top;

    for (i, raw) in text.lines().enumerate() {
        let n = i + 1;
        let line = strip_comment(raw).trim();
        if line.is_empty() {
            continue;
        }
        if let Some(inner) = line.strip_prefix('[') {
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
        match section {
            Section::Top => match key {
                "hub" | "default_host" => {
                    if key == "default_host" {
                        warnings.push(format!("line {n}: `default_host` is `hub` now"));
                    }
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
                    "ssh_alias" => {
                        let alias = val.string().unwrap_or("");
                        if !alias.is_empty() && alias != host.name {
                            return Err(format!(
                                "line {n}: `ssh_alias = \"{alias}\"` differs from the section key `{}` (the key IS the ssh alias now)",
                                host.name
                            ));
                        }
                        warnings.push(format!(
                            "line {n}: `ssh_alias` is the section key now; `[host.{}]` treated as `daemon = true`",
                            host.name
                        ));
                        host.daemon = true;
                    }
                    "tcp_port" => warnings.push(format!("line {n}: `tcp_port` is ignored; ports are ordinal (`sotd topology plan`)")),
                    "remote_socket" => warnings.push(format!("line {n}: `remote_socket` is ignored; queried via `sotd session-socket-path sot`")),
                    "remote_repo" => warnings.push(format!("line {n}: `remote_repo` is ignored; a remote daemon is never started by path")),
                    "remote_home" => warnings.push(format!("line {n}: `remote_home` is ignored; `workspace.list` serves the default row root")),
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
    if hosts.iter().all(|h| h.name != hub) {
        return Err(format!("hub `{hub}` is not a listed host (add `[host.{hub}]`)"));
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
/// (`tcp:127.0.0.1:18743`); everywhere else the hub's reverse tunnel, which
/// lands on `<runtime dir>/sot-relay.sock` (`/run/user/<uid>/sot-relay.sock`
/// — the path `sot-relay-tunnel@` has always used). Path derivations are
/// this box's own, so ask on the box in question.
pub fn relay_endpoint(topo: &Topology, self_host: &str) -> Result<String, String> {
    let host = topo.host(self_host).ok_or_else(|| format!("`{self_host}` is not a listed host"))?;
    Ok(if self_host == topo.hub {
        local_endpoint("sot")
    } else if host.frontend {
        format!("tcp:127.0.0.1:{HUB_LOCAL_PORT}")
    } else {
        let run = crate::runtime_sot_dir();
        let base = run.parent().map(PathBuf::from).unwrap_or(run);
        format!("unix:{}", base.join("sot-relay.sock").display())
    })
}

/// `sotd topology plan --self <host>` — plain lines, one fact per line,
/// space-separated fields, stable order; the launcher reads them with a
/// `split` and nothing else. Documented here and nowhere else:
///
/// ```text
/// self <host>
/// hub <host>
/// relay-endpoint <endpoint>          # what SOT_RELAY_ENDPOINT is on this box
/// dial <host> <endpoint>             # one per daemon host: this box's own socket
///                                    # for itself, tcp:127.0.0.1:<port> for others
/// tunnel <host> <port>               # one per daemon host except self:
///                                    # ssh -L <port>:$(ssh <host> sotd session-socket-path sot) <host>
/// ```
pub fn plan(topo: &Topology, self_host: &str) -> Result<String, String> {
    let relay = relay_endpoint(topo, self_host)?;
    let mut out = format!("self {self_host}\nhub {}\nrelay-endpoint {relay}\n", topo.hub);
    for h in topo.hosts.iter().filter(|h| h.daemon) {
        let port = topo.local_port(&h.name).expect("listed host has a port");
        if h.name == self_host {
            out.push_str(&format!("dial {} {}\n", h.name, local_endpoint("sot")));
        } else {
            out.push_str(&format!("dial {} tcp:127.0.0.1:{port}\n", h.name));
        }
    }
    for h in topo.hosts.iter().filter(|h| h.daemon && h.name != self_host) {
        out.push_str(&format!("tunnel {} {}\n", h.name, topo.local_port(&h.name).expect("listed")));
    }
    Ok(out)
}

/// `sotd topology status` — the declared table only; live columns come
/// from `version.query` on each daemon.
pub fn status_table(topo: &Topology) -> String {
    let sampled: Vec<&str> = topo.monitor.iter().flat_map(|(l, t)| [l.as_str(), t.as_str()]).collect();
    let mut out = String::from("HOST DECLARED\n");
    for h in &topo.hosts {
        let mut words = Vec::new();
        if h.name == topo.hub { words.push("hub") }
        if h.daemon { words.push("daemon") }
        if h.frontend { words.push("frontend") }
        if words.is_empty() { words.push("shell") }
        if sampled.contains(&h.name.as_str()) { words.push("sampled") }
        out.push_str(&format!("{} {}\n", h.name, words.join(",")));
    }
    for (label, target) in topo.monitor_targets() {
        if topo.host(&label).is_none() && topo.host(&target).is_none() {
            out.push_str(&format!("{label} monitor-only {target}\n"));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

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
        let t = parse(V2).unwrap();
        assert_eq!(t.hub, "alpha");
        assert_eq!(t.hosts.len(), 4);
        assert_eq!(t.host("gamma").unwrap(), &HostDecl { name: "gamma".into(), daemon: true, frontend: true });
        assert_eq!(t.host("beta").unwrap(), &HostDecl { name: "beta".into(), daemon: false, frontend: false });
        assert_eq!(t.monitor_targets()[1], ("beta".to_string(), "beta".to_string()));
        assert_eq!(t.monitor_targets()[2].1, "other-user@gpu-box");
        assert!(t.warnings.is_empty());
        assert_eq!(t.local_port("alpha"), Some(18743));
        assert_eq!(t.local_port("beta"), Some(18744));
        assert_eq!(t.local_port("delta"), Some(18746));
    }

    #[test]
    fn two_hubs_fail() {
        let e = parse("hub = \"a\"\nhub = \"b\"\n[host.a]\n").unwrap_err();
        assert!(e.contains("line 2") && e.contains("`hub` declared twice"), "{e}");
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

    #[test]
    fn v1_parses_with_warnings() {
        let v1 = r#"
default_host = "alpha"

[host.alpha]
ssh_alias = "alpha"
remote_repo = "/home/me/project"
tcp_port = 18743
remote_socket = "/run/user/1000/sot/sessions/sot.sock"
remote_home = "/home/me"

[host.beta]
ssh_alias = "beta"
tcp_port = 18744
"#;
        let t = parse(v1).unwrap();
        assert_eq!(t.hub, "alpha");
        assert!(t.host("alpha").unwrap().daemon && t.host("beta").unwrap().daemon);
        assert_eq!(t.warnings.len(), 8, "{:?}", t.warnings);
        assert!(t.warnings[0].contains("`default_host` is `hub` now"));
        assert!(t.warnings.iter().any(|w| w.contains("`tcp_port` is ignored")));
        let e = parse("default_host = \"a\"\n[host.a]\nssh_alias = \"other\"\n").unwrap_err();
        assert!(e.contains("`ssh_alias = \"other\"` differs from the section key `a`"), "{e}");
    }

    #[test]
    fn plan_lines_are_stable() {
        let t = parse(V2).unwrap();
        let own = local_endpoint("sot");
        assert_eq!(
            plan(&t, "gamma").unwrap(),
            format!(
                "self gamma\nhub alpha\nrelay-endpoint tcp:127.0.0.1:18743\n\
                 dial alpha tcp:127.0.0.1:18743\ndial gamma {own}\ntunnel alpha 18743\n"
            )
        );
        assert_eq!(
            plan(&t, "alpha").unwrap(),
            format!("self alpha\nhub alpha\nrelay-endpoint {own}\ndial alpha {own}\ndial gamma tcp:127.0.0.1:18745\ntunnel gamma 18745\n")
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

    #[test]
    fn locate_honours_sot_hosts() {
        // Env is process-global; this test only reads the override path.
        std::env::set_var("SOT_HOSTS", "/nowhere/hosts.toml");
        assert_eq!(locate(), Some(PathBuf::from("/nowhere/hosts.toml")));
        std::env::remove_var("SOT_HOSTS");
    }
}
