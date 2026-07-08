// hosts.rs — host registry parsing for in-app target selection (ADR 0015).
//
// The frontend reads `.sot/hosts.toml` (or its layered fallbacks) at
// startup so `Mode::Hosts` can render a picker. Selecting an entry
// persists `last_host` in `state-<hostname>.toml`; the launcher reads
// that on next launch and aims the SSH tunnel + spawn at the chosen
// host. We don't manage tunnels from Rust — the launcher does that.
//
// Format (deliberately simple — same shape PowerShell can regex-parse):
//
//   default_host = "myserver"
//
//   [host.myserver]
//   ssh_alias   = "myserver"
//   remote_repo = "/home/me/project"
//   tcp_port    = 18743
//   remote_socket = "/run/user/<uid>/sot/sessions/sot.sock"  # optional override
//
//   [host.local]
//   socket = "\\\\.\\pipe\\sot-local"
//
// Missing/malformed files resolve to an empty registry; the chrome
// just shows "no hosts configured" in the picker.

use std::collections::HashMap;
use std::path::PathBuf;

/// One configured host. Remote hosts use `tcp_port` for the local side of the
/// SSH forward, with `ssh_alias` + `remote_repo` telling the launcher where to
/// find/start the daemon; `remote_socket` optionally overrides the remote side
/// of that forward. Local hosts use `socket` directly. Both `tcp_port` and
/// `socket` being set is treated as "tcp wins"; both missing means the entry
/// is unusable and the picker renders it dimmed.
#[derive(Debug, Clone)]
pub struct HostEntry {
    /// Slug as it appears under `[host.<name>]`. Matched against
    /// `last_host` in state-toml on launcher startup.
    pub name: String,
    /// SSH alias the launcher passes to `ssh` (host as `~/.ssh/config`
    /// knows it, not necessarily the FQDN). `None` for local-socket
    /// hosts.
    pub ssh_alias: Option<String>,
    /// Path on the remote where the launcher pkill+nohup's the daemon.
    /// `None` for local-socket hosts.
    pub remote_repo: Option<String>,
    /// Loopback port the launcher SSH-forwards from local to remote.
    /// `None` for local-socket hosts.
    pub tcp_port: Option<u16>,
    /// Remote Unix socket path for the SSH forward's remote side. If absent,
    /// launchers query `sotd session-socket-path sot` on the remote host.
    pub remote_socket: Option<String>,
    /// Local socket / named pipe path for local-only hosts.
    pub socket: Option<String>,
    /// User home on the remote — `/home/<user>` for Linux, `/Users/<user>`
    /// for macOS, `C:/Users/<user>` for Windows. Used as the default
    /// root for the workspace-create picker (Sessions mode → `[+ create
    /// new]`) so it lands in the user's home instead of the filesystem
    /// root. `None` falls through to "/" as before.
    pub remote_home: Option<String>,
}

/// Full hosts.toml content: ordered list of entries (as they appear on
/// disk so the picker has stable ordering) plus the default-host slug
/// the launcher falls back to when state-toml has no `last_host`.
#[derive(Debug, Clone, Default)]
pub struct HostsConfig {
    pub default_host: Option<String>,
    pub hosts: Vec<HostEntry>,
}

impl HostsConfig {
    /// Look up a host entry by name; `None` if absent. Returned by
    /// reference so the caller can clone what they need.
    pub fn find(&self, name: &str) -> Option<&HostEntry> {
        self.hosts.iter().find(|h| h.name == name)
    }
}

/// Read the hosts toml from each candidate location in priority order.
/// First file that exists wins; later candidates aren't merged in. A
/// malformed file at a higher-priority path still wins (and resolves
/// to defaults) — same fail-soft pattern as the settings loader.
pub fn load() -> HostsConfig {
    for path in candidate_paths() {
        if let Ok(text) = std::fs::read_to_string(&path) {
            tracing::info!(path = ?path, "hosts.toml loaded");
            return parse(&text);
        }
    }
    tracing::info!("no hosts.toml found; registry is empty");
    HostsConfig::default()
}

/// Layered discovery, highest priority first:
///   1. $SOT_HOSTS (explicit override)
///   2. <repo-root>/.sot/hosts.toml
///   3. $XDG_CONFIG_HOME/sot/hosts.toml (Linux/Mac)
///   4. $HOME/.config/sot/hosts.toml (Linux/Mac fallback)
///   5. %APPDATA%/sot/hosts.toml (Windows)
fn candidate_paths() -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Some(p) = std::env::var_os("SOT_HOSTS") {
        out.push(PathBuf::from(p));
    }
    if let Ok(cwd) = std::env::current_dir() {
        out.push(cwd.join(".sot").join("hosts.toml"));
    }
    if let Some(p) = std::env::var_os("XDG_CONFIG_HOME") {
        let base = PathBuf::from(p);
        out.push(base.join("sot").join("hosts.toml"));
    }
    if let Some(p) = std::env::var_os("HOME") {
        let cfg = PathBuf::from(p).join(".config");
        out.push(cfg.join("sot").join("hosts.toml"));
    }
    if let Some(p) = std::env::var_os("APPDATA") {
        let base = PathBuf::from(p);
        out.push(base.join("sot").join("hosts.toml"));
    }
    out
}

