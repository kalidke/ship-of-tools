//! What a Unix install puts on this computer, and the gates before it writes (scripts/CLAUDE.md, Installers: "The role
//! is declared"). The role is read from `sotd topology status`'s table (the one parser of hosts.toml), else the flags,
//! else install.json; the gates read files and decide, so a refused install has written nothing. Every decision here
//! is a pure function of what was read.

use std::path::Path;

use super::record;

/// What this computer runs: a daemon, a window (a window always comes with its own daemon).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Role {
    pub daemon: bool,
    pub frontend: bool,
}

/// This host's entry in `sotd topology status`'s table (a header line, then `HOST DECLARED`, the declared words joined
/// by commas), or `None` when the table does not list it.
pub(super) fn from_status(table: &str, me: &str) -> Option<Role> {
    table.lines().skip(1).find_map(|line| {
        let mut words = line.split_whitespace();
        (words.next()? == me).then_some(())?;
        let declared: Vec<&str> = words.next()?.split(',').collect();
        let frontend = declared.contains(&"frontend");
        Some(Role { daemon: frontend || declared.contains(&"daemon"), frontend })
    })
}

/// The role a flag names.
pub(super) fn from_flag(flag: &super::options::RoleFlag) -> Role {
    use super::options::RoleFlag;
    match flag {
        RoleFlag::Local => Role { daemon: true, frontend: true },
        RoleFlag::BeOnly => Role { daemon: true, frontend: false },
        RoleFlag::Remote(_) => Role { daemon: false, frontend: true },
    }
}

/// The role flag an install here recorded: its `daemon` and `frontend` bits (since 0.6.0), else a 0.5 `role` key; a
/// remote install's alias is the `SOT_HOST` line of its own wrapper. `None` when the record names none.
pub(super) fn recorded(record_text: &str, wrapper_text: &str) -> Option<super::options::RoleFlag> {
    use super::options::RoleFlag;
    let role = match (record::boolean(record_text, "daemon"), record::boolean(record_text, "frontend")) {
        (Some(true), Some(true)) => "local".to_string(),
        (Some(true), Some(false)) => "be-only".to_string(),
        (Some(false), Some(true)) => "remote".to_string(),
        (Some(_), Some(_)) => return None,
        _ => record::string(record_text, "role")?,
    };
    match role.as_str() {
        "local" => Some(RoleFlag::Local),
        "be-only" => Some(RoleFlag::BeOnly),
        "remote" => {
            let alias = wrapper_text.lines().find_map(|l| l.strip_prefix("export SOT_HOST=\"")?.strip_suffix('"'))?;
            (!alias.is_empty()).then(|| RoleFlag::Remote(alias.to_string()))
        }
        _ => None,
    }
}

/// What install.json's `service` records for a run: `systemd` when it installs the unit (Linux, a daemon, no
/// `--no-service`), else `none`.
pub(super) fn service_record(linux: bool, daemon: bool, no_service: bool) -> &'static str {
    if linux && daemon && !no_service { "systemd" } else { "none" }
}

/// ExecStart's program from a unit's text, in both shapes: `ExecStart=<bin>/sotd ...` and the `bash -c '...; exec
/// "<bin>/sotd" ...'` the template writes.
pub(super) fn unit_owner_path(unit: &str) -> Option<String> {
    let line = unit.lines().find_map(|l| l.strip_prefix("ExecStart="))?;
    let rest = match line.rfind("exec \"") {
        Some(at) => &line[at + "exec \"".len()..],
        None => line,
    };
    let path: String = rest.chars().take_while(|c| *c != ' ' && *c != '"').collect();
    (!path.is_empty()).then_some(path)
}

/// The prefix a `sot-launch` wrapper's text names, in its three known shapes (the all-in-one's `PENDING=`, the remote
/// one's `SOT_FRONTEND_BIN=`, the legacy one-line `exec`); `None` for any other text: an unknown wrapper is never taken
/// for this install's.
pub(super) fn wrapper_owner_prefix(text: &str) -> Option<String> {
    text.lines().find_map(|l| {
        if let Some(v) = l.strip_prefix("PENDING=\"").and_then(|r| r.strip_suffix('"')) {
            return v.rfind("/updates/pending-").map(|at| v[..at].to_string()).filter(|p| !p.is_empty());
        }
        if let Some(v) = l.strip_prefix("export SOT_FRONTEND_BIN=\"").and_then(|r| r.strip_suffix('"')) {
            return v.strip_suffix("/bin/sot").map(String::from).filter(|p| !p.is_empty());
        }
        let v = l.strip_prefix("exec \"")?;
        v.rfind("/bin/sot\"").map(|at| v[..at].to_string()).filter(|p| !p.is_empty())
    })
}

/// The program a desktop entry's `Exec=` or an app bundle's `exec "..."` runs.
pub(super) fn exec_target(text: &str) -> Option<String> {
    text.lines().find_map(|l| l.strip_prefix("Exec=").map(String::from).or_else(|| l.strip_prefix("exec \"")?.strip_suffix('"').map(String::from)))
}

/// A gate's answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Gate {
    Allow,
    /// Another install's: `--force-role-change` consents.
    Refuse(String),
    /// No owner can be named, so nothing consents: move it aside.
    Unresolvable(String),
}

