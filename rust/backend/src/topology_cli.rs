//! `sotd topology <plan|status|relay-endpoint|sync|apply>` — what a box
//! derives from the declared topology (`sot_protocol::topology`, the one
//! parser). A pure query arm of `main` (no startup side effects). Output is
//! line-oriented so a shell or PowerShell launcher reads it with `split`;
//! the `plan` line set is documented on `topology::plan` and nowhere else.
//! Warnings from the parser (accepted v1 keys) go to stderr, one line each.

use sot_protocol::topology::{self, Topology};
use std::path::PathBuf;

const USAGE: &str = "\
Usage: sotd topology <subcommand>

  plan [--self <host>]  this box's derived facts, one per line: self, hub,
                        relay-endpoint, dial <host> <endpoint> (every daemon
                        host), tunnel <host> <port> (every daemon host but
                        self; the hub is 18743, others ordinal in file order)
  status                the declared table (HOST DECLARED); live state is
                        version.query on each daemon
  relay-endpoint        SOT_RELAY_ENDPOINT for this box
  sync [--hub <alias>]  fetch the hub's ~/.config/sot/hosts.toml into this
                        box's config dir (refused if it does not parse;
                        nothing written when it equals the local copy)
  apply [--yes]         hub only: converge sot-relay-tunnel@<host> systemd
                        --user instances (enable/disable) plus each one's
                        ConditionHost drop-in with the declared list.
                        Default is a DRY RUN (prints what it would do);
                        --yes runs systemctl for real. --dry-run is
                        accepted too, as the explicit spelling of the
                        default. Refuses on a non-hub box, naming the hub.

The file: $SOT_HOSTS, else <config dir>/hosts.toml (~/.config/sot).";

/// Exit status for `main`.
pub fn run(args: &[String]) -> i32 {
    let sub = args.first().map(String::as_str).unwrap_or("");
    let flag = |name: &str| args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).cloned();
    match sub {
        "plan" => with_topology(true, |t| {
            let me = match flag("--self") { Some(h) => Ok(h), None => self_host() };
            me.and_then(|me| topology::plan(t, &me)).map(|s| print!("{s}"))
        }),
        "status" => with_topology(true, |t| Ok(print!("{}", topology::status_table(t)))),
        // Warnings stay quiet here: the shell profile runs this on every
        // non-interactive ssh, and N stderr lines per remote command is
        // not a warning, it is noise.
        "relay-endpoint" => with_topology(false, |t| {
            self_host().and_then(|me| topology::relay_endpoint(t, &me)).map(|e| println!("{e}"))
        }),
        "sync" => report(sync(flag("--hub"))),
        "apply" => with_topology(true, |t| apply(t, !args.iter().any(|a| a == "--yes"))),
        _ => {
            eprintln!("{USAGE}");
            2
        }
    }
}

fn self_host() -> Result<String, String> {
    sot_log::state_dir::host_name()
}

fn with_topology(warn: bool, f: impl FnOnce(&Topology) -> Result<(), String>) -> i32 {
    let loaded = match topology::load() {
        Ok(Some(x)) => x,
        Ok(None) => {
            let looked = topology::locate().map(|p| p.display().to_string()).unwrap_or_default();
            return report(Err(format!("no hosts.toml at {looked} (run `sotd topology sync --hub <alias>`)")));
        }
        Err(e) => return report(Err(e)),
    };
    if warn {
        for w in &loaded.1.warnings {
            eprintln!("sotd topology: {}: {w}", loaded.0.display());
        }
    }
    report(f(&loaded.1))
}

fn report(r: Result<(), String>) -> i32 {
    match r {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("sotd topology: {e}");
            2
        }
    }
}