/// Parse the hosts.toml format. Tolerant: unknown keys ignored,
/// malformed numbers fall back to None, missing sections silently
/// dropped. The frontend re-renders the picker on a fresh load so
/// editing the file + relaunching is the iteration loop.
pub fn parse(text: &str) -> HostsConfig {
    let mut cfg = HostsConfig::default();
    let mut current_section: Option<String> = None;
    let mut current_kv: HashMap<String, String> = HashMap::new();
    let mut order: Vec<String> = Vec::new();

    let flush = |cfg: &mut HostsConfig, section: &Option<String>, kv: &HashMap<String, String>| {
        if let Some(name) = section.as_deref() {
            // Only `[host.<name>]` sections become entries; bare
            // sections (we don't have any today, but reserved)
            // are ignored.
            if let Some(host_name) = name.strip_prefix("host.") {
                let entry = HostEntry {
                    name: host_name.to_string(),
                    ssh_alias: kv.get("ssh_alias").cloned(),
                    remote_repo: kv.get("remote_repo").cloned(),
                    tcp_port: kv.get("tcp_port").and_then(|s| s.parse::<u16>().ok()),
                    remote_socket: kv.get("remote_socket").cloned(),
                    socket: kv.get("socket").cloned(),
                    remote_home: kv.get("remote_home").cloned(),
                };
                cfg.hosts.push(entry);
            }
        }
    };

    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some(stripped) = line.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
            // Flush previous section before switching.
            flush(&mut cfg, &current_section, &current_kv);
            current_kv.clear();
            current_section = Some(stripped.trim().to_string());
            if let Some(h) = stripped.trim().strip_prefix("host.") {
                if !order.iter().any(|n| n == h) {
                    order.push(h.to_string());
                }
            }
            continue;
        }
        let Some((k, v)) = line.split_once('=') else {
            continue;
        };
        let k = k.trim();
        let v = strip_quotes(v.trim());
        if current_section.is_none() {
            // Top-level scalar; default_host is the only one we
            // recognise today.
            if k == "default_host" {
                cfg.default_host = Some(v.to_string());
            }
            continue;
        }
        current_kv.insert(k.to_string(), v.to_string());
    }
    // Flush the final section.
    flush(&mut cfg, &current_section, &current_kv);
    cfg
}

fn strip_quotes(s: &str) -> &str {
    let s = s.trim();
    let b = s.as_bytes();
    if b.len() >= 2 && b[0] == b'"' && b[b.len() - 1] == b'"' {
        &s[1..s.len() - 1]
    } else {
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_empty_returns_defaults() {
        let cfg = parse("");
        assert!(cfg.default_host.is_none());
        assert!(cfg.hosts.is_empty());
    }

    #[test]
    fn parse_minimal_tcp_host() {
        let cfg = parse(
            r#"
default_host = "host-a"

[host.host-a]
ssh_alias = "host-a"
remote_repo = "/home/user/ship-of-tools"
tcp_port = 18743
remote_socket = "/run/user/4242/sot/sessions/sot.sock"
"#,
        );
        assert_eq!(cfg.default_host.as_deref(), Some("host-a"));
        assert_eq!(cfg.hosts.len(), 1);
        let h = &cfg.hosts[0];
        assert_eq!(h.name, "host-a");
        assert_eq!(h.ssh_alias.as_deref(), Some("host-a"));
        assert_eq!(h.remote_repo.as_deref(), Some("/home/user/ship-of-tools"));
        assert_eq!(h.tcp_port, Some(18743));
        assert_eq!(
            h.remote_socket.as_deref(),
            Some("/run/user/4242/sot/sessions/sot.sock")
        );
        assert!(h.socket.is_none());
    }

    #[test]
    fn parse_multiple_hosts_preserves_order() {
        // The parser doesn't process TOML escape sequences — values
        // pass through after a `strip_quotes` only. So Windows pipe
        // paths in hosts.toml should be written with single
        // backslashes (the file format isn't strict TOML; that's the
        // contract for now). Documented as a deliberate simplification.
        let cfg = parse(
            r#"
[host.host-a]
ssh_alias = "host-a"
tcp_port = 18743

[host.host-b]
ssh_alias = "host-b"
tcp_port = 18744

[host.local]
socket = "\\.\pipe\sot-local"
"#,
        );
        assert_eq!(cfg.hosts.len(), 3);
        assert_eq!(cfg.hosts[0].name, "host-a");
        assert_eq!(cfg.hosts[1].name, "host-b");
        assert_eq!(cfg.hosts[2].name, "local");
        assert_eq!(cfg.hosts[2].socket.as_deref(), Some(r"\\.\pipe\sot-local"));
    }

    #[test]
    fn parse_malformed_port_drops_to_none() {
        let cfg = parse(
            r#"
[host.weird]
tcp_port = not-a-number
"#,
        );
        assert_eq!(cfg.hosts.len(), 1);
        assert!(cfg.hosts[0].tcp_port.is_none());
    }

    #[test]
    fn find_returns_match_or_none() {
        let cfg = parse(
            r#"
[host.host-a]
tcp_port = 18743

[host.host-b]
tcp_port = 18744
"#,
        );
        assert_eq!(cfg.find("host-a").map(|h| h.tcp_port), Some(Some(18743)));
        assert_eq!(cfg.find("host-b").map(|h| h.tcp_port), Some(Some(18744)));
        assert!(cfg.find("nope").is_none());
    }

    #[test]
    fn parse_tolerates_comments_and_blank_lines() {
        let cfg = parse(
            r#"
# top-level comment
default_host = "host-a"  # trailing comment-like text NOT yet supported (kept as part of value)

# section comment
[host.host-a]
# inner comment
ssh_alias = "host-a"
"#,
        );
        // The "trailing comment" goes into the value because we
        // don't strip inline comments. That's the current contract;
        // documented here as a regression guard. Worth fixing if it
        // bites later.
        assert!(cfg.default_host.as_deref().unwrap().contains("host-a"));
        assert_eq!(cfg.hosts.len(), 1);
        assert_eq!(cfg.hosts[0].ssh_alias.as_deref(), Some("host-a"));
    }
}
