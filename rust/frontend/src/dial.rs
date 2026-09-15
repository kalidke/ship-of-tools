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
// `<endpoint>` carries the same `unix:`/`pipe:`/`tcp:` scheme
// `sot_protocol::topology::local_endpoint`/`relay_endpoint` already speak —
// a path after `unix:`/`pipe:`, a `host:port` after `tcp:`.
//
// We don't manage tunnels from Rust — the launcher does that (one SSH
// forward per remote daemon host), then dials the resulting loopback ports
// itself via `--dial`. A host with no reachable endpoint just isn't in the
// list; there is nothing here to mark "unreachable but configured" the way
// hosts.toml entries with no socket/tcp_port used to be skipped-with-a-log
// — an omitted `--dial` is the caller's own choice, not a parse failure.

use std::path::PathBuf;

/// A dialed host's name — identifies which daemon connection owns a
/// workspace, a Sessions-tree host node, or an `IncomingEvt`. A plain
/// `String` alias (not a newtype): every call site treats it as an opaque
/// display + lookup key (ADR 0042 L2a).
pub type HostKey = String;

/// Endpoint override for the implicit `"local"` connection — sourced from
/// the CLI `--socket`/`--tcp`/`--token` flags. These are the ad hoc/manual
/// path (dev, tests, a box with no launcher/topology at all); the launcher
/// itself never needs them, since `sotd topology plan` hands it this box's
/// own endpoint as an ordinary `--dial <self>=<endpoint>` entry.
#[derive(Debug, Clone, Default)]
pub struct CliOverride {
    pub socket: Option<PathBuf>,
    pub tcp: Option<String>,
    pub token: Option<String>,
}

/// Parse one `--dial <host>=<endpoint>` argument. `host` must look like a
/// plain host name (`[a-z0-9][a-z0-9._-]*` — the same grammar
/// `sotd topology plan` emits it in); `endpoint` must carry a
/// `unix:`/`pipe:`/`tcp:` scheme naming a non-empty path or `host:port`.
/// Anything else is an `Err` describing the problem; the caller logs it and
/// skips the entry rather than aborting the whole list — one malformed
/// `--dial` shouldn't take down every other host's connection.
pub fn parse_dial_arg(arg: &str) -> Result<(HostKey, crate::transport::TransportConfig), String> {
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
        crate::transport::TransportConfig {
            pipe: Some(PathBuf::from(path)),
            tcp: None,
            token: None,
        }
    } else if let Some(addr) = endpoint.strip_prefix("tcp:") {
        if addr.is_empty() {
            return Err(format!("`--dial {arg}`: empty tcp address"));
        }
        crate::transport::TransportConfig {
            pipe: None,
            tcp: Some(addr.to_string()),
            token: None,
        }
    } else {
        return Err(format!(
            "`--dial {arg}`: endpoint `{endpoint}` has no unix:/pipe:/tcp: scheme"
        ));
    };
    Ok((host.to_string(), config))
}

/// Same grammar `sot_protocol::topology`'s (private) `is_plain_host_name`
/// checks a listed host against — kept as its own copy here because that
/// one isn't `pub`, and a `--dial` host name is the frontend's own CLI
/// surface, not a re-parse of `sotd topology plan`'s output (that parsing
/// stays in exactly one function, in the launcher script that reads it).
fn is_plain_host_name(s: &str) -> bool {
    let mut chars = s.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_lowercase() || c.is_ascii_digit())
        && chars
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '.' | '_' | '-'))
}