/// `ssh <hub> cat ~/.config/sot/hosts.toml` into this box's own copy,
/// tmp + rename, only after the fetched text parses, and only when it
/// differs from the copy here: on a shared-home box the "copy" IS the
/// canonical file, and a rename over it could drop an owner edit made
/// between fetch and rename. The hub alias comes from `--hub`, else from
/// the copy already here.
fn sync(hub: Option<String>) -> Result<(), String> {
    let dest = topology::locate().ok_or("no config dir: set $HOME (or %LOCALAPPDATA%) or $SOT_HOSTS")?;
    let hub = match hub {
        Some(h) => h,
        None => topology::load()?
            .map(|(_, t)| t.hub)
            .ok_or_else(|| format!("no hosts.toml at {} yet: pass --hub <alias>", dest.display()))?,
    };
    let out = std::process::Command::new("ssh")
        .args(["-o", "BatchMode=yes", "-o", "ConnectTimeout=10", &hub, "cat ~/.config/sot/hosts.toml"])
        .output()
        .map_err(|e| format!("ssh {hub}: {e}"))?;
    if !out.status.success() {
        return Err(format!("ssh {hub}: {}", String::from_utf8_lossy(&out.stderr).trim()));
    }
    let text = String::from_utf8(out.stdout).map_err(|_| format!("hub `{hub}`: hosts.toml is not UTF-8"))?;
    let fetched = topology::parse(&text).map_err(|e| format!("hub `{hub}`: fetched hosts.toml rejected, keeping the local copy: {e}"))?;
    if self_host().as_deref() == Ok(fetched.hub.as_str()) {
        return Err(format!("this box is the hub `{}`; its file is the canonical copy", fetched.hub));
    }
    if std::fs::read_to_string(&dest).ok().as_deref() == Some(text.as_str()) {
        println!("{} is current (same as {hub})", dest.display());
        return Ok(());
    }
    write_atomic(&dest, &text)?;
    println!("synced {} from {hub} ({} hosts, {} monitor targets)", dest.display(), fetched.hosts.len(), fetched.monitor.len());
    for w in &fetched.warnings {
        eprintln!("sotd topology: {}: {w}", dest.display());
    }
    Ok(())
}

fn write_atomic(dest: &PathBuf, text: &str) -> Result<(), String> {
    let io = |e: std::io::Error| format!("{}: {e}", dest.display());
    if let Some(dir) = dest.parent() {
        std::fs::create_dir_all(dir).map_err(io)?;
    }
    let tmp = dest.with_extension("toml.tmp");
    std::fs::write(&tmp, text).map_err(io)?;
    std::fs::rename(&tmp, dest).map_err(io)
}

/// `sotd topology apply` (plan §C, §F step 5): on the hub, converge the
/// `sot-relay-tunnel@<host>` systemd --user instances (plus each one's
/// ConditionHost drop-in, `topology::tunnel_dropin`) with the declared
/// list. Refuses off the hub (`topology::require_hub`). `dry_run` prints
/// every action without touching systemd or the filesystem — the default
/// (ADR 0028 units forward a live relay port; this box silently flipping
/// which remotes it tunnels to is not something `apply` does on its own
/// say-so). Pass `--yes` to actually run it.
fn apply(topo: &Topology, dry_run: bool) -> Result<(), String> {
    let me = self_host()?;
    topology::require_hub(topo, &me)?;
    let enabled_now = enabled_tunnel_hosts()?;
    let plan = topology::apply_plan(topo, &enabled_now);
    if plan.enable.is_empty() && plan.disable.is_empty() {
        println!("up to date: {} tunnel instance(s)", topology::tunnel_hosts(topo).len());
        return Ok(());
    }
    let verb = if dry_run { "would " } else { "" };
    for h in &plan.enable {
        println!("{verb}enable {}", topology::tunnel_unit(h));
        if !dry_run {
            enable_tunnel(h, &topo.hub)?;
        }
    }
    for h in &plan.disable {
        println!("{verb}disable {}", topology::tunnel_unit(h));
        if !dry_run {
            disable_tunnel(h)?;
        }
    }
    if dry_run {
        println!("(dry run — pass --yes to apply)");
    }
    Ok(())
}

