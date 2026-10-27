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

/// This host's entry in `sotd topology status`'s table (read by `sot_topology::parse_status`), or `None` when the table
/// does not list it. A frontend runs its own daemon.
pub(super) fn from_status(rows: &[sot_topology::StatusRow], me: &str) -> Option<Role> {
    rows.iter().find_map(|row| match row {
        sot_topology::StatusRow::Host { name, daemon, frontend, .. } if name == me => Some(Role { daemon: *daemon || *frontend, frontend: *frontend }),
        _ => None,
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

/// The command the shipped template's ExecStart gives systemd, up to the program it execs (`deploy/sotd.service`, v0.6.0
/// on), as `systemctl show` prints its arguments: joined by spaces, the single quotes systemd removed.
const BASH_FORM: &str = "/bin/bash -c [ -r \"$HOME/.bashrc\" ] && . \"$HOME/.bashrc\"; exec \"";

/// The program `sotd.service` runs, from `systemctl --user show -p ExecStart --value sotd.service`: systemd's own reading
/// of the unit and its drop-ins, one `{ path=<program> ; argv[]=<arguments joined by spaces> ; ignore_errors=... }` per
/// command. It reads in each form a release shipped:
/// - v0.3.0 and v0.3.2 (`ExecStart=@SOT_BIN@ ...`): the owner is `path=` when it is `argv[0]` and its file name is
///   `sotd`, whatever the flags;
/// - v0.6.0 on (`ExecStart=/bin/bash -c '...; exec "@SOT_BIN@" --project-root "@SOT_PROJECT_ROOT@" --label sot'`):
///   `path=/bin/bash` and `argv[]` exactly that command as the library renders it, the program and the project root
///   holding no quote; the owner is the program it execs.
/// Anything else, and more than one command, is refused, saying why. Limit: systemd's reading is of the unit as last
/// loaded, so an edit not yet reloaded is read as it was.
pub(super) fn unit_owner_shown(record: &str) -> Result<String, String> {
    let commands: Vec<&str> = record.lines().map(str::trim).filter(|l| !l.is_empty()).collect();
    let command = match commands.as_slice() {
        [one] if one.matches("{ path=").count() == 1 => *one,
        [] => return Err("runs no command".to_string()),
        _ => return Err(format!("runs more than one command ('{}')", commands.join("; "))),
    };
    let fields = command.strip_prefix("{ path=").and_then(|f| f.strip_suffix(" }")).ok_or_else(|| format!("is no command record ('{command}')"))?;
    let (path, rest) = fields.split_once(" ; argv[]=").ok_or_else(|| format!("names no arguments ('{command}')"))?;
    let argv = rest.rsplit_once(" ; ignore_errors=").map(|(argv, _)| argv).ok_or_else(|| format!("is no command record ('{command}')"))?;
    let sotd = |p: &str| p.starts_with('/') && Path::new(p).file_name().is_some_and(|n| n == "sotd");
    if sotd(path) && (argv == path || argv.starts_with(&format!("{path} "))) {
        return Ok(path.to_string());
    }
    if path == "/bin/bash" {
        let shipped = argv.strip_prefix(BASH_FORM).and_then(|exec| exec.split_once('"')).and_then(|(program, rest)| {
            let root = rest.strip_prefix(" --project-root \"")?.strip_suffix("\" --label sot")?;
            (sotd(program) && !root.contains('"')).then(|| program.to_string())
        });
        if let Some(program) = shipped {
            return Ok(program);
        }
    }
    Err(format!("runs {path} as no shipped sotd.service does ('{argv}')"))
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

/// The ownership gate over the files an install writes: the unit (when it wires one; `unit_owner` is the program
/// systemd reads it to run, asked whenever its file exists), the wrapper, and the desktop entry (Linux) or app bundle
/// (macOS), each this install's, another's (consent), or no one's that can be named (never overwritten).
pub(super) fn ownership(running: Option<&str>, unit_owner: Option<&str>, home: &Path, prefix: &str, macos: bool, frontend: bool, wires_unit: bool, force: bool) -> Gate {
    let gate = running_daemon(running, prefix, force);
    if gate != Gate::Allow {
        return gate;
    }
    // The unit, running here or not (its owner as systemd reads it, [`unit_owner_shown`]): on a home several hosts
    // share it is what they start.
    if let (true, Some(owner)) = (wires_unit, unit_owner) {
        if !force && !owner.starts_with(&format!("{prefix}/")) {
            let unit = home.join(".config/systemd/user/sotd.service");
            return Gate::Refuse(format!("{} runs {owner}, another install's sotd; this install targets {prefix}/bin/sotd", unit.display()));
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
