// dial.rs — the frontend's connection set, from `--dial` args only (topology
// plan, lane D). Replaces the old `hosts.rs` (ADR 0015 registry format,
// ADR 0042 L2a targeting model): that module read `.sot/hosts.toml` (or one
// of four fallback paths) and parsed it itself — a SECOND parser for the
// same file `sot_protocol::topology` now owns as the one grammar-v2 reader
// (module doc there). The frontend reads no config file for hosts at all:
// `sotd topology plan --self <host>` computes the dial set, and the
// launcher hands it over as repeated `--dial <host>=<endpoint>` flags
// (scripts/launch-sot.ps1 / relaunch-sot.ps1 / scripts/sot-hosts.ps1).
//
// `<endpoint>` carries the same `unix:`/`pipe:`/`ssh:` scheme
// `sot_protocol::topology::endpoint::local_endpoint`/`relay_endpoint` already speak —
// a path after `unix:`/`pipe:`, `<target>[/<host>]` after `ssh:` (C3 as
// amended, isolation-plan.md §3).
//
// We don't manage tunnels from Rust — reaching a daemon that isn't this
// box's own means spawning an ssh child directly (`sot_protocol::topology::
// ssh_bridge`), never a forward to dial afterward. A host with no reachable
// endpoint just isn't in the list; there is nothing here to mark
// "unreachable but configured" the way hosts.toml entries with no
// socket/tcp_port used to be skipped-with-a-log — an omitted `--dial` is
// the caller's own choice, not a parse failure.

use std::path::PathBuf;
use sot_protocol::topology::endpoint::is_plain_host_name;

/// A dialed host's name — identifies which daemon connection owns a
/// workspace, a Sessions-tree host node, or an `IncomingEvt`. A plain
/// `String` alias (not a newtype): every call site treats it as an opaque
/// display + lookup key (ADR 0042 L2a).
pub type HostKey = String;

/// Endpoint override for the implicit `"local"` connection — sourced from
/// the CLI `--socket`/`--token` flags. These are the ad hoc/manual path
/// (dev, tests, a box with no launcher/topology at all); the launcher
/// itself never needs them, since `sotd topology plan` hands it this box's
/// own endpoint as an ordinary `--dial <self>=<endpoint>` entry. There is
/// no `--tcp` twin: the daemon has had no TCP listener since 0.4.0, so a
/// raw address was never dialable on its own — the ad hoc path for a
/// remote box is `--dial local=ssh:<target>`, the same override shape
/// `resolve_connections` below already gives an explicit `--dial local=…`.
#[derive(Debug, Clone, Default)]
pub struct CliOverride {
    pub socket: Option<PathBuf>,
    pub token: Option<String>,
}

/// Parse one `--dial <host>=<endpoint>` argument. `host` must look like a
/// plain host name (`[a-z0-9][a-z0-9._-]*` — the same grammar
/// `sotd topology plan` emits it in); `endpoint` must carry a
/// `unix:`/`pipe:`/`ssh:` scheme naming a non-empty path or ssh target.
/// Anything else is an `Err` describing the problem; the caller logs it and
/// skips the entry rather than aborting the whole list — one malformed
/// `--dial` shouldn't take down every other host's connection.
pub fn parse_dial_arg(arg: &str) -> Result<(HostKey, crate::net::transport::TransportConfig), String> {
    let Some((host, endpoint)) = arg.split_once('=') else {
        return Err(format!("`--dial {arg}`: expected `<host>=<endpoint>`"));
    };
    if host.is_empty() || !is_plain_host_name(host) {
        return Err(format!("`--dial {arg}`: `{host}` is not a plain host name"));
    }
    let config = if let Some(path) = endpoint
        .strip_prefix("unix:")
        .or_else(|| endpoint.strip_prefix("pipe:"))
    {
        if path.is_empty() {
            return Err(format!("`--dial {arg}`: empty socket path"));
        }
        // Unix parsing classifies a generated hub-relay path once; everything else keeps the local socket's policy.
        let path = PathBuf::from(path);
        let relay = endpoint.starts_with("unix:")
            && sot_protocol::topology::relay_host_for_path(&path).is_some();
        crate::net::transport::TransportConfig {
            dial: if relay {
                crate::net::transport::Dial::Relay(path)
            } else {
                crate::net::transport::Dial::Pipe(path)
            },
            token: None,
        }
    } else if let Some(rest) = endpoint.strip_prefix("ssh:") {
        if rest.is_empty() {
            return Err(format!("`--dial {arg}`: empty ssh target"));
        }
        // `ssh:<target>` for that box's own daemon, `ssh:<target>/<host>`
        // for a daemon `<target>` (always the hub) relays to on `<host>`'s
        // behalf — `sotd topology plan`'s own two forms (`topology/mod.rs`).
        let (target, ssh_host) = match rest.split_once('/') {
            Some((t, h)) => (t, Some(h)),
            None => (rest, None),
        };
        let recipe = sot_protocol::topology::ssh_bridge::SshRecipe::new(target, ssh_host)
            .map_err(|e| format!("`--dial {arg}`: {e}"))?;
        crate::net::transport::TransportConfig {
            dial: crate::net::transport::Dial::Ssh(recipe),
            token: None,
        }
    } else {
        return Err(format!(
            "`--dial {arg}`: endpoint `{endpoint}` has no unix:/pipe:/ssh: scheme"
        ));
    };
    Ok((host.to_string(), config))
}

