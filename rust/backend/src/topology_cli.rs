//! `sotd topology <plan|status|relay-endpoint|sync|apply>` — what a box
//! derives from the declared topology (`sot_protocol::topology`, the one
//! parser). A pure query arm of `main` (no startup side effects). Output is
//! line-oriented so a shell or PowerShell launcher reads it with `split`;
//! the `plan` line set is documented on `topology::plan` and nowhere else.

use sot_protocol::topology::{self, Topology};
use std::path::PathBuf;

const USAGE: &str = "\
Usage: sotd topology <subcommand>

  plan [--self <host>]  this box's derived facts, one per line: self, hub,
                        relay-endpoint, dial <host> <endpoint> (every daemon
                        host), tunnel <host> <port> (every daemon host but
                        self; the hub is 18743, others ordinal in file order)
  status                the declared table (HOST DECLARED), plus a
                        \"cache diverged\" line when this box's file hash
                        disagrees with the hub's (skipped ON the hub, and
                        best-effort — silent if the hub can't be reached)
  relay-endpoint        SOT_RELAY_ENDPOINT for this box
  sync [--hub <alias>]  fetch the hub's ~/.config/sot/hosts.toml into this
                        box's config dir (refused if it does not parse or
                        does not list this box; nothing written when it
                        equals the local copy)
  apply [--yes]         hub only: converge sot-relay-tunnel@<host> systemd
                        --user instances (enable/disable) plus each one's
                        ConditionHost drop-in with the declared list.
                        Default is a DRY RUN (prints what it would do);
                        --yes runs systemctl for real. --dry-run is
                        accepted too, as the explicit spelling of the
                        default. Refuses on a non-hub box, naming the hub.
  set <edit>            send one edit to the hub daemon over this box's own
                        control dial (the dial itself is the authorisation —
                        no second credential). <edit> is one of:
                          add <host> [--daemon] [--frontend]
                          remove <host>
                          flag <host> daemon|frontend true|false
                          monitor-add <label> <target>
                          monitor-remove <label>

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
        "status" => with_topology(true, |t| {
            print!("{}", topology::status_table(t));
            report_cache_divergence(t);
            Ok(())
        }),
        // Warnings stay quiet here: the shell profile runs this on every
        // non-interactive ssh, and N stderr lines per remote command is
        // not a warning, it is noise.
        "relay-endpoint" => with_topology(false, |t| {
            self_host().and_then(|me| topology::relay_endpoint(t, &me)).map(|e| println!("{e}"))
        }),
        "sync" => report(sync(flag("--hub"))),
        "apply" => with_topology(true, |t| apply(t, !args.iter().any(|a| a == "--yes"))),
        "set" => report(set(&args[1..])),
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
    let me = self_host()?;
    if me == fetched.hub {
        return Err(format!("this box is the hub `{}`; its file is the canonical copy", fetched.hub));
    }
    refuse_if_not_listed(&fetched, &me)?;
    if std::fs::read_to_string(&dest).ok().as_deref() == Some(text.as_str()) {
        println!("{} is current (same as {hub})", dest.display());
        return Ok(());
    }
    crate::topology_store::write_atomic(&dest, &text)?;
    println!("synced {} from {hub} ({} hosts, {} monitor targets)", dest.display(), fetched.hosts.len(), fetched.monitor.len());
    for w in &fetched.warnings {
        eprintln!("sotd topology: {}: {w}", dest.display());
    }
    Ok(())
}