/// Host names of every `sot-relay-tunnel@*` instance systemd --user
/// currently reports `enabled`. The only I/O `apply` does to read state;
/// `topology::apply_plan` is the pure decision made from its result.
fn enabled_tunnel_hosts() -> Result<Vec<String>, String> {
    let out = std::process::Command::new("systemctl")
        .args(["--user", "list-unit-files", "sot-relay-tunnel@*.service", "--no-legend", "--no-pager"])
        .output()
        .map_err(|e| format!("systemctl --user list-unit-files: {e}"))?;
    if !out.status.success() {
        return Err(format!("systemctl --user list-unit-files: {}", String::from_utf8_lossy(&out.stderr).trim()));
    }
    let text = String::from_utf8_lossy(&out.stdout);
    Ok(text
        .lines()
        .filter_map(|l| {
            let mut w = l.split_whitespace();
            let unit = w.next()?;
            if w.next()? != "enabled" {
                return None;
            }
            unit.strip_prefix("sot-relay-tunnel@")?.strip_suffix(".service").map(str::to_string)
        })
        .collect())
}

/// `~/.config/systemd/user` — same `$XDG_CONFIG_HOME`-or-`$HOME/.config`
/// rule `scripts/install.sh` uses for its own unit writes.
fn systemd_user_dir() -> Result<PathBuf, String> {
    std::env::var_os("XDG_CONFIG_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
        .map(|d| d.join("systemd").join("user"))
        .ok_or_else(|| "no $XDG_CONFIG_HOME or $HOME: can't find ~/.config/systemd/user".to_string())
}

fn enable_tunnel(host: &str, hub: &str) -> Result<(), String> {
    write_dropin(host, hub)?;
    run_systemctl(&["--user", "daemon-reload"])?;
    run_systemctl(&["--user", "enable", "--now", &topology::tunnel_unit(host)])
}

fn disable_tunnel(host: &str) -> Result<(), String> {
    run_systemctl(&["--user", "disable", "--now", &topology::tunnel_unit(host)])?;
    remove_dropin(host)
}

fn run_systemctl(args: &[&str]) -> Result<(), String> {
    let out = std::process::Command::new("systemctl")
        .args(args)
        .output()
        .map_err(|e| format!("systemctl {}: {e}", args.join(" ")))?;
    if !out.status.success() {
        return Err(format!("systemctl {}: {}", args.join(" "), String::from_utf8_lossy(&out.stderr).trim()));
    }
    Ok(())
}

fn dropin_path(host: &str) -> Result<PathBuf, String> {
    Ok(systemd_user_dir()?.join(format!("{}.d", topology::tunnel_unit(host))).join(topology::TUNNEL_DROPIN_FILE))
}

/// Writes only apply's own fixed-named file (`topology::TUNNEL_DROPIN_FILE`)
/// inside the instance's `.d/` directory — any other, hand-made drop-in
/// beside it is never touched (mirrors `scripts/install.sh`'s own rule for
/// `sotd.service.d`: it heals only the one drop-in name it knows).
fn write_dropin(host: &str, hub: &str) -> Result<(), String> {
    let path = dropin_path(host)?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    std::fs::write(&path, topology::tunnel_dropin(hub)).map_err(|e| format!("{}: {e}", path.display()))
}

/// Removes apply's own drop-in file, then the `.d/` directory only if that
/// left it empty — a hand-made drop-in under a different name keeps the
/// directory (and itself) alive.
fn remove_dropin(host: &str) -> Result<(), String> {
    let path = dropin_path(host)?;
    let _ = std::fs::remove_file(&path);
    if let Some(dir) = path.parent() {
        let _ = std::fs::remove_dir(dir);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    // apply's decision logic (topology::apply_plan, topology::require_hub,
    // topology::tunnel_dropin) is pure and tested in sot-protocol, against
    // a fixture, with no systemd involved. What's left here is flag
    // parsing only — enabled_tunnel_hosts/run_systemctl/write_dropin need
    // a real `systemctl --user` and `$HOME`, unverified by this suite.

    #[test]
    fn dry_run_is_default_yes_flips_it() {
        let dry = |args: &[&str]| !args.iter().any(|a| *a == "--yes");
        assert!(dry(&[]));
        assert!(dry(&["--dry-run"]));
        assert!(!dry(&["--yes"]));
    }
}