/// The startup connection set: every `--dial` entry (argument order; a
/// repeated host name has its LAST occurrence win), plus one `"local"`
/// connection synthesized from `--socket`/`--token` when given —
/// overriding an explicit `--dial local=...` entry outright (the CLI flags
/// are the ad hoc override, same meaning they always had pre-topology).
/// `"local"` sorts first when present, matching the old implicit-local
/// display order (ADR 0042 L2b) callers still rely on.
///
/// A later `--dial` claiming an `ssh:` endpoint another host already
/// claimed is skipped (one log line naming both) — two hosts resolving to
/// the SAME `ssh:<target>[/<host>]` recipe would otherwise both connect to
/// the same daemon, and the second one's label would silently reach the
/// first one's connection (`record_declared_host`/`drain_events` in
/// `ui/connections.rs` and `ui/events.rs` lean on this: it's the reason two dials can never land on the
/// same daemon in the first place — there is no shutdown path to close a
/// newcomer transport that collides after the fact). A well-formed `sotd
/// topology plan` never emits this — every host gets a distinct recipe —
/// so this only fires on a hand-built or stale `--dial` list. Was a
/// literal `tcp:host:port` string before C3; the same collision, keyed on
/// `SshRecipe`'s own `Display` (`ssh:<target>[/<host>]`) now that a shared
/// loopback port is no longer the thing two dials could collide on.
///
/// No `--dial` and no CLI endpoint yields an EMPTY set — the caller's job
/// (`main.rs`) is to report that plainly and run offline, exactly as a box
/// with no hosts.toml and no `--socket`/`--dial` always has.
pub fn resolve_connections(
    dials: &[(HostKey, crate::net::transport::TransportConfig)],
    cli: &CliOverride,
) -> Vec<(HostKey, crate::net::transport::TransportConfig)> {
    let mut out: Vec<(HostKey, crate::net::transport::TransportConfig)> = Vec::new();
    let mut claimed_ssh: std::collections::HashMap<String, HostKey> =
        std::collections::HashMap::new();
    for (host, config) in dials {
        // A repeated host name: last occurrence wins. Drop the earlier
        // one's ssh claim first so re-dialing the same host on a new
        // endpoint doesn't spuriously collide with itself.
        if let Some(pos) = out.iter().position(|(h, _)| h == host) {
            if let crate::net::transport::Dial::Ssh(recipe) = &out[pos].1.dial {
                claimed_ssh.remove(&recipe.to_string());
            }
            out.remove(pos);
        }
        if let crate::net::transport::Dial::Ssh(recipe) = &config.dial {
            let key = recipe.to_string();
            if let Some(existing) = claimed_ssh.get(&key) {
                tracing::warn!(host = %host, existing_host = %existing, endpoint = %key, "duplicate ssh --dial endpoint with another host; skipping to avoid reaching the wrong daemon");
                continue;
            }
            claimed_ssh.insert(key, host.clone());
        }
        out.push((host.clone(), config.clone()));
    }
    if let Some(socket) = cli.socket.clone() {
        let config = crate::net::transport::TransportConfig {
            dial: crate::net::transport::Dial::Pipe(socket),
            token: cli.token.clone(),
        };
        if let Some(existing) = out.iter_mut().find(|(h, _)| h == "local") {
            existing.1 = config;
        } else {
            out.insert(0, ("local".to_string(), config));
        }
    }
    if let Some(idx) = out.iter().position(|(h, _)| h == "local") {
        if idx != 0 {
            let local = out.remove(idx);
            out.insert(0, local);
        }
    }
    out
}

#[cfg(test)]
#[path = "dial_tests.rs"]
mod tests;