/// The startup connection set: every `--dial` entry (argument order; a
/// repeated host name has its LAST occurrence win), plus one `"local"`
/// connection synthesized from `--socket`/`--tcp`/`--token` when given —
/// overriding an explicit `--dial local=...` entry outright (the CLI flags
/// are the ad hoc override, same meaning they always had pre-topology).
/// `"local"` sorts first when present, matching the old implicit-local
/// display order (ADR 0042 L2b) callers still rely on.
///
/// A later `--dial` claiming a `tcp:` endpoint another host already claimed
/// is skipped (one log line naming both) — two loopback-forward hosts on
/// the same port would otherwise both resolve to `127.0.0.1:<port>`, and
/// the second one's label would silently reach the first one's daemon
/// (`record_declared_host`/`drain_events` in `gpu.rs` lean on this: it's
/// the reason two dials can never land on the same daemon in the first
/// place). A well-formed `sotd topology plan` never emits this — its
/// ordinal port series gives every host a distinct port — so this only
/// fires on a hand-built or stale `--dial` list.
///
/// No `--dial` and no CLI endpoint yields an EMPTY set — the caller's job
/// (`main.rs`) is to report that plainly and run offline, exactly as a box
/// with no hosts.toml and no `--socket`/`--tcp` always has.
pub fn resolve_connections(
    dials: &[(HostKey, crate::transport::TransportConfig)],
    cli: &CliOverride,
) -> Vec<(HostKey, crate::transport::TransportConfig)> {
    let mut out: Vec<(HostKey, crate::transport::TransportConfig)> = Vec::new();
    let mut claimed_tcp: std::collections::HashMap<String, HostKey> =
        std::collections::HashMap::new();
    for (host, config) in dials {
        // A repeated host name: last occurrence wins. Drop the earlier
        // one's tcp claim first so re-dialing the same host on a new
        // endpoint doesn't spuriously collide with itself.
        if let Some(pos) = out.iter().position(|(h, _)| h == host) {
            if let Some(t) = &out[pos].1.tcp {
                claimed_tcp.remove(t);
            }
            out.remove(pos);
        }
        if let Some(t) = &config.tcp {
            if let Some(existing) = claimed_tcp.get(t) {
                tracing::warn!(host = %host, existing_host = %existing, endpoint = %t, "duplicate tcp --dial endpoint with another host; skipping to avoid reaching the wrong daemon");
                continue;
            }
            claimed_tcp.insert(t.clone(), host.clone());
        }
        out.push((host.clone(), config.clone()));
    }
    if cli.socket.is_some() || cli.tcp.is_some() {
        let config = crate::transport::TransportConfig {
            pipe: cli.socket.clone(),
            tcp: cli.tcp.clone(),
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
mod tests {
    use super::*;

    #[test]
    fn parse_dial_arg_good_unix() {
        let (host, cfg) = parse_dial_arg("host-2=unix:/run/user/1234/sot/sessions/sot.sock")
            .expect("valid");
        assert_eq!(host, "host-2");
        assert_eq!(
            cfg.pipe,
            Some(PathBuf::from("/run/user/1234/sot/sessions/sot.sock"))
        );
        assert!(cfg.tcp.is_none());
    }

    #[test]
    fn parse_dial_arg_good_tcp() {
        let (host, cfg) = parse_dial_arg("host-1=tcp:127.0.0.1:18744").expect("valid");
        assert_eq!(host, "host-1");
        assert_eq!(cfg.tcp.as_deref(), Some("127.0.0.1:18744"));
        assert!(cfg.pipe.is_none());
    }

    #[test]
    fn parse_dial_arg_good_pipe() {
        let (host, cfg) = parse_dial_arg(r"host-4=pipe:\\.\pipe\sot-host-4").expect("valid");
        assert_eq!(host, "host-4");
        assert_eq!(cfg.pipe, Some(PathBuf::from(r"\\.\pipe\sot-host-4")));
    }

    #[test]
    fn parse_dial_arg_malformed_missing_equals() {
        assert!(parse_dial_arg("host-2-tcp:127.0.0.1:18744").is_err());
    }

    #[test]
    fn parse_dial_arg_malformed_unknown_scheme() {
        assert!(parse_dial_arg("host-2=ssh:somewhere").is_err());
    }

    #[test]
    fn parse_dial_arg_malformed_empty_endpoint() {
        assert!(parse_dial_arg("host-2=tcp:").is_err());
        assert!(parse_dial_arg("host-2=unix:").is_err());
    }

    #[test]
    fn parse_dial_arg_unknown_host_rejects_bad_syntax() {
        // Uppercase, a leading dash, and an embedded space are all outside
        // the plain-host-name grammar `sotd topology plan` emits hosts in.
        assert!(parse_dial_arg("Descent=tcp:127.0.0.1:18744").is_err());
        assert!(parse_dial_arg("-host-2=tcp:127.0.0.1:18744").is_err());
        assert!(parse_dial_arg("de scent=tcp:127.0.0.1:18744").is_err());
        assert!(parse_dial_arg("=tcp:127.0.0.1:18744").is_err());
    }

    #[test]
    fn resolve_connections_repeated_dial_last_one_wins() {
        let dials = vec![
            parse_dial_arg("host-2=tcp:127.0.0.1:18744").unwrap(),
            parse_dial_arg("host-2=tcp:127.0.0.1:19999").unwrap(),
        ];
        let out = resolve_connections(&dials, &CliOverride::default());
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].1.tcp.as_deref(), Some("127.0.0.1:19999"));
    }

    #[test]
    fn resolve_connections_local_sorts_first() {
        let dials = vec![
            parse_dial_arg("host-2=tcp:127.0.0.1:18744").unwrap(),
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
            socket: None,
            tcp: Some("127.0.0.1:9999".to_string()),
            token: Some("secret".to_string()),
        };
        let out = resolve_connections(&dials, &cli);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].0, "local");
        assert_eq!(out[0].1.tcp.as_deref(), Some("127.0.0.1:9999"));
        assert!(out[0].1.pipe.is_none(), "cli override replaces the stale dial entirely");
    }

    #[test]
    fn resolve_connections_cli_only_synthesizes_local() {
        let cli = CliOverride {
            socket: None,
            tcp: Some("127.0.0.1:18743".to_string()),
            token: None,
        };
        let out = resolve_connections(&[], &cli);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].0, "local");
    }

    #[test]
    fn resolve_connections_duplicate_tcp_endpoint_skips_the_later_host() {
        let dials = vec![
            parse_dial_arg("beta=tcp:127.0.0.1:18744").unwrap(),
            parse_dial_arg("alpha=tcp:127.0.0.1:18744").unwrap(),
        ];
        let out = resolve_connections(&dials, &CliOverride::default());
        let names: Vec<&str> = out.iter().map(|(h, _)| h.as_str()).collect();
        assert_eq!(names, vec!["beta"]);
    }

    #[test]
    fn resolve_connections_no_dial_no_cli_is_empty() {
        let out = resolve_connections(&[], &CliOverride::default());
        assert!(out.is_empty(), "no --dial and no --socket/--tcp must yield no connections at all");
    }
}