/// A stale file that doesn't list this box is the root cause of the
/// old-vs-new install confusion (a v1 file never listed the frontend box):
/// caught here, at fetch time, rather than left for `plan` to fail on
/// silently later. Factored out so it's testable without ssh or a real
/// `self_host()`.
fn refuse_if_not_listed(fetched: &Topology, me: &str) -> Result<(), String> {
    if fetched.host(me).is_none() {
        return Err(format!("hub's hosts.toml does not list this box; add `[host.{me}]` on the hub"));
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

/// `sotd topology set <edit>`. Sends one edit to the hub over this box's
/// own control dial — the SAME endpoint the launcher's own tunnel plan
/// already establishes for the hub (`hub_endpoint`, below: this box's own
/// socket when it IS the hub, else the forwarded `tcp:127.0.0.1:18743`
/// every non-hub box already dials for everything else). No separate
/// "find the hub" step: authorisation is the dial itself (`op::
/// TOPOLOGY_SET`'s own doc).
fn set(words: &[String]) -> Result<(), String> {
    let edit = parse_edit(words)?;
    let (_, topo) = topology::load()?.ok_or_else(|| "no hosts.toml yet (run `sotd topology sync --hub <alias>`)".to_string())?;
    let me = self_host()?;

    // "removing or clearing `daemon` on a host with running rows" — the
    // HUB only ever checks its OWN rows (plan §B: a star, it cannot see
    // another daemon's). When THIS edit touches THIS box's own daemon
    // flag, this box's own CLI does the equivalent check against its own
    // local daemon before ever dialing the hub, so the gap the hub can't
    // cover is covered here instead.
    let touches_own_daemon = match &edit {
        topology::TopologyEdit::RemoveHost { name } => *name == me,
        topology::TopologyEdit::SetFlag { name, key, value } => *name == me && key == "daemon" && !*value,
        _ => false,
    };
    if touches_own_daemon && local_has_running_rows(&me) {
        return Err(format!("`{me}` has running capsule rows on this box's own daemon; stop them before removing or un-daemoning this host"));
    }

    let endpoint = hub_endpoint(&topo, &me);
    let payload = serde_json::json!({ "edit": edit });
    let res = crate::topology_dial::dial_and_call(&endpoint, &me, sot_protocol::op::TOPOLOGY_SET, payload)?;
    if let Some(err) = res.get("error").and_then(|v| v.as_str()) {
        let code = res.get("code").and_then(|v| v.as_str()).unwrap_or("refused");
        return Err(format!("{err} ({code})"));
    }
    let hash = res.get("hash").and_then(|v| v.as_str()).unwrap_or("?");
    println!("topology.set ok, hash {hash}");
    Ok(())
}

/// This box's own dial to the hub: its own socket when it IS the hub, else
/// the SAME forwarded local port `plan`'s `dial <hub>` line already uses
/// (the launcher's own tunnel, already up on every non-hub box).
fn hub_endpoint(topo: &Topology, me: &str) -> String {
    if me == topo.hub {
        topology::local_endpoint("sot")
    } else {
        format!("tcp:127.0.0.1:{}", topology::HUB_LOCAL_PORT)
    }
}

/// Best-effort: dial THIS box's own local daemon and ask `workspace.list`
/// for any row whose phase is `starting`/`ready`. A daemon that can't be
/// reached here means nothing is running here either (`false`), same as
/// no daemon at all — this is a local safety pre-check, not a source of
/// truth the hub relies on.
fn local_has_running_rows(me: &str) -> bool {
    let endpoint = topology::local_endpoint("sot");
    let res = match crate::topology_dial::dial_and_call(&endpoint, me, sot_protocol::op::WORKSPACE_LIST, serde_json::json!({})) {
        Ok(v) => v,
        Err(_) => return false,
    };
    res.get("workspaces")
        .and_then(|v| v.as_array())
        .map(|rows| {
            rows.iter().any(|r| {
                r.get("phase")
                    .and_then(|p| p.as_str())
                    .is_some_and(|p| p.eq_ignore_ascii_case("starting") || p.eq_ignore_ascii_case("ready"))
            })
        })
        .unwrap_or(false)
}

/// `sotd topology status`'s cache line: skipped on the hub itself (its
/// file IS the canonical copy, trivially current); best-effort elsewhere —
/// a hub that can't be reached prints nothing rather than an error, since
/// `status` is meant to work offline too.
fn report_cache_divergence(topo: &Topology) {
    let Ok(me) = self_host() else { return };
    if me == topo.hub {
        return;
    }
    let Some(path) = topology::locate() else { return };
    let Ok(local_text) = std::fs::read_to_string(&path) else { return };
    let local_hash = topology::hash_text(&local_text);
    let endpoint = hub_endpoint(topo, &me);
    let Ok(res) = crate::topology_dial::dial_and_call(&endpoint, &me, sot_protocol::op::VERSION_QUERY, serde_json::json!({})) else {
        return;
    };
    let Some(hub_hash) = res.get("daemon").and_then(|d| d.get("hosts_toml_hash")).and_then(|v| v.as_str()) else {
        return;
    };
    if !hub_hash.is_empty() && hub_hash != local_hash {
        println!("cache diverged: this box's hosts.toml ({local_hash}) != the hub's ({hub_hash}); run `sotd topology sync`");
    }
}

/// Parse `sotd topology set <words...>` into one [`topology::TopologyEdit`]
/// — the shapes documented in [`USAGE`].
fn parse_edit(words: &[String]) -> Result<topology::TopologyEdit, String> {
    const USAGE: &str = "usage: sotd topology set <add|remove|flag|monitor-add|monitor-remove> ...";
    match words.first().map(String::as_str) {
        Some("add") => {
            let name = words.get(1).cloned().ok_or(USAGE)?;
            let daemon = words.iter().any(|w| w == "--daemon");
            let frontend = words.iter().any(|w| w == "--frontend");
            Ok(topology::TopologyEdit::AddHost { name, daemon, frontend })
        }
        Some("remove") => Ok(topology::TopologyEdit::RemoveHost { name: words.get(1).cloned().ok_or(USAGE)? }),
        Some("flag") => {
            let name = words.get(1).cloned().ok_or(USAGE)?;
            let key = words.get(2).cloned().ok_or(USAGE)?;
            let value = match words.get(3).map(String::as_str) {
                Some("true") => true,
                Some("false") => false,
                _ => return Err(USAGE.to_string()),
            };
            Ok(topology::TopologyEdit::SetFlag { name, key, value })
        }
        Some("monitor-add") => Ok(topology::TopologyEdit::MonitorAdd {
            label: words.get(1).cloned().ok_or(USAGE)?,
            target: words.get(2).cloned().ok_or(USAGE)?,
        }),
        Some("monitor-remove") => Ok(topology::TopologyEdit::MonitorRemove { label: words.get(1).cloned().ok_or(USAGE)? }),
        _ => Err(USAGE.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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

    fn w(words: &[&str]) -> Vec<String> {
        words.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn parse_edit_covers_every_shape() {
        assert_eq!(
            parse_edit(&w(&["add", "gamma", "--daemon"])).unwrap(),
            topology::TopologyEdit::AddHost { name: "gamma".into(), daemon: true, frontend: false }
        );
        assert_eq!(parse_edit(&w(&["remove", "gamma"])).unwrap(), topology::TopologyEdit::RemoveHost { name: "gamma".into() });
        assert_eq!(
            parse_edit(&w(&["flag", "gamma", "daemon", "true"])).unwrap(),
            topology::TopologyEdit::SetFlag { name: "gamma".into(), key: "daemon".into(), value: true }
        );
        assert_eq!(
            parse_edit(&w(&["monitor-add", "gpu", "box"])).unwrap(),
            topology::TopologyEdit::MonitorAdd { label: "gpu".into(), target: "box".into() }
        );
        assert_eq!(parse_edit(&w(&["monitor-remove", "gpu"])).unwrap(), topology::TopologyEdit::MonitorRemove { label: "gpu".into() });
        assert!(parse_edit(&w(&["flag", "gamma", "daemon", "maybe"])).is_err());
        assert!(parse_edit(&w(&[])).is_err());
    }

    #[test]
    fn hub_endpoint_is_local_on_the_hub_else_the_forwarded_port() {
        let t = topology::parse("hub = \"alpha\"\n[host.alpha]\ndaemon = true\n").unwrap();
        assert_eq!(hub_endpoint(&t, "alpha"), topology::local_endpoint("sot"));
        assert_eq!(hub_endpoint(&t, "beta"), format!("tcp:127.0.0.1:{}", topology::HUB_LOCAL_PORT));
    }

    #[test]
    fn sync_refuses_a_fetched_file_that_does_not_list_self() {
        let fetched = topology::parse("hub = \"hub\"\n[host.hub]\ndaemon = true\n").unwrap();
        let e = refuse_if_not_listed(&fetched, "frontend-box").unwrap_err();
        assert!(e.contains("does not list this box") && e.contains("[host.frontend-box]"), "{e}");
        assert!(refuse_if_not_listed(&fetched, "hub").is_ok());
    }
}