/// A running `sotd.service` whose program is `running` may be taken over by an install at `prefix`: its own, or with
/// consent.
pub(super) fn running_daemon(running: Option<&str>, prefix: &str, force: bool) -> Gate {
    match running {
        Some(r) if !force && !r.starts_with(&format!("{prefix}/")) => {
            Gate::Refuse(format!("the sotd.service running for this user runs {r}; this install targets {prefix}/bin/sotd"))
        }
        _ => Gate::Allow,
    }
}

/// The ownership gate over the files an install writes: the unit (when it wires one), the wrapper, and the desktop entry
/// (Linux) or app bundle (macOS), each this install's, another's (consent), or no one's that can be named (never
/// overwritten).
pub(super) fn ownership(running: Option<&str>, home: &Path, prefix: &str, macos: bool, frontend: bool, wires_unit: bool, force: bool) -> Gate {
    let gate = running_daemon(running, prefix, force);
    if gate != Gate::Allow {
        return gate;
    }
    // The unit file, running here or not: on a home several hosts share it is what they start.
    let unit = home.join(".config/systemd/user/sotd.service");
    if let (true, Ok(text)) = (wires_unit, std::fs::read_to_string(&unit)) {
        match unit_owner_path(&text) {
            None => return Gate::Unresolvable(format!("{} exists but names no program — move it aside and re-run", unit.display())),
            Some(owner) if !force && !owner.starts_with(&format!("{prefix}/")) => {
                return Gate::Refuse(format!("{} runs {owner}, another install's sotd; this install targets {prefix}/bin/sotd", unit.display()));
            }
            Some(_) => {}
        }
    }
    if !frontend {
        return gate;
    }
    let wrapper = home.join(".local/bin/sot-launch");
    let wrapper_s = wrapper.to_string_lossy().into_owned();
    if let Ok(text) = std::fs::read_to_string(&wrapper) {
        match wrapper_owner_prefix(&text) {
            None => return Gate::Unresolvable(format!("{wrapper_s} exists but its owner could not be determined — move it aside and re-run")),
            Some(owner) if owner != prefix && !force => {
                return Gate::Refuse(format!("{wrapper_s} belongs to the install at {owner}; this install targets {prefix}"));
            }
            Some(_) => {}
        }
    }
    let launcher = if macos { home.join("Applications/Ship of Tools.app/Contents/MacOS/sot-launch") } else { home.join(".local/share/applications/ship-of-tools.desktop") };
    if let Ok(text) = std::fs::read_to_string(&launcher) {
        if exec_target(&text).as_deref() != Some(wrapper_s.as_str()) {
            return Gate::Unresolvable(format!("{} exists but does not launch this install's wrapper — move it aside and re-run", launcher.display()));
        }
    }
    Gate::Allow
}

/// The shared install is written only from a host the declared topology runs sotd on: under a readable hosts.toml that
/// runs none here, a run over an install already at the prefix is refused.
pub(super) fn host(readable: bool, declared_daemon: bool, prefix: &Path, force: bool) -> Gate {
    let installed = prefix.join("install.json").is_file() || prefix.join("bin/sotd").exists();
    if declared_daemon || !readable || force || !installed {
        return Gate::Allow;
    }
    Gate::Refuse(format!("hosts.toml runs no sotd on this host, and {} already holds an install", prefix.display()))
}

/// Under a declared topology a run records no less than install.json does: a daemon, or its unit.
pub(super) fn record_gate(record_path: &Path, declared: bool, daemon: bool, service: &str, force: bool) -> Gate {
    let Ok(text) = std::fs::read_to_string(record_path) else { return Gate::Allow };
    if !declared || force {
        return Gate::Allow;
    }
    let shown = record_path.display();
    if !daemon && record::boolean(&text, "daemon") == Some(true) {
        return Gate::Refuse(format!("{shown} records an install that runs a daemon, and this run would record none"));
    }
    if service != "systemd" && record::string(&text, "service").as_deref() == Some("systemd") {
        return Gate::Refuse(format!("{shown} records an install whose daemon runs as a systemd unit, and this run would record none"));
    }
    Gate::Allow
}

/// Whether a `sotd topology plan` dials a daemon host other than its own `self` line.
pub(super) fn plan_dials_others(plan: &str) -> bool {
    let me = plan.lines().find_map(|l| l.strip_prefix("self ")).map(str::trim).unwrap_or("");
    plan.lines().filter_map(|l| l.strip_prefix("dial ")).filter_map(|r| r.split_whitespace().next()).any(|h| !h.is_empty() && h != me)
}

/// Whether a `sotd.service` host pin under `dir` names `host` and no other: the pin a claim writes with no hosts.toml.
pub(super) fn pin_names_only(dir: &Path, host: &str) -> bool {
    let Ok(text) = std::fs::read_to_string(dir.join("sotd.service.d/topology.conf")) else { return false };
    let lines: Vec<&str> = text.lines().filter(|l| l.starts_with("ConditionHost=")).collect();
    !host.is_empty() && lines == [format!("ConditionHost=|{host}"), format!("ConditionHost=|{host}.*")]
}

/// The hosts a `sotd.service` host pin under `dir` names, from its `ConditionHost=|<host>` lines.
pub(super) fn pin_hosts(dir: &Path) -> Vec<String> {
    let Ok(text) = std::fs::read_to_string(dir.join("sotd.service.d/topology.conf")) else { return Vec::new() };
    text.lines()
        .filter_map(|l| l.strip_prefix("ConditionHost=|"))
        .filter(|h| !h.is_empty() && !h.contains('*') && !h.ends_with('.'))
        .map(String::from)
        .collect()
}

#[cfg(test)]
#[path = "role_tests.rs"]
mod tests;
